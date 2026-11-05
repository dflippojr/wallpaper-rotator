use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use zbus::zvariant::OwnedValue;

use super::{Display, Layout};

const SCHEMA: &str = "org.gnome.desktop.background";

pub fn set(layout: &Layout) -> Result<()> {
    let (file, option) = match layout {
        // Zoom covers each monitor, cropping the overflow.
        Layout::Same(file) => (file, "zoom"),
        // Spanned stretches one image across the whole monitor layout.
        Layout::Spanned(file) => (file, "spanned"),
        Layout::Each(_) => bail!("GNOME needs per-display photos stitched into one image"),
    };
    let uri = file_uri(file);
    gsettings("picture-uri", &uri)?;
    // Older GNOME releases have no dark-mode key; that's fine.
    let _ = gsettings("picture-uri-dark", &uri);
    gsettings("picture-options", option)
}

fn gsettings(key: &str, value: &str) -> Result<()> {
    let output = Command::new("gsettings")
        .args(["set", SCHEMA, key, value])
        .output()
        .context("could not run gsettings; is this a GNOME desktop?")?;
    if !output.status.success() {
        bail!(
            "gsettings set {key} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

// ---- Displays, from Mutter's DisplayConfig D-Bus API ----
//
// GTK 3 only knows whole scale factors, so at GNOME's fractional scales it
// reports wrong pixel sizes. Mutter, GNOME's compositor, knows each
// monitor's real mode and scale.

const MUTTER: &str = "org.gnome.Mutter.DisplayConfig";
const MUTTER_PATH: &str = "/org/gnome/Mutter/DisplayConfig";

type Props = HashMap<String, OwnedValue>;
/// Connector, vendor, product, serial.
type MonitorSpec = (String, String, String, String);
/// ID, width, height, refresh rate, preferred scale, supported scales, props.
type Mode = (String, i32, i32, f64, f64, Vec<f64>, Props);
type PhysicalMonitor = (MonitorSpec, Vec<Mode>, Props);
/// x, y, scale, transform, primary, monitors, props.
type LogicalMonitor = (i32, i32, f64, u32, bool, Vec<MonitorSpec>, Props);
/// What `GetCurrentState` returns: serial, monitors, logical monitors, props.
pub(crate) type CurrentState = (u32, Vec<PhysicalMonitor>, Vec<LogicalMonitor>, Props);

/// Mutter's `layout-mode` for positions in physical pixels (X11). Without
/// the property, positions are logical.
const PHYSICAL_LAYOUT: u32 = 2;

pub fn displays() -> Result<Vec<Display>> {
    let bus = zbus::blocking::Connection::session().context("no D-Bus session")?;
    displays_on(&bus)
}

/// The displays as Mutter on `bus` reports them.
fn displays_on(bus: &zbus::blocking::Connection) -> Result<Vec<Display>> {
    let reply = bus
        .call_method(
            Some(MUTTER),
            MUTTER_PATH,
            Some(MUTTER),
            "GetCurrentState",
            &(),
        )
        .context("GNOME's display service did not answer")?;
    let state: CurrentState = reply
        .body()
        .deserialize()
        .context("GNOME's display service gave an unexpected answer")?;
    let displays = from_state(&state);
    if displays.is_empty() {
        bail!("GNOME reported no active displays");
    }
    Ok(displays)
}

fn prop<'a, T: TryFrom<&'a OwnedValue>>(props: &'a Props, key: &str) -> Option<T> {
    props.get(key).and_then(|v| T::try_from(v).ok())
}

/// One display per logical monitor (mirrored monitors share one): the
/// current mode of its monitor, turned as the monitor is, positioned in
/// GNOME's layout, at the logical monitor's scale (placed at 1 in physical
/// layout mode).
pub(crate) fn from_state((_, monitors, logical, props): &CurrentState) -> Vec<Display> {
    let physical_layout = prop::<u32>(props, "layout-mode") == Some(PHYSICAL_LAYOUT);
    logical
        .iter()
        .filter_map(|(x, y, scale, transform, _, specs, _)| {
            let (spec, modes, monitor_props) = specs
                .iter()
                .find_map(|spec| monitors.iter().find(|(s, _, _)| s == spec))?;
            let (_, w, h, ..) = modes
                .iter()
                .find(|(.., p)| prop::<bool>(p, "is-current") == Some(true))?;
            let (w, h) = (u32::try_from(*w).ok()?, u32::try_from(*h).ok()?);
            // Odd transforms turn the monitor a quarter.
            let (width, height) = if transform % 2 == 1 { (h, w) } else { (w, h) };
            let scale = if scale.is_finite() && *scale > 0.0 {
                *scale
            } else {
                1.0
            };
            let connector = spec.0.clone();
            let name = prop::<&str>(monitor_props, "display-name")
                .filter(|n| !n.is_empty())
                .map_or_else(|| connector.clone(), str::to_owned);
            Some(Display {
                id: connector,
                name,
                x: *x,
                y: *y,
                width,
                height,
                scale,
                // In physical layout mode (X11) positions and sizes are mode
                // pixels whatever the scale, so photos are placed at 1.
                layout_scale: physical_layout.then_some(1.0),
            })
        })
        .collect()
}

/// A `file://` URI with everything outside the unreserved set escaped.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_spaces_and_unicode() {
        assert_eq!(
            file_uri(Path::new("/home/d/.cache/Wallpaper Rotator/images/ä.jpg")),
            "file:///home/d/.cache/Wallpaper%20Rotator/images/%C3%A4.jpg"
        );
    }

    fn spec(connector: &str) -> MonitorSpec {
        (
            connector.into(),
            "VND".into(),
            "Model".into(),
            "0001".into(),
        )
    }

    fn mode(w: i32, h: i32, current: bool) -> Mode {
        let mut props = Props::new();
        if current {
            props.insert("is-current".into(), OwnedValue::from(true));
        }
        (
            format!("{w}x{h}@60"),
            w,
            h,
            60.0,
            1.0,
            vec![1.0, 2.0],
            props,
        )
    }

    fn monitor(connector: &str, name: &str, modes: Vec<Mode>) -> PhysicalMonitor {
        let mut props = Props::new();
        if !name.is_empty() {
            props.insert(
                "display-name".into(),
                OwnedValue::try_from(zbus::zvariant::Value::from(name)).unwrap(),
            );
        }
        (spec(connector), modes, props)
    }

    /// The issue's layout as Mutter reports it on Wayland: a 3024x1964 panel
    /// at scale 2, a 2560x1440 monitor at 1 to its right and a 2560x1440
    /// monitor turned to portrait after that, plus a fractional 1.5 one below
    /// and a disabled one with no logical monitor.
    fn mixed_state(layout_mode: Option<u32>) -> CurrentState {
        let mut props = Props::new();
        if let Some(mode) = layout_mode {
            props.insert("layout-mode".into(), OwnedValue::from(mode));
        }
        let logical = |x, y, scale, transform, connector: &str| {
            (
                x,
                y,
                scale,
                transform,
                false,
                vec![spec(connector)],
                Props::new(),
            )
        };
        (
            7,
            vec![
                monitor("eDP-1", "Built-in display", vec![mode(3024, 1964, true)]),
                monitor(
                    "DP-1",
                    "Dell 27\"",
                    vec![mode(3840, 2160, false), mode(2560, 1440, true)],
                ),
                monitor("DP-2", "", vec![mode(2560, 1440, true)]),
                monitor("HDMI-1", "TV", vec![mode(3840, 2160, true)]),
                monitor("DP-3", "Off", vec![mode(1920, 1080, false)]),
            ],
            vec![
                logical(0, 0, 2.0, 0, "eDP-1"),
                logical(1512, 0, 1.0, 0, "DP-1"),
                logical(4072, 0, 1.0, 1, "DP-2"),
                logical(0, 1440, 1.5, 0, "HDMI-1"),
            ],
            props,
        )
    }

    #[test]
    fn mutter_monitors_keep_their_own_pixels_scale_and_turn() {
        // Logical layout mode, stated or (without the property) assumed.
        for layout_mode in [None, Some(1)] {
            let displays = from_state(&mixed_state(layout_mode));
            let got: Vec<_> = displays
                .iter()
                .map(|d| {
                    (
                        d.id.as_str(),
                        d.name.as_str(),
                        d.x,
                        d.y,
                        d.width,
                        d.height,
                        d.scale,
                    )
                })
                .collect();
            assert_eq!(
                got,
                [
                    ("eDP-1", "Built-in display", 0, 0, 3024, 1964, 2.0),
                    ("DP-1", "Dell 27\"", 1512, 0, 2560, 1440, 1.0),
                    ("DP-2", "DP-2", 4072, 0, 1440, 2560, 1.0),
                    // 150 %: its real 3840x2160 pixels, not GTK's 2x guess.
                    ("HDMI-1", "TV", 0, 1440, 3840, 2160, 1.5),
                ],
                "layout mode {layout_mode:?}"
            );
        }
    }

    /// Physical layout mode (X11): positions and sizes are mode pixels,
    /// whatever the scales say, so the layout is kept as it is and every
    /// display is placed at 1, while still carrying its own scale.
    #[test]
    fn physical_layout_keeps_mutters_physical_geometry() {
        let (serial, monitors, mut logical, props) = mixed_state(Some(PHYSICAL_LAYOUT));
        // The same monitors, laid out in physical pixels.
        logical[1].0 = 3024;
        logical[2].0 = 5584;
        logical[3].1 = 1964;
        let displays = from_state(&(serial, monitors, logical, props));
        let got: Vec<_> = displays
            .iter()
            .map(|d| (d.x, d.y, d.width, d.height, d.scale, d.layout_scale()))
            .collect();
        assert_eq!(
            got,
            [
                (0, 0, 3024, 1964, 2.0, 1.0),
                (3024, 0, 2560, 1440, 1.0, 1.0),
                (5584, 0, 1440, 2560, 1.0, 1.0),
                (0, 1964, 3840, 2160, 1.5, 1.0),
            ]
        );
        assert_eq!(
            displays[0].describe(),
            "Built-in display: 3024x1964 pixels, scale 2"
        );
        // A scale change alone still changes the list, so the prefetched
        // set no longer fits and the next rotation is sized again.
        let (serial, monitors, mut logical2, props) = mixed_state(Some(PHYSICAL_LAYOUT));
        logical2[1].0 = 3024;
        logical2[2].0 = 5584;
        logical2[3].1 = 1964;
        logical2[0].2 = 1.0;
        assert_ne!(from_state(&(serial, monitors, logical2, props)), displays);
    }

    /// Stitched in physical layout mode, the panel and the monitor beside
    /// it touch: no gap opens where the layout has none.
    #[test]
    fn physical_layout_stitches_without_gaps() {
        let (serial, monitors, mut logical, props) = mixed_state(Some(PHYSICAL_LAYOUT));
        logical.truncate(2);
        logical[1].0 = 3024;
        let displays = from_state(&(serial, monitors, logical, props));
        let refs: Vec<&Display> = displays.iter().collect();
        let (canvas, rects) = super::super::span::placements(&refs).unwrap();
        assert_eq!(canvas, (5584, 1964));
        assert_eq!(rects, [(0, 0, 3024, 1964), (3024, 0, 2560, 1440)]);
    }

    /// A stand-in for Mutter, served over a socket pair, so the real D-Bus
    /// call and the decoding of its answer are tested end to end.
    struct FakeMutter;

    #[zbus::interface(name = "org.gnome.Mutter.DisplayConfig")]
    impl FakeMutter {
        fn get_current_state(&self) -> CurrentState {
            mixed_state(Some(1))
        }
    }

    #[test]
    fn displays_come_from_mutters_current_state_over_d_bus() {
        use zbus::blocking::connection::Builder;
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            Builder::async_io_unix_stream(a)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .serve_at(MUTTER_PATH, FakeMutter)
                .unwrap()
                .build()
                .unwrap()
        });
        let client = Builder::async_io_unix_stream(b).p2p().build().unwrap();
        let _server = server.join().unwrap();

        let displays = displays_on(&client).unwrap();
        assert_eq!(displays, from_state(&mixed_state(Some(1))));
        assert_eq!(displays[3].describe(), "TV: 3840x2160 pixels, scale 1.5");
    }

    /// The reply type must be exactly the signature Mutter documents for
    /// `GetCurrentState` (org.gnome.Mutter.DisplayConfig.xml), or decoding
    /// a real answer fails.
    #[test]
    fn the_reply_type_matches_mutters_documented_signature() {
        use zbus::zvariant::Type;
        assert_eq!(
            CurrentState::SIGNATURE.to_string(),
            "(ua((ssss)a(siiddada{sv})a{sv})a(iiduba(ssss)a{sv})a{sv})"
        );
    }

    #[test]
    fn no_active_display_is_an_error_so_the_app_falls_back() {
        let (serial, monitors, _, props) = mixed_state(None);
        assert!(from_state(&(serial, monitors, Vec::new(), props)).is_empty());
    }

    #[test]
    fn refuses_a_photo_per_display_before_touching_gsettings() {
        let each = Layout::Each(vec![("a".into(), "a.jpg".into())]);
        let err = set(&each).unwrap_err().to_string();
        assert!(err.contains("stitched"), "{err}");
    }
}
