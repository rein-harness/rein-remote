# The image the NAT test runs: rein-remote for Linux (built by build.sh) and the tools to make NAT
# routers out of containers (ip, iptables, tc).
FROM debian:bookworm-slim
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends iproute2 iptables iputils-ping procps ca-certificates > /dev/null \
    && rm -rf /var/lib/apt/lists/*
COPY rein-remote.linux /usr/local/bin/rein-remote
