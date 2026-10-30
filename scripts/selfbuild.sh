#!/bin/bash
set -euo pipefail

usage() {
  echo "usage: scripts/selfbuild.sh [--vm NAME] [--ref GIT_REF] [--keep]"
  echo "Builds and tests the committed Shovel sources inside a clean Shovel VM."
}

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VM="shovel-selfbuild"
REF="HEAD"
KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --vm) VM="$2"; shift 2 ;;
    --ref) REF="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

SHOVEL="$(command -v shovel || echo "$ROOT/cli/target/release/shovel")"
[ -x "$SHOVEL" ] || { echo "shovel not found; run 'make cli' first"; exit 1; }
COMMIT="$(git -C "$ROOT" rev-parse "$REF")"

cleanup() {
  if [ "$KEEP" = 0 ]; then "$SHOVEL" down "$VM" --rm >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

echo "==> Booting $VM with $ROOT mounted read-only"
"$SHOVEL" up "$VM" --share "$ROOT" --share-mode ro >/dev/null

echo "==> Building $COMMIT inside $VM"
"$SHOVEL" exec "$VM" -- "
set -euo pipefail
rm -rf /tmp/shovel && git clone -q ~/workspace /tmp/shovel && cd /tmp/shovel && git checkout -q $COMMIT
if ! command -v cargo >/dev/null && [ ! -x ~/.cargo/bin/cargo ]; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1
fi
source ~/.cargo/env
echo '--> cargo build'
cargo build --release --quiet --manifest-path cli/Cargo.toml
echo '--> cargo test'
cargo test --release --quiet --manifest-path cli/Cargo.toml 2>&1 | grep 'test result'
echo '--> Shovel.app'
./scripts/bundle-host.sh >/dev/null 2>&1
codesign -d --entitlements - build/Shovel.app 2>&1 | grep -q com.apple.security.virtualization
echo '--> video encoder smoke test'
head -c \$((64 * 64 * 4 * 20)) /dev/urandom | build/Shovel.app/Contents/MacOS/Shovel --encode /tmp/smoke.mp4 --width 64 --height 64 --fps 10 --gif /tmp/smoke.gif
./cli/target/release/shovel --version
"
echo "==> Self-build passed for $COMMIT"
