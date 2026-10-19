#!/usr/bin/env bash
# Destructive: use only on a fresh disposable machine, never a user's desktop.
# GitHub-hosted runners are accepted automatically. Disposable VMs must opt in:
# SMOKE_DISPOSABLE=1 bash scripts/smoke-installed.sh /path/to/installers
set -euo pipefail
if [ "${RUNNER_ENVIRONMENT:-}" != github-hosted ] && [ "${SMOKE_DISPOSABLE:-}" != 1 ]; then
  echo 'Refusing to install/purge outside a disposable machine (SMOKE_DISPOSABLE=1).' >&2
  exit 2
fi
root=$(cd "$(dirname "$0")/.." && pwd)
assets=$(cd "${1:?pass the directory containing the built installers}" && pwd)
evidence="$root/smoke-out"
mkdir -p "$evidence"
unset WALLPAPER_ROTATOR_HOME UNSPLASH_ACCESS_KEY PEXELS_API_KEY XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME
pid="" tray_pid="" mount="" app="" cli=""
stop_app() {
  if [ -z "$pid" ]; then return; fi
  if [ "$(uname -s)" = Linux ]; then kill -- "-$pid" 2>/dev/null || true
  else kill "$pid" 2>/dev/null || true; fi
  wait "$pid" 2>/dev/null || true
  pid=""
}
cleanup() {
  local status=$?
  stop_app
  if [ -n "$tray_pid" ]; then kill "$tray_pid" 2>/dev/null || true; wait "$tray_pid" 2>/dev/null || true; fi
  if [ -n "$mount" ]; then hdiutil detach "$mount" -quiet || true; fi
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '### Installer smoke (%s)\n\nExit status: %s. See smoke evidence artifact.\n' "$(uname -s)" "$status" >> "$GITHUB_STEP_SUMMARY"
  fi
}
trap cleanup EXIT
one_asset() {
  # shellcheck disable=SC2206 # The argument is an intentional asset glob.
  local matches=("$assets"/$1)
  [ "${#matches[@]}" = 1 ] && [ -f "${matches[0]}" ] || { echo "Expected exactly one $1" >&2; exit 1; }
  printf '%s\n' "${matches[0]}"
}
absent() {
  [ ! -e "$1" ] && [ ! -L "$1" ] || { echo "Unexpected leftover/pre-existing path: $1" >&2; exit 1; }
}
read_paths() {
  "$cli" paths | tee "$evidence/$kind-paths.txt"
  config=$(sed -n 's/^config: *//p' "$evidence/$kind-paths.txt")
  state=$(sed -n 's/^state: *//p' "$evidence/$kind-paths.txt")
  images=$(sed -n 's/^images: *//p' "$evidence/$kind-paths.txt")
  log=$(sed -n 's/^log: *//p' "$evidence/$kind-paths.txt")
  favorites=$(sed -n 's/^favorites: *//p' "$evidence/$kind-paths.txt")
  for path in "$config" "$state" "$images" "$log" "$favorites"; do
    case "$path" in "$HOME"/*) ;; *) echo "Invalid CLI path: $path" >&2; exit 1 ;; esac
    absent "$path"
  done
}
inspect_log() {
  if [ -f "$log" ]; then
    cp "$log" "$evidence/$kind-rotator.log"
  else
    # A fresh no-key startup has nothing to log. The config below proves setup ran.
    printf 'No application log was created on no-key startup.\n' > "$evidence/$kind-log-check.txt"
  fi
  [ -f "$evidence/$kind-output.log" ] || { echo 'Missing launch output capture' >&2; exit 1; }
  if grep -Ei '(^|[[:space:]])(error|fatal|panic|panicked|failed|could not)(:|[[:space:]])' \
      "$evidence/$kind-output.log" "$evidence/$kind-rotator.log" 2>/dev/null; then
    echo 'Application logged an error' >&2; exit 1
  fi
}
exercise() {
  read_paths
  if [ "$(uname -s)" = Darwin ]; then
    open -n -a "$app" --stdout "$evidence/$kind-output.log" --stderr "$evidence/$kind-output.log"
    for _ in {1..10}; do
      pid=$(pgrep -f "^$app/Contents/MacOS/wallpaper-rotator( |$)" || true)
      [ -z "$pid" ] || break
      sleep 1
    done
    [ -n "$pid" ] || { echo 'Installed app did not launch' >&2; exit 1; }
  else
    setsid env APPIMAGE_EXTRACT_AND_RUN=1 "$app" > "$evidence/$kind-output.log" 2>&1 &
    pid=$!
  fi
  sleep 20
  kill -0 "$pid" || { cat "$evidence/$kind-output.log"; echo 'App exited within 20 seconds' >&2; exit 1; }
  printf '%s: installed app alive after 20 seconds (pid %s)\n' "$kind" "$pid" | tee "$evidence/$kind-alive.txt"
  [ -s "$config" ] || { echo 'App never initialized its configuration' >&2; exit 1; }
  "$cli" status | tee "$evidence/$kind-status.txt"
  inspect_log
  stop_app
  bash "$root/install.sh" --uninstall --purge |& tee "$evidence/$kind-uninstall.txt"
  for path in "$config" "$state" "$images" "$log" "$favorites"; do absent "$path"; done
  absent "$app"
  absent "$cli"
  printf '%s: all five CLI paths and installed executables removed\n' "$kind" | tee "$evidence/$kind-purge.txt"
}
case "$(uname -s)" in
  Linux)
    absent /usr/bin/wallpaper-rotator
    absent /usr/bin/wallpaper-rotator-cli
    absent "$HOME/.local/bin/wallpaper-rotator.AppImage"
    absent "$HOME/.local/share/wallpaper-rotator"
    trayer --edge top > "$evidence/tray.log" 2>&1 &
    tray_pid=$!
    kind=deb
    sudo apt-get install -y "$(one_asset '*amd64.deb')"
    app=/usr/bin/wallpaper-rotator
    cli=/usr/bin/wallpaper-rotator-cli
    exercise
    # Test the AppImage independently: the .deb and its CLI are gone.
    kind=appimage
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/wallpaper-rotator"
    app="$HOME/.local/bin/wallpaper-rotator.AppImage"
    cp "$(one_asset '*.AppImage')" "$app"
    chmod +x "$app"
    (cd "$HOME/.local/share/wallpaper-rotator" && "$app" --appimage-extract > "$evidence/appimage-extract.txt")
    cli="$HOME/.local/share/wallpaper-rotator/squashfs-root/usr/bin/wallpaper-rotator-cli"
    exercise
    ;;
  Darwin)
    kind=dmg
    app='/Applications/Wallpaper Rotator.app'
    absent "$app"
    mount=$(mktemp -d)
    hdiutil attach "$(one_asset '*.dmg')" -nobrowse -readonly -mountpoint "$mount"
    ditto "$mount/Wallpaper Rotator.app" "$app"
    hdiutil detach "$mount" -quiet
    rmdir "$mount"
    mount=""
    # Model the quarantine on a downloaded unsigned app, then apply install.sh's handling.
    xattr -w com.apple.quarantine '0083;00000000;InstallerSmoke;' "$app"
    xattr -dr com.apple.quarantine "$app"
    if xattr -r "$app" | grep -q com.apple.quarantine; then echo 'Quarantine remains' >&2; exit 1; fi
    cli="$app/Contents/MacOS/wallpaper-rotator-cli"
    exercise
    ;;
  *) echo 'Only Linux and macOS are supported; Windows uses smoke-installed.ps1' >&2; exit 2 ;;
esac
# A manual dispatch can exercise the release gate without changing an installer or publishing.
if [ "${SMOKE_FORCE_FAILURE:-0}" = 1 ]; then
  echo 'Intentional negative proof: assert a missing uninstall evidence path exists.' >&2
  test -e "$evidence/intentionally-missing-uninstall-proof"
fi
