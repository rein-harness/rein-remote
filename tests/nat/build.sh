#!/bin/sh
# Builds rein-remote for Linux (the Docker machine's own architecture) in a Rust container, with the
# cargo registry and the build kept in volumes between runs, then the small image the NAT test runs:
# rein-remote plus ip, iptables and tc. Run from anywhere: tests/nat/build.sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
docker run --rm -v "$root":/src -w /src \
  -v rr-punch-registry:/usr/local/cargo/registry -v rr-punch-target:/target -e CARGO_TARGET_DIR=/target \
  rust:1-bookworm sh -c 'cargo build --release -p rr-server && cp /target/release/rein-remote /src/tests/nat/rein-remote.linux'
docker build -q -t rr-punch -f "$root/tests/nat/Dockerfile" "$root/tests/nat"
rm -f "$root/tests/nat/rein-remote.linux"
