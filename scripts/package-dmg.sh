#!/bin/sh
# Build PowerMac.app and package it as a drag-to-Applications disk image:
#   $CARGO_TARGET_DIR/PowerMac-<version>-macos-<arch>.dmg   (+ .sha256)
#
# The version comes from the workspace Cargo.toml; the architecture from
# this Mac (arm64 / x86_64), or "universal" with UNIVERSAL=1 (Apple Silicon
# + Intel, merged by bundle-macos.sh).
#
# Environment:
#   CARGO_TARGET_DIR  where the build and the DMG go (default: target)
#   UNIVERSAL=1       universal binary (needs both Rust targets installed)
#   SKIP_BUILD=1      package the PowerMac.app that's already built
#   DMG_DIR=<dir>     put the DMG there instead of $CARGO_TARGET_DIR
#   SIGN_IDENTITY     sign the DMG with this identity (default: the local
#                     "PowerMac Local Signing" identity when it exists; none
#                     otherwise — an unsigned DMG is fine, the app inside is
#                     what macOS checks)
#   FANCY=1           a window with a background picture and an arrow from
#                     the app to Applications. Lays the window out with
#                     Finder through AppleScript, so macOS asks once to let
#                     Terminal control Finder; without it the DMG is a plain
#                     window with the app and an Applications link.
#
# Not notarized (that needs an Apple Developer account): on other Macs,
# Gatekeeper blocks the first launch until the user allows it in
# System Settings → Privacy & Security → "Open Anyway" (see README).
set -e
cd "$(dirname "$0")/.."
ROOT=$(pwd)
TARGET="${CARGO_TARGET_DIR:-target}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
if [ "$UNIVERSAL" = "1" ]; then
  ARCH=universal
else
  case "$(uname -m)" in
    arm64 | aarch64) ARCH=arm64 ;;
    x86_64) ARCH=x86_64 ;;
    *) ARCH=$(uname -m) ;;
  esac
fi

if [ "$SKIP_BUILD" != "1" ]; then
  ./scripts/bundle-macos.sh
fi
APP="$TARGET/release/PowerMac.app"
[ -d "$APP" ] || { echo "$APP not found (build it with scripts/bundle-macos.sh)" >&2; exit 1; }
APP_VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Contents/Info.plist" 2>/dev/null || echo "?")
[ "$APP_VERSION" = "$VERSION" ] || echo "warning: $APP is version $APP_VERSION, Cargo.toml says $VERSION" >&2

OUT_DIR="${DMG_DIR:-$TARGET}"
mkdir -p "$OUT_DIR"
DMG="$OUT_DIR/PowerMac-$VERSION-macos-$ARCH.dmg"
VOLNAME="PowerMac $VERSION"

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/powermac-dmg.XXXXXX")
MOUNT=""
cleanup() {
  if [ -n "$MOUNT" ] && [ -d "$MOUNT" ]; then hdiutil detach -quiet "$MOUNT" 2>/dev/null || true; fi
  rm -rf "$STAGE"
}
trap cleanup EXIT

# ditto keeps the code signature, extended attributes and symlinks intact.
ditto "$APP" "$STAGE/PowerMac.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"

if [ "$FANCY" = "1" ]; then
  mkdir -p "$STAGE/.background"
  python3 "$ROOT/scripts/dmg-background.py" "$STAGE/.background/background.png" 640 400
  RW="$STAGE.rw.dmg"
  hdiutil create -quiet -volname "$VOLNAME" -srcfolder "$STAGE" -fs HFS+ -format UDRW -ov "$RW"
  MOUNT="/Volumes/$VOLNAME"
  hdiutil attach -quiet -readwrite -noverify -noautoopen -mountpoint "$MOUNT" "$RW"
  osascript <<APPLESCRIPT
tell application "Finder"
  tell disk "$VOLNAME"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set the bounds of container window to {200, 120, 840, 520}
    set opts to the icon view options of container window
    set arrangement of opts to not arranged
    set icon size of opts to 112
    set text size of opts to 13
    set background picture of opts to file ".background:background.png"
    set position of item "PowerMac.app" of container window to {170, 190}
    set position of item "Applications" of container window to {470, 190}
    update without registering applications
    delay 1
    close
  end tell
end tell
APPLESCRIPT
  sync
  hdiutil detach -quiet "$MOUNT"
  MOUNT=""
  hdiutil convert -quiet "$RW" -format UDZO -imagekey zlib-level=9 -o "$DMG"
  rm -f "$RW"
else
  hdiutil create -quiet -volname "$VOLNAME" -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"
fi

IDENTITY="${SIGN_IDENTITY:-}"
if [ -z "$IDENTITY" ] && security find-certificate -c "PowerMac Local Signing" >/dev/null 2>&1; then
  IDENTITY="PowerMac Local Signing"
fi
if [ -n "$IDENTITY" ]; then
  if [ "$IDENTITY" = "PowerMac Local Signing" ]; then
    codesign --force -s "$IDENTITY" "$DMG"
  else
    codesign --force --timestamp -s "$IDENTITY" "$DMG"
  fi
  echo "signed $DMG with $IDENTITY"
else
  echo "note: DMG not signed (no signing identity; the app inside is signed)"
fi

hdiutil verify -quiet "$DMG"
SUM=$(shasum -a 256 "$DMG" | awk '{print $1}')
echo "$SUM  $(basename "$DMG")" > "$DMG.sha256"
SIZE=$(du -h "$DMG" | awk '{print $1}')
echo "built $DMG ($SIZE)"
echo "sha256 $SUM"
