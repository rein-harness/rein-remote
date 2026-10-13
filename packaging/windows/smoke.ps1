# The .msi as a user installs it, on a CI runner: install silently, check what it put where, run the
# installed rein-remote.exe host and the installed app against it, uninstall with both running, and
# check everything is gone (and that no reboot was needed: the uninstaller stops them first).
#
#   pwsh packaging/windows/smoke.ps1 -Msi rein-remote.msi [-Scope user|machine] [-NoStartAtLogin]
param(
  [Parameter(Mandatory = $true)][string]$Msi,
  [ValidateSet('user', 'machine')][string]$Scope = 'user',
  [switch]$NoStartAtLogin
)
$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path $Msi).Path
$logs = Join-Path $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }) "rr-msi-$Scope"
New-Item -ItemType Directory -Force $logs | Out-Null
function Check($ok, $what) { if (-not $ok) { throw "FAILED: $what" }; Write-Host "ok: $what" }
function Msiexec([string[]]$a, $log) {
  $p = Start-Process msiexec.exe -ArgumentList ($a + @('/qn', '/l*v', "`"$log`"")) -Wait -PassThru
  if ($p.ExitCode -ne 0) { Get-Content $log -Tail 80 | Write-Host; throw "msiexec $($a -join ' '): exit $($p.ExitCode)" }
}

if ($Scope -eq 'user') {
  $dir = Join-Path $env:LOCALAPPDATA 'Programs\Rein Remote'
  $lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Rein Remote.lnk'
  $hive = 'HKCU:'
  $extra = @()
} else {
  $dir = Join-Path $env:ProgramFiles 'Rein Remote'
  $lnk = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Rein Remote.lnk'
  $hive = 'HKLM:'
  $extra = @('ALLUSERS=1')
}
if ($NoStartAtLogin) { $extra += 'STARTATLOGIN=0' }
$run = "$hive\Software\Microsoft\Windows\CurrentVersion\Run"
function Arp {
  foreach ($k in 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall') {
    Get-ChildItem $k -ErrorAction SilentlyContinue | Get-ItemProperty | Where-Object DisplayName -eq 'Rein Remote'
  }
}

Write-Host "== install ($Scope$(if ($NoStartAtLogin) { ', STARTATLOGIN=0' }))"
Msiexec (@('/i', "`"$Msi`"") + $extra) "$logs\install.log"
foreach ($f in 'ReinRemoteHost.exe', 'rein-remote.exe', 'Assets\rein-remote.ico', 'Microsoft.WindowsAppRuntime.dll') {
  Check (Test-Path (Join-Path $dir $f)) "$dir\$f installed"
}
Write-Host "$((Get-ChildItem $dir -Recurse -File).Count) files in $dir"
Check (Test-Path $lnk) "Start menu shortcut $lnk"
$target = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).TargetPath
Check ($target -eq (Join-Path $dir 'ReinRemoteHost.exe')) "the shortcut opens $target"
$value = (Get-ItemProperty $run -Name 'Rein Remote' -ErrorAction SilentlyContinue).'Rein Remote'
if ($NoStartAtLogin) {
  Check ($null -eq $value) "no Run key with STARTATLOGIN=0"
} else {
  Check ($value -eq "`"$(Join-Path $dir 'ReinRemoteHost.exe')`" --background") "Run key at sign-in: $value"
}
$arp = @(Arp)
Check ($arp.Count -eq 1) "one Add/Remove Programs entry: $($arp.DisplayName) $($arp.DisplayVersion) ($($arp.PSPath -replace '^.*::', ''))"
foreach ($f in 'ReinRemoteHost.exe', 'rein-remote.exe') {
  $s = Get-AuthenticodeSignature (Join-Path $dir $f)
  Write-Host "signature of ${f}: $($s.Status) $($s.SignerCertificate.Subject)"
}
& (Join-Path $dir 'rein-remote.exe') --version
Check ($LASTEXITCODE -eq 0) 'the installed rein-remote.exe runs'

Write-Host '== the installed host and app'
$home_ = Join-Path $logs 'home'
Remove-Item -Recurse -Force $home_ -ErrorAction SilentlyContinue
New-Item -ItemType Directory $home_ | Out-Null
$env:REIN_REMOTE_HOME = $home_
$host_ = Start-Process (Join-Path $dir 'rein-remote.exe') -ArgumentList 'host', '--test-pattern', '640x480', '--mdns', 'off' `
  -RedirectStandardError "$logs\host.log" -PassThru -WindowStyle Hidden
$deadline = (Get-Date).AddSeconds(60)
while (-not (Select-String -Path "$logs\host.log" -Pattern 'control API on' -Quiet -ErrorAction SilentlyContinue)) {
  if ($host_.HasExited -or (Get-Date) -gt $deadline) { Get-Content "$logs\host.log" | Write-Host; throw 'the host did not start its control API' }
  Start-Sleep -Milliseconds 300
}
$ctl = Get-Content (Join-Path $home_ 'control.json') -Raw | ConvertFrom-Json
$state = Invoke-RestMethod -Uri "$($ctl.url)/control/state" -Headers @{ Authorization = "Bearer $($ctl.token)" }
Check ($null -ne $state) "the installed host answers its control API ($($ctl.url)): $($state | ConvertTo-Json -Compress -Depth 2)"
$sh = Invoke-RestMethod -Method Post -Uri "$($ctl.url)/control/sharing/start" -Headers @{ Authorization = "Bearer $($ctl.token)" }
Start-Sleep -Seconds 2
$state = Invoke-RestMethod -Uri "$($ctl.url)/control/state" -Headers @{ Authorization = "Bearer $($ctl.token)" }
Check ($state.sharing -eq $true) "sharing the test pattern: $($state | ConvertTo-Json -Compress -Depth 2)"

# The app as the Run key starts it (in the tray): it finds the host above through control.json.
$app = Start-Process (Join-Path $dir 'ReinRemoteHost.exe') -ArgumentList '--background' -PassThru
Start-Sleep -Seconds 12
Check (-not $app.HasExited) "the installed app runs (pid $($app.Id))"
$applog = Get-Content (Join-Path $home_ 'app.log') -Raw -ErrorAction SilentlyContinue
Write-Host $applog
Check ($applog -match [regex]::Escape("host binary $(Join-Path $dir 'rein-remote.exe')")) 'the app found the rein-remote.exe installed beside it'

Write-Host '== uninstall, the app and the host still running'
Msiexec @('/x', "`"$Msi`"") "$logs\uninstall.log"
Start-Sleep -Seconds 2
Check (-not (Get-Process -Id $app.Id -ErrorAction SilentlyContinue)) 'the app was stopped'
Check (-not (Get-Process -Id $host_.Id -ErrorAction SilentlyContinue)) 'the host was stopped'
Check (-not (Test-Path $dir)) "$dir removed"
Check (-not (Test-Path $lnk)) 'the shortcut removed'
Check ($null -eq (Get-ItemProperty $run -Name 'Rein Remote' -ErrorAction SilentlyContinue)) 'the Run key removed'
Check (@(Arp).Count -eq 0) 'no Add/Remove Programs entry'
Check (-not (Test-Path "$hive\Software\Rein Harness\Rein Remote")) 'no registry left'
Check (-not (Select-String -Path "$logs\uninstall.log" -Pattern 'REBOOTREQUIRED|ScheduleReboot|files in use' -Quiet)) 'no reboot needed'
Remove-Item Env:\REIN_REMOTE_HOME
Write-Host "== ${Scope}: passed"
