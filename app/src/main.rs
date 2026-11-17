// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(any(all(windows, debug_assertions), test))]
mod automation;
mod commands;
mod popover;
mod tray;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rotator_core::log;
use rotator_core::paths::Paths;
use rotator_core::service::{Command, Hooks, Service, Snapshot};
use rotator_core::wallpaper::{self, Display, Layout, Monitor};
use tauri::menu::CheckMenuItem;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};

use popover::{hide_popover, show_popover, PopoverLife, POPOVER};
use tray::{build_tray, refresh_tray};

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
            commands::get_state,
            commands::next_wallpaper,
            commands::favorite,
            commands::save_settings,
            commands::open_link,
            commands::open_favorites,
            commands::hide_window,
            commands::quit
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
        .run(|app, event| match event {
            // Destroying the popover, the only window, must not quit the
            // tray app; it lives on in the tray until Quit.
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => api.prevent_exit(),
            // Tauri has dropped the window by the time this runs, so a
            // waiting show can build a new one.
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::Destroyed,
                ..
            } if label == POPOVER => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.popover.destroyed();
                    log::write(&state.paths.log_file, "popover webview destroyed");
                }
            }
            _ => {}
        });
}
