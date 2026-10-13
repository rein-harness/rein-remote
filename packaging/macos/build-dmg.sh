#!/bin/sh
# Rein Remote's .dmg: Rein Remote.app (clients/macos-host, the menu-bar app) with rein-remote inside it
# (Contents/MacOS/rein-remote, which the app starts as `rein-remote host`), and a link to
# /Applications to drag it onto.
#
#   packaging/macos/build-dmg.sh [OUT_DIR]           # all of it: rein-remote-VERSION-macos-ARCH.dmg
#   UNIVERSAL=1 packaging/macos/build-dmg.sh         # arm64 and x86_64 in one (what a release ships)
#   packaging/macos/build-dmg.sh app STAGE           # only the app, unsigned, in STAGE (with the Applications link)
#   packaging/macos/build-dmg.sh dmg STAGE OUT_DIR   # only the .dmg of STAGE, unsigned
#
# All of it signs with tools/sign/macos.sh: the app (its executables, then the bundle), then the .dmg.
# With a Developer ID and Apple's credentials (MACOS_CERTIFICATE…, APPLE_ID…:
# docs/verification/signing.md) each is notarized and stapled; without them, ad hoc. release.yml runs
# the steps apart, so that the secrets are in the signing steps only. Start at login is the app's own (Settings →
# SMAppService.mainApp): nothing is installed for it here.
# Needs Xcode, xcodegen (brew install xcodegen) and Rust.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
src=${MACOS_HOST_SRC:-$root/clients/macos-host}
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)}

# app STAGE: Rein Remote.app built into STAGE, an Applications link beside it.
build_app() {
  stage=$1
  [ -f "$src/project.yml" ] || { echo "no macOS host app at $src" >&2; exit 1; }
  work=$(mktemp -d)
  cd "$root"
  if [ "${UNIVERSAL:-0}" = 1 ]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build --release -p rr-server --target aarch64-apple-darwin --target x86_64-apple-darwin
    lipo -create -output "$work/rein-remote" target/aarch64-apple-darwin/release/rein-remote target/x86_64-apple-darwin/release/rein-remote
    archs="arm64 x86_64"
  else
    cargo build --release -p rr-server
    cp target/release/rein-remote "$work/rein-remote"
    archs=$(uname -m)
  fi
  lipo -info "$work/rein-remote"
  (cd "$src" && xcodegen generate --quiet)
  # RR_BIN: the rein-remote that bundle-server.sh copies into the app.
  RR_BIN="$work/rein-remote" xcodebuild -project "$src/ReinRemoteHost.xcodeproj" -scheme ReinRemoteHost \
    -configuration Release -derivedDataPath "$work/dd" ARCHS="$archs" ONLY_ACTIVE_ARCH=NO \
    MARKETING_VERSION="$version" RR_BIN="$work/rein-remote" build > "$work/xcodebuild.log" 2>&1 ||
    { grep -E 'error:|BUILD FAILED' "$work/xcodebuild.log" || tail -40 "$work/xcodebuild.log"; exit 1; }
  built="$work/dd/Build/Products/Release/Rein Remote.app"
  [ -x "$built/Contents/MacOS/rein-remote" ] || { echo "rein-remote isn't in the app" >&2; exit 1; }
  rm -rf "$stage" && mkdir -p "$stage"
  ditto "$built" "$stage/Rein Remote.app"
  ln -s /Applications "$stage/Applications"
  rm -rf "$work"
  echo "built: $stage/Rein Remote.app"
}

# dmg STAGE OUT_DIR: the .dmg (named for the app's architectures). Prints its path last.
build_dmg() {
  stage=$1 out=$2
  case $(lipo -archs "$stage/Rein Remote.app/Contents/MacOS/rein-remote") in
    *x86_64*arm64*|*arm64*x86_64*) suffix=universal ;;
    *) suffix=$(lipo -archs "$stage/Rein Remote.app/Contents/MacOS/rein-remote") ;;
  esac
  mkdir -p "$out"
  dmg="$(cd "$out" && pwd)/rein-remote-$version-macos-$suffix.dmg"
  rm -f "$dmg"
  # hdiutil fails now and then on a busy CI machine ("Resource busy"): a few tries.
  for try in 1 2 3; do
    hdiutil create -volname "Rein Remote" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg" && break
    [ "$try" = 3 ] && exit 1
    sleep 5
  done
  echo "$dmg"
}

case "${1:-}" in
  app) build_app "$2" ;;
  dmg) build_dmg "$2" "$3" ;;
  *)
    stage=$(mktemp -d)/stage
    build_app "$stage"
    sh "$root/tools/sign/macos.sh" "$stage/Rein Remote.app"
    dmg=$(build_dmg "$stage" "${1:-$root/target/dmg}" | tail -1)
    sh "$root/tools/sign/macos.sh" "$dmg"
    rm -rf "$(dirname "$stage")"
    ls -la "$dmg" ;;
esac
