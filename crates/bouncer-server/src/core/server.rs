use anyhow::{Context, Result};
use bouncer_proto::{ACK, decode_header_json, read_frame_async};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::app::AppState;

use super::parser::ObserverDeliveryEvent;

const MAX_HEADER_LEN: u32 = 64 * 1024;
const MAX_BODY_LEN: u64 = 25 * 1024 * 1024;

/// Runs the TCP ingest loop and spawns one task per accepted client.
///
/// The loop exits only when the shared shutdown token is cancelled.
pub async fn run_tcp_server(
    listen: &str,
    state: AppState,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("failed to bind tcp listener on {listen}"))?;

    loop {
        tokio::select! {
            _ = state.shutdown.cancelled() => {
                info!("tcp server stopping");
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("tcp accept failed")?;
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_client(stream, state).await {
                        warn!(
                            "client ingest failed: peer={}, error={}",
                            peer,
                            err
                        );
                    }
                });
            }
        }
    }

    Ok(())
}

/// Handles a single framed client message.
///
/// Supported kinds:
/// - `heartbeat` / `register`: ACK only (control plane)
/// - `observer_event`: decode JSON payload and apply directly to DB
/// - everything else: treat payload as raw mail and enqueue to spool
async fn handle_client(
    mut stream: TcpStream,
    state: AppState,
) -> Result<()> {
    let (header_bytes, body) =
        read_frame_async(&mut stream, MAX_HEADER_LEN, MAX_BODY_LEN)
            .await
            .context("failed to read frame")?;

    let header =
        decode_header_json(&header_bytes).context("failed to decode header")?;

    if matches!(header.kind.as_deref(), Some("heartbeat" | "register")) {
        stream.write_all(ACK).await.context("failed to write ACK")?;
        info!(
            "control frame accepted: kind={}, source={}, from={}",
            header.kind.as_deref().unwrap_or("-"),
            header.source.as_deref().unwrap_or("-"),
            header.from
        );
        return Ok(());
    }

    if matches!(header.kind.as_deref(), Some("observer_event")) {
        let event: ObserverDeliveryEvent = serde_json::from_slice(&body)
            .context("failed to decode observer event body")?;

        state
            .db
            .apply_observer_event(&event)
            .await
            .context("failed to apply observer event")?;

        stream.write_all(ACK).await.context("failed to write ACK")?;
        info!(
            "observer event accepted: source={}, hash={}, queue_id={}, status_code={}, action={}",
            header.source.as_deref().unwrap_or("-"),
            event.hash,
            event.queue_id,
            event.status_code,
            event.action
        );
        return Ok(());
    }

    let written_path = state
        .spool
        .enqueue_mail(&body)
        .await
        .context("failed to enqueue payload to spool")?;

    stream.write_all(ACK).await.context("failed to write ACK")?;

    info!(
        "message accepted: bytes={}, path={}, kind={}, source={}",
        body.len(),
        written_path.display(),
        header.kind.as_deref().unwrap_or("mail"),
        header.source.as_deref().unwrap_or("-")
    );

    Ok(())
}
