$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
$Release = Join-Path $Root "target\release\airgap-xfer.exe"
$Debug = Join-Path $Root "target\debug\airgap-xfer.exe"
$Shipped = Join-Path $Root "bin\airgap-xfer.exe"
$DestDir = Join-Path $Root "bin"
$Dest = Join-Path $DestDir "airgap-xfer.exe"
$Src = if ($args[0]) {
  $args[0]
} elseif (Test-Path $Release) {
  $Release
} elseif (Test-Path $Debug) {
  $Debug
} else {
  $Shipped
}
if (-not (Test-Path $Src)) { Write-Error "missing binary: $Src (copy bin/airgap-xfer.exe or cargo build --release)" }
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
$SrcFull = (Resolve-Path $Src).Path
$DestFull = $Dest
if (Test-Path $Dest) { $DestFull = (Resolve-Path $Dest).Path }
if ($SrcFull -ne $DestFull) {
  Copy-Item -Force $Src $Dest
}
Write-Host "installed $Dest"
