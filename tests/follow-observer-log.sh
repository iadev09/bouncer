#!/usr/bin/env bash

set -euo pipefail

# Realtime observer log follower (systemd/journald).
# Shows bouncer-observer logs and optional focus filter for parser/publisher flow.

UNIT="${OBSERVER_UNIT:-bouncer-observer.service}"
SINCE="${OBSERVER_LOG_SINCE:-10m}"
MODE="${OBSERVER_LOG_MODE:-focus}" # focus | all

usage() {
  cat <<'EOF'
usage: tools/follow-observer-log.sh [--unit NAME] [--since WINDOW] [--mode focus|all]

Examples:
  bash tools/follow-observer-log.sh
  bash tools/follow-observer-log.sh --unit bouncer-observer.service --since 30m --mode all
EOF
}

while (($# > 0)); do
  case "$1" in
    --unit)
      UNIT="${2:-}"
      shift 2
      ;;
    --since)
      SINCE="${2:-}"
      shift 2
      ;;
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if ! command -v journalctl >/dev/null 2>&1; then
  echo "error: journalctl not found (this script is for Linux/systemd hosts)" >&2
  exit 1
fi

echo "observer log follow: unit=$UNIT since=$SINCE mode=$MODE"

if [[ "$MODE" == "all" ]]; then
  exec journalctl -u "$UNIT" --since "$SINCE" -f -o cat
fi

if command -v rg >/dev/null 2>&1; then
  exec journalctl -u "$UNIT" --since "$SINCE" -f -o cat \
    | rg --line-buffered -i "observer connected|udp listener ready|smtp log without known queue mapping|event queue is full|failed to publish|register frame failed|observer_event|queue_id=|message-id=|dsn=|status="
fi

exec journalctl -u "$UNIT" --since "$SINCE" -f -o cat \
  | grep --line-buffered -Ei "observer connected|udp listener ready|smtp log without known queue mapping|event queue is full|failed to publish|register frame failed|observer_event|queue_id=|message-id=|dsn=|status="

