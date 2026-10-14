//! Sets the desktop wallpaper on each platform.
//!
//! - Windows: `IDesktopWallpaper`, all monitors. Windows keeps its own copy,
//!   so deleting the old file is safe.
//! - macOS: `NSWorkspace` for every screen. Must run on the main thread.
//!   macOS only changes the active Space, so [`reconcile`] re-applies the
//!   current photo when another Space still shows an old cached one.
//! - Linux: GNOME through `gsettings`.

use std::path::Path;

use anyhow::Result;

#[cfg(target_os = "linux")]
mod gnome;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// Sets `path` as the wallpaper on every screen.
pub fn set(path: &Path) -> Result<()> {
    #[cfg(windows)]
    return windows::set(path);
    #[cfg(target_os = "macos")]
    return macos::set(path);
    #[cfg(target_os = "linux")]
    return gnome::set(path);
    #[allow(unreachable_code)]
    {
        let _ = path;
        anyhow::bail!("setting the wallpaper is not supported on this platform")
    }
}

/// Whether wallpaper calls must happen on the UI main thread.
pub const NEEDS_MAIN_THREAD: bool = cfg!(target_os = "macos");

/// macOS only: if the active Space shows a photo from `images_dir` other than
/// `current` (an old one cleanup may have deleted), switch it to `current`.
/// Returns whether anything changed. A no-op elsewhere.
pub fn reconcile(current: &Path, images_dir: &Path) -> Result<bool> {
    #[cfg(target_os = "macos")]
    return macos::reconcile(current, images_dir);
    #[allow(unreachable_code)]
    {
        let _ = (current, images_dir);
        Ok(false)
    }
}
