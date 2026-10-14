#!/usr/bin/env bash
# Installs or updates Wallpaper Rotator on macOS or Linux (Ubuntu GNOME).
#
#   curl -fsSL https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash
#   wget -qO- https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash
#
# Options go after `bash -s --`, or after the file name when run from a clone:
#   bash install.sh [--dry-run] [--from-source] [--force] [--version vX.Y.Z]
#
# It checks the machine and what is already installed, installs only the
# missing pieces, then installs the latest release: the .dmg on Apple
# silicon Macs, the .deb on Ubuntu/Debian, the AppImage on other x86_64
# Linux. When no prebuilt file fits (an Intel Mac, ARM Linux, an older
# glibc) or with --from-source, it builds from source instead.
#
#   --dry-run      only report what it finds and what it would do
#   --from-source  build here instead of downloading a release
#   --force        reinstall even if this version is installed; continue
#                  on a desktop other than GNOME
#   --version      install a specific release tag instead of the latest
#
# Run it again any time to update.

# Everything is inside main(), so when this arrives through a pipe bash reads
# all of it before running anything, and no command can eat the rest of the
# script from stdin.
main() {
set -euo pipefail

REPO=dflippojr/wallpaper-rotator
APP_NAME="Wallpaper Rotator"
# The release's .deb and AppImage are built on Ubuntu 24.04 and need this glibc.
BUILT_GLIBC=2.39

DRY_RUN=0 FROM_SOURCE=0 FORCE=0 TAG="" PKG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --from-source) FROM_SOURCE=1 ;;
    --force) FORCE=1 ;;
    --version) TAG="${2:?--version needs a tag}"; shift ;;
    -h|--help) echo "options: --dry-run --from-source --force --version vX.Y.Z (details at the top of install.sh)"; exit 0 ;;
    *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

# Messages go to stderr so $(...) captures only real output.
say()  { printf '\033[1m==>\033[0m %s\n' "$*" >&2; }
note() { printf '    %s\n' "$*" >&2; }
ok()   { printf '    \033[32mfound\033[0m    %s\n' "$*" >&2; }
miss() { printf '    \033[33mmissing\033[0m  %s\n' "$*" >&2; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
run() {
  if [ "$DRY_RUN" = 1 ]; then note "would run: $*"; else "$@" </dev/null; fi
}
# Prompts must read the terminal, not stdin: stdin may be this script.
tty_in() { if [ -r /dev/tty ]; then "$@" </dev/tty; else "$@"; fi; }
# Sorts version numbers; true if $1 >= $2.
# (sed -n 1p rather than head, which can cut the pipe and trip pipefail.)
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | sed -n 1p)" = "$2" ]; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------- detect --

OS=$(uname -s) ARCH=$(uname -m)
say "Checking this machine"
case "$OS" in
  Darwin)
    PLATFORM=macos
    note "macOS $(sw_vers -productVersion) on $ARCH"
    version_ge "$(sw_vers -productVersion)" 11.0 || die "needs macOS 11 or later"
    if [ "$ARCH" != arm64 ]; then
      say "No prebuilt app for Intel Macs; building from source"
      FROM_SOURCE=1
    fi
    ;;
  Linux)
    PLATFORM=linux
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
    if have apt-get && have dpkg-query; then PKG=deb; else PKG=appimage; fi
    if [ "$ARCH" != x86_64 ]; then
      say "No prebuilt package for $ARCH; building from source"
      FROM_SOURCE=1
    elif ! version_ge "$glibc" "$BUILT_GLIBC"; then
      say "glibc $glibc is older than the release build's $BUILT_GLIBC; building from source"
      FROM_SOURCE=1
    fi
    if [ "$FROM_SOURCE" = 1 ] && [ "$PKG" != deb ]; then
      die "building from source is only scripted for Ubuntu/Debian (apt). See 'Build' in the README."
    fi
    ;;
  *) die "unsupported system: $OS. On Windows, use install.ps1." ;;
esac

# ------------------------------------------------------------- dependencies --

APT_UPDATED=0
apt_install() {
  [ $# -gt 0 ] || return 0
  if [ "$APT_UPDATED" = 0 ]; then run sudo apt-get update; APT_UPDATED=1; fi
  run sudo apt-get install -y "$@"
}
deb_installed() {
  dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q "install ok installed"
}

say "Checking what's already installed"

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
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      export PATH="$HOME/.cargo/bin:$PATH"
    fi
  fi

  if have cargo && cargo tauri --version 2>/dev/null | grep -q ' 2\.'; then
    ok "Tauri CLI ($(cargo tauri --version | awk '{print $2}'))"
  else
    miss "Tauri CLI 2"
    run cargo install --locked tauri-cli --version "^2"
  fi
fi

# ---------------------------------------------------------------- versions --

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
  elif [ "$PKG" = deb ]; then
    dpkg-query -W -f='${Version}' wallpaper-rotator 2>/dev/null || true
  else
    [ -f "$HOME/.local/share/wallpaper-rotator/version" ] && cat "$HOME/.local/share/wallpaper-rotator/version" || true
  fi
}
CURRENT=$(installed_version)
RELEASE="$TMP/release.json"
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
  say "Already up to date. Use --force to reinstall."
  exit 0
fi

# ----------------------------------------------------------------- install --

stop_app() {
  if [ "$PLATFORM" = macos ]; then
    run pkill -f "$APP_NAME.app/Contents/MacOS/" || true
  else
    # Process names are cut to 15 characters, so match the command line.
    run pkill -f '^(/usr/bin/)?wallpaper-rotator( |$)' || true
    run pkill -f 'wallpaper-rotator\.AppImage' || true
  fi
}

start_app() {
  say "Starting $APP_NAME"
  if [ "$PLATFORM" = macos ]; then
    run open -a "/Applications/$APP_NAME.app"
  elif [ "$PKG" = deb ]; then
    run sh -c 'nohup wallpaper-rotator >/dev/null 2>&1 &'
  else
    run sh -c "nohup '$HOME/.local/bin/wallpaper-rotator.AppImage' >/dev/null 2>&1 &"
  fi
}

install_macos_app() { # $1 = path to "Wallpaper Rotator.app"
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
  if [ -f "$1" ]; then
    say "Checking the libraries the package needs"
    dpkg-deb -f "$1" Depends | tr ',' '\n' | sed 's/^ *//; s/ .*//; s/|.*//' | while read -r dep; do
      [ -n "$dep" ] || continue
      if deb_installed "$dep"; then ok "$dep"; else miss "$dep (apt will install it)"; fi
    done
  fi
  stop_app
  # apt resolves and installs the missing libraries itself.
  if [ "$APT_UPDATED" = 0 ]; then run sudo apt-get update; APT_UPDATED=1; fi
  run sudo apt-get install -y "$1"
}

install_appimage() { # $1 = path to the AppImage, $2 = version
  if ldconfig -p 2>/dev/null | grep -q 'libfuse\.so\.2'; then
    ok "libfuse2 (AppImages need it)"
  else
    miss "libfuse2: install your distribution's fuse2 package (Fedora: fuse-libs) so the AppImage can start"
  fi
  stop_app
  run mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/wallpaper-rotator"
  run install -m 755 "$1" "$HOME/.local/bin/wallpaper-rotator.AppImage"
  if [ "$DRY_RUN" = 0 ]; then
    printf '%s\n' "$2" > "$HOME/.local/share/wallpaper-rotator/version"
    cat > "$HOME/.local/share/applications/wallpaper-rotator.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$APP_NAME
Comment=Rotates Unsplash wallpapers without filling your disk
Exec=$HOME/.local/bin/wallpaper-rotator.AppImage
Icon=preferences-desktop-wallpaper
Categories=Utility;
EOF
  fi
}

download() { # $1 = end of the release file's name, such as _amd64.deb
  url=$(sed -n 's/.*"browser_download_url": *"\([^"]*\)".*/\1/p' "$RELEASE" 2>/dev/null \
    | grep -F -- "$1" | sed -n 1p || true)
  say "Downloading ${TAG:-the latest release} (${url##*/})"
  if [ "$DRY_RUN" = 1 ]; then
    note "would download: ${url:-the release file ending in $1}"
    echo "$TMP/${url##*/}"
    return
  fi
  [ -n "$url" ] || die "release $TAG has no file ending in $1. Rerun with --from-source."
  fetch "$url" "$TMP/${url##*/}"
  echo "$TMP/${url##*/}"
}

if [ "$FROM_SOURCE" = 0 ]; then
  case "$PLATFORM-${PKG:-}" in
    macos-*)
      dmg=$(download '_aarch64.dmg')
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
    linux-deb) install_deb "$(download '_amd64.deb')" ;;
    linux-appimage) install_appimage "$(download '_amd64.AppImage')" "${TAG#v}" ;;
  esac
else
  # Build in the current clone if run from one, else in ~/wallpaper-rotator.
  src="$HOME/wallpaper-rotator"
  here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
  if [ -n "$here" ] && [ -f "$here/app/tauri.conf.json" ]; then src="$here"; fi
  if [ -d "$src/.git" ]; then
    say "Updating the source in $src"
    run git -C "$src" pull --ff-only
  else
    say "Downloading the source to $src"
    run git clone "https://github.com/$REPO.git" "$src"
  fi
  say "Building (the first build takes several minutes)"
  if [ "$PLATFORM" = macos ]; then
    run sh -c "cd '$src/app' && cargo tauri build --bundles app"
    install_macos_app "$src/target/release/bundle/macos/$APP_NAME.app"
  else
    run sh -c "cd '$src/app' && cargo tauri build --bundles deb"
    deb=$(ls -t "$src"/target/release/bundle/deb/*.deb 2>/dev/null | sed -n 1p || true)
    install_deb "${deb:-$src/target/release/bundle/deb/(built).deb}"
  fi
fi

start_app
if [ "$DRY_RUN" = 1 ]; then
  say "Dry run: nothing was changed."
else
  say "Done. $APP_NAME $(installed_version) is installed."
  if [ "$PLATFORM" = linux ]; then
    note "Its icon is in the top bar: click it and choose Show. On a first install, add your Unsplash key under the gear."
  else
    note "Its icon is in the menu bar. On a first install, click it and add your Unsplash key under the gear."
  fi
fi
}

main "$@"
