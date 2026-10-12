# The image the relay-fleet test runs: rein-remote for Linux (built by build.sh) and curl.
FROM debian:bookworm-slim
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends ca-certificates curl procps > /dev/null \
    && rm -rf /var/lib/apt/lists/*
COPY rein-remote.linux /usr/local/bin/rein-remote
