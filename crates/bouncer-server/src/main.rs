mod app;
mod config;
mod core;

use std::sync::Arc;

use anyhow::{Context, Result};
use app::AppState;
use bouncer_helpers::{logging, shutdown};
use config::Config;
use core::{
    Database, Spool, run_imap_poll_loop, run_tcp_server, spawn_notify_watcher,
    spawn_periodic_scan, spawn_worker_dispatcher,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    logging::init_logging(
        "bouncer_server=info,notify=warn,tokio=warn",
        "BOUNCER_LOG",
        "bouncer-server",
    );

    let config = Config::load().context("failed to load configuration")?;
    let spool = Arc::new(Spool::new(config.spool.clone()));
    spool.ensure_dirs().await?;

    let db = Arc::new(
        Database::connect(&config.database_url)
            .await
            .context("failed to connect database")?,
    );

    let state = AppState { spool, db, shutdown: CancellationToken::new() };

    info!(
        "server starting: listen={}, spool={}",
        config.listen,
        config.spool.display()
    );

    let process_queue_capacity = config
        .worker_concurrency
        .max(1)
        .saturating_mul(config.process_queue_per_worker);
    let (process_tx, process_rx) = mpsc::channel(process_queue_capacity);
    info!("process queue configured: capacity={}", process_queue_capacity);

    tokio::spawn(shutdown::listen_shutdown(state.shutdown.clone()));
    tokio::spawn(spawn_notify_watcher(state.clone(), process_tx.clone()));
    tokio::spawn(spawn_periodic_scan(
        state.clone(),
        process_tx.clone(),
        config.incoming_scan_secs,
    ));
    tokio::spawn(spawn_worker_dispatcher(
        state.clone(),
        process_rx,
        config.worker_concurrency,
    ));
    tokio::spawn(run_imap_poll_loop(
        config.imap.clone(),
        state.db.clone(),
        state.shutdown.clone(),
    ));

    run_tcp_server(&config.listen, state).await
}
