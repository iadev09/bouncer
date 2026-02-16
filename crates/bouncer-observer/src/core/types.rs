use std::time::Instant;

use serde::Serialize;

#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub hash: String,
    pub updated_at: Instant,
}

#[derive(Debug, Clone)]
pub struct SmtpEvent {
    pub queue_id: String,
    pub recipient: String,
    pub smtp_status: String,
    pub status_code: String,
    pub action: String,
    pub diagnostic: String,
}

#[derive(Debug, Clone)]
pub struct DeliveryEvent {
    pub hash: String,
    pub queue_id: String,
    pub recipient: String,
    pub status_code: String,
    pub action: String,
    pub diagnostic: String,
    pub smtp_status: String,
}

#[derive(Debug, Serialize)]
pub struct DeliveryEventPayload {
    pub source: String,
    pub hash: String,
    pub queue_id: String,
    pub recipient: String,
    pub status_code: String,
    pub action: String,
    pub diagnostic: String,
    pub smtp_status: String,
    pub observed_at_unix: u64,
}

pub enum ParsedSyslog {
    Cleanup { queue_id: String, hash: String },
    Smtp(SmtpEvent),
}
