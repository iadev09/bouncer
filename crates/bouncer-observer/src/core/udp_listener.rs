use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::parser::parse_postfix_line;
use super::types::{DeliveryEvent, ParsedSyslog, QueueEntry};
use crate::config::ObserverConfig;

const UDP_PACKET_BYTES: usize = 8192;

/// Runs the UDP syslog listener and converts postfix log lines into delivery
/// events for the publisher queue.
///
/// The listener keeps an in-memory `queue_id -> message hash` map using
/// `cleanup` lines and enriches `smtp` lines with that mapping.
pub async fn run_udp_listener(
    config: ObserverConfig,
    events_tx: mpsc::Sender<DeliveryEvent>,
    shutdown: CancellationToken
) -> Result<()> {
    let socket =
        UdpSocket::bind(config.listen_udp).await.with_context(|| {
            format!("failed to bind udp socket {}", config.listen_udp)
        })?;

    let mut buf = [0_u8; UDP_PACKET_BYTES];
    let mut queue_map: HashMap<String, QueueEntry> = HashMap::new();
    let ttl = Duration::from_secs(config.mapping_ttl_secs.max(60));
    let mut cleanup_tick = interval(Duration::from_secs(300));

    info!("udp listener ready: listen_udp={}", config.listen_udp);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("udp listener stopping");
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
            recv = socket.recv_from(&mut buf) => {
                let (len, _addr) = recv.context("udp recv failed")?;
                if len == 0 {
                    continue;
                }

                let line = match std::str::from_utf8(&buf[..len]) {
                    Ok(text) => text.trim(),
                    Err(_) => continue,
                };

                let Some(parsed) = parse_postfix_line(line) else {
                    continue;
                };

                match parsed {
                    ParsedSyslog::Cleanup { queue_id, hash } => {
                        // First stage: remember which app hash belongs to this postfix queue id.
                        queue_map.insert(
                            queue_id,
                            QueueEntry {
                                hash,
                                updated_at: Instant::now(),
                            },
                        );
                    }
                    ParsedSyslog::Smtp(smtp) => {
                        // Second stage: smtp has status fields; join with cached hash via queue id.
                        let Some(entry) = queue_map.get_mut(&smtp.queue_id) else {
                            debug!(
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

                        if let Err(err) = events_tx.try_send(event) {
                            warn!(
                                "observer event queue is full, dropping event: error={err}"
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Removes stale queue-id mappings that were not refreshed within `ttl`.
fn prune_queue_map(
    queue_map: &mut HashMap<String, QueueEntry>,
    ttl: Duration
) -> usize {
    let before = queue_map.len();
    let now = Instant::now();
    queue_map.retain(|_, entry| now.duration_since(entry.updated_at) <= ttl);
    before.saturating_sub(queue_map.len())
}
