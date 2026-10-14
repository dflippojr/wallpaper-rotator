use std::path::Path;

use anyhow::{anyhow, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper, DWPOS_FILL};

pub fn set(path: &Path) -> Result<()> {
    let wallpaper = HSTRING::from(path.as_os_str());
    unsafe {
        // S_FALSE (already initialized) still needs a matching uninitialize;
        // a different apartment mode is an error we can work with anyway.
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> windows::core::Result<()> {
            let desktop: IDesktopWallpaper = CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?;
            desktop.SetPosition(DWPOS_FILL)?;
            // A null monitor ID means every monitor.
            desktop.SetWallpaper(PCWSTR::null(), &wallpaper)
        })();
        if initialized {
            CoUninitialize();
        }
        result.map_err(|e| anyhow!("Windows would not set the wallpaper: {e}"))
    }
}
