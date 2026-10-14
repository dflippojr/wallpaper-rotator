//! Unsplash. API terms this follows: hotlink Unsplash image URLs for
//! display, call the photo's `download_location` when it becomes a
//! wallpaper, and credit the photographer with links back to Unsplash.

use anyhow::{bail, Result};
use serde::Deserialize;

use super::{Candidate, Image, SourceId};
use crate::net::{encode, Http};

const API: &str = "https://api.unsplash.com";
/// Appended to Unsplash links, as the API guidelines ask.
pub const UTM: &str = "utm_source=wallpaper_rotator&utm_medium=referral";
/// The most `/photos/random` returns at once.
pub const BATCH: u32 = 30;

#[derive(Debug, Clone, Deserialize)]
struct Photo {
    id: String,
    width: u32,
    height: u32,
    color: Option<String>,
    description: Option<String>,
    alt_description: Option<String>,
    urls: Urls,
    links: Links,
    user: User,
    #[serde(default)]
    location: Option<Location>,
}

#[derive(Debug, Clone, Deserialize)]
struct Urls {
    raw: String,
    small: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Links {
    html: String,
    download_location: String,
}

#[derive(Debug, Clone, Deserialize)]
struct User {
    name: String,
    links: UserLinks,
}

#[derive(Debug, Clone, Deserialize)]
struct UserLinks {
    html: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Location {
    name: Option<String>,
    city: Option<String>,
    country: Option<String>,
}

impl Location {
    fn label(&self) -> Option<String> {
        if let Some(name) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
            return Some(name.trim().to_string());
        }
        let parts: Vec<&str> = [self.city.as_deref(), self.country.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

#[derive(Deserialize)]
struct Topic {
    id: String,
}

fn auth(key: &str) -> String {
    format!("Client-ID {}", key.trim())
}

fn get_json<T: serde::de::DeserializeOwned>(http: &Http, key: &str, url: &str) -> Result<T> {
    if key.trim().is_empty() {
        bail!("add your Unsplash access key in Settings");
    }
    http.get_json(
        "Unsplash",
        url,
        &[("Authorization", &auth(key)), ("Accept-Version", "v1")],
    )
}

/// Random landscape photos for a search query, or else from topics.
pub fn batch(http: &Http, key: &str, query: &str, topic_ids: &[String]) -> Result<Vec<Candidate>> {
    let mut url =
        format!("{API}/photos/random?orientation=landscape&content_filter=high&count={BATCH}");
    let query = query.trim();
    if !query.is_empty() {
        url.push_str("&query=");
        url.push_str(&encode(query));
    } else if !topic_ids.is_empty() {
        url.push_str("&topics=");
        url.push_str(&encode(&topic_ids.join(",")));
    }
    let photos: Vec<Photo> = get_json(http, key, &url)?;
    Ok(photos.into_iter().map(candidate).collect())
}

/// Resolves a topic slug (or ID) to its ID.
pub fn topic_id(http: &Http, key: &str, slug_or_id: &str) -> Result<String> {
    let url = format!("{API}/topics/{}", encode(slug_or_id.trim()));
    let topic: Topic = get_json(http, key, &url)
        .map_err(|e| e.context(format!("unknown Unsplash topic \"{slug_or_id}\"")))?;
    Ok(topic.id)
}

/// Tells Unsplash the photo was used, as the API terms require.
pub fn track(http: &Http, key: &str, url: &str) -> Result<()> {
    http.get(
        "Unsplash",
        url,
        &[("Authorization", &auth(key)), ("Accept-Version", "v1")],
    )?;
    Ok(())
}

fn candidate(photo: Photo) -> Candidate {
    Candidate {
        source: SourceId::Unsplash,
        id: photo.id,
        width: photo.width,
        height: photo.height,
        author: photo.user.name,
        author_url: with_utm(&photo.user.links.html),
        photo_url: with_utm(&photo.links.html),
        preview_url: photo.urls.small,
        location: photo.location.and_then(|l| l.label()),
        description: photo.description.or(photo.alt_description),
        color: photo.color,
        image: Image::Sized {
            url: sized_url(&photo.urls.raw),
        },
        track_url: Some(photo.links.download_location),
    }
}

/// An imgix URL template that crops the photo to exactly `{w}`x`{h}`, so
/// the app downloads a screen-sized JPEG instead of the huge original.
fn sized_url(raw: &str) -> String {
    let sep = if raw.contains('?') { '&' } else { '?' };
    format!("{raw}{sep}w={{w}}&h={{h}}&fit=crop&crop=entropy&fm=jpg&q=85")
}

pub fn with_utm(url: &str) -> String {
    if url.is_empty() || url.contains("utm_source=") {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{UTM}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
        "id": "abc123", "width": 6000, "height": 4000, "color": "#a0a0a0",
        "description": null, "alt_description": "a lake at sunset",
        "urls": {"raw": "https://images.unsplash.com/photo-1?ixid=x&ixlib=rb-4.0.3",
                 "full": "f", "regular": "r", "small": "https://images.unsplash.com/photo-1?w=400", "thumb": "t"},
        "links": {"html": "https://unsplash.com/photos/abc123",
                  "download_location": "https://api.unsplash.com/photos/abc123/download?ixid=x"},
        "user": {"name": "Mario Dobelmann", "links": {"html": "https://unsplash.com/@mario"}},
        "location": {"name": null, "city": "Kirchsee", "country": "Germany"}
    }"##;

    #[test]
    fn turns_photos_into_candidates() {
        let photos: Vec<Photo> = serde_json::from_str(&format!("[{SAMPLE}]")).unwrap();
        let c = candidate(photos[0].clone());
        assert_eq!(c.id, "abc123");
        assert_eq!(c.author, "Mario Dobelmann");
        assert_eq!(c.location.as_deref(), Some("Kirchsee, Germany"));
        assert_eq!(c.description.as_deref(), Some("a lake at sunset"));
        assert!(c.photo_url.ends_with(UTM));
        assert!(c.track_url.unwrap().contains("/download"));
        let Image::Sized { url } = c.image else {
            panic!()
        };
        assert_eq!(
            url,
            "https://images.unsplash.com/photo-1?ixid=x&ixlib=rb-4.0.3&w={w}&h={h}&fit=crop&crop=entropy&fm=jpg&q=85"
        );
    }

    #[test]
    fn location_is_optional() {
        let without = SAMPLE.replace(
            r#""location": {"name": null, "city": "Kirchsee", "country": "Germany"}"#,
            r#""location": null"#,
        );
        let photo: Photo = serde_json::from_str(&without).unwrap();
        assert!(photo.location.is_none());
        let empty = Location {
            name: Some(" ".into()),
            city: None,
            country: Some("".into()),
        };
        assert_eq!(empty.label(), None);
    }

    #[test]
    fn utm_is_added_once() {
        let once = with_utm("https://unsplash.com/@a");
        assert_eq!(with_utm(&once), once);
        assert_eq!(with_utm(""), "");
    }

    #[test]
    fn needs_a_key() {
        assert!(batch(&Http::default(), "  ", "", &[]).is_err());
    }
}
