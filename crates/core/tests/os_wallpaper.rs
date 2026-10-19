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
//! can't pass. The original wallpaper is put back afterwards, best effort.
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
use image::{Rgb, RgbImage};
use rotator_core::wallpaper::{self, Display, Layout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Colour {
    Red,
    Blue,
}

impl Colour {
    fn rgb(self) -> Rgb<u8> {
        match self {
            Colour::Red => Rgb([255, 0, 0]),
            Colour::Blue => Rgb([0, 0, 255]),
        }
    }

    /// Whether a pixel reads as this colour. Loose on purpose: display
    /// colour profiles and JPEG transcoding shift the exact values.
    fn matches(self, Rgb([r, g, b]): Rgb<u8>) -> bool {
        let (main, others) = match self {
            Colour::Red => (r, g.max(b)),
            Colour::Blue => (b, r.max(g)),
        };
        main >= 128 && others <= main / 2
    }
}

/// A part of an image, as fractions of its width and height.
#[derive(Debug, Clone, Copy)]
struct Region {
    x: (f64, f64),
    y: (f64, f64),
}

/// Most of the screen, leaving out the menu bar, top bar, taskbar and Dock.
const DESKTOP: Region = Region {
    x: (0.05, 0.95),
    y: (0.10, 0.90),
};
const LEFT_HALF: Region = Region {
    x: (0.05, 0.45),
    y: (0.10, 0.90),
};
const RIGHT_HALF: Region = Region {
    x: (0.55, 0.95),
    y: (0.10, 0.90),
};

/// The share of sampled pixels that must match for a screenshot to pass.
const MIN_SHARE: f64 = 0.6;

/// What a grid of samples across a region of an image looked like.
struct Sample {
    /// The most common colour, averaged within its bucket.
    dominant: Rgb<u8>,
    /// The share of samples that read as the expected colour.
    share: f64,
}

impl Sample {
    fn of(img: &RgbImage, region: Region, want: Colour) -> Sample {
        const STEPS: u32 = 24;
        let (w, h) = img.dimensions();
        let mut buckets: Vec<([u8; 3], u32, [u32; 3])> = Vec::new();
        let mut matching = 0;
        for i in 0..STEPS {
            for j in 0..STEPS {
                let at = |(from, to): (f64, f64), k: u32, size: u32| {
                    let f = from + (to - from) * (f64::from(k) + 0.5) / f64::from(STEPS);
                    ((f * f64::from(size)) as u32).min(size - 1)
                };
                let px = *img.get_pixel(at(region.x, i, w), at(region.y, j, h));
                if want.matches(px) {
                    matching += 1;
                }
                let key = px.0.map(|c| c >> 5);
                let sum = px.0.map(u32::from);
                match buckets.iter_mut().find(|(k, _, _)| *k == key) {
                    Some((_, n, total)) => {
                        *n += 1;
                        for c in 0..3 {
                            total[c] += sum[c];
                        }
                    }
                    None => buckets.push((key, 1, sum)),
                }
            }
        }
        let (_, n, total) = buckets
            .into_iter()
            .max_by_key(|(_, n, _)| *n)
            .expect("a non-empty grid");
        Sample {
            dominant: Rgb(total.map(|c| (c / n) as u8)),
            share: f64::from(matching) / f64::from(STEPS * STEPS),
        }
    }
}

fn rgb(Rgb([r, g, b]): Rgb<u8>) -> String {
    format!("rgb({r}, {g}, {b})")
}

/// Decodes an image file whatever its name says, since Windows' transcoded
/// copy has no extension.
fn decode(file: &Path) -> Result<RgbImage> {
    Ok(image::ImageReader::open(file)
        .with_context(|| format!("could not open {}", file.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("could not decode {}", file.display()))?
        .to_rgb8())
}

/// Checks that an image file the OS names as the wallpaper is `want`.
fn file_is(file: &Path, want: Colour) -> Result<String> {
    let sample = Sample::of(&decode(file)?, DESKTOP, want);
    ensure!(
        sample.share >= MIN_SHARE,
        "{} is {}, not {want:?}",
        file.display(),
        rgb(sample.dominant)
    );
    Ok(rgb(sample.dominant))
}

/// Asks the OS what the desktop shows until it says `want`, for up to
/// 10 seconds: the shell may apply a new wallpaper after the call returns.
fn read_back(layout: &Layout, want: Colour) -> Result<String> {
    let start = Instant::now();
    loop {
        match os::read_back(layout, want) {
            Ok(says) if start.elapsed() < Duration::from_millis(100) => return Ok(says),
            Ok(says) => {
                return Ok(format!(
                    "{says} (after {:.1} s)",
                    start.elapsed().as_secs_f64()
                ))
            }
            Err(e) if start.elapsed() > Duration::from_secs(10) => {
                return Err(e.context(format!("still wrong after 10 s; {}", os::diagnose())));
            }
            Err(_) => sleep(Duration::from_millis(250)),
        }
    }
}

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
    os::restoration_cases(run, &red, &blue)?;
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
    wallpaper::set(&Layout::Same(red.to_path_buf()))?;
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
    ensure!(
        !wallpaper::reconcile(&current, dir)?,
        "reconcile replaced a wallpaper from outside the app's folder"
    );
    read_back(&users, Colour::Red).context("reconcile replaced the user's wallpaper")?;
    Ok(())
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

    let saved = os::save();
    let result = cases(&mut run);
    match saved.and_then(os::restore) {
        Ok(()) => println!("  restored the original wallpaper"),
        Err(e) => println!("  could not restore the original wallpaper: {e:#}"),
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

#[cfg(windows)]
mod os {
    use super::*;
    use std::process::Command;
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        DesktopWallpaper, IDesktopWallpaper, DESKTOP_WALLPAPER_POSITION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };

    fn desktop() -> Result<IDesktopWallpaper> {
        unsafe {
            // Stays initialized for the life of the test process.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            Ok(CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?)
        }
    }

    fn wallpaper_of(desktop: &IDesktopWallpaper, monitor: &str) -> Result<String> {
        unsafe {
            let path = desktop.GetWallpaper(&HSTRING::from(monitor))?;
            let out = path.to_string().unwrap_or_default();
            CoTaskMemFree(Some(path.0 as *const _));
            Ok(out)
        }
    }

    fn system_wallpaper() -> Result<PathBuf> {
        let mut buf = [0u16; 1024];
        unsafe {
            SystemParametersInfoW(
                SPI_GETDESKWALLPAPER,
                buf.len() as u32,
                Some(buf.as_mut_ptr().cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )?;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }

    fn names(path: &Path, fixture: &Path) -> &'static str {
        if path == fixture {
            "the photo"
        } else {
            "a copy"
        }
    }

    pub fn read_back(layout: &Layout, want: Colour) -> Result<String> {
        let desktop = desktop()?;
        let displays = wallpaper::displays().context("no displays")?;
        let mut says = Vec::new();
        for (i, display) in displays.iter().enumerate() {
            let fixture = layout.file_for(&display.id, i).context("no photo")?;
            let shown = PathBuf::from(wallpaper_of(&desktop, &display.id)?);
            let colour = file_is(&shown, want).with_context(|| {
                format!("GetWallpaper({}) names {}", display.name, shown.display())
            })?;
            says.push(format!(
                "GetWallpaper({}) = {} ({}, {colour})",
                display.name,
                shown.display(),
                names(&shown, fixture)
            ));
        }
        if let Layout::Same(fixture) = layout {
            let shown = system_wallpaper()?;
            let colour = file_is(&shown, want)
                .with_context(|| format!("SPI_GETDESKWALLPAPER names {}", shown.display()))?;
            says.push(format!(
                "SPI_GETDESKWALLPAPER = {} ({}, {colour})",
                shown.display(),
                names(&shown, fixture)
            ));
        }
        Ok(says.join("; "))
    }

    /// What else Windows says about the wallpaper, for a failure message.
    pub fn diagnose() -> String {
        let mut out = format!("SPI_GETDESKWALLPAPER = {:?}", system_wallpaper().ok());
        for (key, value) in [
            (r"HKCU\Control Panel\Desktop", "Wallpaper"),
            (
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System",
                "Wallpaper",
            ),
            (
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\ActiveDesktop",
                "NoChangingWallPaper",
            ),
        ] {
            let got = Command::new("reg")
                .args(["query", key, "/v", value])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let got = got.lines().last().unwrap_or("(not set)").trim().to_string();
            out.push_str(&format!("; {key} {value}: {got}"));
        }
        let explorer = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq explorer.exe", "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("explorer.exe"))
            .unwrap_or(false);
        out.push_str(&format!("; explorer.exe running: {explorer}"));
        out
    }

    pub struct Saved {
        position: DESKTOP_WALLPAPER_POSITION,
        monitors: Vec<(String, String)>,
    }

    pub fn save() -> Result<Saved> {
        let desktop = desktop()?;
        let monitors = wallpaper::displays()
            .unwrap_or_default()
            .into_iter()
            .map(|d| Ok((wallpaper_of(&desktop, &d.id)?, d.id)))
            .map(|r: Result<(String, String)>| r.map(|(file, id)| (id, file)))
            .collect::<Result<_>>()?;
        Ok(Saved {
            position: unsafe { desktop.GetPosition()? },
            monitors,
        })
    }

    pub fn restore(saved: Saved) -> Result<()> {
        let desktop = desktop()?;
        unsafe {
            desktop.SetPosition(saved.position)?;
            for (id, file) in saved.monitors.iter().filter(|(_, f)| !f.is_empty()) {
                desktop.SetWallpaper(&HSTRING::from(id.as_str()), &HSTRING::from(file.as_str()))?;
            }
        }
        Ok(())
    }

    /// Round-trip both image and no-image starting states through save/restore.
    /// This stays inside the desktop-changing opt-in target, on the main thread.
    pub fn restoration_cases(run: &mut Run, red: &Path, blue: &Path) -> Result<()> {
        let desktop = desktop()?;
        let displays = wallpaper::displays().context("no displays")?;
        for case in ["restore-image", "restore-solid", "restore-mixed"] {
            if case == "restore-mixed" && displays.len() < 2 {
                println!("  restore-mixed: not run (needs two attached monitors)");
                continue;
            }
            wallpaper::set(&Layout::Same(red.to_path_buf()))?;
            super::read_back(&Layout::Same(red.to_path_buf()), Colour::Red)?;
            for (i, display) in displays.iter().enumerate() {
                if case == "restore-solid" || (case == "restore-mixed" && i % 2 == 0) {
                    unsafe {
                        desktop
                            .SetWallpaper(&HSTRING::from(display.id.as_str()), &HSTRING::new())?;
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
            super::read_back(&Layout::Same(blue.to_path_buf()), Colour::Blue)?;
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

    pub fn screen_size() -> Result<(u32, u32)> {
        let displays = wallpaper::displays().context("no displays")?;
        let d = displays.first().context("no displays")?;
        Ok((d.width, d.height))
    }

    /// Minimizes every window, then grabs the primary screen.
    pub fn screenshot(dest: &Path) -> Result<()> {
        let script = format!(
            "$ErrorActionPreference = 'Stop'
             (New-Object -ComObject Shell.Application).MinimizeAll()
             Start-Sleep -Milliseconds 500
             Add-Type -AssemblyName System.Drawing, System.Windows.Forms
             $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
             $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
             [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
             $bmp.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
            dest.display().to_string().replace('\'', "''")
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    pub fn resolve(path: &Path) -> PathBuf {
        path.to_path_buf()
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSScreen, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSString, NSURL};
    use std::process::Command;

    fn screens() -> Result<Vec<Retained<NSScreen>>> {
        let mtm = MainThreadMarker::new().context("not on the main thread")?;
        Ok(NSScreen::screens(mtm).iter().collect())
    }

    fn shown(workspace: &NSWorkspace, screen: &NSScreen) -> Option<PathBuf> {
        workspace
            .desktopImageURLForScreen(screen)
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()))
    }

    pub fn read_back(layout: &Layout, want: Colour) -> Result<String> {
        let workspace = NSWorkspace::sharedWorkspace();
        let displays = wallpaper::displays().context("no displays")?;
        let mut says = Vec::new();
        for (i, (display, screen)) in displays.iter().zip(screens()?).enumerate() {
            let fixture = layout.file_for(&display.id, i).context("no photo")?;
            let shown = shown(&workspace, &screen).context("macOS reports no wallpaper")?;
            ensure!(
                shown == fixture,
                "desktopImageURL({}) is {}, want {}",
                display.name,
                shown.display(),
                fixture.display()
            );
            let colour = file_is(&shown, want)?;
            says.push(format!(
                "desktopImageURL({}) = {} ({colour})",
                display.name,
                shown.display()
            ));
        }
        Ok(says.join("; "))
    }

    /// Every screen's wallpaper, for a failure message.
    pub fn diagnose() -> String {
        let workspace = NSWorkspace::sharedWorkspace();
        screens()
            .map(|screens| {
                screens
                    .iter()
                    .map(|s| format!("{:?}", shown(&workspace, s)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .map(|all| format!("desktopImageURL for each screen: {all}"))
            .unwrap_or_else(|e| e.to_string())
    }

    type Options = Retained<NSDictionary<NSString, AnyObject>>;

    pub struct Saved(Vec<(Retained<NSScreen>, Retained<NSURL>, Options)>);

    pub fn save() -> Result<Saved> {
        let workspace = NSWorkspace::sharedWorkspace();
        Ok(Saved(
            screens()?
                .into_iter()
                .filter_map(|screen| {
                    let url = workspace.desktopImageURLForScreen(&screen)?;
                    let options = workspace.desktopImageOptionsForScreen(&screen)?;
                    Some((screen, url, options))
                })
                .collect(),
        ))
    }

    pub fn restore(saved: Saved) -> Result<()> {
        let workspace = NSWorkspace::sharedWorkspace();
        for (screen, url, options) in saved.0 {
            #[allow(unused_unsafe)]
            unsafe {
                workspace.setDesktopImageURL_forScreen_options_error(&url, &screen, &options)
            }
            .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
        }
        Ok(())
    }

    pub fn screen_size() -> Result<(u32, u32)> {
        let displays = wallpaper::displays().context("no displays")?;
        let d = displays.first().context("no displays")?;
        Ok((d.width, d.height))
    }

    /// The main display, without the shutter sound.
    pub fn screenshot(dest: &Path) -> Result<()> {
        let output = Command::new("screencapture")
            .args(["-x", "-m", "-t", "png"])
            .arg(dest)
            .output()?;
        ensure!(
            output.status.success() && dest.exists(),
            "screencapture: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    pub fn resolve(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::*;
    use std::process::Command;

    const SCHEMA: &str = "org.gnome.desktop.background";
    const KEYS: [&str; 3] = ["picture-uri", "picture-uri-dark", "picture-options"];

    fn get(key: &str) -> Result<String> {
        let output = Command::new("gsettings")
            .args(["get", SCHEMA, key])
            .output()
            .context("could not run gsettings")?;
        ensure!(
            output.status.success(),
            "gsettings get {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn set(key: &str, value: &str) -> Result<()> {
        let status = Command::new("gsettings")
            .args(["set", SCHEMA, key, value])
            .status()?;
        ensure!(status.success(), "gsettings set {key} failed");
        Ok(())
    }

    /// The URI GNOME should hold for a test photo. Test paths only have
    /// spaces to escape, which keeps this independent of the app's escaping.
    fn uri(file: &Path) -> Result<String> {
        let path = file.to_str().context("non-UTF-8 path")?;
        ensure!(
            path.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" /._-".contains(&b)),
            "unexpected characters in {path}"
        );
        Ok(format!("'file://{}'", path.replace(' ', "%20")))
    }

    pub fn read_back(layout: &Layout, want: Colour) -> Result<String> {
        let (file, option) = match layout {
            Layout::Same(file) => (file, "'zoom'"),
            Layout::Spanned(file) => (file, "'spanned'"),
            Layout::Each(_) => anyhow::bail!("GNOME has no per-display wallpaper"),
        };
        let want_uri = uri(file)?;
        for key in ["picture-uri", "picture-uri-dark"] {
            let got = get(key)?;
            ensure!(got == want_uri, "{key} is {got}, want {want_uri}");
        }
        let got = get("picture-options")?;
        ensure!(got == option, "picture-options is {got}, want {option}");
        let colour = if matches!(layout, Layout::Spanned(_)) {
            let img = decode(file)?;
            let left = Sample::of(&img, LEFT_HALF, Colour::Red);
            let right = Sample::of(&img, RIGHT_HALF, Colour::Blue);
            ensure!(
                left.share >= MIN_SHARE && right.share >= MIN_SHARE,
                "the stitched image is {} | {}",
                rgb(left.dominant),
                rgb(right.dominant)
            );
            format!("{} | {}", rgb(left.dominant), rgb(right.dominant))
        } else {
            file_is(file, want)?
        };
        Ok(format!(
            "picture-uri = picture-uri-dark = {want_uri} ({colour}), picture-options = {got}"
        ))
    }

    /// Every background key, for a failure message.
    pub fn diagnose() -> String {
        KEYS.iter()
            .map(|key| format!("{key} = {}", get(key).unwrap_or_else(|e| e.to_string())))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub struct Saved(Vec<(&'static str, String)>);

    pub fn save() -> Result<Saved> {
        Ok(Saved(
            KEYS.iter()
                .map(|&key| Ok((key, get(key)?)))
                .collect::<Result<_>>()?,
        ))
    }

    pub fn restore(saved: Saved) -> Result<()> {
        for (key, value) in saved.0 {
            set(key, &value)?;
        }
        Ok(())
    }

    /// GNOME lists no displays itself; CI's headless shell has one
    /// 1920x1080 virtual monitor.
    pub fn screen_size() -> Result<(u32, u32)> {
        Ok((1920, 1080))
    }

    /// Through GNOME Shell's own screenshot service on the session bus.
    pub fn screenshot(dest: &Path) -> Result<()> {
        let output = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.gnome.Shell.Screenshot",
                "--object-path",
                "/org/gnome/Shell/Screenshot",
                "--method",
                "org.gnome.Shell.Screenshot.Screenshot",
                "false",
                "false",
            ])
            .arg(dest)
            .output()
            .context("could not run gdbus")?;
        ensure!(
            output.status.success() && dest.exists(),
            "org.gnome.Shell.Screenshot: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    pub fn resolve(path: &Path) -> PathBuf {
        path.to_path_buf()
    }
}
