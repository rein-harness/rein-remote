#!/bin/bash
# Read-only copies of the two repos into the Docker build context (nothing is written back):
# this repo, and Rein from REIN_SRC (default: ../rein-harness next to this repo).
set -euo pipefail
D="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$D/../../.." && pwd)"
mkdir -p "$D/ctx"
rsync -a --delete --exclude target --exclude .git --exclude tests/e2e "$ROOT/" "$D/ctx/rein-remote/"
rsync -a --delete --exclude node_modules --exclude .git --exclude site --exclude dist "${REIN_SRC:-$ROOT/../rein-harness}/" "$D/ctx/rein-harness/"
rsync -a --delete "$D/host/" "$D/ctx/host/"
rsync -a --delete "$D/phone/" "$D/ctx/phone/"
