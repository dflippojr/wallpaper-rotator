//! Sets the desktop wallpaper on each platform.
//!
//! - Windows: `IDesktopWallpaper`, one photo per monitor or one for all.
//!   Windows keeps its own copy, so deleting the old file is safe.
//! - macOS: `NSWorkspace` per screen. Must run on the main thread. macOS
//!   only changes the active Space, so [`reconcile`] re-applies the current
//!   photos when another Space still shows an old cached one.
//! - Linux: GNOME through `gsettings`. GNOME has one wallpaper for all
//!   monitors, so different photos per display are stitched into one image
//!   ([`compose`]) and shown with the `spanned` option.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod gnome;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod span;
#[cfg(windows)]
mod windows;

/// A screen, as far as choosing and placing photos goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    /// Identifies the display while it stays connected.
    pub id: String,
    /// Shown in the popover, such as "Built-in Retina Display".
    pub name: String,
    /// Position in the layout, in the same pixels as `width` and `height`.
    /// Only GNOME uses it, to stitch photos into one spanned image.
    pub x: i32,
    pub y: i32,
    /// The photo size to download for this display.
    pub width: u32,
    pub height: u32,
}

/// What to show where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One photo on every display.
    Same(PathBuf),
    /// A photo per display, keyed by display ID.
    Each(Vec<(String, PathBuf)>),
    /// GNOME: one image stitched from per-display photos, stretched across
    /// all monitors.
    Spanned(PathBuf),
}

impl Layout {
    /// The photo a display should show: its own, or else one of the others
    /// in turn (for a display connected after the photos were chosen).
    pub fn file_for(&self, display: &str, index: usize) -> Option<&Path> {
        match self {
            Layout::Same(file) | Layout::Spanned(file) => Some(file),
            Layout::Each(files) => files
                .iter()
                .find(|(id, _)| id == display)
                .or_else(|| (!files.is_empty()).then(|| &files[index % files.len()]))
                .map(|(_, file)| file.as_path()),
        }
    }
}

/// The connected displays, where the platform can list them itself. GNOME
/// returns `None`; the app reads monitors from the windowing toolkit there.
pub fn displays() -> Option<Vec<Display>> {
    #[cfg(windows)]
    return windows::displays().ok();
    #[cfg(target_os = "macos")]
    return macos::displays().ok();
    #[allow(unreachable_code)]
    None
}

pub fn set(layout: &Layout) -> Result<()> {
    #[cfg(windows)]
    return windows::set(layout);
    #[cfg(target_os = "macos")]
    return macos::set(layout);
    #[cfg(target_os = "linux")]
    return gnome::set(layout);
    #[allow(unreachable_code)]
    {
        let _ = layout;
        anyhow::bail!("setting the wallpaper is not supported on this platform")
    }
}

/// Whether wallpaper calls must happen on the UI main thread.
pub const NEEDS_MAIN_THREAD: bool = cfg!(target_os = "macos");

/// Whether per-display photos have to be stitched into one image.
pub const SPANS_ONE_IMAGE: bool = cfg!(target_os = "linux");

/// GNOME only: stitches each display's photo into one image laid out like
/// the monitors, written as a JPEG to `dest`.
pub fn compose(parts: &[(Display, PathBuf)], dest: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    return span::compose(parts, dest);
    #[allow(unreachable_code)]
    {
        let _ = (parts, dest);
        anyhow::bail!("only GNOME needs a stitched wallpaper")
    }
}

/// macOS only: switch any screen on the active Space that shows an older
/// photo from `images_dir` (one cleanup may have deleted) to its current
/// photo. Returns whether anything changed. A no-op elsewhere.
pub fn reconcile(layout: &Layout, images_dir: &Path) -> Result<bool> {
    #[cfg(target_os = "macos")]
    return macos::reconcile(layout, images_dir);
    #[allow(unreachable_code)]
    {
        let _ = (layout, images_dir);
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_get_their_own_photo_or_take_turns() {
        let each = Layout::Each(vec![
            ("a".into(), "a.jpg".into()),
            ("b".into(), "b.jpg".into()),
        ]);
        assert_eq!(each.file_for("b", 0), Some(Path::new("b.jpg")));
        // A display that showed up later borrows one in turn.
        assert_eq!(each.file_for("new", 2), Some(Path::new("a.jpg")));
        assert_eq!(each.file_for("new", 3), Some(Path::new("b.jpg")));

        let same = Layout::Same("all.jpg".into());
        assert_eq!(same.file_for("any", 5), Some(Path::new("all.jpg")));
        assert_eq!(Layout::Each(Vec::new()).file_for("any", 0), None);
    }
}
