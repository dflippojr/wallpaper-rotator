#!/usr/bin/env bash
# Installs, updates or uninstalls Wallpaper Rotator on macOS or Linux
# (Ubuntu GNOME).
#
#   curl -fsSL https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash
#   wget -qO- https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash
#
# Options go after `bash -s --`, or after the file name when run from a clone:
#   bash install.sh [--dry-run] [--verbose] [--from-source] [--force] [--version vX.Y.Z]
#   bash install.sh --uninstall [--purge]
#
# It checks the machine and what is already installed, installs only the
# missing pieces, then installs the latest release: the universal .dmg on
# Macs, the .deb on Ubuntu/Debian (22.04 or later, x86_64 or arm64), the
# AppImage on other x86_64 Linux. Only when no prebuilt file fits (an older
# glibc, ARM Linux without apt) or with --from-source does it build here.
#
#   --dry-run      only report what it finds and what it would do
#   --verbose      print every check and command instead of a progress bar
#                  (also the default when the output isn't a terminal)
#   --from-source  build here instead of downloading a release
#   --force        reinstall even if this version is installed; continue
#                  on a desktop other than GNOME
#   --version      install a specific release tag instead of the latest
#   --uninstall    remove the app, its start-at-login entry and its cache;
#                  settings stay unless --purge is given too, and the
#                  favorites folder is never touched
#
# Run it again any time to update.

# Everything is inside main(), so when this arrives through a pipe bash reads
# all of it before running anything, and no command can eat the rest of the
# script from stdin.
main() {
set -euo pipefail

REPO=dflippojr/wallpaper-rotator
APP_NAME="Wallpaper Rotator"
# The release's Linux packages are built on Ubuntu 22.04 and need this glibc.
BUILT_GLIBC=2.35

DRY_RUN=0 VERBOSE=0 FROM_SOURCE=0 FORCE=0 UNINSTALL=0 PURGE=0 TAG="" PKG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --verbose) VERBOSE=1 ;;
    --from-source) FROM_SOURCE=1 ;;
    --force) FORCE=1 ;;
    --uninstall) UNINSTALL=1 ;;
    --purge) PURGE=1 ;;
    --version) TAG="${2:?--version needs a tag}"; shift ;;
    -h|--help) echo "options: --dry-run --verbose --from-source --force --version vX.Y.Z --uninstall --purge (details at the top of install.sh)"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done
[ "$PURGE" = 0 ] || [ "$UNINSTALL" = 1 ] || { echo "--purge goes with --uninstall" >&2; exit 2; }
if [ "$UNINSTALL" = 1 ]; then VERB="Uninstalling"; else VERB="Installing"; fi

TMP=$(mktemp -d)
LOG="${TMPDIR:-/tmp}/wallpaper-rotator-install.log"

# On a terminal, show one progress bar and send the details to $LOG. With
# --verbose, --dry-run or no terminal (CI), print every line instead.
PROGRESS=0
if [ "$VERBOSE" = 0 ] && [ "$DRY_RUN" = 0 ] && [ -t 2 ]; then
  PROGRESS=1
  : > "$LOG"
  # fd 3 is the terminal; everything written to stderr goes to the log.
  exec 3>&2 2>>"$LOG"
fi
if [ -t 2 ]; then B=$'\033[1m' G=$'\033[32m' Y=$'\033[33m' R=$'\033[31m' N=$'\033[0m'
else B="" G="" Y="" R="" N=""; fi
case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
  *UTF-8*|*utf8*) FULL="█" EMPTY="░" TICK="✓" CROSS="✗" ;;
  *) FULL="#" EMPTY="-" TICK="ok" CROSS="error:" ;;
esac

on_exit() {
  status=$?
  if [ "$status" != 0 ] && [ "$PROGRESS" = 1 ] && [ ! -e "$TMP/died" ]; then
    printf '\r\033[K%s It stopped. The last lines of its log:\n' "$CROSS" >&3
    tail -n 8 "$LOG" | sed 's/^/  /' >&3
    printf '  Full log: %s\n' "$LOG" >&3
  fi
  rm -rf "$TMP"
}
trap on_exit EXIT

# Messages go to stderr (the log, with a progress bar), so $(...) captures
# only real output.
say()  { printf '%s==>%s %s\n' "$B" "$N" "$*" >&2; }
note() { printf '    %s\n' "$*" >&2; }
ok()   { printf '    %sfound%s    %s\n' "$G" "$N" "$*" >&2; }
miss() { printf '    %smissing%s  %s\n' "$Y" "$N" "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---- the progress bar ----
PCT=0 LABEL=""
clear_bar() { [ "$PROGRESS" = 0 ] || printf '\r\033[K' >&3; }
draw() {
  [ "$PROGRESS" = 1 ] || return 0
  local width=28 filled bar="" i cols room
  filled=$((PCT * width / 100))
  for ((i = 0; i < width; i++)); do
    if [ "$i" -lt "$filled" ]; then bar="$bar$FULL"; else bar="$bar$EMPTY"; fi
  done
  cols=$( (stty size </dev/tty) 2>/dev/null | awk '{print $2}' || true)
  # Some terminals (containers) report a width of 0.
  [ "${cols:-0}" -ge 40 ] 2>/dev/null || cols=80
  room=$((cols - width - 10))
  [ "$room" -gt 10 ] || room=10
  printf '\r\033[K  %s %3d%%  %s' "$bar" "$PCT" "${LABEL:0:$room}" >&3
}
# progress PERCENT LABEL: moves the bar (and logs the step).
progress() { PCT=$1 LABEL=$2; say "$2"; draw; }
# show MESSAGE: a line for the user, on the terminal either way.
show() {
  if [ "$PROGRESS" = 1 ]; then printf '%s\n' "$*" >&3; fi
  printf '%s\n' "$*" >&2
}
# tell MESSAGE: like show, above a progress bar that keeps going.
tell() {
  if [ "$PROGRESS" = 1 ]; then
    clear_bar; printf '%s\n' "$*" >&3; printf '%s\n' "$*" >&2; draw
  else
    printf '%s\n' "$*" >&2
  fi
}
warn() { tell "${Y}warning:${N} $*"; }
die() {
  if [ "$PROGRESS" = 1 ]; then
    clear_bar
    printf '%s %s\n  Full log: %s\n' "$CROSS" "$*" "$LOG" >&3
    printf 'error: %s\n' "$*" >&2
    : > "$TMP/died"
  else
    printf '%serror:%s %s\n' "$R" "$N" "$*" >&2
  fi
  exit 1
}
run() {
  if [ "$DRY_RUN" = 1 ]; then note "would run: $*"
  elif [ "$PROGRESS" = 1 ]; then "$@" </dev/null >>"$LOG"
  else "$@" </dev/null; fi
}
# busy LABEL FROM TO CMD...: runs a slow command while the bar creeps from
# FROM toward TO and the label counts seconds.
busy() {
  local label=$1 from=$2 to=$3 pid start step status=0
  shift 3
  if [ "$PROGRESS" = 0 ] || [ "$DRY_RUN" = 1 ]; then
    [ "$DRY_RUN" = 1 ] || say "$label"
    run "$@"
    return
  fi
  say "$label"
  "$@" </dev/null >>"$LOG" 2>&1 &
  pid=$!
  start=$SECONDS
  PCT=$from
  while kill -0 "$pid" 2>/dev/null; do
    LABEL="$label ($((SECONDS - start))s)"
    draw
    sleep 1
    # A step of an eighth of the gap, at least 1%, stopping short of TO.
    step=$(((to - PCT) / 8))
    [ "$step" -ge 1 ] || step=1
    [ $((PCT + step)) -lt "$to" ] && PCT=$((PCT + step))
  done
  wait "$pid" || status=$?
  PCT=$to
  # The command's own status, so callers (and set -e) see failures.
  return "$status"
}
# sudo asks for a password on the terminal; do that before the bar is drawn
# over it, and again if a long build outlasted sudo's timeout.
sudo_ready() {
  [ "$DRY_RUN" = 0 ] || return 0
  sudo -n true 2>/dev/null && return 0
  clear_bar
  [ "$PROGRESS" = 1 ] && printf 'This uses apt, so sudo needs your password.\n' >&3
  if [ -r /dev/tty ]; then sudo -v </dev/tty; else sudo -v; fi || die "sudo is needed for apt"
  draw
}
# Sorts version numbers; true if $1 >= $2.
# (sed -n 1p rather than head, which can cut the pipe and trip pipefail.)
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | sed -n 1p)" = "$2" ]; }

[ "$PROGRESS" = 0 ] || printf '%s%s %s%s\n' "$B" "$VERB" "$APP_NAME" "$N" >&3

# Where things live (the app's paths come from the `directories` crate and
# Tauri's identifier).
LOCAL_BIN="$HOME/.local/bin"
LOCAL_SHARE="$HOME/.local/share"
VERSION_FILE="$LOCAL_SHARE/wallpaper-rotator/version"
DESKTOP_FILE="$LOCAL_SHARE/applications/wallpaper-rotator.desktop"
ICON_FILE="$LOCAL_SHARE/icons/hicolor/128x128/apps/wallpaper-rotator.png"

# ---------------------------------------------------------------- detect --

OS=$(uname -s) ARCH=$(uname -m)
progress 2 "Checking this machine"
case "$OS" in
  Darwin)
    PLATFORM=macos
    note "macOS $(sw_vers -productVersion) on $ARCH"
    [ "$UNINSTALL" = 1 ] || version_ge "$(sw_vers -productVersion)" 11.0 || die "needs macOS 11 or later"
    ;;
  Linux)
    PLATFORM=linux
    if have apt-get && have dpkg-query; then PKG=deb; else PKG=appimage; fi
    if [ "$UNINSTALL" = 1 ]; then
      : # Nothing below matters for removing it.
    else
      # WSL's own markers. The kernel name alone isn't enough: Docker Desktop
      # containers also run on the WSL kernel.
      if [ -n "${WSL_DISTRO_NAME:-}${WSL_INTEROP:-}" ] || [ -d /mnt/wsl ] \
         || ls /proc/sys/fs/binfmt_misc/WSLInterop* >/dev/null 2>&1; then
        die "this is WSL. Linux here has no desktop of its own, so the app can't set a wallpaper. Install it on Windows instead: see install.ps1 in the README."
      fi
      distro=$( (. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-Linux}") || echo Linux)
      # sed reads all of ldd's output; `head` would cut the pipe and, with
      # pipefail, turn a good answer into a failure.
      glibc=$(ldd --version 2>/dev/null | sed -n '1s/.* \([0-9][0-9]*\.[0-9][0-9]*\)$/\1/p')
      glibc=${glibc:-0}
      note "$distro on $ARCH, glibc $glibc, desktop: ${XDG_CURRENT_DESKTOP:-unknown}"
      if [ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
        warn "no graphical session found (DISPLAY and WAYLAND_DISPLAY are unset). Run this from a terminal on the desktop."
      fi
      case "${XDG_CURRENT_DESKTOP:-}" in
        *GNOME*) ;;
        *)
          [ "$FORCE" = 1 ] || die "the wallpaper code supports GNOME only, and this desktop is '${XDG_CURRENT_DESKTOP:-unknown}'. Rerun with --force to install anyway."
          warn "not GNOME: the app will run, but setting the wallpaper may fail"
          ;;
      esac
      case "$ARCH-$PKG" in
        x86_64-*) ;;
        aarch64-deb) ;;
        *) say "No prebuilt package for $ARCH without apt; building from source"; FROM_SOURCE=1 ;;
      esac
      if [ "$FROM_SOURCE" = 0 ] && ! version_ge "$glibc" "$BUILT_GLIBC"; then
        say "glibc $glibc is older than the release build's $BUILT_GLIBC; building from source"
        FROM_SOURCE=1
      fi
      if [ "$FROM_SOURCE" = 1 ] && [ "$PKG" != deb ]; then
        die "building from source is only scripted for Ubuntu/Debian (apt). See 'Build' in the README."
      fi
    fi
    ;;
  *) die "unsupported system: $OS. On Windows, use install.ps1." ;;
esac

APT_UPDATED=0
apt_update() {
  [ "$APT_UPDATED" = 0 ] || return 0
  sudo_ready
  busy "Updating the package list" "$PCT" $((PCT + 3)) sudo apt-get update
  APT_UPDATED=1
}
apt_install() {
  [ $# -gt 0 ] || return 0
  apt_update
  sudo_ready
  busy "Installing $*" "$PCT" $((PCT + 5)) sudo apt-get install -y "$@"
}
deb_installed() {
  dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q "install ok installed"
}

stop_app() {
  if [ "$PLATFORM" = macos ]; then
    run pkill -f "$APP_NAME.app/Contents/MacOS/" || true
  else
    # Process names are cut to 15 characters, so match the command line.
    run pkill -f "^(/usr/bin/|$LOCAL_BIN/)?wallpaper-rotator( |\$)" || true
    run pkill -f 'wallpaper-rotator\.AppImage' || true
  fi
}

# ------------------------------------------------------------- uninstall --

if [ "$UNINSTALL" = 1 ]; then
  removed=""
  # remove PATH...: deletes what exists, noting it.
  remove() {
    local p
    for p in "$@"; do
      [ -e "$p" ] || continue
      run rm -rf "$p"
      removed="$removed\n    $p"
    done
  }
  progress 10 "Stopping $APP_NAME"
  stop_app
  if [ "$PLATFORM" = macos ]; then
    progress 30 "Removing start at login"
    agent="$HOME/Library/LaunchAgents/$APP_NAME.plist"
    if [ -e "$agent" ]; then
      run launchctl unload "$agent" || true
      remove "$agent"
    fi
    progress 50 "Removing the app"
    remove "/Applications/$APP_NAME.app"
    progress 75 "Removing its cache"
    remove "$HOME/Library/Caches/dev.dflippojr.wallpaper-rotator" \
           "$HOME/Library/Caches/dev.dflippojr.Wallpaper-Rotator" \
           "$HOME/Library/WebKit/dev.dflippojr.wallpaper-rotator"
    settings="$HOME/Library/Application Support/dev.dflippojr.Wallpaper-Rotator"
  else
    progress 20 "Removing start at login"
    remove "$HOME/.config/autostart/$APP_NAME.desktop"
    # GNOME would be left pointing at a deleted file; go back to the default.
    if have gsettings && gsettings get org.gnome.desktop.background picture-uri 2>/dev/null \
         | grep -q "/wallpaperrotator/"; then
      progress 30 "Resetting the desktop background"
      for key in picture-uri picture-uri-dark picture-options; do
        run gsettings reset org.gnome.desktop.background "$key" 2>/dev/null || true
      done
    fi
    progress 45 "Removing the app"
    if [ "$PKG" = deb ] && deb_installed wallpaper-rotator; then
      sudo_ready
      busy "Removing the package" "$PCT" 70 sudo apt-get remove -y wallpaper-rotator
      removed="$removed\n    the wallpaper-rotator package"
    fi
    remove "$LOCAL_BIN/wallpaper-rotator" "$LOCAL_BIN/wallpaper-rotator.AppImage" \
           "$DESKTOP_FILE" "$ICON_FILE" "$LOCAL_SHARE/wallpaper-rotator"
    progress 80 "Removing its cache"
    remove "$HOME/.cache/wallpaperrotator" "$HOME/.cache/dev.dflippojr.wallpaper-rotator" \
           "$LOCAL_SHARE/dev.dflippojr.wallpaper-rotator"
    settings="$HOME/.config/wallpaperrotator $LOCAL_SHARE/wallpaperrotator"
  fi
  if [ "$PURGE" = 1 ]; then
    progress 90 "Removing settings"
    # shellcheck disable=SC2086 # Linux lists two folders.
    if [ "$PLATFORM" = macos ]; then remove "$settings"; else remove $settings; fi
  fi
  if [ "$DRY_RUN" = 1 ]; then say "Dry run: nothing was changed."; exit 0; fi
  progress 100 "Done"
  [ "$PROGRESS" = 0 ] || printf '\n' >&3
  if [ -z "$removed" ]; then
    show "$TICK $APP_NAME wasn't installed; nothing to remove."
  else
    show "$TICK $APP_NAME is uninstalled."
    say "Removed:$(printf '%b' "$removed")"
    if [ "$PURGE" = 0 ]; then
      show "  Your settings stay, in case you reinstall. To remove them too, add --purge."
    fi
  fi
  show "  Your favorites folder is untouched: ~/Pictures/Wallpaper Favorites"
  exit 0
fi

# ---------------------------------------------------------- dependencies --

# Ask for the sudo password now, before the progress bar runs.
if [ "$PLATFORM" = linux ] && [ "$PKG" = deb ]; then sudo_ready; fi

progress 6 "Checking what's already installed"

# HTTPS needs the CA certificates (missing only on stripped-down systems).
if [ "$PLATFORM" = linux ] && [ "$PKG" = deb ]; then
  if deb_installed ca-certificates; then ok "ca-certificates"; else miss "ca-certificates"; apt_install ca-certificates; fi
fi

# Downloads use curl, or wget where curl is missing (stock Ubuntu desktops).
if have curl; then
  ok "curl"
elif have wget; then
  ok "wget"
else
  miss "curl or wget"
  if [ "$PLATFORM" = linux ] && [ "$PKG" = deb ]; then apt_install curl; else die "install curl, then rerun this"; fi
fi

if [ "$FROM_SOURCE" = 1 ]; then
  if [ "$PLATFORM" = macos ]; then
    if xcode-select -p >/dev/null 2>&1; then
      ok "Xcode Command Line Tools"
    else
      miss "Xcode Command Line Tools"
      run xcode-select --install
      [ "$DRY_RUN" = 1 ] || die "finish the Xcode Command Line Tools install, then rerun this"
    fi
  else
    missing=""
    for p in build-essential curl git pkg-config libwebkit2gtk-4.1-dev \
             libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev; do
      if deb_installed "$p"; then ok "$p"; else miss "$p"; missing="$missing $p"; fi
    done
    # shellcheck disable=SC2086
    apt_install $missing
  fi

  [ -x "$HOME/.cargo/bin/cargo" ] && export PATH="$HOME/.cargo/bin:$PATH"
  if have cargo; then
    ok "Rust ($(cargo --version | awk '{print $2}'))"
  else
    miss "Rust"
    if [ "$DRY_RUN" = 1 ]; then
      note "would run: the rustup installer from https://sh.rustup.rs"
    else
      busy "Installing Rust" "$PCT" $((PCT + 6)) \
        sh -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
      export PATH="$HOME/.cargo/bin:$PATH"
    fi
  fi

  # macOS needs Tauri's bundler for the .app; Linux installs the binary
  # directly, so it skips compiling the Tauri build tool (minutes).
  if [ "$PLATFORM" = macos ]; then
    if have cargo && cargo tauri --version 2>/dev/null | grep -q ' 2\.'; then
      ok "Tauri CLI ($(cargo tauri --version | awk '{print $2}'))"
    else
      miss "Tauri CLI 2"
      busy "Installing the Tauri build tool" "$PCT" $((PCT + 10)) \
        cargo install --locked tauri-cli --version "^2"
    fi
  fi
fi

# --------------------------------------------------------------- versions --

# api URL: a GitHub API call. Uses GITHUB_TOKEN when set (CI), which lifts
# the 60-requests-an-hour limit for anonymous calls.
api() {
  if [ -n "${GITHUB_TOKEN:-}" ] && have curl; then
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$1"
  else
    fetch "$1"
  fi
}

# fetch URL [FILE]: prints the URL's body, or saves it to FILE.
fetch() {
  if have curl; then
    if [ $# -gt 1 ]; then curl -fsSL -o "$2" "$1"; else curl -fsSL "$1"; fi
  else
    if [ $# -gt 1 ]; then wget -q -O "$2" "$1"; else wget -q -O - "$1"; fi
  fi
}

installed_version() {
  if [ "$PLATFORM" = macos ]; then
    defaults read "/Applications/$APP_NAME.app/Contents/Info" CFBundleShortVersionString 2>/dev/null || true
  elif [ "$PKG" = deb ] && deb_installed wallpaper-rotator; then
    dpkg-query -W -f='${Version}' wallpaper-rotator 2>/dev/null || true
  elif [ -f "$VERSION_FILE" ]; then
    cat "$VERSION_FILE"
  fi
}
CURRENT=$(installed_version)
RELEASE="$TMP/release.json"
progress 15 "Finding the latest release"
if [ "$FROM_SOURCE" = 0 ] && { have curl || have wget; }; then
  if [ -n "$TAG" ]; then api="https://api.github.com/repos/$REPO/releases/tags/$TAG"
  else api="https://api.github.com/repos/$REPO/releases/latest"; fi
  api "$api" > "$RELEASE" \
    || die "couldn't read the release from GitHub ($api). Check the connection, or rerun with --from-source."
  # GitHub's JSON has one field per line, so sed can pick fields out.
  TAG=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$RELEASE" | sed -n 1p)
  [ -n "$TAG" ] || die "no release found in $REPO. Rerun with --from-source."
fi
if [ "$FROM_SOURCE" = 1 ]; then TARGET="a build from source"; else TARGET=${TAG:-the latest release}; fi
say "Installed: ${CURRENT:-none}.  Installing: $TARGET"
if [ -n "$CURRENT" ] && [ -n "$TAG" ] && [ "v$CURRENT" = "$TAG" ] && [ "$FORCE" = 0 ]; then
  clear_bar
  show "$TICK $APP_NAME $CURRENT is already up to date. Use --force to reinstall."
  exit 0
fi

# ---------------------------------------------------------------- install --

start_app() {
  progress 97 "Starting $APP_NAME"
  if [ "$PLATFORM" = macos ]; then
    run open -a "/Applications/$APP_NAME.app"
  elif [ "$PKG" = deb ] && deb_installed wallpaper-rotator; then
    run sh -c 'nohup wallpaper-rotator >/dev/null 2>&1 &'
  elif [ -x "$LOCAL_BIN/wallpaper-rotator" ]; then
    run sh -c "nohup '$LOCAL_BIN/wallpaper-rotator' >/dev/null 2>&1 &"
  else
    run sh -c "nohup '$LOCAL_BIN/wallpaper-rotator.AppImage' >/dev/null 2>&1 &"
  fi
}

install_macos_app() { # $1 = path to "Wallpaper Rotator.app"
  progress 88 "Copying to Applications"
  stop_app
  run rm -rf "/Applications/$APP_NAME.app"
  run ditto "$1" "/Applications/$APP_NAME.app"
  # Built without an Apple Developer ID; drop the download quarantine flag so
  # Gatekeeper doesn't block the first launch.
  if [ "$DRY_RUN" = 1 ]; then
    note "would run: xattr -dr com.apple.quarantine /Applications/$APP_NAME.app"
  else
    xattr -dr com.apple.quarantine "/Applications/$APP_NAME.app" 2>/dev/null || true
  fi
}

install_deb() { # $1 = path to the .deb
  progress 82 "Checking the libraries the package needs"
  local missing=0 dep
  if [ -f "$1" ]; then
    while read -r dep; do
      [ -n "$dep" ] || continue
      if deb_installed "$dep"; then ok "$dep"; else miss "$dep (apt will install it)"; missing=1; fi
    done < <(dpkg-deb -f "$1" Depends | tr ',' '\n' | sed 's/^ *//; s/ .*//; s/|.*//')
  else
    missing=1
  fi
  stop_app
  sudo_ready
  # A desktop usually has every library already, so skip refreshing the
  # package lists (the slowest part) unless something has to be fetched.
  if [ "$missing" = 1 ]; then
    apt_update
    busy "Installing the package and its libraries" "$PCT" 95 sudo apt-get install -y "$1"
  elif ! busy "Installing the package" "$PCT" 95 sudo apt-get install -y "$1"; then
    apt_update
    busy "Installing the package" "$PCT" 95 sudo apt-get install -y "$1"
  fi
}

desktop_entry() { # $1 = command, $2 = icon name or path
  [ "$DRY_RUN" = 0 ] || { note "would write $DESKTOP_FILE"; return; }
  mkdir -p "$(dirname "$DESKTOP_FILE")"
  cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=$APP_NAME
Comment=Rotates wallpapers from Unsplash and other sources without filling your disk
Exec=$1
Icon=$2
Categories=Utility;
EOF
}

install_appimage() { # $1 = path to the AppImage, $2 = version
  progress 88 "Installing the AppImage"
  if ldconfig -p 2>/dev/null | grep -q 'libfuse\.so\.2'; then
    ok "libfuse2 (AppImages need it)"
  else
    warn "libfuse2 is missing: install your distribution's fuse2 package (Fedora: fuse-libs) so the AppImage can start"
  fi
  stop_app
  run mkdir -p "$LOCAL_BIN" "$(dirname "$VERSION_FILE")"
  run install -m 755 "$1" "$LOCAL_BIN/wallpaper-rotator.AppImage"
  [ "$DRY_RUN" = 1 ] || printf '%s\n' "$2" > "$VERSION_FILE"
  desktop_entry "$LOCAL_BIN/wallpaper-rotator.AppImage" preferences-desktop-wallpaper
}

download() { # $1 = end of the release file's name, such as _amd64.deb
  url=$(sed -n 's/.*"browser_download_url": *"\([^"]*\)".*/\1/p' "$RELEASE" 2>/dev/null \
    | grep -F -- "$1" | sed -n 1p || true)
  progress 20 "Downloading ${TAG:-the latest release} (${url##*/})"
  if [ "$DRY_RUN" = 1 ]; then
    note "would download: ${url:-the release file ending in $1}"
    echo "$TMP/${url##*/}"
    return
  fi
  [ -n "$url" ] || die "release $TAG has no file ending in $1. Rerun with --from-source."
  local dest="$TMP/${url##*/}" size got pid
  if [ "$PROGRESS" = 0 ]; then
    fetch "$url" "$dest"
  else
    # The file's size from the release, which lists "size" just before
    # each "browser_download_url"; the bar fills 20-80% as bytes arrive.
    size=$(awk -v url="$url" '/"size":/ { gsub(/[^0-9]/, ""); last = $0 }
      index($0, url) { print last; exit }' "$RELEASE")
    fetch "$url" "$dest" &
    pid=$!
    while kill -0 "$pid" 2>/dev/null; do
      got=$(wc -c <"$dest" 2>/dev/null | tr -d ' ' || true)
      got=${got:-0}
      if [ "${size:-0}" -gt 0 ]; then
        PCT=$((20 + 60 * got / size))
        LABEL="Downloading $(awk -v g="$got" -v s="$size" 'BEGIN { printf "%.1f of %.1f MB", g / 1048576, s / 1048576 }')"
      fi
      draw
      sleep 0.2
    done
    wait "$pid" || die "the download failed: $url"
    PCT=80
  fi
  echo "$dest"
}

if [ "$FROM_SOURCE" = 0 ]; then
  case "$PLATFORM-$PKG-$ARCH" in
    macos-*)
      dmg=$(download '_universal.dmg')
      if [ "$DRY_RUN" = 1 ]; then
        install_macos_app "/Volumes/(dmg)/$APP_NAME.app"
      else
        mnt="$TMP/mnt"
        mkdir -p "$mnt"
        hdiutil attach -nobrowse -readonly -quiet -mountpoint "$mnt" "$dmg"
        install_macos_app "$mnt/$APP_NAME.app"
        hdiutil detach -quiet "$mnt"
      fi
      ;;
    # Assigned first, so a failed download stops the install (set -e).
    linux-deb-aarch64) deb=$(download '_arm64.deb'); install_deb "$deb" ;;
    linux-deb-*) deb=$(download '_amd64.deb'); install_deb "$deb" ;;
    linux-appimage-*) img=$(download '_amd64.AppImage'); install_appimage "$img" "${TAG#v}" ;;
  esac
else
  # Build in the current clone if run from one, else in ~/wallpaper-rotator.
  src="$HOME/wallpaper-rotator"
  here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
  if [ -n "$here" ] && [ -f "$here/app/tauri.conf.json" ]; then src="$here"; fi
  if [ -d "$src/.git" ]; then
    progress 25 "Updating the source in $src"
    run git -C "$src" pull --ff-only
  else
    progress 25 "Downloading the source"
    run git clone "https://github.com/$REPO.git" "$src"
  fi
  if [ "$PLATFORM" = macos ]; then
    busy "Building, which takes several minutes" 30 85 \
      sh -c "cd '$src/app' && cargo tauri build --bundles app"
    install_macos_app "$src/target/release/bundle/macos/$APP_NAME.app"
  else
    # Plain cargo with the `local` profile (no link-time optimization) is
    # much faster than a release bundle; custom-protocol serves the
    # embedded UI, as `cargo tauri build` would.
    busy "Building, which takes a few minutes" 30 85 \
      cargo build --manifest-path "$src/Cargo.toml" --profile local \
        -p wallpaper-rotator --features tauri/custom-protocol
    progress 88 "Installing to ~/.local"
    stop_app
    if [ "$DRY_RUN" = 1 ]; then
      note "would install $src/target/local/wallpaper-rotator to $LOCAL_BIN"
    else
      mkdir -p "$LOCAL_BIN" "$(dirname "$ICON_FILE")" "$(dirname "$VERSION_FILE")"
      install -m 755 "$src/target/local/wallpaper-rotator" "$LOCAL_BIN/wallpaper-rotator"
      install -m 644 "$src/app/icons/128x128.png" "$ICON_FILE"
      sed -n 's/^  "version": "\(.*\)",$/\1/p' "$src/app/tauri.conf.json" > "$VERSION_FILE"
    fi
    desktop_entry "$LOCAL_BIN/wallpaper-rotator" wallpaper-rotator
  fi
fi

start_app
if [ "$DRY_RUN" = 1 ]; then
  say "Dry run: nothing was changed."
  exit 0
fi
progress 100 "Done"
# Leave the full bar on screen above the summary.
[ "$PROGRESS" = 0 ] || printf '\n' >&3
if [ "$PLATFORM" = linux ]; then
  where="Click its icon in the top bar and choose Show"
else
  where="Click its icon in the menu bar"
fi
show "$TICK $APP_NAME $(installed_version) is installed and running."
show "  $where; on a first install, pick photo sources under the gear."
}

main "$@"
