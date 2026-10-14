//! Wallhaven. No key needed for safe-for-work results; about 45 requests a
//! minute. Images are user uploads with unclear copyright, so this source
//! is for personal use. The search is limited to the "general" category
//! (no anime or people) and SFW purity.

use anyhow::Result;
use serde::Deserialize;

use super::{Candidate, Image, SourceId};
use crate::net::{encode, Http};

const API: &str = "https://wallhaven.cc/api/v1/search";

#[derive(Deserialize)]
struct Page {
    data: Vec<Wallpaper>,
}

#[derive(Deserialize)]
struct Wallpaper {
    id: String,
    url: String,
    dimension_x: u32,
    dimension_y: u32,
    path: String,
    #[serde(default)]
    colors: Vec<String>,
    thumbs: Thumbs,
}

#[derive(Deserialize)]
struct Thumbs {
    large: String,
}

/// A random page (24) of SFW, general, landscape wallpapers at least
/// `min_width`x`min_height`, matching `query` if given.
pub fn batch(http: &Http, query: &str, min_width: u32, min_height: u32) -> Result<Vec<Candidate>> {
    let mut url = format!(
        "{API}?categories=100&purity=100&sorting=random&ratios=landscape&atleast={min_width}x{min_height}"
    );
    let query = query.trim();
    if !query.is_empty() {
        url.push_str("&q=");
        url.push_str(&encode(query));
    }
    let page: Page = http.get_json("Wallhaven", &url, &[])?;
    Ok(page.data.into_iter().map(candidate).collect())
}

fn candidate(w: Wallpaper) -> Candidate {
    Candidate {
        source: SourceId::Wallhaven,
        id: w.id,
        width: w.dimension_x,
        height: w.dimension_y,
        // Search results don't name the uploader.
        author: "Wallhaven".into(),
        author_url: w.url.clone(),
        photo_url: w.url,
        preview_url: w.thumbs.large,
        location: None,
        description: None,
        color: w.colors.into_iter().next(),
        image: Image::Full { url: w.path },
        track_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_results() {
        let sample = r##"{"data": [{"id": "d5wlmj", "url": "https://wallhaven.cc/w/d5wlmj",
            "resolution": "5184x3456", "dimension_x": 5184, "dimension_y": 3456,
            "path": "https://w.wallhaven.cc/full/d5/wallhaven-d5wlmj.jpg", "file_type": "image/jpeg",
            "colors": ["#999999", "#abbcda"], "category": "general", "purity": "sfw",
            "thumbs": {"large": "https://th.wallhaven.cc/lg/d5/d5wlmj.jpg", "original": "o", "small": "s"}}],
            "meta": {"current_page": 1, "last_page": 212, "per_page": 24, "total": 5068, "seed": "ONmLxh"}}"##;
        let page: Page = serde_json::from_str(sample).unwrap();
        let c = candidate(page.data.into_iter().next().unwrap());
        assert_eq!((c.width, c.height), (5184, 3456));
        assert_eq!(c.color.as_deref(), Some("#999999"));
        assert_eq!(
            c.image,
            Image::Full {
                url: "https://w.wallhaven.cc/full/d5/wallhaven-d5wlmj.jpg".into()
            }
        );
    }
}
