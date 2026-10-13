# Builds Rein Remote's .msi from the published Windows host app (the folder `dotnet publish
# --self-contained` makes, with rein-remote.exe in it: ci.yml's windows-host-app-build artifact).
#
#   pwsh packaging/windows/build.ps1 -AppDir "$env:RUNNER_TEMP\Rein Remote" -OutDir out    # out\rein-remote-VERSION-x64.msi
#
# Signing is the workflow's, around this (so the certificate's secrets are in those steps only):
# tools/sign/windows.ps1 on ReinRemoteHost.exe and rein-remote.exe before, on the .msi after (CI does
# the same with -SelfSigned). Needs WiX v5: dotnet tool install --global wix --version 5.0.2
param(
  [Parameter(Mandatory = $true)][string]$AppDir,
  [Parameter(Mandatory = $true)][string]$OutDir,
  [string]$Version
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
if (-not $Version) {
  $Version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([0-9.]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}
foreach ($f in 'ReinRemoteHost.exe', 'rein-remote.exe', 'Assets\rein-remote.ico') {
  if (-not (Test-Path (Join-Path $AppDir $f))) { throw "$AppDir has no $f" }
}
New-Item -ItemType Directory -Force $OutDir | Out-Null
$Out = Join-Path (Resolve-Path $OutDir).Path "rein-remote-$Version-x64.msi"

wix extension add -g WixToolset.Util.wixext/5.0.2
if ($LASTEXITCODE -ne 0) { throw 'wix extension add failed' }
wix build -arch x64 -ext WixToolset.Util.wixext -d "AppDir=$((Resolve-Path $AppDir).Path)" -d "Version=$Version" `
  (Join-Path $PSScriptRoot 'rein-remote.wxs') -o $Out
if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
# Per user without UAC: Summary Information's Word Count (property 15) gets bit 3, "elevated privileges
# are not required" (WiX 5's perUserOrMachine leaves it off, so a standard user would be asked for an
# administrator). ALLUSERS=1 is then for an elevated prompt (msiexec doesn't ask on its own).
$installer = New-Object -ComObject WindowsInstaller.Installer
$si = $installer.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $installer, @($Out, 1))
$wc = $si.GetType().InvokeMember('Property', 'GetProperty', $null, $si, @(15))
$si.GetType().InvokeMember('Property', 'SetProperty', $null, $si, @(15, ($wc -bor 8))) | Out-Null
$si.GetType().InvokeMember('Persist', 'InvokeMethod', $null, $si, $null) | Out-Null
[void][Runtime.InteropServices.Marshal]::ReleaseComObject($si)
Write-Host "Word Count: $wc -> $($wc -bor 8) (no elevation needed)"
Get-Item $Out | Format-Table Name, Length
