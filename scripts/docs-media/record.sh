#!/usr/bin/env bash
# Record an animated GIF of a PowerMac window driven by a scripted pointer,
# using the same throwaway demo config as capture.sh.
#
#   ./scripts/docs-media/record.sh <name> <settings-page|editor|region:X,Y,W,H> <script>
#
# <script> holds mouse.swift commands with coordinates relative to the
# window's top-left corner (in points). region:X,Y,W,H records that part of
# the main screen (points) instead, for the running app's notch, overlays
# and so on; the script's coordinates are then relative to the region. The GIF goes to docs/assets/gifs.
# Needs Screen Recording and Accessibility permission for the terminal, and
# moves the real pointer while it runs.
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$PWD
NAME=$1 TARGET=$2 SCRIPT=$3
BIN=${BIN:-$ROOT/target/release/powermac}
OUT=$ROOT/docs/assets/gifs
WORK=${TMPDIR:-/tmp}/powermac-docs-media
WIDTH=${WIDTH:-900} FPS=${FPS:-15}
mkdir -p "$OUT" "$WORK"

for tool in winid mouse; do
  [ -x "$WORK/$tool" ] && [ "$WORK/$tool" -nt "$ROOT/scripts/docs-media/$tool.swift" ] ||
    swiftc -O "$ROOT/scripts/docs-media/$tool.swift" -o "$WORK/$tool"
done
CONFIG=$WORK/demo/config.json
[ -f "$CONFIG" ] || { mkdir -p "$WORK/demo"; "$BIN" init --config "$CONFIG" >/dev/null; }
STATUS_FROM=${STATUS_FROM-$HOME/Library/Application Support/PowerMac}
[ -n "$STATUS_FROM" ] && cp "$STATUS_FROM"/*-status.json "$WORK/demo/" 2>/dev/null || true
pause() { perl -e "select(undef,undef,undef,$1)"; }

APP=
if [[ $TARGET == region:* ]]; then
  IFS=, read -r X Y W H <<<"${TARGET#region:}"
elif [ "$TARGET" = editor ]; then
  "$BIN" editor --config "$CONFIG" >/dev/null 2>&1 &
else
  "$BIN" settings --config "$CONFIG" --page "$TARGET" >/dev/null 2>&1 &
fi
if [ -z "${W:-}" ]; then
  APP=$!
  trap 'kill $APP 2>/dev/null || true' EXIT
  for _ in $(seq 40); do
    read -r _ X Y W H _ < <("$WORK/winid" "$APP" | head -1) && [ -n "${W:-}" ] && break
    pause 0.25
  done
  pause 1.5
fi

# Screen pixels per point (2 on Retina).
SCALE=$(system_profiler SPDisplaysDataType | awk '/Retina/ {print 2; exit}')
SCALE=${SCALE:-1}
SCREEN=$( (ffmpeg -hide_banner -f avfoundation -list_devices true -i "" 2>&1 || true) |
  (sed -n 's/.*\[\([0-9]*\)\] Capture screen 0.*/\1/p'))

ffmpeg -loglevel error -y -f avfoundation -capture_cursor 1 -pixel_format bgr0 -framerate 30 -i "$SCREEN:none" \
  -vf "crop=$((W*SCALE)):$((H*SCALE)):$((X*SCALE)):$((Y*SCALE))" -c:v libx264 -preset ultrafast -crf 12 \
  "$WORK/$NAME.mp4" </dev/null &
REC=$!
pause 1
# Shift the script's window-relative coordinates to the screen.
awk -v x="$X" -v y="$Y" '$1=="move"{$2+=x;$3+=y} {print}' "$SCRIPT" | "$WORK/mouse"
pause 0.8
kill -INT $REC; wait $REC || true

ffmpeg -loglevel error -y -ss 0.8 -i "$WORK/$NAME.mp4" -vf \
  "fps=$FPS,scale=$WIDTH:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle" \
  "$OUT/$NAME.gif"
echo "$OUT/$NAME.gif ($(du -h "$OUT/$NAME.gif" | cut -f1))"
