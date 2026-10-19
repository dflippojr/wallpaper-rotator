# Agent notes for wallpaper-rotator

A Tauri tray app (`app/`) on top of a platform-independent core (`crates/core`) that fetches photos and sets them as the desktop wallpaper on macOS, Windows and GNOME. See the README for building and releasing.

## How to prove a change works

### 1. Run the local gate

One command runs what the CI `test` job runs on your OS:

```sh
scripts/check.sh                                          # Linux, macOS (bash)
powershell -ExecutionPolicy Bypass -File scripts\check.ps1   # Windows (PowerShell 5.1 or 7)
```

Both run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. On Linux, `check.sh` also runs `cargo llvm-cov --workspace` against the coverage floor (`COVERAGE_FLOOR` in `.github/workflows/ci.yml`). It needs `rustup component add llvm-tools-preview` and `cargo install cargo-llvm-cov --locked`.

The floor only goes up. If your change raises coverage, raise `COVERAGE_FLOOR` to the new total minus 1 point. Never lower it, delete tests or add `#[ignore]` to get a PR through. If you must, say so under `### Gate changes` in the PR.

CI's required checks are `gate` and `gate-guard`. `gate` passes only when every other job (`test` on all three OSes, `installers`, `installer-windows`, `gate-guard`) succeeded.

`gate-guard` is the shared guard from dflippojr/.github. It fails a PR that deletes tests, adds `#[ignore]` or retries, or edits workflows or coverage config, unless the owner applied `gate-change-approved`. The required `gate-guard` check comes from `.github/workflows/gate-guard-target.yml`, which runs from `main` under `pull_request_target`, so a PR can't rewrite its own judge. It counts the label only when the owner applied it after the newest push, so a push after an approval needs a new one. Only the owner applies that label; if your PR needs it, say so in the PR body.

The CLI journeys in `crates/core/tests/cli.rs` run the real binary with a fresh
`WALLPAPER_ROTATOR_HOME` per test, three generated favorite PNGs,
`ROTATOR_SIZE=320x200` and `rotate --dry-run`. The home override is shared by
the CLI and tray app: config, state, log, images and default favorites all live
under that root. Dry runs update state and cache but never call the OS wallpaper
setter. Keep every automated CLI rotation in dry-run mode.

### 2. Check the wallpaper on the real OS

Unit tests don't prove a photo ends up on the desktop. The real-OS check (set the wallpaper, then read it back from the OS) is tracked in #14 and doesn't exist yet. Until it does, a change to `crates/core/src/wallpaper/` or to rotation needs a manual run on each affected OS:

```sh
cargo run -p rotator-core --bin wallpaper-rotator-cli -- rotate
```

Then read the wallpaper back from the OS, not from the app. Don't run `rotate` while the tray app is running.

### 3. Reach the Mac

A MacBook Pro is reachable through Orca. Use it for anything that touches macOS:

```sh
orca terminal --environment "MacBook Pro"
git clone https://github.com/dflippojr/wallpaper-rotator.git   # public repo: clone over HTTPS
cd wallpaper-rotator && git checkout <your branch> && scripts/check.sh
```

"No Mac was available" is not a reason to skip a macOS check. If the Mac is unreachable, say so in `### Not verified` with the error you got.

### 4. Fill in the `## Proof` table in the PR

Every PR body has a `## Proof` table (dflippojr/.github#13), with one row per acceptance criterion in the issue:

```markdown
## Proof

| Acceptance criterion | How verified (command, test name, verify skill) | Evidence (output excerpt, artifact link, red-before/green-after) |
| --- | --- | --- |
| Refresh changes only the selected display | `cargo test -p rotator-core refresh_one_display`; manual rotate on the Mac | `test refresh_one_display ... ok`; before/after wallpaper paths |

### Not verified
What you did not check, and why.

### Gate changes
Tests deleted or skipped, or config loosened, with the reason. Empty if none.
```

**Never claim an OS was verified unless you ran it there.** CI passing on `macos-latest` proves it compiles and the unit tests pass. It doesn't prove the wallpaper changed on a Mac.

After you push fix commits to a PR, re-run the automated review on its head: `gh workflow run review.yml -f pr_number=<PR> -f mode=full`.

## Popover and real-app journeys

Install Node.js 22+ and run these from the repository root:

```sh
npm ci
npx playwright install --with-deps chromium webkit
npx playwright test --workers=1
```

The committed tests in ui/tests load the actual UI with a recorded fake Tauri
object that logs every invoke. They run at 340 by 452 in Chromium and WebKit, in
light and dark modes: all settings sections must fit, Refresh must target the
shown display, Save must send the full edited payload, and console/page errors
fail each test. Reports, screenshots and traces go under target/popover-*.
Do not add retries. For the issue #17 mutation proof, temporarily replace the
next_wallpaper index expression in ui/app.js with 0, then run
`npx playwright test --workers=1 --grep "Refresh sends the shown"`. It must fail
when External is shown (expected 1, received 0). Restore app.js and run the full
suite again; keep the failing output with the PR proof.

The app/tests/e2e journey uses tauri-driver and the real debug app, with no mocked
IPC. Quit any running tray app first. It uses a fresh temporary
WALLPAPER_ROTATOR_HOME, copies the three bundled synthetic PNGs into favorites,
and selects only that source with automatic rotation/login disabled. The app
opens its normal popover through the debug-only --show-popover launch argument,
then WebDriver clicks Refresh. This journey actually changes the desktop
wallpaper; use a disposable desktop/session. It checks that both the live app
and CLI status report a different current photo, the preview loads, and the app
log has no errors. It never runs CLI rotate alongside the app.

Build/install prerequisites:

```sh
cargo build --locked -p wallpaper-rotator -p rotator-core --bin wallpaper-rotator --bin wallpaper-rotator-cli -j 4
cargo install tauri-driver --version 2.1.0 --locked -j 4
```

On Windows, MSVC C++ Build Tools and WebView2 must be installed. Run:

```powershell
./app/tests/e2e/install-edge-driver.ps1
$env:NATIVE_WEBDRIVER = (Resolve-Path target/e2e-tools/edge/msedgedriver.exe).Path
npm run test:app
```

WebView2 Runtime 150+ ignores environment overrides in elevated processes,
including GitHub-hosted Windows runners. For debug Windows builds with
TAURI_WEBVIEW_AUTOMATION=true (set by tauri-driver), the app passes the driver's
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS and WEBVIEW2_USER_DATA_FOLDER through
Tauri's window configuration to the WebView2 API before creating the window.
This keeps the driver-selected debugging port and isolated browser profile
working on those runners. Ordinary launches and release builds do not enable
this bridge. See MicrosoftEdge/WebView2Feedback#5645 and tauri-apps/wry#1782.

On Ubuntu 24.04, install the normal Tauri build libraries plus
webkit2gtk-driver, xvfb, dbus-x11 and gsettings-desktop-schemas, then run
`dbus-run-session -- xvfb-run -a npm run test:app`. tauri-driver must be on PATH;
TAURI_DRIVER can instead name its absolute path. E2E_BINARY_DIR can select a
different debug binary directory. Run one journey at a time (ports 4444/4445).

Diagnostics (before/after CLI status, snapshots, screenshot, app/driver logs)
are kept in target/real-app-results and uploaded by CI. The temporary home is
retained so the current wallpaper file stays present. The journey tears down
its own driver/app processes. A successful CLI status comparison is the
explicit fallback allowed by #17 while #14 is open; it does not independently
prove OS read-back. Direct tauri-driver does not support macOS.

CI's popover and real-app jobs are required by gate alongside the existing
jobs. The real-app matrix uses GitHub-hosted ubuntu-24.04 and windows-latest.
