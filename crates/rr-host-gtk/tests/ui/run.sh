#!/bin/bash
# The Linux host app's UI test: the real app (rein-remote-host-gtk) on Xvfb, with a D-Bus session and
# the AT-SPI bus, starting a real `rein-remote host --test-pattern 1280x720` on a home folder of its
# own, driven through AT-SPI (ui_test.py). Screenshots of each page go to $SHOTS.
#
#   crates/rr-host-gtk/tests/ui/run.sh [TARGET_DIR]     # TARGET_DIR: where rein-remote and the app are (target/release)
#
# Needs: Xvfb, dbus-run-session, at-spi2-core, python3-pyatspi, imagemagick (import), libglib2.0-bin (gdbus).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
bin=$(cd "${1:-target/release}" && pwd)
export SHOTS=${SHOTS:-$PWD/linux-host-app-shots}
mkdir -p "$SHOTS"

if [ -z "${IN_SESSION:-}" ]; then
  export DISPLAY=${DISPLAY_UI:-:41}
  Xvfb "$DISPLAY" -screen 0 1280x900x24 -nolisten tcp > /tmp/xvfb.log 2>&1 &
  xvfb=$!
  trap 'kill $xvfb 2>/dev/null || true' EXIT
  sleep 2
  IN_SESSION=1 dbus-run-session -- "$0" "$@"; exit $?
fi

# The accessibility bus (as a desktop session starts it).
for l in /usr/libexec/at-spi-bus-launcher /usr/lib/at-spi2-core/at-spi-bus-launcher; do
  [ -x "$l" ] && { "$l" --launch-immediately > /tmp/at-spi.log 2>&1 & break; }
done
sleep 1

home=$(mktemp -d)
export REIN_REMOTE_HOME=$home
export GTK_A11Y=atspi GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=0 LIBGL_ALWAYS_SOFTWARE=1
export RR_ADAPT=off
echo "home: $home; display: $DISPLAY"

# The app, with no host running: it starts the rein-remote beside it, with these arguments.
"$bin/rein-remote-host-gtk" --host-arg --test-pattern --host-arg 1280x720 --host-arg --mdns --host-arg off > /tmp/app.log 2>&1 &
app=$!
status=0
REIN_REMOTE="$bin/rein-remote" APP_PID=$app python3 "$here/ui_test.py" || status=$?
echo "--- the app's output"; cat /tmp/app.log || true
echo "--- the host's log"; grep -v '^rein-remote: {' "$home/host.log" || true
echo "--- host.toml"; cat "$home/host.toml" || true
kill $app 2>/dev/null || true
sleep 1
pkill -f "rein-remote host" 2>/dev/null || true
exit $status
