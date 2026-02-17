use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use async_imap::types::Uid;
use async_imap::{Client, Session};
use async_native_tls::{TlsConnector, TlsStream};
use futures_util::TryStreamExt;
use time::{Month, OffsetDateTime};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use super::UpsertBounceOutcome;
use super::database::Database;
use super::parser::{ParserError, parse_bounce_report_detailed};
use crate::config::ImapConfig;

type ImapSession = Session<TlsStream<TcpStream>>;
const IMAP_PROCESS_CONCURRENCY_MAX: usize = 16;
const IMAP_FETCH_QUERY_BODY_UID: &str = "(UID BODY.PEEK[])";

/// Runs the optional IMAP fallback polling loop.
///
/// The loop is disabled when IMAP host is not configured and exits on
/// cancellation.
pub async fn run_imap_poll_loop(
    config: ImapConfig,
    db: Arc<Database>,
    shutdown: CancellationToken
) {
    if !config.enabled() {
        info!("imap fallback disabled (IMAP_HOST missing)");
        return;
    }

    info!(
        "imap fallback loop enabled: host={}, mailbox={}, poll_secs={}, connect_timeout_secs={}, max_messages_per_poll={}, max_history={}, mark_seen_if_not_exist={}",
        config.host.as_deref().unwrap_or_default(),
        config.mailbox,
        config.poll_secs,
        config.connect_timeout_secs,
        config.max_messages_per_poll,
        config
            .max_history
            .map(|duration| humantime::format_duration(duration).to_string())
            .unwrap_or_else(|| "none".to_string()),
        config.mark_seen_if_not_exist
    );

    let mut ticker = interval(Duration::from_secs(config.poll_secs.max(5)));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("imap poll loop stopping");
                break;
            }
            _ = ticker.tick() => {
                if let Err(err) = run_imap_poll_once(&config, db.clone()).await {
                    warn!("imap poll iteration failed: error={err:#}");
                }
            }
        }
    }
}

/// Executes one IMAP poll iteration.
///
/// Fetches a bounded unseen batch from IMAP, parses bounce payloads and writes
/// status updates directly to DB (without going through spool/worker path).
async fn run_imap_poll_once(
    config: &ImapConfig,
    db: Arc<Database>
) -> Result<()> {
    trace!("imap poll started");
    let host = config.host.as_deref().context("IMAP_HOST missing")?;
    let user = config.user.as_deref().context("IMAP_USER missing")?;
    let pass = config.pass.as_deref().context("IMAP_PASS missing")?;

    let max_messages = config.max_messages_per_poll.max(1);
    let mut session = open_imap_session(config, host, user, pass).await?;

    session.select(&config.mailbox).await.with_context(|| {
        format!("imap select mailbox failed: mailbox={}", config.mailbox)
    })?;

    let uid_search_query = build_uid_search_query(config.max_history);
    let mut uids: Vec<Uid> = session
        .uid_search(&uid_search_query)
        .await
        .with_context(|| {
            format!("imap UID SEARCH failed: query={uid_search_query}")
        })?
        .into_iter()
        .collect();
    let unseen_total = uids.len();
    // Process newest mailbox UIDs first to prioritize recent delivery outcomes.
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.truncate(max_messages);

    debug!(
        "imap unseen selected: unseen_total={}, selected={}, max_messages_per_poll={}, search_query={}",
        unseen_total,
        uids.len(),
        max_messages,
        uid_search_query
    );

    if uids.is_empty() {
        session.logout().await.ok();
        return Ok(());
    }

    let mut processed_uids = Vec::with_capacity(uids.len());
    let mut seen_uids = Vec::with_capacity(uids.len());
    let mut parse_failures = 0usize;
    let mut ignored_not_delivery = 0usize;
    let mut ignored_missing_hash = 0usize;
    let mut fetch_failures = 0usize;
    let mut fetched_items = 0usize;
    let mut fallback_fetch_attempts = 0usize;
    let mut fallback_fetch_hits = 0usize;
    let mut db_failures = 0usize;
    let mut missing_in_db = 0usize;
    let mut join_failures = 0usize;
    let selected_total = uids.len();
    let process_concurrency = max_messages.min(IMAP_PROCESS_CONCURRENCY_MAX);
    let mut processing = JoinSet::new();

    let uid_set = uids.iter().map(Uid::to_string).collect::<Vec<_>>().join(",");
    let mut fetches = session
        .uid_fetch(uid_set, IMAP_FETCH_QUERY_BODY_UID)
        .await
        .context("imap UID FETCH batch failed")?;

    while let Some(fetch) = fetches
        .try_next()
        .await
        .context("imap UID FETCH batch stream failed")?
    {
        fetched_items += 1;

        let Some(uid) = fetch.uid else {
            fetch_failures += 1;
            warn!("imap fetch item missing UID field");
            continue;
        };

        debug!("imap processing message: uid={}", uid);

        let raw_mail = match fetch.body() {
            Some(bytes) => {
                debug!(
                    "imap message fetched: uid={}, bytes={}",
                    uid,
                    bytes.len()
                );
                bytes.to_vec()
            }
            None => {
                fetch_failures += 1;
                warn!("imap message has no body: uid={uid}");
                continue;
            }
        };
        let db = db.clone();
        let mark_seen_if_not_exist = config.mark_seen_if_not_exist;
        processing.spawn(async move {
            process_fetched_message(uid, raw_mail, db, mark_seen_if_not_exist)
                .await
        });

        if processing.len() >= process_concurrency {
            collect_one_process_result(
                &mut processing,
                &mut processed_uids,
                &mut seen_uids,
                &mut parse_failures,
                &mut ignored_not_delivery,
                &mut ignored_missing_hash,
                &mut db_failures,
                &mut missing_in_db,
                &mut join_failures
            )
            .await;
        }
    }

    drop(fetches);

    // Some IMAP servers may return UIDs in SEARCH but yield an empty stream in
    // batched FETCH. Retry with per-UID fetch to separate "could not download"
    // from parser/DB outcomes.
    if selected_total > 0 && fetched_items == 0 {
        warn!(
            "imap batch fetch returned no messages, retrying per-uid fetch: selected={}",
            selected_total
        );

        for &uid in &uids {
            fallback_fetch_attempts += 1;
            match fetch_single_message_body(&mut session, uid).await {
                Ok(Some(raw_mail)) => {
                    fetched_items += 1;
                    fallback_fetch_hits += 1;

                    let db = db.clone();
                    let mark_seen_if_not_exist = config.mark_seen_if_not_exist;
                    processing.spawn(async move {
                        process_fetched_message(
                            uid,
                            raw_mail,
                            db,
                            mark_seen_if_not_exist
                        )
                        .await
                    });
                }
                Ok(None) => {
                    fetch_failures += 1;
                    warn!("imap per-uid fetch returned no body: uid={}", uid);
                }
                Err(err) => {
                    fetch_failures += 1;
                    warn!(
                        "imap per-uid fetch failed: uid={}, error={err:#}",
                        uid
                    );
                }
            }

            if processing.len() >= process_concurrency {
                collect_one_process_result(
                    &mut processing,
                    &mut processed_uids,
                    &mut seen_uids,
                    &mut parse_failures,
                    &mut ignored_not_delivery,
                    &mut ignored_missing_hash,
                    &mut db_failures,
                    &mut missing_in_db,
                    &mut join_failures
                )
                .await;
            }
        }
    }

    while !processing.is_empty() {
        collect_one_process_result(
            &mut processing,
            &mut processed_uids,
            &mut seen_uids,
            &mut parse_failures,
            &mut ignored_not_delivery,
            &mut ignored_missing_hash,
            &mut db_failures,
            &mut missing_in_db,
            &mut join_failures
        )
        .await;
    }

    if !seen_uids.is_empty() {
        mark_seen_uids(&mut session, &seen_uids).await?;
    }

    session.logout().await.ok();

    if selected_total > 0 && fetched_items == 0 {
        warn!(
            "imap poll selected messages but fetch stream returned none: selected={}",
            selected_total
        );
    }

    info!(
        "imap poll processed: selected={}, fetched_items={}, fallback_fetch_attempts={}, fallback_fetch_hits={}, parsed_ok={}, parse_failures={}, ignored_not_delivery={}, ignored_missing_hash={}, fetch_failures={}, db_failures={}, missing_in_db={}, join_failures={}, marked_seen={}",
        selected_total,
        fetched_items,
        fallback_fetch_attempts,
        fallback_fetch_hits,
        processed_uids.len(),
        parse_failures,
        ignored_not_delivery,
        ignored_missing_hash,
        fetch_failures,
        db_failures,
        missing_in_db,
        join_failures,
        seen_uids.len()
    );

    Ok(())
}

async fn fetch_single_message_body(
    session: &mut ImapSession,
    uid: Uid
) -> Result<Option<Vec<u8>>> {
    let mut fetches = session
        .uid_fetch(uid.to_string(), IMAP_FETCH_QUERY_BODY_UID)
        .await
        .with_context(|| format!("imap per-uid FETCH failed: uid={uid}"))?;

    while let Some(fetch) = fetches.try_next().await.with_context(|| {
        format!("imap per-uid FETCH stream failed: uid={uid}")
    })? {
        if let Some(bytes) = fetch.body() {
            let fetched_uid = fetch.uid.unwrap_or(uid);
            debug!(
                "imap message fetched (per-uid): uid={}, bytes={}",
                fetched_uid,
                bytes.len()
            );
            return Ok(Some(bytes.to_vec()));
        }
    }

    Ok(None)
}

#[derive(Debug)]
enum ProcessResult {
    Processed { uid: Uid },
    MissingInDb { uid: Uid, hash: String, mark_seen: bool },
    IgnoredNotDelivery { uid: Uid },
    IgnoredMissingHash { uid: Uid },
    ParseFailed { uid: Uid, code: &'static str, message: String },
    DbFailed { uid: Uid, hash: String, message: String }
}

async fn process_fetched_message(
    uid: Uid,
    raw_mail: Vec<u8>,
    db: Arc<Database>,
    mark_seen_if_not_exist: bool
) -> ProcessResult {
    let parsed = match parse_bounce_report_detailed(&raw_mail) {
        Ok(parsed) => {
            debug!(
                "imap message parsed: uid={}, hash={}, status_code={}, action={}, from={}, to={}",
                uid,
                parsed.hash,
                parsed.status_code,
                parsed.action.as_deref().unwrap_or("-"),
                parsed.sender.as_deref().unwrap_or("-"),
                parsed.recipient.as_deref().unwrap_or("-")
            );
            parsed
        }
        Err(ParserError::NotDeliveryReport) => {
            return ProcessResult::IgnoredNotDelivery { uid };
        }
        Err(ParserError::MissingHash) => {
            return ProcessResult::IgnoredMissingHash { uid };
        }
        Err(err) => {
            return ProcessResult::ParseFailed {
                uid,
                code: err.code(),
                message: err.to_string()
            };
        }
    };

    match db.upsert_bounce(&parsed).await {
        Ok(UpsertBounceOutcome::UpdatedLocalMessage) => {
            ProcessResult::Processed { uid }
        }
        Ok(UpsertBounceOutcome::MissingLocalMessage) => {
            ProcessResult::MissingInDb {
                uid,
                hash: parsed.hash,
                mark_seen: mark_seen_if_not_exist
            }
        }
        Err(err) => ProcessResult::DbFailed {
            uid,
            hash: parsed.hash,
            message: format!("{err:#}")
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn collect_one_process_result(
    processing: &mut JoinSet<ProcessResult>,
    processed_uids: &mut Vec<Uid>,
    seen_uids: &mut Vec<Uid>,
    parse_failures: &mut usize,
    ignored_not_delivery: &mut usize,
    ignored_missing_hash: &mut usize,
    db_failures: &mut usize,
    missing_in_db: &mut usize,
    join_failures: &mut usize
) {
    match processing.join_next().await {
        Some(Ok(ProcessResult::Processed { uid })) => {
            processed_uids.push(uid);
            seen_uids.push(uid);
        }
        Some(Ok(ProcessResult::MissingInDb { uid, hash, mark_seen })) => {
            *missing_in_db += 1;
            if mark_seen {
                seen_uids.push(uid);
            }
            warn!(
                "ERROR_CODE=IMAP_HASH_NOT_FOUND_IN_DB imap message hash not found in DB: uid={}, hash={}, mark_seen_if_not_exist={}",
                uid, hash, mark_seen
            );
        }
        Some(Ok(ProcessResult::IgnoredNotDelivery { uid })) => {
            *parse_failures += 1;
            *ignored_not_delivery += 1;
            seen_uids.push(uid);
            warn!(
                "ERROR_CODE=IMAP_DISCARDED_NOT_DELIVERY imap message discarded and marked seen: uid={}, parser_code={}, reason={}",
                uid,
                ParserError::NotDeliveryReport.code(),
                ParserError::NotDeliveryReport
            );
        }
        Some(Ok(ProcessResult::IgnoredMissingHash { uid })) => {
            *parse_failures += 1;
            *ignored_missing_hash += 1;
            seen_uids.push(uid);
            warn!(
                "ERROR_CODE=IMAP_DISCARDED_MISSING_HASH imap message discarded and marked seen: uid={}, parser_code={}, reason={}",
                uid,
                ParserError::MissingHash.code(),
                ParserError::MissingHash
            );
        }
        Some(Ok(ProcessResult::ParseFailed { uid, code, message })) => {
            *parse_failures += 1;
            warn!(
                "ERROR_CODE=IMAP_PARSE_FAILED imap message parse failed: uid={}, parser_code={}, error={}",
                uid, code, message
            );
        }
        Some(Ok(ProcessResult::DbFailed { uid, hash, message })) => {
            *db_failures += 1;
            warn!(
                "ERROR_CODE=IMAP_DB_UPSERT_FAILED imap message db upsert failed: uid={}, hash={}, error={}",
                uid, hash, message
            );
        }
        Some(Err(err)) => {
            *join_failures += 1;
            warn!(
                "ERROR_CODE=IMAP_TASK_JOIN_FAILED imap process task join failed: error={err}"
            );
        }
        None => {}
    }
}

fn build_uid_search_query(max_history: Option<StdDuration>) -> String {
    match max_history {
        Some(duration) => {
            let since = format_imap_since_date(duration);
            format!("UNSEEN SINCE {since}")
        }
        None => "UNSEEN".to_string()
    }
}

fn format_imap_since_date(duration: StdDuration) -> String {
    let seconds = duration.as_secs().min(i64::MAX as u64) as i64;
    let cutoff = OffsetDateTime::now_utc() - time::Duration::seconds(seconds);
    format!(
        "{:02}-{}-{}",
        cutoff.day(),
        month_short(cutoff.month()),
        cutoff.year()
    )
}

fn month_short(month: Month) -> &'static str {
    match month {
        Month::January => "Jan",
        Month::February => "Feb",
        Month::March => "Mar",
        Month::April => "Apr",
        Month::May => "May",
        Month::June => "Jun",
        Month::July => "Jul",
        Month::August => "Aug",
        Month::September => "Sep",
        Month::October => "Oct",
        Month::November => "Nov",
        Month::December => "Dec"
    }
}

async fn open_imap_session(
    config: &ImapConfig,
    host: &str,
    user: &str,
    pass: &str
) -> Result<ImapSession> {
    let port = config.port;
    let connect_timeout =
        Duration::from_secs(config.connect_timeout_secs.max(1));

    let tcp = tokio::time::timeout(
        connect_timeout,
        TcpStream::connect((host, port)),
    )
    .await
    .with_context(|| {
        format!(
            "imap tcp connect timeout: host={host}, port={port}, timeout_secs={}",
            config.connect_timeout_secs
        )
    })?
    .with_context(|| format!("imap tcp connect failed: host={host}, port={port}"))?;

    let tls = TlsConnector::new();
    let tls_stream = tokio::time::timeout(connect_timeout, tls.connect(host, tcp))
        .await
        .with_context(|| {
            format!(
                "imap tls handshake timeout: host={host}, port={port}, timeout_secs={}",
                config.connect_timeout_secs
            )
        })?
        .with_context(|| format!("imap tls handshake failed: host={host}, port={port}"))?;

    let mut client = Client::new(tls_stream);
    let resp = tokio::time::timeout(connect_timeout, client.read_response())
        .await
        .with_context(|| {
            format!(
                "imap greeting timeout: host={host}, port={port}, timeout_secs={}",
                config.connect_timeout_secs
            )
        })?
        .context("failed to read imap greeting")?
        .context("unexpected end of stream while waiting imap greeting")?;

    tracing::trace!("imap greeting: {resp:?}");

    tokio::time::timeout(connect_timeout, client.login(user, pass))
        .await
        .with_context(|| {
            format!(
                "imap login timeout: host={host}, user={user}, timeout_secs={}",
                config.connect_timeout_secs
            )
        })?
        .map_err(|(err, _client)| err)
        .with_context(|| format!("imap login failed: host={host}, user={user}"))
}

async fn mark_seen_uids(
    session: &mut ImapSession,
    uids: &[Uid]
) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }

    let uid_set = uids.iter().map(Uid::to_string).collect::<Vec<_>>().join(",");

    let mut updates = session
        .uid_store(uid_set, "+FLAGS (\\Seen)")
        .await
        .context("imap UID STORE +FLAGS (\\\\Seen) failed")?;

    while updates
        .try_next()
        .await
        .context("imap UID STORE response stream failed")?
        .is_some()
    {}

    Ok(())
}
