#!/bin/sh
# Builds the Rust core (crates/rr-jni) for Android into app/src/main/jniLibs, which the app packs.
#
#   ./build-core.sh                  # arm64-v8a (phones, and the emulator on an Apple silicon Mac)
#   ./build-core.sh x86_64           # the emulator on an x86 machine (CI)
#   ./build-core.sh arm64-v8a x86_64
#   PROFILE=debug ./build-core.sh
#
# Needs: the NDK (ANDROID_NDK_HOME, or ANDROID_HOME/ndk/<version>), cargo install cargo-ndk, and
# rustup target add aarch64-linux-android (x86_64-linux-android for x86_64).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
profile=${PROFILE:-release}
[ $# -eq 0 ] && set -- arm64-v8a
if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -n "${ANDROID_HOME:-}" ] && [ -d "$ANDROID_HOME/ndk" ]; then
  ANDROID_NDK_HOME="$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk" | sort -V | tail -1)"
  export ANDROID_NDK_HOME
fi
flag=""
[ "$profile" = release ] && flag=--release
targets=""
for abi in "$@"; do targets="$targets -t $abi"; done
cd "$root"
# shellcheck disable=SC2086
cargo ndk $targets -P 26 -o "$here/app/src/main/jniLibs" build -p rr-jni $flag
find "$here/app/src/main/jniLibs" -name librr_jni.so
