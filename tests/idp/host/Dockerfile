# The machine to reach: an xterm on Xvfb, and rein-remote (built by ../build.sh, with SAML) twice: one
# signs viewers in with OIDC (:7979), the other with SAML (:7980), both against Keycloak at idp.test.
FROM debian:bookworm-slim
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      xvfb xterm fonts-dejavu-core curl ca-certificates procps ffmpeg libxml2 libxmlsec1 libxmlsec1-openssl libssl3 jq \
 && rm -rf /var/lib/apt/lists/*
COPY rein-remote.linux /usr/local/bin/rein-remote
COPY host/start.sh /start.sh
COPY policy.toml /idp/policy.toml
RUN chmod +x /start.sh /usr/local/bin/rein-remote && rein-remote --version
ENTRYPOINT ["/start.sh"]
