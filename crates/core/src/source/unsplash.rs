//! Unsplash. API terms this follows: hotlink Unsplash image URLs for
//! display, call the photo's `download_location` when it becomes a
//! wallpaper, and credit the photographer with links back to Unsplash.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::{Candidate, Image, SourceId};
use crate::net::{encode, parse, Call, Http};

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

fn call(key: &str, url: String) -> Call {
    Call::new("Unsplash", url)
        .header("Authorization", auth(key))
        .header("Accept-Version", "v1")
}

fn send(http: &Http, call: &Call, key: &str) -> Result<String> {
    if key.trim().is_empty() {
        bail!("add your Unsplash access key in Settings");
    }
    http.send(call)
}

/// What random photos come from. The API ignores collections and topics
/// when a username is sent, so a request carries exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Search,
    Collections,
    Photographer,
    /// Topics, or any photo when there are none.
    Topics,
}

/// Search wins over collections, collections over a photographer, and a
/// photographer over topics.
pub fn selection(query: &str, collections: &[String], username: &str) -> Selection {
    if !query.trim().is_empty() {
        Selection::Search
    } else if !collections.is_empty() {
        Selection::Collections
    } else if !username.trim().is_empty() {
        Selection::Photographer
    } else {
        Selection::Topics
    }
}

/// Why a photographer is set but not used, for the settings screen.
pub fn photographer_ignored(query: &str, collections: &[String], username: &str) -> Option<String> {
    if username.trim().is_empty() {
        return None;
    }
    let winner = match selection(query, collections, username) {
        Selection::Search => "Search",
        Selection::Collections => "Collections",
        Selection::Photographer | Selection::Topics => return None,
    };
    Some(format!("Photographer is ignored while {winner} is set."))
}

/// Random landscape photos from whichever selection wins (see `selection`).
pub fn batch(
    http: &Http,
    key: &str,
    query: &str,
    topic_ids: &[String],
    collections: &[String],
    username: &str,
) -> Result<Vec<Candidate>> {
    let call = batch_call(http, key, query, topic_ids, collections, username);
    let body = send(http, &call, key)
        .map_err(|e| batch_error(e, selection(query, collections, username)))?;
    parse_batch(&body)
}

pub fn batch_call(
    http: &Http,
    key: &str,
    query: &str,
    topic_ids: &[String],
    collections: &[String],
    username: &str,
) -> Call {
    let url = batch_url(
        http.base("Unsplash"),
        query,
        topic_ids,
        collections,
        username,
    );
    call(key, url)
}

pub fn parse_batch(body: &str) -> Result<Vec<Candidate>> {
    let photos: Vec<Photo> = parse("Unsplash", body)?;
    Ok(photos.into_iter().map(candidate).collect())
}

fn batch_error(error: anyhow::Error, selection: Selection) -> anyhow::Error {
    if !matches!(
        error.downcast_ref::<ureq::Error>(),
        Some(ureq::Error::StatusCode(404))
    ) {
        return error;
    }
    match selection {
        Selection::Collections => anyhow::anyhow!("Unsplash couldn't find that collection"),
        Selection::Photographer => anyhow::anyhow!("Unsplash couldn't find that photographer"),
        Selection::Search | Selection::Topics => error,
    }
}

fn batch_url(
    api: &str,
    query: &str,
    topic_ids: &[String],
    collections: &[String],
    username: &str,
) -> String {
    let mut url =
        format!("{api}/photos/random?orientation=landscape&content_filter=high&count={BATCH}");
    let (name, value) = match selection(query, collections, username) {
        Selection::Search => ("query", query.trim().to_string()),
        Selection::Collections => ("collections", collections.join(",")),
        Selection::Photographer => ("username", username.trim().to_string()),
        Selection::Topics if topic_ids.is_empty() => return url,
        Selection::Topics => ("topics", topic_ids.join(",")),
    };
    url.push_str(&format!("&{name}={}", encode(&value)));
    url
}

/// Normalize IDs and pasted public collection URLs at the settings boundary.
pub fn parse_collections(text: &str) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for value in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        let id = if let Some(path) = unsplash_path(value) {
            path.strip_prefix("collections/")
                .and_then(|p| p.split('/').next())
                .context("paste an Unsplash collection URL or collection ID")?
        } else {
            value
        };
        if !identifier(id) {
            bail!("paste an Unsplash collection URL or collection ID");
        }
        if !ids.iter().any(|s| s == id) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

/// Accept a username, @username, or an Unsplash profile URL.
pub fn parse_username(text: &str) -> Result<String> {
    let value = text.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let username = unsplash_path(value)
        .unwrap_or(value)
        .trim_end_matches('/')
        .trim_start_matches('@');
    if !identifier(username) {
        bail!("paste an Unsplash profile URL or @username");
    }
    Ok(username.to_string())
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn unsplash_path(value: &str) -> Option<&str> {
    let value = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value);
    let path = value
        .strip_prefix("unsplash.com/")
        .or_else(|| value.strip_prefix("www.unsplash.com/"))?;
    path.split(['?', '#']).next()
}

/// Resolves a topic slug (or ID) to its ID.
pub fn topic_id(http: &Http, key: &str, slug_or_id: &str) -> Result<String> {
    send(http, &topic_call(http, key, slug_or_id), key)
        .and_then(|body| parse_topic(&body))
        .map_err(|e| e.context(format!("unknown Unsplash topic \"{slug_or_id}\"")))
}

pub fn topic_call(http: &Http, key: &str, slug_or_id: &str) -> Call {
    let url = format!(
        "{}/topics/{}",
        http.base("Unsplash"),
        encode(slug_or_id.trim())
    );
    call(key, url)
}

pub fn parse_topic(body: &str) -> Result<String> {
    let topic: Topic = parse("Unsplash", body)?;
    Ok(topic.id)
}

/// Tells Unsplash the photo was used, as the API terms require.
pub fn track(http: &Http, key: &str, url: &str) -> Result<()> {
    http.send(&track_call(key, url))?;
    Ok(())
}

/// The download report for a photo's `download_location`.
pub fn track_call(key: &str, url: &str) -> Call {
    call(key, url.to_string())
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

    /// A `/photos/random` reply; the worker journeys serve it too.
    const SAMPLE: &str = include_str!("../../tests/fixtures/sources/unsplash/random.json");

    #[test]
    fn turns_photos_into_candidates() {
        let photos: Vec<Photo> = serde_json::from_str(SAMPLE).unwrap();
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
        let photos: Vec<Photo> = serde_json::from_str(&without).unwrap();
        assert!(photos[0].location.is_none());
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
    fn collection_ids_and_urls() {
        assert_eq!(parse_collections(" 123, https://unsplash.com/collections/abc_-12/lakes?utm_source=x#top, 123\nwww.unsplash.com/collections/456/ ").unwrap(), ["123", "abc_-12", "456"]);
        assert!(parse_collections(" , ").unwrap().is_empty());
        for invalid in [
            "https://example.com/collections/123",
            "https://unsplash.com/collections/",
            "https://unsplash.com/@ann",
            "bad/id",
        ] {
            assert!(parse_collections(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn photographer_names_and_profile_urls() {
        for input in [
            "ann_lee",
            " @ann_lee ",
            "https://unsplash.com/@ann_lee/",
            "https://www.unsplash.com/ann_lee?utm_source=x",
        ] {
            assert_eq!(parse_username(input).unwrap(), "ann_lee");
        }
        assert_eq!(parse_username(" ").unwrap(), "");
        for invalid in [
            "@",
            "https://example.com/@ann",
            "ann lee",
            "https://unsplash.com/@ann/likes",
        ] {
            assert!(parse_username(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn random_photo_selection_precedence() {
        let topics: Vec<String> = vec!["topic1".into(), "topic2".into()];
        let collections: Vec<String> = vec!["123".into(), "abc_12".into()];
        let none: &[String] = &[];
        // Search > collections > photographer > topics; only the winner is sent.
        for (query, collections, username, topics, expected, filter) in [
            (
                " lake & sky ",
                &collections[..],
                "ann",
                &topics[..],
                Selection::Search,
                "&query=lake%20%26%20sky",
            ),
            (
                " ",
                &collections[..],
                "ann",
                &topics[..],
                Selection::Collections,
                "&collections=123%2Cabc_12",
            ),
            (
                "",
                none,
                " ann ",
                &topics[..],
                Selection::Photographer,
                "&username=ann",
            ),
            (
                "",
                none,
                " ",
                &topics[..],
                Selection::Topics,
                "&topics=topic1%2Ctopic2",
            ),
            ("", none, "", none, Selection::Topics, ""),
        ] {
            assert_eq!(selection(query, collections, username), expected);
            assert_eq!(
                batch_url("https://api", query, topics, collections, username),
                format!("https://api/photos/random?orientation=landscape&content_filter=high&count=30{filter}")
            );
        }
    }

    #[test]
    fn says_when_the_photographer_is_ignored() {
        let collections = vec!["123".to_string()];
        assert_eq!(
            photographer_ignored("lake", &collections, "ann").as_deref(),
            Some("Photographer is ignored while Search is set.")
        );
        assert_eq!(
            photographer_ignored(" ", &collections, "ann").as_deref(),
            Some("Photographer is ignored while Collections is set.")
        );
        assert_eq!(photographer_ignored("", &[], "ann"), None);
        assert_eq!(photographer_ignored("lake", &collections, " "), None);
    }

    #[test]
    fn missing_collection_or_photographer_has_a_specific_error() {
        let missing = || crate::net::explain("Unsplash", ureq::Error::StatusCode(404));
        assert_eq!(
            batch_error(missing(), Selection::Collections).to_string(),
            "Unsplash couldn't find that collection"
        );
        assert_eq!(
            batch_error(missing(), Selection::Photographer).to_string(),
            "Unsplash couldn't find that photographer"
        );
        assert_eq!(
            batch_error(missing(), Selection::Search).to_string(),
            "Unsplash found nothing for that search"
        );
        let limited = crate::net::explain("Unsplash", ureq::Error::StatusCode(403));
        assert!(crate::net::is_rate_limited(&batch_error(
            limited,
            Selection::Collections
        )));
    }

    #[test]
    fn needs_a_key() {
        assert!(batch(&Http::default(), "  ", "", &[], &[], "").is_err());
    }
}
