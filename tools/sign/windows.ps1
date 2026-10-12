# Signs Windows binaries with Authenticode (signtool), timestamped, then checks them.
#
#   pwsh tools/sign/windows.ps1 FILE... [-SelfSigned]
#
# With a code-signing certificate (release.yml, from the repository's secrets):
#   WINDOWS_CERTIFICATE           base64 of a .pfx (an OV or EV code-signing certificate and its key)
#   WINDOWS_CERTIFICATE_PASSWORD  the .pfx's password
#   WINDOWS_TIMESTAMP_URL         optional (default http://timestamp.digicert.com, RFC 3161)
# -SelfSigned (CI): a certificate made here, trusted on this machine only, so the whole path runs
#   (sign, verify against a trusted root) with nothing secret. Not timestamped (no network needed).
# Neither: says so and signs nothing.
param(
  [Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Files,
  [switch]$SelfSigned
)
$ErrorActionPreference = 'Stop'
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
  Sort-Object FullName -Descending | Select-Object -First 1
if (-not $signtool) { throw 'signtool.exe not found (Windows SDK)' }
Write-Host "signtool: $($signtool.FullName)"
function Run { & $signtool @args; if ($LASTEXITCODE -ne 0) { throw "signtool $($args[0]) failed ($LASTEXITCODE)" } }

if ($env:WINDOWS_CERTIFICATE) {
  $pfx = Join-Path ([IO.Path]::GetTempPath()) ("sign-" + [guid]::NewGuid() + '.pfx')
  try {
    [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE))
    $ts = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    Run sign /fd sha256 /tr $ts /td sha256 /f $pfx /p $env:WINDOWS_CERTIFICATE_PASSWORD /d 'Rein Remote' @Files
  } finally {
    Remove-Item -Force -ErrorAction SilentlyContinue $pfx
  }
} elseif ($SelfSigned) {
  $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Rein Remote CI (self-signed, test only)' `
    -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddDays(2)
  # Trusted here only (so verify /pa can build the chain); a real certificate chains to a public root.
  $cer = Join-Path ([IO.Path]::GetTempPath()) 'rein-remote-ci.cer'
  Export-Certificate -Cert $cert -FilePath $cer | Out-Null
  Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
  Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
  Run sign /fd sha256 /s My /sha1 $cert.Thumbprint /d 'Rein Remote' @Files
} else {
  Write-Host 'no WINDOWS_CERTIFICATE: not signed'
  exit 0
}
Run verify /pa /v @Files
foreach ($f in $Files) {
  $s = Get-AuthenticodeSignature $f
  Write-Host "$f : $($s.Status) ($($s.SignerCertificate.Subject))$(if ($s.TimeStamperCertificate) { ', timestamped by ' + $s.TimeStamperCertificate.Subject })"
  if ($s.Status -ne 'Valid') { throw "$f : $($s.Status) $($s.StatusMessage)" }
}
