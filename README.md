# Wallpaper Rotator

A tray app for macOS, Windows and Ubuntu GNOME that sets a new photo as the desktop wallpaper on a schedule, from Unsplash, Pexels, Wallhaven, Bing and Wikimedia Commons, or your own favorites, and **never lets its image cache grow**.

Other rotators save every photo they cycle through. This one keeps only the wallpaper in use and the next one, downloaded ahead so "Next" is instant (one photo each, or one per display). Everything else in its cache folder is deleted after each rotation, and a hard size cap (200 MB by default) backs that up. Photos you want to keep go to a separate favorites folder that cleanup never touches.

## Features

- Rotates hourly, every 6 hours, daily (default), weekly, or only on request.
- Mixes photo sources, and fills in from the others when one is rate-limited or offline (see below).
- One search ("mountains") for Unsplash, Pexels and Wallhaven, plus Unsplash topics (default: `wallpapers`), public collections or one photographer's photos.
- Every photo is cropped to your screen's resolution instead of keeping the multi-MB original.
- Optional different photo on each display, each sized for that display. The popover's refresh button then changes only the display you're viewing; the tray menu's "Next wallpaper" and the schedule change them all.
- Tray popover: preview, location, next, save to favorites, photographer credit, settings.
- Starts at login (release builds).

## Setup

Tick the photo sources you want in Settings (it opens on first run). Wallhaven, Photo of the day and My favorites work without a key. For the others:

- **Unsplash:** create a free app at <https://unsplash.com/developers> and paste its **Access Key**, or set `UNSPLASH_ACCESS_KEY`. Demo keys allow 50 requests an hour; Unsplash raises that to 5,000 for free once you apply for production access on the app's page.
- **Pexels:** get a free key at <https://www.pexels.com/api/> and paste it, or set `PEXELS_API_KEY`. About 200 requests an hour.

In Settings → More sources, paste Unsplash collection URLs or comma-separated IDs, and optionally an `@photographer` or profile URL. Unsplash uses one of these at a time: Search wins over collections, collections over the photographer, and the photographer over topics (Unsplash ignores collections and topics when a photographer is sent). Settings says when a saved photographer is being ignored. Clearing them restores topics.

## Photo sources

| Source | Key | Limits | Notes |
| --- | --- | --- | --- |
| Unsplash | yes | 50/hour (demo) | Server-side crop. Each photo you get is reported to Unsplash, as its terms require. |
| Pexels | yes | about 200/hour | Server-side crop. Random page of a search, or curated photos. |
| Wallhaven | no | 45/minute | Only the general category, safe for work, landscape, at least your screen size. User uploads with unclear copyright: personal use. |
| Photo of the day | no | | Bing's last 8 daily images (4K, licensed stock: personal use) and the last 5 days of Wikimedia Commons' Picture of the Day (free licenses, credited with the license). NASA's APOD feed is left out: in October 2026 it returned a NASA logo for every day. |
| My favorites | no | none | Your `Wallpaper Favorites` folder, in shuffled order. Originals are never moved or changed. |

How the mix works:
- Each photo comes from a randomly picked ticked source.
- Each source's answer is a batch (up to 30 for Unsplash), kept in a queue, so most rotations need no search request. Unsplash's required download report still counts.
- A source that fails rests for 10 minutes, or 20 after a rate limit, and the popover says what filled in, for example "Unsplash is rate-limited, so this one is from Pexels".
- If every ticked source is resting, your favorites fill in, even if they aren't ticked.

## Install

One command per system. The installer checks your machine and which dependencies are already there, installs only what's missing, then installs the latest release. **Run the same command again to update.**

**macOS (one app for Apple silicon and Intel):**
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
| Mac (Apple silicon or Intel) | the universal `.dmg`'s app into `/Applications` |
| Ubuntu 22.04+ / Debian 12+, x86_64 or arm64 | the `.deb` with apt; the package lists are only refreshed if a library is missing |
| Other x86_64 Linux | the AppImage in `~/.local/bin`, plus a menu entry |
| Older Linux, or ARM without apt | a build from source (apt-based only): installs only the missing build packages, builds with `cargo` (no link-time optimization, so it's quicker) and installs into `~/.local/bin` with a menu entry |
| Windows 10/11 x64 (ARM64 runs it emulated) | the setup `.exe`, silently, for the current user; it adds WebView2 if missing |
| WSL | nothing: WSL has no desktop of its own. Install on Windows instead. |

Before installing, the app is stopped if it's running, and it's started again afterwards. Your settings and favorites are kept.

The installer shows one progress bar. The download stage fills as bytes arrive, and slow steps such as apt show a seconds counter. The details go to a log, `wallpaper-rotator-install.log` in your temp folder, and its path is printed if something fails. On Ubuntu it asks for your `sudo` password once, before the bar starts.

Options:
- **macOS and Linux:** end the command with `| bash -s -- --dry-run` instead of `| bash`.
  - `--dry-run` reports what it finds and would do, without changing anything.
  - `--verbose` prints every check and command instead of the progress bar. This is also the default when the output isn't a terminal.
  - `--force` reinstalls the same version, or continues on a desktop other than GNOME.
  - `--from-source` builds locally.
  - `--version v0.2.0` picks a release.
- **Windows:** add `-DryRun` after the last parenthesis. `-Verbose`, `-Force` and `-Version v0.2.0` also work.

From a clone of the repo, run `bash install.sh` or `powershell -ExecutionPolicy Bypass -File install.ps1`.

### Uninstall

The same scripts remove the app, its start-at-login entry and its cache:

- **macOS / Linux:** `curl -fsSL https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.sh | bash -s -- --uninstall` (or `wget -qO-` on Ubuntu).
- **Windows:** `& ([scriptblock]::Create((irm https://raw.githubusercontent.com/dflippojr/wallpaper-rotator/main/install.ps1))) -Uninstall`

What it removes:
- **Your settings and keys** stay, in case you reinstall. Add `--purge` (`-Purge` on Windows) to remove them too.
- **The favorites folder** is never touched.
- **On GNOME**, the desktop background goes back to the default, since the cached photo is deleted.
- **`--dry-run`** shows what would be removed.

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

Before opening a PR, run the same checks CI runs: `scripts/check.sh` (or `scripts\check.ps1` on Windows). [AGENTS.md](AGENTS.md) explains how to prove a change works.

For an installable bundle (`.app`/`.dmg`, `.msi`, `.deb`), install the Tauri CLI (`cargo install tauri-cli --version "^2"`) and run `cargo tauri build` inside `app/`.

The headless CLI is handy for testing: `cargo run -p rotator-core --bin wallpaper-rotator-cli -- rotate` (also `status`, `cleanup`, `displays`, `paths`). Use `rotate --dry-run` to print `would set <layout>` instead of applying the desktop wallpaper; it still fetches photos and updates the state and cache. `ROTATOR_SIZE=320x200` overrides the detected display size. Set `WALLPAPER_ROTATOR_HOME` to a separate root to keep the CLI and tray app's config, state, log, images and default favorites there (`paths` prints all five paths). Don't run `rotate`, including a dry run, while the tray app manages the same root.

### Popover and real-app checks

The committed popover suite runs `npx playwright test --workers=1` after
`npm ci` and `npx playwright install --with-deps chromium webkit` (Node.js 22+).
It checks the actual UI at 340 by 452 with recorded fake Tauri IPC in Chromium
and WebKit, in light and dark modes.

`npm run test:app` drives the real debug tray app through `tauri-driver`,
opens the popover and clicks Refresh using only bundled synthetic favorites in
a temporary home. **It changes the desktop wallpaper**; quit your tray app and
use a disposable session. The journey checks changed live state/CLI status and
error-free app logs. Until the independent OS read-back helper in #14 lands,
CLI status is the allowed fallback and does not prove OS read-back.

Direct `tauri-driver` supports Windows (matching Edge WebDriver) and Linux
(WebKitWebDriver under Xvfb), **not macOS**. Playwright WebKit tests are renderer
checks and do not verify the native Mac app. Both suites are required CI jobs.
See [AGENTS.md](AGENTS.md#popover-and-real-app-journeys) for complete build, driver,
Windows/Linux commands, diagnostic artifacts and the deliberate-break proof.

### Photo sources against the real APIs

The source tests parse replies recorded from the real APIs, in `crates/core/tests/fixtures/sources/`. To record them again:

```sh
cargo run -p rotator-core --bin record-fixtures              # every source you have keys for
cargo run -p rotator-core --bin record-fixtures -- wallhaven # or only some
```

It sends the app's own requests, keeps the first 4 photos of each list, drops fields about the key's account, checks the result with the app's parser, and refuses to record a reply that contains a key. Keys come from `UNSPLASH_ACCESS_KEY` and `PEXELS_API_KEY`, else from the app's settings. Its messages and errors, and the live check's report and issue, have keys and other credentials (`client_id=`, `api_key=`, `Authorization` values) replaced with `[redacted]`. Review the diff before committing. The Pexels fixtures are still hand-written until they are recorded with a key.

The [Live sources](.github/workflows/live-sources.yml) workflow runs nightly and on demand, never on pull requests. For each source it makes one real request through the app's code, checks a photo comes back with a credit and a download URL, then downloads that image and checks it decodes at least 1920 wide and 1080 high. For Unsplash it also reports the download, as the app does. Unsplash and Pexels run only when the `UNSPLASH_ACCESS_KEY` and `PEXELS_API_KEY` repository secrets exist; otherwise the job summary warns "NOT VERIFIED". A failure opens (or updates) one `bug` issue naming the source, the HTTP status and the field that failed. Run it locally with `cargo run -p rotator-core --bin live-sources`.

## Releasing

The installers fetch the files attached to the latest GitHub release. To publish a new version:

1. Bump `version` in `Cargo.toml` and `app/tauri.conf.json`, and commit.
2. Tag and push:
   ```sh
   git tag v0.3.0 && git push origin v0.3.0
   ```
   The **Release** workflow checks that the tag matches the version, then builds and uploads the installers:
   - the `.deb` and AppImage on Ubuntu 22.04, so they run on 22.04 and later
   - an arm64 `.deb`
   - a universal `.dmg`
   - the Windows setup `.exe`

   Then it publishes the release.

To build the installers by hand instead, use `scripts/build-release.sh linux|windows|macos`. Linux and Windows build in Docker (Windows is cross-built, so no MSVC is needed); the macOS build runs on a Mac. Then publish with `gh release create vX.Y.Z dist/*`.

### Branch ruleset

The ruleset for `main` requires a pull request and the status checks `gate` (from `ci.yml`) and `gate-guard`, both from GitHub Actions. `gate-guard` is the check run that `.github/workflows/gate-guard-target.yml` posts from `main` under `pull_request_target`. A pull request can't rewrite it, and it counts the owner's `gate-change-approved` label only when the label was applied after the newest push.

## How it works

| Piece | Where |
| --- | --- |
| Photo sources (Unsplash, Pexels, Wallhaven, Bing + Commons, favorites) and cropping | `crates/core/src/source/` |
| HTTP, downloads, rate-limit detection | `crates/core/src/net.rs` |
| Cache rules: keep current + next, cap, never touch other files | `crates/core/src/store.rs` |
| Rotation worker: schedule, prefetch, source mixing, queues, fallback, favorites | `crates/core/src/service.rs` |
| Wallpaper setters: `IDesktopWallpaper` (Windows), `NSWorkspace` (macOS), `gsettings` (GNOME) | `crates/core/src/wallpaper/` |
| Tray, popover window, commands | `app/src/main.rs`, `ui/` |

Files live in the platform folders (`wallpaper-rotator-cli paths` prints them): config TOML in the config folder, state and a size-capped log in the data folder, images in the cache folder, favorites in `Pictures/Wallpaper Favorites`.

### Platform notes

- **macOS** sets wallpapers per Space. When you switch to a Space still showing an older photo from the cache, the app switches it to the current one.
- **GNOME** has no click events for tray icons, so the tray icon opens a menu (Show, Next, Save to favorites, Pause, Quit). Ubuntu's AppIndicator extension, on by default, shows it. GNOME has a single wallpaper for all monitors, so with a photo per display the app stitches them into one image laid out like your monitors and sets it as "spanned".
- **Per display:** bigger displays pick first, each taking the queued photo that covers it best (asking its source for a new batch when none fits well). When monitors come or go, the current photos are re-placed (a new display borrows one) and the next rotation fetches photos sized for the new layout.
- **Windows** copies the wallpaper into its own cache, so deleting old files is always safe.

### API terms

- **Unsplash:** the app hotlinks Unsplash's preview image, calls each photo's `download_location` when it becomes your wallpaper, and credits the photographer and Unsplash with links.
- **Pexels:** credits the photographer and Pexels with links.
- **Wikimedia Commons:** credits the author and license.
- **Bing and Wallhaven:** these images are for personal use.
