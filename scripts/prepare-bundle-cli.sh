#!/usr/bin/env bash
# Prepare the CLI sidecar before building with app/tauri.bundle.conf.json.
# Usage: scripts/prepare-bundle-cli.sh [target-triple] [xwin]
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:-$(rustc -vV | sed -n 's/^host: //p')}
mkdir -p app/binaries
build() {
  local triple=$1
  local cargo=(cargo)
  if [ "${2:-}" = xwin ]; then cargo+=(xwin); fi
  "${cargo[@]}" build --locked --release -p rotator-core --bin wallpaper-rotator-cli --target "$triple" -j 4
}
if [ "$target" = universal-apple-darwin ]; then
  build aarch64-apple-darwin
  build x86_64-apple-darwin
  lipo -create "${CARGO_TARGET_DIR:-target}/aarch64-apple-darwin/release/wallpaper-rotator-cli" \
    "${CARGO_TARGET_DIR:-target}/x86_64-apple-darwin/release/wallpaper-rotator-cli" \
    -output "app/binaries/wallpaper-rotator-cli-$target"
else
  build "$target" "${2:-}"
  suffix=""
  case "$target" in *windows*) suffix=.exe ;; esac
  cp "${CARGO_TARGET_DIR:-target}/$target/release/wallpaper-rotator-cli$suffix" \
    "app/binaries/wallpaper-rotator-cli-$target$suffix"
fi
