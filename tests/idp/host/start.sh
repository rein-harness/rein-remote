#!/bin/sh
# An xterm on display :1 (no window manager: the keyboard goes where the pointer is), then rein-remote
# with OIDC on :7979 and with SAML on :7980 once Keycloak answers. Logs, audit logs and recordings in /out.
mkdir -p /out/rec
Xvfb :1 -screen 0 1024x640x24 -nolisten tcp >/out/xvfb.log 2>&1 &
sleep 1
# A shell to type into (left), and a clock that changes ten times a second (right): a still screen
# sends nothing, and the check wants frames.
DISPLAY=:1 xterm -geometry 70x36+0+0 -fa 'DejaVu Sans Mono' -fs 11 -e bash >/dev/null 2>&1 &
DISPLAY=:1 xterm -geometry 40x10+600+0 -fa 'DejaVu Sans Mono' -fs 11 -e sh -c 'while :; do date +%T.%N; sleep 0.1; done' >/dev/null 2>&1 &
until curl -sf http://idp.test:8080/realms/rr/.well-known/openid-configuration >/dev/null; do sleep 2; done
until curl -sf http://idp.test:8080/realms/rr/protocol/saml/descriptor >/dev/null; do sleep 2; done
# The client secret from a file (never the command line).
printf '%s' "${OIDC_SECRET:-rr-test-oidc-secret}" > /run/oidc-secret
chmod 600 /run/oidc-secret
common="--display :1 --policy /idp/policy.toml --record /out/rec --audio off --mdns off --tailscale-cert off"
DISPLAY=:1 rein-remote serve $common --listen 0.0.0.0:7979 --public-url http://rr.test:7979 \
  --oidc-issuer http://idp.test:8080/realms/rr --oidc-client-id rein-remote --oidc-client-secret-file /run/oidc-secret \
  --audit /out/audit-oidc.jsonl >/out/serve-oidc.log 2>&1 &
DISPLAY=:1 rein-remote serve $common --listen 0.0.0.0:7980 --public-url http://rr.test:7980 \
  --saml-idp-metadata http://idp.test:8080/realms/rr/protocol/saml/descriptor --saml-entity-id rein-remote-saml \
  --audit /out/audit-saml.jsonl >/out/serve-saml.log 2>&1 &
for p in 7979 7980; do until curl -sf http://127.0.0.1:$p/info >/dev/null; do sleep 0.5; done; done
touch /out/ready
wait
