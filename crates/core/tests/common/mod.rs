//! A worker wired to a local stub API, a temporary folder and fake
//! displays, driven only through its public surface.

pub mod stub_api;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use image::{Rgb, RgbImage};
use rotator_core::config::Config;
use rotator_core::paths::Paths;
use rotator_core::service::{Command, Hooks, Service, Snapshot};
use rotator_core::source::SourceId;
use rotator_core::store::State;
use rotator_core::wallpaper::{Display, Layout};
use tempfile::TempDir;

use stub_api::StubApi;

pub const UNSPLASH_KEY: &str = "test-key";
pub const PEXELS_KEY: &str = "test-pexels-key";

pub struct World {
    pub api: StubApi,
    pub paths: Paths,
    pub svc: Service,
    /// The displays the worker sees; change it to plug screens in or out.
    pub screens: Arc<Mutex<Vec<Display>>>,
    /// Makes setting the wallpaper fail while set.
    pub apply_fails: Arc<AtomicBool>,
    applied: Arc<Mutex<Vec<Layout>>>,
    _dir: TempDir,
}

impl World {
    pub fn new(config: Config, screens: Vec<Display>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        config.save(&paths.config_file).unwrap();
        let api = StubApi::start();
        let applied = Arc::new(Mutex::new(Vec::new()));
        let screens = Arc::new(Mutex::new(screens));
        let apply_fails = Arc::new(AtomicBool::new(false));
        let hooks = Hooks {
            apply: Box::new({
                let (applied, fails) = (applied.clone(), apply_fails.clone());
                move |layout| {
                    if fails.load(Ordering::SeqCst) {
                        anyhow::bail!("no display");
                    }
                    applied.lock().unwrap().push(layout.clone());
                    Ok(())
                }
            }),
            displays: Box::new({
                let screens = screens.clone();
                move || screens.lock().unwrap().clone()
            }),
            notify: Box::new(|_| {}),
        };
        let svc = Service::with_http(paths.clone(), hooks, api.http()).unwrap();
        Self {
            api,
            paths,
            svc,
            screens,
            apply_fails,
            applied,
            _dir: dir,
        }
    }

    pub fn run(&mut self, command: Command) -> Snapshot {
        self.svc.handle(command);
        self.svc.snapshot()
    }

    /// Saves a changed config, as the settings screen does, and reloads.
    pub fn configure(&mut self, change: impl FnOnce(&mut Config)) -> Snapshot {
        let mut config = Config::load_or_create(&self.paths.config_file).unwrap();
        change(&mut config);
        config.save(&self.paths.config_file).unwrap();
        self.run(Command::Reload)
    }

    /// The state as saved on disk.
    pub fn state(&self) -> State {
        State::load(&self.paths.state_file)
    }

    /// Every layout the worker set, oldest first.
    pub fn applied(&self) -> Vec<Layout> {
        self.applied.lock().unwrap().clone()
    }

    /// Files in the image cache, sorted.
    pub fn cached_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.paths.images_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        files
    }

    pub fn favorites_dir(&self) -> PathBuf {
        self.paths.default_favorites_dir.clone()
    }

    /// Puts 300x200 PNGs in the favorites folder.
    pub fn add_favorites(&self, names: &[&str]) {
        let dir = self.favorites_dir();
        fs::create_dir_all(&dir).unwrap();
        for name in names {
            RgbImage::from_pixel(300, 200, Rgb([10, 120, 200]))
                .save(dir.join(name))
                .unwrap();
        }
    }
}

/// A config using `sources`, with both keys set.
pub fn config(sources: &[SourceId]) -> Config {
    Config {
        access_key: UNSPLASH_KEY.into(),
        pexels_key: PEXELS_KEY.into(),
        sources: sources.to_vec(),
        ..Config::default()
    }
}

pub fn screen(id: &str, width: u32, height: u32) -> Display {
    Display {
        id: id.into(),
        name: format!("Screen {id}"),
        x: 0,
        y: 0,
        width,
        height,
    }
}
