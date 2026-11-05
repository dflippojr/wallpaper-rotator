use anyhow::{anyhow, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper, DWPOS_FILL};

use super::{Display, Layout};

/// Runs `f` with the desktop wallpaper COM object.
fn with_desktop<T>(f: impl FnOnce(&IDesktopWallpaper) -> windows::core::Result<T>) -> Result<T> {
    unsafe {
        // S_FALSE (already initialized) still needs a matching uninitialize;
        // a different apartment mode is an error we can work with anyway.
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        // The COM object is dropped inside the closure, before uninitializing.
        let result = CoCreateInstance::<_, IDesktopWallpaper>(&DesktopWallpaper, None, CLSCTX_ALL)
            .and_then(|desktop| f(&desktop));
        if initialized {
            CoUninitialize();
        }
        result.map_err(|e| anyhow!("Windows wallpaper call failed: {e}"))
    }
}

/// Attached monitors with their device paths and rectangles. Windows also
/// lists monitors it remembers but that are unplugged; those have no
/// rectangle.
fn monitors(desktop: &IDesktopWallpaper) -> windows::core::Result<Vec<(String, RECT)>> {
    let mut out = Vec::new();
    unsafe {
        for i in 0..desktop.GetMonitorDevicePathCount()? {
            let path = desktop.GetMonitorDevicePathAt(i)?;
            let id = path.to_string().unwrap_or_default();
            CoTaskMemFree(Some(path.0 as *const _));
            if let Ok(rect) = desktop.GetMonitorRECT(&HSTRING::from(id.as_str())) {
                if rect.right > rect.left && rect.bottom > rect.top {
                    out.push((id, rect));
                }
            }
        }
    }
    Ok(out)
}

/// Physical pixels per logical pixel for a monitor's effective DPI
/// (96 DPI is 100 %).
fn scale_for_dpi(dpi: u32) -> f64 {
    if dpi == 0 {
        1.0
    } else {
        f64::from(dpi) / 96.0
    }
}

/// The monitor's scale factor, or 1 if Windows won't say.
fn monitor_scale(rect: &RECT) -> f64 {
    let (mut dpi_x, mut dpi_y) = (0, 0);
    unsafe {
        let monitor = MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_err() {
            return 1.0;
        }
    }
    scale_for_dpi(dpi_x)
}

/// Runs `f` as a per-monitor (v2) DPI aware thread, then puts the thread's
/// awareness back.
fn per_monitor_aware<T>(f: impl FnOnce() -> T) -> T {
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let result = f();
    if !previous.0.is_null() {
        unsafe { SetThreadDpiAwarenessContext(previous) };
    }
    result
}

pub fn displays() -> Result<Vec<Display>> {
    displays_from(|| with_desktop(monitors))
}

/// A process that isn't DPI aware (the CLI) sees scaled monitors at their
/// logical size and every monitor at 96 DPI. `enumerate` and the scale
/// lookups run as a per-monitor aware thread, so sizes are physical pixels
/// and scales are real, as the tray app (aware already) sees them.
fn displays_from(enumerate: impl FnOnce() -> Result<Vec<(String, RECT)>>) -> Result<Vec<Display>> {
    per_monitor_aware(|| {
        Ok(enumerate()?
            .into_iter()
            .enumerate()
            .map(|(i, (id, rect))| Display {
                id,
                name: format!("Display {}", i + 1),
                x: rect.left,
                y: rect.top,
                width: (rect.right - rect.left) as u32,
                height: (rect.bottom - rect.top) as u32,
                scale: monitor_scale(&rect),
            })
            .collect())
    })
}

pub fn set(layout: &Layout) -> Result<()> {
    with_desktop(|desktop| unsafe {
        desktop.SetPosition(DWPOS_FILL)?;
        if let Layout::Same(file) | Layout::Spanned(file) = layout {
            // A null monitor ID means every monitor.
            return desktop.SetWallpaper(PCWSTR::null(), &HSTRING::from(file.as_os_str()));
        }
        for (i, (id, _)) in monitors(desktop)?.iter().enumerate() {
            if let Some(file) = layout.file_for(id, i) {
                desktop.SetWallpaper(
                    &HSTRING::from(id.as_str()),
                    &HSTRING::from(file.as_os_str()),
                )?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use windows::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_UNAWARE,
    };

    fn thread_is_per_monitor_aware() -> bool {
        unsafe {
            AreDpiAwarenessContextsEqual(
                GetThreadDpiAwarenessContext(),
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
            .as_bool()
        }
    }

    /// The test binary has no DPI manifest, so it starts unaware, like the
    /// CLI. Monitors must be listed (and scales read) as an aware thread, and
    /// the thread must be unaware again afterwards.
    #[test]
    fn monitors_are_listed_as_a_per_monitor_aware_thread() {
        assert!(
            !thread_is_per_monitor_aware(),
            "the test thread starts unaware"
        );
        let mut aware_while_listing = false;
        let displays = displays_from(|| {
            aware_while_listing = thread_is_per_monitor_aware();
            let rect = RECT {
                left: -2560,
                top: 0,
                right: 0,
                bottom: 1440,
            };
            Ok(vec![("\\\\?\\DISPLAY#TEST".into(), rect)])
        })
        .unwrap();
        assert!(
            aware_while_listing,
            "listed without per-monitor DPI awareness"
        );
        assert!(
            !thread_is_per_monitor_aware(),
            "the thread's awareness is restored"
        );
        assert_eq!(
            (displays[0].x, displays[0].width, displays[0].height),
            (-2560, 2560, 1440)
        );
        assert!(displays[0].scale >= 1.0);
    }

    /// The real list on this machine: every monitor has a size and a scale,
    /// and asking leaves the thread as it was.
    #[test]
    fn real_monitors_have_a_size_and_scale() {
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE) };
        let displays = displays().expect("Windows lists its monitors");
        assert!(!displays.is_empty(), "at least one monitor is attached");
        for d in &displays {
            assert!(d.width > 0 && d.height > 0 && d.scale >= 1.0, "{d:?}");
        }
        assert!(!thread_is_per_monitor_aware());
    }

    #[test]
    fn scale_follows_the_effective_dpi() {
        assert_eq!(scale_for_dpi(96), 1.0);
        assert_eq!(scale_for_dpi(144), 1.5);
        assert_eq!(scale_for_dpi(192), 2.0);
        assert_eq!(scale_for_dpi(0), 1.0);
    }
}
