// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rotator_core::config::Config;
use rotator_core::log;
use rotator_core::paths::Paths;
use rotator_core::service::{Command, Hooks, Service, Settings, Snapshot};
use rotator_core::wallpaper::{self, Display, Layout};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, State, WebviewWindow, WindowEvent,
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

/// The platform's own display list (macOS, Windows), or else the monitors
/// the windowing toolkit reports (GNOME).
fn displays(app: &AppHandle) -> Vec<Display> {
    let native = if wallpaper::NEEDS_MAIN_THREAD {
        on_main(app, wallpaper::displays).ok().flatten()
    } else {
        wallpaper::displays()
    };
    native.unwrap_or_else(|| toolkit_displays(app))
}

/// Monitors as GTK sees them. GNOME's spanned wallpaper stretches across
/// the layout in logical pixels, so positions are logical, scaled up by the
/// highest scale factor to keep HiDPI screens sharp.
fn toolkit_displays(app: &AppHandle) -> Vec<Display> {
    let monitors = app.available_monitors().unwrap_or_default();
    let k = monitors
        .iter()
        .map(|m| m.scale_factor())
        .fold(1.0, f64::max);
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let s = m.scale_factor().max(0.1);
            let px = |v: f64| (v / s * k).round();
            let (x, y) = (px(m.position().x as f64), px(m.position().y as f64));
            let name = m
                .name()
                .cloned()
                .unwrap_or_else(|| format!("Display {}", i + 1));
            Display {
                id: format!("{name}@{x},{y}"),
                name,
                x: x as i32,
                y: y as i32,
                width: px(m.size().width as f64) as u32,
                height: px(m.size().height as f64) as u32,
            }
        })
        .collect()
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

#[tauri::command]
fn open_favorites(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let dir = PathBuf::from(&state.snapshot.lock().unwrap().favorites_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn hide_window(window: WebviewWindow) {
    let _ = window.hide();
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

fn show_popover(app: &AppHandle, near: Option<&Rect>) {
    let Some(window) = app.get_webview_window("main") else {
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
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
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
        let tip = match snapshot.current.as_slice() {
            [] => "Wallpaper Rotator".into(),
            photos => {
                let mut authors: Vec<&str> = photos.iter().map(|w| w.author.as_str()).collect();
                authors.dedup();
                let noun = if authors.len() == 1 {
                    "Photo"
                } else {
                    "Photos"
                };
                format!(
                    "Wallpaper Rotator\n{noun} by {} on Unsplash",
                    authors.join(", ")
                )
            }
        };
        let _ = tray.set_tooltip(Some(tip));
    }
    if let Some(state) = app.try_state::<AppState>() {
        if let Some(item) = state.pause_item.lock().unwrap().as_ref() {
            let _ = item.set_checked(snapshot.settings.paused);
        }
    }
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
            });
            build_tray(&handle, first.settings.paused)?;
            sync_autostart(&handle, first.settings.start_at_login);

            #[cfg(target_os = "macos")]
            watch_spaces(handle.clone(), paths.clone(), current_layout.clone());
            #[cfg(not(target_os = "macos"))]
            let _ = current_layout;

            // First run without a key: open the popover so setup is obvious.
            if first.needs_setup {
                show_popover(&handle, None);
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Behave like a popover: hide when focus moves elsewhere.
            WindowEvent::Focused(false) => {
                if window.is_visible().unwrap_or(false) {
                    let _ = window.hide();
                    *window.state::<AppState>().last_hidden.lock().unwrap() = Some(Instant::now());
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building Wallpaper Rotator")
        .run(|_app, event| {
            // Closing the popover must not quit the tray app.
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}
