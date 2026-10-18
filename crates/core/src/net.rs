//! HTTP shared by every photo source.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::de::DeserializeOwned;

/// Where each source's API lives, keyed by the name sources pass to
/// [`Http::get`].
const BASES: [(&str, &str); 5] = [
    ("Unsplash", "https://api.unsplash.com"),
    ("Pexels", "https://api.pexels.com"),
    ("Wallhaven", "https://wallhaven.cc"),
    ("Bing", "https://www.bing.com"),
    ("Wikimedia", "https://api.wikimedia.org"),
];
const MAX_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
/// Wikimedia asks API clients to say who they are.
const USER_AGENT: &str = concat!(
    "wallpaper-rotator/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/dflippojr/wallpaper-rotator)"
);

/// A source turned the app away for asking too often. The worker uses
/// another source until it cools down.
#[derive(Debug)]
pub struct RateLimited(pub &'static str);

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} rate limit reached", self.0)
    }
}

impl std::error::Error for RateLimited {}

pub fn is_rate_limited(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<RateLimited>().is_some())
}

#[derive(Clone)]
pub struct Http {
    agent: ureq::Agent,
    /// Sources sent somewhere other than their real API (a test's stub).
    bases: HashMap<&'static str, String>,
}

impl Default for Http {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent(USER_AGENT)
            .build()
            .new_agent();
        Self {
            agent,
            bases: HashMap::new(),
        }
    }
}

impl Http {
    /// Sends the named sources' requests to other base URLs, such as
    /// `http://127.0.0.1:PORT` for a local stub. Others keep their real API.
    pub fn with_bases(bases: HashMap<&'static str, String>) -> Self {
        let bases = bases
            .into_iter()
            .map(|(name, url)| (name, url.trim_end_matches('/').to_string()))
            .collect();
        Self {
            bases,
            ..Self::default()
        }
    }

    /// The base URL of `name`'s API, without a trailing slash.
    pub fn base(&self, name: &str) -> &str {
        self.bases.get(name).map(String::as_str).unwrap_or_else(|| {
            BASES
                .iter()
                .find(|(n, _)| *n == name)
                .map_or("", |(_, url)| url)
        })
    }

    /// GETs `url` with extra headers. `name` labels errors ("Pexels ...").
    pub fn get(
        &self,
        name: &'static str,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let mut request = self.agent.get(url);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        request.call().map_err(|e| explain(name, e))
    }

    /// Sends `call` and returns the reply's text; an error status becomes
    /// an error saying what it means for the app.
    pub fn send(&self, call: &Call) -> Result<String> {
        let headers: Vec<(&str, &str)> =
            call.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.get(call.api, &call.url, &headers)?
            .body_mut()
            .read_to_string()
            .map_err(|e| {
                anyhow!(
                    "could not read {}'s reply: {}",
                    call.api,
                    redact(&e.to_string())
                )
            })
    }

    /// Sends `call` and returns the status and the reply whatever the
    /// status, for tools that report it themselves (the fixture recorder
    /// and the live check).
    pub fn send_raw(&self, call: &Call) -> Result<(u16, Vec<u8>)> {
        let mut request = self
            .agent
            .get(&call.url)
            .config()
            .http_status_as_error(false)
            .build();
        for (key, value) in &call.headers {
            request = request.header(*key, value);
        }
        let response = request
            .call()
            .map_err(|e| anyhow!("could not reach {}: {}", call.api, redact(&e.to_string())))?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut body)?;
        if body.len() as u64 > MAX_IMAGE_BYTES {
            bail!(
                "{}'s reply is larger than {} MB",
                call.api,
                MAX_IMAGE_BYTES / 1024 / 1024
            );
        }
        Ok((status, body))
    }

    /// Downloads to `dest` through a `.part` file, so a failed download
    /// never leaves a half-written image behind.
    pub fn download(&self, name: &'static str, url: &str, dest: &Path) -> Result<u64> {
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir)?;
        }
        let part = part_path(dest);
        let result = (|| -> Result<u64> {
            let response = self.get(name, url, &[])?;
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

/// One API request as a source builds it. The app, the fixture recorder
/// and the live check all send these, so they ask exactly the same thing.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The source's name in [`BASES`]; labels errors.
    pub api: &'static str,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

impl Call {
    pub fn new(api: &'static str, url: String) -> Self {
        Self {
            api,
            url,
            headers: Vec::new(),
        }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

/// Reads a source's JSON reply. The error names the field that didn't
/// match, so API drift is easy to spot. It can quote the reply, so it is
/// [`redact`]ed.
pub fn parse<T: DeserializeOwned>(api: &str, body: &str) -> Result<T> {
    serde_json::from_str(body).map_err(|e| {
        anyhow!(
            "{api} sent a reply this app could not read: {}",
            redact(&e.to_string())
        )
    })
}

/// `photo.jpg` -> `photo.jpg.part`, which cleanup knows to delete.
pub fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

pub(crate) fn explain(name: &'static str, err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::StatusCode(401) => anyhow!("{name} rejected the key"),
        // Unsplash answers 403 when the hourly limit is used up.
        ureq::Error::StatusCode(403) if name == "Unsplash" => RateLimited(name).into(),
        ureq::Error::StatusCode(429) => RateLimited(name).into(),
        ureq::Error::StatusCode(404) => anyhow::Error::new(ureq::Error::StatusCode(404))
            .context(format!("{name} found nothing for that search")),
        ureq::Error::StatusCode(code) => anyhow!("{name} returned HTTP {code}"),
        other => anyhow!("could not reach {name}: {}", redact(&other.to_string())),
    }
}

/// What a credential is replaced with in errors and reports.
pub const REDACTED: &str = "[redacted]";
/// Query parameters whose values are credentials.
const SECRET_PARAMS: [&str; 5] = [
    "client_id=",
    "access_key=",
    "api_key=",
    "apikey=",
    "access_token=",
];
/// Where a query parameter's or a token's value ends.
const VALUE_ENDS: &[u8] = b"&#\"'<>()[]{},; \t\r\n\\";

/// `text` with every credential in it replaced by [`REDACTED`]: the values
/// of credential query parameters (`client_id=`, `api_key=`, ...), of
/// `Authorization` headers and of `Client-ID` tokens. Errors quote URLs
/// and replies, so every error this module builds goes through here.
pub fn redact(text: &str) -> String {
    // ASCII lowercasing keeps byte offsets, so matches index `text` too.
    let lower = text.to_ascii_lowercase();
    let mut secrets = Vec::new();
    let mut values_after = |marker: &str, separators: &[u8], ends: &[u8]| {
        for (at, _) in lower.match_indices(marker) {
            let after = at + marker.len();
            let start = after
                + text.as_bytes()[after..]
                    .iter()
                    .take_while(|b| separators.contains(b))
                    .count();
            if !separators.is_empty() && start == after {
                continue; // "Authorizations", not a header
            }
            let end = text.as_bytes()[start..]
                .iter()
                .position(|b| ends.contains(b))
                .map_or(text.len(), |n| start + n);
            if end > start {
                secrets.push((start, end));
            }
        }
    };
    for param in SECRET_PARAMS {
        values_after(param, b"", VALUE_ENDS);
    }
    values_after("client-id ", b"", VALUE_ENDS);
    // A header's value runs to its closing quote or the end of the line.
    values_after("authorization", b"\"':=, \t\\", b"\"'\r\n&<>,;\\");

    secrets.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    for (start, end) in secrets {
        if start >= done {
            out.push_str(&text[done..start]);
            out.push_str(REDACTED);
        }
        done = done.max(end);
    }
    out.push_str(&text[done..]);
    out
}

/// `text` with each of `keys`, in every form [`key_forms`] gives, and any
/// other credential [`redact`] finds replaced by [`REDACTED`]. A reply the
/// parser rejects can quote any value back, a key too, with no marker
/// around it, so whatever the app logs or shows goes through here.
pub fn scrub(text: &str, keys: &[&str]) -> String {
    let mut text = text.to_string();
    for key in keys.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
        for form in key_forms(key) {
            text = text.replace(&form, REDACTED);
        }
    }
    redact(&text)
}

/// A key as is, percent-encoded and JSON-escaped.
pub fn key_forms(key: &str) -> [String; 3] {
    let json = serde_json::to_string(key).unwrap_or_default();
    [
        key.to_string(),
        encode(key),
        json.trim_matches('"').to_string(),
    ]
}

/// Percent-encodes a URL query component.
pub fn encode(s: &str) -> String {
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

    #[test]
    fn rate_limits_are_recognised_through_context() {
        let e = anyhow::Error::from(RateLimited("Pexels")).context("fetching photos");
        assert!(is_rate_limited(&e));
        assert!(!is_rate_limited(&anyhow!("offline")));
        assert!(is_rate_limited(&explain(
            "Unsplash",
            ureq::Error::StatusCode(403)
        )));
        assert!(!is_rate_limited(&explain(
            "Pexels",
            ureq::Error::StatusCode(403)
        )));
    }

    #[test]
    fn bases_default_to_the_real_apis() {
        let stub = Http::with_bases(HashMap::from([("Pexels", "http://127.0.0.1:1/".into())]));
        assert_eq!(stub.base("Pexels"), "http://127.0.0.1:1");
        assert_eq!(stub.base("Unsplash"), "https://api.unsplash.com");
        assert_eq!(Http::default().base("Pexels"), "https://api.pexels.com");
    }

    #[test]
    fn redacts_credentials() {
        let cases = [
            (
                "GET https://x/p?client_id=s3cret&page=2 failed",
                "GET https://x/p?client_id=[redacted]&page=2 failed",
            ),
            ("?API_KEY=s3cret", "?API_KEY=[redacted]"),
            (
                "apikey=s3cret access_key=s3cret",
                "apikey=[redacted] access_key=[redacted]",
            ),
            (
                "Authorization: Client-ID s3cret\nnext",
                "Authorization: [redacted]\nnext",
            ),
            (
                r#"{"Authorization":"s3cret"}"#,
                r#"{"Authorization":"[redacted]"}"#,
            ),
            (
                r#"("Authorization", "s3cret")"#,
                r#"("Authorization", "[redacted]")"#,
            ),
            (
                "token Client-ID s3cret, then",
                "token Client-ID [redacted], then",
            ),
            ("no Authorizations here", "no Authorizations here"),
            ("missing field `urlbase`", "missing field `urlbase`"),
        ];
        for (text, redacted) in cases {
            assert_eq!(redact(text), redacted);
            assert_eq!(
                redact(redacted),
                redacted,
                "redacting twice changes nothing"
            );
        }
        let error = parse::<Vec<u32>>("Unsplash", r#"["?client_id=s3cret"]"#).unwrap_err();
        assert!(!error.to_string().contains("s3cret"), "{error}");
    }

    #[test]
    fn scrubs_bare_keys() {
        let error = parse::<Vec<u32>>("Unsplash", r#"["FAKE-key/0"]"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("FAKE-key/0"), "no marker, so not redacted");
        assert_eq!(
            scrub(&error, &["", " FAKE-key/0 "]),
            error.replace("FAKE-key/0", REDACTED)
        );
        assert_eq!(
            scrub("q=FAKE-key%2F0 client_id=x", &["FAKE-key/0"]),
            "q=[redacted] client_id=[redacted]"
        );
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(
            encode("snowy mountains & lakes"),
            "snowy%20mountains%20%26%20lakes"
        );
        assert_eq!(encode("a,b"), "a%2Cb");
    }
}
