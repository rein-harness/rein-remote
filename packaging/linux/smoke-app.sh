#!/bin/bash
# The installed host app, started as a user would (here on Xvfb with a D-Bus session), with no host
# running: it must start the rein-remote installed beside it (`rein-remote host`, a test pattern on a
# home folder of its own) and connect to it. Passes when, within 60 s:
#   - the host is the app's child, and its control API answers (control.json, GET /control/state);
#   - the app holds a connection open to that API (its event stream: what "connected" is in the app);
#   - sharing started through the API is seen, and the app is still running.
#
#   packaging/linux/smoke-app.sh /usr/bin/rein-remote-host-gtk
#   packaging/linux/smoke-app.sh ./rein-remote-0.1.1-x86_64.AppImage --appimage-extract-and-run
#
# Needs: Xvfb, dbus-run-session (dbus), curl, ss (iproute2), pgrep (procps).
set -euo pipefail
if [ -z "${IN_SESSION:-}" ]; then
  export DISPLAY=:47
  Xvfb "$DISPLAY" -screen 0 1280x900x24 -nolisten tcp > /tmp/smoke-xvfb.log 2>&1 &
  xvfb=$!
  trap 'kill $xvfb 2>/dev/null || true' EXIT
  sleep 2
  IN_SESSION=1 dbus-run-session -- "$0" "$@"; exit $?
fi

home=$(mktemp -d)
export REIN_REMOTE_HOME=$home GDK_BACKEND=x11 GSK_RENDERER=cairo LIBGL_ALWAYS_SOFTWARE=1 RR_ADAPT=off
log=$(mktemp)
"$@" --host-arg --test-pattern --host-arg 640x480 --host-arg --mdns --host-arg off > "$log" 2>&1 &
launched=$!
fail() {
  echo "FAILED: $*"
  echo "--- the app's output"; cat "$log" || true
  echo "--- the host's log"; grep -v '^rein-remote: {' "$home/host.log" 2>/dev/null || true
  pkill -f 'rein-remote host' || true; kill $launched 2>/dev/null || true
  exit 1
}
ok() { echo "ok: $*"; }

app="" host="" port="" token=""
for _ in $(seq 120); do
  sleep 0.5
  kill -0 $launched 2>/dev/null || fail "the app exited"
  app=$(pgrep -n -x rein-remote-hos || true)        # comm: 15 characters
  host=$(pgrep -n -f '/rein-remote host' || true)
  if [ -n "$host" ] && [ -s "$home/control.json" ]; then
    url=$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$home/control.json")
    token=$(sed -n 's/.*"token":"\([^"]*\)".*/\1/p' "$home/control.json")
    port=${url##*:}
    if curl -sf -H "Authorization: Bearer $token" "$url/control/state" > /dev/null &&
       ss -tnp state established "( dport = :$port )" | grep -q "pid=$app,"; then
      break
    fi
  fi
done
[ -n "$app" ] || fail "no rein-remote-host-gtk process"
[ -n "$host" ] || fail "no rein-remote host started"
ok "the app (pid $app: $(readlink /proc/$app/exe)) started the host (pid $host: $(readlink /proc/$host/exe))"
[ "$(ps -o ppid= -p "$host" | tr -d ' ')" = "$app" ] || fail "the host isn't the app's child"
ok "the host is the app's child"
curl -sf -H "Authorization: Bearer $token" "$url/control/state" > /dev/null || fail "no answer from $url/control/state"
ok "the host's control API answers on $url"
ss -tnp state established "( dport = :$port )" | grep "pid=$app," || fail "the app has no connection to the host's API"
ok "the app is connected to it"
curl -sf -X POST -H "Authorization: Bearer $token" "$url/control/sharing/start" > /dev/null || fail "sharing didn't start"
sleep 2
curl -sf -H "Authorization: Bearer $token" "$url/control/state" | grep -q '"sharing":true' || fail "not sharing"
kill -0 "$app" || fail "the app exited"
ok "sharing the test pattern; the app still running"
kill "$app"; sleep 2
pkill -f '/rein-remote host' || true
echo "--- the app's output"; cat "$log"
echo "passed"
