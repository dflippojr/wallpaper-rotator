use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// User settings, stored as TOML in the platform config folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Unsplash access key. `UNSPLASH_ACCESS_KEY` in the environment wins.
    pub access_key: String,
    /// Search text, such as "mountains". Unsplash can't combine a query with
    /// topics, so a non-empty query takes precedence.
    pub query: String,
    /// Unsplash topic slugs or IDs, such as "wallpapers" or "nature".
    pub topics: Vec<String>,
    /// Minutes between rotations. 0 means only rotate on request.
    pub interval_minutes: u64,
    pub paused: bool,
    /// Hard cap for the image cache in megabytes.
    pub cache_cap_mb: u64,
    /// Where "Save to favorites" copies photos. Defaults to
    /// `<Pictures>/Wallpaper Favorites`.
    pub favorites_dir: Option<PathBuf>,
    pub start_at_login: bool,
    /// A different photo on each display, each sized for it.
    pub per_display: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            access_key: String::new(),
            query: String::new(),
            topics: vec!["wallpapers".into()],
            interval_minutes: 24 * 60,
            paused: false,
            cache_cap_mb: 200,
            favorites_dir: None,
            start_at_login: true,
            per_display: false,
        }
    }
}

impl Config {
    /// Loads the config, writing the defaults on first run.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("could not parse {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                config.save(path)?;
                Ok(config)
            }
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        crate::store::write_atomic(path, text.as_bytes())
    }

    pub fn effective_key(&self) -> String {
        std::env::var("UNSPLASH_ACCESS_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| self.access_key.clone())
            .trim()
            .to_string()
    }

    pub fn cache_cap_bytes(&self) -> u64 {
        self.cache_cap_mb.max(20) * 1024 * 1024
    }

    /// The settings that shape fetched photos; prefetched photos are dropped
    /// when these change.
    pub fn same_source(&self, other: &Config) -> bool {
        self.query.trim() == other.query.trim()
            && self.topics == other.topics
            && self.per_display == other.per_display
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_load_writes_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let config = Config::load_or_create(&path).unwrap();
        assert_eq!(config, Config::default());
        assert!(path.exists());
        assert_eq!(Config::load_or_create(&path).unwrap(), config);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "query = \"mountains\"\ninterval_minutes = 60\n").unwrap();
        let config = Config::load_or_create(&path).unwrap();
        assert_eq!(config.query, "mountains");
        assert_eq!(config.interval_minutes, 60);
        assert_eq!(config.cache_cap_mb, 200);
    }

    #[test]
    fn cache_cap_has_a_floor() {
        let config = Config {
            cache_cap_mb: 0,
            ..Config::default()
        };
        assert_eq!(config.cache_cap_bytes(), 20 * 1024 * 1024);
    }
}
