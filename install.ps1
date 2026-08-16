$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Release = Join-Path $Root "target\release\airgap-xfer.exe"
$Debug = Join-Path $Root "target\debug\airgap-xfer.exe"
$Src = if ($args[0]) {
  $args[0]
} elseif (Test-Path $Release) {
  $Release
} elseif (Test-Path $Debug) {
  $Debug
} else {
  $Release
}
if (-not (Test-Path $Src)) { Write-Error "missing binary: $Src (cargo build --release)" }
$DestDir = Join-Path $Root "bin"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
Copy-Item -Force $Src (Join-Path $DestDir "airgap-xfer.exe")
Write-Host "installed $DestDir\airgap-xfer.exe"
