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
use image::{DynamicImage, ImageReader};
use serde::{Deserialize, Serialize};

use crate::config::Keys;
use crate::net::{part_path, Http};
use crate::probe::{Fixture, Probe, ProbeArgs};

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
        spec(self).name
    }

    /// Used in favorite file names, so a saved photo's credit can be rebuilt.
    pub fn key(self) -> &'static str {
        spec(self).slug
    }
}

// ---- The table of sources ----

/// Which saved key a source needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Unsplash,
    Pexels,
}

/// One API a source calls. Errors, the fixture recorder and the live check
/// name it by `name`.
#[derive(Debug)]
pub struct Api {
    pub name: &'static str,
    /// Where it lives, without a trailing slash. A test can send it
    /// elsewhere with [`Http::with_bases`].
    pub base: &'static str,
    /// It answers 403, not 429, when its rate limit is used up.
    pub forbidden_is_rate_limit: bool,
    /// The live check's requests and parser (`live-sources`).
    pub probe: fn(ProbeArgs<'_>) -> Probe<'_>,
    /// The replies `record-fixtures` records for the tests.
    pub fixtures: fn(ProbeArgs<'_>) -> Vec<Fixture>,
}

/// Everything the app knows about one source, apart from how to ask it for
/// photos (see [`batch`]).
#[derive(Debug)]
pub struct Spec {
    pub id: SourceId,
    /// Shown in Settings and in credits.
    pub name: &'static str,
    /// Used in favorite file names; the same as the config's spelling.
    pub slug: &'static str,
    /// The key it needs, if any.
    pub key: Option<KeyKind>,
    /// The APIs it calls (the picture of the day has two).
    pub apis: &'static [Api],
    /// Photo and credit links the app may open in the browser.
    pub link_prefixes: &'static [&'static str],
    /// Origins its popover previews load from. The CSP's `img-src` in
    /// `app/tauri.conf.json` must list exactly these, which a test checks.
    pub preview_hosts: &'static [&'static str],
    /// Its credit names a photographer, not the site or a file.
    pub credits_a_person: bool,
}

pub const UNSPLASH: Api = Api {
    name: "Unsplash",
    base: "https://api.unsplash.com",
    // Unsplash answers 403 when the hourly limit is used up.
    forbidden_is_rate_limit: true,
    probe: unsplash::probe,
    fixtures: unsplash::fixtures,
};
pub const PEXELS: Api = Api {
    name: "Pexels",
    base: "https://api.pexels.com",
    forbidden_is_rate_limit: false,
    probe: pexels::probe,
    fixtures: pexels::fixtures,
};
pub const WALLHAVEN: Api = Api {
    name: "Wallhaven",
    base: "https://wallhaven.cc",
    forbidden_is_rate_limit: false,
    probe: wallhaven::probe,
    fixtures: wallhaven::fixtures,
};
pub const BING: Api = Api {
    name: "Bing",
    base: "https://www.bing.com",
    forbidden_is_rate_limit: false,
    probe: daily::bing_probe,
    fixtures: daily::bing_fixtures,
};
pub const WIKIMEDIA: Api = Api {
    name: "Wikimedia",
    base: "https://api.wikimedia.org",
    forbidden_is_rate_limit: false,
    probe: daily::wikimedia_probe,
    fixtures: daily::wikimedia_fixtures,
};

/// One row per source, in [`SourceId::ALL`] order.
pub const SPECS: [Spec; 5] = [
    Spec {
        id: SourceId::Unsplash,
        name: UNSPLASH.name,
        slug: "unsplash",
        key: Some(KeyKind::Unsplash),
        apis: &[UNSPLASH],
        link_prefixes: &["https://unsplash.com/", "https://www.unsplash.com/"],
        preview_hosts: &["https://images.unsplash.com", "https://plus.unsplash.com"],
        credits_a_person: true,
    },
    Spec {
        id: SourceId::Pexels,
        name: PEXELS.name,
        slug: "pexels",
        key: Some(KeyKind::Pexels),
        apis: &[PEXELS],
        link_prefixes: &["https://www.pexels.com/"],
        preview_hosts: &["https://images.pexels.com"],
        credits_a_person: true,
    },
    Spec {
        id: SourceId::Wallhaven,
        name: WALLHAVEN.name,
        slug: "wallhaven",
        key: None,
        apis: &[WALLHAVEN],
        link_prefixes: &["https://wallhaven.cc/"],
        preview_hosts: &["https://th.wallhaven.cc"],
        // Search results don't name the uploader.
        credits_a_person: false,
    },
    Spec {
        id: SourceId::Daily,
        name: "Photo of the day",
        slug: "daily",
        key: None,
        apis: &[BING, WIKIMEDIA],
        link_prefixes: &["https://www.bing.com/", "https://commons.wikimedia.org/"],
        preview_hosts: &[
            "https://www.bing.com",
            "https://thumb.wikimedia.org",
            "https://upload.wikimedia.org",
        ],
        credits_a_person: true,
    },
    Spec {
        id: SourceId::Favorites,
        name: "Favorites",
        slug: "favorites",
        key: None,
        apis: &[],
        link_prefixes: &[],
        // Previews are the cached files, which the CSP allows as `asset:`.
        preview_hosts: &[],
        // The credit comes from the file name.
        credits_a_person: false,
    },
];

/// Everything a source may need to make a batch. Each takes what it uses.
pub struct Request<'a> {
    pub http: &'a Http,
    pub keys: &'a Keys,
    pub query: &'a str,
    /// Unsplash topics, already resolved to IDs (the worker caches them).
    pub topic_ids: &'a [String],
    pub collections: &'a [String],
    pub username: &'a str,
    /// The display size, the smallest photo Wallhaven should offer.
    pub size: (u32, u32),
    /// Unix seconds, for the picture of the day.
    pub today: u64,
    pub favorites_dir: &'a Path,
    /// The number of result pages Pexels last reported, if known.
    pub pexels_pages: Option<u64>,
    /// A random number, to pick a Pexels page.
    pub random: u64,
}

/// What one request to a source gave.
pub struct Batch {
    pub candidates: Vec<Candidate>,
    /// The Pexels page count to remember: the request's, or the one a
    /// Pexels search just reported.
    pub pexels_pages: Option<u64>,
}

/// Asks source `id` for a batch of photos.
pub fn batch(id: SourceId, r: &Request<'_>) -> Result<Batch> {
    let mut pexels_pages = r.pexels_pages;
    let candidates = match id {
        SourceId::Unsplash => unsplash::batch(
            r.http,
            &r.keys.unsplash,
            r.query,
            r.topic_ids,
            r.collections,
            r.username,
        )?,
        SourceId::Pexels => {
            let (photos, pages) =
                pexels::batch(r.http, &r.keys.pexels, r.query, r.pexels_pages, r.random)?;
            pexels_pages = Some(pages);
            photos
        }
        SourceId::Wallhaven => wallhaven::batch(r.http, r.query, r.size.0, r.size.1)?,
        SourceId::Daily => daily::batch(r.http, r.today)?,
        SourceId::Favorites => favorites::batch(r.favorites_dir)?,
    };
    Ok(Batch {
        candidates,
        pexels_pages,
    })
}

/// `id`'s row in [`SPECS`].
pub fn spec(id: SourceId) -> &'static Spec {
    // SPECS is in SourceId::ALL order, which a test checks.
    &SPECS[id as usize]
}

/// Every source's APIs, in table order.
pub fn apis() -> impl Iterator<Item = &'static Api> {
    SPECS.iter().flat_map(|s| s.apis)
}

/// The API called `name`.
pub fn api(name: &str) -> Option<&'static Api> {
    apis().find(|a| a.name == name)
}

/// True for a photo or credit link on one of the sources' sites: the only
/// links the popover may open.
pub fn is_source_link(url: &str) -> bool {
    SPECS
        .iter()
        .flat_map(|s| s.link_prefixes)
        .any(|prefix| url.starts_with(prefix))
}

/// Whether a credit for `provider` (a [`WallpaperView`]'s `source`, such as
/// "Unsplash" or "Bing") names a person. Only the sources that never do
/// say no.
///
/// [`WallpaperView`]: crate::service::WallpaperView
pub fn credits_a_person(provider: &str) -> bool {
    !SPECS
        .iter()
        .any(|s| s.name == provider && !s.credits_a_person)
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
    fit_rgb8(photo, width, height, |rgb| write_jpeg(rgb, dest))
}

/// Hands `use_it` the photo as RGB8 at `width`x`height`: scaled to cover that
/// size and cropped from the middle, unless it has that size already. An RGB8
/// photo is moved, not copied. A resized original is freed before `use_it`
/// runs, so the decoded image is not held while the result is used.
pub(crate) fn fit_rgb8<T>(
    photo: DynamicImage,
    width: u32,
    height: u32,
    use_it: impl FnOnce(&image::RgbImage) -> T,
) -> T {
    let rgb = if (photo.width(), photo.height()) == (width, height) {
        photo.into_rgb8()
    } else {
        let fitted = photo.resize_to_fill(width, height, FilterType::CatmullRom);
        drop(photo);
        fitted.into_rgb8()
    };
    use_it(&rgb)
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
    use crate::alloc_probe::{large_allocations, live, unwatch, watch};
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
    fn the_table_has_one_row_per_source_in_order() {
        assert_eq!(SPECS.map(|s| s.id), SourceId::ALL);
        for id in SourceId::ALL {
            assert_eq!(spec(id).id, id);
            // The file-name key is the config's spelling.
            let config = serde_json::to_value(id).unwrap();
            assert_eq!(config.as_str(), Some(id.key()));
        }
    }

    #[test]
    fn api_names_are_unique_and_found_by_name() {
        let names: Vec<&str> = apis().map(|a| a.name).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        for name in names {
            assert_eq!(api(name).map(|a| a.name), Some(name));
        }
        assert!(api("Photo of the day").is_none());
        assert!(apis().all(|a| a.base.starts_with("https://") && !a.base.ends_with('/')));
    }

    #[test]
    fn links_and_previews_are_https_origins() {
        for s in &SPECS {
            for prefix in s.link_prefixes {
                // A trailing slash, so "https://unsplash.com.evil" is refused.
                assert!(prefix.starts_with("https://") && prefix.ends_with('/'));
            }
            for host in s.preview_hosts {
                assert!(host.starts_with("https://"));
                assert_eq!(host.matches('/').count(), 2, "{host} is an origin");
            }
        }
    }

    #[test]
    fn only_source_sites_can_be_opened() {
        assert!(is_source_link("https://unsplash.com/photos/x?utm_source=a"));
        assert!(is_source_link(
            "https://commons.wikimedia.org/wiki/File:A.jpg"
        ));
        assert!(!is_source_link("https://unsplash.com.example.com/"));
        assert!(!is_source_link("http://unsplash.com/photos/x"));
        assert!(!is_source_link("https://example.com/"));
        assert!(!is_source_link("file:///etc/passwd"));
    }

    #[test]
    fn credits_name_a_person_except_for_wallhaven_and_favorites() {
        for provider in ["Unsplash", "Pexels", "Bing", "Wikimedia Commons"] {
            assert!(credits_a_person(provider), "{provider}");
        }
        assert!(!credits_a_person(SourceId::Wallhaven.name()));
        assert!(!credits_a_person(SourceId::Favorites.name()));
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

    #[test]
    fn an_rgb8_photo_is_used_in_place() {
        let (w, h) = (160, 90);
        watch(w as usize * h as usize * 3);
        let photo =
            DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| Rgb([x as u8, y as u8, 7])));
        let mut live_while_used = 0;
        fit_rgb8(photo, w, h, |_| live_while_used = live());
        unwatch();
        // Moved, not copied: the photo's buffer is the only one alive.
        assert_eq!(live_while_used, 1);
    }

    #[test]
    fn a_resized_photo_is_freed_before_it_is_used() {
        let (w, h) = (640, 400);
        watch(w as usize * h as usize * 3);
        let photo =
            DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| Rgb([x as u8, y as u8, 7])));
        let mut live_while_used = 0;
        fit_rgb8(photo, 160, 90, |_| live_while_used = live());
        unwatch();
        // The decoded original is gone by the time the resized image is used.
        assert_eq!(live_while_used, 0);
    }
}
