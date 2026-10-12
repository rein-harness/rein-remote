#!/bin/sh
# Builds the Rust core (crates/rr-ffi) for iOS into ReinRemoteCore.xcframework, which the app links.
#
#   ./build-core.sh                 # a device (arm64) and the simulator on this Mac's architecture
#   ./build-core.sh sim             # the simulator only (CI, and day to day)
#   ./build-core.sh device sim x86  # also an Intel Mac's simulator
#   PROFILE=debug ./build-core.sh sim
#
# One target at a time (memory). Needs: rustup target add aarch64-apple-ios aarch64-apple-ios-sim
# (and x86_64-apple-ios for x86).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out="$here/build"
profile=${PROFILE:-release}
[ $# -eq 0 ] && set -- device sim

host_sim=aarch64-apple-ios-sim
[ "$(uname -m)" = x86_64 ] && host_sim=x86_64-apple-ios

device="" sims=""
for w in "$@"; do
  case "$w" in
    device) device=aarch64-apple-ios ;;
    sim) sims="$sims $host_sim" ;;
    x86) sims="$sims x86_64-apple-ios" ;;
    *) echo "usage: $0 [device] [sim] [x86]" >&2; exit 2 ;;
  esac
done

flag=""
[ "$profile" = release ] && flag=--release
export IPHONEOS_DEPLOYMENT_TARGET=17.0
for t in $device $sims; do
  echo "building rr-ffi for $t ($profile)"
  (cd "$root" && cargo build -p rr-ffi --lib --target "$t" $flag)
done

lib() { echo "$root/target/$1/$profile/librr_ffi.a"; }
rm -rf "$out/ReinRemoteCore.xcframework" "$out/headers" "$out/sim"
mkdir -p "$out/headers" "$out/sim"
cp "$root/crates/rr-ffi/include/rr_ffi.h" "$out/headers/"
cat > "$out/headers/module.modulemap" <<'MAP'
module ReinRemoteCore {
  header "rr_ffi.h"
  link "rr_ffi"
  export *
}
MAP

args=""
if [ -n "$device" ]; then
  args="-library $(lib aarch64-apple-ios) -headers $out/headers"
fi
if [ -n "$sims" ]; then
  # Every simulator architecture in one library.
  set --
  for t in $sims; do set -- "$@" "$(lib "$t")"; done
  if [ $# -gt 1 ]; then lipo -create "$@" -output "$out/sim/librr_ffi.a"; else cp "$1" "$out/sim/librr_ffi.a"; fi
  args="$args -library $out/sim/librr_ffi.a -headers $out/headers"
fi
# shellcheck disable=SC2086
xcodebuild -create-xcframework $args -output "$out/ReinRemoteCore.xcframework" > /dev/null
echo "$out/ReinRemoteCore.xcframework"
