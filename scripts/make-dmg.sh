#!/bin/sh
# Package PowerMac.app into a drag-to-Applications disk image.
#   scripts/make-dmg.sh <path/to/PowerMac.app> <output.dmg>
# With SIGN_IDENTITY set, the image is signed too (notarize it afterwards).
set -e
APP="$1"
DMG="$2"
[ -d "$APP" ] && [ -n "$DMG" ] || { echo "usage: $0 <PowerMac.app> <out.dmg>" >&2; exit 2; }
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname PowerMac -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"
if [ -n "$SIGN_IDENTITY" ]; then
  codesign --force --timestamp -s "$SIGN_IDENTITY" "$DMG"
fi
echo "built $DMG"
