//! Where photos come from. Each source turns one request into a batch of
//! [`Candidate`]s; the worker queues them, picks ones that fit each display
//! and downloads each at that display's size.

pub mod daily;
pub mod favorites;
pub mod pexels;
pub mod unsplash;
pub mod wallhaven;

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::ImageReader;
use serde::{Deserialize, Serialize};

use crate::net::{part_path, Http};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum SourceId {
    #[default]
    Unsplash,
    Pexels,
    Wallhaven,
    /// Picture-of-the-day feeds: Bing and Wikimedia Commons.
    Daily,
    Favorites,
}

impl SourceId {
    pub const ALL: [SourceId; 5] = [
        SourceId::Unsplash,
        SourceId::Pexels,
        SourceId::Wallhaven,
        SourceId::Daily,
        SourceId::Favorites,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SourceId::Unsplash => "Unsplash",
            SourceId::Pexels => "Pexels",
            SourceId::Wallhaven => "Wallhaven",
            SourceId::Daily => "Photo of the day",
            SourceId::Favorites => "Favorites",
        }
    }

    /// Used in favorite file names, so a saved photo's credit can be rebuilt.
    pub fn key(self) -> &'static str {
        match self {
            SourceId::Unsplash => "unsplash",
            SourceId::Pexels => "pexels",
            SourceId::Wallhaven => "wallhaven",
            SourceId::Daily => "daily",
            SourceId::Favorites => "favorites",
        }
    }
}

/// How to get a candidate's image at a given size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Image {
    /// The server crops: `{w}` and `{h}` in the URL become the size.
    Sized { url: String },
    /// Download the full image and crop it here.
    Full { url: String },
    /// Wikimedia: `{w}` in the thumbnail URL picks one of the widths the
    /// server allows; `original` is used when it's smaller anyway.
    Thumb { template: String, original: String },
    /// A file on disk (favorites); cropped into the cache, never touched.
    File { path: PathBuf },
}

/// A photo a source offered, not downloaded yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub source: SourceId,
    pub id: String,
    /// Pixel size, or 0 when the source doesn't say.
    pub width: u32,
    pub height: u32,
    pub author: String,
    pub author_url: String,
    /// The photo's page; the popover's credit links here.
    pub photo_url: String,
    /// Small hotlinked image for the popover; empty for local files.
    pub preview_url: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub image: Image,
    /// Unsplash: called when the photo becomes a wallpaper.
    #[serde(default)]
    pub track_url: Option<String>,
}

/// Widths Wikimedia serves thumbnails at (others get HTTP 400).
const THUMB_WIDTHS: [u32; 2] = [1920, 3840];

/// Puts `candidate`'s image at exactly `width`x`height` in `dest` (a JPEG).
pub fn fetch_image(
    http: &Http,
    candidate: &Candidate,
    width: u32,
    height: u32,
    dest: &Path,
) -> Result<()> {
    let name = candidate.source.name();
    let original = dest.with_extension("orig.jpg");
    let result = (|| -> Result<()> {
        match (&candidate.image, image_url(candidate, width, height)) {
            (Image::File { path }, _) => crop(path, width, height, dest, false)?,
            (Image::Sized { .. }, Some(url)) => {
                http.download(name, &url, dest)?;
                // A server that ignored the size gets cropped here.
                if image::image_dimensions(dest).ok() != Some((width, height)) {
                    crop(dest, width, height, dest, false)?;
                }
            }
            (image, Some(url)) => {
                http.download(name, &url, &original)?;
                let landscape_only = matches!(image, Image::Full { .. }) && candidate.width == 0;
                crop(&original, width, height, dest, landscape_only)?;
            }
            (_, None) => bail!("{name} gave no image URL"),
        }
        Ok(())
    })();
    let _ = fs::remove_file(&original);
    result
}

/// The URL [`fetch_image`] downloads for a `width`x`height` wallpaper, or
/// `None` for a local file.
pub fn image_url(candidate: &Candidate, width: u32, height: u32) -> Option<String> {
    Some(match &candidate.image {
        Image::Sized { url } => url
            .replace("{w}", &width.to_string())
            .replace("{h}", &height.to_string()),
        Image::Full { url } => url.clone(),
        Image::Thumb { template, original } => {
            // Wide enough for `width`, and, at the photo's shape, for `height`.
            let needed = match (candidate.width, candidate.height) {
                (w, h) if w > 0 && h > 0 => {
                    let wide = (u64::from(height) * u64::from(w)).div_ceil(u64::from(h));
                    width.max(u32::try_from(wide).unwrap_or(u32::MAX))
                }
                _ => width,
            };
            let step = THUMB_WIDTHS.iter().find(|&&w| w >= needed);
            match step {
                Some(&w) if candidate.width == 0 || candidate.width > w => {
                    template.replace("{w}", &w.to_string())
                }
                _ => original.clone(),
            }
        }
        Image::File { .. } => return None,
    })
}

/// Scales `src` to cover `width`x`height`, crops the overflow from the
/// middle and writes a JPEG to `dest` (which may be `src`). With
/// `landscape_only`, portrait images are refused: a 16:9 crop of one would
/// keep only a strip.
pub fn crop(src: &Path, width: u32, height: u32, dest: &Path, landscape_only: bool) -> Result<()> {
    let photo = ImageReader::open(src)
        .and_then(|r| r.with_guessed_format())
        .with_context(|| format!("could not open {}", src.display()))?
        .decode()
        .with_context(|| format!("could not read the image in {}", src.display()))?;
    if landscape_only && photo.height() > photo.width() {
        bail!("skipped a portrait image");
    }
    let photo = if (photo.width(), photo.height()) == (width, height) {
        photo
    } else {
        photo.resize_to_fill(width, height, FilterType::CatmullRom)
    };
    write_jpeg(&photo.to_rgb8(), dest)
}

pub fn write_jpeg(img: &image::RgbImage, dest: &Path) -> Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let part = part_path(dest);
    let result = (|| -> Result<()> {
        let mut out = BufWriter::new(File::create(&part)?);
        JpegEncoder::new_with_quality(&mut out, 90).encode_image(img)?;
        out.into_inner()?.sync_all()?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(fs::rename(&part, dest)?),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e.context("could not write the wallpaper image"))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::alloc_probe::large_allocations;
    use image::{DynamicImage, GrayImage, ImageBuffer, Rgb, RgbImage, Rgba, RgbaImage};

    pub fn png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let file = dir.join(name);
        RgbImage::from_pixel(w, h, Rgb([10, 120, 200]))
            .save(&file)
            .unwrap();
        file
    }

    pub fn candidate(source: SourceId, id: &str, image: Image) -> Candidate {
        Candidate {
            source,
            id: id.into(),
            width: 400,
            height: 300,
            author: "Ann".into(),
            author_url: String::new(),
            photo_url: String::new(),
            preview_url: String::new(),
            location: None,
            description: None,
            color: None,
            image,
            track_url: None,
        }
    }

    #[test]
    fn thumbnails_cover_the_height_too() {
        let thumb = |width, height| {
            let image = Image::Thumb {
                template: "t/{w}px".into(),
                original: "orig".into(),
            };
            Candidate {
                width,
                height,
                ..candidate(SourceId::Daily, "c", image)
            }
        };
        assert_eq!(
            image_url(&thumb(3935, 3263), 1920, 1080).as_deref(),
            Some("t/1920px")
        );
        // 1920 wide would be 720 high; 3840 wide is 1440.
        assert_eq!(
            image_url(&thumb(4000, 1500), 1920, 1080).as_deref(),
            Some("t/3840px")
        );
        // No thumbnail covers 1080 high: the original does.
        assert_eq!(
            image_url(&thumb(9000, 1100), 1920, 1080).as_deref(),
            Some("orig")
        );
    }

    #[test]
    fn crops_local_files_to_size_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        let src = png(dir.path(), "fav.png", 400, 300);
        let before = fs::read(&src).unwrap();
        let dest = dir.path().join("out.jpg");
        let c = candidate(
            SourceId::Favorites,
            "fav",
            Image::File { path: src.clone() },
        );
        fetch_image(&Http::default(), &c, 160, 90, &dest).unwrap();
        assert_eq!(image::image_dimensions(&dest).unwrap(), (160, 90));
        assert_eq!(fs::read(&src).unwrap(), before);
        assert!(!dir.path().join("out.jpg.part").exists());
    }

    #[test]
    fn refuses_portraits_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let src = png(dir.path(), "tall.png", 100, 300);
        let dest = dir.path().join("out.jpg");
        assert!(crop(&src, 160, 90, &dest, true).is_err());
        crop(&src, 160, 90, &dest, false).unwrap();
        assert_eq!(image::image_dimensions(&dest).unwrap(), (160, 90));
    }

    /// Pixels that vary, so the PNG stays small while the decoded image is
    /// full size.
    fn gradient(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let file = dir.join(name);
        RgbImage::from_fn(w, h, |x, y| Rgb([x as u8, y as u8, (x ^ y) as u8]))
            .save(&file)
            .unwrap();
        file
    }

    /// The crop as it was written before `into_rgb8`: every image that is not
    /// already RGB8 was copied with `to_rgb8`. Same pixels, same JPEG bytes.
    fn previous_crop(src: &Path, width: u32, height: u32, dest: &Path) {
        let photo = image::open(src).unwrap();
        let photo = if (photo.width(), photo.height()) == (width, height) {
            photo
        } else {
            photo.resize_to_fill(width, height, FilterType::CatmullRom)
        };
        write_jpeg(&photo.to_rgb8(), dest).unwrap();
    }

    #[test]
    fn crop_output_is_byte_identical_to_the_previous_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let rgba = RgbaImage::from_fn(320, 240, |x, y| {
            Rgba([x as u8, y as u8, 90, (x % 255) as u8])
        });
        let gray = GrayImage::from_fn(160, 90, |x, y| image::Luma([(x + y) as u8]));
        let rgb16: ImageBuffer<image::Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_fn(320, 180, |x, y| {
                image::Rgb([(x * 200) as u16, (y * 300) as u16, 4000])
            });
        let rgba_file = dir.path().join("rgba.png");
        DynamicImage::ImageRgba8(rgba).save(&rgba_file).unwrap();
        let gray_file = dir.path().join("gray.png");
        DynamicImage::ImageLuma8(gray).save(&gray_file).unwrap();
        let rgb16_file = dir.path().join("rgb16.png");
        DynamicImage::ImageRgb16(rgb16).save(&rgb16_file).unwrap();
        let cases = [
            ("rgb8 at size", png(dir.path(), "same.png", 160, 90)),
            ("rgb8 scaled", gradient(dir.path(), "big.png", 640, 400)),
            ("rgba scaled", rgba_file),
            ("gray at size", gray_file),
            ("rgb16 scaled", rgb16_file),
        ];
        for (name, src) in cases {
            let new = dir.path().join("new.jpg");
            let old = dir.path().join("old.jpg");
            crop(&src, 160, 90, &new, false).unwrap();
            previous_crop(&src, 160, 90, &old);
            assert_eq!(fs::read(&new).unwrap(), fs::read(&old).unwrap(), "{name}");
        }
    }

    #[test]
    fn crop_of_an_rgb8_original_makes_no_copy_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (640, 400);
        let src = gradient(dir.path(), "same.png", w, h);
        let dest = dir.path().join("out.jpg");
        // Already the requested size, so nothing is resized: the decoded
        // original is the only full-size buffer, and a copy would be a second.
        let full = w as usize * h as usize * 3;
        let (result, full_size_buffers) =
            large_allocations(full, || crop(&src, w, h, &dest, false));
        result.unwrap();
        assert_eq!(full_size_buffers, 1, "full-size buffers made by crop");
    }
}
