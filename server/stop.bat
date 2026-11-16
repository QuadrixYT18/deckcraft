@echo off
rem DeckCraft live sessions: stop the server.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0stop.ps1"
pause
