#!/bin/sh
# NAT hole punching, verified in Docker: two private networks, each behind its own NAT router (a
# container doing what a home router does: MASQUERADE out, nothing unasked in), a relay on a "public"
# network both routers reach, the host (rein-remote serve --relay) behind NAT A, the viewer (rein-remote
# connect, the relay's link) behind NAT B.
#
#                       public 198.51.100.0/24  (netem: delay on the routers' and the relay's way out)
#        ┌───────────────────┬───────────────────────┬────────────────────┐
#   rr-punch-nat-a .2     rr-punch-relay .10                        rr-punch-nat-b .3
#        │ lan-a 10.10.1.0/24                                             │ lan-b 10.10.2.0/24
#   rr-punch-host .10 (serve --test-pattern --relay ws://198.51.100.10:7990)   rr-punch-viewer .10 (connect)
#
# What it runs (each `connect` for $SECONDS s, its JSON saved in $OUT):
#   relayed   the viewer stays on the relay (--direct off)
#   direct    the viewer punches through and moves to WebTransport straight to the host
#   lossy-*   both again with 2% loss on each router's way out
#   blocked   UDP dropped between the routers: punching fails, the session stays on the relay
#   symmetric NAT B gives each destination its own port (--random-fully): what the relay saw isn't what
#             the host would reach, so punching fails and it stays on the relay
#
# Needs: docker (colima works), the image from tests/nat/build.sh. Everything is named rr-punch-*, and
# removed at the end (KEEP=1 keeps it).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
SECONDS_EACH=${SECONDS_EACH:-15}
DELAY_NAT=${DELAY_NAT:-15ms}
DELAY_RELAY=${DELAY_RELAY:-20ms}
OUT=${OUT:-$here/out}
IMAGE=${IMAGE:-rr-punch}
# What the host shows: the test pattern (a little motion), or SOURCE='--test-pattern 960x540 --motion full'
# (every pixel changing every frame: megabits a second, so loss and parity show).
SOURCE=${SOURCE:---test-pattern 1280x720}
# RR_ADAPT=off holds the bitrate and frame rate (to compare paths, not the rate controller's moves).
mkdir -p "$OUT"

say() { printf '\n== %s\n' "$*" >&2; }
x() { docker exec "$@"; }

cleanup() {
  [ "${KEEP:-0}" = 1 ] && return
  docker rm -f rr-punch-relay rr-punch-nat-a rr-punch-nat-b rr-punch-host rr-punch-viewer > /dev/null 2>&1 || true
  docker network rm rr-punch-public rr-punch-lan-a rr-punch-lan-b > /dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

say "networks and containers"
docker network create --internal --subnet 198.51.100.0/24 rr-punch-public > /dev/null
docker network create --internal --subnet 10.10.1.0/24 rr-punch-lan-a > /dev/null
docker network create --internal --subnet 10.10.2.0/24 rr-punch-lan-b > /dev/null
run() { name=$1; shift; docker run -d --name "$name" --cap-add NET_ADMIN "$@" "$IMAGE" sleep infinity > /dev/null; }
run rr-punch-relay --network rr-punch-public --ip 198.51.100.10
for side in a b; do
  n=$([ $side = a ] && echo 2 || echo 3)
  l=$([ $side = a ] && echo 1 || echo 2)
  run rr-punch-nat-$side --network rr-punch-public --ip 198.51.100.$n --sysctl net.ipv4.ip_forward=1
  docker network connect --ip 10.10.$l.2 rr-punch-lan-$side rr-punch-nat-$side
done
run rr-punch-host --network rr-punch-lan-a --ip 10.10.1.10
run rr-punch-viewer --network rr-punch-lan-b --ip 10.10.2.10

# The interface a container has on a subnet.
iface() { x "$1" sh -c "ip -o -4 addr show | awk '\$4 ~ /^$2/ {print \$2}' | head -1"; }

for side in a b; do
  r=rr-punch-nat-$side
  pub=$(iface $r 198.51.100.)
  # A home router: everything from inside goes out as the router's address (Linux keeps the source port
  # when it can: the same outside address and port whatever the destination); nothing unasked comes in
  # (dropped, not answered: an answered stray would make conntrack pick another port for the reply).
  x $r iptables -t nat -A POSTROUTING -o "$pub" -j MASQUERADE
  x $r iptables -A INPUT -i "$pub" -m conntrack --ctstate NEW -j DROP
  x $r iptables -A FORWARD -i "$pub" -m conntrack --ctstate NEW -j DROP
  x $r tc qdisc add dev "$pub" root netem delay "$DELAY_NAT"
done
x rr-punch-relay tc qdisc add dev "$(iface rr-punch-relay 198.51.100.)" root netem delay "$DELAY_RELAY"
x rr-punch-host ip route replace default via 10.10.1.2
x rr-punch-viewer ip route replace default via 10.10.2.2

say "relay, then the host behind NAT A"
docker exec -d rr-punch-relay sh -c 'rein-remote relay --listen 0.0.0.0:7990 > /tmp/relay.log 2>&1'
sleep 1
docker exec -d -e RR_ADAPT="${RR_ADAPT:-}" rr-punch-host sh -c "rein-remote serve $SOURCE --listen 0.0.0.0:7979 --mdns off --pair off --relay ws://198.51.100.10:7990 > /tmp/serve.log 2>&1"
link=""
for _ in $(seq 1 60); do
  link=$(x rr-punch-host sh -c "grep -o 'http://198.51.100.10:7990/?room=[^ ]*' /tmp/serve.log" 2> /dev/null || true)
  [ -n "$link" ] && x rr-punch-host grep -q "WebTransport on udp" /tmp/serve.log && break
  sleep 1
done
[ -n "$link" ] || { x rr-punch-host cat /tmp/serve.log; exit 1; }
x rr-punch-host grep -E "relay sees|didn't answer" /tmp/serve.log >&2 || true

connect() {
  name=$1; shift
  say "$name"
  x rr-punch-viewer rein-remote connect "$link" --seconds "$SECONDS_EACH" --json "$@" > "$OUT/$name.json" 2> "$OUT/$name.log" || true
  grep -E "punching|now on|no direct|relay \(" "$OUT/$name.log" >&2 || true
  cat "$OUT/$name.json" >&2
  sleep 2
}

connect relayed --direct off
connect direct

say "2% loss on each router's way out"
for side in a b; do x rr-punch-nat-$side tc qdisc change dev "$(iface rr-punch-nat-$side 198.51.100.)" root netem delay "$DELAY_NAT" loss 2%; done
connect lossy-relayed --direct off
connect lossy-direct
for side in a b; do x rr-punch-nat-$side tc qdisc change dev "$(iface rr-punch-nat-$side 198.51.100.)" root netem delay "$DELAY_NAT"; done

say "UDP between the routers dropped (punching can't work; the relay's UDP still answers)"
x rr-punch-nat-a iptables -I FORWARD -p udp -d 198.51.100.3 -j DROP
x rr-punch-nat-a iptables -I FORWARD -p udp -s 198.51.100.3 -j DROP
x rr-punch-nat-b iptables -I FORWARD -p udp -d 198.51.100.2 -j DROP
x rr-punch-nat-b iptables -I FORWARD -p udp -s 198.51.100.2 -j DROP
connect blocked
x rr-punch-nat-a iptables -D FORWARD -p udp -d 198.51.100.3 -j DROP
x rr-punch-nat-a iptables -D FORWARD -p udp -s 198.51.100.3 -j DROP
x rr-punch-nat-b iptables -D FORWARD -p udp -d 198.51.100.2 -j DROP
x rr-punch-nat-b iptables -D FORWARD -p udp -s 198.51.100.2 -j DROP

say "NAT B symmetric (a port per destination)"
pub=$(iface rr-punch-nat-b 198.51.100.)
x rr-punch-nat-b iptables -t nat -D POSTROUTING -o "$pub" -j MASQUERADE
x rr-punch-nat-b iptables -t nat -A POSTROUTING -o "$pub" -j MASQUERADE --random-fully
connect symmetric

say "the host's log"
x rr-punch-host sh -c "grep -E 'offered|punching|came straight|connected' /tmp/serve.log" >&2 || true
x rr-punch-host cat /tmp/serve.log > "$OUT/serve.log"
x rr-punch-relay cat /tmp/relay.log > "$OUT/relay.log"

say "summary (per path: fps, kbit/s, capture→arrival p50/p95 ms on the shared clock, rtt ms, frames lost, FEC rebuilt, datagrams lost ‰)"
for f in relayed direct lossy-relayed lossy-direct blocked symmetric; do
  python3 - "$OUT/$f.json" "$f" << 'EOF'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"{sys.argv[2]:<14} (no result: {e})"); sys.exit()
for p in d["phases"]:
    if p["seconds"] < 2:
        # The relay's first moments before the session moved: the switch, not a measurement.
        print(f"{sys.argv[2]:<14} {p['transport']:<20} {p['seconds']:>5.1f} s (until it moved)")
        continue
    print(f"{sys.argv[2]:<14} {p['transport']:<20} {p['seconds']:>5.1f} s {p['fps']:>5.1f} fps {p['kbps']:>6.0f} kbit/s  "
          f"latency {p['latency_same_clock_ms']['p50']:>5.1f} / {p['latency_same_clock_ms']['p95']:>5.1f}  rtt {p['rtt_ms']['p50']:>5.1f}  "
          f"lost {p['frames_lost']}  fec {p['fec_recovered']}  dgram loss {p['datagrams_lost_permille']:.0f}‰")
EOF
done
