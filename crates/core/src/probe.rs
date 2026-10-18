//! The photo sources against their real APIs: the fixture recorder
//! (`record-fixtures`) and the nightly live check (`live-sources`). Both
//! build each request and parse each reply with the sources' own code, so
//! a reply that passes here is one the app can use.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::config::Config;
use crate::net::{encode, redact, Call, Http, REDACTED};
use crate::source::{daily, image_url, pexels, unsplash, wallhaven, Candidate};

/// The sources the live check calls, by their API names.
pub const SOURCES: [&str; 5] = ["Unsplash", "Pexels", "Wallhaven", "Bing", "Wikimedia"];
/// Photos kept in a recorded list, so fixtures stay small.
const KEEP: usize = 4;
/// Fields about the account behind the key, not the photo. Removed from
/// recordings.
const ACCOUNT_FIELDS: [&str; 4] = [
    "bookmarked",
    "current_user_collections",
    "liked_by_user",
    "liked",
];

/// The keys for the keyed sources; empty when not set.
#[derive(Debug, Clone, Default)]
pub struct Keys {
    pub unsplash: String,
    pub pexels: String,
}

impl Keys {
    /// The keys the app would use: `UNSPLASH_ACCESS_KEY` and
    /// `PEXELS_API_KEY`, else the ones saved in `config` (never written).
    pub fn load(config: Option<&Path>) -> Self {
        let saved = config
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str::<Config>(&text).ok())
            .unwrap_or_default();
        Self {
            unsplash: saved.effective_key(),
            pexels: saved.effective_pexels_key(),
        }
    }

    fn for_source(&self, source: &str) -> Option<&str> {
        match source {
            "Unsplash" => Some(&self.unsplash),
            "Pexels" => Some(&self.pexels),
            _ => None,
        }
    }

    fn all(&self) -> impl Iterator<Item = &str> {
        [self.unsplash.trim(), self.pexels.trim()]
            .into_iter()
            .filter(|k| !k.is_empty())
    }

    /// `text` with every key, in each form [`contains_key`] finds, and any
    /// other credential [`redact`] finds replaced by [`REDACTED`]. Every
    /// error and report the recorder and the live check print goes
    /// through here.
    pub fn scrub(&self, text: &str) -> String {
        let mut text = text.to_string();
        for key in self.all() {
            for form in key_forms(key) {
                text = text.replace(&form, REDACTED);
            }
        }
        redact(&text)
    }

    fn scrub_error(&self, error: anyhow::Error) -> anyhow::Error {
        anyhow!(self.scrub(&format!("{error:#}")))
    }
}

/// A key as is, percent-encoded and JSON-escaped.
fn key_forms(key: &str) -> [String; 3] {
    let json = serde_json::to_string(key).unwrap_or_default();
    [
        key.to_string(),
        encode(key),
        json.trim_matches('"').to_string(),
    ]
}

/// True when `text` holds any of the keys, as is, percent-encoded or
/// JSON-escaped.
pub fn contains_key(text: &str, keys: &Keys) -> bool {
    keys.all()
        .any(|key| key_forms(key).iter().any(|form| text.contains(form)))
}

// ---- Recorder ----

/// What part of a reply a recording keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Keep {
    /// The whole reply.
    All,
    /// The first [`KEEP`] items of the top-level list.
    List,
    /// The first [`KEEP`] items of this field's list.
    ListIn(&'static str),
    /// Only this field.
    Field(&'static str),
}

/// A recorded reply the tests read, and the request that records it.
pub struct Fixture {
    /// Under `tests/fixtures/sources/`.
    pub path: &'static str,
    /// Tried in order until one gives a photo (Wikimedia's picture of the
    /// day can be a portrait or an SVG).
    calls: Vec<Call>,
    keep: Keep,
    check: Counter,
}

/// The app's parser for a reply; how many photos (or topics) it gave.
type Counter = Box<dyn Fn(&str) -> Result<usize>>;

/// Every fixture the keys allow, and the keyed sources skipped for lack
/// of a key. `today` is Unix seconds, for the picture of the day.
pub fn fixtures(http: &Http, keys: &Keys, today: u64) -> (Vec<Fixture>, Vec<&'static str>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    let key = &keys.unsplash;
    if key.is_empty() {
        skipped.push("Unsplash");
    } else {
        let photos = |path, query| Fixture {
            path,
            calls: vec![unsplash::batch_call(http, key, query, &[], &[], "")],
            keep: Keep::List,
            check: Box::new(|body| Ok(unsplash::parse_batch(body)?.len())),
        };
        out.push(photos("unsplash/random.json", ""));
        out.push(photos("unsplash/search.json", "mountains"));
        out.push(Fixture {
            path: "unsplash/topic.json",
            calls: vec![unsplash::topic_call(http, key, "wallpapers")],
            keep: Keep::All,
            check: Box::new(|body| unsplash::parse_topic(body).map(|_| 1)),
        });
    }
    let key = &keys.pexels;
    if key.is_empty() {
        skipped.push("Pexels");
    } else {
        let photos = |path, query| Fixture {
            path,
            calls: vec![pexels::batch_call(http, key, query, 1)],
            keep: Keep::ListIn("photos"),
            check: Box::new(|body| Ok(pexels::parse_batch(body)?.0.len())),
        };
        out.push(photos("pexels/curated.json", ""));
        out.push(photos("pexels/search.json", "mountains"));
    }
    out.push(Fixture {
        path: "wallhaven/search.json",
        calls: vec![wallhaven::search_call(http, "", 1920, 1080)],
        keep: Keep::ListIn("data"),
        check: Box::new(|body| Ok(wallhaven::parse_search(body)?.len())),
    });
    let bing = http.base("Bing").to_string();
    out.push(Fixture {
        path: "daily/bing.json",
        calls: vec![daily::bing_call(http)],
        keep: Keep::ListIn("images"),
        check: Box::new(move |body| Ok(daily::parse_bing(&bing, body)?.len())),
    });
    out.push(Fixture {
        path: "daily/wikimedia.json",
        calls: wikimedia_calls(http, today),
        keep: Keep::Field("image"),
        check: Box::new(|body| Ok(daily::parse_wikimedia(body)?.into_iter().count())),
    });
    (out, skipped)
}

/// The days of Wikimedia history the app asks for, newest first.
fn wikimedia_calls(http: &Http, today: u64) -> Vec<Call> {
    (0..daily::WIKIMEDIA_DAYS)
        .map(|back| {
            let (y, m, d) = daily::civil_date(today.saturating_sub(back * 86_400));
            daily::wikimedia_call(http, y, m, d)
        })
        .collect()
}

/// Records every fixture the keys allow whose source is in `only` (all
/// when empty) under `dir`, and the Unsplash download report after its
/// random list. `say` gets one line per fixture written or source
/// skipped. Errors and lines never hold a key.
pub fn record_sources(
    http: &Http,
    keys: &Keys,
    dir: &Path,
    only: &[String],
    today: u64,
    say: &mut dyn FnMut(&str),
) -> Result<()> {
    let wanted = |path: &str| {
        only.is_empty()
            || only.iter().any(|s| {
                path.starts_with(&format!("{s}/")) || path.ends_with(&format!("/{s}.json"))
            })
    };
    let mut say = |line: String| say(&keys.scrub(&line));
    let (fixtures, skipped) = fixtures(http, keys, today);
    for source in skipped {
        if wanted(&format!("{}/", source.to_lowercase())) {
            say(format!("skipped {source}: no key"));
        }
    }
    for fixture in fixtures.iter().filter(|f| wanted(f.path)) {
        let photos = record(http, fixture, keys, dir)?;
        say(format!("wrote {} ({photos})", fixture.path));
        if fixture.path == "unsplash/random.json" {
            let random = fs::read_to_string(dir.join(fixture.path))?;
            let download = unsplash_download(keys, &random)?;
            record(http, &download, keys, dir)?;
            say(format!("wrote {}", download.path));
        }
    }
    Ok(())
}

/// The download report for the first photo in a recorded Unsplash list.
pub fn unsplash_download(keys: &Keys, random: &str) -> Result<Fixture> {
    download_fixture(keys, random).map_err(|e| keys.scrub_error(e))
}

fn download_fixture(keys: &Keys, random: &str) -> Result<Fixture> {
    let photo = unsplash::parse_batch(random)?
        .into_iter()
        .next()
        .context("the Unsplash recording has no photo")?;
    let url = photo
        .track_url
        .context("the photo has no download_location")?;
    Ok(Fixture {
        path: "unsplash/download.json",
        calls: vec![unsplash::track_call(&keys.unsplash, &url)],
        keep: Keep::All,
        check: Box::new(|body| {
            let reply: Value = serde_json::from_str(body)?;
            match reply["url"].as_str() {
                Some(url) if !url.is_empty() => Ok(1),
                _ => bail!("the download report has no url"),
            }
        }),
    })
}

/// Records `fixture` under `dir`: the real reply, trimmed to a few photos
/// and without account fields, checked with the app's parser. Refuses to
/// write anything that holds one of `keys`, and checks for them before
/// parsing, since a parser error can quote the reply. Returns how many
/// photos it has. The error never holds a key.
pub fn record(http: &Http, fixture: &Fixture, keys: &Keys, dir: &Path) -> Result<usize> {
    record_reply(http, fixture, keys, dir).map_err(|e| keys.scrub_error(e))
}

fn record_reply(http: &Http, fixture: &Fixture, keys: &Keys, dir: &Path) -> Result<usize> {
    let refuse = || {
        anyhow!(
            "refusing to record {}: it contains an API key",
            fixture.path
        )
    };
    for call in &fixture.calls {
        let (status, body) = http.send_raw(call)?;
        if status != 200 {
            bail!("{} returned HTTP {status} for {}", call.api, fixture.path);
        }
        let body = String::from_utf8(body).context("the reply is not text")?;
        if contains_key(&body, keys) {
            return Err(refuse());
        }
        let text = trim(&body, fixture.keep)?;
        // Re-encoding can turn an escaped key into a plain one.
        if contains_key(&text, keys) {
            return Err(refuse());
        }
        let count = (fixture.check)(&text)
            .with_context(|| format!("{} no longer parses once trimmed", fixture.path))?;
        if count == 0 {
            continue;
        }
        let path = dir.join(fixture.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text).with_context(|| format!("could not write {}", path.display()))?;
        return Ok(count);
    }
    bail!("no reply for {} had a usable photo", fixture.path)
}

/// Keeps the part of a reply `keep` names, drops account fields, and
/// pretty-prints the rest.
fn trim(body: &str, keep: Keep) -> Result<String> {
    let mut value: Value = serde_json::from_str(body).context("the reply is not JSON")?;
    match keep {
        Keep::All => {}
        Keep::List => shorten(&mut value),
        Keep::ListIn(field) => shorten(&mut value[field]),
        Keep::Field(field) => {
            let kept = value.get(field).cloned().unwrap_or(Value::Null);
            value = serde_json::json!({ field: kept });
        }
    }
    strip_account_fields(&mut value);
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

fn shorten(list: &mut Value) {
    if let Some(items) = list.as_array_mut() {
        items.truncate(KEEP);
    }
}

fn strip_account_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for field in ACCOUNT_FIELDS {
                map.remove(field);
            }
            map.values_mut().for_each(strip_account_fields);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_account_fields),
        _ => {}
    }
}

// ---- Live check ----

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A photo came back with a credit, and its image downloaded and
    /// decoded at least at the requested size.
    Passed {
        photo: String,
        author: String,
        size: (u32, u32),
    },
    Failed(Failure),
    /// Not called, for this reason.
    NotVerified(&'static str),
}

/// What failed, for the issue a failed night opens.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// The step or field that failed: "request", "reply", "author",
    /// "image URL", "image", "image size" or "download report".
    pub field: &'static str,
    /// The HTTP status of the request that failed, when one answered.
    pub status: Option<u16>,
    pub detail: String,
}

impl Failure {
    fn new(field: &'static str, status: Option<u16>, detail: impl Into<String>) -> Self {
        Self {
            field,
            status,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub source: &'static str,
    pub outcome: Outcome,
}

impl Outcome {
    /// The outcome with `keys` scrubbed from every text taken from a reply
    /// or an error, so reports and issues built from it hold no key.
    fn scrubbed(self, keys: &Keys) -> Self {
        match self {
            Outcome::Passed {
                photo,
                author,
                size,
            } => Outcome::Passed {
                photo: keys.scrub(&photo),
                author: keys.scrub(&author),
                size,
            },
            Outcome::Failed(f) => Outcome::Failed(Failure {
                detail: keys.scrub(&f.detail),
                ..f
            }),
            other => other,
        }
    }
}

/// Calls every source once, as the app does, and checks a photo from each
/// downloads at `width`x`height`. Keyed sources without a key are not
/// called. `today` is Unix seconds, for the picture of the day. No key
/// appears in the results.
pub fn live(http: &Http, keys: &Keys, (width, height): (u32, u32), today: u64) -> Vec<Check> {
    SOURCES
        .iter()
        .map(|&source| {
            let outcome = match keys.for_source(source) {
                Some("") => Outcome::NotVerified("no key"),
                key => match check(http, source, key.unwrap_or_default(), width, height, today) {
                    Ok(passed) => passed,
                    Err(failure) => Outcome::Failed(failure),
                },
            };
            Check {
                source,
                outcome: outcome.scrubbed(keys),
            }
        })
        .collect()
}

fn check(
    http: &Http,
    source: &'static str,
    key: &str,
    width: u32,
    height: u32,
    today: u64,
) -> Result<Outcome, Failure> {
    type Parse<'a> = Box<dyn Fn(&str) -> Result<Vec<Candidate>> + 'a>;
    let bing = http.base("Bing");
    let (calls, parse): (Vec<Call>, Parse) = match source {
        "Unsplash" => (
            vec![unsplash::batch_call(http, key, "", &[], &[], "")],
            Box::new(unsplash::parse_batch),
        ),
        "Pexels" => (
            vec![pexels::batch_call(http, key, "", 1)],
            Box::new(|body| Ok(pexels::parse_batch(body)?.0)),
        ),
        "Wallhaven" => (
            vec![wallhaven::search_call(http, "", width, height)],
            Box::new(wallhaven::parse_search),
        ),
        "Bing" => (
            vec![daily::bing_call(http)],
            Box::new(move |body| daily::parse_bing(bing, body)),
        ),
        _ => (
            wikimedia_calls(http, today),
            Box::new(|body| Ok(daily::parse_wikimedia(body)?.into_iter().collect())),
        ),
    };
    let mut photo = None;
    for call in &calls {
        let body = send(http, call, "request")?;
        let photos = parse(&String::from_utf8_lossy(&body))
            .map_err(|e| Failure::new("reply", Some(200), format!("{e:#}")))?;
        if let Some(first) = photos.into_iter().next() {
            photo = Some(first);
            break;
        }
    }
    let photo = photo.ok_or_else(|| Failure::new("reply", Some(200), "no usable photo"))?;
    if photo.author.trim().is_empty() {
        return Err(Failure::new("author", Some(200), "the photo has no credit"));
    }
    let url = image_url(&photo, width, height)
        .filter(|url| url.starts_with("http"))
        .ok_or_else(|| Failure::new("image URL", Some(200), "the photo has no download URL"))?;
    let bytes = send(http, &Call::new(calls[0].api, url.clone()), "image")?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| Failure::new("image", Some(200), format!("{url} did not decode: {e}")))?;
    let size = (img.width(), img.height());
    if size.0 < width || size.1 < height {
        return Err(Failure::new(
            "image size",
            Some(200),
            format!(
                "{url} is {}x{}, smaller than {width}x{height}",
                size.0, size.1
            ),
        ));
    }
    if source == "Unsplash" {
        // The API terms: report the download of a photo the app fetched.
        let report = photo.track_url.clone().unwrap_or_default();
        send(http, &unsplash::track_call(key, &report), "download report")?;
    }
    Ok(Outcome::Passed {
        photo: photo.id,
        author: photo.author,
        size,
    })
}

/// Sends `call`, failing as `field` unless the answer is a 200.
fn send(http: &Http, call: &Call, field: &'static str) -> Result<Vec<u8>, Failure> {
    match http.send_raw(call) {
        Ok((200, body)) => Ok(body),
        Ok((status, _)) => Err(Failure::new(field, Some(status), format!("HTTP {status}"))),
        Err(e) => Err(Failure::new(field, None, format!("{e:#}"))),
    }
}

/// The sources not called for lack of a key, as the job summary words it.
pub fn not_verified(checks: &[Check]) -> Option<String> {
    let names: Vec<&str> = checks
        .iter()
        .filter(|c| matches!(c.outcome, Outcome::NotVerified(_)))
        .map(|c| c.source)
        .collect();
    (!names.is_empty()).then(|| format!("NOT VERIFIED: {} (no key)", names.join(", ")))
}

/// A Markdown report: one row per source, then what wasn't verified.
pub fn summary(checks: &[Check]) -> String {
    let mut out = String::from(
        "## Live source check\n\n| Source | Result | Details |\n| --- | --- | --- |\n",
    );
    for c in checks {
        let (result, details) = match &c.outcome {
            Outcome::Passed {
                photo,
                author,
                size,
            } => (
                "passed".to_string(),
                format!("`{photo}` by {author}, image {}x{}", size.0, size.1),
            ),
            Outcome::Failed(f) => ("**FAILED**".to_string(), failure_text(f)),
            Outcome::NotVerified(why) => ("NOT VERIFIED".to_string(), why.to_string()),
        };
        out.push_str(&format!(
            "| {} | {result} | {} |\n",
            c.source,
            details.replace('|', "\\|")
        ));
    }
    if let Some(line) = not_verified(checks) {
        out.push_str(&format!("\n> [!WARNING]\n> **{line}**\n"));
    }
    out
}

/// The body of the issue a failed night opens, or `None` when nothing
/// failed. Names each source, the HTTP status and the field that failed.
pub fn issue_body(checks: &[Check]) -> Option<String> {
    let failed: Vec<String> = checks
        .iter()
        .filter_map(|c| match &c.outcome {
            Outcome::Failed(f) => Some(format!("- **{}**: {}", c.source, failure_text(f))),
            _ => None,
        })
        .collect();
    if failed.is_empty() {
        return None;
    }
    Some(format!(
        "The nightly live check (`.github/workflows/live-sources.yml`) failed for:\n\n{}\n\n\
         Each line names the source, the HTTP status and the field or step that failed. \
         The app's parser and download code ran against the real API, so the app is likely \
         failing the same way for users.\n",
        failed.join("\n")
    ))
}

fn failure_text(f: &Failure) -> String {
    let status = f
        .status
        .map_or("no HTTP status".to_string(), |s| format!("HTTP {s}"));
    format!("`{}` failed ({status}): {}", f.field, f.detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys {
            unsplash: "uns/plash key".into(),
            pexels: String::new(),
        }
    }

    #[test]
    fn finds_keys_in_any_form() {
        let keys = keys();
        assert!(contains_key(r#"{"a": "uns/plash key"}"#, &keys));
        assert!(contains_key("?client_id=uns%2Fplash%20key", &keys));
        assert!(!contains_key(r#"{"a": "uns plash"}"#, &keys));
        assert!(!contains_key("anything", &Keys::default()));
    }

    #[test]
    fn trims_lists_and_account_fields() {
        let body = r#"{"photos": [{"id": 1, "liked": true}, {"id": 2}, {"id": 3}, {"id": 4}, {"id": 5}],
            "user": {"current_user_collections": [], "name": "Ann"}}"#;
        let text = trim(body, Keep::ListIn("photos")).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["photos"].as_array().unwrap().len(), KEEP);
        assert!(!text.contains("liked") && !text.contains("current_user"));
        assert_eq!(value["user"]["name"], "Ann");
        let only = trim(
            r#"{"image": {"a": 1}, "news": [1, 2]}"#,
            Keep::Field("image"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&only).unwrap(),
            serde_json::json!({"image": {"a": 1}})
        );
        assert!(trim("not json", Keep::All).is_err());
    }

    #[test]
    fn keys_come_from_the_saved_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "access_key = \"saved\"\npexels_key = \"\"\n").unwrap();
        // The environment wins, as in the app; only check what it can't override.
        if std::env::var("UNSPLASH_ACCESS_KEY").is_err() {
            assert_eq!(Keys::load(Some(&path)).unsplash, "saved");
        }
        if std::env::var("PEXELS_API_KEY").is_err() {
            assert_eq!(Keys::load(Some(&path)).pexels, "");
            assert_eq!(Keys::load(None).pexels, "");
        }
    }

    #[test]
    fn reports_say_what_was_not_verified_and_what_failed() {
        let checks = vec![
            Check {
                source: "Unsplash",
                outcome: Outcome::NotVerified("no key"),
            },
            Check {
                source: "Pexels",
                outcome: Outcome::NotVerified("no key"),
            },
            Check {
                source: "Bing",
                outcome: Outcome::Failed(Failure::new(
                    "reply",
                    Some(200),
                    "missing field `urlbase` | at line 1",
                )),
            },
            Check {
                source: "Wallhaven",
                outcome: Outcome::Passed {
                    photo: "x1".into(),
                    author: "Wallhaven".into(),
                    size: (1920, 1080),
                },
            },
        ];
        assert_eq!(
            not_verified(&checks).as_deref(),
            Some("NOT VERIFIED: Unsplash, Pexels (no key)")
        );
        let text = summary(&checks);
        assert!(text.contains("| Bing | **FAILED** | `reply` failed (HTTP 200): missing field `urlbase` \\| at line 1 |"), "{text}");
        assert!(text.contains("| Wallhaven | passed | `x1` by Wallhaven, image 1920x1080 |"));
        assert!(text.contains("**NOT VERIFIED: Unsplash, Pexels (no key)**"));
        let issue = issue_body(&checks).unwrap();
        assert!(issue.contains("- **Bing**: `reply` failed (HTTP 200): missing field `urlbase`"));
        assert!(!issue.contains("Wallhaven"));
        assert_eq!(issue_body(&checks[3..]), None);
        assert_eq!(not_verified(&checks[2..]), None);
    }
}
