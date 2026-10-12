#!/bin/bash
# Phase 4's link measurements, one command: builds the image from this repo, runs every link and
# transport (scenarios.sh), prints one JSON line each. SECS=15 MOTION=full RES=1280x720 by default (MOTION=none:
# the test pattern's moving block on a still background). Each run's logs: LOGS (tests/netem/logs).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
docker run --rm -v "$ROOT":/src -w /src \
  -v rr-netem-registry:/usr/local/cargo/registry -v rr-netem-target:/target -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
  rust:1-bookworm sh -c 'apt-get update -qq && apt-get install -y -qq cmake nasm > /dev/null && cargo build --release -p rr-server && cp /target/release/rein-remote /src/tests/netem/rein-remote.linux' >&2
docker build -q -f "$ROOT/tests/netem/Dockerfile" -t rr-netem "$ROOT" >/dev/null
rm -f "$ROOT/tests/netem/rein-remote.linux"
mkdir -p "${LOGS:-$ROOT/tests/netem/logs}"
docker run --rm -v "${LOGS:-$ROOT/tests/netem/logs}":/logs --cap-add NET_ADMIN --shm-size 1g -e SECS="${SECS:-15}" -e MOTION="${MOTION:-full}" -e RES="${RES:-1280x720}" rr-netem
