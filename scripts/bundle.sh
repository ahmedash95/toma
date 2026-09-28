#!/usr/bin/env bash
# Build a release Toma.app into dist/.
#
#   scripts/bundle.sh                  # release build + bundle + ad-hoc sign
#   SKIP_BUILD=1 scripts/bundle.sh     # reuse target/release/toma
#   TOMA_SIGN_IDENTITY="..." scripts/bundle.sh
#   TOMA_DIST_DIR=/tmp/out scripts/bundle.sh
#   CARGO_TARGET_DIR=...               # honoured
set -euo pipefail

BUNDLE_ID="dev.toma.app"
EXECUTABLE="Toma"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[[ -n "$VERSION" ]] || { echo "could not read workspace version from Cargo.toml" >&2; exit 1; }

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
DIST_DIR="${TOMA_DIST_DIR:-$ROOT/dist}"
APP="$DIST_DIR/Toma.app"
SIGN_IDENTITY="${TOMA_SIGN_IDENTITY:-}"

if [[ -z "${SKIP_BUILD:-}" ]]; then
  cargo build --release --locked -p toma
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$TARGET_DIR/release/toma" "$APP/Contents/MacOS/$EXECUTABLE"
cp assets/Toma.icns "$APP/Contents/Resources/Toma.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>Toma</string>
  <key>CFBundleExecutable</key>
  <string>${EXECUTABLE}</string>
  <key>CFBundleIconFile</key>
  <string>Toma</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Toma</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

plutil -lint "$APP/Contents/Info.plist" >/dev/null

if [[ -n "$SIGN_IDENTITY" ]]; then
  codesign --force --deep -s "$SIGN_IDENTITY" -i "$BUNDLE_ID" "$APP"
  echo "Signed with identity: $SIGN_IDENTITY"
else
  codesign --force --deep -s - -i "$BUNDLE_ID" "$APP"
  echo "Signed ad-hoc (set TOMA_SIGN_IDENTITY for a stable signature)"
fi
codesign --verify --deep "$APP"

du -sh "$APP"
echo "Built $APP ($BUNDLE_ID v$VERSION)"
