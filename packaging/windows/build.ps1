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
# Set through msi.dll's own API (no SDK tool needed; its COM setter refuses PowerShell's values).
Add-Type -Namespace RR -Name Msi -MemberDefinition @'
[DllImport("msi.dll", CharSet = CharSet.Unicode)] public static extern uint MsiGetSummaryInformation(IntPtr db, string path, uint updates, out IntPtr handle);
[DllImport("msi.dll", CharSet = CharSet.Unicode)] public static extern uint MsiSummaryInfoGetProperty(IntPtr h, uint pid, out uint type, out int value, IntPtr ft, System.Text.StringBuilder sb, ref uint len);
[DllImport("msi.dll", CharSet = CharSet.Unicode)] public static extern uint MsiSummaryInfoSetProperty(IntPtr h, uint pid, uint type, int value, IntPtr ft, string s);
[DllImport("msi.dll")] public static extern uint MsiSummaryInfoPersist(IntPtr h);
[DllImport("msi.dll")] public static extern uint MsiCloseHandle(IntPtr h);
'@
$h = [IntPtr]::Zero
if ([RR.Msi]::MsiGetSummaryInformation([IntPtr]::Zero, $Out, 1, [ref]$h) -ne 0) { throw 'MsiGetSummaryInformation failed' }
try {
  $type = [uint32]0; $wc = 0; $len = [uint32]0
  if ([RR.Msi]::MsiSummaryInfoGetProperty($h, 15, [ref]$type, [ref]$wc, [IntPtr]::Zero, $null, [ref]$len) -ne 0) { throw 'reading Word Count failed' }
  # 3: VT_I4
  if ([RR.Msi]::MsiSummaryInfoSetProperty($h, 15, 3, ($wc -bor 8), [IntPtr]::Zero, $null) -ne 0) { throw 'setting Word Count failed' }
  if ([RR.Msi]::MsiSummaryInfoPersist($h) -ne 0) { throw 'MsiSummaryInfoPersist failed' }
  Write-Host "Word Count: $wc -> $($wc -bor 8) (no elevation needed)"
} finally { [void][RR.Msi]::MsiCloseHandle($h) }
Get-Item $Out | Format-Table Name, Length
