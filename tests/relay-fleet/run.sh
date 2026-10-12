#!/bin/sh
# A relay fleet behind one name, in Docker: two relays behind haproxy (compose.yml), a host with its
# room open on both, a viewer through the one name. Then:
#   1. the relay the viewer is on is killed (docker kill: no goodbye): how long until a viewer that
#      opens the same link again has a frame (through the other relay; haproxy retries it there);
#   2. the relay is started again: how long until the host is back in its room there.
# Each measured $ROUNDS times. Needs docker with compose (COMPOSE='docker-compose' otherwise), the image
# from build.sh. Everything is project rr-fleet, removed at the end (KEEP=1 keeps it).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
COMPOSE=${COMPOSE:-docker compose}
ROUNDS=${ROUNDS:-3}
OUT=${OUT:-$here/out}
mkdir -p "$OUT"
dc() { $COMPOSE -f "$here/compose.yml" "$@"; }
now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
say() { printf '\n== %s\n' "$*" >&2; }
cleanup() { [ "${KEEP:-0}" = 1 ] || dc down -t 1 > /dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup

say "up: two relays, haproxy in front, the host"
dc up -d relay-a relay-b lb host viewer
link=""
for i in $(seq 1 60); do
  link=$(dc logs host 2>/dev/null | sed -n 's/.*through the relay: open //p' | tail -1)
  [ -n "$link" ] && break
  sleep 0.5
done
[ -n "$link" ] || { dc logs host; echo "no link from the host"; exit 1; }
echo "the host's link: $link"
# The same room and key through the fleet's one name.
room=$(printf '%s' "$link" | sed -n 's/.*?room=\([^#&]*\).*/\1/p')
key=$(printf '%s' "$link" | sed -n 's/.*#k=\([^&]*\).*/\1/p')
lb="http://lb:7990/?room=$room#k=$key"
echo "through the one name: http://lb:7990/?room=…#k=…"

waiting() { dc exec -T "$1" curl -fsS localhost:7990/healthz 2>/dev/null | grep -q '"hosts_waiting":1'; }
for r in relay-a relay-b; do
  for i in $(seq 1 60); do waiting $r && break; sleep 0.25; done
  waiting $r || { echo "the host isn't in its room on $r"; exit 1; }
done
echo "the host waits in its room on both relays"

view() { dc exec -T viewer rein-remote connect "$lb" --seconds "$1" --json --direct off --webtransport off 2> /dev/null; }
frames() { sed -n 's/.*"frames":\([0-9]*\).*/\1/p' | head -1; }
first() { sed -n 's/.*"first_frame_ms":\([0-9]*\).*/\1/p'; }

say "through the one name"
n=$(view 3 | tee "$OUT/baseline.json" | frames)
echo "frames in 3 s: $n"
[ "${n:-0}" -gt 0 ]

: > "$OUT/rounds.txt"
for round in $(seq 1 "$ROUNDS"); do
  say "round $round"
  dc exec -d -T viewer sh -c "rein-remote connect '$lb' --seconds 120 --json --direct off --webtransport off > /tmp/watching.json 2>&1"
  on=""
  for i in $(seq 1 40); do
    for r in relay-a relay-b; do
      dc exec -T $r curl -fsS localhost:7991/metrics 2>/dev/null | grep -q '^rein_remote_relay_sessions 1' && on=$r
    done
    [ -n "$on" ] && break
    sleep 0.25
  done
  [ -n "$on" ] || { echo "the viewer isn't on either relay"; exit 1; }
  sleep 2
  echo "the viewer is on $on: killing it"
  t0=$(now)
  docker kill "rr-fleet-$on-1" > /dev/null
  # The viewer's session ends with the relay (its container exits).
  for i in $(seq 1 200); do dc exec -T viewer pgrep -f 'connect http' > /dev/null || break; sleep 0.05; done
  t_closed=$(now)
  # Opened again, the same link, until frames come: the try that got them says when its first came.
  t_back="" ff=""
  for i in $(seq 1 30); do
    t_try=$(now)
    out=$(view 1)
    n=$(printf '%s' "$out" | frames)
    if [ "${n:-0}" -gt 0 ]; then ff=$(printf '%s' "$out" | first); t_back=$(echo "$t_try + $ff / 1000" | bc -l); break; fi
  done
  [ -n "$t_back" ] || { echo "no frames through the other relay"; exit 1; }
  # The killed relay back: how long until the host is in its room there again.
  t2=$(now)
  dc start "$on" > /dev/null
  for i in $(seq 1 120); do waiting "$on" && break; sleep 0.1; done
  waiting "$on" || { echo "the host didn't come back to $on"; exit 1; }
  t3=$(now)
  perl -e 'printf "round %d: the viewer saw it close %.2f s after the kill; a frame again %.2f s after the kill (the reconnect itself: %d ms to its first frame); the host back in its room %.2f s after the relay restarted\n", @ARGV' \
    "$round" "$(echo "$t_closed - $t0" | bc)" "$(echo "$t_back - $t0" | bc -l)" "$ff" "$(echo "$t3 - $t2" | bc)" | tee -a "$OUT/rounds.txt"
  sleep 2
done

say "metrics (relay-a, after the rounds)"
dc exec -T relay-a curl -fsS localhost:7991/metrics | grep -v '^#'
dc exec -T host sh -c 'kill -0 1' && echo "the host ran throughout"
say "summary"
cat "$OUT/rounds.txt"
