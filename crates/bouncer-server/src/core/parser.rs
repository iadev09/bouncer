use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

use anyhow::Result;
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders};
use serde::Deserialize;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct ParsedBounce {
    pub hash: String,
    pub status_code: String,
    pub action: Option<String>,
    pub recipient: Option<String>,
    pub description: Option<String>
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserError {
    NotDeliveryReport,
    MissingHash,
    MissingStatusCode
}

impl fmt::Display for ParserError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>
    ) -> fmt::Result {
        match self {
            Self::NotDeliveryReport => {
                write!(f, "message does not look like a delivery status report")
            }
            Self::MissingHash => {
                write!(f, "bounce hash not found (X-Message-Id/Message-ID)")
            }
            Self::MissingStatusCode => write!(f, "status code not found")
        }
    }
}

impl Error for ParserError {}

impl ParserError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotDeliveryReport => "NOT_DELIVERY_REPORT",
            Self::MissingHash => "MISSING_HASH",
            Self::MissingStatusCode => "MISSING_STATUS_CODE"
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ObserverDeliveryEvent {
    pub source: String,
    pub hash: String,
    pub queue_id: String,
    pub recipient: String,
    pub status_code: String,
    pub action: String,
    pub diagnostic: String,
    pub smtp_status: String,
    pub observed_at_unix: u64
}

impl ObserverDeliveryEvent {
    pub fn as_parsed_bounce(&self) -> ParsedBounce {
        ParsedBounce {
            hash: self.hash.clone(),
            status_code: self.status_code.clone(),
            action: Some(self.action.clone()),
            recipient: Some(self.recipient.clone()),
            description: Some(self.diagnostic.clone())
        }
    }
}

pub fn parse_bounce_report(raw_mail: &[u8]) -> Result<ParsedBounce> {
    parse_bounce_report_detailed(raw_mail).map_err(anyhow::Error::new)
}

pub fn parse_bounce_report_detailed(
    raw_mail: &[u8]
) -> std::result::Result<ParsedBounce, ParserError> {
    let parsed_message = message_parser().parse(raw_mail);
    let attachment_candidates = parsed_message
        .as_ref()
        .map(collect_attachment_text_candidates)
        .unwrap_or_default();
    let mut full_text: Option<String> = None;

    let mut looks_like_report = attachment_candidates
        .iter()
        .any(|candidate| candidate.kind == CandidateKind::DeliveryStatus)
        || attachment_candidates
            .iter()
            .any(|candidate| looks_like_delivery_report(candidate.text));
    if !looks_like_report {
        looks_like_report = looks_like_delivery_report(full_message_text(
            raw_mail,
            &mut full_text
        ));
    }

    if !looks_like_report {
        return Err(ParserError::NotDeliveryReport);
    }

    let mut merged = ParsedFields::default();

    for candidate in &attachment_candidates {
        let mut parsed =
            parse_fields_from_text(candidate.text, &candidate.scan_label);
        match candidate.kind {
            CandidateKind::DeliveryStatus => {
                // DSN part should provide status metadata, not message hash.
                parsed.hash = None;
                parsed.hash_priority = u8::MAX;
            }
            CandidateKind::OriginalHeaders | CandidateKind::OriginalMessage => {
                // Original headers/message should provide message hash only.
                parsed.status_code = None;
                parsed.action = None;
                parsed.recipient = None;
                parsed.description = None;
            }
            CandidateKind::TextBody | CandidateKind::Other => {
                continue;
            }
        }
        merge_missing(&mut merged, parsed);
        if merged.hash.is_some() && merged.status_code.is_some() {
            debug!(
                "bounce parser optimization: required fields found in typed attachment scan, skipping fallback scan: scan={}",
                candidate.scan_label
            );
            break;
        }
    }

    if merged.hash.is_none() || merged.status_code.is_none() {
        for candidate in &attachment_candidates {
            let mut parsed =
                parse_fields_from_text(candidate.text, &candidate.scan_label);
            constrain_hash_source(&mut parsed, candidate.kind);
            merge_missing(&mut merged, parsed);
            if merged.hash.is_some() && merged.status_code.is_some() {
                debug!(
                    "bounce parser optimization: required fields found in fallback attachment scan, skipping full_message scan: scan={}",
                    candidate.scan_label
                );
                break;
            }
        }
    }

    if merged.status_code.is_none() {
        let mut parsed = parse_fields_from_text(
            full_message_text(raw_mail, &mut full_text),
            "full_message"
        );
        // Never trust the top-level bounce Message-ID as our delivery hash.
        parsed.hash = None;
        parsed.hash_priority = u8::MAX;
        merge_missing(&mut merged, parsed);
    }

    if merged.status_code.is_none() {
        for candidate in &attachment_candidates {
            if let Some(code) = find_status_code_in_text(candidate.text) {
                merged.status_code = Some(code);
                break;
            }
        }
    }

    if merged.status_code.is_none() {
        merged.status_code = find_status_code_in_text(full_message_text(
            raw_mail,
            &mut full_text
        ));
    }

    let hash = merged.hash.ok_or(ParserError::MissingHash)?;
    let status_code =
        merged.status_code.ok_or(ParserError::MissingStatusCode)?;

    Ok(ParsedBounce {
        hash,
        status_code,
        action: merged.action,
        recipient: merged.recipient,
        description: merged.description
    })
}

fn header_value<'a>(
    line: &'a str,
    header_name: &str
) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    if name.trim().eq_ignore_ascii_case(header_name) {
        Some(value.trim())
    } else {
        None
    }
}

struct ParsedFields {
    hash: Option<String>,
    hash_priority: u8,
    status_code: Option<String>,
    action: Option<String>,
    recipient: Option<String>,
    description: Option<String>
}

impl Default for ParsedFields {
    fn default() -> Self {
        Self {
            hash: None,
            hash_priority: u8::MAX,
            status_code: None,
            action: None,
            recipient: None,
            description: None
        }
    }
}

fn parse_fields_from_text(
    text: &str,
    scan_label: &str
) -> ParsedFields {
    let mut parsed = ParsedFields::default();
    let mut current = String::new();
    let mut logical_lines_scanned = 0usize;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            if !current.is_empty() {
                current.push(' ');
                current.push_str(line.trim_start());
            }
            continue;
        }

        if !current.is_empty() {
            logical_lines_scanned += 1;
            apply_header_line(
                &mut parsed,
                &current,
                scan_label,
                logical_lines_scanned
            );
            // Lazy stop: once required fields are found, avoid scanning the
            // rest of large MIME payloads.
            if parsed.hash.is_some() && parsed.status_code.is_some() {
                debug!(
                    "bounce parser lazy stop: scan={}, scanned_lines={}, found=hash+status",
                    scan_label, logical_lines_scanned
                );
                return parsed;
            }
        }

        current.clear();
        current.push_str(line);
    }

    if !current.is_empty() {
        apply_header_line(
            &mut parsed,
            &current,
            scan_label,
            logical_lines_scanned.saturating_add(1)
        );
    }

    parsed
}

fn full_message_text<'a>(
    raw_mail: &'a [u8],
    cache: &'a mut Option<String>
) -> &'a str {
    cache
        .get_or_insert_with(|| String::from_utf8_lossy(raw_mail).into_owned())
        .as_str()
}

fn apply_header_line(
    parsed: &mut ParsedFields,
    line: &str,
    scan_label: &str,
    line_no: usize
) {
    try_set_hash_from_header(parsed, line, "X-Message-Id", scan_label, line_no);
    try_set_hash_from_header(
        parsed,
        line,
        "X-MS-Exchange-Parent-Message-Id",
        scan_label,
        line_no
    );
    try_set_hash_from_header(parsed, line, "In-Reply-To", scan_label, line_no);
    try_set_hash_from_header(parsed, line, "References", scan_label, line_no);
    try_set_hash_from_header(parsed, line, "Message-ID", scan_label, line_no);

    if parsed.status_code.is_none() {
        if let Some(value) = header_value(line, "Status") {
            parsed.status_code = parse_status_code(value);
        }
    }

    if parsed.action.is_none() {
        if let Some(value) = header_value(line, "Action") {
            let word = value.split_whitespace().next().unwrap_or("").trim();
            if !word.is_empty() {
                parsed.action = Some(word.to_string());
            }
        }
    }

    if parsed.recipient.is_none() {
        if let Some(value) = header_value(line, "Original-Recipient")
            .or_else(|| header_value(line, "Final-Recipient"))
        {
            let recipient = value
                .split_once(';')
                .map(|(_, rhs)| rhs.trim())
                .unwrap_or_else(|| value.trim());
            if !recipient.is_empty() {
                parsed.recipient = Some(recipient.to_string());
            }
        }
    }

    if parsed.description.is_none() {
        if let Some(value) = header_value(line, "Diagnostic-Code") {
            let description = value
                .split_once(';')
                .map(|(_, rhs)| rhs.trim())
                .unwrap_or_else(|| value.trim());
            if !description.is_empty() {
                parsed.description = Some(description.to_string());
            }
        }
    }
}

fn try_set_hash_from_header(
    parsed: &mut ParsedFields,
    line: &str,
    header_name: &str,
    scan_label: &str,
    line_no: usize
) {
    let Some(value) = header_value(line, header_name) else {
        return;
    };

    let Some(hash) = extract_hash_from_message_id_like_header(value) else {
        return;
    };

    let priority = hash_header_priority(header_name);
    if parsed.hash.is_some() && parsed.hash_priority <= priority {
        return;
    }

    debug!(
        "bounce parser hash found: scan={}, line={}, header={}, hash={}, priority={}",
        scan_label, line_no, header_name, hash, priority
    );
    parsed.hash = Some(hash);
    parsed.hash_priority = priority;
}

fn merge_missing(
    target: &mut ParsedFields,
    source: ParsedFields
) {
    if source.hash.is_some()
        && (target.hash.is_none()
            || source.hash_priority < target.hash_priority)
    {
        target.hash = source.hash;
        target.hash_priority = source.hash_priority;
    }
    if target.status_code.is_none() {
        target.status_code = source.status_code;
    }
    if target.action.is_none() {
        target.action = source.action;
    }
    if target.recipient.is_none() {
        target.recipient = source.recipient;
    }
    if target.description.is_none() {
        target.description = source.description;
    }
}

fn hash_header_priority(header_name: &str) -> u8 {
    match header_name.to_ascii_lowercase().as_str() {
        "x-message-id" => 0,
        "x-ms-exchange-parent-message-id" => 1,
        "in-reply-to" => 2,
        "references" => 3,
        "message-id" => 4,
        _ => 10
    }
}

fn constrain_hash_source(
    parsed: &mut ParsedFields,
    kind: CandidateKind
) {
    if !matches!(
        kind,
        CandidateKind::OriginalHeaders | CandidateKind::OriginalMessage
    ) {
        parsed.hash = None;
        parsed.hash_priority = u8::MAX;
    }
}

#[derive(Debug)]
struct AttachmentScanCandidate<'a> {
    scan_label: String,
    text: &'a str,
    kind: CandidateKind,
    priority: u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    DeliveryStatus,
    OriginalHeaders,
    OriginalMessage,
    TextBody,
    Other
}

fn collect_attachment_text_candidates<'a>(
    parsed: &'a Message<'a>
) -> Vec<AttachmentScanCandidate<'a>> {
    let mut out = Vec::new();
    collect_attachment_text_candidates_from_attachments(parsed, "0", &mut out);
    collect_attachment_text_candidates_from_text_bodies(parsed, "0", &mut out);
    out.sort_by_key(|candidate| candidate.priority);
    out
}

fn message_parser() -> &'static MessageParser {
    static PARSER: OnceLock<MessageParser> = OnceLock::new();
    PARSER.get_or_init(MessageParser::default)
}

fn collect_attachment_text_candidates_from_attachments<'a>(
    message: &'a Message<'a>,
    path: &str,
    out: &mut Vec<AttachmentScanCandidate<'a>>
) {
    for (idx, part) in message.attachments().enumerate() {
        let part_path = format!("{path}.{idx}");
        let mime = part_mime_type(part);

        if should_scan_attachment_mime(&mime) {
            if let Some(text) = decoded_part_text(part) {
                if !text.trim().is_empty() {
                    let kind = classify_attachment_kind(&mime);
                    let priority = attachment_scan_priority(kind, text);
                    out.push(AttachmentScanCandidate {
                        scan_label: format!(
                            "attachment:{}@{}",
                            mime, part_path
                        ),
                        text,
                        kind,
                        priority
                    });
                }
            }
        }

        if let Some(nested) = part.message() {
            collect_attachment_text_candidates_from_attachments(
                nested,
                &format!("{part_path}.m"),
                out
            );
            collect_attachment_text_candidates_from_text_bodies(
                nested,
                &format!("{part_path}.m"),
                out
            );
        }
    }
}

fn collect_attachment_text_candidates_from_text_bodies<'a>(
    message: &'a Message<'a>,
    path: &str,
    out: &mut Vec<AttachmentScanCandidate<'a>>
) {
    for (idx, part) in message.text_bodies().enumerate() {
        if let Some(text) = decoded_part_text(part) {
            if !text.trim().is_empty() {
                let kind = CandidateKind::TextBody;
                let priority = attachment_scan_priority(kind, text);
                out.push(AttachmentScanCandidate {
                    scan_label: format!("text_body:text/plain@{path}.{idx}"),
                    text,
                    kind,
                    priority
                });
            }
        }
    }

    for (idx, part) in message.html_bodies().enumerate() {
        if let Some(text) = decoded_part_text(part) {
            if !text.trim().is_empty() {
                let kind = CandidateKind::TextBody;
                let priority = attachment_scan_priority(kind, text);
                out.push(AttachmentScanCandidate {
                    scan_label: format!("text_body:text/html@{path}.{idx}"),
                    text,
                    kind,
                    priority
                });
            }
        }
    }
}

fn part_mime_type(part: &MessagePart<'_>) -> String {
    if let Some(ct) = part.content_type() {
        let ctype = ct.ctype().trim().to_ascii_lowercase();
        if let Some(subtype) = ct.subtype() {
            return format!(
                "{}/{}",
                ctype,
                subtype.trim().to_ascii_lowercase()
            );
        }
        return ctype;
    }

    if part.is_text_html() {
        return "text/html".to_string();
    }
    if part.is_text() {
        return "text/plain".to_string();
    }
    if part.is_message() {
        return "message/rfc822".to_string();
    }

    "application/octet-stream".to_string()
}

fn should_scan_attachment_mime(mime: &str) -> bool {
    mime == "message/delivery-status"
        || mime == "message/rfc822"
        || mime.starts_with("text/")
}

fn classify_attachment_kind(mime: &str) -> CandidateKind {
    match mime {
        "message/delivery-status" => CandidateKind::DeliveryStatus,
        "text/rfc822-headers" => CandidateKind::OriginalHeaders,
        "message/rfc822" => CandidateKind::OriginalMessage,
        _ if mime.starts_with("text/") => CandidateKind::TextBody,
        _ => CandidateKind::Other
    }
}

fn attachment_scan_priority(
    kind: CandidateKind,
    text: &str
) -> u8 {
    match kind {
        CandidateKind::DeliveryStatus => 0,
        CandidateKind::OriginalHeaders => 1,
        CandidateKind::OriginalMessage => 2,
        CandidateKind::TextBody => {
            if looks_like_delivery_report(text) {
                3
            } else {
                4
            }
        }
        CandidateKind::Other => {
            if looks_like_delivery_report(text) {
                4
            } else {
                5
            }
        }
    }
}

fn decoded_part_text<'a>(part: &'a MessagePart<'a>) -> Option<&'a str> {
    if let Some(text) = part.text_contents() {
        if !text.is_empty() {
            return Some(text);
        }
    }

    let bytes = part.contents();
    if bytes.is_empty() {
        return None;
    }

    std::str::from_utf8(bytes).ok()
}

fn extract_hash_from_message_id_like_header(value: &str) -> Option<String> {
    // Prefer explicit RFC5322 message-id tokens enclosed in angle brackets.
    let mut start = 0usize;
    while let Some(open_rel) = value[start..].find('<') {
        let open = start + open_rel;
        if let Some(close_rel) = value[open + 1..].find('>') {
            let close = open + 1 + close_rel;
            if let Some(hash) = normalize_message_hash(&value[open..=close]) {
                return Some(hash);
            }
            start = close + 1;
        } else {
            break;
        }
    }

    // Fallback: parse whitespace-separated tokens.
    for token in value.split_whitespace() {
        if let Some(hash) = normalize_message_hash(token) {
            return Some(hash);
        }
    }

    normalize_message_hash(value)
}

fn normalize_message_hash(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|c| c == '<' || c == '>');
    let local_part = trimmed.split('@').next().unwrap_or("").trim();

    let hash: String =
        local_part.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

    if hash.is_empty() { None } else { Some(hash) }
}

fn parse_status_code(value: &str) -> Option<String> {
    let candidate = value.split_whitespace().next().unwrap_or("").trim();
    if is_valid_status_code(candidate) {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn is_valid_status_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 20
        && code.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn looks_like_delivery_report(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "final-recipient:",
        "original-recipient:",
        "diagnostic-code:",
        "report-type=delivery-status",
        "message/delivery-status",
        "undelivered",
        "mail delivery",
        "returned mail"
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn find_status_code_in_text(text: &str) -> Option<String> {
    text.split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .find(|token| {
            token.len() >= 5
                && token.matches('.').count() >= 2
                && is_valid_status_code(token)
                && (token.starts_with("2.")
                    || token.starts_with("4.")
                    || token.starts_with("5."))
        })
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_postfix_delivery_status_with_hash_from_rfc822_part() {
        let raw = concat!(
            "From: Mail Delivery System <mailer-daemon@claviron.app>\r\n",
            "Content-Type: multipart/report; report-type=delivery-status; boundary=\"B19557E240.1761150593/claviron.app\"\r\n",
            "\r\n",
            "--B19557E240.1761150593/claviron.app\r\n",
            "Content-Description: Delivery report\r\n",
            "Content-Type: message/delivery-status\r\n",
            "\r\n",
            "Reporting-MTA: dns; claviron.app\r\n",
            "X-Postfix-Queue-ID: B19557E240\r\n",
            "X-Postfix-Sender: rfc822; noreply@claviron.app\r\n",
            "Arrival-Date: Wed, 22 Oct 2025 19:29:52 +0300 (+03)\r\n",
            "\r\n",
            "Final-Recipient: rfc822; janedoe@gmail.com\r\n",
            "Original-Recipient: rfc822;janedoe@gmail.com\r\n",
            "Action: failed\r\n",
            "Status: 5.7.1\r\n",
            "Remote-MTA: dns; gmail-smtp-in.l.google.com\r\n",
            "Diagnostic-Code: smtp; 550-5.7.1 Gmail has detected\r\n",
            "    that this message is likely suspicious.\r\n",
            "    550 5.7.1 https://support.google.com/mail/answer/188131\r\n",
            "\r\n",
            "--B19557E240.1761150593/claviron.app\r\n",
            "Content-Type: message/rfc822\r\n",
            "\r\n",
            "From: noreply@claviron.app\r\n",
            "To: janedoe@gmail.com\r\n",
            "Message-ID: <c27335e4586d69311bb4668e9dc70bd5@claviron.app>\r\n",
            "Subject: test\r\n",
            "\r\n",
            "hello\r\n",
            "\r\n",
            "--B19557E240.1761150593/claviron.app--\r\n",
        );

        let parsed = parse_bounce_report_detailed(raw.as_bytes())
            .expect("postfix DSN sample should parse");

        assert_eq!(parsed.hash, "c27335e4586d69311bb4668e9dc70bd5");
        assert_eq!(parsed.status_code, "5.7.1");
        assert_eq!(parsed.action.as_deref(), Some("failed"));
        assert_eq!(parsed.recipient.as_deref(), Some("janedoe@gmail.com"));
        assert!(
            parsed
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("550-5.7.1")
        );
    }

    #[test]
    fn returns_missing_hash_when_dsn_has_no_message_id_reference() {
        let raw = concat!(
            "Content-Type: message/delivery-status\r\n",
            "\r\n",
            "Final-Recipient: rfc822; user@example.com\r\n",
            "Action: failed\r\n",
            "Status: 5.7.1\r\n",
            "Diagnostic-Code: smtp; 550 5.7.1 blocked\r\n",
        );

        let err = parse_bounce_report_detailed(raw.as_bytes())
            .expect_err("missing hash should fail");
        assert_eq!(err, ParserError::MissingHash);
    }

    #[test]
    fn parses_notification_eml_fixture() {
        let raw = include_bytes!("../../../../tests/bounces/notification.eml");
        let parsed = parse_bounce_report_detailed(raw)
            .expect("notification fixture should parse");

        assert_eq!(parsed.hash, "4a22e0f0aa194d6833c619097380befa");
        assert_eq!(parsed.status_code, "5.5.0");
        assert_eq!(parsed.action.as_deref(), Some("failed"));
        assert_eq!(
            parsed.recipient.as_deref(),
            Some("dummyuser08585@hotmail.com")
        );
    }

    #[test]
    fn parses_inbox_returned_eml_fixture() {
        let raw =
            include_bytes!("../../../../tests/bounces/inbox.returned.eml");
        let parsed = parse_bounce_report_detailed(raw)
            .expect("imap inbox-returned fixture should parse");

        assert_eq!(parsed.hash, "44b54b9b9f739ca1a82e91aab5200e0e");
        assert_eq!(parsed.status_code, "5.7.1");
        assert_eq!(parsed.action.as_deref(), Some("failed"));
        assert_eq!(parsed.recipient.as_deref(), Some("member09@gmail.com"));
    }

    #[test]
    fn parses_outlook_bounce_eml_fixture() {
        let raw =
            include_bytes!("../../../../tests/bounces/outlook.bounce.eml");
        let parsed = parse_bounce_report_detailed(raw)
            .expect("outlook bounce fixture should parse");

        assert_eq!(parsed.hash, "c27335e4586d69311bb4668e9dc70bd5");
        assert_eq!(parsed.status_code, "5.2.1");
        assert_eq!(parsed.action.as_deref(), Some("failed"));
        assert_eq!(
            parsed.recipient.as_deref(),
            Some("sx1300624@steanne-stlouis.fr")
        );
    }

    #[test]
    fn does_not_take_hash_from_non_original_sections() {
        let raw = concat!(
            "Message-ID: <bounce-message-id@example.net>\r\n",
            "References: <orig-hash-should-not-be-read-from-top-level@claviron.app>\r\n",
            "Content-Type: message/delivery-status\r\n",
            "\r\n",
            "Final-Recipient: rfc822; user@example.com\r\n",
            "Action: failed\r\n",
            "Status: 5.7.1\r\n",
            "Diagnostic-Code: smtp; 550 5.7.1 blocked\r\n",
        );

        let err = parse_bounce_report_detailed(raw.as_bytes()).expect_err(
            "hash should not be accepted outside original sections"
        );
        assert_eq!(err, ParserError::MissingHash);
    }
}
