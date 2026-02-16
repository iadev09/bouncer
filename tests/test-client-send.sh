#!/usr/bin/env bash

set -euo pipefail

# Test script:
# - Sources `.env` from repo root (or BOUNCER_ENV_FILE)
# - Sends `.eml` fixtures to bouncer-server via bouncer-client (stdin pipe)
# - Reuses tests/client-test.sh runner

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

SERVER_ADDR="${BOUNCER_SERVER_ADDR:-127.0.0.1:2147}"
MAIL_FROM="${BOUNCER_FROM:-sender@example.com}"
MAIL_TO="${BOUNCER_TO:-bounces@example.com}"
FIXTURE_DIR="${BOUNCER_FIXTURE_DIR:-tests/bounces}"
LIMIT="${BOUNCER_LIMIT:-10}"

if ! [[ "$LIMIT" =~ ^[0-9]+$ ]]; then
  echo "error: BOUNCER_LIMIT must be numeric (got: $LIMIT)" >&2
  exit 1
fi

echo "client send test start: server=$SERVER_ADDR from=$MAIL_FROM to=$MAIL_TO dir=$FIXTURE_DIR limit=$LIMIT"

cd "$ROOT_DIR"
bash tests/client-test.sh \
  --server "$SERVER_ADDR" \
  --from "$MAIL_FROM" \
  --to "$MAIL_TO" \
  --dir "$FIXTURE_DIR" \
  --limit "$LIMIT"

