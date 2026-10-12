#!/bin/bash
# Phase 4's link measurements, one command: builds the image from this repo, runs every link and
# transport (scenarios.sh), prints one JSON line each. SECS=15 MOTION=full by default (MOTION=none:
# the test pattern's moving block on a still background).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
docker build -q -f "$ROOT/tests/netem/Dockerfile" -t rr-netem "$ROOT" >/dev/null
docker run --rm --cap-add NET_ADMIN --shm-size 1g -e SECS="${SECS:-15}" -e MOTION="${MOTION:-full}" rr-netem
