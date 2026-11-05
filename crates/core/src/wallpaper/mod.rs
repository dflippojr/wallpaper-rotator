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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Display {
    /// Identifies the display while it stays connected.
    pub id: String,
    /// Shown in the popover, such as "Built-in Retina Display".
    pub name: String,
    /// Top-left corner in the desktop's layout: points on macOS, desktop
    /// pixels on Windows, logical pixels on GNOME. Only GNOME uses it, to
    /// stitch photos into one spanned image.
    pub x: i32,
    pub y: i32,
    /// The display's physical pixels (its logical size times `scale`), as
    /// it is turned now: the size its photo is downloaded and cropped to.
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel: 2 on a Retina panel, 1.5 for
    /// Windows' 150 %. Lists saved before it existed read as 1.
    #[serde(default = "unit_scale")]
    pub scale: f64,
    /// Physical pixels per pixel of `x` and `y`'s layout, when that differs
    /// from `scale`: GNOME's physical layout mode (X11) lays monitors out
    /// in their own pixels whatever their scale, so it is 1 there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_scale: Option<f64>,
}

fn unit_scale() -> f64 {
    1.0
}

impl Display {
    /// The scale factor, or 1 when it makes no sense (zero, negative, NaN).
    pub fn scale(&self) -> f64 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }

    /// Physical pixels per layout pixel: `layout_scale`, else `scale`.
    pub fn layout_scale(&self) -> f64 {
        match self.layout_scale {
            Some(s) if s.is_finite() && s > 0.0 => s,
            _ => self.scale(),
        }
    }

    /// The display's size in the layout's pixels (logical ones, except in
    /// GNOME's physical layout mode).
    pub fn logical_size(&self) -> (f64, f64) {
        let s = self.layout_scale();
        (f64::from(self.width) / s, f64::from(self.height) / s)
    }

    /// One line for the log and the `displays` command, such as
    /// "Built-in: 3024x1964 pixels, scale 2".
    pub fn describe(&self) -> String {
        format!(
            "{}: {}x{} pixels, scale {}",
            self.name,
            self.width,
            self.height,
            self.scale()
        )
    }
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

/// macOS: a screen from its frame (origin and size in points) and backing
/// scale. Its photo size is its real pixels; its position stays in points,
/// the one space every screen shares.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn from_points(
    id: String,
    name: String,
    (x, y): (f64, f64),
    (width, height): (f64, f64),
    scale: f64,
) -> Display {
    Display {
        id,
        name,
        x: x.round() as i32,
        y: y.round() as i32,
        width: (width * scale).round() as u32,
        height: (height * scale).round() as u32,
        scale,
        layout_scale: None,
    }
}

/// A monitor as the windowing toolkit reports it (GTK, on GNOME): position
/// and size in physical pixels, and its scale factor.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// Linux, when Mutter can't be asked: the displays for the toolkit's
/// monitors. GTK 3 rounds fractional scales up to whole ones, so there its
/// sizes can be off; Mutter's list ([`displays`]) has the real ones. Each keeps its own
/// physical size and scale; positions go back to GNOME's logical layout
/// (the toolkit multiplies them by each monitor's own scale), where
/// [`compose`] stitches the photos.
pub fn toolkit_displays(monitors: &[Monitor]) -> Vec<Display> {
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let scale = if m.scale.is_finite() && m.scale >= 0.1 {
                m.scale
            } else {
                1.0
            };
            let logical = |v: i32| (f64::from(v) / scale).round() as i32;
            let (x, y) = (logical(m.x), logical(m.y));
            let name = m
                .name
                .clone()
                .unwrap_or_else(|| format!("Display {}", i + 1));
            Display {
                id: format!("{name}@{x},{y}"),
                name,
                x,
                y,
                width: m.width,
                height: m.height,
                scale,
                layout_scale: None,
            }
        })
        .collect()
}

/// The connected displays, where the platform can list them itself: on
/// GNOME from Mutter's display service. `None` when that fails (another
/// Linux desktop, say); the app then reads monitors from the windowing
/// toolkit ([`toolkit_displays`]).
pub fn displays() -> Option<Vec<Display>> {
    #[cfg(windows)]
    return windows::displays().ok();
    #[cfg(target_os = "macos")]
    return macos::displays().ok();
    #[cfg(target_os = "linux")]
    return gnome::displays().ok();
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

    /// GTK reports each monitor's position and size in physical pixels: its
    /// logical geometry times its own scale. The MacBook-style panel is
    /// 1512x982 logical at scale 2, the external 2560x1440 at scale 1 to its
    /// right, and a rotated portrait 1440x2560 at scale 1 after that.
    fn mixed_monitors() -> Vec<Monitor> {
        let monitor = |name: &str, x: i32, width: u32, height: u32, scale: f64| Monitor {
            name: Some(name.into()),
            x,
            y: 0,
            width,
            height,
            scale,
        };
        vec![
            monitor("Built-in", 0, 3024, 1964, 2.0),
            monitor("External", 1512, 2560, 1440, 1.0),
            monitor("Portrait", 4072, 1440, 2560, 1.0),
        ]
    }

    #[test]
    fn toolkit_monitors_are_sized_at_their_own_physical_pixels() {
        let displays = toolkit_displays(&mixed_monitors());
        let sizes: Vec<_> = displays
            .iter()
            .map(|d| (d.name.as_str(), d.width, d.height))
            .collect();
        assert_eq!(
            sizes,
            [
                ("Built-in", 3024, 1964),
                ("External", 2560, 1440),
                ("Portrait", 1440, 2560),
            ],
            "each display's photo size is its own physical pixels"
        );
        // Positions stay in GNOME's logical layout, where the monitors touch.
        let xs: Vec<_> = displays.iter().map(|d| d.x).collect();
        assert_eq!(xs, [0, 1512, 4072]);
    }

    #[test]
    fn toolkit_displays_carry_each_monitor_scale() {
        let displays = toolkit_displays(&mixed_monitors());
        let scales: Vec<_> = displays.iter().map(|d| d.scale).collect();
        assert_eq!(scales, [2.0, 1.0, 1.0]);
        assert_eq!(displays[1].id, "External@1512,0");
        assert_eq!(displays[0].logical_size(), (1512.0, 982.0));
        assert_eq!(
            displays[0].describe(),
            "Built-in: 3024x1964 pixels, scale 2"
        );

        // A fractional scale, and nonsense ones that count as 1.
        let mut odd = mixed_monitors();
        odd[0].scale = 1.25;
        odd[0].x = 0;
        odd[1].x = 2500;
        odd[1].scale = f64::NAN;
        odd[2].name = None;
        odd[2].scale = 0.0;
        let displays = toolkit_displays(&odd);
        let got: Vec<_> = displays.iter().map(|d| (d.x, d.scale)).collect();
        assert_eq!(got, [(0, 1.25), (2500, 1.0), (4072, 1.0)]);
        assert_eq!(displays[2].name, "Display 3");
    }

    /// The issue's Mac layout, with the external monitor placed left of
    /// the panel: positions stay in points (the old code multiplied each by
    /// its own screen's scale, so mixed screens no longer touched).
    #[test]
    fn mac_screens_are_sized_in_pixels_and_placed_in_points() {
        let panel = from_points(
            "1".into(),
            "Built-in".into(),
            (0.0, 0.0),
            (1512.0, 982.0),
            2.0,
        );
        let external = from_points(
            "2".into(),
            "External".into(),
            (-2560.0, 0.0),
            (2560.0, 1440.0),
            1.0,
        );
        let above = from_points(
            "3".into(),
            "Above".into(),
            (0.0, 982.0),
            (1440.0, 2560.0),
            2.0,
        );
        let got: Vec<_> = [&panel, &external, &above]
            .iter()
            .map(|d| (d.x, d.y, d.width, d.height, d.scale))
            .collect();
        assert_eq!(
            got,
            [
                (0, 0, 3024, 1964, 2.0),
                (-2560, 0, 2560, 1440, 1.0),
                (0, 982, 2880, 5120, 2.0),
            ]
        );
    }

    #[test]
    fn saved_displays_without_a_scale_read_as_scale_1() {
        let old = r#"{"id":"a","name":"A","x":0,"y":0,"width":1920,"height":1080}"#;
        let display: Display = serde_json::from_str(old).unwrap();
        assert_eq!(display.scale, 1.0);
        let round_trip: Display = serde_json::from_str(
            &serde_json::to_string(&Display {
                scale: 2.0,
                ..display
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(round_trip.scale, 2.0);
        // A layout in physical pixels places the display by its own pixels.
        let physical = Display {
            layout_scale: Some(1.0),
            ..round_trip.clone()
        };
        assert_eq!((physical.scale(), physical.layout_scale()), (2.0, 1.0));
        assert_eq!(physical.logical_size(), (1920.0, 1080.0));
        assert_eq!(round_trip.logical_size(), (960.0, 540.0));
        assert!(!serde_json::to_string(&round_trip)
            .unwrap()
            .contains("layout_scale"));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let d = Display {
                scale: bad,
                ..round_trip.clone()
            };
            assert_eq!(d.scale(), 1.0, "{bad}");
            assert_eq!(d.logical_size(), (1920.0, 1080.0));
        }
    }
}
