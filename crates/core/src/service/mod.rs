//! The rotation worker. One thread owns the state and cache; the UI talks
//! to it through [`Command`]s and gets [`Snapshot`]s back, so nothing else
//! touches `state.json` or the images folder.
//!
//! `config.toml` has more than one writer: the settings screen's Save (on
//! the Tauri command thread), this worker's Pause, and the first-run
//! defaults that `Config::load_or_create` writes from either side (for
//! example via [`favorites_folder`]). They all go through
//! `Config::update` or `Config::load_or_create`, which hold one
//! process-wide lock while they read, change and write the file (#92). The
//! worker never saves its own copy of the config; after a write it adopts
//! the file (`Reload`).
//!
//! Photos come from the sources ticked in Settings. Each source's answers
//! are queued, so most rotations need no search request; each photo is
//! taken from a randomly ordered source, and a source that fails or is
//! rate-limited rests while the others (and finally the favorites folder)
//! fill in.

mod favorites;
mod names;
mod picker;
mod rotation;
#[cfg(test)]
mod test_support;
mod view;

pub use favorites::favorites_folder;
pub use view::{Settings, Snapshot, WallpaperView};

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;

use crate::config::{Config, Keys, Unparseable};
use crate::net::Http;
use crate::paths::Paths;
use crate::source::{unsplash, SourceId};
use crate::store::{Set, State};
use crate::wallpaper::{Display, Layout};
use crate::{log, net, now, store};

const TICK: Duration = Duration::from_secs(60);
/// Wait this long after every source failed before trying again on a tick.
const RETRY_AFTER_SECS: u64 = 15 * 60;
const FALLBACK_SIZE: (u32, u32) = (3840, 2160);
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
    /// The displays for which the cache cap had to drop a prefetched set.
    /// Prefetching waits until the cap, the sources or the displays
    /// change; without this, every tick would download a set only to
    /// delete it again.
    prefetch_over_cap: Option<Vec<Display>>,
    /// Sources resting after an error, until this time.
    resting: HashMap<SourceId, u64>,
    rng: u64,
    /// Whether `UNSPLASH_ACCESS_KEY` and `PEXELS_API_KEY` win over the
    /// config's keys.
    env_keys: bool,
    /// The config file couldn't be parsed, so `config` is the defaults rather
    /// than what the user saved.
    on_fallback_defaults: bool,
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
        // A broken file is logged and left for the user to fix: Save and Pause
        // keep failing on it, so nothing writes these defaults over it.
        let (config, broken) = match Config::load_or_create(&paths.config_file) {
            Ok(config) => (config, None),
            Err(e) if e.is::<Unparseable>() => (Config::default(), Some(e)),
            Err(e) => return Err(e),
        };
        let state = State::load(&paths.state_file);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        let service = Self {
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
            prefetch_over_cap: None,
            resting: HashMap::new(),
            rng: seed | 1,
            env_keys,
            on_fallback_defaults: broken.is_some(),
        };
        if service.state.keep_cache {
            service.log(
                "the saved state was lost, so cached images are kept until a new photo is on screen",
            );
        }
        if let Some(e) = broken {
            service.log(&format!("error: {e:#}; using the default settings"));
        }
        Ok(service)
    }

    /// The user's "start at login" choice, or `None` while a broken config
    /// has the service on defaults: those say nothing about what the user
    /// chose, so they mustn't change the login item.
    pub fn saved_start_at_login(&self) -> Option<bool> {
        (!self.on_fallback_defaults).then_some(self.config.start_at_login)
    }

    /// The Unsplash and Pexels keys in use.
    fn keys(&self) -> Keys {
        self.config.keys(self.env_keys)
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
                // The settings screen writes the file too, and may have saved
                // since this was queued: change only `paused` on disk, then
                // adopt the file, so its settings aren't lost.
                match self.save_paused(paused) {
                    Ok(()) => self.reload(),
                    Err(e) => self.fail(e),
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
            .then(|| self.state.last_rotated.saturating_add(self.interval_secs()));
        let keys = self.keys();
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
            has_unsplash_key: !keys.unsplash.is_empty(),
            has_pexels_key: !keys.pexels.is_empty(),
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
        let env = self.config.keys(true);
        let saved = self.config.keys(false);
        net::scrub(
            text,
            &[&env.unsplash, &env.pexels, &saved.unsplash, &saved.pexels],
        )
    }

    fn save_paused(&self, paused: bool) -> Result<()> {
        Config::update(&self.paths.config_file, |config| {
            config.paused = paused;
            Ok(())
        })
        .map(drop)
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
                    self.prefetch_over_cap = None;
                }
                if config.cache_cap_mb != self.config.cache_cap_mb {
                    self.prefetch_over_cap = None;
                }
                if config.keys(self.env_keys) != self.keys() {
                    self.last_failed_fetch = 0;
                    self.resting.clear();
                }
                self.config = config;
                self.on_fallback_defaults = false;
                self.error = None;
                self.save_state();
                self.cleanup();
            }
            Err(e) => self.fail(e),
        }
    }

    /// Asks the platform for the displays, keeping the last known list if
    /// it can't say. Logs each new list with every display's size and scale.
    fn live_displays(&mut self) -> Vec<Display> {
        let live = (self.hooks.displays)();
        if !live.is_empty() && live != self.displays {
            let list: Vec<String> = live.iter().map(Display::describe).collect();
            self.log(&format!("displays: {}", list.join("; ")));
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
            && now() >= self.state.last_rotated.saturating_add(self.interval_secs())
            && now() >= self.last_failed_fetch + RETRY_AFTER_SECS
    }

    /// The rotation interval in seconds; a huge hand-edited value saturates.
    fn interval_secs(&self) -> u64 {
        self.config.interval_minutes.saturating_mul(60)
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
                    if self.prefetch_over_cap.as_ref() != Some(&self.displays) {
                        self.prefetch_over_cap = Some(self.displays.clone());
                        self.log(
                            "prefetch paused: the cache cap is too small for a prefetched photo",
                        );
                    }
                }
            }
            Err(e) => self.fail(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::rotation::largest;
    use crate::service::test_support::{cached, screen, service};
    use std::fs;

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
    fn huge_intervals_saturate_instead_of_overflowing() {
        let dir = tempfile::tempdir().unwrap();
        let mut svc = service(dir.path());
        svc.config.access_key = "k".into();
        svc.state.current = Some(cached(&svc, "cur"));
        svc.state.last_rotated = now();
        // The first interval whose seconds don't fit in a u64, and the largest.
        for minutes in [u64::MAX / 60 + 1, u64::MAX] {
            svc.config.interval_minutes = minutes;
            assert!(!svc.rotation_due(), "{minutes} minutes");
            assert_eq!(svc.snapshot().next_rotation_at, Some(u64::MAX));
        }
    }

    #[test]
    fn largest_covers_every_display() {
        let screens = [screen("1", 3024, 1964), screen("2", 2560, 2880)];
        assert_eq!(largest(&screens), Some((3024, 2880)));
        assert_eq!(largest(&[]), None);
    }
}
