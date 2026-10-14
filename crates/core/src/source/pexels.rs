//! Pexels. Free key, about 200 requests an hour. It has no random endpoint,
//! so a batch is a random page of search results (or curated photos).
//! Credit the photographer and Pexels with links.

use anyhow::{bail, Result};
use serde::Deserialize;

use super::{Candidate, Image, SourceId};
use crate::net::{encode, Http};

const API: &str = "https://api.pexels.com/v1";
pub const BATCH: u32 = 40;
/// Pexels serves search results this deep at most.
const MAX_PAGES: u64 = 100;

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    total_results: u64,
    photos: Vec<Photo>,
}

#[derive(Deserialize)]
struct Photo {
    id: u64,
    width: u32,
    height: u32,
    url: String,
    photographer: String,
    photographer_url: String,
    avg_color: Option<String>,
    alt: Option<String>,
    src: Src,
}

#[derive(Deserialize)]
struct Src {
    original: String,
    medium: String,
}

fn get(http: &Http, key: &str, url: &str) -> Result<Page> {
    if key.trim().is_empty() {
        bail!("add your Pexels API key in Settings");
    }
    http.get_json("Pexels", url, &[("Authorization", key.trim())])
}

/// A random page of landscape photos for `query`, or of curated photos.
/// `pages` is the number of result pages the last search reported, so the
/// pick can range over all of them; `None` asks for page 1 to learn it.
/// Returns the photos and the page count for next time.
pub fn batch(
    http: &Http,
    key: &str,
    query: &str,
    pages: Option<u64>,
    random: u64,
) -> Result<(Vec<Candidate>, u64)> {
    let page = pages.map_or(1, |n| 1 + random % n.clamp(1, MAX_PAGES));
    let query = query.trim();
    let url = if query.is_empty() {
        format!("{API}/curated?per_page={BATCH}&page={page}")
    } else {
        format!(
            "{API}/search?query={}&orientation=landscape&size=large&per_page={BATCH}&page={page}",
            encode(query)
        )
    };
    let result = get(http, key, &url)?;
    let pages = result.total_results.div_ceil(u64::from(BATCH)).max(1);
    let photos: Vec<Candidate> = result
        .photos
        .into_iter()
        // Curated photos include portraits; search asked for landscape.
        .filter(|p| p.width >= p.height)
        .map(candidate)
        .collect();
    Ok((photos, pages))
}

fn candidate(photo: Photo) -> Candidate {
    let sep = if photo.src.original.contains('?') {
        '&'
    } else {
        '?'
    };
    Candidate {
        source: SourceId::Pexels,
        id: photo.id.to_string(),
        width: photo.width,
        height: photo.height,
        author: photo.photographer,
        author_url: photo.photographer_url,
        photo_url: photo.url,
        preview_url: photo.src.medium,
        location: None,
        description: photo.alt.filter(|a| !a.trim().is_empty()),
        color: photo.avg_color,
        image: Image::Sized {
            url: format!(
                "{}{sep}auto=compress&cs=tinysrgb&fit=crop&w={{w}}&h={{h}}",
                photo.src.original
            ),
        },
        track_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{"page": 1, "per_page": 40, "total_results": 81, "photos": [
        {"id": 417074, "width": 5184, "height": 3456, "url": "https://www.pexels.com/photo/lake-417074/",
         "photographer": "Pixabay", "photographer_url": "https://www.pexels.com/@pixabay",
         "photographer_id": 2659, "avg_color": "#6C7C88", "liked": false, "alt": "Lake and mountains",
         "src": {"original": "https://images.pexels.com/photos/417074/pexels-photo-417074.jpeg",
                 "large2x": "x", "large": "x", "medium": "https://images.pexels.com/photos/417074/m.jpeg",
                 "small": "x", "portrait": "x", "landscape": "x", "tiny": "x"}},
        {"id": 2, "width": 3000, "height": 4500, "url": "u", "photographer": "P", "photographer_url": "p",
         "avg_color": null, "alt": "", "src": {"original": "o", "medium": "m"}}
    ]}"##;

    #[test]
    fn parses_a_page_and_drops_portraits() {
        let page: Page = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(page.total_results.div_ceil(u64::from(BATCH)), 3);
        let photos: Vec<Candidate> = page
            .photos
            .into_iter()
            .filter(|p| p.width >= p.height)
            .map(candidate)
            .collect();
        assert_eq!(photos.len(), 1);
        let c = &photos[0];
        assert_eq!((c.id.as_str(), c.author.as_str()), ("417074", "Pixabay"));
        assert_eq!(c.description.as_deref(), Some("Lake and mountains"));
        let Image::Sized { url } = &c.image else {
            panic!()
        };
        assert_eq!(
            url,
            "https://images.pexels.com/photos/417074/pexels-photo-417074.jpeg?auto=compress&cs=tinysrgb&fit=crop&w={w}&h={h}"
        );
    }

    #[test]
    fn needs_a_key() {
        assert!(batch(&Http::default(), "", "x", None, 0).is_err());
    }
}
