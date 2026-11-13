# DeckCraft live sessions: start the server on Windows (start.bat runs this).
# Settings: copy deckcraft-server.env.example to deckcraft-server.env next to this file.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $here
Set-Location $here

function Fail($msg) {
  Write-Host "`nDeckCraft server: $msg" -ForegroundColor Red
  Read-Host 'Press Enter to close' | Out-Null
  exit 1
}

$envFile = Join-Path $here 'deckcraft-server.env'
if (Test-Path $envFile) {
  foreach ($line in Get-Content $envFile) {
    if ($line -match '^\s*([A-Z_]+)\s*=\s*(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2].Trim(), 'Process') }
  }
}
if (-not $env:DECKCRAFT_DATA) { $env:DECKCRAFT_DATA = Join-Path $env:LOCALAPPDATA 'DeckCraft\live-server' }
if (-not $env:DECKCRAFT_BIND) { $env:DECKCRAFT_BIND = '0.0.0.0:8080' }
$port = $env:DECKCRAFT_BIND.Split(':')[-1]
New-Item -ItemType Directory -Force -Path $env:DECKCRAFT_DATA | Out-Null
$pidFile = Join-Path $env:DECKCRAFT_DATA 'server.pid'
$logFile = Join-Path $env:DECKCRAFT_DATA 'server.log'

if (Test-Path $pidFile) {
  $old = Get-Process -Id (Get-Content $pidFile) -ErrorAction SilentlyContinue
  if ($old) { Write-Host "The DeckCraft server is already running. Host dashboard: http://localhost:$port/host"; exit 0 }
}

$bin = @((Join-Path $here 'deckcraft-server.exe'), (Join-Path $repo 'target\release\deckcraft-server.exe')) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $bin) {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail 'Rust is not installed. Install it from https://rustup.rs and run this again.' }
  Write-Host 'Building the DeckCraft server (first start only, a few minutes)...'
  Push-Location $repo; cargo build --release -p deckcraft-server; $ok = $?; Pop-Location
  if (-not $ok) { Fail 'the build failed (see above)' }
  $bin = Join-Path $repo 'target\release\deckcraft-server.exe'
}

if (-not $env:DECKCRAFT_WEB_DIR) {
  $env:DECKCRAFT_WEB_DIR = if (Test-Path (Join-Path $here 'web\index.html')) { Join-Path $here 'web' } else { Join-Path $repo 'dist\web' }
}
if (-not (Test-Path (Join-Path $env:DECKCRAFT_WEB_DIR 'index.html'))) {
  Write-Host 'Note: the browser editor is not built, so people cannot edit in a browser yet. See docs/collaboration.md.' -ForegroundColor Yellow
}

if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
  Fail "port $port is in use by another program. Set DECKCRAFT_BIND=0.0.0.0:<other port> in deckcraft-server.env."
}

$p = Start-Process -FilePath $bin -WorkingDirectory $here -RedirectStandardOutput $logFile -RedirectStandardError "$logFile.err" -WindowStyle Hidden -PassThru
Set-Content -Path $pidFile -Value $p.Id

$up = $false
for ($i = 0; $i -lt 30; $i++) {
  try { Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$port/healthz" -TimeoutSec 2 | Out-Null; $up = $true; break } catch { Start-Sleep -Milliseconds 500 }
  if ($p.HasExited) { break }
}
if (-not $up) {
  Remove-Item $pidFile -ErrorAction SilentlyContinue
  Get-Content $logFile, "$logFile.err" -Tail 15 -ErrorAction SilentlyContinue
  Fail "the server did not start"
}

$lan = (Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' -and $_.PrefixOrigin -ne 'WellKnown' } | Select-Object -First 1).IPAddress
Write-Host ''
Write-Host 'DeckCraft live sessions are running.' -ForegroundColor Green
Write-Host "  On this computer:  http://localhost:$port/host   (start a session, see the access code)"
if ($lan) { Write-Host "  Others on your network open:  http://${lan}:$port/" }
Write-Host "  In the DeckCraft app: Review > Live Session, server http://localhost:$port"
Write-Host "  Data: $($env:DECKCRAFT_DATA)"
Write-Host '  Stop: stop.bat'
Write-Host 'If Windows asks whether deckcraft-server may use the network, allow it for private networks.'
Start-Process "http://localhost:$port/host"
