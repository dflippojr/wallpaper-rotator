//! Records the photo APIs' real replies as the test fixtures:
//!
//!   record-fixtures [--out DIR] [SOURCE...]
//!
//! SOURCE is unsplash, pexels, wallhaven, bing or wikimedia (default: all).
//! Keys come from `UNSPLASH_ACCESS_KEY` and `PEXELS_API_KEY`, else from the
//! app's saved settings; a keyed source without a key is skipped. Writes to
//! `crates/core/tests/fixtures/sources/` unless `--out` names a folder.
//! Refuses to write a fixture that contains a key, and never prints one.

use std::path::PathBuf;

use anyhow::{bail, Result};
use rotator_core::net::Http;
use rotator_core::paths::Paths;
use rotator_core::probe::{self, Keys};

fn main() -> Result<()> {
    let mut out = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sources"
    ));
    let mut only = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => match args.next() {
                Some(dir) => out = dir.into(),
                None => bail!("--out needs a folder"),
            },
            "unsplash" | "pexels" | "wallhaven" | "bing" | "wikimedia" => only.push(arg),
            _ => bail!(
                "usage: record-fixtures [--out DIR] [unsplash|pexels|wallhaven|bing|wikimedia...]"
            ),
        }
    }
    let wanted = |path: &str| {
        only.is_empty()
            || only.iter().any(|s| {
                path.starts_with(&format!("{s}/")) || path.ends_with(&format!("/{s}.json"))
            })
    };

    let config = Paths::discover().ok().map(|p| p.config_file);
    let keys = Keys::load(config.as_deref());
    let http = Http::default();
    let (fixtures, skipped) = probe::fixtures(&http, &keys, rotator_core::now());
    for source in skipped {
        if wanted(&format!("{}/", source.to_lowercase())) {
            println!("skipped {source}: no key");
        }
    }
    for fixture in fixtures.iter().filter(|f| wanted(f.path)) {
        let photos = probe::record(&http, fixture, &keys, &out)?;
        println!("wrote {} ({photos})", fixture.path);
        if fixture.path == "unsplash/random.json" {
            let random = std::fs::read_to_string(out.join(fixture.path))?;
            let download = probe::unsplash_download(&keys, &random)?;
            probe::record(&http, &download, &keys, &out)?;
            println!("wrote {}", download.path);
        }
    }
    Ok(())
}
