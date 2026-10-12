#!/bin/sh
# rein-remote for Linux with SAML (samael needs libxml2, libxmlsec1 and clang), built in a Rust container
# with the registry and the build kept in volumes between runs; then the host image.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
docker run --rm -v "$root":/src -w /src \
  -v rr-idp-registry:/usr/local/cargo/registry -v rr-idp-target:/target -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}" \
  rust:1-bookworm sh -c 'apt-get update -qq && apt-get install -y -qq libxml2-dev libxmlsec1-dev libxmlsec1-openssl libclang-dev pkg-config libssl-dev cmake nasm >/dev/null \
    && cargo build --release -p rr-server --features saml && cp /target/release/rein-remote /src/tests/idp/rein-remote.linux'
docker build -q -t rr-idp-host -f "$root/tests/idp/host/Dockerfile" "$root/tests/idp" >/dev/null
