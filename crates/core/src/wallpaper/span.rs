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
        let photo = if (photo.width(), photo.height()) == (display.width, display.height) {
            photo
        } else {
            photo.resize_to_fill(display.width, display.height, FilterType::CatmullRom)
        };
        imageops::replace(
            &mut canvas,
            &photo.to_rgb8(),
            i64::from(display.x - left),
            i64::from(display.y - top),
        );
    }

    crate::source::write_jpeg(&canvas, dest).context("could not write the stitched wallpaper")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

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
}
