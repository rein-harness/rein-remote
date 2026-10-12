#!/bin/sh
# A relay fleet behind one name, in Docker: two relays behind haproxy (compose.yml), a host with its
# room open on both, a viewer through the one name that opens the link again when its session ends.
# Then, each round:
#   1. the relay the viewer is on is killed (docker kill: it vanishes, no goodbye): how long the viewer
#      is without a picture (its session ends, it opens the link again, haproxy sends it to the other);
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
n=$(view 8 | tee "$OUT/baseline.json" | frames || true)
echo "frames in 8 s (the first viewer: the host opens its encoder): $n"
[ "${n:-0}" -gt 0 ]

: > "$OUT/rounds.txt"
# A viewer that opens the link again whenever its session ends (as an app would), each session's start
# and its first and last frame logged inside the Docker machine (one clock for every container).
dc exec -d -T viewer sh -c "while :; do s=\$(date +%s.%N); o=\$(rein-remote connect '$lb' --seconds 300 --json --direct off --webtransport off 2> /dev/null); echo \"\$s \$o\" >> /tmp/sessions.log; done"
for round in $(seq 1 "$ROUNDS"); do
  say "round $round"
  sleep 3
  on=""
  for i in $(seq 1 40); do
    for r in relay-a relay-b; do
      dc exec -T $r curl -fsS localhost:7991/metrics 2>/dev/null | grep -q '^rein_remote_relay_sessions 1' && on=$r
    done
    [ -n "$on" ] && break
    sleep 0.25
  done
  [ -n "$on" ] || { echo "the viewer isn't on either relay"; exit 1; }
  echo "the viewer is on $on: killing it (docker kill: the relay vanishes, no FIN, no RST)"
  docker kill "rr-fleet-$on-1" > /dev/null
  sleep 8
  dc start "$on" > /dev/null
  for i in $(seq 1 300); do waiting "$on" && break; sleep 0.1; done
  waiting "$on" || { echo "the host didn't come back to $on"; exit 1; }
  # When it started, and when the host said it was back in its room there (Docker's clock, both).
  started=$(docker inspect -f '{{.State.StartedAt}}' "rr-fleet-$on-1")
  back=$(docker logs -t --since "$started" rr-fleet-host-1 2>&1 | grep "back in the room on the relay $on" | head -1 | cut -d' ' -f1)
  perl -MTime::Local -e '
    sub t { my ($y,$mo,$d,$h,$mi,$s,$f) = $_[0] =~ /(\d+)-(\d+)-(\d+)T(\d+):(\d+):(\d+)(\.\d+)?/; timegm($s,$mi,$h,$d,$mo-1,$y) + ($f || 0) }
    printf "round %d: %s restarted; the host back in its room there %.2f s after it started\n", $ARGV[0], $ARGV[1], t($ARGV[3]) - t($ARGV[2]) if $ARGV[3];
    printf "round %d: %s restarted; the host is in its room there (it never said it had lost it)\n", $ARGV[0], $ARGV[1] unless $ARGV[3];
  ' "$round" "$on" "$started" "$back" | tee -a "$OUT/rounds.txt"
done
dc exec -T viewer sh -c 'pkill -f "while :" ; pkill -f "rein-remote connect"' || true
sleep 1
dc exec -T viewer cat /tmp/sessions.log > "$OUT/sessions.log"
# Each session that the relay's death ended (closed early): how long until the next one's first frame.
perl -ne '
  my ($s) = /^(\S+)/; my ($f) = /"first_frame_ms":(\d+)/; my ($l) = /"last_frame_ms":(\d+)/; my $closed = !/"closed":null/;
  if (defined $prev_last && $prev_closed && defined $f) { my $gap = $s + $f/1000 - $prev_last; push @g, $gap; printf "a relay died: no picture for %.2f s (the reconnect itself: %d ms to its first frame)\n", $gap, $f }
  ($prev_last, $prev_closed) = (defined $l ? $s + $l/1000 : undef, $closed);
  END { @g = sort { $a <=> $b } @g; printf "%d failovers: median %.2f s, worst %.2f s\n", scalar @g, $g[$#g/2], $g[-1] if @g }
' "$OUT/sessions.log" | tee -a "$OUT/rounds.txt"

say "metrics (relay-a, after the rounds)"
dc exec -T relay-a curl -fsS localhost:7991/metrics | grep -v '^#'
dc exec -T host sh -c 'kill -0 1' && echo "the host ran throughout"
say "summary"
cat "$OUT/rounds.txt"
