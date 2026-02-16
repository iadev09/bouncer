use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use notify::{
    Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher
};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::parser::parse_bounce_report;
use crate::app::AppState;

/// Watches the `incoming/` spool directory for new files and forwards
/// discovered `.eml` paths to the processing queue.
pub async fn spawn_notify_watcher(
    state: AppState,
    process_tx: mpsc::Sender<PathBuf>
) {
    if let Err(err) = run_notify_watcher(
        state.spool.incoming.clone(),
        state.shutdown.clone(),
        process_tx
    )
    .await
    {
        error!("notify watcher stopped with error: error={err}");
    }
}

async fn run_notify_watcher(
    incoming_dir: PathBuf,
    shutdown: CancellationToken,
    process_tx: mpsc::Sender<PathBuf>
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = match RecommendedWatcher::new(
        move |result| {
            let _ = tx.send(result);
        },
        NotifyConfig::default()
    ) {
        Ok(w) => w,
        Err(err) => {
            return Err(anyhow::anyhow!(
                "failed to create notify watcher: {err}"
            ));
        }
    };

    watcher.watch(&incoming_dir, RecursiveMode::NonRecursive).with_context(
        || {
            format!(
                "failed to watch incoming spool: {}",
                incoming_dir.display()
            )
        }
    )?;

    info!("notify watcher active: path={}", incoming_dir.display());

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("notify watcher stopping");
                break;
            }
            maybe_event = rx.recv() => {
                let Some(result) = maybe_event else {
                    break;
                };

                match result {
                    Ok(event) => {
                        for path in event.paths {
                            if is_eml_file(&path) {
                                if process_tx.send(path).await.is_err() {
                                    info!("notify watcher stopping: process queue closed");
                                    break;
                                }
                            }
                        }
                    }
                    Err(err) => warn!("watch event error: error={err}"),
                }
            }
        }
    }

    Ok(())
}

/// Periodically scans `incoming/` as a fallback for missed filesystem events.
///
/// Every discovered `.eml` file is pushed into the same processing queue used
/// by the notify watcher.
pub async fn spawn_periodic_scan(
    state: AppState,
    process_tx: mpsc::Sender<PathBuf>,
    scan_secs: u64
) {
    let mut ticker = interval(Duration::from_secs(scan_secs.max(1)));

    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => {
                info!("incoming scan loop stopping");
                break;
            }
            _ = ticker.tick() => {
                match tokio::fs::read_dir(&state.spool.incoming).await {
                    Ok(mut entries) => {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let path = entry.path();
                            if is_eml_file(&path) {
                                if process_tx.send(path).await.is_err() {
                                    info!("incoming scan loop stopping: process queue closed");
                                    return;
                                }
                            }
                        }
                    }
                    Err(err) => warn!("incoming scan failed: error={err}"),
                }
            }
        }
    }
}

/// Consumes queued spool paths and executes bounded concurrent workers.
///
/// Concurrency is limited by a fixed worker count to avoid unbounded task
/// growth and to protect DB and disk I/O.
pub async fn spawn_worker_dispatcher(
    state: AppState,
    process_rx: mpsc::Receiver<PathBuf>,
    concurrency: usize
) {
    let workers = concurrency.max(1);
    let shared_rx = Arc::new(Mutex::new(process_rx));
    let mut handles = Vec::with_capacity(workers);

    info!("worker dispatcher started: workers={}", workers);

    for worker_id in 0..workers {
        let state = state.clone();
        let shared_rx = shared_rx.clone();

        handles.push(tokio::spawn(async move {
            loop {
                let recv_next = async {
                    let mut rx = shared_rx.lock().await;
                    rx.recv().await
                };

                tokio::select! {
                    _ = state.shutdown.cancelled() => {
                        break;
                    }
                    maybe_path = recv_next => {
                        let Some(path) = maybe_path else {
                            break;
                        };

                        if let Err(err) = process_spooled_message(state.clone(), &path).await {
                            warn!(
                                "message processing failed: worker={}, path={}, error={}",
                                worker_id,
                                path.display(),
                                err
                            );
                        }
                    }
                }
            }
        }));
    }

    for handle in handles {
        if let Err(err) = handle.await {
            warn!("worker task join failed: error={err}");
        }
    }

    info!("worker dispatcher stopping");
}

/// Moves a message through `incoming -> processing -> done/failed` and applies
/// parsed bounce status to the database.
async fn process_spooled_message(
    state: AppState,
    incoming_path: &Path
) -> Result<()> {
    if !is_eml_file(incoming_path) {
        return Ok(());
    }

    let file_name =
        incoming_path.file_name().context("incoming path has no file name")?;

    let processing_path = state.spool.processing.join(file_name);

    match tokio::fs::rename(incoming_path, &processing_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to move file into processing: {} -> {}",
                    incoming_path.display(),
                    processing_path.display()
                )
            });
        }
    }

    let result = async {
        let raw_mail = tokio::fs::read(&processing_path)
            .await
            .with_context(|| format!("failed to read {}", processing_path.display()))?;

        if raw_mail.is_empty() {
            bail!("empty mail payload");
        }

        let parsed = parse_bounce_report(&raw_mail)?;
        state
            .db
            .upsert_bounce(&parsed)
            .await
            .context("database upsert failed")?;

        info!(
            "processed message: path={}, bytes={}, hash={}, status_code={}, action={}, recipient={}",
            processing_path.display(),
            raw_mail.len(),
            parsed.hash,
            parsed.status_code,
            parsed.action.as_deref().unwrap_or("-"),
            parsed.recipient.as_deref().unwrap_or("-")
        );

        Ok::<(), anyhow::Error>(())
    }
    .await;

    let target_dir =
        if result.is_ok() { &state.spool.done } else { &state.spool.failed };

    let final_path = target_dir.join(file_name);
    tokio::fs::rename(&processing_path, &final_path).await.with_context(
        || {
            format!(
                "failed to finalize file: {} -> {}",
                processing_path.display(),
                final_path.display()
            )
        }
    )?;

    result
}

/// Returns true when the given path ends with `.eml`.
fn is_eml_file(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("eml")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::run_notify_watcher;

    fn make_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::now_v7()))
    }

    async fn wait_for_path(
        rx: &mut mpsc::Receiver<PathBuf>,
        expected: &Path
    ) -> bool {
        let expected = expected.to_path_buf();
        let receive = async {
            loop {
                let Some(path) = rx.recv().await else {
                    return false;
                };
                if path == expected {
                    return true;
                }
            }
        };
        timeout(Duration::from_secs(3), receive).await.unwrap_or(false)
    }

    #[tokio::test]
    async fn notify_enqueues_eml_file_on_create() {
        let incoming = make_temp_dir("bouncer-notify-incoming");
        tokio::fs::create_dir_all(&incoming).await.unwrap();

        let (tx, mut rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();
        let join = tokio::spawn(run_notify_watcher(
            incoming.clone(),
            shutdown.clone(),
            tx
        ));

        tokio::time::sleep(Duration::from_millis(200)).await;

        let eml_path = incoming.join("sample.eml");
        tokio::fs::write(&eml_path, b"Subject: test\r\n\r\nbody")
            .await
            .unwrap();

        assert!(wait_for_path(&mut rx, &eml_path).await);

        shutdown.cancel();
        let _ = timeout(Duration::from_secs(2), join).await;
        let _ = tokio::fs::remove_dir_all(&incoming).await;
    }

    #[tokio::test]
    async fn notify_ignores_non_eml_file() {
        let incoming = make_temp_dir("bouncer-notify-incoming");
        tokio::fs::create_dir_all(&incoming).await.unwrap();

        let (tx, mut rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();
        let join = tokio::spawn(run_notify_watcher(
            incoming.clone(),
            shutdown.clone(),
            tx
        ));

        tokio::time::sleep(Duration::from_millis(200)).await;

        let txt_path = incoming.join("ignore.txt");
        tokio::fs::write(&txt_path, b"not eml").await.unwrap();

        let got_any = timeout(Duration::from_millis(700), rx.recv())
            .await
            .ok()
            .flatten()
            .is_some();
        assert!(!got_any);

        shutdown.cancel();
        let _ = timeout(Duration::from_secs(2), join).await;
        let _ = tokio::fs::remove_dir_all(&incoming).await;
    }
}
