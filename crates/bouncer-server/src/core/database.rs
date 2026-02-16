use anyhow::{Context, Result};
use sqlx::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;
use tracing::{debug, warn};

use super::parser::{ObserverDeliveryEvent, ParsedBounce};

const MAIL_STATUS_SUCCESS: i32 = 7;
const MAIL_STATUS_PENDING: i32 = 3;
const MAIL_STATUS_SUSPENDED: i32 = -2;
const MAIL_STATUS_FAILED: i32 = -7;

#[derive(Debug)]
pub struct Database {
    pool: MySqlPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertBounceOutcome {
    UpdatedLocalMessage,
    MissingLocalMessage,
}

impl Database {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .context("failed to open mysql pool")?;

        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&pool)
            .await
            .context("database ping failed")?;

        Ok(Self { pool })
    }

    pub async fn apply_observer_event(
        &self,
        event: &ObserverDeliveryEvent,
    ) -> Result<()> {
        let parsed = event.as_parsed_bounce();
        let message_status = map_mail_message_status(&parsed);

        let mut tx = self.pool.begin().await.context("failed to begin tx")?;
        let message_id = sqlx::query_scalar::<_, u32>(
            "SELECT id FROM mail_messages WHERE hash = ? LIMIT 1",
        )
        .bind(&parsed.hash)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to query mail_messages")?;

        let Some(message_id) = message_id else {
            tx.commit().await.context("failed to commit tx")?;
            warn!(
                "observer event not linked to local message: hash={}, queue_id={}, source={}, smtp_status={}, observed_at_unix={}",
                event.hash,
                event.queue_id,
                event.source,
                event.smtp_status,
                event.observed_at_unix
            );
            return Ok(());
        };

        sqlx::query(
            "UPDATE mail_messages SET status = ?, updated_at = NOW() WHERE id = ?",
        )
        .bind(message_status)
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .context("failed to update mail_messages from observer event")?;

        if message_status != MAIL_STATUS_SUCCESS {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM mail_message_bounces WHERE message_id = ? LIMIT 1",
            )
            .bind(message_id)
            .fetch_optional(&mut *tx)
            .await
            .context("failed to query mail_message_bounces")?;

            if exists.is_some() {
                sqlx::query(
                    "UPDATE mail_message_bounces SET action = ?, status_code = ?, description = ?, created_at = NOW() WHERE message_id = ?",
                )
                .bind(parsed.action.as_deref())
                .bind(&parsed.status_code)
                .bind(parsed.description.as_deref())
                .bind(message_id)
                .execute(&mut *tx)
                .await
                .context("failed to update mail_message_bounces")?;
            } else {
                sqlx::query(
                    "INSERT INTO mail_message_bounces (message_id, action, status_code, description, created_at) VALUES (?, ?, ?, ?, NOW())",
                )
                .bind(message_id)
                .bind(parsed.action.as_deref())
                .bind(&parsed.status_code)
                .bind(parsed.description.as_deref())
                .execute(&mut *tx)
                .await
                .context("failed to insert mail_message_bounces")?;
            }
        }

        tx.commit().await.context("failed to commit tx")?;
        Ok(())
    }

    pub async fn upsert_bounce(
        &self,
        parsed: &ParsedBounce,
    ) -> Result<UpsertBounceOutcome> {
        let mut tx = self.pool.begin().await.context("failed to begin tx")?;

        let message_id = sqlx::query_scalar::<_, u32>(
            "SELECT id FROM mail_messages WHERE hash = ? LIMIT 1",
        )
        .bind(&parsed.hash)
        .fetch_optional(&mut *tx)
        .await
        .context("failed to query mail_messages")?;

        if let Some(message_id) = message_id {
            let message_status = map_mail_message_status(parsed);

            let message_update_result = sqlx::query(
                "UPDATE mail_messages SET status = ?, updated_at = NOW() WHERE hash = ?",
            )
            .bind(message_status)
            .bind(&parsed.hash)
            .execute(&mut *tx)
            .await
            .context("failed to update mail_messages")?;
            debug!(
                "db upsert mail_messages: op=update, hash={}, rows_affected={}",
                parsed.hash,
                message_update_result.rows_affected()
            );

            if message_status != MAIL_STATUS_SUCCESS {
                let exists = sqlx::query_scalar::<_, i64>(
                    "SELECT 1 FROM mail_message_bounces WHERE message_id = ? LIMIT 1",
                )
                .bind(message_id)
                .fetch_optional(&mut *tx)
                .await
                .context("failed to query mail_message_bounces")?;

                if exists.is_some() {
                    let bounce_update_result = sqlx::query(
                        "UPDATE mail_message_bounces SET action = ?, status_code = ?, description = ?, created_at = NOW() WHERE message_id = ?",
                    )
                    .bind(parsed.action.as_deref())
                    .bind(&parsed.status_code)
                    .bind(parsed.description.as_deref())
                    .bind(message_id)
                    .execute(&mut *tx)
                    .await
                    .context("failed to update mail_message_bounces")?;
                    debug!(
                        "db upsert mail_message_bounces: op=update, message_id={}, hash={}, rows_affected={}",
                        message_id,
                        parsed.hash,
                        bounce_update_result.rows_affected()
                    );
                } else {
                    let bounce_insert_result = sqlx::query(
                        "INSERT INTO mail_message_bounces (message_id, action, status_code, description, created_at) VALUES (?, ?, ?, ?, NOW())",
                    )
                    .bind(message_id)
                    .bind(parsed.action.as_deref())
                    .bind(&parsed.status_code)
                    .bind(parsed.description.as_deref())
                    .execute(&mut *tx)
                    .await
                    .context("failed to insert mail_message_bounces")?;
                    debug!(
                        "db upsert mail_message_bounces: op=insert, message_id={}, hash={}, rows_affected={}",
                        message_id,
                        parsed.hash,
                        bounce_insert_result.rows_affected()
                    );
                }
            }
        } else {
            warn!(
                "bounce hash not found in local mail_messages: hash={}, status_code={}, action={}",
                parsed.hash,
                parsed.status_code,
                parsed.action.as_deref().unwrap_or("-")
            );

            let message_status = map_mail_message_status(parsed);
            if message_status == MAIL_STATUS_SUCCESS {
                tx.commit().await.context("failed to commit tx")?;
                debug!(
                    "db upsert mail_bounces: op=skip, hash={}, reason=missing_local_message_and_success_status",
                    parsed.hash
                );
                return Ok(UpsertBounceOutcome::MissingLocalMessage);
            }

            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT 1 FROM mail_bounces WHERE hash = ? LIMIT 1",
            )
            .bind(&parsed.hash)
            .fetch_optional(&mut *tx)
            .await
            .context("failed to query mail_bounces")?;

            if exists.is_some() {
                let bounce_update_result = sqlx::query(
                    "UPDATE mail_bounces SET recipient = ?, action = ?, status_code = ?, description = ?, created_at = NOW() WHERE hash = ?",
                )
                .bind(parsed.recipient.as_deref())
                .bind(parsed.action.as_deref())
                .bind(&parsed.status_code)
                .bind(parsed.description.as_deref())
                .bind(&parsed.hash)
                .execute(&mut *tx)
                .await
                .context("failed to update mail_bounces")?;
                debug!(
                    "db upsert mail_bounces: op=update, hash={}, rows_affected={}",
                    parsed.hash,
                    bounce_update_result.rows_affected()
                );
            } else {
                let bounce_insert_result = sqlx::query(
                    "INSERT INTO mail_bounces (hash, recipient, action, status_code, description, created_at) VALUES (?, ?, ?, ?, ?, NOW())",
                )
                .bind(&parsed.hash)
                .bind(parsed.recipient.as_deref())
                .bind(parsed.action.as_deref())
                .bind(&parsed.status_code)
                .bind(parsed.description.as_deref())
                .execute(&mut *tx)
                .await
                .context("failed to insert mail_bounces")?;
                debug!(
                    "db upsert mail_bounces: op=insert, hash={}, rows_affected={}",
                    parsed.hash,
                    bounce_insert_result.rows_affected()
                );
            }
        }

        tx.commit().await.context("failed to commit tx")?;
        Ok(if message_id.is_some() {
            UpsertBounceOutcome::UpdatedLocalMessage
        } else {
            UpsertBounceOutcome::MissingLocalMessage
        })
    }
}

fn map_mail_message_status(parsed: &ParsedBounce) -> i32 {
    if let Some(action) = parsed.action.as_deref() {
        if action.eq_ignore_ascii_case("delivered")
            || action.eq_ignore_ascii_case("sent")
        {
            return MAIL_STATUS_SUCCESS;
        }
        if action.eq_ignore_ascii_case("delayed")
            || action.eq_ignore_ascii_case("deferred")
        {
            return MAIL_STATUS_PENDING;
        }
    }

    match parsed.status_code.as_str() {
        "5.7.1" | "5.7.2" | "5.7.3" | "5.7.0" => MAIL_STATUS_SUSPENDED,
        _ if parsed.status_code.starts_with("2.") => MAIL_STATUS_SUCCESS,
        _ if parsed.status_code.starts_with("4.") => MAIL_STATUS_PENDING,
        _ => MAIL_STATUS_FAILED,
    }
}
