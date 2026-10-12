# Signed and notarized binaries

A release (`release.yml`, on a `v*` tag) signs what it builds when the repository has the secrets for
it, and skips those steps when it doesn't (the build is then as before: unsigned, which is how every
release so far was made). The steps live in `tools/sign/`, so a release and CI run the same code:

| Platform | What | Script | Without the secrets |
|---|---|---|---|
| macOS | `codesign --options runtime --timestamp` with a Developer ID Application certificate, then `xcrun notarytool submit --wait` | `tools/sign/macos.sh` | step skipped |
| Windows | Authenticode with `signtool sign /fd sha256 /tr … /td sha256` (RFC 3161 timestamp), then `signtool verify /pa` | `tools/sign/windows.ps1` | step skipped |
| Linux, and every archive | `SHA256SUMS` signed with cosign, keyless (Sigstore: a short-lived certificate for this workflow's GitHub identity, logged in Rekor): `SHA256SUMS.sigstore.json` | `tools/sign/cosign.sh` | needs no secret (only `id-token: write`); if Sigstore can't be reached the release is published without the bundle and the step shows a warning |

Bare executables can't be stapled (only an `.app`, `.pkg` or `.dmg` can carry a notarization ticket):
Gatekeeper looks the ticket up online the first time the binary runs. If rein-remote ever ships in a
`.pkg` or `.dmg`, add `xcrun stapler staple` for it after notarization.

The signature's identifier is `com.rein-harness.rein-remote` (and `…rein-remote-viewer`): macOS keeps
the Screen Recording and Accessibility permissions by identifier and signer, so a signed update keeps
them (an unsigned build asks again after every update).

## Secrets (repository → Settings → Secrets and variables → Actions)

| Secret | What |
|---|---|
| `MACOS_CERTIFICATE` | `base64 -i cert.p12`: the **Developer ID Application** certificate with its private key, exported from Keychain Access as .p12 |
| `MACOS_CERTIFICATE_PASSWORD` | the .p12's password |
| `MACOS_SIGN_IDENTITY` | optional: the identity's full name, `Developer ID Application: Name (TEAMID)` (default: the first Developer ID Application identity in the .p12) |
| `APPLE_ID` | the Apple ID that notarizes (a member of the team) |
| `APPLE_TEAM_ID` | the 10-character team ID |
| `APPLE_APP_PASSWORD` | an app-specific password for that Apple ID (appleid.apple.com → Sign-In and Security → App-Specific Passwords) |
| `WINDOWS_CERTIFICATE` | `base64` of a .pfx: an OV or EV code-signing certificate and its key. (An EV certificate on a hardware token or in a cloud HSM can't be exported to a .pfx: use the CA's signing service, or Azure Trusted Signing, in place of `/f`; the rest of the script is the same.) |
| `WINDOWS_CERTIFICATE_PASSWORD` | the .pfx's password |

And one optional repository **variable**: `WINDOWS_TIMESTAMP_URL` (default `http://timestamp.digicert.com`).

Only the macOS and Windows signing steps see these secrets: the build job is told only whether each
is set (`HAS_MACOS_CERTIFICATE`, `HAS_WINDOWS_CERTIFICATE`), so no dependency's build script can
read them.

## What CI checks on every pull request (no secrets)

The `signing` job in `ci.yml` (never on tags) takes the binary the `test` job built on each OS and:

- **macOS:** signs it ad hoc with the hardened runtime (`codesign -s - --options runtime`), runs it,
  `codesign --verify --strict` passes, and a copy with one byte changed is refused.
- **Windows:** makes a self-signed code-signing certificate (`New-SelfSignedCertificate -Type
  CodeSigningCert`), trusts it on the runner only (LocalMachine\Root), signs with `signtool`,
  `signtool verify /pa /v` and `Get-AuthenticodeSignature` say Valid, the binary runs, and a copy with
  one byte changed is not Valid.
- **Linux:** `cosign generate-key-pair`, signs `SHA256SUMS` and the binary into `.sigstore.json`
  bundles (nothing sent to the public log), `cosign verify-blob` passes, and a changed copy fails.

## Manual test with real credentials (needs the hardware and accounts)

No Developer ID or code-signing certificate was available when this was written, so these haven't
run yet. With them:

### macOS: Developer ID and notarization

On a Mac with Xcode's command line tools, the Developer ID Application certificate in the login
keychain, and a build (`cargo build --release -p rr-server -p rr-viewer`):

```sh
security find-identity -v -p codesigning           # shows "Developer ID Application: Name (TEAMID)"
export MACOS_CERTIFICATE=$(base64 -i ~/DeveloperID.p12) MACOS_CERTIFICATE_PASSWORD=…
export APPLE_ID=you@example.com APPLE_TEAM_ID=ABCDE12345 APPLE_APP_PASSWORD=abcd-efgh-ijkl-mnop
cp target/release/rein-remote target/release/rein-remote-viewer /tmp/
sh tools/sign/macos.sh /tmp/rein-remote /tmp/rein-remote-viewer
```

Expect: `Authority=Developer ID Application: …`, `Authority=Developer ID Certification Authority`,
`Authority=Apple Root CA`, a `Timestamp=` line, `flags=0x10000(runtime)`, then notarytool's JSON with
`"status":"Accepted"`. Then, as a user who downloaded it would:

```sh
codesign --verify --strict --verbose=4 /tmp/rein-remote          # valid on disk, satisfies its Designated Requirement
spctl --assess -vvv --type open --context context:primary-signature /tmp/rein-remote   # accepted, source=Notarized Developer ID
xattr -w com.apple.quarantine "0081;$(printf %x $(date +%s));Safari;" /tmp/rein-remote
/tmp/rein-remote --version                                          # runs, no Gatekeeper dialog
/tmp/rein-remote serve --window Finder                              # asks for Screen Recording once
# rebuild, sign again, replace /tmp/rein-remote: it must not ask again (same identifier and team)
```

Then the release itself: set the six `MACOS_*`/`APPLE_*` secrets, push a tag on a fork (never on this
repository without the maintainers), and check the `Sign and notarize (macOS, Developer ID)` step's log
shows the same, and that an archive from the release, downloaded with a browser, runs without a
Gatekeeper dialog.

### Windows: OV or EV certificate

On Windows with the Windows SDK (signtool) and the certificate as a .pfx:

```powershell
$env:WINDOWS_CERTIFICATE = [Convert]::ToBase64String([IO.File]::ReadAllBytes("C:\certs\codesign.pfx"))
$env:WINDOWS_CERTIFICATE_PASSWORD = '…'
Copy-Item target\release\rein-remote.exe, target\release\rein-remote-viewer.exe $env:TEMP\
pwsh tools/sign/windows.ps1 $env:TEMP\rein-remote.exe $env:TEMP\rein-remote-viewer.exe
```

Expect: `Successfully signed`, then `signtool verify /pa /v` with the chain up to a public root and
`The signature is timestamped: …`, then `Valid (CN=…), timestamped by CN=DigiCert …`. The file's
Properties → Digital Signatures tab shows the publisher; downloading a release archive with Edge and
running the exe shows the publisher's name in SmartScreen (an EV certificate has reputation at once;
an OV certificate builds it with downloads).

### Linux and SHA256SUMS: cosign, keyless

From any machine with cosign 2 or 3, for a release `vX.Y.Z`:

```sh
gh release download vX.Y.Z --repo rein-harness/rein-remote
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp '^https://github.com/rein-harness/rein-remote/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
sha256sum -c SHA256SUMS
```

Expect `Verified OK`, then `OK` for every archive. Changing a byte of SHA256SUMS must make the first
command fail.

The keyless branch of `tools/sign/cosign.sh` has not run yet: CI exercises the key branch only (an
OIDC token on every pull request would put a CI signature in the public log each time), and the
release step is `continue-on-error`, so a broken keyless step does **not** fail a release. On the first
tag, check that the release has `SHA256SUMS.sigstore.json` among its assets and that the command above
verifies it; if it's missing, the "Sign SHA256SUMS (cosign, keyless)" step's log says why.
