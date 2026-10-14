#!/usr/bin/env bash
# Builds release installers into dist/.
#
#   scripts/build-release.sh linux     .deb and .AppImage (in Docker)
#   scripts/build-release.sh windows   NSIS setup .exe, cross-built (in Docker)
#   scripts/build-release.sh macos     universal .dmg (run on a Mac)
#
# Then publish with: gh release create vX.Y.Z dist/*
set -euo pipefail

cd "$(dirname "$0")/.."
root=$(pwd)
mkdir -p dist

in_docker() {
  docker build -t wallpaper-rotator-build -f scripts/Dockerfile.build scripts
  # Build from a copy so the host's target/ and line endings don't matter.
  MSYS_NO_PATHCONV=1 docker run --rm \
    -v "$root:/repo:ro" -v "$root/dist:/dist" \
    -v wallpaper-rotator-registry:/root/.cargo/registry \
    -v wallpaper-rotator-target:/target -e CARGO_TARGET_DIR=/target \
    wallpaper-rotator-build bash -euo pipefail -c "
      rm -rf /src/* && cp -r /repo/. /src && cd /src/app
      # Bundles from earlier builds share the cached target folder.
      rm -rf /target/release/bundle /target/*/release/bundle
      $1
    "
}

case "${1:-}" in
  linux)
    in_docker '
      cargo tauri build --bundles deb,appimage
      cp /target/release/bundle/deb/*.deb /target/release/bundle/appimage/*.AppImage /dist/
    '
    ;;
  windows)
    in_docker '
      cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
      cp /target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe /dist/
    '
    ;;
  macos)
    [ "$(uname -s)" = Darwin ] || { echo "macOS builds must run on a Mac" >&2; exit 1; }
    # CI=true skips the Finder AppleScript that lays out the disk image
    # window, which would otherwise ask for automation permission.
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    (cd app && CI=true cargo tauri build --bundles dmg --target universal-apple-darwin)
    cp target/universal-apple-darwin/release/bundle/dmg/*.dmg dist/
    ;;
  *)
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac

# Spaces in Tauri's file names make install commands awkward; use dashes.
for f in dist/*\ *; do
  [ -e "$f" ] && mv "$f" "${f// /-}"
done
ls -l dist
