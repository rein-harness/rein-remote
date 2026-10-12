#!/bin/sh
# Builds rein-remote for Linux (the Docker machine's own architecture) in a Rust container, the cargo
# registry and the build kept in volumes between runs, then the small image the fleet test runs:
# rein-remote and curl. Run from anywhere: tests/relay-fleet/build.sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
docker run --rm -v "$root":/src -w /src \
  -v rr-fleet-registry:/usr/local/cargo/registry -v rr-fleet-target:/target -e CARGO_TARGET_DIR=/target \
  rust:1-bookworm sh -c 'apt-get update -qq && apt-get install -y -qq cmake > /dev/null && cargo build --release -j 2 -p rr-server && cp /target/release/rein-remote /src/tests/relay-fleet/rein-remote.linux'
docker build -q -t rr-fleet -f "$root/tests/relay-fleet/Dockerfile" "$root/tests/relay-fleet"
rm -f "$root/tests/relay-fleet/rein-remote.linux"
