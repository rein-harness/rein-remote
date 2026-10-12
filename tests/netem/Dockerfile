# Phase 4's measurements: rein-remote (built from this repo) and headless Chromium in one Linux
# container, so the barcode's clock is shared, with tc netem on the loopback for a bad link.
# Built on the same Debian as the runtime (its glibc).
FROM rust:1-bookworm AS rr
RUN apt-get update && apt-get install -y --no-install-recommends cmake nasm && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . /src/
RUN cargo build --release -p rr-server && cp target/release/rein-remote /rein-remote

FROM node:22-bookworm
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends chromium fonts-dejavu-core iproute2 procps jq \
 && rm -rf /var/lib/apt/lists/*
COPY --from=rr /rein-remote /usr/local/bin/rein-remote
COPY tools/ /m/tools/
COPY tests/netem/scenarios.sh /m/
ENV CHROME=/usr/bin/chromium
WORKDIR /m
CMD ["/m/scenarios.sh"]
