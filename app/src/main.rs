// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(any(all(windows, debug_assertions), test))]
mod automation;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rotator_core::config::Config;
use rotator_core::log;
use rotator_core::paths::Paths;
use rotator_core::service::{self, Command, Hooks, Service, Settings, Snapshot, WallpaperView};
use rotator_core::source::SourceId;
use rotator_core::wallpaper::{self, Display, Layout, Monitor};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, State, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_opener::OpenerExt;

struct AppState {
    commands: Mutex<Sender<Command>>,
    snapshot: Arc<Mutex<Snapshot>>,
    paths: Paths,
    /// When the popover last hid itself on blur, so a tray click that caused
    /// the blur doesn't immediately reopen it.
    last_hidden: Mutex<Option<Instant>>,
    pause_item: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    popover: PopoverLife,
}

/// The popover's label in `tauri.conf.json`. Its window is not created at
/// startup (`"create": false`); `popover` builds it from that config.
const POPOVER: &str = "main";

/// How long a hidden popover keeps its webview, so a quick re-open is instant.
/// After that it is destroyed and the webview engine's processes can exit.
const POPOVER_GRACE: Duration = Duration::from_secs(30);

/// Decides when a hidden popover's webview may be destroyed. Each hide takes
/// a ticket; any later show or hide makes older tickets stale, so only the
/// most recent hide, with no show since, destroys the window.
#[derive(Default)]
struct PopoverLife {
    generation: AtomicU64,
}

impl PopoverLife {
    /// The popover was shown: pending destroys no longer apply.
    fn shown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The popover was hidden: returns the ticket a later destroy must hold.
    fn hidden(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether the destroy scheduled with `ticket` should still go ahead.
    fn may_destroy(&self, ticket: u64, visible: bool) -> bool {
        !visible && self.generation.load(Ordering::SeqCst) == ticket
    }
}

impl AppState {
    fn send(&self, command: Command) {
        let _ = self.commands.lock().unwrap().send(command);
    }
}

/// Runs `f` on the UI main thread and waits for its result.
fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> T + Send + 'static,
) -> anyhow::Result<T> {
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })?;
    Ok(rx.recv_timeout(Duration::from_secs(30))?)
}

fn apply_wallpaper(app: &AppHandle, layout: &Layout) -> anyhow::Result<()> {
    if wallpaper::NEEDS_MAIN_THREAD {
        let layout = layout.clone();
        on_main(app, move || wallpaper::set(&layout))?
    } else {
        wallpaper::set(layout)
    }
}

/// The platform's own display list (macOS, Windows, GNOME's Mutter), or
/// else the monitors the windowing toolkit reports.
fn displays(app: &AppHandle) -> Vec<Display> {
    let native = if wallpaper::NEEDS_MAIN_THREAD {
        on_main(app, wallpaper::displays).ok().flatten()
    } else {
        wallpaper::displays()
    };
    native.unwrap_or_else(|| toolkit_displays(app))
}

/// Monitors as GTK sees them (Linux without Mutter).
fn toolkit_displays(app: &AppHandle) -> Vec<Display> {
    let monitors: Vec<Monitor> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| Monitor {
            name: m.name().cloned(),
            x: m.position().x,
            y: m.position().y,
            width: m.size().width,
            height: m.size().height,
            scale: m.scale_factor(),
        })
        .collect();
    wallpaper::toolkit_displays(&monitors)
}

// ---- Commands the popover calls ----

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> Snapshot {
    state.snapshot.lock().unwrap().clone()
}

#[tauri::command]
/// A new photo for the display at `index` in the snapshot, or for all.
fn next_wallpaper(state: State<'_, AppState>, index: Option<usize>) {
    state.send(Command::Next(index));
}

/// Saves the photo at `index` in the snapshot, or all current photos.
#[tauri::command]
fn favorite(state: State<'_, AppState>, index: Option<usize>) {
    state.send(Command::Favorite(index));
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
    access_key: Option<String>,
    pexels_key: Option<String>,
) -> Result<(), String> {
    let path = &state.paths.config_file;
    let mut config = Config::load_or_create(path).map_err(|e| format!("{e:#}"))?;
    settings
        .apply_to(&mut config, access_key.as_deref(), pexels_key.as_deref())
        .map_err(|e| e.to_string())?;
    config.save(path).map_err(|e| format!("{e:#}"))?;
    sync_autostart(&app, config.start_at_login);
    state.send(Command::Reload);
    Ok(())
}

/// Opens photo and credit links in the browser. Only the photo sources'
/// sites, so the page can't be used to open anything else.
#[tauri::command]
fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    let allowed = [
        "https://unsplash.com/",
        "https://www.unsplash.com/",
        "https://www.pexels.com/",
        "https://wallhaven.cc/",
        "https://www.bing.com/",
        "https://commons.wikimedia.org/",
    ];
    if !allowed.iter().any(|prefix| url.starts_with(prefix)) {
        return Err("only Unsplash links can be opened".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Opens the favorites folder in Finder, Explorer or the GNOME file manager.
/// It takes no path: the folder comes from the saved config, so the page
/// can't open anything else.
#[tauri::command]
fn open_favorites(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
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
fn hide_window(app: AppHandle) {
    hide_popover(&app);
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

// ---- Window and tray ----

fn sync_autostart(app: &AppHandle, enabled: bool) {
    // Debug builds would register the dev binary; leave login items alone.
    if cfg!(debug_assertions) {
        return;
    }
    let launcher = app.autolaunch();
    let result = match (enabled, launcher.is_enabled().unwrap_or(false)) {
        (true, false) => launcher.enable(),
        (false, true) => launcher.disable(),
        _ => Ok(()),
    };
    if let Err(e) = result {
        let paths = &app.state::<AppState>().paths;
        log::write(
            &paths.log_file,
            &format!("could not update start at login: {e}"),
        );
    }
}

/// Places the popover next to the tray icon: below it when the menu bar is
/// at the top (macOS, GNOME), above it when the taskbar is at the bottom.
fn place_near(window: &WebviewWindow, icon: &Rect) -> tauri::Result<()> {
    let scale = window.scale_factor()?;
    let pos = icon.position.to_physical::<f64>(scale);
    let size = icon.size.to_physical::<f64>(scale);
    let win = window.outer_size()?;
    let (win_w, win_h) = (win.width as f64, win.height as f64);
    let center_x = pos.x + size.width / 2.0;
    let center_y = pos.y + size.height / 2.0;
    let monitor = window
        .monitor_from_point(center_x, center_y)?
        .or(window.primary_monitor()?);
    let Some(monitor) = monitor else {
        return window.center();
    };
    let (mx, my) = (monitor.position().x as f64, monitor.position().y as f64);
    let (mw, mh) = (monitor.size().width as f64, monitor.size().height as f64);
    let margin = 8.0 * scale;
    let x =
        (center_x - win_w / 2.0).clamp(mx + margin, (mx + mw - win_w - margin).max(mx + margin));
    let y = if center_y < my + mh / 2.0 {
        pos.y + size.height + margin
    } else {
        pos.y - win_h - margin
    };
    window.set_position(PhysicalPosition::new(x, y.max(my)))
}

/// The popover window, built from its config when it doesn't exist yet.
fn popover(app: &AppHandle) -> Option<WebviewWindow> {
    if let Some(window) = app.get_webview_window(POPOVER) {
        return Some(window);
    }
    let built = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == POPOVER)
        .ok_or_else(|| anyhow::anyhow!("no {POPOVER} window in the config"))
        .and_then(|config| Ok(WebviewWindowBuilder::from_config(app, config)?.build()?));
    match built {
        Ok(window) => Some(window),
        Err(e) => {
            let paths = &app.state::<AppState>().paths;
            log::write(
                &paths.log_file,
                &format!("could not open the popover: {e:#}"),
            );
            None
        }
    }
}

/// Hides the popover and destroys its webview once it has stayed hidden for
/// `POPOVER_GRACE`, so the engine isn't resident while the app idles.
fn hide_popover(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPOVER) {
        let _ = window.hide();
    }
    let app = app.clone();
    let ticket = app.state::<AppState>().popover.hidden();
    std::thread::spawn(move || {
        std::thread::sleep(POPOVER_GRACE);
        let main = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(window) = main.get_webview_window(POPOVER) else {
                return;
            };
            let visible = window.is_visible().unwrap_or(true);
            if main
                .state::<AppState>()
                .popover
                .may_destroy(ticket, visible)
            {
                let _ = window.destroy();
            }
        });
    });
}

fn show_popover(app: &AppHandle, near: Option<&Rect>) {
    app.state::<AppState>().popover.shown();
    let Some(window) = popover(app) else {
        return;
    };
    match near {
        Some(rect) => {
            let _ = place_near(&window, rect);
        }
        None => {
            let _ = window.center();
        }
    }
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("popover-shown", ());
}

fn toggle_popover(app: &AppHandle, near: &Rect) {
    if let Some(window) = app.get_webview_window(POPOVER) {
        if window.is_visible().unwrap_or(false) {
            hide_popover(app);
            return;
        }
    }
    let state = app.state::<AppState>();
    let just_hidden = state
        .last_hidden
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < Duration::from_millis(300));
    if !just_hidden {
        show_popover(app, Some(near));
    }
}

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

fn build_tray(app: &AppHandle, paused: bool) -> tauri::Result<()> {
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
fn refresh_tray(app: &AppHandle, snapshot: &Snapshot) {
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

#[cfg(target_os = "macos")]
fn watch_spaces(app: AppHandle, paths: Paths, current: Arc<Mutex<Option<Layout>>>) {
    // macOS sets wallpapers per Space. When the user switches to a Space that
    // still shows older (possibly deleted) photos, switch it to the current ones.
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        let Some(layout) = current.lock().unwrap().clone() else {
            continue;
        };
        let images = paths.images_dir.clone();
        let _ = on_main(&app, move || wallpaper::reconcile(&layout, &images));
    });
}

fn main() {
    let paths = match Paths::discover() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Wallpaper Rotator could not start: {e:#}");
            std::process::exit(1);
        }
    };

    let context = tauri::generate_context!();
    #[cfg(all(windows, debug_assertions))]
    let context = {
        let mut context = context;
        automation::configure(
            &mut context.config_mut().app.windows,
            std::env::var("TAURI_WEBVIEW_AUTOMATION").as_deref() == Ok("true"),
            std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").ok(),
            std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").map(std::path::PathBuf::from),
        );
        context
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_popover(app, None);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_state,
            next_wallpaper,
            favorite,
            save_settings,
            open_link,
            open_favorites,
            hide_window,
            quit
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            let snapshot = Arc::new(Mutex::new(Snapshot::default()));
            let current_layout: Arc<Mutex<Option<Layout>>> = Arc::new(Mutex::new(None));

            let hooks = Hooks {
                apply: {
                    let app = handle.clone();
                    let current_layout = current_layout.clone();
                    Box::new(move |layout| {
                        apply_wallpaper(&app, layout)?;
                        *current_layout.lock().unwrap() = Some(layout.clone());
                        Ok(())
                    })
                },
                displays: {
                    let app = handle.clone();
                    Box::new(move || displays(&app))
                },
                notify: {
                    let app = handle.clone();
                    let snapshot = snapshot.clone();
                    Box::new(move |snap| {
                        *snapshot.lock().unwrap() = snap.clone();
                        refresh_tray(&app, snap);
                        let _ = app.emit("state", snap);
                    })
                },
            };
            let service = Service::new(paths.clone(), hooks)?;
            *current_layout.lock().unwrap() = service.current_layout();
            let first = service.snapshot();
            let start_at_login = service.saved_start_at_login();
            let (commands, _worker) = service.spawn();

            *snapshot.lock().unwrap() = first.clone();
            // Favorites have no hosted preview; the popover shows the cached
            // copy through the asset protocol, allowed for this folder only.
            if let Err(e) = app
                .asset_protocol_scope()
                .allow_directory(&paths.images_dir, false)
            {
                log::write(&paths.log_file, &format!("could not allow previews: {e}"));
            }
            app.manage(AppState {
                commands: Mutex::new(commands),
                snapshot,
                paths: paths.clone(),
                last_hidden: Mutex::new(None),
                pause_item: Mutex::new(None),
                popover: PopoverLife::default(),
            });
            build_tray(&handle, first.settings.paused)?;
            if let Some(enabled) = start_at_login {
                sync_autostart(&handle, enabled);
            }

            #[cfg(target_os = "macos")]
            watch_spaces(handle.clone(), paths.clone(), current_layout.clone());
            #[cfg(not(target_os = "macos"))]
            let _ = current_layout;

            // First run without a key: open the popover so setup is obvious.
            // A debug launch can open the same real popover for WebDriver,
            // without depending on a desktop shell exposing the tray icon.
            if first.needs_setup
                || (cfg!(debug_assertions) && std::env::args().any(|arg| arg == "--show-popover"))
            {
                show_popover(&handle, None);
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Behave like a popover: hide when focus moves elsewhere.
            WindowEvent::Focused(false) => {
                if window.is_visible().unwrap_or(false) {
                    hide_popover(window.app_handle());
                    *window.state::<AppState>().last_hidden.lock().unwrap() = Some(Instant::now());
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                hide_popover(window.app_handle());
            }
            _ => {}
        })
        .build(context)
        .expect("error while building Wallpaper Rotator")
        .run(|_app, event| {
            // Destroying the popover, the only window, must not quit the
            // tray app; it lives on in the tray until Quit.
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn photo(author: &str, source: &str) -> WallpaperView {
        WallpaperView {
            author: author.into(),
            source: source.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_popover_left_hidden_is_destroyed() {
        let life = PopoverLife::default();
        let ticket = life.hidden();
        assert!(life.may_destroy(ticket, false));
    }

    #[test]
    fn reopening_within_the_grace_period_keeps_the_popover() {
        let life = PopoverLife::default();
        let ticket = life.hidden();
        life.shown();
        assert!(
            !life.may_destroy(ticket, false),
            "a show cancels the destroy"
        );
        assert!(!life.may_destroy(ticket, true));
    }

    #[test]
    fn only_the_latest_hide_destroys_the_popover() {
        let life = PopoverLife::default();
        let first = life.hidden();
        life.shown();
        let second = life.hidden();
        assert!(!life.may_destroy(first, false), "stale ticket");
        assert!(life.may_destroy(second, false));
    }

    #[test]
    fn a_visible_popover_is_never_destroyed() {
        let life = PopoverLife::default();
        let ticket = life.hidden();
        assert!(!life.may_destroy(ticket, true));
    }

    #[test]
    fn the_popover_is_created_on_demand_not_at_startup() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let windows = config["app"]["windows"].as_array().unwrap();
        let popover = windows
            .iter()
            .find(|w| w["label"] == POPOVER)
            .expect("the popover's window config");
        assert_eq!(popover["create"], false, "no webview at startup");
        assert_eq!(popover["visible"], false, "built hidden, shown once placed");
        assert_eq!(windows.len(), 1, "no other window keeps a webview alive");
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
