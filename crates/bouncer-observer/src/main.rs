mod args;
mod config;
mod core;

use anyhow::{Context, Result};
use bouncer_helpers::{logging, shutdown};
use config::ObserverConfig;
use core::{run_publisher, run_udp_listener};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    logging::init_logging(
        "bouncer_observer=info,tokio=warn",
        "OBSERVER_LOG",
        "bouncer-observer",
    );

    let config = ObserverConfig::load()?;

    info!(
        "observer starting: listen_udp={}, server={}, source={}",
        config.listen_udp, config.server, config.source
    );

    let (events_tx, events_rx) = mpsc::channel(config.queue_capacity.max(1));
    let shutdown = CancellationToken::new();
    tokio::spawn(shutdown::listen_shutdown(shutdown.clone()));

    let listener_task = tokio::spawn(run_udp_listener(
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
