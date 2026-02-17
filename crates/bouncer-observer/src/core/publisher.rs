use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bouncer_proto::{
    Header, encode_header_json, read_ack_async, write_frame_async
};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::types::{DeliveryEvent, DeliveryEventPayload};
use crate::config::ObserverConfig;

const RETRY_ATTEMPTS: usize = 3;
const FRAME_TO: &str = "bouncer@ingest";

/// Runs the TCP publisher loop.
///
/// It consumes delivery events from the channel, publishes them to bouncer
/// server, and emits periodic heartbeat frames on the same connection.
pub async fn run_publisher(
    config: ObserverConfig,
    mut events_rx: mpsc::Receiver<DeliveryEvent>,
    shutdown: CancellationToken
) -> Result<()> {
    let mut connection: Option<TcpStream> = None;
    let mut heartbeat_tick =
        interval(Duration::from_secs(config.heartbeat_secs.max(1)));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("publisher stopping");
                break;
            }
            maybe_event = events_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };

                let payload = match build_delivery_payload(&config, &event) {
                    Ok(payload) => payload,
                    Err(err) => {
                        warn!(
                            "failed to serialize observer event: hash={}, queue_id={}, error={}",
                            event.hash,
                            event.queue_id,
                            err
                        );
                        continue;
                    }
                };
                if let Err(err) = send_with_retry(
                    &config,
                    &mut connection,
                    "observer_event",
                    &payload,
                ).await {
                    warn!(
                        "failed to publish observer event: hash={}, queue_id={}, smtp_status={}, error={}",
                        event.hash,
                        event.queue_id,
                        event.smtp_status,
                        err
                    );
                } else {
                    debug!(
                        "observer event published: hash={}, queue_id={}, smtp_status={}, status_code={}, action={}, recipient={}",
                        event.hash,
                        event.queue_id,
                        event.smtp_status,
                        event.status_code,
                        event.action,
                        event.recipient
                    );
                }
            }
            _ = heartbeat_tick.tick(), if config.heartbeat_secs > 0 => {
                let payload = build_heartbeat_payload();
                if let Err(err) = send_with_retry(
                    &config,
                    &mut connection,
                    "heartbeat",
                    &payload,
                ).await {
                    debug!("heartbeat send failed: error={err}");
                }
            }
        }
    }

    Ok(())
}

/// Sends a frame with reconnection and bounded retry logic.
async fn send_with_retry(
    config: &ObserverConfig,
    connection: &mut Option<TcpStream>,
    kind: &str,
    payload: &[u8]
) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 1..=RETRY_ATTEMPTS {
        if connection.is_none() {
            match connect_and_register(config).await {
                Ok(stream) => {
                    *connection = Some(stream);
                }
                Err(err) => {
                    last_error = Some(err);
                    sleep(Duration::from_millis((attempt * 250) as u64)).await;
                    continue;
                }
            }
        }

        let Some(stream) = connection.as_mut() else {
            continue;
        };

        match send_frame(config, stream, kind, payload).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                *connection = None;
                last_error = Some(err);
                sleep(Duration::from_millis((attempt * 250) as u64)).await;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("send failed")))
}

/// Opens a TCP connection to server and sends an initial `register` frame.
async fn connect_and_register(config: &ObserverConfig) -> Result<TcpStream> {
    let timeout_window =
        Duration::from_secs(config.connect_timeout_secs.max(1));
    let mut stream =
        timeout(timeout_window, TcpStream::connect(&config.server))
            .await
            .with_context(|| format!("connect timeout to {}", config.server))?
            .with_context(|| format!("connect failed to {}", config.server))?;

    stream.set_nodelay(true).ok();

    let register_payload = format!(
        "source={}\nlisten_udp={}\n",
        sanitize_header_value(&config.source),
        sanitize_header_value(&config.listen_udp.to_string())
    );

    send_frame(config, &mut stream, "register", register_payload.as_bytes())
        .await
        .context("register frame failed")?;

    info!(
        "observer connected: server={}, source={}",
        config.server, config.source
    );
    Ok(stream)
}

/// Encodes and writes one framed message, then waits for ACK within timeout.
async fn send_frame(
    config: &ObserverConfig,
    stream: &mut TcpStream,
    kind: &str,
    payload: &[u8]
) -> Result<()> {
    let header = Header {
        from: format!("observer@{}", sanitize_header_value(&config.source)),
        to: FRAME_TO.to_string(),
        kind: Some(kind.to_string()),
        source: Some(config.source.clone())
    };

    let header_bytes =
        encode_header_json(&header).context("failed to encode frame header")?;

    let io_timeout = Duration::from_secs(config.io_timeout_secs.max(1));

    timeout(io_timeout, write_frame_async(stream, &header_bytes, payload))
        .await
        .with_context(|| format!("write timeout for frame kind={kind}"))?
        .with_context(|| format!("failed to write frame kind={kind}"))?;

    timeout(io_timeout, read_ack_async(stream))
        .await
        .with_context(|| format!("ack timeout for frame kind={kind}"))?
        .with_context(|| format!("invalid ack for frame kind={kind}"))?;

    Ok(())
}

/// Builds the JSON payload sent as `kind=observer_event`.
fn build_delivery_payload(
    config: &ObserverConfig,
    event: &DeliveryEvent
) -> Result<Vec<u8>> {
    let payload = DeliveryEventPayload {
        source: sanitize_header_value(&config.source),
        hash: sanitize_header_value(&event.hash),
        queue_id: sanitize_header_value(&event.queue_id),
        recipient: sanitize_header_value(&event.recipient),
        status_code: sanitize_header_value(&event.status_code),
        action: sanitize_header_value(&event.action),
        diagnostic: sanitize_header_value(&event.diagnostic),
        smtp_status: sanitize_header_value(&event.smtp_status),
        observed_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };

    serde_json::to_vec(&payload)
        .context("failed to encode observer delivery event")
}

/// Builds a lightweight heartbeat payload with current unix timestamp.
fn build_heartbeat_payload() -> Vec<u8> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("ts={ts}\n").into_bytes()
}

/// Strips CR/LF from header values to keep frame metadata single-line.
fn sanitize_header_value(value: &str) -> String {
    value.chars().filter(|c| *c != '\r' && *c != '\n').collect::<String>()
}
