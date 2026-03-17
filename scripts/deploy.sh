#!/usr/bin/env bash

set -e
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
BASE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd -P)"

cd "${BASE_DIR}" || exit 1

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  echo "Usage: $0 [options]"
  echo "Deploy bouncer binaries to a target host."
  echo ""
  echo "Options:"
  echo "  --bin <name>            Deploy one or more specific binaries (repeatable)."
  echo "  --all                   Deploy all binaries (default if no --bin is provided)."
  echo "  --mode <debug|release>   Build mode (default: release)."
  echo "  --host <target_host>     Target deployment host (default: current hostname)."
  echo "  --native                Use native CPU optimizations and local-only deployment."
  echo "  --dry-run               Print deployment commands without executing."
  echo "  -h, --help              Print this help message."
  echo ""
  echo "Binaries:"
  echo "  bouncer-observer"
  echo "  bouncer-server"
  echo "  bouncer-client"
  echo "  bouncer-journal"
  echo "  bounce-delivery"
  echo ""
  echo "Examples:"
  echo "  $0 --all"
  echo "  $0 --bin bouncer-server --mode debug"
  echo "  $0 --bin bouncer-journal --host mailhost1"
  echo "  $0 --all --mode release --host mailhost1"
  echo "  $0 --bin bouncer-server --native"
  echo ""
  exit 0
fi

if [ -z "${SERVANT_HOME:-}" ]; then
  echo "Error: SERVANT_HOME environment variable is not set." >&2
  exit 1
fi

DEPLOY_SCRIPT="${SERVANT_HOME}/bin/rust-deploy.sh"

if ! [ -x "${DEPLOY_SCRIPT}" ]; then
  echo "Error: ${DEPLOY_SCRIPT} not found or not executable" >&2
  exit 1
fi

HOSTNAME_SHORT="$(hostname -s)"

# Defaults
BUILD_MODE="release"
TARGET_HOST="${HOSTNAME_SHORT}"
NATIVE_BUILD="false"
DRY_RUN="false"
BINS=()
DEPLOY_ALL="false"

# Parse flags
while [ $# -gt 0 ]; do
  case "$1" in
  --bin)
    shift
    if [ -z "${1:-}" ]; then
      echo "Error: --bin requires a binary name argument." >&2
      exit 1
    fi
    BINS+=("$1")
    shift
    ;;
  --all)
    BINS=()
    DEPLOY_ALL="true"
    shift
    ;;
  --mode)
    shift
    if [ "${1:-}" != "debug" ] && [ "${1:-}" != "release" ]; then
      echo "Error: --mode must be 'debug' or 'release'." >&2
      exit 1
    fi
    BUILD_MODE="$1"
    shift
    ;;
  --host)
    shift
    if [ -z "${1:-}" ]; then
      echo "Error: --host requires a host argument." >&2
      exit 1
    fi
    TARGET_HOST="$1"
    shift
    ;;
  --native)
    NATIVE_BUILD="true"
    shift
    ;;
  --dry-run)
    DRY_RUN="true"
    shift
    ;;
  -h | --help)
    echo "Usage: $0 [options]"
    exit 0
    ;;
  *)
    echo "Error: Unknown option: $1" >&2
    exit 1
    ;;
  esac
done

if [ "${DEPLOY_ALL}" != "true" ] && [ "${#BINS[@]}" -eq 0 ]; then
  DEPLOY_ALL="true"
fi

if [ "${BUILD_MODE}" != "debug" ] && [ "${BUILD_MODE}" != "release" ]; then
  echo "Error: Invalid build mode '${BUILD_MODE}'. Use 'debug' or 'release'." >&2
  exit 1
fi

# Validate native host
if [ "${NATIVE_BUILD}" = "true" ]; then
  LOCAL_OK="false"
  case "${TARGET_HOST}" in
  "${HOSTNAME_SHORT}" | localhost | 127.0.0.1 | ::1)
    LOCAL_OK="true"
    ;;
  esac
  if [ "${LOCAL_OK}" != "true" ]; then
    echo "Error: --native builds are CPU-specific and cannot be deployed to remote hosts." >&2
    exit 1
  fi
  NATIVE_RUSTFLAGS="-C target-cpu=native ${RUSTFLAGS:-}"
  NATIVE_CARGO_TARGET_DIR="${BASE_DIR}/target-native"
fi

# Only ping if non-local and not dry-run
case "${TARGET_HOST}" in
"${HOSTNAME_SHORT}" | localhost | 127.0.0.1 | ::1) ;;
*)
  if [ "${DRY_RUN}" != "true" ]; then
    if ! ping -c 1 "${TARGET_HOST}" >/dev/null 2>&1; then
      echo "Error: Target host '${TARGET_HOST}' is not reachable." >&2
      exit 1
    fi
  fi
  ;;
esac

run_deploy() {
  local bin="$1"

  if [ "${NATIVE_BUILD}" = "true" ]; then
    USE_WORKSPACE=1 RUSTFLAGS="${NATIVE_RUSTFLAGS}" CARGO_TARGET_DIR="${NATIVE_CARGO_TARGET_DIR}" \
      "${DEPLOY_SCRIPT}" "${TARGET_HOST}" "${bin}"
  else
    USE_WORKSPACE=1 "${DEPLOY_SCRIPT}" "${TARGET_HOST}" "${bin}"
  fi
}


# Deployment functions

deploy_bouncer_observer() {
  if [ "${DRY_RUN}" = "true" ]; then
    echo "BUILD_MODE=\"${BUILD_MODE}\" ${DEPLOY_SCRIPT} \"${TARGET_HOST}\" bouncer-observer"
    return 0
  fi
  echo "Deploying bouncer-observer to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" run_deploy "bouncer-observer"
}

deploy_bouncer_server() {
  if [ "${DRY_RUN}" = "true" ]; then
    echo "BUILD_MODE=\"${BUILD_MODE}\" ${DEPLOY_SCRIPT} \"${TARGET_HOST}\" bouncer-server"
    return 0
  fi
  echo "Deploying bouncer-server to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" run_deploy "bouncer-server"
}

deploy_bouncer_client() {
  if [ "${DRY_RUN}" = "true" ]; then
    echo "BUILD_MODE=\"${BUILD_MODE}\" ${DEPLOY_SCRIPT} \"${TARGET_HOST}\" bouncer-client"
    return 0
  fi
  echo "Deploying bouncer-client to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" run_deploy "bouncer-client"
}

deploy_bouncer_journal() {
  if [ "${DRY_RUN}" = "true" ]; then
    echo "BUILD_MODE=\"${BUILD_MODE}\" ${DEPLOY_SCRIPT} \"${TARGET_HOST}\" bouncer-journal"
    return 0
  fi
  echo "Deploying bouncer-journal to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" run_deploy "bouncer-journal"
}

deploy_bounce_delivery() {
  if [ "${DRY_RUN}" = "true" ]; then
    echo "BUILD_MODE=\"${BUILD_MODE}\" ${DEPLOY_SCRIPT} \"${TARGET_HOST}\" bounce-delivery"
    return 0
  fi
  echo "Deploying bounce-delivery to ${TARGET_HOST} (${BUILD_MODE})"
  BUILD_MODE="${BUILD_MODE}" run_deploy "bounce-delivery"
}

deploy_all() {
  echo "Deploying all bouncer binaries to ${TARGET_HOST} (${BUILD_MODE})"
  deploy_bouncer_server
  deploy_bouncer_journal
  deploy_bounce_delivery
  # deploy_bouncer_observer
  # deploy_bouncer_client
  echo "✅ All selected binaries deployed successfully."
}

# Dispatch
if [ "${DEPLOY_ALL}" = "true" ]; then
  deploy_all
else
  for bin in "${BINS[@]}"; do
    case "${bin}" in
    bouncer-observer)
      deploy_bouncer_observer
      ;;
    bouncer-server)
      deploy_bouncer_server
      ;;
    bouncer-client)
      deploy_bouncer_client
      ;;
    bouncer-journal)
      deploy_bouncer_journal
      ;;
    bounce-delivery)
      deploy_bounce_delivery
      ;;
    *)
      echo "Error: Unknown binary name: ${bin}" >&2
      echo "Valid options: bouncer-observer, bouncer-server, bouncer-client, bouncer-journal, bounce-delivery, all" >&2
      exit 1
      ;;
    esac
  done
fi
