//! Contract tests between the table of sources (`source::SPECS`, #95) and
//! what depends on it outside Rust: the CSP in `app/tauri.conf.json`, which
//! Tauri reads at build time, and the hosts the sources' real replies use.
//!
//! A preview host missing from the CSP only shows as a blank preview in the
//! popover, so nothing else would catch it.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use rotator_core::source::{
    daily, pexels, spec, unsplash, wallhaven, Candidate, SourceId, BING, SPECS,
};
use serde_json::Value;

/// What `img-src` allows besides the sources: the page itself and the
/// cached files (favorites' previews) through the asset protocol.
const LOCAL_IMAGES: [&str; 3] = ["'self'", "asset:", "http://asset.localhost"];

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The sources of `directive` in the app's CSP, in order.
fn csp_sources(directive: &str) -> Vec<String> {
    let config: Value = serde_json::from_str(&repo_file("app/tauri.conf.json")).unwrap();
    let csp = config["app"]["security"]["csp"]
        .as_str()
        .expect("app.security.csp in app/tauri.conf.json");
    let found = csp
        .split(';')
        .map(str::trim)
        .find_map(|d| d.strip_prefix(directive)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("the CSP has no {directive}: {csp}"));
    found.split_whitespace().map(String::from).collect()
}

#[test]
fn csp_img_src_is_exactly_the_tables_preview_hosts() {
    let listed = csp_sources("img-src");
    let unique: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        listed.len(),
        "img-src repeats a host: {listed:?}"
    );

    let mut expected: BTreeSet<&str> = LOCAL_IMAGES.into_iter().collect();
    expected.extend(SPECS.iter().flat_map(|s| s.preview_hosts.iter().copied()));

    let missing: Vec<_> = expected.difference(&unique).collect();
    let extra: Vec<_> = unique.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "app/tauri.conf.json's img-src must be {LOCAL_IMAGES:?} plus every source's \
         preview_hosts.\n  missing from the CSP: {missing:?}\n  not in the table: {extra:?}"
    );
}

/// The candidates each recorded reply gives, by source.
fn recorded() -> Vec<(SourceId, Vec<Candidate>)> {
    let fixture = |path: &str| repo_file(&format!("crates/core/tests/fixtures/sources/{path}"));
    let mut daily = daily::parse_bing(BING.base, &fixture("daily/bing.json")).unwrap();
    daily.extend(daily::parse_wikimedia(&fixture("daily/wikimedia.json")).unwrap());
    vec![
        (
            SourceId::Unsplash,
            unsplash::parse_batch(&fixture("unsplash/random.json")).unwrap(),
        ),
        (
            SourceId::Pexels,
            pexels::parse_batch(&fixture("pexels/search.json"))
                .unwrap()
                .0,
        ),
        (
            SourceId::Wallhaven,
            wallhaven::parse_search(&fixture("wallhaven/search.json")).unwrap(),
        ),
        (SourceId::Daily, daily),
    ]
}

#[test]
fn recorded_previews_load_from_the_tables_preview_hosts() {
    for (source, photos) in recorded() {
        assert!(!photos.is_empty(), "{source:?} fixture has no photos");
        let hosts = spec(source).preview_hosts;
        for photo in photos {
            let url = &photo.preview_url;
            assert!(
                hosts.iter().any(|h| url.starts_with(&format!("{h}/"))),
                "{source:?} preview {url} is not on {hosts:?}"
            );
        }
    }
}

#[test]
fn recorded_credit_links_are_ones_the_app_may_open() {
    for (source, photos) in recorded() {
        let prefixes = spec(source).link_prefixes;
        for photo in photos {
            for url in [&photo.photo_url, &photo.author_url] {
                assert!(
                    prefixes.iter().any(|p| url.starts_with(p)),
                    "{source:?} link {url} is not under {prefixes:?}"
                );
            }
        }
    }
}
