#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/build/Shovel.app"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$ROOT/host/Info.plist")"
DMG="$ROOT/build/Shovel-$VERSION.dmg"
STAGE="$ROOT/build/dmg"

cargo build --release --manifest-path "$ROOT/cli/Cargo.toml"
"$ROOT/scripts/bundle-host.sh"

mkdir -p "$APP/Contents/Helpers"
cp "$ROOT/cli/target/release/shovel" "$APP/Contents/Helpers/shovel"
codesign --force --sign - "$APP/Contents/Helpers/shovel"
codesign --force --sign - --entitlements "$ROOT/host/Shovel.entitlements" "$APP"
codesign --verify --deep --strict "$APP"

rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp -R "$ROOT/skills" "$STAGE/skills"
hdiutil create -volname "Shovel $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"
echo "$DMG"
