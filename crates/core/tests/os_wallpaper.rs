//! Sets the wallpaper with the app's own setter on this machine, then asks
//! the OS, not the app, what the desktop shows.
//!
//! It changes the desktop, so it only runs with `ROTATOR_OS_TESTS=1`:
//!
//! ```sh
//! ROTATOR_OS_TESTS=1 cargo test -p rotator-core --test os_wallpaper
//! ```
//!
//! Every check sets a pure red photo and reads it back, then a pure blue
//! one, so a setter that does nothing (or a desktop that was already red)
//! can't pass. The original wallpaper is put back afterwards, and the run
//! fails unless the OS then reports exactly what it reported before.
//!
//! The `stalled-call` case changes the desktop and then makes a call stall
//! (a child process that sleeps, standing in for a hung OS call). It must
//! fail in bounded time, and the restoration check above then proves the
//! original wallpaper came back anyway.
//!
//! Time limits (details in `common/os_wallpaper.rs`): helper subprocesses
//! are killed after 30 s; on Windows and GNOME a stuck read-back attempt is
//! abandoned after 30 s. The 10 s and 20 s polling limits only bound how long
//! a wrong answer is retried, between calls, and are not wall-clock bounds.
//! Not bounded at all: macOS AppKit calls (main thread only), and on every OS
//! the app's own setter and the save and restore calls, so a hang there still
//! runs into the CI job timeout.
//!
//! Read-back per OS:
//! - Windows: `IDesktopWallpaper::GetWallpaper` per monitor and
//!   `SPI_GETDESKWALLPAPER`. Windows may hand back its own transcoded copy,
//!   so the file it names is decoded and its pixels compared, not its path.
//! - macOS: `NSWorkspace.desktopImageURL(for:)` per screen equals the photo.
//! - GNOME: `gsettings get` for `picture-uri`, `picture-uri-dark` and
//!   `picture-options`.
//!
//! With `ROTATOR_OS_TESTS_SCREENSHOT=1` it also screenshots the desktop and
//! checks the colour there. That needs a desktop with nothing in front of
//! the wallpaper (the CI runners, with headless GNOME Shell on Linux), so a
//! developer run skips it and says so. `ROTATOR_OS_TESTS_OUT=<dir>` keeps the
//! screenshots, and on GitHub Actions the results go to the job summary.
//!
//! There is no libtest harness (`harness = false` in Cargo.toml): macOS
//! wallpaper calls must run on the main thread, and libtest runs each test
//! on a worker thread.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use image::RgbImage;
use rotator_core::wallpaper::{self, Display, Layout};

#[path = "common/os_wallpaper.rs"]
mod readback;
use readback::{
    decode, os, read_back, read_back_with, rgb, run_bounded, stall_command, stall_if_asked,
    with_restoration, Colour, Region, Sample, Stalled, DESKTOP, LEFT_HALF, MIN_SHARE, RIGHT_HALF,
};

fn fixture(dir: &Path, name: &str, (w, h): (u32, u32), colour: Colour) -> Result<PathBuf> {
    let file = dir.join(name);
    RgbImage::from_pixel(w, h, colour.rgb()).save(&file)?;
    Ok(file)
}

/// One line of the report: a check and what the OS said.
struct Row {
    case: &'static str,
    want: String,
    os_says: String,
    screen: String,
}

struct Run {
    rows: Vec<Row>,
    /// Why the screenshot check is off, if it is.
    no_screenshot: Option<String>,
    out: Option<PathBuf>,
    scratch: PathBuf,
}

impl Run {
    /// Checks the desktop shows each colour in its region, retrying while
    /// the OS fades from the old wallpaper.
    fn screen_shows(&self, name: &str, want: &[(Region, Colour)]) -> Result<String> {
        if self.no_screenshot.is_some() {
            return Ok("not run".into());
        }
        let shot = self.scratch.join(format!("{name}.png"));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let _ = fs::remove_file(&shot);
            os::screenshot(&shot).context("could not screenshot the desktop")?;
            let img = decode(&shot)?;
            let samples: Vec<_> = want
                .iter()
                .map(|&(region, colour)| (colour, Sample::of(&img, region, colour)))
                .collect();
            let report = samples
                .iter()
                .map(|(colour, s)| {
                    format!(
                        "{colour:?}: {} ({:.0}% match)",
                        rgb(s.dominant),
                        s.share * 100.0
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let passed = samples.iter().all(|(_, s)| s.share >= MIN_SHARE);
            if passed || Instant::now() > deadline {
                if let Some(out) = &self.out {
                    let _ = fs::copy(&shot, out.join(format!("{name}.png")));
                }
                let report = format!("{}x{} screenshot, {report}", img.width(), img.height());
                ensure!(passed, "the desktop does not show the photo: {report}");
                return Ok(report);
            }
            sleep(Duration::from_secs(1));
        }
    }

    /// Sets `layout`, then checks the OS and the screen agree it shows
    /// `want`.
    fn set_and_check(
        &mut self,
        case: &'static str,
        layout: &Layout,
        want: Colour,
        screen: &[(Region, Colour)],
    ) -> Result<()> {
        wallpaper::set(layout).context("the app's setter failed")?;
        let os_says = read_back(layout, want).context("the OS read-back failed")?;
        let screen = self.screen_shows(&format!("{case}-{want:?}").to_lowercase(), screen)?;
        println!("  {case} {want:?}: OS says {os_says}; screen: {screen}");
        self.rows.push(Row {
            case,
            want: format!("{want:?}"),
            os_says,
            screen,
        });
        Ok(())
    }

    fn red_then_blue(
        &mut self,
        case: &'static str,
        layout: impl Fn(&Path) -> Layout,
        red: &Path,
        blue: &Path,
    ) -> Result<()> {
        for (colour, file) in [(Colour::Red, red), (Colour::Blue, blue)] {
            self.set_and_check(case, &layout(file), colour, &[(DESKTOP, colour)])?;
        }
        Ok(())
    }
}

fn cases(run: &mut Run) -> Result<()> {
    let dir = run.scratch.join("os wallpaper");
    fs::create_dir_all(&dir)?;
    let size = os::screen_size()?;
    println!("  screen: {}x{}", size.0, size.1);
    let red = fixture(&dir, "red.png", size, Colour::Red)?;
    let blue = fixture(&dir, "blue.png", size, Colour::Blue)?;

    run.red_then_blue("same", |f| Layout::Same(f.to_path_buf()), &red, &blue)?;
    stalled_call(run, &red)?;

    if wallpaper::SPANS_ONE_IMAGE {
        ensure!(
            wallpaper::set(&Layout::Each(Vec::new())).is_err(),
            "GNOME must refuse a photo per display; they need stitching first"
        );
        spanned(run, &dir, size)?;
    } else {
        let displays = wallpaper::displays().context("the OS lists no displays")?;
        ensure!(!displays.is_empty(), "the OS lists no displays");
        let each = |file: &Path| {
            Layout::Each(
                displays
                    .iter()
                    .map(|d| (d.id.clone(), file.to_path_buf()))
                    .collect(),
            )
        };
        run.red_then_blue("each", each, &red, &blue)?;
    }

    if cfg!(target_os = "macos") {
        reconcile(run, &dir, &red, &blue, size)?;
    } else {
        ensure!(
            !wallpaper::reconcile(&Layout::Same(red.clone()), &dir)?,
            "reconcile is a no-op off macOS"
        );
    }
    #[cfg(windows)]
    restoration_cases(run, &red, &blue)?;
    Ok(())
}

/// How long the stalled call is given before it must fail.
const STALL_LIMIT: Duration = Duration::from_secs(3);

/// A call that never comes back, while the desktop shows our photo. The wait
/// must end in `STALL_LIMIT` plus a little, with the stalled child killed;
/// `main` then restores the original and checks the OS agrees.
fn stalled_call(run: &mut Run, red: &Path) -> Result<()> {
    let layout = Layout::Same(red.to_path_buf());
    wallpaper::set(&layout).context("the app's setter failed")?;
    read_back(&layout, Colour::Red).context("the stalled-call case needs our photo up first")?;

    let started = Instant::now();
    let stalled = read_back_with(
        STALL_LIMIT,
        STALL_LIMIT,
        |budget| run_bounded(&mut stall_command(&[], None), budget).map(|_| "returned".into()),
        String::new,
    );
    let took = started.elapsed();
    let err = stalled
        .err()
        .context("the stalled call returned; it was not stalled")?;
    ensure!(
        err.is::<Stalled>(),
        "the stalled call failed another way: {err:#}"
    );
    ensure!(
        took < STALL_LIMIT + Duration::from_secs(10),
        "the stalled call took {took:.1?}, limit {STALL_LIMIT:?}"
    );
    let os_says = format!(
        "gave up after {:.1} s (limit {} s) and killed the stalled child; restored below",
        took.as_secs_f64(),
        STALL_LIMIT.as_secs()
    );
    println!("  stalled-call: {os_says}");
    run.rows.push(Row {
        case: "stalled-call",
        want: "Fails in bounded time".into(),
        os_says,
        screen: "not run".into(),
    });
    Ok(())
}

/// GNOME: two displays side by side, stitched into one image and spanned
/// across the monitor, red on the left and blue on the right.
fn spanned(run: &mut Run, dir: &Path, (w, h): (u32, u32)) -> Result<()> {
    let half = w / 2;
    let display = |id: &str, x: i32, width: u32| Display {
        id: id.into(),
        name: id.into(),
        x,
        y: 0,
        width,
        height: h,
    };
    let parts = vec![
        (
            display("left", 0, half),
            fixture(dir, "left.png", (half, h), Colour::Red)?,
        ),
        (
            display("right", half as i32, w - half),
            fixture(dir, "right.png", (w - half, h), Colour::Blue)?,
        ),
    ];
    let stitched = dir.join("spanned.jpg");
    wallpaper::compose(&parts, &stitched)?;
    let dims = image::image_dimensions(&stitched)?;
    ensure!(
        dims == (w, h),
        "stitched {}x{}, want the summed layout {w}x{h}",
        dims.0,
        dims.1
    );
    let layout = Layout::Spanned(stitched);
    wallpaper::set(&layout).context("the app's setter failed")?;
    let os_says = read_back(&layout, Colour::Red)?;
    let screen = run.screen_shows(
        "spanned",
        &[(LEFT_HALF, Colour::Red), (RIGHT_HALF, Colour::Blue)],
    )?;
    println!("  spanned {w}x{h}: OS says {os_says}; screen: {screen}");
    run.rows.push(Row {
        case: "spanned",
        want: "Red left, Blue right".into(),
        os_says,
        screen,
    });
    Ok(())
}

/// macOS: `reconcile` swaps a stale photo of ours for the current one, and
/// leaves a wallpaper the user picked alone.
fn reconcile(run: &mut Run, dir: &Path, red: &Path, blue: &Path, size: (u32, u32)) -> Result<()> {
    let current = Layout::Same(blue.to_path_buf());
    let stale = Layout::Same(red.to_path_buf());
    wallpaper::set(&stale)?;
    read_back(&stale, Colour::Red).context("reconcile's stale starting wallpaper did not apply")?;
    ensure!(
        wallpaper::reconcile(&current, dir)?,
        "reconcile left our stale red photo up"
    );
    let os_says = read_back(&current, Colour::Blue)?;
    let screen = run.screen_shows("reconcile-blue", &[(DESKTOP, Colour::Blue)])?;
    println!("  reconcile stale red -> Blue: OS says {os_says}; screen: {screen}");
    run.rows.push(Row {
        case: "reconcile",
        want: "Blue (was our stale red)".into(),
        os_says,
        screen,
    });
    ensure!(
        !wallpaper::reconcile(&current, dir)?,
        "reconcile changed a desktop that was already current"
    );

    let mine = run.scratch.join("user");
    fs::create_dir_all(&mine)?;
    let mine = fixture(&mine, "red.png", size, Colour::Red)?;
    let users = Layout::Same(mine);
    wallpaper::set(&users)?;
    read_back(&users, Colour::Red).context("reconcile's user-selected wallpaper did not apply")?;
    ensure!(
        !wallpaper::reconcile(&current, dir)?,
        "reconcile replaced a wallpaper from outside the app's folder"
    );
    read_back(&users, Colour::Red).context("reconcile replaced the user's wallpaper")?;
    Ok(())
}

/// Waits for the OS to report `before` again; the shell may take a moment
/// to apply the restored wallpaper.
fn restored_state(before: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let now = os::fingerprint()?;
        if now == before {
            return Ok(now);
        }
        ensure!(
            Instant::now() < deadline,
            "the original wallpaper is not back: was {before}, now {now}"
        );
        sleep(Duration::from_millis(250));
    }
}

fn summary(run: &Run, result: &Result<()>) -> String {
    let mut md = format!("### OS wallpaper read-back ({})\n\n", env::consts::OS);
    md.push_str("| Case | Want | OS says | Screen |\n| --- | --- | --- | --- |\n");
    for row in &run.rows {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} |",
            row.case,
            row.want,
            row.os_says.replace('|', "\\|"),
            row.screen
        );
    }
    if let Some(why) = &run.no_screenshot {
        let _ = write!(md, "\n**Screenshot check not run:** {why}\n");
    }
    match result {
        Ok(()) => md.push_str("\n**Passed.**\n"),
        Err(e) => {
            let _ = write!(md, "\n**Failed:** {e:#}\n");
        }
    }
    md
}

fn main() -> ExitCode {
    // The stalled-call case re-runs this binary as its stalled child.
    stall_if_asked();
    if env::var("ROTATOR_OS_TESTS").as_deref() != Ok("1") {
        println!(
            "os_wallpaper: skipped. It changes this desktop's wallpaper; \
             set ROTATOR_OS_TESTS=1 to run it."
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "os_wallpaper: setting and reading back the wallpaper on {}",
        env::consts::OS
    );
    if wallpaper::NEEDS_MAIN_THREAD {
        println!("  running on the main thread, as NEEDS_MAIN_THREAD requires");
    }

    let scratch = match tempfile::Builder::new().prefix("rotator-os-").tempdir() {
        Ok(dir) => dir,
        Err(e) => {
            println!("os_wallpaper: FAILED: no temp dir: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = env::var_os("ROTATOR_OS_TESTS_OUT").map(PathBuf::from);
    if let Some(out) = &out {
        let _ = fs::create_dir_all(out);
    }
    let no_screenshot = (env::var("ROTATOR_OS_TESTS_SCREENSHOT").as_deref() != Ok("1")).then(|| {
        "ROTATOR_OS_TESTS_SCREENSHOT is not 1 (needs a desktop with nothing in front of the wallpaper)"
            .to_string()
    });
    if let Some(why) = &no_screenshot {
        println!("  SCREENSHOT CHECK NOT RUN: {why}");
    }
    let mut run = Run {
        rows: Vec::new(),
        no_screenshot,
        out,
        // macOS reports wallpaper paths with symlinks resolved
        // (/private/var/... for /var/...).
        scratch: os::resolve(scratch.path()),
    };

    let mut restored = None;
    let result = with_restoration(
        || Ok((os::fingerprint()?, os::save()?)),
        || cases(&mut run),
        |(before, saved)| {
            os::restore(saved)?;
            let after = restored_state(&before)?;
            println!("  restored the original wallpaper: {after}");
            restored = Some(after);
            Ok(())
        },
    );
    if let Some(after) = restored {
        run.rows.push(Row {
            case: "restored",
            want: "The OS reports the original wallpaper again".into(),
            os_says: after,
            screen: "not run".into(),
        });
    }

    let md = summary(&run, &result);
    if let Some(file) = env::var_os("GITHUB_STEP_SUMMARY") {
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(file) {
            let _ = f.write_all(md.as_bytes());
        }
    }
    match result {
        Ok(()) => {
            println!("os_wallpaper: ok");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("os_wallpaper: FAILED: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Round-trip both image and no-image starting states through save/restore.
/// This stays inside the desktop-changing opt-in target, on the main thread.
#[cfg(windows)]
fn restoration_cases(run: &mut Run, red: &Path, blue: &Path) -> Result<()> {
    use os::{desktop, restore, save, wallpaper_of};
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    use windows::core::HSTRING;
    let desktop = desktop()?;
    let displays = wallpaper::displays().context("no displays")?;
    for case in ["restore-image", "restore-solid", "restore-mixed"] {
        if case == "restore-mixed" && displays.len() < 2 {
            println!("  restore-mixed: not run (needs two attached monitors)");
            continue;
        }
        wallpaper::set(&Layout::Same(red.to_path_buf()))?;
        read_back(&Layout::Same(red.to_path_buf()), Colour::Red)?;
        for (i, display) in displays.iter().enumerate() {
            if case == "restore-solid" || (case == "restore-mixed" && i % 2 == 0) {
                unsafe {
                    desktop.SetWallpaper(&HSTRING::from(display.id.as_str()), &HSTRING::new())?;
                }
                ensure!(
                    wallpaper_of(&desktop, &display.id)?.is_empty(),
                    "{case}: could not establish a no-image starting state"
                );
            }
        }
        let saved = save()?;
        let expected = saved.monitors.clone();
        let position = saved.position;
        let background = unsafe { desktop.GetBackgroundColor()? };
        wallpaper::set(&Layout::Same(blue.to_path_buf()))?;
        read_back(&Layout::Same(blue.to_path_buf()), Colour::Blue)?;
        restore(saved)?;

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let actual = save()?;
            if actual.monitors == expected {
                ensure!(
                    actual.position == position,
                    "{case}: position was not restored"
                );
                ensure!(
                    unsafe { desktop.GetBackgroundColor()? } == background,
                    "{case}: background colour changed"
                );
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "{case}: restoration mismatch: expected {expected:?}, got {:?}",
                actual.monitors
            );
            sleep(Duration::from_millis(250));
        }
        let says = format!(
            "{} monitor(s): image/no-image paths, position and background colour match the saved state",
            expected.len()
        );
        println!("  {case}: {says}");
        run.rows.push(Row {
            case,
            want: "Original background".into(),
            os_says: says,
            screen: "not run".into(),
        });
    }
    Ok(())
}
