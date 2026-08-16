$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Src = if ($args[0]) { $args[0] } else { Join-Path $Root "target\release\airgap-xfer.exe" }
if (-not (Test-Path $Src)) { Write-Error "missing binary: $Src (cargo build --release)" }
$DestDir = Join-Path $env:LOCALAPPDATA "airgap-xfer"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
Copy-Item -Force $Src (Join-Path $DestDir "airgap-xfer.exe")
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not ($UserPath.Split(';') -contains $DestDir)) {
  [Environment]::SetEnvironmentVariable("Path", "$DestDir;$UserPath", "User")
}
Write-Host "installed $DestDir\airgap-xfer.exe"
