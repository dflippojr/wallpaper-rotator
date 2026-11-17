//! What the UI sees of the worker: the snapshot it publishes, the photos it
//! lists, and Settings as the UI edits them.

use anyhow::Result;
use serde::Serialize;

use crate::config::{Config, MIN_CACHE_MB};
use crate::source::{unsplash, SourceId};
use crate::store::{Set, Wallpaper};

/// What the UI shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    /// One photo for every display, or one per display.
    pub current: Vec<WallpaperView>,
    pub busy: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// No ticked source can deliver photos (for example, only Unsplash is
    /// ticked and it has no key), so the popover opens Settings.
    pub needs_setup: bool,
    pub has_unsplash_key: bool,
    pub has_pexels_key: bool,
    pub settings: Settings,
    /// Set when a photographer is saved but Search or Collections wins.
    pub photographer_note: Option<String>,
    pub next_rotation_at: Option<u64>,
    pub favorites_dir: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WallpaperView {
    pub id: String,
    pub author: String,
    pub author_url: String,
    pub photo_url: String,
    pub preview_url: String,
    /// The cached file, for previews of photos with no hosted preview
    /// (favorites).
    pub preview_file: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    /// The display it is on; `None` when one photo covers every display.
    pub display: Option<String>,
    /// Where it came from, for the credit line ("Unsplash", "Bing").
    pub source: String,
}

/// The source stores its credit links (Unsplash tags them with UTM when
/// it builds the candidate), so the view shows them as they are.
impl From<&Wallpaper> for WallpaperView {
    fn from(w: &Wallpaper) -> Self {
        Self {
            id: w.id.clone(),
            author: w.author.clone(),
            author_url: w.author_url.clone(),
            photo_url: w.photo_url.clone(),
            preview_url: w.preview_url.clone(),
            preview_file: w
                .preview_url
                .is_empty()
                .then(|| w.file.display().to_string()),
            location: w.location.clone(),
            description: w.description.clone(),
            color: w.color.clone(),
            display: w.display.as_ref().map(|d| d.name.clone()),
            source: provider(w.source, &w.id).into(),
        }
    }
}
/// The name to credit: picture-of-the-day photos come from Bing or Commons.
fn provider(source: SourceId, id: &str) -> &'static str {
    match source {
        SourceId::Daily if id.starts_with("bing-") => "Bing",
        SourceId::Daily => "Wikimedia Commons",
        other => other.name(),
    }
}
/// Settings as the UI edits them. Keys are write-only from the UI's side.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Settings {
    pub query: String,
    pub topics: String,
    #[serde(default)]
    pub collections: String,
    #[serde(default)]
    pub username: String,
    pub interval_minutes: u64,
    pub paused: bool,
    pub cache_cap_mb: u64,
    pub start_at_login: bool,
    #[serde(default)]
    pub per_display: bool,
    #[serde(default)]
    pub sources: Vec<SourceId>,
}

impl Settings {
    pub fn from_config(c: &Config) -> Self {
        Self {
            query: c.query.clone(),
            topics: c.topics.join(", "),
            collections: c.collections.join(", "),
            username: c.username.clone(),
            interval_minutes: c.interval_minutes,
            paused: c.paused,
            cache_cap_mb: c.cache_cap_mb,
            start_at_login: c.start_at_login,
            per_display: c.per_display,
            sources: c.sources.clone(),
        }
    }

    /// Applies UI settings to a config. An empty key keeps the old key.
    pub fn apply_to(
        &self,
        config: &mut Config,
        unsplash_key: Option<&str>,
        pexels_key: Option<&str>,
    ) -> Result<()> {
        let collections = unsplash::parse_collections(&self.collections)?;
        let username = unsplash::parse_username(&self.username)?;
        config.collections = collections;
        config.username = username;
        config.query = self.query.trim().to_string();
        config.topics = self
            .topics
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        config.interval_minutes = self.interval_minutes;
        config.paused = self.paused;
        config.cache_cap_mb = self.cache_cap_mb.max(MIN_CACHE_MB);
        config.start_at_login = self.start_at_login;
        config.per_display = self.per_display;
        config.sources = SourceId::ALL
            .into_iter()
            .filter(|s| self.sources.contains(s))
            .collect();
        let new = |key: Option<&str>| {
            key.map(str::trim)
                .filter(|k| !k.is_empty())
                .map(String::from)
        };
        if let Some(key) = new(unsplash_key) {
            config.access_key = key;
        }
        if let Some(key) = new(pexels_key) {
            config.pexels_key = key;
        }
        Ok(())
    }
}
/// "Unsplash is rate-limited, so this one is from Pexels."
pub(super) fn fallback_notice(notes: &[String], set: &Set) -> String {
    let mut from: Vec<&str> = set
        .photos
        .iter()
        .map(|w| provider(w.source, &w.id))
        .collect();
    from.dedup();
    let these = if set.photos.len() > 1 {
        "these are"
    } else {
        "this one is"
    };
    format!(
        "{}, so {these} from {}",
        notes.join(" and "),
        from.join(" and ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::test_support::stored;
    use std::path::PathBuf;

    /// Characterization for #51: the links a view shows for photos whose
    /// source already stored them (Unsplash tagged, Pexels as given).
    #[test]
    fn view_shows_the_links_the_source_stored() {
        let tagged = format!("https://unsplash.com/@ann?{}", unsplash::UTM);
        let view = WallpaperView::from(&stored(SourceId::Unsplash, &tagged, &tagged));
        assert!(view.author_url.contains("utm_source="));
        assert_eq!(view.photo_url, tagged);

        let pexels = "https://www.pexels.com/@ann";
        let view = WallpaperView::from(&stored(SourceId::Pexels, pexels, pexels));
        assert_eq!(view.author_url, pexels);
        assert_eq!(view.photo_url, pexels);
    }

    /// #51 option (a): the source owns the UTM rule, so the view shows an
    /// untagged link as stored. Fails on the base branch, which tags it.
    #[test]
    fn view_does_not_retag_a_link_the_source_stored_untagged() {
        let plain = "https://unsplash.com/photos/x";
        let view = WallpaperView::from(&stored(SourceId::Unsplash, plain, plain));
        assert_eq!(view.author_url, plain);
        assert_eq!(view.photo_url, plain);
    }

    #[test]
    fn invalid_source_settings_do_not_change_config() {
        let mut config = Config::default();
        let original = config.clone();
        let mut settings = Settings::from_config(&config);
        settings.query = "new search".into();
        settings.collections = "https://example.com/123".into();
        assert!(settings.apply_to(&mut config, None, None).is_err());
        assert_eq!(config, original);
        settings.collections = "123".into();
        settings.username = "@".into();
        assert!(settings.apply_to(&mut config, None, None).is_err());
        assert_eq!(config, original);
    }

    #[test]
    fn settings_round_trip() {
        let mut config = Config {
            access_key: "old".into(),
            ..Config::default()
        };
        let mut settings = Settings::from_config(&config);
        assert_eq!(settings.topics, "wallpapers");
        settings.topics = " nature, ,film ".into();
        settings.cache_cap_mb = 1;
        settings.per_display = true;
        settings.sources = vec![SourceId::Favorites, SourceId::Pexels, SourceId::Pexels];
        settings.collections = "https://unsplash.com/collections/123/lakes, abc_12".into();
        settings.username = "https://unsplash.com/@ann".into();
        settings
            .apply_to(&mut config, Some("  "), Some("px"))
            .unwrap();
        assert_eq!(config.collections, ["123", "abc_12"]);
        assert_eq!(config.username, "ann");
        let round_trip = Settings::from_config(&config);
        assert_eq!(round_trip.collections, "123, abc_12");
        assert_eq!(round_trip.username, "ann");
        assert_eq!(config.topics, ["nature", "film"]);
        assert_eq!(config.cache_cap_mb, 20);
        assert_eq!(config.access_key, "old");
        assert_eq!(config.pexels_key, "px");
        assert!(config.per_display);
        assert_eq!(config.sources, [SourceId::Pexels, SourceId::Favorites]);
        settings.apply_to(&mut config, Some("new"), None).unwrap();
        assert_eq!(config.access_key, "new");
        assert_eq!(config.pexels_key, "px");
    }

    #[test]
    fn notices_name_the_fallback() {
        let set = Set::single(Wallpaper {
            id: "bing-20261014-x".into(),
            file: PathBuf::new(),
            author: String::new(),
            author_url: String::new(),
            photo_url: String::new(),
            preview_url: String::new(),
            location: None,
            description: None,
            download_location: String::new(),
            color: None,
            source: SourceId::Daily,
            display: None,
        });
        assert_eq!(
            fallback_notice(&["Unsplash is rate-limited".into()], &set),
            "Unsplash is rate-limited, so this one is from Bing"
        );
    }
}
