#!/bin/sh
# Signs macOS binaries (hardened runtime) and, with Apple's credentials, notarizes them.
#
#   tools/sign/macos.sh FILE...
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
  for f in "$@"; do codesign --force --options runtime --timestamp --identifier "com.rein-harness.$(basename "$f" .exe)" --sign "$identity" "$f"; done
else
  echo "no MACOS_CERTIFICATE: ad-hoc signature (runs here; Gatekeeper still treats it as unsigned elsewhere)"
  for f in "$@"; do codesign --force --options runtime --timestamp=none --identifier "com.rein-harness.$(basename "$f")" --sign - "$f"; done
fi

for f in "$@"; do
  codesign --verify --strict --verbose=2 "$f"
  codesign --display --verbose=2 "$f" 2>&1 | grep -E '^(Identifier|Format|CodeDirectory|Signature|Authority|TeamIdentifier|Timestamp|Runtime)' || true
done

if [ -n "${MACOS_CERTIFICATE:-}" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  mkdir "$tmp/notarize"
  cp "$@" "$tmp/notarize/"
  ditto -c -k --keepParent "$tmp/notarize" "$tmp/notarize.zip"
  xcrun notarytool submit "$tmp/notarize.zip" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait --timeout 30m --output-format json > "$tmp/notary.json"
  cat "$tmp/notary.json"; echo
  id=$(plutil -extract id raw -o - "$tmp/notary.json" 2>/dev/null || sed -n 's/.*"id":"\([^"]*\)".*/\1/p' "$tmp/notary.json")
  if ! grep -q '"status":"Accepted"' "$tmp/notary.json"; then
    [ -n "$id" ] && xcrun notarytool log "$id" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" || true
    echo "notarization was not accepted" >&2
    exit 1
  fi
  echo "notarized ($id); bare executables aren't stapled (Gatekeeper checks the ticket online)"
elif [ -n "${MACOS_CERTIFICATE:-}" ]; then
  echo "no APPLE_ID / APPLE_TEAM_ID / APPLE_APP_PASSWORD: signed, not notarized"
fi
