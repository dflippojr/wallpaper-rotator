//! Wallhaven. No key needed for safe-for-work results; about 45 requests a
//! minute. Images are user uploads with unclear copyright, so this source
//! is for personal use. The search is limited to the "general" category
//! (no anime or people) and SFW purity.

use anyhow::Result;
use serde::Deserialize;

use super::{Candidate, Image, SourceId};
use crate::net::{encode, parse, Call, Http};

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
    parse_search(&http.send(&search_call(http, query, min_width, min_height))?)
}

pub fn search_call(http: &Http, query: &str, min_width: u32, min_height: u32) -> Call {
    let mut url = format!(
        "{}/api/v1/search?categories=100&purity=100&sorting=random&ratios=landscape&atleast={min_width}x{min_height}",
        http.base("Wallhaven")
    );
    let query = query.trim();
    if !query.is_empty() {
        url.push_str("&q=");
        url.push_str(&encode(query));
    }
    Call::new("Wallhaven", url)
}

pub fn parse_search(body: &str) -> Result<Vec<Candidate>> {
    let page: Page = parse("Wallhaven", body)?;
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

    const SAMPLE: &str = include_str!("../../tests/fixtures/sources/wallhaven/search.json");

    #[test]
    fn parses_search_results() {
        let page: Page = serde_json::from_str(SAMPLE).unwrap();
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
