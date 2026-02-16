#!/usr/bin/env bash

set -euo pipefail

SERVER_ADDR="${BOUNCER_SERVER_ADDR:-127.0.0.1:2147}"
MAIL_FROM="${BOUNCER_FROM:-sender@example.com}"
MAIL_TO="${BOUNCER_TO:-bounces@example.com}"
FIXTURE_DIR="${BOUNCER_FIXTURE_DIR:-tests/bounces}"
LIMIT="${BOUNCER_LIMIT:-10}"
CLIENT_BIN="${BOUNCER_CLIENT_BIN:-target/debug/bouncer-client}"
BUILD_CLIENT=1

usage() {
  cat <<'EOF'
usage: tests/client-test.sh [options] [file1.eml file2.eml ...]

Options:
  --server ADDR       bouncer-server address (default: 127.0.0.1:2147)
  --from ADDRESS      sender header value (default: sender@example.com)
  --to ADDRESS        recipient header value (default: bounces@example.com)
  --dir PATH          fixture dir (default: tests/bounces)
  --limit N           max files from --dir (default: 10, 0=all)
  --client-bin PATH   bouncer-client binary path (default: target/debug/bouncer-client)
  --no-build          skip cargo build step
  -h, --help          show this help

Examples:
  tests/client-test.sh --limit 50
  tests/client-test.sh tests/bounces/uid-6634.eml
  tests/client-test.sh --server 10.8.0.1:2147 --from noreply@claviron.app --to poster@claviron.app --limit 20
EOF
}

explicit_files=()
while (($# > 0)); do
  case "$1" in
    --server)
      SERVER_ADDR="${2:-}"
      shift 2
      ;;
    --from)
      MAIL_FROM="${2:-}"
      shift 2
      ;;
    --to)
      MAIL_TO="${2:-}"
      shift 2
      ;;
    --dir)
      FIXTURE_DIR="${2:-}"
      shift 2
      ;;
    --limit)
      LIMIT="${2:-}"
      shift 2
      ;;
    --client-bin)
      CLIENT_BIN="${2:-}"
      shift 2
      ;;
    --no-build)
      BUILD_CLIENT=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      while (($# > 0)); do
        explicit_files+=("$1")
        shift
      done
      ;;
    *)
      explicit_files+=("$1")
      shift
      ;;
  esac
done

if ! [[ "$LIMIT" =~ ^[0-9]+$ ]]; then
  echo "error: --limit must be an integer (got: $LIMIT)" >&2
  exit 2
fi

if (( BUILD_CLIENT == 1 )); then
  cargo build -p bouncer-client >/dev/null
fi

if [[ ! -x "$CLIENT_BIN" ]]; then
  echo "error: bouncer-client binary not found/executable: $CLIENT_BIN" >&2
  exit 2
fi

files=()
if ((${#explicit_files[@]} > 0)); then
  files=("${explicit_files[@]}")
else
  if [[ ! -d "$FIXTURE_DIR" ]]; then
    echo "error: fixture dir does not exist: $FIXTURE_DIR" >&2
    exit 2
  fi
  mapfile -t files < <(find "$FIXTURE_DIR" -maxdepth 1 -type f -name '*.eml' | sort -V)
  if (( LIMIT > 0 && ${#files[@]} > LIMIT )); then
    files=("${files[@]: -LIMIT}")
  fi
fi

if ((${#files[@]} == 0)); then
  echo "no .eml file selected"
  exit 0
fi

echo "client test started: server=$SERVER_ADDR from=$MAIL_FROM to=$MAIL_TO files=${#files[@]}"

ok=0
fail=0
idx=0
for file in "${files[@]}"; do
  idx=$((idx + 1))
  if [[ ! -f "$file" ]]; then
    echo "[$idx/${#files[@]}] missing file: $file"
    fail=$((fail + 1))
    continue
  fi

  echo "[$idx/${#files[@]}] send: $file"
  if cat "$file" | "$CLIENT_BIN" --server "$SERVER_ADDR" --from "$MAIL_FROM" --to "$MAIL_TO"; then
    ok=$((ok + 1))
  else
    fail=$((fail + 1))
  fi
done

echo "client test finished: ok=$ok fail=$fail total=${#files[@]}"
if (( fail > 0 )); then
  exit 1
fi
