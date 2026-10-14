//! The rotation worker. One thread owns the config, state and cache; the UI
//! talks to it through [`Command`]s and gets [`Snapshot`]s back, so nothing
//! else touches the images folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::paths::Paths;
use crate::store::{self, State, Wallpaper};
use crate::unsplash::{self, Client};
use crate::{log, now};

const TICK: Duration = Duration::from_secs(60);
/// Wait this long after a failed fetch before trying again on a tick.
const RETRY_AFTER_SECS: u64 = 15 * 60;
const FALLBACK_SIZE: (u32, u32) = (3840, 2160);

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Rotate now.
    Next,
    /// Copy the current wallpaper into the favorites folder.
    Favorite,
    /// Re-read the config file (after the settings screen saves it).
    Reload,
    SetPaused(bool),
    /// Check whether a rotation or prefetch is due.
    Tick,
    Shutdown,
}

/// What the UI shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub current: Option<WallpaperView>,
    pub busy: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub has_key: bool,
    pub settings: Settings,
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
    pub location: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
}

impl From<&Wallpaper> for WallpaperView {
    fn from(w: &Wallpaper) -> Self {
        Self {
            id: w.id.clone(),
            author: w.author.clone(),
            author_url: unsplash::with_utm(&w.author_url),
            photo_url: unsplash::with_utm(&w.photo_url),
            preview_url: w.preview_url.clone(),
            location: w.location.clone(),
            description: w.description.clone(),
            color: w.color.clone(),
        }
    }
}

/// Settings as the UI edits them. The key is write-only from the UI's side.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Settings {
    pub query: String,
    pub topics: String,
    pub interval_minutes: u64,
    pub paused: bool,
    pub cache_cap_mb: u64,
    pub start_at_login: bool,
}

impl Settings {
    pub fn from_config(c: &Config) -> Self {
        Self {
            query: c.query.clone(),
            topics: c.topics.join(", "),
            interval_minutes: c.interval_minutes,
            paused: c.paused,
            cache_cap_mb: c.cache_cap_mb,
            start_at_login: c.start_at_login,
        }
    }

    /// Applies UI settings to a config. An empty `new_key` keeps the old key.
    pub fn apply_to(&self, config: &mut Config, new_key: Option<&str>) {
        config.query = self.query.trim().to_string();
        config.topics = self
            .topics
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        config.interval_minutes = self.interval_minutes;
        config.paused = self.paused;
        config.cache_cap_mb = self.cache_cap_mb.max(20);
        config.start_at_login = self.start_at_login;
        if let Some(key) = new_key.map(str::trim).filter(|k| !k.is_empty()) {
            config.access_key = key.to_string();
        }
    }
}

pub type ApplyFn = Box<dyn Fn(&Path) -> Result<()> + Send>;
pub type ScreenSizeFn = Box<dyn Fn() -> Option<(u32, u32)> + Send>;
pub type NotifyFn = Box<dyn Fn(&Snapshot) + Send>;

/// How the worker reaches the platform and the UI.
pub struct Hooks {
    /// Sets the wallpaper. Must hop to the main thread on macOS.
    pub apply: ApplyFn,
    /// Largest screen size in physical pixels, if known.
    pub screen_size: ScreenSizeFn,
    /// Called whenever the snapshot changes.
    pub notify: NotifyFn,
}

pub struct Service {
    paths: Paths,
    config: Config,
    state: State,
    hooks: Hooks,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    last_failed_fetch: u64,
}

impl Service {
    pub fn new(paths: Paths, hooks: Hooks) -> Result<Self> {
        let config = Config::load_or_create(&paths.config_file)?;
        let state = State::load(&paths.state_file);
        Ok(Self {
            paths,
            config,
            state,
            hooks,
            busy: false,
            error: None,
            notice: None,
            last_failed_fetch: 0,
        })
    }

    /// Starts the worker thread and returns its command channel.
    pub fn spawn(self) -> (Sender<Command>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("rotator".into())
            .spawn(move || self.run(rx))
            .expect("could not start the rotator thread");
        (tx, handle)
    }

    fn run(mut self, rx: Receiver<Command>) {
        // Clean up anything left from a crash before doing anything else.
        self.cleanup();
        self.handle(Command::Tick);
        loop {
            let command = match rx.recv_timeout(TICK) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => Command::Tick,
            };
            self.handle(command);
        }
    }

    pub fn handle(&mut self, command: Command) {
        self.notice = None;
        match command {
            Command::Next => self.rotate_and_prefetch(),
            Command::Favorite => {
                match self.favorite() {
                    Ok(path) => self.notice = Some(format!("Saved to {}", path.display())),
                    Err(e) => self.fail(e),
                }
                self.publish();
            }
            Command::Reload => {
                self.reload();
                self.handle(Command::Tick);
            }
            Command::SetPaused(paused) => {
                self.config.paused = paused;
                if let Err(e) = self.config.save(&self.paths.config_file) {
                    self.fail(e);
                }
                self.publish();
            }
            Command::Tick => {
                if self.rotation_due() {
                    self.rotate_and_prefetch();
                } else {
                    self.prefetch_if_needed();
                    self.publish();
                }
            }
            Command::Shutdown => {}
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let next_rotation_at = (!self.config.paused && self.config.interval_minutes > 0)
            .then(|| self.state.last_rotated + self.config.interval_minutes * 60);
        Snapshot {
            current: self.state.current.as_ref().map(WallpaperView::from),
            busy: self.busy,
            error: self.error.clone(),
            notice: self.notice.clone(),
            has_key: !self.config.effective_key().is_empty(),
            settings: Settings::from_config(&self.config),
            next_rotation_at,
            favorites_dir: self.favorites_dir().display().to_string(),
        }
    }

    pub fn current_file(&self) -> Option<PathBuf> {
        self.state.current.as_ref().map(|w| w.file.clone())
    }

    fn publish(&self) {
        (self.hooks.notify)(&self.snapshot());
    }

    fn fail(&mut self, e: anyhow::Error) {
        let message = format!("{e:#}");
        log::write(&self.paths.log_file, &format!("error: {message}"));
        self.error = Some(message);
    }

    fn reload(&mut self) {
        match Config::load_or_create(&self.paths.config_file) {
            Ok(config) => {
                if !config.same_source(&self.config) {
                    // The prefetched photo came from the old search.
                    self.state.next = None;
                    self.last_failed_fetch = 0;
                }
                if config.effective_key() != self.config.effective_key() {
                    self.last_failed_fetch = 0;
                }
                self.config = config;
                self.error = None;
                self.save_state();
                self.cleanup();
            }
            Err(e) => self.fail(e),
        }
    }

    fn rotation_due(&self) -> bool {
        if self.config.effective_key().is_empty() {
            return false;
        }
        let current_missing = self.state.current.as_ref().is_none_or(|w| !w.file.exists());
        if current_missing {
            // No wallpaper yet (or the OS purged the cache): get one, unless
            // the last attempt just failed.
            return now() >= self.last_failed_fetch + RETRY_AFTER_SECS;
        }
        !self.config.paused
            && self.config.interval_minutes > 0
            && now() >= self.state.last_rotated + self.config.interval_minutes * 60
            && now() >= self.last_failed_fetch + RETRY_AFTER_SECS
    }

    fn rotate_and_prefetch(&mut self) {
        self.busy = true;
        self.publish();
        let result = self.rotate();
        self.busy = false;
        match result {
            Ok(()) => self.error = None,
            Err(e) => {
                self.last_failed_fetch = now();
                self.fail(e);
            }
        }
        self.publish();
        if self.error.is_none() {
            self.prefetch_if_needed();
            self.publish();
        }
    }

    fn rotate(&mut self) -> Result<()> {
        let client = self.client()?;
        let next = match self.state.next.take() {
            Some(w) if w.file.exists() => w,
            _ => self.fetch(&client)?,
        };
        if let Err(e) = (self.hooks.apply)(&next.file) {
            self.state.next = Some(next);
            return Err(e);
        }
        if let Err(e) = client.track_download(&next.download_location) {
            log::write(
                &self.paths.log_file,
                &format!("download tracking failed: {e:#}"),
            );
        }
        log::write(
            &self.paths.log_file,
            &format!("wallpaper set: {} by {}", next.id, next.author),
        );
        self.state.current = Some(next);
        self.state.last_rotated = now();
        self.save_state();
        self.cleanup();
        Ok(())
    }

    fn prefetch_if_needed(&mut self) {
        let has_next = self.state.next.as_ref().is_some_and(|w| w.file.exists());
        if has_next
            || self.config.effective_key().is_empty()
            || now() < self.last_failed_fetch + RETRY_AFTER_SECS
        {
            return;
        }
        let result = self.client().and_then(|client| self.fetch(&client));
        match result {
            Ok(w) => {
                self.state.next = Some(w);
                self.save_state();
                self.cleanup();
            }
            Err(e) => {
                self.last_failed_fetch = now();
                log::write(&self.paths.log_file, &format!("prefetch failed: {e:#}"));
            }
        }
    }

    fn client(&self) -> Result<Client> {
        Client::new(&self.config.effective_key())
    }

    fn topic_ids(&mut self, client: &Client) -> Result<Vec<String>> {
        if !self.config.query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for topic in self.config.topics.clone() {
            let id = match self.state.topic_ids.get(&topic) {
                Some(id) => id.clone(),
                None => {
                    let id = client.topic_id(&topic)?;
                    self.state.topic_ids.insert(topic, id.clone());
                    id
                }
            };
            ids.push(id);
        }
        Ok(ids)
    }

    /// Picks a random photo at least as large as the screen (best of three
    /// tries) and downloads it sized to the screen.
    fn fetch(&mut self, client: &Client) -> Result<Wallpaper> {
        let (width, height) = (self.hooks.screen_size)().unwrap_or(FALLBACK_SIZE);
        let topics = self.topic_ids(client)?;
        let mut photo = client.random_photo(&self.config.query, &topics)?;
        for _ in 0..2 {
            if photo.width >= width && photo.height >= height {
                break;
            }
            photo = client.random_photo(&self.config.query, &topics)?;
        }
        let file = self
            .paths
            .images_dir
            .join(format!("{}.jpg", safe_name(&photo.id)));
        client.download(&unsplash::sized_url(&photo.urls.raw, width, height), &file)?;
        Ok(Wallpaper {
            id: photo.id,
            file,
            author: photo.user.name,
            author_url: photo.user.links.html,
            photo_url: photo.links.html,
            preview_url: photo.urls.small,
            location: photo.location.and_then(|l| l.label()),
            description: photo.description.or(photo.alt_description),
            download_location: photo.links.download_location,
            color: photo.color,
        })
    }

    fn favorites_dir(&self) -> PathBuf {
        self.config
            .favorites_dir
            .clone()
            .unwrap_or_else(|| self.paths.default_favorites_dir.clone())
    }

    fn favorite(&self) -> Result<PathBuf> {
        let current = self
            .state
            .current
            .as_ref()
            .context("no wallpaper to save yet")?;
        let dir = self.favorites_dir();
        fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let dest = dir.join(format!(
            "{} - {}.jpg",
            safe_name(&current.author),
            safe_name(&current.id)
        ));
        if !dest.exists() {
            fs::copy(&current.file, &dest).context("could not copy the wallpaper")?;
        }
        Ok(dest)
    }

    fn save_state(&mut self) {
        if let Err(e) = self.state.save(&self.paths.state_file) {
            self.fail(e);
        }
    }

    fn cleanup(&mut self) {
        match store::cleanup(
            &self.paths.images_dir,
            &mut self.state,
            self.config.cache_cap_bytes(),
        ) {
            Ok(report) => {
                if report.removed_files > 0 {
                    log::write(
                        &self.paths.log_file,
                        &format!(
                            "cleanup removed {} files ({} KB)",
                            report.removed_files,
                            report.removed_bytes / 1024
                        ),
                    );
                }
                if report.dropped_next {
                    self.save_state();
                }
            }
            Err(e) => self.fail(e),
        }
    }
}

/// Keeps file names portable: letters, digits, `-`, `_`, spaces and dots.
fn safe_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "photo".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn service(root: &Path) -> (Service, Arc<Mutex<Vec<PathBuf>>>) {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let seen = applied.clone();
        let hooks = Hooks {
            apply: Box::new(move |p| {
                seen.lock().unwrap().push(p.to_path_buf());
                Ok(())
            }),
            screen_size: Box::new(|| Some((100, 100))),
            notify: Box::new(|_| {}),
        };
        (Service::new(Paths::under(root), hooks).unwrap(), applied)
    }

    fn cached(svc: &Service, id: &str) -> Wallpaper {
        let file = svc.paths.images_dir.join(format!("{id}.jpg"));
        fs::create_dir_all(&svc.paths.images_dir).unwrap();
        fs::write(&file, b"jpeg").unwrap();
        Wallpaper {
            id: id.into(),
            file,
            author: "Ann / Lee".into(),
            author_url: "https://unsplash.com/@ann".into(),
            photo_url: "https://unsplash.com/photos/x".into(),
            preview_url: String::new(),
            location: None,
            description: None,
            download_location: "http://127.0.0.1:9/never".into(),
            color: None,
        }
    }

    #[test]
    fn no_key_means_no_rotation() {
        std::env::remove_var("UNSPLASH_ACCESS_KEY");
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, applied) = service(dir.path());
        assert!(!svc.rotation_due());
        svc.handle(Command::Tick);
        assert!(applied.lock().unwrap().is_empty());
        assert!(!svc.snapshot().has_key);
        svc.handle(Command::Next);
        assert!(svc.snapshot().error.unwrap().contains("access key"));
    }

    #[test]
    fn next_uses_the_prefetched_photo_and_deletes_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, applied) = service(dir.path());
        svc.config.access_key = "test-key".into();
        // Keep the follow-up prefetch from reaching the network.
        svc.last_failed_fetch = now();
        let old = cached(&svc, "old");
        let next = cached(&svc, "next");
        svc.state.current = Some(old.clone());
        svc.state.next = Some(next.clone());

        svc.rotate().unwrap();

        assert_eq!(*applied.lock().unwrap(), vec![next.file.clone()]);
        assert_eq!(svc.state.current.as_ref().unwrap().id, "next");
        assert!(svc.state.next.is_none());
        assert!(!old.file.exists(), "old wallpaper should be deleted");
        assert!(next.file.exists());
        assert!(State::load(&svc.paths.state_file).current.is_some());
    }

    #[test]
    fn failed_apply_keeps_the_photo_for_later() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, _) = service(dir.path());
        svc.hooks.apply = Box::new(|_| anyhow::bail!("no display"));
        svc.config.access_key = "test-key".into();
        let next = cached(&svc, "next");
        svc.state.next = Some(next.clone());
        assert!(svc.rotate().is_err());
        assert_eq!(svc.state.next, Some(next));
    }

    #[test]
    fn favorites_copy_outside_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, _) = service(dir.path());
        svc.state.current = Some(cached(&svc, "cur"));
        svc.handle(Command::Favorite);
        let saved = dir.path().join("favorites").join("Ann _ Lee - cur.jpg");
        assert!(saved.exists());
        assert!(svc.snapshot().notice.unwrap().starts_with("Saved to"));
        // Cleanup never touches favorites.
        svc.cleanup();
        assert!(saved.exists());
    }

    #[test]
    fn interval_and_pause_control_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, _) = service(dir.path());
        svc.config.access_key = "k".into();
        svc.state.current = Some(cached(&svc, "cur"));
        svc.state.last_rotated = now();
        assert!(!svc.rotation_due());
        svc.state.last_rotated = now() - 25 * 3600;
        assert!(svc.rotation_due());
        svc.config.paused = true;
        assert!(!svc.rotation_due());
        svc.config.paused = false;
        svc.config.interval_minutes = 0;
        assert!(!svc.rotation_due());
        // A purged cache file triggers a fresh photo even when paused.
        svc.config.paused = true;
        fs::remove_file(&svc.state.current.as_ref().unwrap().file).unwrap();
        assert!(svc.rotation_due());
    }

    #[test]
    fn changing_the_search_drops_the_prefetched_photo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, _) = service(dir.path());
        let next = cached(&svc, "next");
        svc.state.next = Some(next.clone());
        let mut config = svc.config.clone();
        config.query = "mountains".into();
        config.save(&svc.paths.config_file).unwrap();
        svc.reload();
        assert!(svc.state.next.is_none());
        assert!(!next.file.exists());
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
        settings.apply_to(&mut config, Some("  "));
        assert_eq!(config.topics, ["nature", "film"]);
        assert_eq!(config.cache_cap_mb, 20);
        assert_eq!(config.access_key, "old");
        settings.apply_to(&mut config, Some("new"));
        assert_eq!(config.access_key, "new");
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("Ann / Lee"), "Ann _ Lee");
        assert_eq!(safe_name("../.."), "_");
        assert_eq!(safe_name("..."), "photo");
    }
}
