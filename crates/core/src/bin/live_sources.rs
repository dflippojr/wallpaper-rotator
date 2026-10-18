//! The nightly live check: one real request per photo source through the
//! app's own code, then one image downloaded and decoded.
//!
//!   live-sources [--size WxH] [--issue FILE]
//!
//! Prints a Markdown report (and appends it to `$GITHUB_STEP_SUMMARY` when
//! set). Keyed sources run only with `UNSPLASH_ACCESS_KEY` or
//! `PEXELS_API_KEY`; the others are reported NOT VERIFIED, never passed.
//! When a source fails, writes the issue body to FILE and exits 1.

use std::fs::{self, OpenOptions};
use std::io::Write;

use anyhow::{bail, Context, Result};
use rotator_core::net::Http;
use rotator_core::probe::{self, Keys};

fn main() -> Result<()> {
    let mut size = (1920, 1080);
    let mut issue = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--size", Some(value)) => {
                let (w, h) = value.split_once('x').context("--size is WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            ("--issue", Some(path)) => issue = Some(path),
            _ => bail!("usage: live-sources [--size WxH] [--issue FILE]"),
        }
    }

    // Keys only from the environment: CI's secrets, never a saved config.
    let keys = Keys::load(None);
    let checks = probe::live(&Http::default(), &keys, size, rotator_core::now());
    let report = probe::summary(&checks);
    println!("{report}");
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(report.as_bytes())?;
    }
    if let Some(line) = probe::not_verified(&checks) {
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            println!("::warning title=Live source check::{line}");
        }
    }
    match (probe::issue_body(&checks), issue) {
        (None, _) => Ok(()),
        (Some(body), path) => {
            if let Some(path) = path {
                fs::write(path, &body)?;
            }
            bail!("a photo source failed the live check")
        }
    }
}
