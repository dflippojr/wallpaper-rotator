//! The rotation worker. One thread owns the config, state and cache; the UI
//! talks to it through [`Command`]s and gets [`Snapshot`]s back, so nothing
//! else touches the images folder.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::paths::Paths;
use crate::store::{self, Set, State, Wallpaper};
use crate::unsplash::{self, Client, Photo};
use crate::wallpaper::{self, Display, Layout};
use crate::{log, now};

const TICK: Duration = Duration::from_secs(60);
/// Wait this long after a failed fetch before trying again on a tick.
const RETRY_AFTER_SECS: u64 = 15 * 60;
const FALLBACK_SIZE: (u32, u32) = (3840, 2160);
/// Random photos to ask for per display, to pick ones big enough.
const CANDIDATES_PER_DISPLAY: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Rotate now.
    Next,
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
    /// The display it is on; `None` when one photo covers every display.
    pub display: Option<String>,
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
            display: w.display.as_ref().map(|d| d.name.clone()),
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
    #[serde(default)]
    pub per_display: bool,
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
            per_display: c.per_display,
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
        config.per_display = self.per_display;
        if let Some(key) = new_key.map(str::trim).filter(|k| !k.is_empty()) {
            config.access_key = key.to_string();
        }
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
    /// Displays as of the last check, to notice monitors coming and going.
    displays: Vec<Display>,
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
            displays: Vec::new(),
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
            Command::Favorite(index) => {
                match self.favorite(index) {
                    Ok(saved) => self.notice = Some(saved),
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
                self.check_displays();
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
            has_key: !self.config.effective_key().is_empty(),
            settings: Settings::from_config(&self.config),
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
        let message = format!("{e:#}");
        log::write(&self.paths.log_file, &format!("error: {message}"));
        self.error = Some(message);
    }

    fn reload(&mut self) {
        match Config::load_or_create(&self.paths.config_file) {
            Ok(config) => {
                if !config.same_source(&self.config) {
                    // The prefetched photos came from the old settings.
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
        log::write(
            &self.paths.log_file,
            &format!("displays changed: {} connected", live.len()),
        );
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
        if self.config.effective_key().is_empty() {
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
        let displays = self.live_displays();
        let each = self.each(&displays);
        let next = match self.state.next.take() {
            Some(set) if set.on_disk() && set.fits(&displays, each) => set,
            _ => self.fetch(&client, &displays, each)?,
        };
        if let Err(e) = (self.hooks.apply)(&next.layout()) {
            self.state.next = Some(next);
            return Err(e);
        }
        for photo in &next.photos {
            if let Err(e) = client.track_download(&photo.download_location) {
                log::write(
                    &self.paths.log_file,
                    &format!("download tracking failed: {e:#}"),
                );
            }
            log::write(
                &self.paths.log_file,
                &format!("wallpaper set: {} by {}", photo.id, photo.author),
            );
        }
        self.state.current = Some(next);
        self.state.last_rotated = now();
        self.save_state();
        self.cleanup();
        Ok(())
    }

    fn prefetch_if_needed(&mut self) {
        if self.config.effective_key().is_empty()
            || now() < self.last_failed_fetch + RETRY_AFTER_SECS
        {
            return;
        }
        let displays = self.live_displays();
        let each = self.each(&displays);
        let ready = self
            .state
            .next
            .as_ref()
            .is_some_and(|s| s.on_disk() && s.fits(&displays, each));
        if ready {
            return;
        }
        let result = self
            .client()
            .and_then(|client| self.fetch(&client, &displays, each));
        match result {
            Ok(set) => {
                self.state.next = Some(set);
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

    /// Picks random photos big enough for their displays (from one batch of
    /// candidates) and downloads each cropped to its display's size: one
    /// photo for all displays, or one per display when `each` is set.
    fn fetch(&mut self, client: &Client, displays: &[Display], each: bool) -> Result<Set> {
        let shared = largest(displays).unwrap_or(FALLBACK_SIZE);
        let targets: Vec<Option<&Display>> = if each {
            displays.iter().map(Some).collect()
        } else {
            vec![None]
        };
        let sizes: Vec<(u32, u32)> = targets
            .iter()
            .map(|t| t.map_or(shared, |d| (d.width, d.height)))
            .collect();

        let topics = self.topic_ids(client)?;
        let count = targets.len() as u32 * CANDIDATES_PER_DISPLAY;
        let photos = client.random_photos(&self.config.query, &topics, count)?;
        let dims: Vec<(u32, u32)> = photos.iter().map(|p| (p.width, p.height)).collect();

        let mut set = Set {
            photos: Vec::new(),
            spanned: None,
        };
        let choices = pick(&dims, &sizes);
        for ((target, &(width, height)), choice) in targets.iter().zip(&sizes).zip(choices) {
            let photo = &photos[choice];
            let file = self
                .paths
                .images_dir
                .join(format!("{}-{width}x{height}.jpg", safe_name(&photo.id)));
            client.download(&unsplash::sized_url(&photo.urls.raw, width, height), &file)?;
            set.photos
                .push(wallpaper_from(photo.clone(), file, target.cloned()));
        }
        if each && wallpaper::SPANS_ONE_IMAGE {
            set = self.stitch(set, displays)?;
        }
        Ok(set)
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
        let dir = self.favorites_dir();
        fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let mut saved = Vec::new();
        for photo in photos {
            let dest = dir.join(format!(
                "{} - {}.jpg",
                safe_name(&photo.author),
                safe_name(&photo.id)
            ));
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

fn wallpaper_from(photo: Photo, file: PathBuf, display: Option<Display>) -> Wallpaper {
    Wallpaper {
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
        display,
    }
}

/// The size that covers every display: the widest width and tallest height.
fn largest(displays: &[Display]) -> Option<(u32, u32)> {
    let width = displays.iter().map(|d| d.width).max()?;
    let height = displays.iter().map(|d| d.height).max()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// Chooses a candidate photo (by index into `photos`) for each wanted size.
/// Bigger displays choose first; each takes the unused photo that covers it
/// best, so photos too small to fill a screen are a last resort. Photos are
/// reused only if there are fewer than displays.
fn pick(photos: &[(u32, u32)], sizes: &[(u32, u32)]) -> Vec<usize> {
    let coverage = |photo: (u32, u32), size: (u32, u32)| {
        let w = f64::from(photo.0) / f64::from(size.0.max(1));
        let h = f64::from(photo.1) / f64::from(size.1.max(1));
        w.min(h).min(1.0)
    };
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(u64::from(sizes[i].0) * u64::from(sizes[i].1)));
    let mut used = vec![false; photos.len()];
    let mut picks = vec![0; sizes.len()];
    for i in order {
        let best = |candidates: &mut dyn Iterator<Item = usize>| {
            candidates.fold(None, |best: Option<usize>, j| match best {
                Some(b) if coverage(photos[b], sizes[i]) >= coverage(photos[j], sizes[i]) => {
                    Some(b)
                }
                _ => Some(j),
            })
        };
        let choice = best(&mut (0..photos.len()).filter(|&j| !used[j]))
            .or_else(|| best(&mut (0..photos.len())))
            .unwrap_or(0);
        if let Some(slot) = used.get_mut(choice) {
            *slot = true;
        }
        picks[i] = choice;
    }
    picks
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
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    type Applied = Arc<Mutex<Vec<Layout>>>;

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

    fn service(root: &Path) -> (Service, Applied) {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let seen = applied.clone();
        let hooks = Hooks {
            apply: Box::new(move |layout| {
                seen.lock().unwrap().push(layout.clone());
                Ok(())
            }),
            displays: Box::new(|| vec![screen("1", 100, 100)]),
            notify: Box::new(|_| {}),
        };
        (Service::new(Paths::under(root), hooks).unwrap(), applied)
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
            download_location: "http://127.0.0.1:9/never".into(),
            color: None,
            display,
        }
    }

    fn cached(svc: &Service, id: &str) -> Set {
        Set::single(photo(svc, id, None))
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

        assert_eq!(*applied.lock().unwrap(), vec![next.layout()]);
        assert_eq!(svc.state.current.as_ref().unwrap().photos[0].id, "next");
        assert!(svc.state.next.is_none());
        assert!(
            !old.photos[0].file.exists(),
            "old wallpaper should be deleted"
        );
        assert!(next.on_disk());
        assert!(State::load(&svc.paths.state_file).current.is_some());
    }

    #[test]
    fn per_display_uses_a_prefetched_set_only_if_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, applied) = service(dir.path());
        let screens = vec![screen("1", 3024, 1964), screen("2", 3840, 2160)];
        let live = screens.clone();
        svc.hooks.displays = Box::new(move || live.clone());
        svc.config.access_key = "test-key".into();
        svc.config.per_display = true;
        svc.last_failed_fetch = now();
        let set = Set {
            photos: vec![
                photo(&svc, "a", Some(screens[0].clone())),
                photo(&svc, "b", Some(screens[1].clone())),
            ],
            spanned: None,
        };
        svc.state.next = Some(set.clone());
        svc.rotate().unwrap();
        assert_eq!(
            *applied.lock().unwrap(),
            vec![Layout::Each(vec![
                ("1".into(), set.photos[0].file.clone()),
                ("2".into(), set.photos[1].file.clone()),
            ])]
        );
        assert_eq!(svc.snapshot().current.len(), 2);
        assert_eq!(
            svc.snapshot().current[1].display.as_deref(),
            Some("Screen 2")
        );
        // A shared photo doesn't fit once per-display is on.
        assert!(!cached(&svc, "shared").fits(&screens, svc.each(&screens)));
    }

    #[test]
    fn display_changes_re_place_the_current_photos() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, applied) = service(dir.path());
        let screens = Arc::new(Mutex::new(vec![screen("1", 100, 100)]));
        let live = screens.clone();
        svc.hooks.displays = Box::new(move || live.lock().unwrap().clone());
        svc.state.current = Some(cached(&svc, "cur"));

        svc.check_displays();
        assert!(
            applied.lock().unwrap().is_empty(),
            "first look only records"
        );
        svc.check_displays();
        assert!(applied.lock().unwrap().is_empty(), "nothing changed");

        screens.lock().unwrap().push(screen("2", 200, 100));
        svc.check_displays();
        assert_eq!(applied.lock().unwrap().len(), 1);

        // An empty answer means "unknown", not "every display unplugged".
        screens.lock().unwrap().clear();
        svc.check_displays();
        assert_eq!(applied.lock().unwrap().len(), 1);
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
        svc.handle(Command::Favorite(Some(0)));
        let saved = dir.path().join("favorites").join("Ann _ Lee - cur.jpg");
        assert!(saved.exists());
        assert!(svc.snapshot().notice.unwrap().starts_with("Saved to"));
        // Cleanup never touches favorites.
        svc.cleanup();
        assert!(saved.exists());
    }

    #[test]
    fn favorites_can_save_every_display() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, _) = service(dir.path());
        svc.state.current = Some(Set {
            photos: vec![
                photo(&svc, "a", Some(screen("1", 1, 1))),
                photo(&svc, "b", Some(screen("2", 1, 1))),
            ],
            spanned: None,
        });
        svc.handle(Command::Favorite(None));
        assert!(svc.snapshot().notice.unwrap().starts_with("Saved 2 photos"));
        assert!(dir.path().join("favorites/Ann _ Lee - b.jpg").exists());
        svc.handle(Command::Favorite(Some(5)));
        assert!(svc.snapshot().error.is_some());
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
        fs::remove_file(&svc.state.current.as_ref().unwrap().photos[0].file).unwrap();
        assert!(svc.rotation_due());
    }

    #[test]
    fn changing_the_search_or_per_display_drops_the_prefetched_photo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, _) = service(dir.path());
        for change in [
            |c: &mut Config| c.query = "mountains".into(),
            |c: &mut Config| c.per_display = !c.per_display,
        ] {
            let next = cached(&svc, "next");
            svc.state.next = Some(next.clone());
            let mut config = svc.config.clone();
            change(&mut config);
            config.save(&svc.paths.config_file).unwrap();
            svc.reload();
            assert!(svc.state.next.is_none());
            assert!(!next.on_disk());
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
        settings.apply_to(&mut config, Some("  "));
        assert_eq!(config.topics, ["nature", "film"]);
        assert_eq!(config.cache_cap_mb, 20);
        assert_eq!(config.access_key, "old");
        assert!(config.per_display);
        settings.apply_to(&mut config, Some("new"));
        assert_eq!(config.access_key, "new");
    }

    #[test]
    fn picks_photos_that_cover_each_display() {
        let laptop = (3024, 1964);
        let monitor = (3840, 2160);
        // The 4K photo goes to the 4K monitor, the next best to the laptop.
        let photos = [(1920, 1080), (6000, 4000), (3200, 2000)];
        assert_eq!(pick(&photos, &[laptop, monitor]), vec![2, 1]);
        // Fewer photos than displays: the second display reuses one.
        assert_eq!(pick(&[(6000, 4000)], &[laptop, monitor]), vec![0, 0]);
        assert_eq!(pick(&photos, &[monitor]), vec![1]);
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
