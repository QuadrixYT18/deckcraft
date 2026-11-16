# DeckCraft live sessions: stop the server started by start.bat.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$envFile = Join-Path $here 'deckcraft-server.env'
if (Test-Path $envFile) {
  foreach ($line in Get-Content $envFile) {
    if ($line -match '^\s*([A-Z_]+)\s*=\s*(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2].Trim(), 'Process') }
  }
}
if (-not $env:DECKCRAFT_DATA) { $env:DECKCRAFT_DATA = Join-Path $env:LOCALAPPDATA 'DeckCraft\live-server' }
$pidFile = Join-Path $env:DECKCRAFT_DATA 'server.pid'
if (-not (Test-Path $pidFile)) { Write-Host "The DeckCraft server isn't running."; exit 0 }
$p = Get-Process -Id (Get-Content $pidFile) -ErrorAction SilentlyContinue
if ($p) {
  # Windows has no Ctrl+C for a hidden process. The server writes every change within two
  # seconds, so waiting that long first loses nothing.
  Start-Sleep -Seconds 3
  Stop-Process -Id $p.Id
  Write-Host 'DeckCraft server stopped.'
} else {
  Write-Host "The DeckCraft server isn't running."
}
Remove-Item $pidFile -ErrorAction SilentlyContinue
