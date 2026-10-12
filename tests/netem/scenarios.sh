#!/bin/bash
# Inside the container: each link (netem on lo: both directions, so delay is half the round trip),
# each transport, the test pattern (full motion: a game's worst case) measured in Chromium.
# One JSON line per run on stdout.
set -u
SECS=${SECS:-15}
MOTION=${MOTION:-full}
RES=${RES:-1280x720}
# name|netem (delay, loss, jitter)|rate (a token bucket under it, with 100 ms of buffer, like a router's)
links=(
  "lan||"
  "rtt40_loss2|delay 20ms loss 2%|"
  "rtt40_loss5|delay 20ms loss 5%|"
  "rtt80_jitter|delay 40ms 10ms distribution normal loss 1%|"
  "bw1500|delay 20ms|1500kbit"
  "bw800|delay 20ms|800kbit"
)
LOGS=${LOGS:-/logs}; mkdir -p "$LOGS"
# A real link's packet size: lo's 64 KB MTU would put whole TCP windows in one packet, which a token
# bucket with a router-sized burst drops every time (and loss would hit 64 KB at once).
ip link set dev lo mtu 1500
link() { # netem rate
  tc qdisc del dev lo root 2>/dev/null
  [ -z "$1" ] && return
  tc qdisc add dev lo root handle 1: netem $1 limit 10000
  [ -n "$2" ] && tc qdisc add dev lo parent 1:1 handle 10: tbf rate "$2" burst 16kbit latency 100ms
}
run() { # name transport url-extra
  local name=$1 transport=$2
  local wt=on; [ "$transport" = websocket ] && wt=off
  rein-remote serve --test-pattern "$RES" --motion "$MOTION" --listen 127.0.0.1:7979 --webtransport $wt --mdns off > "$LOGS/$name-$transport.serve.log" 2>&1 &
  local sp=$!
  sleep 1
  local out
  out=$(node tools/measure.mjs "http://127.0.0.1:7979/?measure" --seconds "$SECS" --warmup 4 --flags '--no-sandbox --disable-gpu' 2>"$LOGS/$name-$transport.measure.log")
  kill $sp; wait $sp 2>/dev/null
  echo "$out" | jq -c --arg link "$name" --arg t "$transport" '{link:$link, transport:$t, got:.transport, fps, mbit, glass:.glassMs, fec, decoder, server_kbps:.server.kbps, server_fps:.server.fps, latency:.server.latency_ms, keyframes:.server.keyframes}'
}
relay() { # through the relay, sealed end to end (localhost is a secure page: the browser's crypto works)
  local name=$1
  rein-remote relay --listen 127.0.0.1:7990 > "$LOGS/$name-relay.relay.log" 2>&1 &
  local rp=$!
  sleep 0.5
  rein-remote serve --test-pattern "$RES" --motion "$MOTION" --listen 127.0.0.1:7979 --webtransport off --mdns off --relay ws://127.0.0.1:7990 > "$LOGS/$name-relay.serve.log" 2>&1 &
  local sp=$!
  sleep 2
  local u; u=$(grep -o 'open http://[^ ]*' "$LOGS/$name-relay.serve.log" | head -1 | cut -c6- | sed 's|http://127.0.0.1|http://localhost|; s|/?room=|/?measure\&room=|')
  local out
  out=$(node tools/measure.mjs "$u" --seconds "$SECS" --warmup 4 --flags '--no-sandbox --disable-gpu' 2>"$LOGS/$name-relay.measure.log")
  kill $sp $rp; wait $sp $rp 2>/dev/null
  echo "$out" | jq -c --arg link "$name" '{link:$link, transport:"relay", got:.transport, fps, mbit, glass:.glassMs, decoder, server_kbps:.server.kbps}'
}
for l in "${links[@]}"; do
  IFS='|' read -r name spec rate <<< "$l"
  link "$spec" "$rate"
  run "$name" websocket
  run "$name" webtransport
  [ "$name" = lan ] || [ "$name" = rtt40_loss2 ] && relay "$name"
done
link ""
