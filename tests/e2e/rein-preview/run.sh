#!/bin/bash
# Phase 2 check (Rein Remote preview fixes), end to end, one command: builds both images (from read-only copies of the repos),
# starts the Rein machine (Ubuntu 24.04: Xvnc + xterm, Rein web UI on 127.0.0.1, Caddy on :8443) and
# the phone (headless Chromium at 390×844), runs phone/p2-check.mjs through the proxy, then checks
# inside the Rein machine that the typed command ran. Results in out/.
set -uo pipefail
D="$(cd "$(dirname "$0")" && pwd)"
OUT="$D/out"; mkdir -p "$OUT"; rm -f "$OUT"/*.png "$OUT"/result.json "$OUT"/remote-stats.jsonl "$OUT"/page-received.jsonl
"$D/sync-ctx.sh"
docker build -q -f "$D/Dockerfile.host" -t rr-p2fix-host "$D/ctx" >/dev/null || { echo "host image build failed"; exit 1; }
docker build -q -f "$D/Dockerfile.phone" -t rr-p2fix-phone "$D/ctx" >/dev/null || { echo "phone image build failed"; exit 1; }

docker rm -f rr-p2fix-host rr-p2fix-phone >/dev/null 2>&1
docker network rm rr-p2fix-net >/dev/null 2>&1
docker network create rr-p2fix-net >/dev/null
# The Rein machine is "rein.test" on this network; only Caddy's :8443 listens on it (Rein is on 127.0.0.1).
docker run -d --name rr-p2fix-host --hostname rein-host --network rr-p2fix-net --network-alias rein.test rr-p2fix-host >/dev/null
for i in $(seq 60); do docker exec rr-p2fix-host test -f /p2/logs/ready 2>/dev/null && break; sleep 0.5; done
for i in $(seq 60); do docker exec rr-p2fix-host curl -sfk --resolve rein.test:8443:127.0.0.1 https://rein.test:8443/api/state >/dev/null 2>&1 && break; sleep 0.5; done
echo "== Rein machine"
docker exec rr-p2fix-host sh -c 'ss -ltnH | awk "{print \$4}" | sort; /root/.rein/bin/rein-remote --version; tail -2 /p2/logs/rein.log'
echo "== the phone can't reach Rein except through the proxy"
docker run --rm --name rr-p2fix-probe --network rr-p2fix-net rr-p2fix-phone sh -c 'curl -s -m 5 -o /dev/null -w "direct http://rein.test:9333 -> %{http_code} (%{errormsg})\n" http://rein.test:9333/; curl -sk -m 5 -o /dev/null -w "proxy https://rein.test:8443 -> %{http_code}\n" https://rein.test:8443/api/state' | tee "$OUT/reachability.txt"

# rein-remote's own /stats, sampled on the Rein machine every second while the phone is connected.
rm -f "$OUT/cpu.txt" "$OUT/top.txt"
( while :; do
    docker exec rr-p2fix-host /p2/remote-stats.sh >> "$OUT/remote-stats.jsonl" 2>/dev/null
    { date +%T; docker stats --no-stream --format '{{.Name}} cpu={{.CPUPerc}} mem={{.MemUsage}}' rr-p2fix-host rr-p2fix-phone 2>/dev/null; } >> "$OUT/cpu.txt"
    { date +%T; docker exec rr-p2fix-host top -b -n 1 -o %CPU 2>/dev/null | sed -n '7,13p'; } >> "$OUT/top.txt"
    sleep 1
  done ) &
SAMPLER=$!

echo "== the phone"
docker create --name rr-p2fix-phone --network rr-p2fix-net -e MEASURE_S="${MEASURE_S:-10}" -e DSF="${DSF:-3}" -w /phone rr-p2fix-phone node p2-check.mjs >/dev/null
docker cp "$D/phone/p2-check.mjs" rr-p2fix-phone:/phone/p2-check.mjs
docker start -a rr-p2fix-phone 2>&1 | tee "$OUT/phone.log"
docker cp rr-p2fix-phone:/out/. "$OUT/" 2>/dev/null
kill $SAMPLER 2>/dev/null

echo "== inside the Rein machine"
docker exec rr-p2fix-host sh -c 'ls -l /tmp/p2-proof && printf "contents: " && cat /tmp/p2-proof' | tee "$OUT/proof.txt"
docker exec rr-p2fix-host sh -c 'ps -eo pid,args | grep "[r]ein-remote serve"' | tee "$OUT/rein-remote-procs.txt"
docker exec rr-p2fix-host cat /p2/logs/caddy-access.log > "$OUT/caddy-access.log"
docker exec rr-p2fix-host cat /p2/logs/rein.log > "$OUT/rein.log"
echo "== proxied rein-remote.js and stream (Caddy's access log)"
jq -c 'select(.request.uri|test("rein-remote.js|/stream")) | {uri: .request.uri, status, duration, size, proto: .request.proto, ws: .request.headers.Upgrade}' "$OUT/caddy-access.log" | tee "$OUT/caddy-remote.txt"
echo "== rein-remote /stats (last sample per process)"
jq -sc 'group_by(.pid) | map(last) | .[]' "$OUT/remote-stats.jsonl" 2>/dev/null | tee "$OUT/remote-stats-last.txt"
# The page must never have received a Rein Remote token or port, or a VNC password: every token the
# running rein-remotes were given (from their environment) and the display's password, searched for in
# everything the page received (replies, events, snapshots), as captured by the phone through CDP.
echo "== secrets the page must not have received"
LEAK=0
TOKENS=$(docker exec rr-p2fix-host sh -c 'for p in $(pgrep -f "^/root/.rein/bin/rein-remote serve"); do tr "\0" "\n" < /proc/$p/environ | sed -n "s/^REIN_REMOTE_TOKEN=//p"; done')
PORTS=$(docker exec rr-p2fix-host sh -c 'for p in $(pgrep -f "^/root/.rein/bin/rein-remote serve"); do ss -ltnpH | grep "pid=$p," | awk "{print \$4}" | sed "s/.*://"; done')
echo "rein-remote processes: $(echo "$TOKENS" | grep -c .) token(s), ports: $(echo $PORTS)"
[ -s "$OUT/page-received.jsonl" ] || { echo "FAIL nothing captured from the page"; LEAK=1; }
echo "captured: $(wc -l < "$OUT/page-received.jsonl") bodies/events"
for t in $TOKENS; do grep -qF -- "$t" "$OUT/page-received.jsonl" && { echo "FAIL a rein-remote token (${#t} chars) reached the page"; LEAK=1; }; done
for p in $PORTS; do grep -qE "\"port\":$p\b" "$OUT/page-received.jsonl" && { echo "FAIL rein-remote's port $p reached the page"; LEAK=1; }; done
grep -qF -- "${VNC_PW:-p2-vnc-pw}" "$OUT/page-received.jsonl" && { echo "FAIL the VNC password reached the page"; LEAK=1; }
[ $LEAK = 0 ] && echo "PASS no token, rein-remote port or VNC password in what the page received"
if [ $LEAK = 0 ] && docker exec rr-p2fix-host grep -qx hi /tmp/p2-proof && ! jq -e '.steps[] | select(.ok == false)' "$OUT/result.json" >/dev/null; then echo "PASS"; else echo "FAIL"; fi
[ -n "${KEEP:-}" ] || { docker rm -f rr-p2fix-host rr-p2fix-phone >/dev/null; docker network rm rr-p2fix-net >/dev/null; }
