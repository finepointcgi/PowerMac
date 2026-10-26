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
BINS="powermac powermac-battery-helper"
OUT="$TARGET/release"

# The battery charge-limit helper (installed from Settings → Battery, with an
# admin prompt, from next to the app executable) is built with the app.
if [ "$UNIVERSAL" = "1" ]; then
  for T in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --target "$T" -p powermac
    cargo build --release --target "$T" -p battery-limit --bin powermac-battery-helper
  done
  mkdir -p "$OUT"
  for B in $BINS; do
    lipo -create -output "$OUT/$B.universal" \
      "$TARGET/aarch64-apple-darwin/release/$B" "$TARGET/x86_64-apple-darwin/release/$B"
  done
  SUFFIX=.universal
else
  cargo build --release -p powermac
  cargo build --release -p battery-limit --bin powermac-battery-helper
  SUFFIX=
fi

APP="$OUT/PowerMac.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
for B in $BINS; do
  cp "$OUT/$B$SUFFIX" "$APP/Contents/MacOS/$B"
done
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
  <key>NSAudioCaptureUsageDescription</key><string>PowerMac sets a volume for each app (Sound → Per-app volume). Nothing is recorded.</string>
  <key>NSCalendarsFullAccessUsageDescription</key><string>The Notch Shelf's Calendar widget shows your next events.</string>
  <key>NSCalendarsUsageDescription</key><string>The Notch Shelf's Calendar widget shows your next events.</string>
  <key>NSCameraUsageDescription</key><string>The Notch Shelf's Mirror widget shows the camera while you click it.</string>
  <key>NSAppleEventsUsageDescription</key><string>PowerMac reads the current track from Music and Spotify (Notch Shelf) and Finder's selection (File Tools) when you turn these on.</string>
</dict>
</plist>
PLIST

if [ -n "$SIGN_IDENTITY" ]; then
  # Distribution: Developer ID, hardened runtime, secure timestamp.
  ENT=scripts/PowerMac.entitlements
  codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP/Contents/MacOS/powermac-battery-helper"
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
  codesign --force --deep -s "$IDENTITY" "$APP"
else
  echo "note: ad-hoc signing; run scripts/create-signing-cert.sh once to keep permissions across rebuilds"
  codesign --force -s - "$APP/Contents/MacOS/powermac-battery-helper"
  codesign --force --deep -s - "$APP"
fi
echo "built $APP — move it to /Applications and open it"
