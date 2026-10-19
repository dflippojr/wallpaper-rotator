//! Deterministic snapshot failures; these tests never access a desktop.
#[path = "common/background_snapshot.rs"]
mod background_snapshot;

#[test]
fn captures_every_screens_url_and_options() {
    let saved = background_snapshot::capture([1, 2], |screen| {
        (Some(format!("wallpaper-{screen}")), Some(*screen + 10))
    })
    .unwrap();
    assert_eq!(
        saved,
        vec![(1, "wallpaper-1".into(), 11), (2, "wallpaper-2".into(), 12)]
    );
}

#[test]
fn refuses_partial_snapshot_when_a_screen_has_no_url() {
    let saved = background_snapshot::capture([1, 2], |screen| {
        ((*screen == 1).then_some("wallpaper"), Some("options"))
    });
    assert!(
        saved.is_err(),
        "a missing URL must abort before any desktop changes"
    );
}

#[test]
fn refuses_partial_snapshot_when_a_screen_has_no_options() {
    let saved = background_snapshot::capture([1, 2], |screen| {
        (Some("wallpaper"), (*screen == 1).then_some("options"))
    });
    assert!(
        saved.is_err(),
        "missing options must abort before any desktop changes"
    );
}
