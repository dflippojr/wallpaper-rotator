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
use rotator_core::config::Config;
use rotator_core::service::Command;
use rotator_core::source::SourceId;
use rotator_core::wallpaper::Layout;

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
