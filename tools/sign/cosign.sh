#!/bin/sh
# Signs files with cosign (Sigstore): a bundle FILE.sigstore.json next to each, then checks it.
#
#   tools/sign/cosign.sh FILE...
#
# Keyless (release.yml): when the job can get a GitHub OIDC token (`permissions: id-token: write`), the
# signature's certificate names this workflow and ref, and the signature is in Sigstore's public
# transparency log. Verify with:
#   cosign verify-blob --bundle FILE.sigstore.json \
#     --certificate-identity-regexp '^https://github.com/rein-harness/rein-remote/\.github/workflows/release\.yml@refs/tags/v' \
#     --certificate-oidc-issuer https://token.actions.githubusercontent.com FILE
# With a key (CI): COSIGN_KEY (a cosign.key file; COSIGN_PASSWORD its password) and COSIGN_PUB; nothing
# goes to the public log.
# Neither: says so and signs nothing (exit 0).
set -eu
[ $# -gt 0 ] || { echo "usage: $0 FILE..." >&2; exit 2; }
if [ -n "${COSIGN_KEY:-}" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  # No transparency log, no timestamp authority: a signing config with no services.
  cosign signing-config create --out "$tmp/offline.json" > /dev/null
  for f in "$@"; do
    cosign sign-blob --yes --key "$COSIGN_KEY" --signing-config "$tmp/offline.json" --bundle "$f.sigstore.json" "$f"
    cosign verify-blob --key "$COSIGN_PUB" --bundle "$f.sigstore.json" --insecure-ignore-tlog=true "$f"
  done
elif [ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ]; then
  identity="https://github.com/$GITHUB_WORKFLOW_REF"
  for f in "$@"; do
    cosign sign-blob --yes --bundle "$f.sigstore.json" "$f"
    cosign verify-blob --bundle "$f.sigstore.json" --certificate-identity "$identity" \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com "$f"
  done
else
  echo "no COSIGN_KEY and no OIDC token (id-token: write): not signed with cosign"
fi
