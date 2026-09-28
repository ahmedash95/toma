#!/usr/bin/env bash
# Build a distributable DMG from dist/Toma.app (run scripts/bundle.sh first).
#
#   scripts/make_dmg.sh
#   TOMA_DIST_DIR=<dir> scripts/make_dmg.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="${TOMA_DIST_DIR:-$ROOT/dist}"
APP="$DIST/Toma.app"
[[ -d "$APP" ]] || { echo "error: $APP not found (run scripts/bundle.sh first)" >&2; exit 1; }

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
DMG="$DIST/Toma-$VERSION.dmg"
VOLNAME="Toma $VERSION"

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/toma-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

ditto "$APP" "$STAGE/Toma.app"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create -quiet -volname "$VOLNAME" -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG"
hdiutil verify -quiet "$DMG"

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
SIZE="$(du -h "$DMG" | awk '{print $1}')"
echo "built   $DMG ($SIZE)"
echo "version $VERSION"
echo "sha256  $SHA"
