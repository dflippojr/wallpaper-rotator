//! Stitches per-display photos into one image for GNOME's `spanned` mode,
//! which stretches a single picture across the bounding box of all monitors.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use image::imageops::{self, FilterType};
use image::RgbImage;

use super::Display;

/// Refuse layouts bigger than this many pixels (about 1.5 GB of RGB).
const MAX_PIXELS: u64 = 500_000_000;

pub fn compose(parts: &[(Display, PathBuf)], dest: &Path) -> Result<()> {
    let displays = || parts.iter().map(|(d, _)| d);
    let left = displays()
        .map(|d| d.x)
        .min()
        .context("no displays to stitch")?;
    let top = displays().map(|d| d.y).min().unwrap_or(0);
    let right = displays().map(|d| d.x + d.width as i32).max().unwrap_or(0);
    let bottom = displays().map(|d| d.y + d.height as i32).max().unwrap_or(0);
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        bail!("monitor layout of {width}x{height} is too large to stitch");
    }

    let mut canvas = RgbImage::new(width, height);
    for (display, file) in parts {
        let photo =
            image::open(file).with_context(|| format!("could not read {}", file.display()))?;
        // Photos are downloaded at the display's size; resize only if the
        // layout changed since.
        let rgb = if (photo.width(), photo.height()) == (display.width, display.height) {
            photo.into_rgb8()
        } else {
            let fitted =
                photo.resize_to_fill(display.width, display.height, FilterType::CatmullRom);
            // The decoded original is not needed any more: free it now.
            drop(photo);
            fitted.into_rgb8()
        };
        imageops::replace(
            &mut canvas,
            &rgb,
            i64::from(display.x - left),
            i64::from(display.y - top),
        );
    }

    crate::source::write_jpeg(&canvas, dest).context("could not write the stitched wallpaper")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_probe::large_allocations;
    use crate::source::write_jpeg;
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
