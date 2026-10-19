//! Independent OS wallpaper read-back, pixel checks, and background restoration.
//! Shared by the opt-in OS harness and the real-app WebDriver probe.
//!
//! Time limits. Every helper subprocess (gsettings, gdbus, PowerShell,
//! screencapture, reg, tasklist) runs under [`run_bounded`] and is killed
//! after `COMMAND_LIMIT` (`SCREENSHOT_LIMIT` for a screenshot). On Windows and
//! GNOME each read-back attempt runs on a worker thread and is abandoned
//! after `ATTEMPT_LIMIT`, so a stalled synchronous COM call fails the check
//! instead of waiting for the job timeout, and the original wallpaper is
//! still restored. What stays unbounded: macOS AppKit calls (they must stay
//! on the main thread), and on every OS the app's own setter, `save` and
//! `restore`. The 10 s and 20 s polling limits are how long to keep retrying
//! a wrong answer, checked between calls; they are not wall-clock bounds on
//! a call that is stuck.

#[cfg(target_os = "macos")]
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::thread::sleep;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use anyhow::{ensure, Context, Result};
use image::{Rgb, RgbImage};
#[cfg(any(windows, target_os = "macos"))]
use rotator_core::wallpaper;
use rotator_core::wallpaper::Layout;

#[cfg(target_os = "macos")]
#[path = "background_snapshot.rs"]
mod background_snapshot;

#[path = "bounded.rs"]
mod bounded;
#[allow(unused_imports)]
pub use bounded::{
    call_within, read_back_with, run_bounded, stall_command, stall_if_asked, with_restoration,
    Stalled, COMMAND_LIMIT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    // Only the real-app probe requests green; the original harness uses red/blue.
    #[allow(dead_code)]
    Green,
    Red,
    Blue,
}

impl Colour {
    pub fn rgb(self) -> Rgb<u8> {
        match self {
            Colour::Green => Rgb([0, 255, 0]),
            Colour::Red => Rgb([255, 0, 0]),
            Colour::Blue => Rgb([0, 0, 255]),
        }
    }

    /// Whether a pixel reads as this colour. Loose on purpose: display
    /// colour profiles and JPEG transcoding shift the exact values.
    fn matches(self, Rgb([r, g, b]): Rgb<u8>) -> bool {
        let (main, others) = match self {
            Colour::Green => (g, r.max(b)),
            Colour::Red => (r, g.max(b)),
            Colour::Blue => (b, r.max(g)),
        };
        main >= 128 && others <= main / 2
    }
}

/// A part of an image, as fractions of its width and height.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    x: (f64, f64),
    y: (f64, f64),
}

/// Most of the screen, leaving out the menu bar, top bar, taskbar and Dock.
pub const DESKTOP: Region = Region {
    x: (0.05, 0.95),
    y: (0.10, 0.90),
};
pub const LEFT_HALF: Region = Region {
    x: (0.05, 0.45),
    y: (0.10, 0.90),
};
pub const RIGHT_HALF: Region = Region {
    x: (0.55, 0.95),
    y: (0.10, 0.90),
};

/// How long a screenshot may take; a bit more than a plain query, since
/// Windows minimizes every window first.
pub const SCREENSHOT_LIMIT: Duration = Duration::from_secs(45);

/// The share of sampled pixels that must match for a screenshot to pass.
pub const MIN_SHARE: f64 = 0.6;

/// What a grid of samples across a region of an image looked like.
pub struct Sample {
    /// The most common colour, averaged within its bucket.
    pub dominant: Rgb<u8>,
    /// The share of samples that read as the expected colour.
    pub share: f64,
}

impl Sample {
    pub fn of(img: &RgbImage, region: Region, want: Colour) -> Sample {
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

pub fn rgb(Rgb([r, g, b]): Rgb<u8>) -> String {
    format!("rgb({r}, {g}, {b})")
}

/// Decodes an image file whatever its name says, since Windows' transcoded
/// copy has no extension.
pub fn decode(file: &Path) -> Result<RgbImage> {
    Ok(image::ImageReader::open(file)
        .with_context(|| format!("could not open {}", file.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("could not decode {}", file.display()))?
        .to_rgb8())
}

/// Checks that an image file the OS names as the wallpaper is `want`.
pub fn file_is(file: &Path, want: Colour) -> Result<String> {
    let sample = Sample::of(&decode(file)?, DESKTOP, want);
    ensure!(
        sample.share >= MIN_SHARE,
        "{} is {}, not {want:?}",
        file.display(),
        rgb(sample.dominant)
    );
    Ok(rgb(sample.dominant))
}

/// How long to keep asking the OS while it gives a wrong answer: the shell
/// may apply a new wallpaper after the call returns.
pub const READ_BACK_RETRY: Duration = Duration::from_secs(10);

/// How long one read-back attempt may take where it can be abandoned.
pub const ATTEMPT_LIMIT: Duration = Duration::from_secs(30);

/// Asks the OS what the desktop shows until it says `want`.
pub fn read_back(layout: &Layout, want: Colour) -> Result<String> {
    read_back_with(
        READ_BACK_RETRY,
        ATTEMPT_LIMIT,
        |budget| attempt(layout, want, budget),
        os::diagnose,
    )
}

/// AppKit must be called on the main thread, so macOS can't abandon a
/// stuck attempt.
#[cfg(target_os = "macos")]
fn attempt(layout: &Layout, want: Colour, _budget: Duration) -> Result<String> {
    os::read_back(layout, want)
}

#[cfg(not(target_os = "macos"))]
fn attempt(layout: &Layout, want: Colour, budget: Duration) -> Result<String> {
    let layout = layout.clone();
    call_within(budget, move || os::read_back(&layout, want))
}

#[cfg(windows)]
pub mod os {
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

    pub(crate) fn desktop() -> Result<IDesktopWallpaper> {
        unsafe {
            // Stays initialized for the life of the test process.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            Ok(CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?)
        }
    }

    pub(crate) fn wallpaper_of(desktop: &IDesktopWallpaper, monitor: &str) -> Result<String> {
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
        ensure!(!displays.is_empty(), "the OS lists no displays");
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

    /// Everything `restore` puts back, as text, to compare before and after.
    pub fn fingerprint() -> Result<String> {
        Ok(format!("{:?}", save()?))
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
            let got = run_bounded(
                Command::new("reg").args(["query", key, "/v", value]),
                COMMAND_LIMIT,
            )
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
            let got = got.lines().last().unwrap_or("(not set)").trim().to_string();
            out.push_str(&format!("; {key} {value}: {got}"));
        }
        let explorer = run_bounded(
            Command::new("tasklist").args(["/FI", "IMAGENAME eq explorer.exe", "/NH"]),
            COMMAND_LIMIT,
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("explorer.exe"))
        .unwrap_or(false);
        out.push_str(&format!("; explorer.exe running: {explorer}"));
        out
    }

    #[derive(Debug, PartialEq)]
    pub struct Saved {
        pub(crate) position: DESKTOP_WALLPAPER_POSITION,
        pub(crate) monitors: Vec<(String, String)>,
    }

    pub fn save() -> Result<Saved> {
        let desktop = desktop()?;
        let monitors = wallpaper::displays()
            .context("could not save Windows displays")?
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
            // An empty path is a saved no-image/solid-colour background.
            // SetWallpaper with an empty path clears that monitor's test image.
            for (id, file) in &saved.monitors {
                desktop.SetWallpaper(&HSTRING::from(id.as_str()), &HSTRING::from(file.as_str()))?;
            }
        }
        // The shell applies changes asynchronously. Do not claim restoration
        // succeeded while a monitor still names one of our temporary fixtures.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let actual = save()?;
            if actual == saved {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "Windows restoration mismatch: expected {saved:?}, got {actual:?}"
            );
            sleep(Duration::from_millis(250));
        }
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
        let output = run_bounded(
            Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]),
            SCREENSHOT_LIMIT,
        )?;
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
pub mod os {
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
        ensure!(!displays.is_empty(), "the OS lists no displays");
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

    /// The wallpaper of every screen, to compare before and after.
    pub fn fingerprint() -> Result<String> {
        let workspace = NSWorkspace::sharedWorkspace();
        Ok(screens()?
            .iter()
            .map(|s| format!("{:?}", shown(&workspace, s)))
            .collect::<Vec<_>>()
            .join(", "))
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
        Ok(Saved(background_snapshot::capture(screens()?, |screen| {
            (
                workspace.desktopImageURLForScreen(screen),
                workspace.desktopImageOptionsForScreen(screen),
            )
        })?))
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
        let output = run_bounded(
            Command::new("screencapture")
                .args(["-x", "-m", "-t", "png"])
                .arg(dest),
            SCREENSHOT_LIMIT,
        )?;
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
pub mod os {
    use super::*;
    use std::process::Command;

    const SCHEMA: &str = "org.gnome.desktop.background";
    const KEYS: [&str; 3] = ["picture-uri", "picture-uri-dark", "picture-options"];

    fn get(key: &str) -> Result<String> {
        let output = run_bounded(
            Command::new("gsettings").args(["get", SCHEMA, key]),
            COMMAND_LIMIT,
        )?;
        ensure!(
            output.status.success(),
            "gsettings get {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn set(key: &str, value: &str) -> Result<()> {
        let output = run_bounded(
            Command::new("gsettings").args(["set", SCHEMA, key, value]),
            COMMAND_LIMIT,
        )?;
        ensure!(output.status.success(), "gsettings set {key} failed");
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

    /// Every background key, to compare before and after.
    pub fn fingerprint() -> Result<String> {
        Ok(KEYS
            .iter()
            .map(|key| Ok(format!("{key} = {}", get(key)?)))
            .collect::<Result<Vec<_>>>()?
            .join(", "))
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
        let output = run_bounded(
            Command::new("gdbus")
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
                .arg(dest),
            SCREENSHOT_LIMIT,
        )?;
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
