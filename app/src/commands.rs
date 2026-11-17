//! The Tauri commands the popover calls, and the favorites folder opener they
//! share with the tests.

use std::path::Path;

use rotator_core::config::Config;
use rotator_core::paths::Paths;
use rotator_core::service::{self, Command, Settings, Snapshot};
use rotator_core::source;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use crate::popover::hide_popover;
use crate::{sync_autostart, AppState};

#[tauri::command]
pub(super) fn get_state(state: State<'_, AppState>) -> Snapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
/// A new photo for the display at `index` in the snapshot, or for all.
pub(super) fn next_wallpaper(state: State<'_, AppState>, index: Option<usize>) {
    state.send(Command::Next(index));
}

/// Saves the photo at `index` in the snapshot, or all current photos.
#[tauri::command]
pub(super) fn favorite(state: State<'_, AppState>, index: Option<usize>) {
    state.send(Command::Favorite(index));
}

#[tauri::command]
pub(super) fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
    access_key: Option<String>,
    pexels_key: Option<String>,
) -> Result<(), String> {
    // Read, change and write under the config lock, so a Pause the worker
    // saves at the same moment can't write back the old settings (#92).
    let config = Config::update(&state.paths.config_file, |config| {
        settings
            .apply_to(config, access_key.as_deref(), pexels_key.as_deref())
            // The form shows only the top message, without its cause.
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
    .map_err(|e| format!("{e:#}"))?;
    sync_autostart(&app, config.start_at_login);
    state.send(Command::Reload);
    Ok(())
}

/// Opens photo and credit links in the browser. Only the photo sources'
/// sites, so the page can't be used to open anything else.
#[tauri::command]
pub(super) fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    check_link(&url)?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Refuses any link that isn't on a photo source's site.
fn check_link(url: &str) -> Result<(), String> {
    if source::is_source_link(url) {
        Ok(())
    } else {
        Err("only links to the photo sources' sites can be opened".into())
    }
}

/// Opens the favorites folder in Finder, Explorer or the GNOME file manager.
/// It takes no path: the folder comes from the saved config, so the page
/// can't open anything else.
#[tauri::command]
pub(super) fn open_favorites(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    open_favorites_with(&state.paths, |dir| {
        // The opener hands the path to the platform's launcher as one
        // argument; no shell parses it.
        app.opener()
            .open_path(dir.to_string_lossy(), None::<&str>)
            .map_err(|e| e.to_string())
    })
}

/// Creates the configured favorites folder if needed and passes it to `open`.
/// Errors say why the folder can't be opened, for the popover to show.
fn open_favorites_with(
    paths: &Paths,
    open: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let dir = service::favorites_folder(paths).map_err(|e| format!("{e:#}"))?;
    open(&dir).map_err(|e| format!("could not open {}: {e}", dir.display()))
}

#[tauri::command]
pub(super) fn hide_window(app: AppHandle) {
    hide_popover(&app);
}

#[tauri::command]
pub(super) fn quit(app: AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn opens_only_the_photo_sources_sites() {
        for url in [
            "https://unsplash.com/photos/x?utm_source=wallpaper_rotator",
            "https://www.pexels.com/photo/1/",
            "https://wallhaven.cc/w/abc",
            "https://www.bing.com/search?q=x",
            "https://commons.wikimedia.org/wiki/File:A.jpg",
        ] {
            assert_eq!(check_link(url), Ok(()), "{url}");
        }
        for url in [
            "https://example.com/",
            "https://unsplash.com.example.com/",
            "http://unsplash.com/photos/x",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                check_link(url),
                Err("only links to the photo sources' sites can be opened".into()),
                "{url}"
            );
        }
    }

    /// The folder `open_favorites_with` handed to the opener, if it got that far.
    fn opened(paths: &Paths) -> (Result<(), String>, Option<PathBuf>) {
        let mut seen = None;
        let result = open_favorites_with(paths, |dir| {
            seen = Some(dir.to_path_buf());
            Ok(())
        });
        (result, seen)
    }

    #[test]
    fn open_favorites_opens_the_folder_named_in_the_config() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        let custom = root.path().join("Custom Favorites");
        Config {
            favorites_dir: Some(custom.clone()),
            ..Config::default()
        }
        .save(&paths.config_file)
        .unwrap();

        assert_eq!(opened(&paths), (Ok(()), Some(custom.clone())));
        assert!(custom.is_dir(), "created before the opener runs");
    }

    #[test]
    fn open_favorites_creates_a_missing_default_folder_first() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        Config::default().save(&paths.config_file).unwrap();
        assert!(!paths.default_favorites_dir.exists());

        let mut existed = false;
        let result = open_favorites_with(&paths, |dir| {
            existed = dir.is_dir();
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert!(existed, "the folder exists when the opener gets it");
    }

    #[test]
    fn open_favorites_says_why_it_cannot_open() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        let blocked = root.path().join("a file");
        std::fs::write(&blocked, b"file").unwrap();
        Config {
            favorites_dir: Some(blocked.clone()),
            ..Config::default()
        }
        .save(&paths.config_file)
        .unwrap();

        let (result, seen) = opened(&paths);
        let message = result.unwrap_err();
        assert!(
            message.starts_with(&format!("could not create {}", blocked.display())),
            "{message}"
        );
        assert_eq!(seen, None, "the opener is never called");

        // An opener failure names the folder too.
        let custom = root.path().join("ok");
        Config {
            favorites_dir: Some(custom.clone()),
            ..Config::default()
        }
        .save(&paths.config_file)
        .unwrap();
        let result = open_favorites_with(&paths, |_| Err("no file manager".into()));
        assert_eq!(
            result,
            Err(format!(
                "could not open {}: no file manager",
                custom.display()
            ))
        );
    }
}
