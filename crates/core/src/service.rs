//! The rotation worker. One thread owns the config, state and cache; the UI
//! talks to it through [`Command`]s and gets [`Snapshot`]s back, so nothing
//! else touches the images folder.
//!
//! Photos come from the sources ticked in Settings. Each source's answers
//! are queued, so most rotations need no search request; each photo is
//! taken from a randomly ordered source, and a source that fails or is
//! rate-limited rests while the others (and finally the favorites folder)
//! fill in.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::net::{self, Http};
use crate::paths::Paths;
use crate::source::{self, daily, favorites, pexels, unsplash, wallhaven, Candidate, SourceId};
use crate::store::{self, Set, State, Wallpaper};
use crate::wallpaper::{self, Display, Layout};
use crate::{log, now};

const TICK: Duration = Duration::from_secs(60);
/// Wait this long after every source failed before trying again on a tick.
const RETRY_AFTER_SECS: u64 = 15 * 60;
/// How long a source rests after a rate limit, or after another error.
const RATE_LIMIT_REST_SECS: u64 = 20 * 60;
const ERROR_REST_SECS: u64 = 10 * 60;
const FALLBACK_SIZE: (u32, u32) = (3840, 2160);
/// Ask a source for a fresh batch when its best queued photo covers less
/// of the display than this.
const GOOD_ENOUGH: f64 = 0.75;
/// Photos to try from one source before moving on to the next.
const TRIES_PER_SOURCE: usize = 3;
/// Queued candidates kept per source.
const QUEUE_LIMIT: usize = 120;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Rotate now: every display, or (with a photo per display) only the
    /// display showing the current photo at this position.
    Next(Option<usize>),
    /// Copy a current photo (by position) or all of them into the
    /// favorites folder.
    Favorite(Option<usize>),
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

impl From<&Wallpaper> for WallpaperView {
    fn from(w: &Wallpaper) -> Self {
        let unsplash = w.source == SourceId::Unsplash;
        let link = |url: &str| {
            if unsplash {
                unsplash::with_utm(url)
            } else {
                url.to_string()
            }
        };
        Self {
            id: w.id.clone(),
            author: w.author.clone(),
            author_url: link(&w.author_url),
            photo_url: link(&w.photo_url),
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
        config.cache_cap_mb = self.cache_cap_mb.max(20);
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

pub type ApplyFn = Box<dyn Fn(&Layout) -> Result<()> + Send>;
pub type DisplaysFn = Box<dyn Fn() -> Vec<Display> + Send>;
pub type NotifyFn = Box<dyn Fn(&Snapshot) + Send>;

/// How the worker reaches the platform and the UI.
pub struct Hooks {
    /// Sets the wallpaper. Must hop to the main thread on macOS.
    pub apply: ApplyFn,
    /// The connected displays, sized in physical pixels. Empty if unknown.
    pub displays: DisplaysFn,
    /// Called whenever the snapshot changes.
    pub notify: NotifyFn,
}

pub struct Service {
    paths: Paths,
    config: Config,
    state: State,
    hooks: Hooks,
    http: Http,
    /// Displays as of the last check, to notice monitors coming and going.
    displays: Vec<Display>,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    last_failed_fetch: u64,
    /// Sources resting after an error, until this time.
    resting: HashMap<SourceId, u64>,
    rng: u64,
    /// Whether `UNSPLASH_ACCESS_KEY` and `PEXELS_API_KEY` win over the
    /// config's keys.
    env_keys: bool,
}

impl Service {
    pub fn new(paths: Paths, hooks: Hooks) -> Result<Self> {
        Self::build(paths, hooks, Http::default(), true)
    }

    /// A worker that sends its requests through `http` (tests point it at a
    /// local stub) and takes keys only from the config file, never from the
    /// environment.
    pub fn with_http(paths: Paths, hooks: Hooks, http: Http) -> Result<Self> {
        Self::build(paths, hooks, http, false)
    }

    fn build(paths: Paths, hooks: Hooks, http: Http, env_keys: bool) -> Result<Self> {
        let config = Config::load_or_create(&paths.config_file)?;
        let state = State::load(&paths.state_file);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Ok(Self {
            paths,
            config,
            state,
            hooks,
            http,
            displays: Vec::new(),
            busy: false,
            error: None,
            notice: None,
            last_failed_fetch: 0,
            resting: HashMap::new(),
            rng: seed | 1,
            env_keys,
        })
    }

    /// The Unsplash and Pexels keys in use.
    fn keys(&self) -> (String, String) {
        keys(&self.config, self.env_keys)
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
        match command {
            Command::Next(only) => {
                self.notice = None;
                self.rotate_and_prefetch(only);
            }
            Command::Favorite(index) => {
                match self.favorite(index) {
                    Ok(saved) => self.notice = Some(saved),
                    Err(e) => self.fail(e),
                }
                self.publish();
            }
            Command::Reload => {
                self.notice = None;
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
                self.check_displays();
                if self.rotation_due() {
                    self.notice = None;
                    self.rotate_and_prefetch(None);
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
        let (unsplash_key, pexels_key) = self.keys();
        Snapshot {
            current: self
                .state
                .current
                .iter()
                .flat_map(|set| &set.photos)
                .map(WallpaperView::from)
                .collect(),
            busy: self.busy,
            error: self.error.clone(),
            notice: self.notice.clone(),
            needs_setup: !self.can_fetch(),
            has_unsplash_key: !unsplash_key.is_empty(),
            has_pexels_key: !pexels_key.is_empty(),
            settings: Settings::from_config(&self.config),
            photographer_note: unsplash::photographer_ignored(
                &self.config.query,
                &self.config.collections,
                &self.config.username,
            ),
            next_rotation_at,
            favorites_dir: self.favorites_dir().display().to_string(),
        }
    }

    pub fn current_layout(&self) -> Option<Layout> {
        self.state.current.as_ref().map(Set::layout)
    }

    fn publish(&self) {
        (self.hooks.notify)(&self.snapshot());
    }

    fn fail(&mut self, e: anyhow::Error) {
        let message = self.scrub(&format!("{e:#}"));
        log::write(&self.paths.log_file, &format!("error: {message}"));
        self.error = Some(message);
    }

    fn log(&self, message: &str) {
        log::write(&self.paths.log_file, &self.scrub(message));
    }

    /// `text` without the keys: the saved ones and the environment's. A
    /// source's error can quote its reply, and a reply can quote a key.
    fn scrub(&self, text: &str) -> String {
        let (env_unsplash, env_pexels) = keys(&self.config, true);
        let (unsplash, pexels) = keys(&self.config, false);
        net::scrub(text, &[&env_unsplash, &env_pexels, &unsplash, &pexels])
    }

    fn reload(&mut self) {
        match Config::load_or_create(&self.paths.config_file) {
            Ok(config) => {
                if !config.same_source(&self.config) {
                    // Prefetched and queued photos came from the old settings.
                    self.state.next = None;
                    self.state.queue.clear();
                    self.state.pexels_pages = None;
                    self.last_failed_fetch = 0;
                    self.resting.clear();
                }
                if keys(&config, self.env_keys) != self.keys() {
                    self.last_failed_fetch = 0;
                    self.resting.clear();
                }
                self.config = config;
                self.error = None;
                self.save_state();
                self.cleanup();
            }
            Err(e) => self.fail(e),
        }
    }

    /// Whether a source can be asked at all (it has the key it needs).
    fn usable(&self, source: SourceId) -> bool {
        match source {
            SourceId::Unsplash => !self.keys().0.is_empty(),
            SourceId::Pexels => !self.keys().1.is_empty(),
            SourceId::Wallhaven | SourceId::Daily | SourceId::Favorites => true,
        }
    }

    fn can_fetch(&self) -> bool {
        self.config.sources.iter().any(|&s| self.usable(s))
    }

    /// Asks the platform for the displays, keeping the last known list if
    /// it can't say.
    fn live_displays(&mut self) -> Vec<Display> {
        let live = (self.hooks.displays)();
        if !live.is_empty() {
            self.displays = live;
        }
        self.displays.clone()
    }

    /// Whether to show a different photo on each of these displays.
    fn each(&self, displays: &[Display]) -> bool {
        self.config.per_display && displays.len() > 1
    }

    /// When monitors come or go (or change resolution), re-places the
    /// current photos so a new display isn't left with an old or deleted
    /// one. Fresh photos sized for the new layout come with the next
    /// rotation.
    fn check_displays(&mut self) {
        let before = self.displays.clone();
        let live = self.live_displays();
        if before.is_empty() || live == before {
            return;
        }
        self.log(&format!("displays changed: {} connected", live.len()));
        let Some(current) = self.state.current.clone() else {
            return;
        };
        let result = (|| -> Result<()> {
            if current.spanned.is_some() {
                let restitched = self.stitch(current, &live)?;
                (self.hooks.apply)(&restitched.layout())?;
                self.state.current = Some(restitched);
                self.save_state();
                self.cleanup();
            } else {
                (self.hooks.apply)(&current.layout())?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.fail(e);
        }
    }

    fn rotation_due(&self) -> bool {
        if !self.can_fetch() {
            return false;
        }
        let current_missing = self.state.current.as_ref().is_none_or(|s| !s.on_disk());
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

    fn rotate_and_prefetch(&mut self, only: Option<usize>) {
        self.busy = true;
        self.publish();
        let result = match only {
            Some(index) => self.rotate_one(index),
            None => self.rotate(),
        };
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
        if !self.can_fetch() {
            bail!("tick a photo source in Settings (and add its key if it needs one)");
        }
        let displays = self.live_displays();
        let each = self.each(&displays);
        let next = match self.state.next.take() {
            Some(set) if set.on_disk() && set.fits(&displays, each) => set,
            _ => {
                let (set, notes) = self.fetch(&displays, each)?;
                if !notes.is_empty() {
                    self.notice = Some(fallback_notice(&notes, &set));
                }
                set
            }
        };
        if let Err(e) = (self.hooks.apply)(&next.layout()) {
            self.state.next = Some(next);
            return Err(e);
        }
        for photo in &next.photos {
            self.used(photo);
        }
        self.state.current = Some(next);
        self.state.last_rotated = now();
        self.save_state();
        self.cleanup();
        Ok(())
    }

    /// Reports a photo that just went on screen (Unsplash's terms) and logs it.
    fn used(&self, photo: &Wallpaper) {
        if photo.source == SourceId::Unsplash && !photo.download_location.is_empty() {
            let (key, _) = self.keys();
            if let Err(e) = unsplash::track(&self.http, &key, &photo.download_location) {
                self.log(&format!("download tracking failed: {e:#}"));
            }
        }
        let place = photo
            .display
            .as_ref()
            .map(|d| format!(" on {}", d.name))
            .unwrap_or_default();
        self.log(&format!(
            "wallpaper set: {} by {} ({}){place}",
            photo.id,
            photo.author,
            photo.source.name()
        ));
    }

    /// Replaces only the photo at `index` (one display's), keeping the
    /// others and the schedule. Falls back to rotating everything when the
    /// photos aren't per display or that display is gone.
    fn rotate_one(&mut self, index: usize) -> Result<()> {
        let displays = self.live_displays();
        let target = self
            .state
            .current
            .as_ref()
            .and_then(|s| s.photos.get(index))
            .and_then(|w| w.display.as_ref())
            .and_then(|d| displays.iter().find(|live| live.id == d.id))
            .cloned();
        let (Some(target), Some(current)) = (target, self.state.current.clone()) else {
            return self.rotate();
        };
        // This display's prefetched photo, if it's ready: instant. Its slot
        // in the prefetched set is filled again in the background.
        let ready = self.state.next.as_mut().and_then(|next| {
            let i = next
                .photos
                .iter()
                .position(|w| w.display.as_ref() == Some(&target) && w.file.exists())?;
            next.spanned = None;
            Some(next.photos.remove(i))
        });
        if self
            .state
            .next
            .as_ref()
            .is_some_and(|n| n.photos.is_empty())
        {
            self.state.next = None;
        }
        let photo = match ready {
            Some(photo) => photo,
            None => {
                let mut used: HashSet<String> = [&self.state.current, &self.state.next]
                    .into_iter()
                    .flatten()
                    .flat_map(|s| &s.photos)
                    .map(|w| w.id.clone())
                    .collect();
                let mut notes = Vec::new();
                let size = (target.width, target.height);
                let photo = self.photo_for(size, Some(target.clone()), &mut used, &mut notes)?;
                if !notes.is_empty() {
                    self.notice = Some(fallback_notice(&notes, &Set::single(photo.clone())));
                }
                photo
            }
        };
        let mut set = current;
        set.photos[index] = photo;
        set.spanned = None;
        if wallpaper::SPANS_ONE_IMAGE {
            set = self.stitch(set, &displays)?;
        }
        (self.hooks.apply)(&set.layout())?;
        self.used(&set.photos[index]);
        self.state.current = Some(set);
        self.save_state();
        self.cleanup();
        Ok(())
    }

    /// Fills the displays a prefetched set is missing (after one display got
    /// its photo early), instead of fetching a whole new set.
    fn top_up_next(&mut self, displays: &[Display]) -> Result<()> {
        let partial = self.state.next.as_ref().is_some_and(|next| {
            !next.photos.is_empty()
                && next.photos.len() < displays.len()
                && next.on_disk()
                && next
                    .photos
                    .iter()
                    .all(|w| w.display.as_ref().is_some_and(|d| displays.contains(d)))
        });
        if !partial {
            return Ok(());
        }
        let Some(mut set) = self.state.next.take() else {
            return Ok(());
        };
        let mut used: HashSet<String> = self
            .state
            .current
            .iter()
            .chain([&set])
            .flat_map(|s| &s.photos)
            .map(|w| w.id.clone())
            .collect();
        let mut notes = Vec::new();
        for display in displays {
            if set
                .photos
                .iter()
                .any(|w| w.display.as_ref() == Some(display))
            {
                continue;
            }
            let size = (display.width, display.height);
            match self.photo_for(size, Some(display.clone()), &mut used, &mut notes) {
                Ok(photo) => set.photos.push(photo),
                Err(e) => {
                    self.state.next = Some(set);
                    return Err(e);
                }
            }
        }
        // Display order, as the popover lists them.
        set.photos.sort_by_key(|w| {
            displays
                .iter()
                .position(|d| w.display.as_ref() == Some(d))
                .unwrap_or(usize::MAX)
        });
        set.spanned = None;
        if wallpaper::SPANS_ONE_IMAGE {
            set = self.stitch(set, displays)?;
        }
        self.state.next = Some(set);
        self.save_state();
        self.cleanup();
        Ok(())
    }

    fn prefetch_if_needed(&mut self) {
        if !self.can_fetch() || now() < self.last_failed_fetch + RETRY_AFTER_SECS {
            return;
        }
        let displays = self.live_displays();
        let each = self.each(&displays);
        if each {
            if let Err(e) = self.top_up_next(&displays) {
                self.last_failed_fetch = now();
                self.log(&format!("prefetch failed: {e:#}"));
                return;
            }
        }
        let ready = self
            .state
            .next
            .as_ref()
            .is_some_and(|s| s.on_disk() && s.fits(&displays, each));
        if ready {
            return;
        }
        match self.fetch(&displays, each) {
            Ok((set, notes)) => {
                if !notes.is_empty() {
                    self.log(&format!("prefetch: {}", notes.join(", ")));
                }
                self.state.next = Some(set);
                self.save_state();
                self.cleanup();
            }
            Err(e) => {
                self.last_failed_fetch = now();
                self.log(&format!("prefetch failed: {e:#}"));
            }
        }
    }

    fn random(&mut self) -> u64 {
        // xorshift64: plenty for shuffling photo sources.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.random() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    /// Ticked, usable sources not resting, in random order, then the
    /// favorites folder as a last resort even when it isn't ticked.
    fn source_order(&mut self) -> Vec<SourceId> {
        let now = now();
        let mut order: Vec<SourceId> = self
            .config
            .sources
            .clone()
            .into_iter()
            .filter(|&s| self.usable(s))
            .filter(|s| self.resting.get(s).is_none_or(|&until| now >= until))
            .collect();
        order.dedup();
        self.shuffle(&mut order);
        if !order.contains(&SourceId::Favorites) {
            order.push(SourceId::Favorites);
        }
        order
    }

    fn rest(&mut self, source: SourceId, e: &anyhow::Error) {
        let secs = if net::is_rate_limited(e) {
            RATE_LIMIT_REST_SECS
        } else {
            ERROR_REST_SECS
        };
        self.resting.insert(source, now() + secs);
    }

    /// Picks and downloads photos for these displays: one for all of them,
    /// or one per display when `each` is set. Also returns notes on sources
    /// that failed and were skipped.
    fn fetch(&mut self, displays: &[Display], each: bool) -> Result<(Set, Vec<String>)> {
        let shared = largest(displays).unwrap_or(FALLBACK_SIZE);
        let targets: Vec<Option<Display>> = if each {
            displays.iter().cloned().map(Some).collect()
        } else {
            vec![None]
        };
        let sizes: Vec<(u32, u32)> = targets
            .iter()
            .map(|t| t.as_ref().map_or(shared, |d| (d.width, d.height)))
            .collect();
        // Bigger displays choose first.
        let mut order: Vec<usize> = (0..targets.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(u64::from(sizes[i].0) * u64::from(sizes[i].1)));

        // Don't show what's already on screen again.
        let mut used: HashSet<String> = self
            .state
            .current
            .iter()
            .flat_map(|s| &s.photos)
            .map(|w| w.id.clone())
            .collect();
        let mut notes = Vec::new();
        let mut photos: Vec<Option<Wallpaper>> = vec![None; targets.len()];
        for i in order {
            let photo = self.photo_for(sizes[i], targets[i].clone(), &mut used, &mut notes)?;
            photos[i] = Some(photo);
        }
        let mut set = Set {
            photos: photos.into_iter().flatten().collect(),
            spanned: None,
        };
        if each && wallpaper::SPANS_ONE_IMAGE {
            set = self.stitch(set, displays)?;
        }
        Ok((set, notes))
    }

    /// One photo at `size`, from the first source that can give one.
    fn photo_for(
        &mut self,
        size: (u32, u32),
        display: Option<Display>,
        used: &mut HashSet<String>,
        notes: &mut Vec<String>,
    ) -> Result<Wallpaper> {
        let mut first_error = None;
        for source in self.source_order() {
            // Favorites fill in quietly when they weren't asked for.
            let ticked = self.config.sources.contains(&source);
            match self.photo_from(source, size, &display, used) {
                Ok(photo) => return Ok(photo),
                Err(e) => {
                    self.log(&format!("{} skipped: {e:#}", source.name()));
                    if ticked {
                        self.rest(source, &e);
                        let note = if net::is_rate_limited(&e) {
                            format!("{} is rate-limited", source.name())
                        } else {
                            format!("{} failed", source.name())
                        };
                        if !notes.contains(&note) {
                            notes.push(note);
                        }
                        first_error.get_or_insert(e);
                    }
                }
            }
        }
        Err(first_error.unwrap_or_else(|| {
            let wait = self
                .resting
                .values()
                .min()
                .map(|until| until.saturating_sub(now()).div_ceil(60))
                .unwrap_or(0);
            anyhow!("every photo source is resting after an error; trying again in {wait} min")
        }))
    }

    /// The best-fitting queued photo from `source` (refilling the queue
    /// when nothing fits well), downloaded at `size`.
    fn photo_from(
        &mut self,
        source: SourceId,
        (width, height): (u32, u32),
        display: &Option<Display>,
        used: &mut HashSet<String>,
    ) -> Result<Wallpaper> {
        let mut refilled = false;
        let mut tries = 0;
        loop {
            let best = self.best_queued(source, (width, height), used);
            if !refilled && best.is_none_or(|(_, cover)| cover < GOOD_ENOUGH) {
                self.refill(source, (width, height))?;
                refilled = true;
                continue;
            }
            // A small pool (a few favorites, say) may have nothing unused
            // left: repeat one rather than fail, preferring one that isn't
            // on screen right now.
            let best = best.or_else(|| {
                let on_screen: HashSet<String> = self
                    .state
                    .current
                    .iter()
                    .flat_map(|s| &s.photos)
                    .map(|w| w.id.clone())
                    .collect();
                self.best_queued(source, (width, height), &on_screen)
                    .or_else(|| self.best_queued(source, (width, height), &HashSet::new()))
            });
            let Some((index, _)) = best else {
                bail!("{} had no photos", source.name());
            };
            let candidate = self
                .state
                .queue
                .get_mut(&source)
                .map(|q| q.remove(index))
                .context("queue changed")?;
            used.insert(candidate.id.clone());
            let file = self
                .paths
                .images_dir
                .join(format!("{}-{width}x{height}.jpg", safe_name(&candidate.id)));
            match source::fetch_image(&self.http, &candidate, width, height, &file) {
                Ok(()) => return Ok(wallpaper_from(candidate, file, display.clone())),
                Err(e) => {
                    tries += 1;
                    self.log(&format!("skipped {}: {e:#}", candidate.id));
                    if net::is_rate_limited(&e) || tries >= TRIES_PER_SOURCE {
                        return Err(e);
                    }
                }
            }
        }
    }

    fn best_queued(
        &self,
        source: SourceId,
        size: (u32, u32),
        used: &HashSet<String>,
    ) -> Option<(usize, f64)> {
        let queue = self.state.queue.get(&source)?;
        queue
            .iter()
            .enumerate()
            .filter(|(_, c)| !used.contains(&c.id))
            .map(|(i, c)| (i, coverage((c.width, c.height), size)))
            .fold(None, |best, (i, cover)| match best {
                Some((_, b)) if b >= cover => best,
                _ => Some((i, cover)),
            })
    }

    /// Asks `source` for a new batch and queues the photos it didn't have.
    fn refill(&mut self, source: SourceId, (width, height): (u32, u32)) -> Result<()> {
        let query = self.config.query.clone();
        let batch: Vec<Candidate> = match source {
            SourceId::Unsplash => {
                let (key, _) = self.keys();
                let topics = self.topic_ids(&key)?;
                unsplash::batch(
                    &self.http,
                    &key,
                    &query,
                    &topics,
                    &self.config.collections,
                    &self.config.username,
                )?
            }
            SourceId::Pexels => {
                let (_, key) = self.keys();
                let random = self.random();
                let (batch, pages) =
                    pexels::batch(&self.http, &key, &query, self.state.pexels_pages, random)?;
                self.state.pexels_pages = Some(pages);
                batch
            }
            SourceId::Wallhaven => wallhaven::batch(&self.http, &query, width, height)?,
            SourceId::Daily => daily::batch(&self.http, now())?,
            SourceId::Favorites => favorites::batch(&self.favorites_dir())?,
        };
        let mut fresh: Vec<Candidate> = {
            let queued: HashSet<&str> = self
                .state
                .queue
                .get(&source)
                .into_iter()
                .flatten()
                .map(|c| c.id.as_str())
                .collect();
            batch
                .into_iter()
                .filter(|c| !queued.contains(c.id.as_str()))
                .collect()
        };
        self.shuffle(&mut fresh);
        let queue = self.state.queue.entry(source).or_default();
        queue.extend(fresh);
        let excess = queue.len().saturating_sub(QUEUE_LIMIT);
        queue.drain(..excess);
        self.save_state();
        Ok(())
    }

    fn topic_ids(&mut self, key: &str) -> Result<Vec<String>> {
        let c = &self.config;
        if unsplash::selection(&c.query, &c.collections, &c.username) != unsplash::Selection::Topics
        {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for topic in self.config.topics.clone() {
            let id = match self.state.topic_ids.get(&topic) {
                Some(id) => id.clone(),
                None => {
                    let id = unsplash::topic_id(&self.http, key, &topic)?;
                    self.state.topic_ids.insert(topic, id.clone());
                    id
                }
            };
            ids.push(id);
        }
        Ok(ids)
    }

    /// GNOME: stitches a set's photos into one image laid out like
    /// `displays`. A display without its own photo borrows one in turn.
    fn stitch(&self, mut set: Set, displays: &[Display]) -> Result<Set> {
        let layout = Layout::Each(
            set.photos
                .iter()
                .map(|w| {
                    let id = w.display.as_ref().map(|d| d.id.clone());
                    (id.unwrap_or_default(), w.file.clone())
                })
                .collect(),
        );
        let parts: Vec<(Display, PathBuf)> = displays
            .iter()
            .enumerate()
            .filter_map(|(i, d)| Some((d.clone(), layout.file_for(&d.id, i)?.to_path_buf())))
            .collect();
        // A new name each time, so GNOME notices the change.
        let first = set.photos.first().map_or("photo", |w| w.id.as_str());
        let dest = self
            .paths
            .images_dir
            .join(format!("span-{}-{}.jpg", safe_name(first), now()));
        wallpaper::compose(&parts, &dest)?;
        set.spanned = Some(dest);
        Ok(set)
    }

    fn favorites_dir(&self) -> PathBuf {
        self.config
            .favorites_dir
            .clone()
            .unwrap_or_else(|| self.paths.default_favorites_dir.clone())
    }

    /// Copies one current photo (or all of them) to the favorites folder and
    /// says where they went.
    fn favorite(&self, index: Option<usize>) -> Result<String> {
        let set = self
            .state
            .current
            .as_ref()
            .context("no wallpaper to save yet")?;
        let photos: Vec<&Wallpaper> = match index {
            Some(i) => vec![set.photos.get(i).context("that wallpaper has changed")?],
            None => set.photos.iter().collect(),
        };
        let photos: Vec<&Wallpaper> = photos
            .into_iter()
            .filter(|w| w.source != SourceId::Favorites)
            .collect();
        if photos.is_empty() {
            return Ok("Already in your favorites".into());
        }
        let dir = self.favorites_dir();
        fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let mut saved = Vec::new();
        for photo in photos {
            // Unsplash favorites keep the original "{author} - {id}" name.
            let id = match photo.source {
                SourceId::Unsplash => safe_name(&photo.id),
                other => format!("{}-{}", other.key(), safe_name(&photo.id)),
            };
            let dest = dir.join(format!("{} - {id}.jpg", safe_name(&photo.author)));
            if !dest.exists() {
                fs::copy(&photo.file, &dest).context("could not copy the wallpaper")?;
            }
            if !saved.contains(&dest) {
                saved.push(dest);
            }
        }
        Ok(match saved.as_slice() {
            [one] => format!("Saved to {}", one.display()),
            many => format!("Saved {} photos to {}", many.len(), dir.display()),
        })
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
                    self.log(&format!(
                        "cleanup removed {} files ({} KB)",
                        report.removed_files,
                        report.removed_bytes / 1024
                    ));
                }
                if report.dropped_next {
                    self.save_state();
                }
            }
            Err(e) => self.fail(e),
        }
    }
}

/// The Unsplash and Pexels keys, from the environment first when `env`.
fn keys(config: &Config, env: bool) -> (String, String) {
    if env {
        (config.effective_key(), config.effective_pexels_key())
    } else {
        (
            config.access_key.trim().to_string(),
            config.pexels_key.trim().to_string(),
        )
    }
}

/// "Unsplash is rate-limited, so this one is from Pexels."
fn fallback_notice(notes: &[String], set: &Set) -> String {
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

fn wallpaper_from(c: Candidate, file: PathBuf, display: Option<Display>) -> Wallpaper {
    Wallpaper {
        id: c.id,
        file,
        author: c.author,
        author_url: c.author_url,
        photo_url: c.photo_url,
        preview_url: c.preview_url,
        location: c.location,
        description: c.description,
        download_location: c.track_url.unwrap_or_default(),
        color: c.color,
        source: c.source,
        display,
    }
}

/// The size that covers every display: the widest width and tallest height.
fn largest(displays: &[Display]) -> Option<(u32, u32)> {
    let width = displays.iter().map(|d| d.width).max()?;
    let height = displays.iter().map(|d| d.height).max()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// How much of a display a photo covers without upscaling, up to 1.0.
/// Unknown sizes count as half.
fn coverage(photo: (u32, u32), size: (u32, u32)) -> f64 {
    if photo.0 == 0 || photo.1 == 0 {
        return 0.5;
    }
    let w = f64::from(photo.0) / f64::from(size.0.max(1));
    let h = f64::from(photo.1) / f64::from(size.1.max(1));
    w.min(h).min(1.0)
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
    //! Pure logic with many branches. Whole journeys through `Command`s,
    //! against a local stub of every photo API, are in `tests/worker.rs`.
    use super::*;
    use crate::source::tests::png;
    use crate::source::Image;
    use std::path::Path;

    fn screen(id: &str, width: u32, height: u32) -> Display {
        Display {
            id: id.into(),
            name: format!("Screen {id}"),
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// A worker with no network: nothing here may reach a photo API.
    fn service(root: &Path) -> Service {
        let hooks = Hooks {
            apply: Box::new(|_| Ok(())),
            displays: Box::new(|| vec![screen("1", 100, 100)]),
            notify: Box::new(|_| {}),
        };
        Service::with_http(Paths::under(root), hooks, Http::default()).unwrap()
    }

    fn photo(svc: &Service, id: &str, display: Option<Display>) -> Wallpaper {
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
            download_location: String::new(),
            color: None,
            source: SourceId::Unsplash,
            display,
        }
    }

    fn cached(svc: &Service, id: &str) -> Set {
        Set::single(photo(svc, id, None))
    }

    #[test]
    fn interval_and_pause_control_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let mut svc = service(dir.path());
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
        fs::remove_file(&svc.state.current.as_ref().unwrap().photos[0].file).unwrap();
        assert!(svc.rotation_due());
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
    fn small_unsplash_pool_prefers_photos_outside_the_current_set() {
        for each in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut svc = service(dir.path());
            let screens = [screen("1", 100, 100), screen("2", 100, 100)];
            let current = photo(&svc, "current", each.then(|| screens[0].clone()));
            svc.state.current = Some(Set::single(current));
            let image = png(dir.path(), "image.png", 100, 100);
            let candidate = |id: &str| Candidate {
                source: SourceId::Unsplash,
                id: id.into(),
                width: 100,
                height: 100,
                author: String::new(),
                author_url: String::new(),
                photo_url: String::new(),
                preview_url: String::new(),
                location: None,
                description: None,
                color: None,
                image: Image::File {
                    path: image.clone(),
                },
                track_url: None,
            };
            svc.config.access_key = "unused-test-key".into();
            svc.state.queue.insert(
                SourceId::Unsplash,
                vec![candidate("current"), candidate("other"), candidate("third")],
            );
            let (next, _) = svc.fetch(&screens, each).unwrap();
            assert!(next.photos.iter().all(|w| w.id != "current"));
            let ids: HashSet<_> = next.photos.iter().map(|w| &w.id).collect();
            assert_eq!(ids.len(), next.photos.len());
        }
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
    fn coverage_prefers_photos_that_fill_the_display() {
        let monitor = (3840, 2160);
        assert_eq!(coverage((6000, 4000), monitor), 1.0);
        assert!((coverage((1920, 1080), monitor) - 0.5).abs() < 1e-9);
        assert_eq!(coverage((0, 0), monitor), 0.5);
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

    #[test]
    fn largest_covers_every_display() {
        let screens = [screen("1", 3024, 1964), screen("2", 2560, 2880)];
        assert_eq!(largest(&screens), Some((3024, 2880)));
        assert_eq!(largest(&[]), None);
    }

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("Ann / Lee"), "Ann _ Lee");
        assert_eq!(safe_name("../.."), "_");
        assert_eq!(safe_name("..."), "photo");
    }
}
