//! Stitches per-display photos into one image for GNOME's `spanned` mode,
//! which stretches a single picture across the bounding box of all monitors.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use image::imageops;
use image::RgbImage;

use super::Display;
use crate::source::fit_rgb8;

/// Refuse layouts bigger than this many pixels (about 1.5 GB of RGB).
const MAX_PIXELS: u64 = 500_000_000;

/// A display's place on the canvas: left, top, width, height.
pub(super) type Rect = (i64, i64, u32, u32);

/// Where each display goes on the stitched canvas: GNOME's logical layout
/// times the highest scale factor, so the sharpest display keeps every
/// pixel of its photo. A display with a lower scale gets a proportionally
/// bigger rectangle (its photo is scaled up evenly, never stretched), which
/// GNOME scales back down to that display's own pixels.
pub(super) fn placements(displays: &[&Display]) -> Result<((u32, u32), Vec<Rect>)> {
    let k = displays
        .iter()
        .map(|d| d.layout_scale())
        .fold(1.0, f64::max);
    let extent = |d: &&Display| {
        let (w, h) = d.logical_size();
        (
            f64::from(d.x),
            f64::from(d.y),
            f64::from(d.x) + w,
            f64::from(d.y) + h,
        )
    };
    let left = displays
        .iter()
        .map(|d| extent(d).0)
        .reduce(f64::min)
        .context("no displays to stitch")?;
    let top = displays
        .iter()
        .map(|d| extent(d).1)
        .fold(f64::MAX, f64::min);
    let right = displays
        .iter()
        .map(|d| extent(d).2)
        .fold(f64::MIN, f64::max);
    let bottom = displays
        .iter()
        .map(|d| extent(d).3)
        .fold(f64::MIN, f64::max);
    // Edges are rounded, not sizes, so touching displays still touch.
    let px = |v: f64, origin: f64| ((v - origin) * k).round();
    let (width, height) = (px(right, left), px(bottom, top));
    if width < 1.0 || height < 1.0 || width * height > MAX_PIXELS as f64 {
        bail!("monitor layout of {width}x{height} is too large to stitch");
    }
    let rects = displays
        .iter()
        .map(|d| {
            let (l, t, r, b) = extent(d);
            let (x, y) = (px(l, left), px(t, top));
            (
                x as i64,
                y as i64,
                (px(r, left) - x).max(1.0) as u32,
                (px(b, top) - y).max(1.0) as u32,
            )
        })
        .collect();
    Ok(((width as u32, height as u32), rects))
}

pub fn compose(parts: &[(Display, PathBuf)], dest: &Path) -> Result<()> {
    let displays: Vec<&Display> = parts.iter().map(|(d, _)| d).collect();
    let ((width, height), rects) = placements(&displays)?;

    let mut canvas = RgbImage::new(width, height);
    for ((_, file), (x, y, w, h)) in parts.iter().zip(rects) {
        let photo =
            image::open(file).with_context(|| format!("could not read {}", file.display()))?;
        // Photos are downloaded at the display's physical size, which is
        // its rectangle here unless the layout changed since or another
        // display is sharper.
        fit_rgb8(photo, w, h, |rgb| {
            imageops::replace(&mut canvas, rgb, x, y);
        });
    }

    crate::source::write_jpeg(&canvas, dest).context("could not write the stitched wallpaper")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_probe::large_allocations;
    use crate::source::write_jpeg;
    use image::imageops::FilterType;
    use image::{imageops, DynamicImage, Rgb, Rgba, RgbaImage};
    use std::fs;

    fn photo(dir: &Path, name: &str, w: u32, h: u32, color: [u8; 3]) -> PathBuf {
        let file = dir.join(name);
        RgbImage::from_pixel(w, h, Rgb(color)).save(&file).unwrap();
        file
    }

    fn display(id: &str, x: i32, y: i32, width: u32, height: u32) -> Display {
        Display {
            id: id.into(),
            name: id.into(),
            x,
            y,
            width,
            height,
            scale: 1.0,
            layout_scale: None,
        }
    }

    fn near(a: Rgb<u8>, b: [u8; 3]) -> bool {
        a.0.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 12)
    }

    #[test]
    fn lays_photos_out_like_the_monitors() {
        let dir = tempfile::tempdir().unwrap();
        // A taller monitor on the left, a shorter one to its right, with a
        // negative origin like a monitor placed left of the primary.
        let parts = vec![
            (
                display("left", -40, 0, 40, 30),
                photo(dir.path(), "l.jpg", 40, 30, [200, 20, 20]),
            ),
            (
                // Downloaded for a different size: gets resized to fit.
                display("right", 0, 0, 20, 10),
                photo(dir.path(), "r.jpg", 64, 64, [20, 20, 200]),
            ),
        ];
        let dest = dir.path().join("span.jpg");
        compose(&parts, &dest).unwrap();

        let out = image::open(&dest).unwrap().to_rgb8();
        assert_eq!(out.dimensions(), (60, 30));
        assert!(near(*out.get_pixel(10, 15), [200, 20, 20]));
        assert!(near(*out.get_pixel(50, 5), [20, 20, 200]));
        // The gap below the short monitor stays black.
        assert!(near(*out.get_pixel(50, 25), [0, 0, 0]));
        assert!(!dir.path().join("span.jpg.part").exists());
        assert!(compose(&[], &dir.path().join("x.jpg")).is_err());
    }

    fn scaled(display: Display, scale: f64) -> Display {
        Display { scale, ..display }
    }

    /// The issue's layout: a 3024x1964 panel at scale 2, a 2560x1440
    /// monitor at scale 1 to its right and a 1440x2560 portrait one after
    /// that, at GNOME's logical positions.
    #[test]
    fn mixed_scales_are_placed_at_the_sharpest_scale_without_stretching() {
        let displays = [
            scaled(display("built-in", 0, 0, 3024, 1964), 2.0),
            display("external", 1512, 0, 2560, 1440),
            display("portrait", 4072, 0, 1440, 2560),
        ];
        let refs: Vec<&Display> = displays.iter().collect();
        let (canvas, rects) = placements(&refs).unwrap();
        // The logical layout is 5512x1280; the canvas is twice that.
        assert_eq!(canvas, (11024, 5120));
        assert_eq!(
            rects,
            [
                // The panel's photo goes in pixel for pixel.
                (0, 0, 3024, 1964),
                // The others are scaled up evenly, keeping their shape, and
                // GNOME scales them back down to their own pixels.
                (3024, 0, 5120, 2880),
                (8144, 0, 2880, 5120),
            ]
        );
        for (d, (_, _, w, h)) in displays.iter().zip(&rects) {
            let photo = f64::from(d.width) / f64::from(d.height);
            let placed = f64::from(*w) / f64::from(*h);
            assert!((photo - placed).abs() < 1e-3, "{} is stretched", d.id);
        }
    }

    #[test]
    fn touching_displays_at_a_fractional_scale_leave_no_gap() {
        // 1.25: 1601 pixels are 1280.8 logical ones, so sizes don't round
        // evenly; edges do.
        let displays = [
            scaled(display("a", 0, 0, 1601, 901), 1.25),
            display("b", 1281, 0, 1000, 700),
        ];
        let refs: Vec<&Display> = displays.iter().collect();
        let (canvas, rects) = placements(&refs).unwrap();
        let (ax, _, aw, _) = rects[0];
        let (bx, _, bw, _) = rects[1];
        assert_eq!(ax, 0);
        assert_eq!(bx, ax + i64::from(aw), "they must touch: {rects:?}");
        assert_eq!(i64::from(canvas.0), bx + i64::from(bw));
        assert!(placements(&[]).is_err());
    }

    #[test]
    fn a_mixed_scale_stitch_shows_each_photo_in_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let parts = vec![
            (
                scaled(display("retina", 0, 0, 40, 20), 2.0),
                photo(dir.path(), "r.png", 40, 20, [200, 20, 20]),
            ),
            (
                display("external", 20, 0, 30, 15),
                photo(dir.path(), "e.png", 30, 15, [20, 20, 200]),
            ),
        ];
        let dest = dir.path().join("span.jpg");
        compose(&parts, &dest).unwrap();
        let out = image::open(&dest).unwrap().to_rgb8();
        assert_eq!(out.dimensions(), (100, 30));
        assert!(near(*out.get_pixel(20, 10), [200, 20, 20]));
        assert!(near(*out.get_pixel(70, 25), [20, 20, 200]));
        // Below the panel stays black.
        assert!(near(*out.get_pixel(20, 26), [0, 0, 0]));
    }

    #[test]
    fn stitched_output_is_byte_identical_to_the_previous_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let alpha = RgbaImage::from_fn(40, 30, |x, y| Rgba([200, (x * 5) as u8, y as u8, 255]));
        let alpha_file = dir.path().join("alpha.png");
        DynamicImage::ImageRgba8(alpha).save(&alpha_file).unwrap();
        let parts = vec![
            (display("left", 0, 0, 40, 30), alpha_file),
            // Downloaded for a different size: resized before it is placed.
            (
                display("right", 40, 0, 20, 10),
                photo(dir.path(), "r.png", 64, 64, [20, 20, 200]),
            ),
        ];
        let dest = dir.path().join("span.jpg");
        compose(&parts, &dest).unwrap();

        // The same stitch, built the way it was before `into_rgb8`.
        let mut canvas = RgbImage::new(60, 30);
        let left = image::open(&parts[0].1).unwrap();
        imageops::replace(&mut canvas, &left.to_rgb8(), 0, 0);
        let right =
            image::open(&parts[1].1)
                .unwrap()
                .resize_to_fill(20, 10, FilterType::CatmullRom);
        imageops::replace(&mut canvas, &right.to_rgb8(), 40, 0);
        let expected = dir.path().join("expected.jpg");
        write_jpeg(&canvas, &expected).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), fs::read(&expected).unwrap());
    }

    #[test]
    fn stitching_copies_no_photo() {
        let dir = tempfile::tempdir().unwrap();
        let parts = vec![
            (
                display("left", 0, 0, 320, 200),
                photo(dir.path(), "l.png", 320, 200, [200, 20, 20]),
            ),
            (
                display("right", 320, 0, 320, 200),
                photo(dir.path(), "r.png", 320, 200, [20, 20, 200]),
            ),
        ];
        let dest = dir.path().join("span.jpg");
        // Both photos are already their display's size. The decoder makes its
        // own buffers, so measure what decoding each photo alone makes and
        // expect compose to add only the canvas on top of that.
        let photo_bytes = 320 * 200 * 3;
        let decoding: usize = parts
            .iter()
            .map(|(_, file)| large_allocations(photo_bytes, || image::open(file).unwrap()).1)
            .sum();
        let (result, large) = large_allocations(photo_bytes, || compose(&parts, &dest));
        result.unwrap();
        assert_eq!(
            large,
            decoding + 1,
            "full-size buffers made by compose (decoding each photo, plus the canvas)"
        );
    }
}
