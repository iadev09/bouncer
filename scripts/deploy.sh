#!/usr/bin/env bash

set -e
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
BASE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd -P)"

cd "${BASE_DIR}" || exit 1

show_help() {
  cat <<'USAGE'
Usage: ./scripts/deploy.sh [observer|server|client|journal|all] [debug|release] [target_host]
Deploy bouncer observer/server/client/journal binaries to a target host.

Parameters:
  observer|server|client|journal|all  Specific app to deploy (default: all)
  debug|release               Build mode (default: release)
  target_host                 Target host (default: current hostname)

Examples:
  ./scripts/deploy.sh
  ./scripts/deploy.sh observer
  ./scripts/deploy.sh server debug
  ./scripts/deploy.sh all release mailhost1
USAGE
}

if [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  show_help
  exit 0
fi

if [ -z "${SERVANT_HOME}" ]; then
  echo "Error: SERVANT_HOME environment variable is not set." >&2
  exit 1
fi

DEPLOY_SCRIPT="${SERVANT_HOME}/bin/rust-deploy.sh"

if ! [ -x "${DEPLOY_SCRIPT}" ]; then
  echo "Error: ${DEPLOY_SCRIPT} not found or not executable" >&2
  exit 1
fi

HOSTNAME_SHORT="$(hostname -s)"
DEPLOY_TARGET="all"
BUILD_MODE="release"
TARGET_HOST="${HOSTNAME_SHORT}"

if [ -n "$1" ]; then
  if [ "$1" = "debug" ] || [ "$1" = "release" ]; then
    DEPLOY_TARGET="all"
    BUILD_MODE="$1"
    TARGET_HOST="${2:-${HOSTNAME_SHORT}}"
  else
    DEPLOY_TARGET="$1"
    BUILD_MODE="${2:-release}"
    TARGET_HOST="${3:-${HOSTNAME_SHORT}}"
  fi
fi

if [ "${BUILD_MODE}" != "debug" ] && [ "${BUILD_MODE}" != "release" ]; then
  echo "Error: Invalid build mode '${BUILD_MODE}'. Use 'debug' or 'release'." >&2
  exit 1
fi

if ! ping -c 1 "${TARGET_HOST}" >/dev/null 2>&1; then
  echo "Error: Target host '${TARGET_HOST}' is not reachable." >&2
  exit 1
fi

restart_server_on_de01() {
  case "${TARGET_HOST}" in
    de01|de01.*)
      echo "Ensuring bouncer-server.service is restarted on ${TARGET_HOST}"
      if ssh "root@${TARGET_HOST}" "systemctl restart bouncer-server.service && systemctl is-active bouncer-server.service"; then
        echo "bouncer-server.service restart confirmed on ${TARGET_HOST}."
      else
        echo "Warning: could not confirm bouncer-server.service restart on ${TARGET_HOST}" >&2
      fi
      ;;
  esac
}

deploy_observer() {
  echo "Deploying bouncer-observer to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" "${DEPLOY_SCRIPT}" "${TARGET_HOST}" "bouncer-observer"
}

deploy_server() {
  echo "Deploying bouncer-server to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" "${DEPLOY_SCRIPT}" "${TARGET_HOST}" "bouncer-server"
  restart_server_on_de01
}

deploy_client() {
  echo "Deploying bouncer-client to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" "${DEPLOY_SCRIPT}" "${TARGET_HOST}" "bouncer-client"
}

deploy_journal() {
  echo "Deploying bouncer-journal to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" "${DEPLOY_SCRIPT}" "${TARGET_HOST}" "bouncer-journal"
}

deploy_all() {
  echo "Deploying all to ${TARGET_HOST} (${BUILD_MODE})"
#  deploy_observer
  deploy_journal
  deploy_server
  deploy_client
  echo "All selected binaries deployed successfully."
}

case "${DEPLOY_TARGET}" in
  all)
    deploy_all
    ;;
  observer|bouncer-observer)
    deploy_observer
    ;;
  server|bouncer-server)
    deploy_server
    ;;
  client|bouncer-client)
    deploy_client
    ;;
  journal|bouncer-journal)
    deploy_journal
    ;;
  *)
    echo "Error: Unknown deploy target '${DEPLOY_TARGET}'." >&2
    echo "Valid options: observer, server, client, journal, all" >&2
    exit 1
    ;;
esac
