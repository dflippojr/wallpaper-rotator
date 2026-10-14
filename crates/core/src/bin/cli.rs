//! Headless control for testing and scripting:
//!
//!   wallpaper-rotator-cli rotate   set a new wallpaper now
//!   wallpaper-rotator-cli status   show the current wallpaper and cache size
//!   wallpaper-rotator-cli cleanup  enforce the cache rules
//!   wallpaper-rotator-cli paths    show where files live
//!
//! Don't run `rotate` while the tray app is running; both would manage the
//! same cache.

use std::fs;

use anyhow::{bail, Result};
use rotator_core::config::Config;
use rotator_core::paths::Paths;
use rotator_core::service::{Command, Hooks, Service};
use rotator_core::store::{self, State};
use rotator_core::wallpaper;

fn main() -> Result<()> {
    let paths = Paths::discover()?;
    match std::env::args().nth(1).as_deref() {
        Some("rotate") => rotate(paths),
        Some("status") => status(&paths),
        Some("cleanup") => {
            let config = Config::load_or_create(&paths.config_file)?;
            let mut state = State::load(&paths.state_file);
            let report = store::cleanup(&paths.images_dir, &mut state, config.cache_cap_bytes())?;
            state.save(&paths.state_file)?;
            println!(
                "removed {} files ({} KB)",
                report.removed_files,
                report.removed_bytes / 1024
            );
            Ok(())
        }
        Some("paths") => {
            println!("config:    {}", paths.config_file.display());
            println!("state:     {}", paths.state_file.display());
            println!("images:    {}", paths.images_dir.display());
            println!("log:       {}", paths.log_file.display());
            println!("favorites: {}", paths.default_favorites_dir.display());
            Ok(())
        }
        _ => bail!("usage: wallpaper-rotator-cli <rotate|status|cleanup|paths>"),
    }
}

fn rotate(paths: Paths) -> Result<()> {
    let hooks = Hooks {
        apply: Box::new(wallpaper::set),
        screen_size: Box::new(|| {
            let size = std::env::var("ROTATOR_SIZE").ok()?;
            let (w, h) = size.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        }),
        notify: Box::new(|_| {}),
    };
    let mut service = Service::new(paths.clone(), hooks)?;
    service.handle(Command::Next);
    let snap = service.snapshot();
    if let Some(error) = snap.error {
        bail!(error);
    }
    if let Some(w) = snap.current {
        println!("set photo {} by {} ({})", w.id, w.author, w.photo_url);
    }
    status(&paths)
}

fn status(paths: &Paths) -> Result<()> {
    let state = State::load(&paths.state_file);
    match &state.current {
        Some(w) => println!("current: {} by {} -> {}", w.id, w.author, w.file.display()),
        None => println!("current: none"),
    }
    if let Some(w) = &state.next {
        println!("next:    {} by {}", w.id, w.author);
    }
    let (count, bytes) = fs::read_dir(&paths.images_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .fold((0, 0u64), |(c, b), m| (c + 1, b + m.len()))
        })
        .unwrap_or((0, 0));
    println!(
        "cache:   {count} files, {:.1} MB",
        bytes as f64 / 1024.0 / 1024.0
    );
    Ok(())
}
