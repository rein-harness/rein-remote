#!/bin/sh
# Copies rein-remote into the app (Contents/MacOS/rein-remote) and signs it as the app is signed.
# The binary: $RR_BIN, else the workspace's target/release/rein-remote (cargo build --release -p rr-server).
set -eu
src="${RR_BIN:-$SRCROOT/../../target/release/rein-remote}"
if [ ! -x "$src" ]; then
  echo "error: no rein-remote at $src: run 'cargo build --release -p rr-server' first (or set RR_BIN)" >&2
  exit 1
fi
dst="$TARGET_BUILD_DIR/$EXECUTABLE_FOLDER_PATH/rein-remote"
cp -f "$src" "$dst"
chmod 755 "$dst"
identity="${EXPANDED_CODE_SIGN_IDENTITY:-}"
[ -n "$identity" ] || identity=-
opts=""
[ "${ENABLE_HARDENED_RUNTIME:-NO}" = YES ] && opts="--options runtime"
codesign --force --sign "$identity" $opts --timestamp=none "$dst"
echo "bundled $src -> $dst"
