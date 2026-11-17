//! The tray icon, its menu and its tooltip. The tooltip names the photos on
//! screen, and the pause item follows the worker's snapshots.

use rotator_core::service::{Command, Snapshot, WallpaperView};
use rotator_core::source::SourceId;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::popover::{show_popover, toggle_popover};
use crate::AppState;

fn tray_icon() -> tauri::Result<Image<'static>> {
    // macOS gets a black template image it recolors for the menu bar; the
    // Windows taskbar and GNOME top bar are dark by default.
    let bytes: &[u8] = if cfg!(target_os = "macos") {
        include_bytes!("../icons/tray.png")
    } else {
        include_bytes!("../icons/tray-light.png")
    };
    Image::from_bytes(bytes)
}

pub(super) fn build_tray(app: &AppHandle, paused: bool) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "Next wallpaper", true, None::<&str>)?;
    let fav = MenuItem::with_id(app, "favorite", "Save to favorites", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause rotation", true, paused, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &next, &fav, &pause, &sep, &quit])?;

    *app.state::<AppState>().pause_item.lock().unwrap() = Some(pause.clone());
    let pause_item = pause.clone();
    TrayIconBuilder::with_id("main")
        .icon(tray_icon()?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Wallpaper Rotator")
        .menu(&menu)
        // Left click opens the popover; the menu is on right click. GNOME
        // has no click events for tray icons, so there the menu is all.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            let state = app.state::<AppState>();
            match event.id().as_ref() {
                "show" => show_popover(app, None),
                "next" => state.send(Command::Next(None)),
                "favorite" => state.send(Command::Favorite(None)),
                "pause" => {
                    let paused = pause_item.is_checked().unwrap_or(false);
                    state.send(Command::SetPaused(paused));
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), &rect);
            }
        })
        .build(app)?;
    Ok(())
}

/// Keeps the tray tooltip and pause checkbox in step with the worker.
pub(super) fn refresh_tray(app: &AppHandle, snapshot: &Snapshot) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(tray_tooltip(&snapshot.current)));
    }
    if let Some(state) = app.try_state::<AppState>() {
        if let Some(item) = state.pause_item.lock().unwrap().as_ref() {
            let _ = item.set_checked(snapshot.settings.paused);
        }
    }
}

/// The tray tooltip: who took the current photos and where they came from.
fn tray_tooltip(photos: &[WallpaperView]) -> String {
    if photos.is_empty() {
        return "Wallpaper Rotator".into();
    }
    let mut sources: Vec<&str> = Vec::new();
    for photo in photos {
        if !sources.contains(&photo.source.as_str()) {
            sources.push(&photo.source);
        }
    }
    let mut authors: Vec<&str> = photos
        .iter()
        .filter(|w| !anonymous(&w.source))
        .map(|w| w.author.as_str())
        .collect();
    authors.dedup();
    let line = if authors.is_empty() {
        let noun = if sources.len() == 1 {
            "Photo"
        } else {
            "Photos"
        };
        format!("{noun} from {}", sources.join(" and "))
    } else {
        let noun = if authors.len() == 1 {
            "Photo"
        } else {
            "Photos"
        };
        format!(
            "{noun} by {} on {}",
            authors.join(", "),
            sources.join(" and ")
        )
    };
    format!("Wallpaper Rotator\n{line}")
}

/// Wallhaven's author is the site and a favorite's author comes from its file
/// name, so these photos name their source instead of a photographer.
fn anonymous(source: &str) -> bool {
    source == SourceId::Wallhaven.name() || source == SourceId::Favorites.name()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(author: &str, source: &str) -> WallpaperView {
        WallpaperView {
            author: author.into(),
            source: source.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tray_tooltip_keeps_the_unsplash_credit() {
        assert_eq!(tray_tooltip(&[]), "Wallpaper Rotator");
        assert_eq!(
            tray_tooltip(&[photo("Ann", "Unsplash")]),
            "Wallpaper Rotator\nPhoto by Ann on Unsplash"
        );
        assert_eq!(
            tray_tooltip(&[photo("Ann", "Unsplash"), photo("Bo", "Unsplash")]),
            "Wallpaper Rotator\nPhotos by Ann, Bo on Unsplash"
        );
    }

    #[test]
    fn tray_tooltip_names_the_provider() {
        assert_eq!(
            tray_tooltip(&[photo("Ann", "Pexels")]),
            "Wallpaper Rotator\nPhoto by Ann on Pexels"
        );
    }

    #[test]
    fn tray_tooltip_lists_each_provider_once() {
        assert_eq!(
            tray_tooltip(&[
                photo("Ann", "Unsplash"),
                photo("Bo", "Pexels"),
                photo("Cy", "Unsplash"),
            ]),
            "Wallpaper Rotator\nPhotos by Ann, Bo, Cy on Unsplash and Pexels"
        );
    }

    #[test]
    fn tray_tooltip_names_the_site_for_wallhaven_and_favorites() {
        assert_eq!(
            tray_tooltip(&[photo("Wallhaven", "Wallhaven")]),
            "Wallpaper Rotator\nPhoto from Wallhaven"
        );
        assert_eq!(
            tray_tooltip(&[photo("sunset", "Favorites")]),
            "Wallpaper Rotator\nPhoto from Favorites"
        );
        assert_eq!(
            tray_tooltip(&[photo("Ann", "Unsplash"), photo("Wallhaven", "Wallhaven")]),
            "Wallpaper Rotator\nPhoto by Ann on Unsplash and Wallhaven"
        );
    }
}
