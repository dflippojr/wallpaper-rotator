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

## Build

Needs Rust (stable) plus the Tauri prerequisites for your platform:

- **macOS:** Xcode Command Line Tools.
- **Windows:** Visual Studio 2022 Build Tools with the C++ workload; WebView2 (built into Windows 11).
- **Ubuntu:** `sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev build-essential libssl-dev`

```sh
cargo test --workspace          # core tests
cargo run -p wallpaper-rotator  # run the tray app (debug)
cargo build --release -p wallpaper-rotator
```

For an installable bundle (`.app`/`.dmg`, `.msi`, `.deb`), install the Tauri CLI (`cargo install tauri-cli --version "^2"`) and run `cargo tauri build` inside `app/`.

The headless CLI is handy for testing: `cargo run -p rotator-core --bin wallpaper-rotator-cli -- rotate` (also `status`, `cleanup`, `displays`, `paths`). Don't run `rotate` while the tray app is running.

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
