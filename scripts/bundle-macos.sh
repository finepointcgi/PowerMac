#!/bin/sh
# Build PowerMac.app: a menu-bar-only app (no Dock icon). macOS ties the
# Accessibility / Input Monitoring grants to this bundle instead of your terminal.
set -e
cd "$(dirname "$0")/.."
cargo build --release -p powermac
APP=target/release/PowerMac.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/powermac "$APP/Contents/MacOS/powermac"
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
</dict>
</plist>
PLIST
# Sign with the stable local identity if scripts/create-signing-cert.sh was run,
# so permission grants survive rebuilds; otherwise ad-hoc (grants reset on
# every rebuild).
IDENTITY="PowerMac Local Signing"
if security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
  codesign --force --deep -s "$IDENTITY" "$APP"
else
  echo "note: ad-hoc signing; run scripts/create-signing-cert.sh once to keep permissions across rebuilds"
  codesign --force --deep -s - "$APP"
fi
echo "built $APP — move it to /Applications and open it"
