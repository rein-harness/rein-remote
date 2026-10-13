#!/bin/sh
# The .dmg as a user opens it: mounted, Rein Remote.app copied out (to a folder standing in for
# /Applications), its signature verified, its rein-remote run as `host` (a test pattern, on a home
# folder of its own) and its API asked to share, then the app itself started: it must start the
# rein-remote inside it. Then detached.
#
#   packaging/macos/smoke.sh rein-remote-0.1.1-macos-arm64.dmg
set -eu
dmg=$1
work=$(mktemp -d)
mnt="$work/mnt"
apps="$work/Applications"
home="$work/home"
mkdir -p "$mnt" "$apps" "$home"
pids=""
cleanup() {
  for p in $pids; do kill "$p" 2>/dev/null || true; done
  pkill -f "$apps/Rein Remote.app/Contents/MacOS/rein-remote host" 2>/dev/null || true
  hdiutil detach "$mnt" -quiet 2>/dev/null || true
}
trap cleanup EXIT
ok() { echo "ok: $*"; }
fail() { echo "FAILED: $*"; [ -f "$home/host.log" ] && grep -v '^rein-remote: {' "$home/host.log" | tail -20; cat "$work"/*.log 2>/dev/null | tail -40; exit 1; }

codesign --verify --verbose=2 "$dmg" && ok "the .dmg's signature"
hdiutil attach "$dmg" -nobrowse -readonly -mountpoint "$mnt" -quiet
ok "mounted at $mnt: $(ls "$mnt" | tr '\n' ' ')"
[ "$(readlink "$mnt/Applications")" = /Applications ] || fail "no Applications link"
ok "Applications -> /Applications"
ditto "$mnt/Rein Remote.app" "$apps/Rein Remote.app"
hdiutil detach "$mnt" -quiet
ok "copied Rein Remote.app out and detached"
app="$apps/Rein Remote.app"
codesign --verify --strict --deep --verbose=2 "$app" || fail "codesign --verify"
codesign --display --verbose=2 "$app" 2>&1 | grep -E '^(Identifier|Authority|Signature|TeamIdentifier|Runtime|flags)|flags=' || true
codesign --display --verbose=2 "$app/Contents/MacOS/rein-remote" 2>&1 | grep -E '^Identifier' || true
spctl --assess --type execute --verbose=2 "$app" 2>&1 || echo "(Gatekeeper refuses it: expected unless it's signed with a Developer ID and notarized)"
ok "the app's signature (and rein-remote's inside it)"
"$app/Contents/MacOS/rein-remote" --version || fail "rein-remote --version"
plutil -p "$app/Contents/Info.plist" | grep -E 'CFBundleIdentifier|CFBundleShortVersionString|LSUIElement'

# The app's rein-remote as `host`.
REIN_REMOTE_HOME=$home RR_ADAPT=off "$app/Contents/MacOS/rein-remote" host --test-pattern 640x480 --mdns off 2> "$work/host.log" &
pids="$pids $!"
for _ in $(seq 120); do grep -q 'control API on' "$work/host.log" && break; sleep 0.5; done
grep -q 'control API on' "$work/host.log" || fail "the host did not start its control API"
url=$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$home/control.json")
token=$(sed -n 's/.*"token":"\([^"]*\)".*/\1/p' "$home/control.json")
curl -sf -H "Authorization: Bearer $token" "$url/control/state" > /dev/null || fail "no answer from the control API"
ok "the app's rein-remote host answers on $url"
curl -sf -X POST -H "Authorization: Bearer $token" "$url/control/sharing/start" > /dev/null || fail "sharing didn't start"
sleep 2
curl -sf -H "Authorization: Bearer $token" "$url/control/state" | grep -q '"sharing":true' || fail "not sharing"
ok "sharing the test pattern"
kill $pids; wait 2>/dev/null || true; pids=""

# The app, with no host running (a home of its own): it starts the rein-remote inside it.
home2="$work/home2"; mkdir -p "$home2"
REIN_REMOTE_HOME=$home2 "$app/Contents/MacOS/Rein Remote" > "$work/app.log" 2>&1 &
apppid=$!
pids="$apppid"
for _ in $(seq 120); do [ -s "$home2/control.json" ] && break; sleep 0.5; done
[ -s "$home2/control.json" ] || fail "the app started no host"
hostpid=$(pgrep -P "$apppid" -f 'rein-remote host' || true)
[ -n "$hostpid" ] || fail "the host isn't the app's child"
ok "the app (pid $apppid) started its own rein-remote host (pid $hostpid: $(ps -o comm= -p "$hostpid"))"
kill -0 "$apppid" || fail "the app exited"
echo "passed"
