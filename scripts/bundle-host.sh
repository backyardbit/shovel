#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/build/Shovel.app"

swift build -c release --package-path "$ROOT/host"
BIN="$(swift build -c release --package-path "$ROOT/host" --show-bin-path)/Shovel"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/Shovel"
cp "$ROOT/host/Info.plist" "$APP/Contents/Info.plist"
codesign --force --sign - --entitlements "$ROOT/host/Shovel.entitlements" "$APP"
echo "$APP"
