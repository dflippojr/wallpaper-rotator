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

pub fn displays() -> Result<Vec<Display>> {
    // A process that isn't DPI aware (the CLI) sees scaled monitors at
    // their logical size and every monitor at 96 DPI. Ask as a per-monitor
    // aware thread so sizes are physical pixels and scales are real, as the
    // tray app (aware already) sees them.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let displays = with_desktop(monitors).map(|monitors| {
        monitors
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
            .collect()
    });
    if !previous.0.is_null() {
        unsafe { SetThreadDpiAwarenessContext(previous) };
    }
    displays
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

    #[test]
    fn scale_follows_the_effective_dpi() {
        assert_eq!(scale_for_dpi(96), 1.0);
        assert_eq!(scale_for_dpi(144), 1.5);
        assert_eq!(scale_for_dpi(192), 2.0);
        assert_eq!(scale_for_dpi(0), 1.0);
    }
}
