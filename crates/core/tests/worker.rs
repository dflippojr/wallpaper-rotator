//! Journeys through the worker as the app and the CLI drive it: `Command`s
//! in, snapshots, saved state and files out, with every photo API answered
//! by a local stub.

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;

use common::stub_api::Route;
use common::{config, screen, World, PEXELS_KEY, UNSPLASH_KEY};
use image::{Rgb, RgbImage};
use rotator_core::config::Config;
use rotator_core::paths::Paths;
use rotator_core::service::{Command, Hooks, Service};
use rotator_core::source::SourceId;
use rotator_core::wallpaper::{Display, Layout};

/// The topic ID `unsplash/topic.json` gives for "wallpapers".
const WALLPAPERS_TOPIC: &str = "bo8jQKTaE0Y";

#[test]
fn first_run_without_a_key_opens_setup() {
    let mut w = World::new(
        Config {
            access_key: String::new(),
            sources: vec![SourceId::Unsplash],
            ..Config::default()
        },
        vec![screen("1", 160, 90)],
    );

    w.run(Command::Tick);
    let snap = w.run(Command::Next(None));

    assert!(snap.needs_setup);
    assert!(!snap.has_unsplash_key);
    let error = snap.error.expect("Next without a source says why");
    assert!(error.contains("photo source"), "{error}");
    assert!(w.applied().is_empty());
    assert!(w.api.requests().is_empty(), "{:?}", w.api.requests());
}

/// The popover opens on Settings only while `needs_setup` holds (#82), so
/// it must clear once a saved source can supply a photo.
#[test]
fn setup_is_no_longer_needed_once_a_source_can_supply_a_photo() {
    let mut w = World::new(
        Config {
            access_key: String::new(),
            sources: vec![SourceId::Unsplash],
            ..Config::default()
        },
        vec![screen("1", 160, 90)],
    );
    assert!(w.run(Command::Tick).needs_setup);

    // What the Settings screen saves: a keyless source ticked too.
    let mut saved = Config::load_or_create(&w.paths.config_file).unwrap();
    saved.sources = vec![SourceId::Unsplash, SourceId::Wallhaven];
    saved.save(&w.paths.config_file).unwrap();
    assert!(!w.run(Command::Reload).needs_setup, "a keyless source");

    // An Unsplash key alone also ends setup.
    let mut saved = Config::load_or_create(&w.paths.config_file).unwrap();
    saved.sources = vec![SourceId::Unsplash];
    saved.access_key = UNSPLASH_KEY.into();
    saved.save(&w.paths.config_file).unwrap();
    assert!(!w.run(Command::Reload).needs_setup, "an Unsplash key");
}

#[test]
fn rotate_then_next_uses_the_prefetched_photo() {
    let mut w = World::new(config(&[SourceId::Unsplash]), vec![screen("1", 160, 90)]);

    let first = w.run(Command::Next(None)).current[0].id.clone();
    let second = w.run(Command::Next(None)).current[0].id.clone();

    let applied = w.applied();
    let files: Vec<_> = applied
        .iter()
        .map(|layout| match layout {
            Layout::Same(file) => file.clone(),
            other => panic!("one photo for one display, got {other:?}"),
        })
        .collect();
    assert_eq!(files.len(), 2);
    assert_ne!(first, second);
    assert!(!files[0].exists(), "the old wallpaper is deleted");

    let state = w.state();
    let current = &state.current.as_ref().unwrap().photos[0];
    let next = &state.next.as_ref().expect("a photo is prefetched").photos[0];
    assert_eq!(current.id, second);
    assert_eq!(current.file, files[1]);
    assert_ne!(next.id, second);
    let mut kept = vec![current.file.clone(), next.file.clone()];
    kept.sort();
    assert_eq!(w.cached_files(), kept, "only current and next stay cached");
    assert_eq!(image::image_dimensions(&current.file).unwrap(), (160, 90));

    // One search fills the queue for both rotations and the prefetches.
    assert_eq!(w.api.to(Route::UnsplashTopic).len(), 1);
    let random = w.api.to(Route::UnsplashRandom);
    assert_eq!(random.len(), 1);
    assert_eq!(random[0].param("topics"), Some(WALLPAPERS_TOPIC));
    // Unsplash's terms: report each photo once, when it goes on screen.
    let downloads = w.api.to(Route::UnsplashDownload);
    let reported: Vec<&str> = downloads
        .iter()
        .map(|r| r.path.split('/').nth(2).unwrap())
        .collect();
    assert_eq!(reported, [first.as_str(), second.as_str()]);
    for request in random.iter().chain(&downloads) {
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.header("Authorization"),
            Some(format!("Client-ID {UNSPLASH_KEY}").as_str())
        );
        assert_eq!(request.header("Accept-Version"), Some("v1"));
    }
    assert!(downloads
        .iter()
        .all(|r| r.host.as_deref() == Some("api.unsplash.com")));
}

#[test]
fn rate_limited_source_rests_and_favorites_fill_in() {
    let mut w = World::new(
        config(&[SourceId::Unsplash, SourceId::Pexels]),
        vec![screen("1", 160, 90)],
    );
    w.add_favorites(&["Ann - a1.png"]);
    // Unsplash says its hourly limit is used up with a 403.
    w.api.status(Route::UnsplashRandom, 403);
    w.api.status(Route::PexelsCurated, 429);

    let snap = w.run(Command::Next(None));

    assert_eq!(snap.error, None);
    assert_eq!(snap.current[0].source, "Favorites");
    // Sources are tried in random order.
    let notice = snap.notice.unwrap();
    assert!(
        [
            "Unsplash is rate-limited and Pexels is rate-limited, so this one is from Favorites",
            "Pexels is rate-limited and Unsplash is rate-limited, so this one is from Favorites",
        ]
        .contains(&notice.as_str()),
        "{notice}"
    );
    // Resting sources aren't asked again.
    let asked = w.api.requests().len();
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.current[0].source, "Favorites");
    assert_eq!(w.api.requests().len(), asked);

    // A rate limit rests a source longer than other errors.
    for (status, says, rest_minutes) in [(429, "is rate-limited", 20), (500, "failed", 10)] {
        let mut w = World::new(config(&[SourceId::Pexels]), vec![screen("1", 160, 90)]);
        w.add_favorites(&["Ann - a1.png"]);
        w.api.status(Route::PexelsCurated, status);

        let snap = w.run(Command::Next(None));
        assert_eq!(
            snap.notice.as_deref(),
            Some(format!("Pexels {says}, so this one is from Favorites").as_str())
        );

        // With nothing left to fill in, the wait is reported.
        fs::remove_dir_all(w.favorites_dir()).unwrap();
        w.run(Command::Next(None)); // shows the prefetched photo
        let error = w.run(Command::Next(None)).error.unwrap();
        assert!(
            error.contains(&format!("trying again in {rest_minutes} min")),
            "{error}"
        );
        assert_eq!(w.api.to(Route::PexelsCurated).len(), 1);
    }
}

#[test]
fn topics_are_resolved_once_and_skipped_for_collections() {
    let mut w = World::new(config(&[SourceId::Unsplash]), vec![screen("1", 160, 90)]);
    let last_random = |w: &World| w.api.to(Route::UnsplashRandom).pop().unwrap();

    w.run(Command::Next(None));
    w.run(Command::Next(None));
    let topics = w.api.to(Route::UnsplashTopic);
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0].path, "/topics/wallpapers");
    assert_eq!(w.state().topic_ids["wallpapers"], WALLPAPERS_TOPIC);

    // Collections win over a photographer and topics.
    let snap = w.configure(|c| {
        c.collections = vec!["123".into()];
        c.username = "ann".into();
    });
    assert_eq!(
        snap.photographer_note.as_deref(),
        Some("Photographer is ignored while Collections is set.")
    );
    w.run(Command::Next(None));
    let random = last_random(&w);
    assert_eq!(random.param("collections"), Some("123"));
    assert_eq!(random.param("username"), None);
    assert_eq!(random.param("topics"), None);

    let snap = w.configure(|c| c.collections.clear());
    assert_eq!(snap.photographer_note, None);
    let random = last_random(&w);
    assert_eq!(random.param("username"), Some("ann"));
    assert_eq!(random.param("topics"), None);

    // Back to topics: the saved topic ID is reused.
    w.configure(|c| c.username.clear());
    assert_eq!(last_random(&w).param("topics"), Some(WALLPAPERS_TOPIC));
    assert_eq!(w.api.to(Route::UnsplashTopic).len(), 1);
}

#[test]
fn pexels_learns_the_page_count_and_drops_portraits() {
    let mut w = World::new(config(&[SourceId::Pexels]), vec![screen("1", 160, 90)]);

    let snap = w.run(Command::Next(None));

    let shown = &snap.current[0];
    assert_eq!(
        (shown.id.as_str(), shown.source.as_str()),
        ("417074", "Pexels")
    );
    assert_eq!(shown.description.as_deref(), Some("Lake and mountains"));
    // The first search learns there are 81 results (3 pages of 40); the
    // prefetch's refill picks one of those pages.
    let asks = w.api.to(Route::PexelsCurated);
    assert_eq!(asks.len(), 2);
    assert_eq!(asks[0].param("page"), Some("1"));
    let page: u64 = asks[1].param("page").unwrap().parse().unwrap();
    assert!((1..=3).contains(&page), "{page}");
    for ask in &asks {
        assert_eq!(ask.param("per_page"), Some("40"));
        assert_eq!(ask.header("Authorization"), Some(PEXELS_KEY));
    }
    let images = w.api.to(Route::Image);
    assert!(images.iter().any(|r| r.path.starts_with("/photos/417074/")));
    assert!(
        images.iter().all(|r| !r.path.starts_with("/photos/2/")),
        "the portrait is never downloaded"
    );
    assert!(images
        .iter()
        .all(|r| r.host.as_deref() == Some("images.pexels.com")));
}

#[test]
fn per_display_refresh_changes_only_that_display() {
    let screens = vec![screen("1", 120, 80), screen("2", 160, 90)];
    let mut w = World::new(
        Config {
            per_display: true,
            ..config(&[SourceId::Favorites])
        },
        screens.clone(),
    );
    w.add_favorites(&["Ann - a1.png", "Bo - b2.png", "Cy - c3.png", "Di - d4.png"]);

    let snap = w.run(Command::Next(None));
    assert_eq!(snap.current.len(), 2);
    assert_eq!(snap.current[1].display.as_deref(), Some("Screen 2"));
    let before = w.state();
    let before_set = before.current.clone().unwrap();
    let applies = w.applied().len();

    let snap = w.run(Command::Next(Some(1)));

    assert_eq!(snap.error, None);
    let after = w.state();
    let after_set = after.current.clone().unwrap();
    assert_eq!(
        after_set.photos[0], before_set.photos[0],
        "the other display keeps its photo"
    );
    let replaced = &after_set.photos[1];
    assert_ne!(replaced.id, before_set.photos[1].id);
    assert_eq!(replaced.display.as_ref().unwrap().id, "2");
    assert_eq!(image::image_dimensions(&replaced.file).unwrap(), (160, 90));
    // The replaced photo is deleted unless the prefetch picked it again.
    let mut kept: Vec<_> = [&after.current, &after.next]
        .into_iter()
        .flatten()
        .flat_map(|set| set.files().map(Path::to_path_buf))
        .collect();
    kept.sort();
    kept.dedup();
    assert_eq!(w.cached_files(), kept);
    assert_eq!(after.last_rotated, before.last_rotated, "same schedule");
    assert_eq!(w.applied().len(), applies + 1);
    // The prefetched set is whole again after lending one photo.
    assert!(after.next.unwrap().fits(&screens, true));

    // With per-display off, one photo covers both, and refreshing one
    // display rotates everything.
    w.configure(|c| c.per_display = false);
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.current.len(), 1);
    assert_eq!(snap.current[0].display, None);
    let shared = snap.current[0].id.clone();
    let snap = w.run(Command::Next(Some(0)));
    assert_eq!(snap.error, None);
    assert_eq!(snap.current.len(), 1);
    assert_ne!(snap.current[0].id, shared);
}

#[test]
fn per_display_refresh_without_a_ready_photo_skips_current_and_prefetched() {
    // Favorites cover only half of the second display, so every pick for it
    // refills the queue with the photos already in use.
    let screens = vec![screen("1", 120, 80), screen("2", 600, 400)];
    let mut w = World::new(
        Config {
            per_display: true,
            ..config(&[SourceId::Favorites])
        },
        screens,
    );
    w.add_favorites(&["Ann - a1.png", "Bo - b2.png", "Cy - c3.png", "Di - d4.png"]);
    w.run(Command::Next(None));
    let before = w.state();
    let current = before.current.clone().unwrap();
    let next = before.next.clone().expect("a set is prefetched");
    assert_eq!((current.photos.len(), next.photos.len()), (2, 2));
    // The second display's prefetched photo is gone from the cache, and the
    // only unused favorite fits that display worse than the ones in use.
    let lost = next
        .photos
        .iter()
        .find(|p| p.display.as_ref().unwrap().id == "2")
        .unwrap();
    fs::remove_file(&lost.file).unwrap();
    image::RgbImage::from_pixel(150, 100, image::Rgb([200, 60, 10]))
        .save(w.favorites_dir().join("Fay - f6.png"))
        .unwrap();

    let snap = w.run(Command::Next(Some(1)));

    assert_eq!(snap.error, None);
    let after = w.state().current.unwrap();
    assert_eq!(after.photos[0], current.photos[0]);
    let in_use: Vec<&str> = current
        .photos
        .iter()
        .chain(&next.photos)
        .map(|p| p.id.as_str())
        .collect();
    let replaced = &after.photos[1];
    assert!(
        replaced.id.contains("f6"),
        "{} was picked over the unused favorite; in use: {in_use:?}",
        replaced.id
    );
    assert_eq!(image::image_dimensions(&replaced.file).unwrap(), (600, 400));
}

#[test]
fn display_hotplug_re_places_the_current_photos() {
    let mut w = World::new(config(&[SourceId::Favorites]), vec![screen("1", 100, 100)]);
    w.add_favorites(&["Ann - a1.png", "Bo - b2.png"]);

    w.run(Command::Tick);
    assert_eq!(w.applied().len(), 1, "the first tick sets a wallpaper");
    w.run(Command::Tick);
    assert_eq!(w.applied().len(), 1, "nothing changed");

    w.screens.lock().unwrap().push(screen("2", 200, 100));
    w.run(Command::Tick);
    let applied = w.applied();
    assert_eq!(applied.len(), 2);
    assert_eq!(
        applied[1],
        w.state().current.unwrap().layout(),
        "the same photo is placed again"
    );

    // An empty answer means "unknown", not "every display unplugged".
    w.screens.lock().unwrap().clear();
    w.run(Command::Tick);
    assert_eq!(w.applied().len(), 2);
}

#[test]
fn changing_sources_drops_the_prefetched_photo() {
    let mut w = World::new(config(&[SourceId::Unsplash]), vec![screen("1", 160, 90)]);
    w.add_favorites(&["Ann - a1.png"]);
    w.run(Command::Next(None));
    let old_next = w.state().next.unwrap();
    assert!(old_next.on_disk());

    w.configure(|c| c.sources = vec![SourceId::Favorites]);

    assert!(!old_next.on_disk(), "the prefetched photo is deleted");
    let state = w.state();
    assert!(!state.queue.contains_key(&SourceId::Unsplash));
    assert_eq!(state.next.unwrap().photos[0].source, SourceId::Favorites);

    // Switching back asks Unsplash again instead of reusing its old queue.
    w.configure(|c| {
        c.sources = vec![SourceId::Unsplash];
        c.query = "mountains".into();
    });
    let random = w.api.to(Route::UnsplashRandom);
    assert_eq!(random.len(), 2);
    assert_eq!(random[1].param("query"), Some("mountains"));
    assert_eq!(w.state().next.unwrap().photos[0].source, SourceId::Unsplash);
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.current[0].source, "Unsplash");
}

#[test]
fn failed_apply_keeps_the_photo_for_later() {
    let mut w = World::new(config(&[SourceId::Favorites]), vec![screen("1", 100, 100)]);
    w.add_favorites(&["Ann - a1.png", "Bo - b2.png", "Cy - c3.png"]);
    w.run(Command::Next(None));
    let before = w.state();
    let next = before.next.clone().unwrap();

    w.apply_fails.store(true, Ordering::SeqCst);
    let snap = w.run(Command::Next(None));

    assert!(snap.error.unwrap().contains("no display"));
    let state = w.state();
    assert_eq!(state.current, before.current);
    assert_eq!(state.next.as_ref(), Some(&next));
    assert!(next.on_disk());

    w.apply_fails.store(false, Ordering::SeqCst);
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.error, None);
    assert_eq!(snap.current[0].id, next.photos[0].id);
}

#[test]
fn favorites_copy_outside_the_cache_named_by_source() {
    let mut w = World::new(
        Config {
            per_display: true,
            ..config(&[SourceId::Pexels])
        },
        vec![screen("1", 160, 90), screen("2", 120, 80)],
    );
    w.run(Command::Next(None));
    // An Unsplash photo on the first display, Pexels' on the second.
    w.configure(|c| c.sources = vec![SourceId::Unsplash]);
    let snap = w.run(Command::Next(Some(0)));
    assert_eq!(snap.current[0].source, "Unsplash");
    assert_eq!(
        (snap.current[1].source.as_str(), snap.current[1].id.as_str()),
        ("Pexels", "417074")
    );
    let unsplash = &snap.current[0];
    let favorites = w.favorites_dir();
    let pexels_copy = favorites.join("Pixabay - pexels-417074.jpg");
    // Unsplash favorites keep the older "{author} - {id}" name.
    let unsplash_copy = favorites.join(format!("{} - {}.jpg", unsplash.author, unsplash.id));

    let snap = w.run(Command::Favorite(Some(1)));
    assert!(snap.notice.unwrap().starts_with("Saved to"));
    assert!(pexels_copy.exists());
    let snap = w.run(Command::Favorite(None));
    assert!(snap.notice.unwrap().starts_with("Saved 2 photos"));
    assert!(unsplash_copy.exists());

    // Rotating (and the cache cleanup after it) never touches favorites.
    w.run(Command::Next(None));
    assert!(pexels_copy.exists() && unsplash_copy.exists());
    let snap = w.run(Command::Favorite(Some(5)));
    assert!(snap.error.is_some());
}

#[test]
fn rotates_through_favorites_without_the_network() {
    let mut w = World::new(config(&[SourceId::Favorites]), vec![screen("1", 100, 100)]);
    w.add_favorites(&["Ann - a1.png", "Bo - pexels-2.png", "Cy - a3.png"]);
    let originals = |w: &World| {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(w.favorites_dir())
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().into_string().unwrap(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    };
    let before = originals(&w);

    let mut seen = HashSet::new();
    for _ in 0..3 {
        let snap = w.run(Command::Next(None));
        assert_eq!(snap.error, None);
        let current = w.state().current.unwrap();
        assert_eq!(
            image::image_dimensions(&current.photos[0].file).unwrap(),
            (100, 100)
        );
        seen.insert(snap.current[0].id.clone());
    }

    // A shuffle bag: three rotations show all three favorites.
    assert_eq!(seen.len(), 3);
    assert_eq!(w.applied().len(), 3);
    let view = &w.svc.snapshot().current[0];
    assert_eq!(view.source, "Favorites");
    assert!(view.preview_file.is_some());
    assert_eq!(originals(&w), before, "the originals are untouched");
    let snap = w.run(Command::Favorite(Some(0)));
    assert_eq!(snap.notice.as_deref(), Some("Already in your favorites"));
    assert!(w.api.requests().is_empty());
}

#[test]
fn wallhaven_and_photos_of_the_day_are_cropped_to_the_display() {
    let mut w = World::new(config(&[SourceId::Wallhaven]), vec![screen("1", 160, 90)]);

    let snap = w.run(Command::Next(None));

    assert_eq!(snap.error, None);
    assert_eq!(snap.current[0].source, "Wallhaven");
    let search = w.api.to(Route::WallhavenSearch);
    assert_eq!(search[0].param("atleast"), Some("160x90"));
    assert_eq!(search[0].header("Authorization"), None, "no key needed");
    let current = w.state().current.unwrap();
    assert_eq!(
        image::image_dimensions(&current.photos[0].file).unwrap(),
        (160, 90),
        "the full image is cropped here"
    );

    w.configure(|c| c.sources = vec![SourceId::Daily]);
    let snap = w.run(Command::Next(None));

    assert_eq!(snap.error, None);
    let source = snap.current[0].source.as_str();
    assert!(["Bing", "Wikimedia Commons"].contains(&source), "{source}");
    assert_eq!(w.api.to(Route::Bing).len(), 1);
    // Five days of Wikimedia history, one request each.
    assert_eq!(w.api.to(Route::Wikimedia).len(), 5);
    let current = w.state().current.unwrap();
    assert_eq!(
        image::image_dimensions(&current.photos[0].file).unwrap(),
        (160, 90)
    );
}

#[test]
fn unsplash_errors_say_what_went_wrong() {
    for (status, collections, says) in [
        (
            404,
            vec!["123".to_string()],
            "Unsplash couldn't find that collection",
        ),
        (401, Vec::new(), "Unsplash rejected the key"),
        (500, Vec::new(), "Unsplash returned HTTP 500"),
    ] {
        let mut w = World::new(
            Config {
                collections,
                ..config(&[SourceId::Unsplash])
            },
            vec![screen("1", 160, 90)],
        );
        w.api.status(Route::UnsplashTopic, status);
        w.api.status(Route::UnsplashRandom, status);

        let snap = w.run(Command::Next(None));

        let error = snap.error.unwrap();
        assert!(error.contains(says), "{status}: {error}");
        assert!(w.applied().is_empty());
    }
}

#[test]
fn errors_and_logs_never_show_the_configured_keys() {
    // Fake keys without any credential marker (`client_id=` ...), quoted
    // back by a reply the parser rejects.
    let (unsplash_key, pexels_key) = ("FAKE-unsplash-key-0000", "FAKE-pexels-key-1111");
    let mut w = World::new(
        Config {
            access_key: unsplash_key.into(),
            pexels_key: pexels_key.into(),
            ..config(&[SourceId::Unsplash, SourceId::Pexels])
        },
        vec![screen("1", 160, 90)],
    );
    w.api.reply(
        Route::UnsplashRandom,
        &format!(r#"[{{"width":"{unsplash_key}"}}]"#),
    );
    for route in [Route::PexelsSearch, Route::PexelsCurated] {
        w.api.reply(
            route,
            &format!(r#"{{"photos":[{{"width":"{pexels_key}"}}]}}"#),
        );
    }

    let snap = w.run(Command::Next(None));

    let error = snap.error.expect("no source could give a photo");
    let log = fs::read_to_string(&w.paths.log_file).unwrap();
    // Both parser errors reached the log, so there was something to leak.
    assert!(
        log.contains("Unsplash sent a reply this app could not read"),
        "{log}"
    );
    assert!(
        log.contains("Pexels sent a reply this app could not read"),
        "{log}"
    );
    for key in [unsplash_key, pexels_key] {
        assert!(!error.contains(key), "a key leaked into the error: {error}");
        assert!(!log.contains(key), "a key leaked into the log: {log}");
    }
}

#[test]
fn own_photos_in_favorites_get_no_unsplash_link() {
    let mut w = World::new(config(&[SourceId::Favorites]), vec![screen("1", 160, 90)]);
    w.add_favorites(&["Paris - Eiffel tower.png"]);
    let snap = w.run(Command::Next(None));
    let p = &snap.current[0];
    assert!(
        !p.photo_url.starts_with("https://unsplash.com/"),
        "{}",
        p.photo_url
    );
}

#[test]
fn pause_handled_after_a_settings_save_keeps_the_saved_settings() {
    let mut w = World::new(config(&[SourceId::Unsplash]), vec![screen("1", 160, 90)]);
    // save_settings on the UI thread: read, change and write the file, then queue Reload.
    let mut saved = Config::load_or_create(&w.paths.config_file).unwrap();
    saved.query = "mountains".into();
    saved.pexels_key = "new-pexels-key".into();
    saved.sources = vec![SourceId::Unsplash, SourceId::Pexels];
    saved.save(&w.paths.config_file).unwrap();
    // The tray's Pause was queued before that Reload.
    w.run(Command::SetPaused(true));
    let snap = w.run(Command::Reload);
    let on_disk = Config::load_or_create(&w.paths.config_file).unwrap();
    assert_eq!(
        on_disk.query, "mountains",
        "settings the user saved are gone"
    );
    assert!(on_disk.paused);
    assert!(snap.has_pexels_key);
}

#[test]
fn pause_handled_after_a_settings_reload_keeps_the_saved_settings() {
    // No Pexels key at first, so the snapshot only has one if the save is kept.
    let start = Config {
        pexels_key: String::new(),
        ..config(&[SourceId::Unsplash])
    };
    let mut w = World::new(start, vec![screen("1", 160, 90)]);
    let mut saved = Config::load_or_create(&w.paths.config_file).unwrap();
    saved.query = "mountains".into();
    saved.pexels_key = "new-pexels-key".into();
    saved.sources = vec![SourceId::Unsplash, SourceId::Pexels];
    saved.save(&w.paths.config_file).unwrap();

    w.run(Command::Reload);
    let snap = w.run(Command::SetPaused(true));

    let on_disk = Config::load_or_create(&w.paths.config_file).unwrap();
    assert_eq!(on_disk.query, "mountains");
    assert_eq!(on_disk.pexels_key, "new-pexels-key");
    assert_eq!(on_disk.sources, [SourceId::Unsplash, SourceId::Pexels]);
    assert!(on_disk.paused);
    assert!(snap.settings.paused);
    assert_eq!(snap.settings.query, "mountains");
    assert!(snap.has_pexels_key);
    assert_eq!(
        snap.settings.sources,
        [SourceId::Unsplash, SourceId::Pexels]
    );
    assert_eq!(snap.next_rotation_at, None, "paused: nothing is scheduled");
}

#[test]
fn pause_and_resume_persist_and_show_in_the_snapshot() {
    let mut w = World::new(config(&[SourceId::Unsplash]), vec![screen("1", 160, 90)]);

    let snap = w.run(Command::SetPaused(true));
    assert!(
        snap.settings.paused,
        "the tray checkbox follows the snapshot"
    );
    assert!(Config::load_or_create(&w.paths.config_file).unwrap().paused);
    assert_eq!(snap.next_rotation_at, None);

    let snap = w.run(Command::SetPaused(false));
    assert!(!snap.settings.paused);
    assert!(!Config::load_or_create(&w.paths.config_file).unwrap().paused);
    assert!(snap.next_rotation_at.is_some());
}

/// Every command, for the test below. A new variant makes the match fail to
/// compile, which points whoever adds it here; add it to `all` too.
fn every_command() -> Vec<Command> {
    let all = vec![
        Command::Tick,
        Command::Next(None),
        Command::Next(Some(0)),
        Command::Favorite(None),
        Command::Favorite(Some(0)),
        Command::Reload,
        Command::SetPaused(true),
        Command::SetPaused(false),
        Command::Shutdown,
    ];
    for command in &all {
        match command {
            Command::Next(_)
            | Command::Favorite(_)
            | Command::Reload
            | Command::SetPaused(_)
            | Command::Tick
            | Command::Shutdown => {}
        }
    }
    all
}

/// Class-level check for #60: the settings screen writes `config.toml` from
/// another thread, so no worker command may write its own (possibly stale)
/// copy of the config over the file. Whatever the command, a setting saved
/// just before it is handled is still on disk afterwards.
#[test]
fn no_command_overwrites_settings_saved_while_it_was_queued() {
    for command in every_command() {
        let mut w = World::new(config(&[SourceId::Unsplash]), vec![screen("1", 160, 90)]);
        // A photo on screen, so Favorite and per-display Next have work to do.
        w.run(Command::Next(None));

        let mut saved = Config::load_or_create(&w.paths.config_file).unwrap();
        saved.query = "saved while queued".into();
        saved.pexels_key = "saved-pexels-key".into();
        saved.interval_minutes = 7;
        saved.save(&w.paths.config_file).unwrap();

        w.run(command.clone());

        let on_disk = Config::load_or_create(&w.paths.config_file).unwrap();
        assert_eq!(on_disk.query, "saved while queued", "{command:?}");
        assert_eq!(on_disk.pexels_key, "saved-pexels-key", "{command:?}");
        assert_eq!(on_disk.interval_minutes, 7, "{command:?}");
    }
}

fn noise(path: &std::path::Path, w: u32, h: u32, seed: u32) {
    let mut x = seed | 1;
    image::RgbImage::from_fn(w, h, |_, _| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        image::Rgb([x as u8, (x >> 8) as u8, (x >> 16) as u8])
    })
    .save(path)
    .unwrap();
}

#[test]
fn a_cap_smaller_than_two_sets_does_not_refetch_every_tick() {
    let (wp, hp) = (3400, 2300); // ~13 MB per JPEG at q90, so 2 sets > 20 MB
    let mut w = World::new(
        Config {
            cache_cap_mb: 20,
            ..config(&[SourceId::Favorites])
        },
        vec![screen("1", wp, hp)],
    );
    let dir = w.favorites_dir();
    fs::create_dir_all(&dir).unwrap();
    for i in 0..4 {
        noise(&dir.join(format!("n{i}.png")), wp, hp, 7 + i);
    }
    w.run(Command::Next(None));
    let log = w.paths.log_file.clone();
    let drops = || {
        fs::read_to_string(&log)
            .unwrap()
            .matches("cleanup removed")
            .count()
    };
    let before = drops();
    for _ in 0..3 {
        w.run(Command::Tick);
    }
    assert_eq!(
        drops() - before,
        0,
        "each tick prefetched a set and deleted it again"
    );
}

/// 11 MB per photo: one set fits under a 20 MB cap, two don't.
const HEAVY: usize = 11 * 1024 * 1024;

#[test]
fn a_cap_smaller_than_two_sets_fetches_on_demand_until_it_is_raised() {
    let mut w = World::new(
        Config {
            cache_cap_mb: 20,
            ..config(&[SourceId::Unsplash])
        },
        vec![screen("1", 160, 90)],
    );
    w.api.pad_images(HEAVY);
    let downloads = |w: &World| w.api.to(Route::Image).len();

    let first = w.run(Command::Next(None));
    assert_eq!(first.error, None);
    assert!(w.state().next.is_none(), "the prefetch is over the cap");
    let after_next = downloads(&w);
    for _ in 0..3 {
        w.run(Command::Tick);
    }
    assert_eq!(downloads(&w), after_next, "ticks download nothing");

    // Next still works, fetching on demand, and keeps the shown photo.
    let second = w.run(Command::Next(None));
    assert_eq!(second.error, None);
    assert_ne!(second.current[0].id, first.current[0].id);
    assert_eq!(downloads(&w), after_next + 1, "only the shown photo");
    let current = w.state().current.unwrap();
    assert!(current.on_disk(), "the current photo is never deleted");
    assert_eq!(w.cached_files(), vec![current.photos[0].file.clone()]);
    w.run(Command::Tick);
    assert_eq!(downloads(&w), after_next + 1);

    // Raising the cap in Settings lets the next tick prefetch again.
    w.configure(|c| c.cache_cap_mb = 200);
    assert_eq!(downloads(&w), after_next + 2);
    assert!(w.state().next.is_some_and(|s| s.on_disk()));
    w.run(Command::Tick);
    assert_eq!(downloads(&w), after_next + 2);
}

/// The class behind issue #58: whatever the cap, the photo sizes and the
/// display layout, a tick with no rotation due settles. Once a rotation
/// has run, further ticks neither download nor delete anything.
#[test]
fn ticks_between_rotations_never_download_or_delete() {
    let one = vec![screen("1", 160, 90)];
    let two = vec![screen("1", 160, 90), {
        let mut s = screen("2", 160, 90);
        s.x = 160;
        s
    }];
    let cases = [20, 30, 200]
        .into_iter()
        .flat_map(|cap_mb| [(cap_mb, 0), (cap_mb, HEAVY)]);
    for (cap_mb, padding) in cases {
        for (screens, per_display) in [(&one, false), (&two, false), (&two, true)] {
            let case = format!(
                "cap {cap_mb} MB, {} KB photos, {} displays, per display {per_display}",
                padding / 1024,
                screens.len()
            );
            let mut w = World::new(
                Config {
                    cache_cap_mb: cap_mb,
                    per_display,
                    ..config(&[SourceId::Unsplash])
                },
                screens.clone(),
            );
            w.api.pad_images(padding);
            let snap = w.run(Command::Next(None));
            assert_eq!(snap.error, None, "{case}");
            let log_file = w.paths.log_file.clone();
            let log = || fs::read_to_string(&log_file).unwrap();

            let (downloads, logged) = (w.api.to(Route::Image).len(), log().len());
            for _ in 0..3 {
                w.run(Command::Tick);
            }
            assert_eq!(w.api.to(Route::Image).len(), downloads, "{case}");
            let since = &log()[logged..];
            assert!(!since.contains("cleanup removed"), "{case}: {since}");
        }
    }
}

#[test]
fn a_display_change_before_the_next_tick_resumes_prefetching() {
    let mut w = World::new(
        Config {
            cache_cap_mb: 40,
            per_display: true,
            ..config(&[SourceId::Unsplash])
        },
        vec![screen("1", 160, 90), {
            let mut s = screen("2", 160, 90);
            s.x = 160;
            s
        }],
    );
    w.api.pad_images(HEAVY);
    // Two 22 MB sets don't fit under 40 MB, so prefetching pauses.
    w.run(Command::Next(None));
    assert!(w.state().next.is_none());

    // Unplug a display and press Next before any tick notices: two 11 MB
    // sets fit now.
    w.screens.lock().unwrap().truncate(1);
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.error, None);
    assert!(
        w.state().next.is_some_and(|s| s.on_disk()),
        "prefetching resumes for the new layout"
    );

    // Plug it back in: the 11 MB current photo and a 22 MB prefetch fit,
    // so the old pause must not come back with the old layout.
    w.screens.lock().unwrap().push({
        let mut s = screen("2", 160, 90);
        s.x = 160;
        s
    });
    w.run(Command::Tick);
    let next = w
        .state()
        .next
        .expect("a set is prefetched for both displays");
    assert_eq!(next.photos.len(), 2);
    assert!(next.on_disk());
}

/// Whether `file` is mostly red, not mostly blue. JPEG shifts the exact values.
fn is_red(file: &Path) -> bool {
    let image = image::open(file).unwrap().to_rgb8();
    let [r, _, b] = image.get_pixel(0, 0).0;
    r > b
}

/// Two displays of one size, each with its own favorite.
fn two_displays_of_favorites(names: [&str; 2]) -> World {
    let mut s2 = screen("2", 160, 90);
    s2.x = 160;
    let w = World::new(
        Config {
            per_display: true,
            ..config(&[SourceId::Favorites])
        },
        vec![screen("1", 160, 90), s2],
    );
    let dir = w.favorites_dir();
    fs::create_dir_all(&dir).unwrap();
    RgbImage::from_pixel(300, 200, Rgb([250, 0, 0]))
        .save(dir.join(names[0]))
        .unwrap();
    RgbImage::from_pixel(300, 200, Rgb([0, 0, 250]))
        .save(dir.join(names[1]))
        .unwrap();
    w
}

#[test]
fn favorites_with_similar_names_get_their_own_cache_files() {
    let mut w = two_displays_of_favorites(["trip (1).png", "trip [1].png"]);
    w.run(Command::Next(None));
    let state = w.state();
    let cur = state.current.unwrap();
    assert_ne!(
        cur.photos[0].file, cur.photos[1].file,
        "two photos, one file"
    );
    for photo in &cur.photos {
        let red = photo.id.contains("(1)");
        assert_eq!(
            is_red(&photo.file),
            red,
            "{} has the wrong pixels",
            photo.id
        );
    }
    // Still only what's on screen and what's next, stitched or not.
    let mut kept: Vec<_> = [&cur]
        .into_iter()
        .chain(&state.next)
        .flat_map(|s| s.files())
        .map(Path::to_path_buf)
        .collect();
    kept.sort();
    kept.dedup();
    assert_eq!(w.cached_files(), kept);
}

#[test]
fn favorites_differing_only_by_extension_are_two_candidates() {
    let mut w = two_displays_of_favorites(["sunset.jpg", "sunset.png"]);
    w.run(Command::Next(None));
    let cur = w.state().current.unwrap();
    let mut ids: Vec<&str> = cur.photos.iter().map(|p| p.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["fav-sunset.jpg", "fav-sunset.png"]);
    assert_ne!(cur.photos[0].file, cur.photos[1].file);
    for photo in &cur.photos {
        assert_eq!(
            is_red(&photo.file),
            photo.id.ends_with(".jpg"),
            "{}",
            photo.id
        );
    }
}

/// Config files only a hand edit can produce: each with what the log must
/// say about it.
const BROKEN_CONFIGS: [(&str, &[u8], &str); 3] = [
    (
        "invalid TOML",
        b"query = \"mountains\"\ninterval_minutes = = 5\n",
        "line 2",
    ),
    (
        "a wrong-typed value",
        b"query = \"mountains\"\ninterval_minutes = 1.5\n",
        "expected u64",
    ),
    (
        "not UTF-8, as Notepad's UTF-16 is",
        b"\xff\xfeq\x00u\x00e\x00r\x00y\x00",
        "UTF-8",
    ),
];

fn log_text(paths: &Paths) -> String {
    fs::read_to_string(&paths.log_file).unwrap_or_default()
}

#[test]
fn service_new_starts_on_defaults_with_a_broken_config() {
    for (what, bytes, problem) in BROKEN_CONFIGS {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        fs::create_dir_all(paths.config_file.parent().unwrap()).unwrap();
        fs::write(&paths.config_file, bytes).unwrap();
        let hooks = Hooks {
            apply: Box::new(|_| Ok(())),
            displays: Box::new(Vec::new),
            notify: Box::new(|_| {}),
        };

        let service = Service::new(paths.clone(), hooks)
            .unwrap_or_else(|e| panic!("{what} stopped the service: {e:#}"));

        let settings = service.snapshot().settings;
        let defaults = Config::default();
        assert_eq!(
            settings.interval_minutes, defaults.interval_minutes,
            "{what}"
        );
        assert_eq!(settings.cache_cap_mb, defaults.cache_cap_mb, "{what}");
        assert_eq!(settings.query, defaults.query, "{what}");
        let log = log_text(&paths);
        assert!(
            log.contains(&paths.config_file.display().to_string()) && log.contains(problem),
            "{what}: {log}"
        );
        assert_eq!(fs::read(&paths.config_file).unwrap(), bytes, "{what}");
    }
}

#[test]
fn a_broken_config_stays_byte_for_byte_while_the_worker_runs() {
    for (what, bytes, _) in BROKEN_CONFIGS {
        let mut w = World::with_config_file(bytes, vec![screen("a", 320, 200)]);
        w.run(Command::Tick);
        // Pause and Save re-read the file to change one setting; neither may
        // write defaults over it.
        let snap = w.run(Command::SetPaused(true));
        assert!(snap.error.is_some(), "{what}");
        assert!(!snap.settings.paused, "{what}");
        let snap = w.run(Command::Reload);
        assert!(snap.error.is_some(), "{what}");
        assert_eq!(fs::read(&w.paths.config_file).unwrap(), bytes, "{what}");
    }
}

#[test]
fn fixing_a_broken_config_takes_effect_on_reload() {
    let (_, bytes, _) = BROKEN_CONFIGS[0];
    let mut w = World::with_config_file(bytes, vec![screen("a", 320, 200)]);
    assert_eq!(w.run(Command::Tick).settings.query, "");

    fs::write(&w.paths.config_file, "query = \"mountains\"\n").unwrap();

    let snap = w.run(Command::Reload);
    assert_eq!(snap.settings.query, "mountains");
    assert_eq!(snap.error, None);
}

#[test]
fn a_saved_key_on_a_broken_line_stays_out_of_the_log() {
    // The parser's own error text quotes the line it stopped on.
    let secret = "my-unquoted-key";
    let file = format!("access_key = {secret}\n");
    let mut w = World::with_config_file(file.as_bytes(), vec![screen("a", 320, 200)]);

    let snap = w.run(Command::Reload);

    let log = log_text(&w.paths);
    assert!(log.contains("config.toml"), "{log}");
    assert!(!log.contains(secret), "{log}");
    assert!(!snap.error.unwrap().contains(secret));
}

#[test]
fn only_a_saved_config_decides_the_login_item() {
    // Defaults say true, so a fallback that passed them on would turn the
    // login item back on for someone who turned it off.
    let (_, bytes, _) = BROKEN_CONFIGS[0];
    let mut w = World::with_config_file(bytes, vec![screen("a", 320, 200)]);
    assert!(w.run(Command::Tick).settings.start_at_login);
    assert_eq!(w.svc.saved_start_at_login(), None);

    // A failed reload leaves it on the defaults.
    w.run(Command::Reload);
    assert_eq!(w.svc.saved_start_at_login(), None);

    fs::write(&w.paths.config_file, "start_at_login = false\n").unwrap();
    w.run(Command::Reload);
    assert_eq!(w.svc.saved_start_at_login(), Some(false));

    let w = World::with_config_file(b"start_at_login = false\n", vec![]);
    assert_eq!(w.svc.saved_start_at_login(), Some(false));
}

/// No source can fetch: no key, and only Unsplash ticked.
fn no_fetching() -> Config {
    Config {
        access_key: String::new(),
        sources: vec![SourceId::Unsplash],
        ..Config::default()
    }
}

fn names(files: &[std::path::PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn an_unparseable_state_file_keeps_the_cache_at_startup() {
    // The desktop may still point at one of these; with the state lost,
    // nothing says which, so none is deleted.
    let w = World::with_state_file(
        no_fetching(),
        |_| b"{not json".to_vec(),
        &["shown.jpg", "older.jpg"],
        vec![screen("1", 160, 90)],
    );

    let (files, applied) = w.start_up();

    assert_eq!(names(&files), ["older.jpg", "shown.jpg"]);
    assert!(applied.is_empty());
}

#[test]
fn an_unparseable_state_file_also_keeps_the_cache_through_a_reload() {
    let mut w = World::with_state_file(
        no_fetching(),
        |_| b"{not json".to_vec(),
        &["shown.jpg"],
        vec![screen("1", 160, 90)],
    );

    w.configure(|c| c.cache_cap_mb = 30);

    assert_eq!(names(&w.cached_files()), ["shown.jpg"]);
}

#[test]
fn an_unparseable_state_file_keeps_the_cache_after_a_save_and_a_restart() {
    // Saving Settings writes a valid state.json before any new photo is on
    // screen; the next start must still keep the cache.
    let mut w = World::with_state_file(
        no_fetching(),
        |_| b"{not json".to_vec(),
        &["shown.jpg"],
        vec![screen("1", 160, 90)],
    );
    w.configure(|c| c.cache_cap_mb = 30);

    let (files, _) = w.restart().start_up();

    assert_eq!(names(&files), ["shown.jpg"]);
}

#[test]
fn an_unparseable_state_file_cleans_up_once_a_new_photo_is_on_screen() {
    let w = World::with_state_file(
        config(&[SourceId::Unsplash]),
        |_| b"{not json".to_vec(),
        &["shown.jpg"],
        vec![screen("1", 160, 90)],
    );

    let (files, applied) = w.start_up();

    assert_eq!(
        applied.len(),
        1,
        "startup rotates: there's no current photo"
    );
    let Layout::Same(file) = &applied[0] else {
        panic!("one photo for one display, got {applied:?}");
    };
    assert!(files.contains(file));
    assert!(
        !names(&files).contains(&"shown.jpg".to_string()),
        "{files:?}"
    );
}

#[test]
fn a_valid_state_file_cleans_up_stale_images_at_startup() {
    let w = World::with_state_file(
        no_fetching(),
        |paths| {
            serde_json::to_vec(&serde_json::json!({
                "current": {
                    "id": "cur", "file": paths.images_dir.join("cur.jpg"),
                    "author": "Ann", "author_url": "", "photo_url": "",
                    "preview_url": "", "location": null, "description": null,
                    "download_location": "", "color": null,
                },
            }))
            .unwrap()
        },
        &["cur.jpg", "stale.jpg"],
        vec![screen("1", 160, 90)],
    );

    let (files, applied) = w.start_up();

    assert_eq!(names(&files), ["cur.jpg"]);
    assert!(applied.is_empty());
}

/// A Retina panel (scale 2), an external monitor (scale 1) and a rotated
/// portrait one, as macOS and Windows list them: each in its own physical
/// pixels. Sizes are a tenth of 3024x1964, 2560x1440 and 1440x2560 to keep
/// the test light; positions are in the logical layout.
fn mixed_layout() -> Vec<Display> {
    let at = |x: i32, scale: f64, s: Display| Display { x, scale, ..s };
    vec![
        at(0, 2.0, screen("retina", 302, 196)),
        at(151, 1.0, screen("external", 256, 144)),
        at(407, 1.0, screen("portrait", 144, 256)),
    ]
}

#[test]
fn each_display_gets_a_photo_fetched_and_cropped_at_its_own_size() {
    let screens = mixed_layout();
    let mut w = World::new(
        Config {
            per_display: true,
            ..config(&[SourceId::Favorites])
        },
        screens.clone(),
    );
    w.add_favorites(&["a.png", "b.png", "c.png", "d.png", "e.png", "f.png"]);
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.error, None);

    for set in [w.state().current.unwrap(), w.state().next.unwrap()] {
        assert_eq!(set.photos.len(), screens.len());
        for (photo, screen) in set.photos.iter().zip(&screens) {
            let display = photo.display.as_ref().expect("a photo per display");
            assert_eq!(display.id, screen.id, "photos follow the display order");
            assert_eq!(
                image::image_dimensions(&photo.file).unwrap(),
                (screen.width, screen.height),
                "{}'s photo is cropped to its own size, not another display's",
                screen.id
            );
        }
        let ids: HashSet<_> = set.photos.iter().map(|p| &p.id).collect();
        assert_eq!(
            ids.len(),
            screens.len(),
            "a different photo on each display"
        );
        assert_eq!(set.photos[0].display.as_ref().unwrap().scale, 2.0);
        // GNOME stitches them on a canvas at the sharpest scale: the
        // logical layout is 551x128, so twice that.
        if cfg!(target_os = "linux") {
            let spanned = set.spanned.as_ref().expect("GNOME gets one stitched image");
            assert_eq!(image::image_dimensions(spanned).unwrap(), (1102, 512));
        }
    }
    let log = log_text(&w.paths);
    assert!(
        log.contains("displays: Screen retina: 302x196 pixels, scale 2; "),
        "the log lists each display's size and scale: {log}"
    );
}

#[test]
fn a_resolution_or_scale_change_re_sizes_the_next_rotation() {
    let mut w = World::new(
        Config {
            per_display: true,
            ..config(&[SourceId::Favorites])
        },
        mixed_layout(),
    );
    w.add_favorites(&["a.png", "b.png", "c.png", "d.png", "e.png", "f.png"]);
    assert_eq!(w.run(Command::Next(None)).error, None);
    assert!(
        w.state().next.is_some(),
        "a set is prefetched for this layout"
    );

    // The panel switches to "More Space" (more pixels, same scale) and the
    // external monitor to 150 % (same pixels, new scale).
    let changed = {
        let mut screens = mixed_layout();
        screens[0].width = 346;
        screens[0].height = 223;
        screens[1].scale = 1.5;
        screens
    };
    *w.screens.lock().unwrap() = changed.clone();
    w.run(Command::Tick);
    let next = w.state().next.expect("a set prefetched for the new layout");
    let displays: Vec<_> = next
        .photos
        .iter()
        .map(|p| p.display.clone().unwrap())
        .collect();
    assert_eq!(
        displays, changed,
        "the prefetched set follows the new layout"
    );

    assert_eq!(w.run(Command::Next(None)).error, None);
    let current = w.state().current.unwrap();
    for (photo, screen) in current.photos.iter().zip(&changed) {
        assert_eq!(
            image::image_dimensions(&photo.file).unwrap(),
            (screen.width, screen.height),
            "{} after the change",
            screen.id
        );
        assert_eq!(photo.display.as_ref(), Some(screen));
    }
    assert!(log_text(&w.paths).contains("displays changed: 3 connected"));
}

#[test]
fn a_resolution_change_re_sizes_the_next_shared_photo() {
    // One monitor with per-display on, and per-display off, both use one
    // shared photo; a photo prefetched before the change must not be used.
    for per_display in [true, false] {
        let mut w = World::new(
            Config {
                per_display,
                ..config(&[SourceId::Favorites])
            },
            vec![mixed_layout().remove(1)],
        );
        w.add_favorites(&["a.png", "b.png", "c.png", "d.png"]);
        assert_eq!(w.run(Command::Next(None)).error, None);
        let next = w.state().next.expect("a photo is prefetched");
        assert_eq!(
            image::image_dimensions(&next.photos[0].file).unwrap(),
            (256, 144)
        );

        let mut changed = mixed_layout().remove(1);
        changed.width = 320;
        changed.height = 200;
        *w.screens.lock().unwrap() = vec![changed];
        w.run(Command::Tick);
        assert_eq!(w.run(Command::Next(None)).error, None);
        let current = w.state().current.unwrap();
        let [photo] = current.photos.as_slice() else {
            panic!("one shared photo (per_display {per_display})");
        };
        assert_eq!(
            image::image_dimensions(&photo.file).unwrap(),
            (320, 200),
            "the photo after the change is sized for it (per_display {per_display})"
        );
        let next = w.state().next.expect("a photo is prefetched again");
        assert_eq!(
            image::image_dimensions(&next.photos[0].file).unwrap(),
            (320, 200),
            "the prefetched photo follows the change (per_display {per_display})"
        );
    }
}

#[test]
fn one_photo_for_every_display_covers_the_widest_and_tallest() {
    let mut w = World::new(config(&[SourceId::Favorites]), mixed_layout());
    w.add_favorites(&["a.png", "b.png"]);
    let snap = w.run(Command::Next(None));
    assert_eq!(snap.error, None);
    let current = w.state().current.unwrap();
    let [photo] = current.photos.as_slice() else {
        panic!("one photo for every display");
    };
    assert_eq!(photo.display, None);
    assert_eq!(
        image::image_dimensions(&photo.file).unwrap(),
        (302, 256),
        "the widest width and the tallest height of any display"
    );
    assert_eq!(current.layout(), Layout::Same(photo.file.clone()));
}
