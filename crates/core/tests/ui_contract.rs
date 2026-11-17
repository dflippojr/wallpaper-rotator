//! Contract test between the popover (`ui/`) and the core it talks to (#93).
//!
//! The popover's Playwright suite loads `ui/tests/fixtures/state.json` and
//! never sees Rust, so a renamed Rust field, a cache floor changed in one
//! place, a source checkbox that the core doesn't know, or a `save_settings`
//! argument the command doesn't take would pass there and break in the real
//! app. This file reads the UI and command sources as text, so it needs no
//! browser and runs in the Rust job on all three OSes.

use std::fs;
use std::path::PathBuf;

use rotator_core::config::MIN_CACHE_MB;
use rotator_core::service::{Settings, Snapshot, WallpaperView};
use rotator_core::source::SourceId;
use serde_json::Value;

/// A file under the repository root, read as text.
fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The value of every ` name="…"` attribute in `text`, in order.
fn attribute_values<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let needle = format!(" {name}=\"");
    text.match_indices(&needle)
        .map(|(at, _)| {
            let start = at + needle.len();
            let len = text[start..].find('"').expect("unterminated attribute");
            &text[start..start + len]
        })
        .collect()
}

/// The whole start tag of the element with ` id="…"` in `html`.
fn tag_with_id<'a>(html: &'a str, id: &str) -> &'a str {
    let needle = format!(" id=\"{id}\"");
    let at = html
        .find(&needle)
        .unwrap_or_else(|| panic!("ui/index.html has no element with id \"{id}\""));
    let start = html[..at].rfind('<').expect("start of the tag");
    let end = at + html[at..].find('>').expect("end of the tag");
    &html[start..=end]
}

/// A key path such as `current[0].display`, for the failure message.
fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Appends to `out` every key that `rust` and `fixture` don't share, walking
/// objects key by key and the first element of arrays. Values are not compared.
fn key_differences(path: &str, rust: &Value, fixture: &Value, out: &mut Vec<String>) {
    match (rust, fixture) {
        (Value::Object(rust_map), Value::Object(fixture_map)) => {
            for key in rust_map.keys().filter(|k| !fixture_map.contains_key(*k)) {
                out.push(format!(
                    "`{}` is serialized by Snapshot but missing from ui/tests/fixtures/state.json",
                    child(path, key)
                ));
            }
            for key in fixture_map.keys().filter(|k| !rust_map.contains_key(*k)) {
                out.push(format!(
                    "`{}` is in ui/tests/fixtures/state.json but Snapshot does not serialize it",
                    child(path, key)
                ));
            }
            for (key, value) in rust_map {
                if let Some(other) = fixture_map.get(key) {
                    key_differences(&child(path, key), value, other, out);
                }
            }
        }
        (Value::Array(rust_items), Value::Array(fixture_items)) => {
            if let (Some(r), Some(f)) = (rust_items.first(), fixture_items.first()) {
                key_differences(&format!("{path}[0]"), r, f, out);
            }
        }
        _ => {}
    }
}

/// A Snapshot with every `Option` set to `Some`, one photo and Settings, so
/// each key is present in the output.
fn live_snapshot() -> Value {
    let photo = WallpaperView {
        id: "live-1".into(),
        author: "Live artist".into(),
        author_url: "https://example.com/artist".into(),
        photo_url: "https://example.com/photo".into(),
        preview_url: "https://example.com/preview".into(),
        preview_file: Some("preview.png".into()),
        location: Some("Somewhere".into()),
        description: Some("A photo".into()),
        color: Some("#102030".into()),
        display: Some("Built-in".into()),
        source: "Favorites".into(),
    };
    let snapshot = Snapshot {
        current: vec![photo],
        busy: false,
        error: Some("error".into()),
        notice: Some("notice".into()),
        needs_setup: false,
        has_unsplash_key: true,
        has_pexels_key: true,
        settings: Settings::default(),
        photographer_note: Some("note".into()),
        next_rotation_at: Some(1),
        favorites_dir: "favorites".into(),
    };
    serde_json::to_value(snapshot).expect("Snapshot serializes")
}

/// Snapshot's keys (recursively through `current[0]` and `settings`) are the
/// keys of the popover's hand-written fixture. Rename or drop a field on
/// either side and this fails, naming the field.
#[test]
fn snapshot_keys_match_the_popover_fixture() {
    let fixture: Value = serde_json::from_str(&repo_file("ui/tests/fixtures/state.json"))
        .expect("ui/tests/fixtures/state.json is JSON");
    let mut differences = Vec::new();
    key_differences("", &live_snapshot(), &fixture, &mut differences);
    assert!(
        differences.is_empty(),
        "the popover fixture and Snapshot disagree; change the fixture or the Rust field:\n  {}",
        differences.join("\n  ")
    );
}

/// The cache floor is one number in Rust (`MIN_CACHE_MB`). The settings form
/// repeats it as `min` on `#f-cap` and as the `Math.max` floor in app.js.
#[test]
fn cache_floor_matches_in_index_html_and_app_js() {
    let html = repo_file("ui/index.html");
    let js = repo_file("ui/app.js");
    let min = attribute_values(tag_with_id(&html, "f-cap"), "min");
    assert_eq!(
        min,
        [MIN_CACHE_MB.to_string()],
        "ui/index.html #f-cap has min {min:?}, but MIN_CACHE_MB is {MIN_CACHE_MB} (crates/core/src/config.rs)"
    );
    let floor = format!("Math.max({MIN_CACHE_MB},");
    assert!(
        js.contains(&floor),
        "ui/app.js has no `{floor}` floor for the cache size, but MIN_CACHE_MB is {MIN_CACHE_MB} (crates/core/src/config.rs)"
    );
}

/// The source checkboxes and the source names app.js ticks are `SourceId`'s
/// serde names, in the same order (`rename_all = "lowercase"`).
#[test]
fn source_names_match_source_id() {
    let html = repo_file("ui/index.html");
    let js = repo_file("ui/app.js");
    let expected: Vec<String> = SourceId::ALL
        .iter()
        .map(|s| {
            serde_json::to_value(s)
                .expect("SourceId serializes")
                .as_str()
                .expect("SourceId serializes as a string")
                .to_string()
        })
        .collect();

    let boxes: Vec<String> = attribute_values(&html, "data-source")
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        boxes, expected,
        "ui/index.html data-source values differ from SourceId::ALL (crates/core/src/source/mod.rs), in order"
    );

    // app.js names sources in `ticked("…")` calls and in `[…].some(ticked)`.
    let mut ticked: Vec<&str> = js
        .match_indices("ticked(\"")
        .map(|(at, _)| {
            let start = at + "ticked(\"".len();
            let len = js[start..].find('"').expect("unterminated source name");
            &js[start..start + len]
        })
        .collect();
    for (at, _) in js.match_indices("].some(ticked)") {
        let open = js[..at].rfind('[').expect("start of the array");
        ticked.extend(
            js[open + 1..at]
                .split(',')
                .map(|item| item.trim().trim_matches('"')),
        );
    }
    assert!(
        !ticked.is_empty(),
        "found no source names in ui/app.js; the check below would pass without comparing anything"
    );
    for name in ticked {
        assert!(
            expected.iter().any(|e| e == name),
            "ui/app.js names source \"{name}\", which SourceId does not serialize (expected one of {expected:?})"
        );
    }
}

/// Tauri maps the camelCase keys app.js sends to the snake_case parameters of
/// `save_settings` (app/src/commands.rs, after #94 split main.rs). The keys
/// and the parameters must be the same set.
#[test]
fn save_settings_arguments_match_the_command() {
    let js = repo_file("ui/app.js");
    let commands = repo_file("app/src/commands.rs");

    let call = "invoke(\"save_settings\", {";
    let start = js
        .find(call)
        .unwrap_or_else(|| panic!("ui/app.js has no `{call}` call"))
        + call.len();
    let body = &js[start..];
    let body = &body[..body.find('}').expect("end of the arguments object")];
    let mut sent: Vec<String> = body
        .split(',')
        .filter_map(|field| {
            let key = field.split(':').next()?.trim();
            (!key.is_empty()).then(|| key.to_string())
        })
        .collect();

    let signature_start = commands
        .find("fn save_settings(")
        .expect("app/src/commands.rs has no fn save_settings(")
        + "fn save_settings(".len();
    let signature_len = commands[signature_start..]
        .find(')')
        .expect("end of save_settings parameters");
    let mut taken: Vec<String> = commands[signature_start..signature_start + signature_len]
        .lines()
        .filter_map(|line| {
            let (name, ty) = line.trim().trim_end_matches(',').split_once(':')?;
            // Tauri supplies the app handle and managed state itself.
            let ty = ty.trim();
            (!ty.starts_with("AppHandle") && !ty.starts_with("State<"))
                .then(|| name.trim().to_string())
        })
        .collect();

    let snake = |camel: &str| {
        camel.chars().fold(String::new(), |mut out, c| {
            if c.is_ascii_uppercase() {
                out.push('_');
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
            out
        })
    };
    let mut sent_as_params: Vec<String> = sent.iter().map(|k| snake(k)).collect();
    sent.sort();
    sent_as_params.sort();
    taken.sort();
    assert_eq!(
        sent, ["accessKey", "pexelsKey", "settings"],
        "ui/app.js sends {sent:?} to save_settings, but the contract is [accessKey, pexelsKey, settings]; match app/src/commands.rs, or change the command and this list together"
    );
    assert_eq!(
        taken, sent_as_params,
        "app/src/commands.rs save_settings takes {taken:?}, but ui/app.js sends {sent:?} (Tauri maps camelCase to snake_case)"
    );
}
