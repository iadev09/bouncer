#!/usr/bin/env bash

set -euo pipefail

# Test script:
# - Sources `.env` from repo root (or BOUNCER_ENV_FILE)
# - Sends synthetic postfix cleanup + smtp syslog lines to observer UDP port
# - Triggers queue_id -> hash correlation path in bouncer-observer

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${BOUNCER_ENV_FILE:-$ROOT_DIR/.env}"

if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: env file not found: $ENV_FILE" >&2
  exit 1
fi

set -a
# shellcheck source=/dev/null
source "$ENV_FILE"
set +a

UDP_HOST="${OBSERVER_UDP_HOST:-127.0.0.1}"
UDP_PORT="${OBSERVER_UDP_PORT:-5140}"
MAIL_HOST="${OBSERVER_MAIL_HOST:-mail.claviron.app}"
QUEUE_ID="${OBSERVER_QUEUE_ID:-Q${RANDOM}${RANDOM}}"
MESSAGE_HASH="${OBSERVER_MESSAGE_HASH:-0123456789abcdef0123456789abcdef}"
MESSAGE_DOMAIN="${OBSERVER_MESSAGE_DOMAIN:-claviron.app}"
RECIPIENT="${OBSERVER_RECIPIENT:-noreply@claviron.app}"
DSN="${OBSERVER_DSN:-5.7.1}"
STATUS="${OBSERVER_STATUS:-bounced}"

if ! [[ "$UDP_PORT" =~ ^[0-9]+$ ]]; then
  echo "error: OBSERVER_UDP_PORT must be numeric (got: $UDP_PORT)" >&2
  exit 1
fi

if ! [[ "$QUEUE_ID" =~ ^[A-Za-z0-9]+$ ]]; then
  echo "error: OBSERVER_QUEUE_ID must be alphanumeric (got: $QUEUE_ID)" >&2
  exit 1
fi

if [[ ${#MESSAGE_HASH} -ne 32 ]] || ! [[ "$MESSAGE_HASH" =~ ^[A-Za-z0-9]+$ ]]; then
  echo "error: OBSERVER_MESSAGE_HASH must be 32 alnum chars (got: $MESSAGE_HASH)" >&2
  exit 1
fi

send_udp_line() {
  local line="$1"
  if command -v nc >/dev/null 2>&1; then
    printf '%s\n' "$line" | nc -u -w 1 "$UDP_HOST" "$UDP_PORT"
  else
    printf '%s\n' "$line" >"/dev/udp/$UDP_HOST/$UDP_PORT"
  fi
}

cleanup_line="<134>Feb 16 10:00:01 ${MAIL_HOST} postfix/cleanup[13001]: ${QUEUE_ID}: message-id=<${MESSAGE_HASH}@${MESSAGE_DOMAIN}>"
smtp_line="<134>Feb 16 10:00:02 ${MAIL_HOST} postfix/smtp[13002]: ${QUEUE_ID}: to=<${RECIPIENT}>, relay=mx.test[203.0.113.25]:25, delay=0.48, delays=0.01/0.01/0.31/0.15, dsn=${DSN}, status=${STATUS} (host mx.test[203.0.113.25] said: 550 5.7.1 Rejected)"

echo "observer udp test start: target=${UDP_HOST}:${UDP_PORT} queue_id=${QUEUE_ID} hash=${MESSAGE_HASH} recipient=${RECIPIENT} dsn=${DSN} status=${STATUS}"
send_udp_line "$cleanup_line"
sleep 0.1
send_udp_line "$smtp_line"
echo "observer udp test sent: 2 lines (cleanup + smtp)"

