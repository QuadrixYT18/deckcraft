#!/usr/bin/env bash
# DeckCraft live sessions: start the server. On macOS, double-click this file; on Linux run it.
#
# It checks what it needs, builds the server (and the browser editor) from this checkout when
# they aren't built yet, keeps its data in one folder, and shows the addresses to use.
# Settings: copy deckcraft-server.env.example to deckcraft-server.env next to this file.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
cd "$here"

say() { printf '%s\n' "$*"; }
fail() {
  printf '\nDeckCraft server: %s\n' "$*" >&2
  # Keep a double-clicked Terminal window open long enough to read the message.
  [[ -t 0 ]] && read -r -p "Press Return to close." _ || true
  exit 1
}

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
export DECKCRAFT_DATA="${DECKCRAFT_DATA:-$default_data}"
export DECKCRAFT_BIND="${DECKCRAFT_BIND:-0.0.0.0:8080}"
port="${DECKCRAFT_BIND##*:}"
mkdir -p "$DECKCRAFT_DATA" || fail "can't create the data folder $DECKCRAFT_DATA"
pidfile="$DECKCRAFT_DATA/server.pid"
logfile="$DECKCRAFT_DATA/server.log"

if [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
  say "The DeckCraft server is already running (process $(cat "$pidfile"))."
  say "Host dashboard: http://localhost:$port/host"
  exit 0
fi

# The server program: next to this script (a download), or built from this checkout.
# The server program: next to this script (a download), or built from this checkout. A checkout
# is always built: cargo only recompiles what changed, so after `git pull` the server is current.
bin=""
if [[ -x "$here/deckcraft-server" ]]; then
  bin="$here/deckcraft-server"
elif command -v cargo >/dev/null; then
  say "Building the DeckCraft server (the first time takes a few minutes)…"
  (cd "$repo" && cargo build --release -p deckcraft-server) || fail "the build failed (see above)"
  bin="$repo/target/release/deckcraft-server"
elif [[ -x "$repo/target/release/deckcraft-server" ]]; then
  bin="$repo/target/release/deckcraft-server"
else
  fail "Rust isn't installed. Install it from https://rustup.rs and run this again."
fi

# The browser editor: next to this script (web/), or dist/web in this checkout.
if [[ -z "${DECKCRAFT_WEB_DIR:-}" ]]; then
  if [[ -f "$here/web/index.html" ]]; then
    export DECKCRAFT_WEB_DIR="$here/web"
  else
    export DECKCRAFT_WEB_DIR="$repo/dist/web"
  fi
fi
rev="$(git -C "$repo" rev-parse HEAD 2>/dev/null || true)"
built="$(cat "$DECKCRAFT_WEB_DIR/.built-rev" 2>/dev/null || true)"
if [[ "$DECKCRAFT_WEB_DIR" == "$repo/dist/web" && -n "$rev" && "$rev" != "$built" ]] && command -v trunk >/dev/null; then
  rm -f "$DECKCRAFT_WEB_DIR/index.html"
fi
if [[ ! -f "$DECKCRAFT_WEB_DIR/index.html" ]]; then
  if command -v trunk >/dev/null; then
    say "Building the browser editor (the first time takes several minutes)…"
    fonts="${CRAFT_FONTS_DIR:-$repo/../craft-fonts}"
    [[ -d "$fonts" ]] || say "Note: $fonts not found; the editor is built without the bundled fonts (see README)."
    (cd "$repo/apps/deckcraft-web" && CRAFT_FONTS_DIR="$fonts" trunk build --release) || fail "the browser editor build failed (see above)"
    [[ -n "$rev" ]] && echo "$rev" >"$DECKCRAFT_WEB_DIR/.built-rev"
  else
    say "Note: the browser editor isn't built, so people can't edit in a browser yet."
    say "      Install trunk (cargo install trunk --locked) and run this again, or see docs/collaboration.md."
  fi
fi

if command -v lsof >/dev/null && lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
  fail "port $port is in use by another program. Set DECKCRAFT_BIND=0.0.0.0:<other port> in deckcraft-server.env."
fi

nohup "$bin" >>"$logfile" 2>&1 &
echo $! >"$pidfile"

ok=""
for _ in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then ok=1; break; fi
  kill -0 "$(cat "$pidfile")" 2>/dev/null || break
  sleep 0.5
done
if [[ -z "$ok" ]]; then
  rm -f "$pidfile"
  say "The last lines of $logfile:"
  tail -n 15 "$logfile" >&2 || true
  fail "the server didn't start"
fi

lan=""
if [[ "$(uname -s)" == "Darwin" ]]; then
  for ifc in en0 en1 en2; do lan="$(ipconfig getifaddr "$ifc" 2>/dev/null || true)"; [[ -n "$lan" ]] && break; done
else
  lan="$(hostname -I 2>/dev/null | awk '{print $1}')"
fi

say ""
say "DeckCraft live sessions are running."
say ""
say "  On this computer:   http://localhost:$port/host     (start a session, see the access code)"
[[ -n "$lan" ]] && say "  Others on your network open:   http://$lan:$port/"
say "  In the DeckCraft app: Review › Live Session, server http://localhost:$port"
say ""
say "  Data:  $DECKCRAFT_DATA"
say "  Log:   $logfile"
say "  Stop:  stop.command"
if [[ "$(uname -s)" == "Darwin" ]]; then
  say ""
  say "If macOS asks whether deckcraft-server may accept incoming connections, choose Allow."
  open "http://localhost:$port/host" >/dev/null 2>&1 || true
fi
