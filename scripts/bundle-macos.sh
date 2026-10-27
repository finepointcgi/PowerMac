#!/bin/sh
# Build PowerMac.app: a menu-bar-only app (no Dock icon). macOS ties the
# Accessibility / Input Monitoring grants to this bundle instead of your terminal.
# Honors CARGO_TARGET_DIR (the bundle goes to $CARGO_TARGET_DIR/release).
#
# Release builds (CI) also use:
#   UNIVERSAL=1             build aarch64 + x86_64 and merge them with lipo
#   SIGN_IDENTITY="Developer ID Application: …"
#                           sign with that identity, hardened runtime and a
#                           secure timestamp (needed for notarization)
set -e
cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-target}"
# powermac-battery-helper: the battery charge-limit helper. It is only copied
# into Contents/MacOS here; it is installed as a root LaunchDaemon solely when
# the user enables the charge limit (one admin prompt), never by this script.
BINS="powermac powermac-battery-helper"
# Helper libraries, shipped in Contents/Frameworks (Notch Shelf Now Playing
# loads libpm_nowplaying into /usr/bin/perl).
LIBS="libpm_nowplaying.dylib"
OUT="$TARGET/release"

if [ "$UNIVERSAL" = "1" ]; then
  for T in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --target "$T" -p powermac -p pm-nowplaying
    cargo build --release --target "$T" -p battery-limit --bin powermac-battery-helper
  done
  mkdir -p "$OUT"
  for B in $BINS; do
    lipo -create -output "$OUT/$B.universal" \
      "$TARGET/aarch64-apple-darwin/release/$B" "$TARGET/x86_64-apple-darwin/release/$B"
  done
  for L in $LIBS; do
    lipo -create -output "$OUT/$L.universal" \
      "$TARGET/aarch64-apple-darwin/release/$L" "$TARGET/x86_64-apple-darwin/release/$L"
  done
  SUFFIX=.universal
else
  cargo build --release -p powermac -p pm-nowplaying
  cargo build --release -p battery-limit --bin powermac-battery-helper
  SUFFIX=
fi

# The PowerMac Audio virtual device (HAL plug-in). It is only shipped in
# Contents/Resources (audio_control::driver::find_bundled_driver looks
# there); installing it is a separate, confirmed step in Settings → Sound.
# build-audio-driver.sh honors UNIVERSAL / SIGN_IDENTITY / CARGO_TARGET_DIR
# and signs the driver bundle itself.
DRIVER="PowerMac Audio.driver"
sh scripts/build-audio-driver.sh

APP="$OUT/PowerMac.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
for B in $BINS; do
  cp "$OUT/$B$SUFFIX" "$APP/Contents/MacOS/$B"
done
for L in $LIBS; do
  cp "$OUT/$L$SUFFIX" "$APP/Contents/Frameworks/$L"
done
# ditto keeps the driver's own signature intact.
ditto "$OUT/$DRIVER" "$APP/Contents/Resources/$DRIVER"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>PowerMac</string>
  <key>CFBundleDisplayName</key><string>PowerMac</string>
  <key>CFBundleIdentifier</key><string>com.powermac.app</string>
  <key>CFBundleExecutable</key><string>powermac</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAudioCaptureUsageDescription</key><string>PowerMac sets a volume for each app and routes or mixes app audio (Sound → Per-app volume, App routing, Mixer). Nothing is recorded.</string>
  <key>NSMicrophoneUsageDescription</key><string>The Sound mixer plays microphones and audio interfaces you add as sources into the outputs you choose. Nothing is recorded or sent anywhere.</string>
  <key>NSCalendarsFullAccessUsageDescription</key><string>The Notch Shelf's Calendar widget shows your next events.</string>
  <key>NSCalendarsUsageDescription</key><string>The Notch Shelf's Calendar widget shows your next events.</string>
  <key>NSRemindersFullAccessUsageDescription</key><string>The Notch Shelf's Reminders widget shows your open reminders and lets you tick them off.</string>
  <key>NSRemindersUsageDescription</key><string>The Notch Shelf's Reminders widget shows your open reminders and lets you tick them off.</string>
  <key>NSCameraUsageDescription</key><string>The Notch Shelf's Mirror widget shows the camera while you click it.</string>
  <key>NSAppleEventsUsageDescription</key><string>PowerMac reads the current track from Music and Spotify (Notch Shelf) and Finder's selection (File Tools) when you turn these on.</string>
</dict>
</plist>
PLIST

if [ -n "$SIGN_IDENTITY" ]; then
  # Distribution: Developer ID, hardened runtime, secure timestamp.
  ENT=scripts/PowerMac.entitlements
  for L in $LIBS; do
    codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP/Contents/Frameworks/$L"
  done
  codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP/Contents/MacOS/powermac-battery-helper"
  codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP/Contents/Resources/$DRIVER"
  codesign --force --options runtime --timestamp --entitlements "$ENT" -s "$SIGN_IDENTITY" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
  echo "built $APP (signed with $SIGN_IDENTITY)"
  exit 0
fi

# Sign with the stable local identity if scripts/create-signing-cert.sh was run,
# so permission grants survive rebuilds; otherwise ad-hoc (grants reset on
# every rebuild).
IDENTITY="PowerMac Local Signing"
if security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
  codesign --force -s "$IDENTITY" "$APP/Contents/MacOS/powermac-battery-helper"
  codesign --force -s "$IDENTITY" "$APP/Contents/Resources/$DRIVER"
  codesign --force --deep -s "$IDENTITY" "$APP"
else
  echo "note: ad-hoc signing; run scripts/create-signing-cert.sh once to keep permissions across rebuilds"
  codesign --force -s - "$APP/Contents/MacOS/powermac-battery-helper"
  codesign --force -s - "$APP/Contents/Resources/$DRIVER"
  codesign --force --deep -s - "$APP"
fi
echo "built $APP — move it to /Applications and open it"
