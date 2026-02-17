use super::types::{ParsedSyslog, SmtpEvent};

const MAX_DIAGNOSTIC_LEN: usize = 512;
const RELAY_HANDOFF_HOSTS: &[&str] = &["mxbg.nxmango.com"];

/// Parses one postfix syslog line into either:
/// - `ParsedSyslog::Cleanup { queue_id, hash }`
/// - `ParsedSyslog::Smtp(SmtpEvent)`
///
/// Correlation model used by observer:
/// 1. `postfix/cleanup` line provides `queue_id` + `message-id`.
/// 2. `postfix/smtp` line provides delivery status for the same `queue_id`.
/// 3. Listener cache joins both using `queue_id` and publishes final event with
///    your application hash.
///
/// Example flow:
/// - cleanup: `ABC123...: message-id=<9f...32chars...@example>`
/// - smtp: `ABC123...: to=<u@d>, dsn=5.1.1, status=bounced (...)`
pub fn parse_postfix_line(line: &str) -> Option<ParsedSyslog> {
    if !line.contains("postfix/") {
        return None;
    }

    let (_, rest) = line.split_once("postfix/")?;
    let (service_raw, rest) = rest.split_once('[')?;
    let (_, message) = rest.split_once("]: ")?;

    let service = service_raw.rsplit('/').next().unwrap_or(service_raw);

    if service.eq_ignore_ascii_case("cleanup") {
        let (queue_id, hash) = parse_cleanup_message(message)?;
        return Some(ParsedSyslog::Cleanup { queue_id, hash });
    }

    if service.eq_ignore_ascii_case("smtp") {
        return parse_smtp_message(message).map(ParsedSyslog::Smtp);
    }

    None
}

/// Parses `postfix/cleanup` message and extracts:
/// - postfix `queue_id`
/// - application hash derived from `message-id=<...>`
///
/// This stage does not contain delivery outcome; it only builds correlation key
/// (`queue_id -> hash`) for later `smtp` lines.
fn parse_cleanup_message(message: &str) -> Option<(String, String)> {
    let (queue_id, detail) = message.split_once(": ")?;
    if !is_queue_id(queue_id) {
        return None;
    }

    let marker = "message-id=<";
    let start = detail.find(marker)? + marker.len();
    let tail = &detail[start..];
    let end = tail.find('>')?;
    let message_id = &tail[..end];
    let hash = normalize_message_hash(message_id)?;

    Some((queue_id.to_string(), hash))
}

/// Parses `postfix/smtp` message and extracts recipient + status fields.
///
/// Returned event still carries `queue_id`; final hash is attached later by the
/// listener cache populated from `cleanup` lines.
fn parse_smtp_message(message: &str) -> Option<SmtpEvent> {
    let (queue_id, detail) = message.split_once(": ")?;
    if !is_queue_id(queue_id) {
        return None;
    }

    let recipient = extract_between(detail, "to=<", ">")?.to_string();
    let smtp_status = extract_token(detail, "status=")?.to_ascii_lowercase();
    let relay_handoff = extract_relay_host(detail)
        .map(|host| is_relay_handoff_host(&host))
        .unwrap_or(false);

    let default_status = default_status_code(&smtp_status, relay_handoff);
    let status_code = extract_token(detail, "dsn=")
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_status.to_string());

    let action = map_action(&smtp_status, relay_handoff).to_string();
    let diagnostic = build_diagnostic(queue_id, detail);

    Some(SmtpEvent {
        queue_id: queue_id.to_string(),
        recipient,
        smtp_status,
        status_code,
        action,
        diagnostic
    })
}

fn extract_between<'a>(
    text: &'a str,
    start: &str,
    end: &str
) -> Option<&'a str> {
    let start_idx = text.find(start)? + start.len();
    let rem = &text[start_idx..];
    let end_idx = rem.find(end)?;
    Some(rem[..end_idx].trim())
}

fn extract_token<'a>(
    text: &'a str,
    key: &str
) -> Option<&'a str> {
    let start_idx = text.find(key)? + key.len();
    let rem = &text[start_idx..];
    let token_len = rem
        .char_indices()
        .take_while(|(_, c)| {
            c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'
        })
        .last()
        .map(|(idx, c)| idx + c.len_utf8())
        .unwrap_or(0);

    if token_len == 0 { None } else { Some(rem[..token_len].trim()) }
}

fn map_action(
    smtp_status: &str,
    relay_handoff: bool
) -> &'static str {
    if smtp_status == "sent" && relay_handoff {
        // "sent" to an internal relay is not final mailbox delivery yet.
        return "delayed";
    }

    match smtp_status {
        "sent" => "delivered",
        "deferred" => "delayed",
        "bounced" | "expired" => "failed",
        _ => "failed"
    }
}

fn default_status_code(
    smtp_status: &str,
    relay_handoff: bool
) -> &'static str {
    if smtp_status == "sent" && relay_handoff {
        return "4.0.0";
    }

    match smtp_status {
        "sent" => "2.0.0",
        "deferred" => "4.0.0",
        "bounced" | "expired" => "5.0.0",
        _ => "5.0.0"
    }
}

fn build_diagnostic(
    queue_id: &str,
    detail: &str
) -> String {
    let mut collapsed = String::with_capacity(detail.len());
    let mut prev_space = false;

    for ch in detail.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
                prev_space = true;
            }
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }

    let collapsed = collapsed.trim();
    let mut diagnostic = format!("queue_id={queue_id}; {collapsed}");

    if diagnostic.len() > MAX_DIAGNOSTIC_LEN {
        diagnostic.truncate(MAX_DIAGNOSTIC_LEN);
    }

    diagnostic
}

fn is_queue_id(queue_id: &str) -> bool {
    !queue_id.is_empty()
        && queue_id.len() <= 32
        && queue_id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn extract_relay_host(detail: &str) -> Option<String> {
    let marker = "relay=";
    let start = detail.find(marker)? + marker.len();
    let rem = &detail[start..];

    let end = rem
        .find(|c: char| c == '[' || c == ':' || c == ',' || c.is_whitespace())
        .unwrap_or(rem.len());

    let host = rem[..end].trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

fn is_relay_handoff_host(host: &str) -> bool {
    RELAY_HANDOFF_HOSTS
        .iter()
        .any(|relay| host.eq_ignore_ascii_case(relay))
}

/// Normalizes message-id into the tracking hash expected by the app.
///
/// Expected input shape is `<{32-alnum-hash}@domain>`.
/// We keep only the local-part alphanumeric characters and accept exactly
/// 32 characters to avoid false matches.
fn normalize_message_hash(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|c| c == '<' || c == '>');
    let local_part = trimmed.split('@').next().unwrap_or("").trim();

    let hash: String =
        local_part.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

    if hash.len() == 32 { Some(hash) } else { None }
}
