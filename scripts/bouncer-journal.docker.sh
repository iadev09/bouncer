#!/usr/bin/env bash

# NOTE: bouncer-journal links against systemd/journald via `libsystemd-sys` (Linux only).
# Build it inside a Linux environment (e.g. Docker). Native builds on macOS/Windows or
# cross-compiles without a proper sysroot will fail with pkg-config/libsystemd errors, e.g.:
#
#  pkg_config could not find "libsystemd": pkg-config has not been configured to support cross-compilation.
#
# Use a Debian/Ubuntu-based image and install the dev package:
#   apt-get update && apt-get install -y pkg-config libsystemd-dev

set -e
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
BASE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd -P)"

cd "${BASE_DIR}" || exit 1

docker build -t bouncer-journal -f Dockerfile.journal .

docker run --rm -it --name bouncer-journal-test bouncer-journal
