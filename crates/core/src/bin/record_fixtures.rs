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

    let config = Paths::discover().ok().map(|p| p.config_file);
    let keys = Keys::load(config.as_deref());
    probe::record_sources(
        &Http::default(),
        &keys,
        &out,
        &only,
        rotator_core::now(),
        &mut |line| println!("{line}"),
    )
}
