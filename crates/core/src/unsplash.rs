//! Minimal Unsplash API client.
//!
//! API terms this follows: hotlink Unsplash image URLs for display, call the
//! photo's `download_location` when it becomes a wallpaper, and credit the
//! photographer with links back to Unsplash.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

const API: &str = "https://api.unsplash.com";
/// Appended to Unsplash links, as the API guidelines ask.
pub const UTM: &str = "utm_source=wallpaper_rotator&utm_medium=referral";
const MAX_IMAGE_BYTES: u64 = 80 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Photo {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub color: Option<String>,
    pub description: Option<String>,
    pub alt_description: Option<String>,
    pub urls: Urls,
    pub links: Links,
    pub user: User,
    #[serde(default)]
    pub location: Option<Location>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Urls {
    pub raw: String,
    pub small: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Links {
    pub html: String,
    pub download_location: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub name: String,
    pub links: UserLinks,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserLinks {
    pub html: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Location {
    pub name: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
}

impl Location {
    pub fn label(&self) -> Option<String> {
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

pub struct Client {
    agent: ureq::Agent,
    key: String,
}

impl Client {
    pub fn new(key: &str) -> Result<Self> {
        let key = key.trim();
        if key.is_empty() {
            bail!("Add your Unsplash access key in Settings");
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent("wallpaper-rotator/0.1")
            .build()
            .new_agent();
        Ok(Self {
            agent,
            key: key.to_string(),
        })
    }

    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
        self.agent
            .get(url)
            .header("Authorization", &format!("Client-ID {}", self.key))
            .header("Accept-Version", "v1")
            .call()
            .map_err(explain)
    }

    /// Up to `count` random landscape photos (at most 30) for a search query,
    /// or else from topics. One API request however many come back.
    pub fn random_photos(
        &self,
        query: &str,
        topic_ids: &[String],
        count: u32,
    ) -> Result<Vec<Photo>> {
        let count = count.clamp(1, 30);
        let mut url =
            format!("{API}/photos/random?orientation=landscape&content_filter=high&count={count}");
        let query = query.trim();
        if !query.is_empty() {
            url.push_str("&query=");
            url.push_str(&encode_component(query));
        } else if !topic_ids.is_empty() {
            url.push_str("&topics=");
            url.push_str(&encode_component(&topic_ids.join(",")));
        }
        let photos = self
            .get(&url)?
            .body_mut()
            .read_json::<Vec<Photo>>()
            .context("Unsplash sent photos this app could not read")?;
        if photos.is_empty() {
            bail!("Unsplash found nothing for that search or topic");
        }
        Ok(photos)
    }

    /// Resolves a topic slug (or ID) to its ID.
    pub fn topic_id(&self, slug_or_id: &str) -> Result<String> {
        let url = format!("{API}/topics/{}", encode_component(slug_or_id.trim()));
        let topic: Topic = self
            .get(&url)
            .with_context(|| format!("unknown Unsplash topic \"{slug_or_id}\""))?
            .body_mut()
            .read_json()?;
        Ok(topic.id)
    }

    /// Tells Unsplash the photo was used, as the API terms require.
    pub fn track_download(&self, download_location: &str) -> Result<()> {
        self.get(download_location)?;
        Ok(())
    }

    /// Downloads an image to `dest` through a `.part` file, so a failed
    /// download never leaves a half-written wallpaper behind.
    pub fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir)?;
        }
        let part = dest.with_extension("jpg.part");
        let result = (|| -> Result<u64> {
            let response = self.agent.get(url).call().map_err(explain)?;
            let mut reader = response.into_body().into_reader().take(MAX_IMAGE_BYTES + 1);
            let mut file = File::create(&part)?;
            let written = io::copy(&mut reader, &mut file)?;
            if written > MAX_IMAGE_BYTES {
                bail!("image is larger than {} MB", MAX_IMAGE_BYTES / 1024 / 1024);
            }
            file.sync_all()?;
            Ok(written)
        })();
        match result {
            Ok(written) => {
                fs::rename(&part, dest)?;
                Ok(written)
            }
            Err(e) => {
                let _ = fs::remove_file(&part);
                Err(e)
            }
        }
    }
}

fn explain(err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::StatusCode(401) => anyhow!("Unsplash rejected the access key"),
        ureq::Error::StatusCode(403) => {
            anyhow!("Unsplash rate limit reached; trying again later")
        }
        ureq::Error::StatusCode(404) => anyhow!("Unsplash found nothing for that search or topic"),
        ureq::Error::StatusCode(code) => anyhow!("Unsplash returned HTTP {code}"),
        other => anyhow!("could not reach Unsplash: {other}"),
    }
}

/// An imgix URL that crops the photo to exactly `width` x `height`, so the
/// app downloads a screen-sized JPEG instead of the multi-megabyte original.
pub fn sized_url(raw: &str, width: u32, height: u32) -> String {
    let sep = if raw.contains('?') { '&' } else { '?' };
    format!("{raw}{sep}w={width}&h={height}&fit=crop&crop=entropy&fm=jpg&q=85")
}

pub fn with_utm(url: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{UTM}")
}

fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
    fn parses_a_random_photo() {
        let photo: Photo = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(photo.id, "abc123");
        assert_eq!(photo.user.name, "Mario Dobelmann");
        assert_eq!(
            photo.location.unwrap().label().as_deref(),
            Some("Kirchsee, Germany")
        );
    }

    #[test]
    fn parses_a_batch() {
        let photos: Vec<Photo> = serde_json::from_str(&format!("[{SAMPLE},{SAMPLE}]")).unwrap();
        assert_eq!(photos.len(), 2);
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
    fn sizes_raw_urls() {
        assert_eq!(
            sized_url("https://images.unsplash.com/p?ixid=x", 3024, 1964),
            "https://images.unsplash.com/p?ixid=x&w=3024&h=1964&fit=crop&crop=entropy&fm=jpg&q=85"
        );
        assert!(sized_url("https://x/p", 1, 1).starts_with("https://x/p?w=1"));
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(
            encode_component("snowy mountains & lakes"),
            "snowy%20mountains%20%26%20lakes"
        );
        assert_eq!(encode_component("a,b"), "a%2Cb");
    }

    #[test]
    fn empty_key_is_rejected() {
        assert!(Client::new("  ").is_err());
    }
}
