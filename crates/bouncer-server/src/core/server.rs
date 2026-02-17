use std::io::ErrorKind;

use anyhow::{Context, Result};
use bouncer_proto::{ACK, ProtoError, decode_header_json, read_frame_async};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, trace, warn};

use super::parser::ObserverDeliveryEvent;
use crate::app::AppState;

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
    loop {
        let (header_bytes, body) =
            match read_frame_async(&mut stream, MAX_HEADER_LEN, MAX_BODY_LEN)
                .await
            {
                Ok(frame) => frame,
                Err(ProtoError::Io(err))
                    if matches!(
                        err.kind(),
                        ErrorKind::UnexpectedEof
                            | ErrorKind::ConnectionReset
                            | ErrorKind::BrokenPipe
                    ) =>
                {
                    warn!("client disconnected: error={}", err);
                    break;
                }
                Err(err) => {
                    return Err(err).context("failed to read frame");
                }
            };

        let header = decode_header_json(&header_bytes)
            .context("failed to decode header")?;

        if matches!(header.kind.as_deref(), Some("heartbeat")) {
            trace!(
                "client heartbeat: source={}",
                header.source.as_deref().unwrap_or("-")
            );
            stream.write_all(ACK).await.context("failed to write ACK")?;
            continue;
        }

        if matches!(header.kind.as_deref(), Some("register")) {
            stream.write_all(ACK).await.context("failed to write ACK")?;
            info!(
                "client registered: source={}, from={}",
                header.source.as_deref().unwrap_or("-"),
                header.from
            );
            continue;
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
            continue;
        }

        let written_path = state
            .spool
            .enqueue_mail(&body)
            .await
            .context("failed to enqueue payload to spool")?;

        stream.write_all(ACK).await.context("failed to write ACK")?;

        info!(
            "bounce accepted: bytes={}, path={}, kind={}, source={}",
            body.len(),
            written_path.display(),
            header.kind.as_deref().unwrap_or("mail"),
            header.source.as_deref().unwrap_or("-")
        );
    }

    Ok(())
}
