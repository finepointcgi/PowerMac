#!/usr/bin/env bash
# Capture screenshots of every settings page and the FancyZones layout
# editor for the documentation, using a throwaway demo config so no personal
# settings end up in the images.
#
#   ./scripts/docs-media/capture.sh            # all pages
#   ./scripts/docs-media/capture.sh sound      # one page
#
# Needs a release build (cargo build --release -p powermac) and Screen
# Recording permission for the terminal. Images go to docs/assets/screenshots.
# When PowerMac is running, its *-status.json files (device lists, running
# state; no settings) are copied next to the demo config before each capture
# so pages show live data instead of "PowerMac isn't running". STATUS_FROM=
# (empty) turns that off.
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$PWD
BIN=${BIN:-$ROOT/target/release/powermac}
OUT=$ROOT/docs/assets/screenshots
WORK=${TMPDIR:-/tmp}/powermac-docs-media
WIDTH=${WIDTH:-1400}
mkdir -p "$OUT" "$WORK"

WINID=$WORK/winid
[ -x "$WINID" ] && [ "$WINID" -nt "$ROOT/scripts/docs-media/winid.swift" ] ||
  swiftc -O "$ROOT/scripts/docs-media/winid.swift" -o "$WINID"

CONFIG=$WORK/demo/config.json
if [ ! -f "$CONFIG" ]; then
  mkdir -p "$(dirname "$CONFIG")"
  "$BIN" init --config "$CONFIG" >/dev/null
fi

STATUS_FROM=${STATUS_FROM-$HOME/Library/Application Support/PowerMac}

pause() { perl -e "select(undef,undef,undef,$1)"; }

# capture <name> <command...>: start the window, wait for it, grab it, close it.
capture() {
  local name=$1; shift
  if [ -n "$STATUS_FROM" ] && [ -d "$STATUS_FROM" ]; then
    cp "$STATUS_FROM"/*-status.json "$(dirname "$CONFIG")"/ 2>/dev/null || true
  fi
  "$@" >/dev/null 2>&1 &
  local pid=$! win=""
  for _ in $(seq 40); do
    win=$("$WINID" "$pid" | head -1 | cut -d' ' -f1)
    [ -n "$win" ] && break
    pause 0.25
  done
  [ -n "$win" ] || { echo "no window for $name" >&2; kill "$pid" 2>/dev/null; return 1; }
  pause "${SETTLE:-1.5}"
  screencapture -x -o -l "$win" "$WORK/$name.png"
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
  # Scale down and reduce to a 256-color palette: a third of the size, same look.
  sips --resampleWidth "$WIDTH" "$WORK/$name.png" --out "$WORK/$name-scaled.png" >/dev/null
  ffmpeg -loglevel error -y -i "$WORK/$name-scaled.png" -vf \
    "split[a][b];[a]palettegen=max_colors=256:reserve_transparent=1[p];[b][p]paletteuse=dither=sierra2_4a" \
    "$OUT/$name.png"
  echo "$OUT/$name.png"
}

PAGES=(general fancyzones window-switcher notch-shelf notch-widgets dock-previews menu-bar
  mouse scrolling mouse-utilities keyboard-manager text-expansion displays sound awake
  clipboard-history color-picker text-extractor file-tools mouse-without-borders)
[ $# -gt 0 ] && PAGES=("$@")

for page in "${PAGES[@]}"; do
  if [ "$page" = editor ]; then
    capture fancyzones-editor "$BIN" editor --config "$CONFIG"
  else
    capture "settings-$page" "$BIN" settings --config "$CONFIG" --page "$page"
  fi
done
[ $# -eq 0 ] && capture fancyzones-editor "$BIN" editor --config "$CONFIG"
exit 0
