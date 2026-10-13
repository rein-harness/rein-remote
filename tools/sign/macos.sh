#!/bin/sh
# Signs macOS binaries (hardened runtime) and, with Apple's credentials, notarizes them.
#
#   tools/sign/macos.sh FILE...
#
# A FILE may also be an .app (the executables in Contents/MacOS and Contents/Helpers signed first, as
# com.rein-harness.NAME, then the bundle, with its own identifier) or a .dmg; these are notarized
# each on its own and the ticket stapled to them (packaging/macos/build-dmg.sh: the app, then the dmg).
#
# With a Developer ID (release.yml, from the repository's secrets):
#   MACOS_CERTIFICATE           base64 of a .p12: the "Developer ID Application" certificate and its key
#   MACOS_CERTIFICATE_PASSWORD  the .p12's password
#   MACOS_SIGN_IDENTITY         optional: which identity (default: the first "Developer ID Application")
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   notarization (an app-specific password); all three or none
# Without MACOS_CERTIFICATE: ad-hoc (`codesign -s -`), what CI checks; nothing is notarized.
#
# The identifier is com.rein-harness.NAME (macOS keeps the Screen Recording permission by it and the
# signer, so a signed update keeps it). Every file is then checked: codesign --verify --strict. A bare executable can't have a notarization
# ticket stapled to it (only an .app, .pkg or .dmg can): Gatekeeper looks the ticket up online.
set -eu
[ $# -gt 0 ] || { echo "usage: $0 FILE..." >&2; exit 2; }
tmp=$(mktemp -d)
kc="$tmp/signing.keychain-db"
cleanup() {
  [ -f "$kc" ] && security delete-keychain "$kc" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT

if [ -n "${MACOS_CERTIFICATE:-}" ]; then
  # A keychain of its own for this run, unlocked, the certificate in it, codesign allowed to use the key.
  pass=$(openssl rand -hex 16)
  security create-keychain -p "$pass" "$kc"
  security set-keychain-settings -lut 3600 "$kc"
  security unlock-keychain -p "$pass" "$kc"
  printf '%s' "$MACOS_CERTIFICATE" | base64 --decode > "$tmp/cert.p12"
  security import "$tmp/cert.p12" -k "$kc" -P "${MACOS_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign > /dev/null
  security set-key-partition-list -S apple-tool:,apple: -s -k "$pass" "$kc" > /dev/null
  # shellcheck disable=SC2046
  security list-keychains -d user -s "$kc" $(security list-keychains -d user | tr -d '"')
  identity=${MACOS_SIGN_IDENTITY:-$(security find-identity -v -p codesigning "$kc" | awk -F'"' '/Developer ID Application/ {print $2; exit}')}
  [ -n "$identity" ] || { echo "no Developer ID Application identity in MACOS_CERTIFICATE" >&2; exit 1; }
  echo "signing with: $identity"
  ts=--timestamp
else
  echo "no MACOS_CERTIFICATE: ad-hoc signature (runs here; Gatekeeper still treats it as unsigned elsewhere)"
  identity=-
  ts=--timestamp=none
fi

for f in "$@"; do
  case "$f" in
    *.app|*.app/)
      main=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$f/Contents/Info.plist")
      for inner in "$f"/Contents/MacOS/* "$f"/Contents/Helpers/*; do
        [ -f "$inner" ] && [ "$(basename "$inner")" != "$main" ] || continue
        codesign --force --options runtime $ts --identifier "com.rein-harness.$(basename "$inner")" --sign "$identity" "$inner"
      done
      codesign --force --options runtime $ts --sign "$identity" "$f"
      codesign --verify --strict --deep --verbose=2 "$f" ;;
    *.dmg)
      codesign --force $ts --identifier "com.rein-harness.$(basename "$f" .dmg)" --sign "$identity" "$f"
      codesign --verify --strict --verbose=2 "$f" ;;
    *)
      codesign --force --options runtime $ts --identifier "com.rein-harness.$(basename "$f" .exe)" --sign "$identity" "$f"
      codesign --verify --strict --verbose=2 "$f" ;;
  esac
  codesign --display --verbose=2 "$f" 2>&1 | grep -E '^(Identifier|Format|CodeDirectory|Signature|Authority|TeamIdentifier|Timestamp|Runtime)' || true
done

# notarize FILE: submits it (a .zip or .dmg), waits, fails unless Accepted.
notarize() {
  xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait --timeout 30m --output-format json > "$tmp/notary.json"
  cat "$tmp/notary.json"; echo
  id=$(plutil -extract id raw -o - "$tmp/notary.json" 2>/dev/null || sed -n 's/.*"id":"\([^"]*\)".*/\1/p' "$tmp/notary.json")
  if ! grep -q '"status":"Accepted"' "$tmp/notary.json"; then
    [ -n "$id" ] && xcrun notarytool log "$id" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" || true
    echo "notarization was not accepted" >&2
    exit 1
  fi
  echo "notarized: $1 ($id)"
}

if [ -n "${MACOS_CERTIFICATE:-}" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  mkdir "$tmp/notarize"
  bare=0
  for f in "$@"; do
    case "$f" in
      *.app|*.app/)
        rm -f "$tmp/app.zip"
        ditto -c -k --keepParent "$f" "$tmp/app.zip"
        notarize "$tmp/app.zip"
        xcrun stapler staple "$f" ;;
      *.dmg)
        notarize "$f"
        xcrun stapler staple "$f" ;;
      *) cp "$f" "$tmp/notarize/"; bare=1 ;;
    esac
  done
  if [ "$bare" = 1 ]; then
    ditto -c -k --keepParent "$tmp/notarize" "$tmp/notarize.zip"
    notarize "$tmp/notarize.zip"
    echo "bare executables aren't stapled (Gatekeeper checks the ticket online)"
  fi
elif [ -n "${MACOS_CERTIFICATE:-}" ]; then
  echo "no APPLE_ID / APPLE_TEAM_ID / APPLE_APP_PASSWORD: signed, not notarized"
fi
