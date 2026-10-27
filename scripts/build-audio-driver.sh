#!/bin/sh
# Build "PowerMac Audio.driver", the Core Audio HAL plug-in (virtual loopback
# devices), from crates/pm-audio-driver and sign it. Nothing is installed:
# install it from Settings → Sound (or audio_control::driver::install), which
# copies it to /Library/Audio/Plug-Ins/HAL and restarts coreaudiod.
#
# Honors CARGO_TARGET_DIR (the bundle goes to $CARGO_TARGET_DIR/release).
#
#   UNIVERSAL=1        build aarch64 + x86_64 and merge them with lipo
#   SIGN_IDENTITY=…    sign with that identity (Developer ID: hardened
#                      runtime + secure timestamp). Otherwise the local
#                      "PowerMac Local Signing" identity if it is in the
#                      keychain (scripts/create-signing-cert.sh), else ad-hoc.
#   CHECK=1            afterwards, load the bundle in-process with the fake
#                      host harness (examples/host_harness.rs)
set -e
cd "$(dirname "$0")/.."
TARGET="${CARGO_TARGET_DIR:-target}"
OUT="$TARGET/release"
LIB=libpm_audio_driver.dylib
NAME="PowerMac Audio"
BUNDLE="$OUT/$NAME.driver"

if [ "$UNIVERSAL" = "1" ]; then
  for T in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --target "$T" -p pm-audio-driver --lib
  done
  mkdir -p "$OUT"
  lipo -create -output "$OUT/$LIB.universal" \
    "$TARGET/aarch64-apple-darwin/release/$LIB" "$TARGET/x86_64-apple-darwin/release/$LIB"
  SRC="$OUT/$LIB.universal"
else
  cargo build --release -p pm-audio-driver --lib
  SRC="$OUT/$LIB"
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$SRC" "$BUNDLE/Contents/MacOS/$NAME"
install_name_tool -id "@loader_path/$NAME" "$BUNDLE/Contents/MacOS/$NAME" 2>/dev/null || true
sed "s/@VERSION@/$VERSION/g" crates/pm-audio-driver/Info.plist.in > "$BUNDLE/Contents/Info.plist"
plutil -lint -s "$BUNDLE/Contents/Info.plist"

if [ -n "$SIGN_IDENTITY" ]; then
  codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$BUNDLE"
  HOW="$SIGN_IDENTITY"
elif security find-identity -v -p codesigning 2>/dev/null | grep -q '"PowerMac Local Signing"'; then
  codesign --force -s "PowerMac Local Signing" "$BUNDLE"
  HOW="PowerMac Local Signing"
else
  codesign --force -s - "$BUNDLE"
  HOW="ad-hoc"
fi
codesign --verify --strict --verbose=1 "$BUNDLE"
echo "built $BUNDLE ($VERSION, signed: $HOW)"

if [ "$CHECK" = "1" ]; then
  cargo run --release -q -p pm-audio-driver --example host_harness -- "$BUNDLE"
fi
