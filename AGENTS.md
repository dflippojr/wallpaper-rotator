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
