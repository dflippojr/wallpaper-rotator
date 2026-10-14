# Wallpaper Rotator

A tray app for macOS, Windows and Ubuntu GNOME that sets a new Unsplash photo as the desktop wallpaper on a schedule, and **never lets its image cache grow**.

Other rotators save every photo they cycle through. This one keeps only the wallpaper in use and the next one, downloaded ahead so "Next" is instant (one photo each, or one per display). Everything else in its cache folder is deleted after each rotation, and a hard size cap (200 MB by default) backs that up. Photos you want to keep go to a separate favorites folder that cleanup never touches.

## Features

- Rotates hourly, every 6 hours, daily (default), weekly, or only on request.
- Photos from an Unsplash search ("mountains") or topics (default: `wallpapers`).
- Downloads each photo cropped to your screen's resolution instead of the multi-MB original.
- Optional different photo on each display, each sized for that display.
- Tray popover: preview, location, next, save to favorites, photographer credit, settings.
- Starts at login (release builds).

## Setup

1. Create a free app at <https://unsplash.com/developers> and copy its **Access Key**. Demo mode allows 50 requests per hour, far more than a rotator uses.
2. Run the app and paste the key in Settings (it opens on first run), or set `UNSPLASH_ACCESS_KEY`.

## Install

One command per system. The installer checks your machine and which dependencies are already there, installs only what's missing, then installs the latest release. **Run the same command again to update.**

**macOS (Apple silicon or Intel):**
```sh
curl -fsSL https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash
```

**Ubuntu GNOME** (stock Ubuntu has `wget`, not `curl`):
```sh
wget -qO- https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash
```

**Windows 10 or 11** (PowerShell):
```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.ps1)))
```

Then add your Unsplash key (see Setup above): click the tray or menu-bar icon (on GNOME choose **Show**), then the gear.

### What the installer does

| Machine | It installs |
| --- | --- |
| Mac with Apple silicon | the `.dmg`'s app into `/Applications` |
| Intel Mac | a build from source (installs Xcode Command Line Tools, Rust and the Tauri CLI if missing) |
| Ubuntu / Debian x86_64 | the `.deb` with apt, which adds any missing libraries |
| Other x86_64 Linux | the AppImage in `~/.local/bin`, plus a menu entry |
| Linux with an older glibc than Ubuntu 24.04, or ARM | a build from source (apt-based only; installs only the missing build packages) |
| Windows 10/11 x64 (ARM64 runs it emulated) | the setup `.exe`, silently, for the current user; it adds WebView2 if missing |
| WSL | nothing: WSL has no desktop of its own. Install on Windows instead. |

Before installing, the app is stopped if it's running, and it's started again afterwards. Your settings and favorites are kept.

Options:
- **macOS and Linux:** end the command with `| bash -s -- --dry-run` instead of `| bash`.
  - `--dry-run` reports what it finds and would do, without changing anything.
  - `--force` reinstalls the same version, or continues on a desktop other than GNOME.
  - `--from-source` builds locally.
  - `--version v0.2.0` picks a release.
- **Windows:** add `-DryRun` after the last parenthesis. `-Force` and `-Version v0.2.0` also work.

From a clone of the repo, run `bash install.sh` or `powershell -ExecutionPolicy Bypass -File install.ps1`.

## Manual install on Ubuntu (GNOME)

This is what the installer automates. Use it if the installer fails. Tested on Ubuntu 24.04; run these in order in a terminal:

1. Check that this is a real GNOME desktop and not WSL. The kernel line must not contain `microsoft`, and the second line should include `GNOME`:
   ```sh
   uname -r; echo "$XDG_CURRENT_DESKTOP"
   ```
2. Install the build tools and libraries:
   ```sh
   sudo apt update
   sudo apt install -y build-essential curl git libwebkit2gtk-4.1-dev \
     libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev
   ```
3. Install Rust. It should end with "Rust is installed now. Great!":
   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   ```
   Don't use `sudo apt install cargo`: Ubuntu's Rust is too old for Tauri 2.
4. Load Rust into this terminal (new terminals do this automatically):
   ```sh
   source "$HOME/.cargo/env"
   ```
5. Check that it worked. If this says `command not found`, step 3 failed; rerun it and read its output:
   ```sh
   cargo --version
   ```
6. Download the code:
   ```sh
   git clone https://github.com/dflippojr/wallpaper-rotator.git ~/wallpaper-rotator
   cd ~/wallpaper-rotator
   ```
7. Install the Tauri build tool. It compiles from source, which takes a few minutes:
   ```sh
   cargo install tauri-cli --version "^2" --locked
   ```
8. Build the installer package:
   ```sh
   (cd app && cargo tauri build --bundles deb)
   ```
9. Install it:
    ```sh
    sudo apt install -y ./target/release/bundle/deb/*.deb
    ```
10. Start it from Activities (search for "Wallpaper Rotator"), or from the terminal:
    ```sh
    wallpaper-rotator &
    ```
11. Click the new icon in the top bar, choose **Show**, open Settings (the gear) and paste your Unsplash access key (see Setup above). The first wallpaper arrives within a few seconds.

To update later:
```sh
cd ~/wallpaper-rotator && git pull
(cd app && cargo tauri build --bundles deb)
sudo apt install -y ./target/release/bundle/deb/*.deb
```
Then quit the app from its menu and start it again.

To uninstall: `sudo apt remove wallpaper-rotator`. Your settings stay in `~/.config`, and favorites in `~/Pictures/Wallpaper Favorites`.

The top-bar icon needs the AppIndicator extension. Ubuntu ships it turned on. On other GNOME distributions, install "AppIndicator and KStatusNotifierItem Support".

## Build

Needs Rust (stable) plus the Tauri prerequisites for your platform:

- **macOS:** Xcode Command Line Tools.
- **Windows:** Visual Studio 2022 Build Tools with the C++ workload; WebView2 (built into Windows 11).
- **Ubuntu:** the packages in step 2 of [Manual install on Ubuntu](#manual-install-on-ubuntu-gnome).

```sh
cargo test --workspace          # core tests
cargo run -p wallpaper-rotator  # run the tray app (debug)
cargo build --release -p wallpaper-rotator
```

For an installable bundle (`.app`/`.dmg`, `.msi`, `.deb`), install the Tauri CLI (`cargo install tauri-cli --version "^2"`) and run `cargo tauri build` inside `app/`.

The headless CLI is handy for testing: `cargo run -p rotator-core --bin wallpaper-rotator-cli -- rotate` (also `status`, `cleanup`, `displays`, `paths`). Don't run `rotate` while the tray app is running.

## Releasing

The installers fetch the files attached to the latest GitHub release. To publish a new version:

1. Bump `version` in `Cargo.toml` and `app/tauri.conf.json`, and commit.
2. Tag and push:
   ```sh
   git tag v0.3.0 && git push origin v0.3.0
   ```
   The **Release** workflow checks that the tag matches the version, builds the `.deb` and AppImage (Ubuntu 24.04), the `.dmg` (Apple silicon) and the Windows setup `.exe`, uploads them, and publishes the release.

To build the installers by hand instead, use `scripts/build-release.sh linux|windows|macos`. Linux and Windows build in Docker (Windows is cross-built, so no MSVC is needed); the macOS build runs on a Mac. Then publish with `gh release create vX.Y.Z dist/*`.

## How it works

| Piece | Where |
| --- | --- |
| Unsplash client (random photo, topic lookup, download tracking, sized downloads) | `crates/core/src/unsplash.rs` |
| Cache rules: keep current + next, cap, never touch other files | `crates/core/src/store.rs` |
| Rotation worker: schedule, prefetch, favorites, retries | `crates/core/src/service.rs` |
| Wallpaper setters: `IDesktopWallpaper` (Windows), `NSWorkspace` (macOS), `gsettings` (GNOME) | `crates/core/src/wallpaper/` |
| Tray, popover window, commands | `app/src/main.rs`, `ui/` |

Files live in the platform folders (`wallpaper-rotator-cli paths` prints them): config TOML in the config folder, state and a size-capped log in the data folder, images in the cache folder, favorites in `Pictures/Wallpaper Favorites`.

### Platform notes

- **macOS** sets wallpapers per Space. When you switch to a Space still showing an older photo from the cache, the app switches it to the current one.
- **GNOME** has no click events for tray icons, so the tray icon opens a menu (Show, Next, Save to favorites, Pause, Quit). Ubuntu's AppIndicator extension, on by default, shows it. GNOME has a single wallpaper for all monitors, so with a photo per display the app stitches them into one image laid out like your monitors and sets it as "spanned".
- **Per display:** one Unsplash request returns a batch of candidates; bigger displays pick first, each taking the photo that covers it best. When monitors come or go, the current photos are re-placed (a new display borrows one) and the next rotation fetches photos sized for the new layout.
- **Windows** copies the wallpaper into its own cache, so deleting old files is always safe.

### Unsplash API terms

The app hotlinks Unsplash's preview image, calls each photo's `download_location` when it becomes your wallpaper, and credits the photographer and Unsplash with links.
