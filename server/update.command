#!/usr/bin/env bash
# DeckCraft: update from this checkout. On macOS, double-click this file (Help › Update DeckCraft…
# in the app opens it). It quits DeckCraft and the live server, fetches the newest `main`, the
# translations and fonts beside it, builds the app and puts it in /Applications, then opens it.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
parent="$(cd "$repo/.." && pwd)"

fail() {
  printf '\nDeckCraft update: %s\n' "$*" >&2
  [[ -t 0 ]] && read -r -p "Press Return to close." _ || true
  exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || fail "this script builds the macOS app; on other systems build with cargo."
command -v git >/dev/null || fail "git is missing (install Xcode Command Line Tools: xcode-select --install)."
command -v cargo >/dev/null || fail "Rust is missing (https://rustup.rs)."

echo "==> Quitting DeckCraft and the live server"
osascript -e 'quit app "DeckCraft"' >/dev/null 2>&1 || true
"$here/stop.command" >/dev/null 2>&1 || true

echo "==> Fetching the newest DeckCraft"
cd "$repo"
if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
  fail "this checkout has changes of its own ($repo); commit or stash them first."
fi
git fetch -q origin main
git checkout -q main
git pull -q --ff-only origin main || fail "could not fast-forward main."

echo "==> Translations and fonts"
if [[ -d "$parent/craft-i18n/.git" ]]; then git -C "$parent/craft-i18n" pull -q || true; else git clone -q https://github.com/storytold/craft-i18n "$parent/craft-i18n"; fi
if [[ -d "$parent/craft-fonts/.git" ]]; then git -C "$parent/craft-fonts" pull -q || true; fi

echo "==> Building (a few minutes)"
arch="$(uname -m)"; [[ "$arch" == "arm64" ]] && arch=aarch64
CRAFT_FONTS_DIR="$parent/craft-fonts" CRAFT_I18N_DIR="$parent/craft-i18n" packaging/macos/package.sh --arch "$arch" || fail "the build failed (see above)."

echo "==> Installing"
rm -rf /Applications/DeckCraft.app
cp -R target/macos-package/DeckCraft.app /Applications/
open -a DeckCraft
echo "==> Done: DeckCraft is up to date."
