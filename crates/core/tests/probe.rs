//! The fixture recorder and the live check, run against the local stub
//! API instead of the real services.

// Only part of the stub is used here; the worker journeys use the rest.
#[allow(dead_code)]
#[path = "common/stub_api.rs"]
mod stub_api;

use std::fs;

use rotator_core::probe::{self, Check, Failure, Keys, Outcome};
use stub_api::{Route, StubApi};

fn keys() -> Keys {
    Keys {
        unsplash: "test-key".into(),
        pexels: "test-pexels-key".into(),
    }
}

fn outcome<'a>(checks: &'a [Check], source: &str) -> &'a Outcome {
    &checks.iter().find(|c| c.source == source).unwrap().outcome
}

fn failure<'a>(checks: &'a [Check], source: &str) -> &'a Failure {
    match outcome(checks, source) {
        Outcome::Failed(f) => f,
        other => panic!("{source} should fail, got {other:?}"),
    }
}

#[test]
fn records_every_fixture_through_the_app_parsers() {
    let api = StubApi::start();
    let http = api.http();
    let dir = tempfile::tempdir().unwrap();
    let keys = keys();

    let (fixtures, skipped) = probe::fixtures(&http, &keys, rotator_core::now());
    assert!(skipped.is_empty());
    for fixture in &fixtures {
        assert!(probe::record(&http, fixture, &keys, dir.path()).unwrap() > 0);
    }
    let random = fs::read_to_string(dir.path().join("unsplash/random.json")).unwrap();
    let download = probe::unsplash_download(&keys, &random).unwrap();
    probe::record(&http, &download, &keys, dir.path()).unwrap();

    let mut written: Vec<_> = walk(dir.path());
    written.sort();
    assert_eq!(
        written,
        [
            "daily/bing.json",
            "daily/wikimedia.json",
            "pexels/curated.json",
            "pexels/search.json",
            "unsplash/download.json",
            "unsplash/random.json",
            "unsplash/search.json",
            "unsplash/topic.json",
            "wallhaven/search.json",
        ]
    );
    // Recorded with the app's own requests and headers.
    let search = api.to(Route::UnsplashRandom).pop().unwrap();
    assert_eq!(search.param("query"), Some("mountains"));
    assert_eq!(search.header("Authorization"), Some("Client-ID test-key"));
    let pexels = api.to(Route::PexelsSearch).pop().unwrap();
    assert_eq!(pexels.header("Authorization"), Some("test-pexels-key"));
    // Lists are cut to a few photos, and account fields are dropped.
    let pexels = fs::read_to_string(dir.path().join("pexels/search.json")).unwrap();
    assert!(!pexels.contains("\"liked\""), "{pexels}");
}

#[test]
fn skips_keyed_sources_without_a_key() {
    let api = StubApi::start();
    let (fixtures, skipped) = probe::fixtures(&api.http(), &Keys::default(), 0);
    assert_eq!(skipped, ["Unsplash", "Pexels"]);
    let paths: Vec<_> = fixtures.iter().map(|f| f.path).collect();
    assert_eq!(
        paths,
        [
            "wallhaven/search.json",
            "daily/bing.json",
            "daily/wikimedia.json"
        ]
    );
}

#[test]
fn refuses_to_write_a_fixture_that_holds_a_key() {
    let api = StubApi::start();
    let http = api.http();
    let dir = tempfile::tempdir().unwrap();
    // A key that happens to appear in the Wallhaven reply.
    let keys = Keys {
        unsplash: "o3v5kl".into(),
        pexels: String::new(),
    };
    let (fixtures, _) = probe::fixtures(&http, &keys, 0);
    let wallhaven = fixtures
        .iter()
        .find(|f| f.path == "wallhaven/search.json")
        .unwrap();

    let error = probe::record(&http, wallhaven, &keys, dir.path()).unwrap_err();

    assert!(error.to_string().contains("contains an API key"), "{error}");
    assert!(walk(dir.path()).is_empty());
}

#[test]
fn recording_stops_on_an_error_status() {
    let api = StubApi::start();
    let http = api.http();
    api.status(Route::Bing, 500);
    let dir = tempfile::tempdir().unwrap();
    let (fixtures, _) = probe::fixtures(&http, &Keys::default(), 0);
    let bing = fixtures
        .iter()
        .find(|f| f.path == "daily/bing.json")
        .unwrap();

    let error = probe::record(&http, bing, &Keys::default(), dir.path()).unwrap_err();

    assert!(error.to_string().contains("HTTP 500"), "{error}");
    assert!(walk(dir.path()).is_empty());
}

#[test]
fn live_check_passes_every_source_and_reports_the_unsplash_download() {
    let api = StubApi::start();

    let checks = probe::live(&api.http(), &keys(), (320, 200), rotator_core::now());

    for c in &checks {
        assert!(
            matches!(&c.outcome, Outcome::Passed { author, size, .. } if !author.is_empty() && size.0 >= 320),
            "{c:?}"
        );
    }
    assert_eq!(checks.len(), probe::SOURCES.len());
    assert_eq!(probe::not_verified(&checks), None);
    assert_eq!(probe::issue_body(&checks), None);
    // One request per source, as the app sends it.
    assert_eq!(api.to(Route::UnsplashRandom).len(), 1);
    assert_eq!(api.to(Route::PexelsCurated).len(), 1);
    assert_eq!(api.to(Route::WallhavenSearch).len(), 1);
    assert_eq!(api.to(Route::Bing).len(), 1);
    assert_eq!(api.to(Route::Wikimedia).len(), 1);
    let reports = api.to(Route::UnsplashDownload);
    assert_eq!(reports.len(), 1, "Unsplash's terms: report the download");
    assert_eq!(
        reports[0].header("Authorization"),
        Some("Client-ID test-key")
    );
}

#[test]
fn live_check_without_keys_does_not_call_keyed_sources() {
    let api = StubApi::start();

    let checks = probe::live(&api.http(), &Keys::default(), (320, 200), 0);

    assert_eq!(
        *outcome(&checks, "Unsplash"),
        Outcome::NotVerified("no key")
    );
    assert_eq!(*outcome(&checks, "Pexels"), Outcome::NotVerified("no key"));
    assert!(matches!(outcome(&checks, "Bing"), Outcome::Passed { .. }));
    assert_eq!(
        probe::not_verified(&checks).as_deref(),
        Some("NOT VERIFIED: Unsplash, Pexels (no key)")
    );
    assert!(api.to(Route::UnsplashRandom).is_empty());
    assert!(api.to(Route::PexelsCurated).is_empty());
}

#[test]
fn live_check_names_the_status_and_the_field_that_failed() {
    let api = StubApi::start();
    api.status(Route::WallhavenSearch, 500);
    api.reply(Route::Bing, r#"{"images": [{"startdate": "20261018"}]}"#);
    api.status(Route::UnsplashDownload, 401);

    let checks = probe::live(&api.http(), &keys(), (320, 200), 0);

    let wallhaven = failure(&checks, "Wallhaven");
    assert_eq!((wallhaven.field, wallhaven.status), ("request", Some(500)));
    let bing = failure(&checks, "Bing");
    assert_eq!((bing.field, bing.status), ("reply", Some(200)));
    assert!(bing.detail.contains("missing field `urlbase`"), "{bing:?}");
    let unsplash = failure(&checks, "Unsplash");
    assert_eq!(
        (unsplash.field, unsplash.status),
        ("download report", Some(401))
    );
    assert!(matches!(outcome(&checks, "Pexels"), Outcome::Passed { .. }));

    let issue = probe::issue_body(&checks).unwrap();
    assert!(
        issue.contains("**Wallhaven**: `request` failed (HTTP 500)"),
        "{issue}"
    );
    assert!(
        issue.contains("**Bing**: `reply` failed (HTTP 200)"),
        "{issue}"
    );
    assert!(!issue.contains("Pexels"));
}

#[test]
fn live_check_fails_images_that_are_missing_or_too_small() {
    let api = StubApi::start();

    // The stub serves 400x300 when the URL doesn't ask for a size.
    let checks = probe::live(&api.http(), &Keys::default(), (1920, 1080), 0);
    let wallhaven = failure(&checks, "Wallhaven");
    assert_eq!(wallhaven.field, "image size");
    assert!(wallhaven.detail.contains("400x300"), "{wallhaven:?}");

    api.status(Route::Image, 404);
    let checks = probe::live(&api.http(), &Keys::default(), (320, 200), 0);
    let wikimedia = failure(&checks, "Wikimedia");
    assert_eq!((wikimedia.field, wikimedia.status), ("image", Some(404)));

    api.status(Route::Image, 200);
    api.reply(Route::Image, "not an image");
    let checks = probe::live(&api.http(), &Keys::default(), (320, 200), 0);
    let bing = failure(&checks, "Bing");
    assert_eq!(bing.field, "image");
    assert!(bing.detail.contains("did not decode"), "{bing:?}");
}

#[test]
fn live_check_needs_a_photo_with_a_credit() {
    let api = StubApi::start();
    api.reply(Route::WallhavenSearch, r#"{"data": []}"#);
    api.reply(
        Route::Bing,
        r#"{"images": [{"startdate": "20261018", "urlbase": "/th?id=OHR.X_EN-US1", "copyright": "(©)"}]}"#,
    );

    let checks = probe::live(&api.http(), &Keys::default(), (320, 200), 0);

    let wallhaven = failure(&checks, "Wallhaven");
    assert_eq!(
        (wallhaven.field, wallhaven.detail.as_str()),
        ("reply", "no usable photo")
    );
    assert_eq!(failure(&checks, "Bing").field, "author");
}

/// Files under `dir`, as `/`-separated relative paths.
fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            out.extend(walk(&path).into_iter().map(|p| format!("{name}/{p}")));
        } else {
            out.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    out
}
