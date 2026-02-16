#!/usr/bin/env bash

set -euo pipefail

# Test script:
# - Sources `.env` from repo root (or BOUNCER_ENV_FILE)
# - Downloads demo bounce messages from IMAP folder into tests/bounces
# - Uses bouncer-tools/imap_fetcher with safe defaults (BODY.PEEK, no \Seen)

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

IMAP_HOST="${IMAP_HOST:-mail.claviron.app}"
IMAP_PORT="${IMAP_PORT:-993}"
IMAP_USER="${IMAP_USER:-}"
IMAP_PASS="${IMAP_PASS:-}"
IMAP_MAILBOX="${IMAP_MAILBOX:-INBOX}"
IMAP_SEARCH="${IMAP_SEARCH:-ALL}"
IMAP_LIMIT="${IMAP_LIMIT:-50}"
IMAP_OUTPUT_DIR="${IMAP_OUTPUT_DIR:-tests/bounces}"

if [[ -z "$IMAP_USER" ]]; then
  echo "error: IMAP_USER missing in $ENV_FILE" >&2
  exit 1
fi

if [[ -z "$IMAP_PASS" ]]; then
  echo "error: IMAP_PASS missing in $ENV_FILE" >&2
  exit 1
fi

if ! [[ "$IMAP_PORT" =~ ^[0-9]+$ ]]; then
  echo "error: IMAP_PORT must be numeric (got: $IMAP_PORT)" >&2
  exit 1
fi

if ! [[ "$IMAP_LIMIT" =~ ^[0-9]+$ ]]; then
  echo "error: IMAP_LIMIT must be numeric (got: $IMAP_LIMIT)" >&2
  exit 1
fi

echo "imap demo fetch start: host=$IMAP_HOST user=$IMAP_USER mailbox=$IMAP_MAILBOX search=$IMAP_SEARCH limit=$IMAP_LIMIT output=$IMAP_OUTPUT_DIR"

cd "$ROOT_DIR"
cargo run -p bouncer-tools --bin imap_fetcher -- \
  --host "$IMAP_HOST" \
  --port "$IMAP_PORT" \
  --user "$IMAP_USER" \
  --pass "$IMAP_PASS" \
  --mailbox "$IMAP_MAILBOX" \
  --search "$IMAP_SEARCH" \
  --limit "$IMAP_LIMIT" \
  --output-dir "$IMAP_OUTPUT_DIR"

