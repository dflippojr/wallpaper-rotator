//! Headless control for testing and scripting:
//!
//!   wallpaper-rotator-cli rotate   set a new wallpaper now (ROTATOR_SIZE=WxH
//!                                  sizes it where displays can't be listed)
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
use rotator_core::wallpaper::{self, Display};

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
        displays: Box::new(|| {
            wallpaper::displays()
                .or_else(size_from_env)
                .unwrap_or_default()
        }),
        notify: Box::new(|_| {}),
    };
    let mut service = Service::new(paths.clone(), hooks)?;
    service.handle(Command::Next);
    let snap = service.snapshot();
    if let Some(error) = snap.error {
        bail!(error);
    }
    for w in snap.current {
        let place = w.display.map(|d| format!(" on {d}")).unwrap_or_default();
        println!(
            "set photo {} by {}{place} ({})",
            w.id, w.author, w.photo_url
        );
    }
    status(&paths)
}

fn size_from_env() -> Option<Vec<Display>> {
    let size = std::env::var("ROTATOR_SIZE").ok()?;
    let (w, h) = size.split_once('x')?;
    Some(vec![Display {
        id: "screen".into(),
        name: "Screen".into(),
        x: 0,
        y: 0,
        width: w.parse().ok()?,
        height: h.parse().ok()?,
    }])
}

fn status(paths: &Paths) -> Result<()> {
    let state = State::load(&paths.state_file);
    let describe = |w: &store::Wallpaper| {
        let place = w
            .display
            .as_ref()
            .map(|d| format!(" on {}", d.name))
            .unwrap_or_default();
        format!("{} by {}{place}", w.id, w.author)
    };
    match &state.current {
        Some(set) => {
            for w in &set.photos {
                println!("current: {} -> {}", describe(w), w.file.display());
            }
            if let Some(spanned) = &set.spanned {
                println!("stitched: {}", spanned.display());
            }
        }
        None => println!("current: none"),
    }
    for w in state.next.iter().flat_map(|set| &set.photos) {
        println!("next:    {}", describe(w));
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
