use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use systemd::{JournalSeek, journal};
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use super::parser::parse_postfix_line;
use super::types::{DeliveryEvent, ParsedSyslog, QueueEntry};
use crate::config::JournalConfig;

pub async fn run_journal_watcher(
    config: JournalConfig,
    events_tx: mpsc::Sender<DeliveryEvent>,
    shutdown: CancellationToken
) -> Result<()> {
    let (lines_tx, mut lines_rx) = mpsc::unbounded_channel::<String>();
    let stop = Arc::new(AtomicBool::new(false));

    let thread_config = config.clone();
    let thread_stop = stop.clone();
    let reader_thread = thread::spawn(move || {
        run_reader_thread(thread_config, lines_tx, thread_stop);
    });

    let mut queue_map: HashMap<String, QueueEntry> = HashMap::new();
    let ttl = Duration::from_secs(config.mapping_ttl_secs.max(60));
    let mut cleanup_tick = interval(Duration::from_secs(300));

    info!(
        "journal listener ready: unit={}, identifiers={}",
        config.unit,
        config.identifiers.join(",")
    );

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("journal listener stopping");
                break;
            }
            _ = cleanup_tick.tick() => {
                let removed = prune_queue_map(&mut queue_map, ttl);
                if removed > 0 {
                    debug!(
                        "cleaned stale queue mappings: removed={}, tracked={}",
                        removed,
                        queue_map.len()
                    );
                }
            }
            maybe_line = lines_rx.recv() => {
                let Some(line) = maybe_line else {
                    break;
                };

                let Some(parsed) = parse_postfix_line(line.trim()) else {
                    continue;
                };

                match parsed {
                    ParsedSyslog::Cleanup { queue_id, hash } => {
                        debug!(
                            "queue mapping stored: queue_id={}, hash={}",
                            queue_id, hash
                        );
                        queue_map.insert(
                            queue_id,
                            QueueEntry {
                                hash,
                                updated_at: Instant::now(),
                            },
                        );
                    }
                    ParsedSyslog::Smtp(smtp) => {
                        let Some(entry) = queue_map.get_mut(&smtp.queue_id) else {
                            trace!(
                                "smtp log without known queue mapping: queue_id={}",
                                smtp.queue_id
                            );
                            continue;
                        };

                        entry.updated_at = Instant::now();
                        let event = DeliveryEvent {
                            hash: entry.hash.clone(),
                            queue_id: smtp.queue_id,
                            recipient: smtp.recipient,
                            status_code: smtp.status_code,
                            action: smtp.action,
                            diagnostic: smtp.diagnostic,
                            smtp_status: smtp.smtp_status,
                        };
                        debug!(
                            "smtp log matched queue mapping: queue_id={}, hash={}, smtp_status={}, status_code={}, action={}, recipient={}",
                            event.queue_id,
                            event.hash,
                            event.smtp_status,
                            event.status_code,
                            event.action,
                            event.recipient
                        );

                        if let Err(err) = events_tx.try_send(event) {
                            warn!(
                                "journal event queue is full, dropping event: error={err}"
                            );
                        }
                    }
                }
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = reader_thread.join();
    })
    .await;

    info!("journal watcher stopped");
    Ok(())
}

fn run_reader_thread(
    config: JournalConfig,
    lines_tx: mpsc::UnboundedSender<String>,
    stop: Arc<AtomicBool>
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }

        let mut reader = match open_reader(&config) {
            Ok(reader) => reader,
            Err(err) => {
                warn!("failed to open journald reader: error={err}");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };

        if config.seek_tail {
            if let Err(err) = reader.seek(JournalSeek::Tail) {
                warn!("failed to seek journald tail: error={err}");
            } else {
                let _ = reader.next();
            }
        }

        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }

            match reader.next() {
                Ok(0) => {
                    let _ = reader.wait(Some(Duration::from_millis(500)));
                }
                Ok(_) => {
                    if let Some(line) =
                        extract_postfix_line(&mut reader, &config.identifiers)
                    {
                        if lines_tx.send(line).is_err() {
                            return;
                        }
                    }
                }
                Err(err) => {
                    warn!("journald next() failed: error={err}");
                    break;
                }
            }
        }
    }
}

fn open_reader(config: &JournalConfig) -> Result<journal::Journal> {
    let mut reader =
        journal::OpenOptions::default().system(true).local_only(true).open()?;
    reader.match_add("_SYSTEMD_UNIT", config.unit.clone())?;
    Ok(reader)
}

fn extract_postfix_line(
    reader: &mut journal::Journal,
    identifiers: &[String]
) -> Option<String> {
    let message = get_data_string(reader, "MESSAGE")?;
    let identifier = get_data_string(reader, "SYSLOG_IDENTIFIER")
        .or_else(|| get_data_string(reader, "_COMM"))?;

    let matched = identifiers
        .iter()
        .any(|needle| identifier.eq_ignore_ascii_case(needle));
    if !matched {
        return None;
    }

    Some(format!("{identifier}[0]: {message}"))
}

fn get_data_string(
    reader: &mut journal::Journal,
    key: &str
) -> Option<String> {
    reader.get_data(key).ok()?.and_then(|field| {
        field.value().map(|value| String::from_utf8_lossy(value).into_owned())
    })
}

fn prune_queue_map(
    queue_map: &mut HashMap<String, QueueEntry>,
    ttl: Duration
) -> usize {
    let before = queue_map.len();
    let now = Instant::now();
    queue_map.retain(|_, entry| now.duration_since(entry.updated_at) <= ttl);
    before.saturating_sub(queue_map.len())
}
