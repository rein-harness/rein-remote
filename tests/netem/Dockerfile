# Phase 4's measurements: rein-remote (built from this repo), its native viewer and headless Chromium in one Linux
# container, so the barcode's clock is shared, with tc netem on the loopback for a bad link.
# rein-remote is built by run.sh (cargo's registry and build kept in Docker volumes) on the same
# Debian as the runtime (its glibc).
FROM node:22-bookworm
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends chromium fonts-dejavu-core iproute2 procps jq \
 && rm -rf /var/lib/apt/lists/*
COPY tests/netem/rein-remote.linux /usr/local/bin/rein-remote
COPY tests/netem/rein-remote-viewer.linux /usr/local/bin/rein-remote-viewer
COPY tools/ /m/tools/
COPY tests/netem/scenarios.sh /m/
ENV CHROME=/usr/bin/chromium
WORKDIR /m
CMD ["/m/scenarios.sh"]
