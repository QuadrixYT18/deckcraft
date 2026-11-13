#!/usr/bin/env bash
# DeckCraft live sessions: stop the server started by start.command. Everything is saved first.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"
if [[ -f deckcraft-server.env ]]; then
  set -a
  # shellcheck disable=SC1091
  . ./deckcraft-server.env
  set +a
fi
if [[ "$(uname -s)" == "Darwin" ]]; then
  default_data="$HOME/Library/Application Support/DeckCraft/live-server"
else
  default_data="${XDG_DATA_HOME:-$HOME/.local/share}/deckcraft/live-server"
fi
pidfile="${DECKCRAFT_DATA:-$default_data}/server.pid"
if [[ ! -f "$pidfile" ]] || ! kill -0 "$(cat "$pidfile")" 2>/dev/null; then
  echo "The DeckCraft server isn't running."
  rm -f "$pidfile"
  exit 0
fi
pid="$(cat "$pidfile")"
# SIGINT is the server's Ctrl+C: it saves every session, then exits.
kill -INT "$pid"
for _ in $(seq 1 40); do
  kill -0 "$pid" 2>/dev/null || { rm -f "$pidfile"; echo "DeckCraft server stopped. Everything is saved."; exit 0; }
  sleep 0.25
done
echo "The server is still saving; stopping it now." >&2
kill -TERM "$pid" 2>/dev/null || true
rm -f "$pidfile"
