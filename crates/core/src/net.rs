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

    pub fn get_json<T: DeserializeOwned>(
        &self,
        name: &'static str,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<T> {
        self.get(name, url, headers)?
            .body_mut()
            .read_json::<T>()
            .map_err(|e| anyhow!("{name} sent a reply this app could not read: {e}"))
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
        other => anyhow!("could not reach {name}: {other}"),
    }
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
    fn encodes_queries() {
        assert_eq!(
            encode("snowy mountains & lakes"),
            "snowy%20mountains%20%26%20lakes"
        );
        assert_eq!(encode("a,b"), "a%2Cb");
    }

    #[test]
    fn part_files_keep_the_extension() {
        assert_eq!(part_path(Path::new("/x/a.jpg")), Path::new("/x/a.jpg.part"));
    }
}
