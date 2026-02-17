#[cfg(target_os = "linux")]
mod args;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod core;

#[cfg(target_os = "linux")]
use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use bouncer_helpers::{logging, shutdown};
#[cfg(target_os = "linux")]
use config::JournalConfig;
#[cfg(target_os = "linux")]
use core::{run_journal_listener, run_publisher};
#[cfg(target_os = "linux")]
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tokio_util::sync::CancellationToken;
#[cfg(target_os = "linux")]
use tracing::{info, warn};

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    logging::init_logging(
        "bouncer_journal=info,tokio=warn",
        "JOURNAL_LOG",
        "bouncer-journal",
    );

    let config = JournalConfig::load()?;
    info!(
        "journal observer starting: unit={}, server={}, source={}, identifiers={}",
        config.unit,
        config.server,
        config.source,
        config.identifiers.join(",")
    );

    let (events_tx, events_rx) = mpsc::channel(config.queue_capacity.max(1));
    let shutdown = CancellationToken::new();
    tokio::spawn(shutdown::listen_shutdown(shutdown.clone()));

    let listener_task = tokio::spawn(run_journal_listener(
        config.clone(),
        events_tx,
        shutdown.clone(),
    ));

    let publisher_task = tokio::spawn(run_publisher(
        config.clone(),
        events_rx,
        shutdown.clone(),
    ));

    shutdown.cancelled().await;

    if let Err(err) =
        listener_task.await.context("listener task join failed")?
    {
        warn!("listener task stopped with error: error={err}");
    }

    if let Err(err) =
        publisher_task.await.context("publisher task join failed")?
    {
        warn!("publisher task stopped with error: error={err}");
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "bouncer-journal requires Linux (systemd/journald). Use bouncer-observer on non-Linux."
    );
}
