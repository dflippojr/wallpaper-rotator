//! Picture-of-the-day feeds:
//!
//! - Bing's daily image: an unofficial endpoint, 4K, licensed stock photos
//!   (Microsoft/Getty), personal use only. Gives the last 8 days.
//! - Wikimedia Commons' Picture of the Day: freely licensed, credit and
//!   license required. One request per day of history.
//!
//! NASA's APOD API is left out: in October 2026 it returned a NASA logo
//! instead of the picture for every day.

use anyhow::{bail, Result};
use serde::Deserialize;

use super::{Candidate, Image, SourceId};
use crate::net::{parse, Call, Http};

/// Days of Wikimedia history to offer (one request each).
pub const WIKIMEDIA_DAYS: u64 = 5;

pub fn batch(http: &Http, today: u64) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    match bing(http) {
        Ok(mut photos) => out.append(&mut photos),
        Err(e) => errors.push(format!("{e:#}")),
    }
    for back in 0..WIKIMEDIA_DAYS {
        let (y, m, d) = civil_date(today.saturating_sub(back * 86_400));
        match wikimedia(http, y, m, d) {
            Ok(Some(photo)) => out.push(photo),
            Ok(None) => {}
            Err(e) => errors.push(format!("{e:#}")),
        }
    }
    if out.is_empty() {
        bail!(
            "no picture of the day was available ({})",
            errors.join("; ")
        );
    }
    Ok(out)
}

// ---- Bing ----

#[derive(Deserialize)]
struct BingArchive {
    images: Vec<BingImage>,
}

#[derive(Deserialize)]
struct BingImage {
    startdate: String,
    urlbase: String,
    copyright: String,
    #[serde(default)]
    copyrightlink: String,
}

fn bing(http: &Http) -> Result<Vec<Candidate>> {
    parse_bing(http.base("Bing"), &http.send(&bing_call(http))?)
}

pub fn bing_call(http: &Http) -> Call {
    let base = http.base("Bing");
    Call::new(
        "Bing",
        format!("{base}/HPImageArchive.aspx?format=js&idx=0&n=8&mkt=en-US"),
    )
}

/// `base` is the Bing API's base URL, which also serves the images.
pub fn parse_bing(base: &str, body: &str) -> Result<Vec<Candidate>> {
    let archive: BingArchive = parse("Bing", body)?;
    Ok(archive
        .images
        .into_iter()
        .map(|img| bing_candidate(base, img))
        .collect())
}

/// `base` is where Bing serves images: the API's own host.
fn bing_candidate(base: &str, img: BingImage) -> Candidate {
    // "Ammonite fossils, Dorset (© J Nemchinova/Getty Images)"
    let (caption, author) = match img.copyright.rsplit_once("(©") {
        Some((caption, rest)) => (
            caption.trim().to_string(),
            rest.trim_end_matches(')').trim().to_string(),
        ),
        None => (img.copyright.trim().to_string(), "Bing".to_string()),
    };
    let name = img
        .urlbase
        .rsplit("OHR.")
        .next()
        .and_then(|s| s.split('_').next())
        .filter(|s| !s.is_empty())
        .unwrap_or(&img.startdate)
        .to_string();
    let link = if img.copyrightlink.starts_with("https://www.bing.com/") {
        img.copyrightlink
    } else {
        "https://www.bing.com/".to_string()
    };
    Candidate {
        source: SourceId::Daily,
        id: format!("bing-{}-{name}", img.startdate),
        width: 3840,
        height: 2160,
        author,
        author_url: link.clone(),
        photo_url: link,
        preview_url: format!("{base}{}_400x240.jpg", img.urlbase),
        location: None,
        description: (!caption.is_empty()).then_some(caption),
        color: None,
        image: Image::Full {
            url: format!("{base}{}_UHD.jpg", img.urlbase),
        },
        track_url: None,
    }
}

// ---- Wikimedia Commons ----

#[derive(Deserialize)]
struct Featured {
    image: Option<Potd>,
}

#[derive(Deserialize)]
struct Potd {
    title: String,
    thumbnail: Sized,
    image: Sized,
    file_page: String,
    #[serde(default)]
    artist: Option<Text>,
    #[serde(default)]
    license: Option<License>,
    #[serde(default)]
    description: Option<Text>,
}

#[derive(Deserialize)]
struct Sized {
    source: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Text {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct License {
    #[serde(rename = "type", default)]
    kind: String,
}

fn wikimedia(http: &Http, y: i64, m: u32, d: u32) -> Result<Option<Candidate>> {
    parse_wikimedia(&http.send(&wikimedia_call(http, y, m, d))?)
}

pub fn wikimedia_call(http: &Http, y: i64, m: u32, d: u32) -> Call {
    let url = format!(
        "{}/feed/v1/wikipedia/en/featured/{y}/{m:02}/{d:02}",
        http.base("Wikimedia")
    );
    Call::new("Wikimedia", url)
}

/// The day's picture, or `None` when there is none or it can't be a
/// wallpaper (a portrait, or not a JPEG or PNG).
pub fn parse_wikimedia(body: &str) -> Result<Option<Candidate>> {
    let featured: Featured = parse("Wikimedia", body)?;
    Ok(featured.image.and_then(wikimedia_candidate))
}

fn wikimedia_candidate(p: Potd) -> Option<Candidate> {
    // Landscape JPEG or PNG only; other formats have odd thumbnail names.
    let original = p.image.source.split('?').next()?.to_string();
    let lower = original.to_ascii_lowercase();
    let usable = [".jpg", ".jpeg", ".png"]
        .iter()
        .any(|ext| lower.ends_with(ext));
    if !usable || p.image.width < p.image.height {
        return None;
    }
    let (prefix, path) = original.split_once("/commons/")?;
    let file = path.rsplit('/').next()?;
    let template = format!("{prefix}/commons/thumb/{path}/{{w}}px-{file}");
    let artist = p
        .artist
        .map(|a| a.text.trim().to_string())
        .filter(|a| !a.is_empty());
    let license = p.license.map(|l| l.kind).filter(|l| !l.is_empty());
    let author = match (artist, license) {
        (Some(a), Some(l)) => format!("{a} ({l})"),
        (Some(a), None) => a,
        (None, Some(l)) => format!("Wikimedia Commons ({l})"),
        (None, None) => "Wikimedia Commons".into(),
    };
    let description = p
        .description
        .map(|t| t.text.trim().to_string())
        .filter(|t| !t.is_empty());
    let id = p.title.trim_start_matches("File:").to_string();
    Some(Candidate {
        source: SourceId::Daily,
        id: format!("commons-{id}"),
        width: p.image.width,
        height: p.image.height,
        author,
        author_url: p.file_page.clone(),
        photo_url: p.file_page,
        preview_url: p.thumbnail.source,
        location: None,
        description,
        color: None,
        image: Image::Thumb { template, original },
        track_url: None,
    })
}

/// Unix seconds to a (year, month, day) in UTC, after Howard Hinnant's
/// `civil_from_days`.
pub fn civil_date(unix: u64) -> (i64, u32, u32) {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BING: &str = include_str!("../../tests/fixtures/sources/daily/bing.json");
    const WIKIMEDIA: &str = include_str!("../../tests/fixtures/sources/daily/wikimedia.json");

    #[test]
    fn dates() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(951_782_400), (2000, 2, 29));
        assert_eq!(civil_date(1_791_980_238), (2026, 10, 14));
    }

    #[test]
    fn bing_credit_and_urls() {
        let photos = parse_bing("https://www.bing.com", BING).unwrap();
        assert_eq!(photos.len(), 4);
        let c = &photos[0];
        assert_eq!(c.id, "bing-20261018-DolbadarnWales");
        assert_eq!(c.author, "joe daniel price/Getty Images");
        assert_eq!(
            c.description.as_deref(),
            Some("Dolbadarn Castle, Llanberis, Eryri National Park (Snowdonia), Wales")
        );
        assert!(c
            .photo_url
            .starts_with("https://www.bing.com/search?q=Eryri"));
        assert_eq!(
            c.image,
            Image::Full {
                url: "https://www.bing.com/th?id=OHR.DolbadarnWales_EN-US6620614213_UHD.jpg".into()
            }
        );
    }

    #[test]
    fn wikimedia_thumbnails_and_filters() {
        let c = parse_wikimedia(WIKIMEDIA).unwrap().unwrap();
        assert_eq!(c.author, "Laitche (CC BY-SA 4.0)");
        assert_eq!((c.width, c.height), (3935, 3263));
        assert_eq!(
            c.image,
            Image::Thumb {
                template: "https://upload.wikimedia.org/wikipedia/commons/thumb/1/1b/Purple_beautyberry%2C_October_2015_-_Stacking.jpg/{w}px-Purple_beautyberry%2C_October_2015_-_Stacking.jpg".into(),
                original: "https://upload.wikimedia.org/wikipedia/commons/1/1b/Purple_beautyberry%2C_October_2015_-_Stacking.jpg".into(),
            }
        );

        // Edge cases the recording doesn't show: a portrait, an SVG, no picture.
        let edited = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut reply: serde_json::Value = serde_json::from_str(WIKIMEDIA).unwrap();
            edit(&mut reply);
            parse_wikimedia(&reply.to_string()).unwrap()
        };
        assert!(edited(&|r| r["image"]["image"]["height"] = 5000.into()).is_none());
        assert!(edited(&|r| r["image"]["image"]["source"] =
            "https://upload.wikimedia.org/wikipedia/commons/a/ab/Map.svg".into())
        .is_none());
        assert!(edited(&|r| r["image"] = serde_json::Value::Null).is_none());
    }
}
