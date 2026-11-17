//! The popover window: its life cycle (built on demand, shown next to the
//! tray icon, hidden on blur, destroyed after a grace period) and the ticket
//! state machine that orders shows and hides.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use rotator_core::log;
use tauri::utils::config::WindowConfig;
use tauri::webview::PageLoadEvent;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, WebviewWindow, WebviewWindowBuilder,
};

use crate::{focus, AppState};

/// The popover's label in `tauri.conf.json`. Its window is not created at
/// startup (`"create": false`); `popover` builds it from that config.
pub(super) const POPOVER: &str = "main";

/// How long a hidden popover keeps its webview, so a quick re-open is instant.
/// After that it is destroyed and the webview engine's processes can exit.
const POPOVER_GRACE: Duration = Duration::from_secs(30);

/// `POPOVER_GRACE`, or in a debug build the milliseconds in
/// `WALLPAPER_ROTATOR_POPOVER_GRACE_MS`, so the real-app journey can watch a
/// destroy without waiting 30 s.
fn popover_grace() -> Duration {
    let debug_override = cfg!(debug_assertions)
        .then(|| std::env::var("WALLPAPER_ROTATOR_POPOVER_GRACE_MS").ok())
        .flatten()
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis);
    debug_override.unwrap_or(POPOVER_GRACE)
}

/// Orders the popover's shows and hides. Each one takes a ticket and makes
/// every older ticket stale. A show commits only with the newest ticket, so a
/// slow show can't undo a later hide; a destroy goes ahead only for the most
/// recent hide, with no show since.
///
/// Tauri destroys a window asynchronously, so between a destroy and the
/// window's `Destroyed` event the popover is "dying": still found by label,
/// but about to go. `popover` waits that out instead of reusing it.
#[derive(Default)]
pub(super) struct PopoverLife {
    generation: AtomicU64,
    /// Held while the window is looked up and, if missing, built.
    building: Mutex<()>,
    dying: Mutex<bool>,
    gone: Condvar,
}

impl PopoverLife {
    /// A show was asked for: pending destroys and older shows no longer
    /// apply. Returns the ticket the show must hold when it commits.
    fn shown(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether the show that took `ticket` should still make the popover
    /// visible: no later show or hide came in while it was being built.
    fn may_show(&self, ticket: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == ticket
    }

    /// The popover was hidden: returns the ticket a later destroy must hold.
    fn hidden(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether the destroy scheduled with `ticket` should still go ahead.
    fn may_destroy(&self, ticket: u64, visible: bool) -> bool {
        !visible && self.generation.load(Ordering::SeqCst) == ticket
    }

    /// Like `may_destroy`, and when it may, marks the popover dying in the
    /// same step, so a show that comes in afterwards waits for the window to
    /// go rather than reusing it.
    fn begin_destroy(&self, ticket: u64, visible: bool) -> bool {
        let mut dying = self.dying.lock().unwrap();
        let go = self.may_destroy(ticket, visible);
        *dying |= go;
        go
    }

    /// The window is gone (its `Destroyed` event), or its destroy failed.
    pub(super) fn destroyed(&self) {
        *self.dying.lock().unwrap() = false;
        self.gone.notify_all();
    }

    /// Waits up to `limit` for a dying popover to be gone. False on timeout.
    fn wait_until_gone(&self, limit: Duration) -> bool {
        let dying = self.dying.lock().unwrap();
        let (dying, _) = self
            .gone
            .wait_timeout_while(dying, limit, |dying| *dying)
            .unwrap();
        !*dying
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
/// Never call it on the main thread: on Windows, building a webview from an
/// event handler deadlocks (see `WebviewWindowBuilder::from_config`).
fn popover(app: &AppHandle) -> Option<WebviewWindow> {
    // Two quick shows must not both build the window.
    let state = app.state::<AppState>();
    let _building = state.popover.building.lock().unwrap();
    if !state.popover.wait_until_gone(Duration::from_secs(5)) {
        log::write(&state.paths.log_file, "popover webview still closing");
        return None;
    }
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
        .map(popover_config)
        .and_then(|config| {
            let log_file = state.paths.log_file.clone();
            Ok(WebviewWindowBuilder::from_config(app, &config)?
                .on_page_load(move |_, payload| {
                    if payload.event() == PageLoadEvent::Finished {
                        log::write(&log_file, "popover webview loaded");
                    }
                })
                .build()?)
        });
    match built {
        Ok(window) => {
            log::write(&state.paths.log_file, "popover webview created");
            Some(window)
        }
        Err(e) => {
            log::write(
                &state.paths.log_file,
                &format!("could not open the popover: {e:#}"),
            );
            None
        }
    }
}

/// The popover's window config as this OS builds it. On macOS the webview is
/// incognito, with no persistent data store: WKWebView's default store keeps
/// WebKit's Networking process alive after the last webview is gone. The
/// popover keeps nothing in web storage. Not on Linux, where Wry's ephemeral
/// WebKitGTK context refuses WebDriver automation (the real-app journey), and
/// not on Windows until #44 measures whether WebView2 needs it.
fn popover_config(config: &WindowConfig) -> WindowConfig {
    let mut config = config.clone();
    config.incognito |= cfg!(target_os = "macos");
    config
}

/// Hides the popover and destroys its webview once it has stayed hidden for
/// `popover_grace()`, so the engine isn't resident while the app idles. Runs
/// on the main thread (window events, commands, tray handlers), as do the
/// commits in `show_popover`, so the two can't interleave.
pub(super) fn hide_popover(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPOVER) {
        let _ = window.hide();
    }
    let app = app.clone();
    let ticket = app.state::<AppState>().popover.hidden();
    std::thread::spawn(move || {
        std::thread::sleep(popover_grace());
        let main = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(window) = main.get_webview_window(POPOVER) else {
                return;
            };
            let visible = window.is_visible().unwrap_or(true);
            let state = main.state::<AppState>();
            if state.popover.begin_destroy(ticket, visible) && window.destroy().is_err() {
                state.popover.destroyed();
            }
        });
    });
}

/// How long after a blur, on Windows, the popover looks at which window is
/// in front before it decides to hide.
const BLUR_SETTLE: Duration = Duration::from_millis(150);

/// The visible popover lost focus. Elsewhere it hides at once. On Windows it
/// hides only if, a moment later, another window is in front: WebView2 can
/// move the focus inside the popover's own window while it loads, and Tauri
/// reports that as a blur too (#98), which hid a popover that was still in
/// front of the user.
pub(super) fn popover_blurred(app: &AppHandle) {
    if !cfg!(windows) {
        hide_on_blur(app, None);
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(BLUR_SETTLE);
        let main = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(window) = main.get_webview_window(POPOVER) else {
                return;
            };
            if !window.is_visible().unwrap_or(false) {
                return;
            }
            let front = focus::foreground();
            if focus::keeps_popover(front.as_ref(), focus::handle(&window)) {
                if let Some(front) = &front {
                    let log_file = &main.state::<AppState>().paths.log_file;
                    log::write(log_file, &focus::kept_message(front));
                }
                return;
            }
            hide_on_blur(&main, front.as_ref());
        });
    });
}

/// Hides the popover because it lost focus, and remembers when, so a tray
/// click that caused the blur doesn't immediately reopen it.
fn hide_on_blur(app: &AppHandle, front: Option<&focus::Foreground>) {
    let state = app.state::<AppState>();
    log::write(&state.paths.log_file, &focus::blur_message(front));
    hide_popover(app);
    *state.last_hidden.lock().unwrap() = Some(Instant::now());
}

/// Shows the popover, next to the tray icon when `near` is given. Callers are
/// tray, menu, single-instance and setup handlers on the main thread, so the
/// window is found or built and placed on a worker thread. Making it visible
/// goes back to the main thread and happens only if no later show or hide
/// came in meanwhile.
pub(super) fn show_popover(app: &AppHandle, near: Option<&Rect>) {
    // A second launch can arrive before setup has registered the state (the
    // single-instance listener runs on its own); show once it has.
    let Some(state) = app.try_state::<AppState>() else {
        let (probe, app) = (app.clone(), app.clone());
        let near = near.cloned();
        when_ready(
            move || probe.try_state::<AppState>().is_some(),
            move || show_popover(&app, near.as_ref()),
            Duration::from_millis(100),
            100,
        );
        return;
    };
    let ticket = state.popover.shown();
    let app = app.clone();
    let near = near.cloned();
    std::thread::spawn(move || {
        let Some(window) = popover(&app) else {
            return;
        };
        let main = app.clone();
        let _ = app.run_on_main_thread(move || {
            // A stale show neither moves nor shows the window.
            if !main.state::<AppState>().popover.may_show(ticket) {
                return;
            }
            let _ = match near {
                Some(rect) => place_near(&window, &rect),
                None => window.center(),
            };
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("popover-shown", ());
        });
    });
}

/// On a new thread, checks `ready` every `interval`, up to `tries` times, and
/// runs `then` once it holds. The handle says whether `then` ran.
fn when_ready(
    ready: impl Fn() -> bool + Send + 'static,
    then: impl FnOnce() + Send + 'static,
    interval: Duration,
    tries: u32,
) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        for _ in 0..tries {
            std::thread::sleep(interval);
            if ready() {
                then();
                return true;
            }
        }
        false
    })
}

pub(super) fn toggle_popover(app: &AppHandle, near: &Rect) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
    fn a_hide_cancels_a_show_still_being_built() {
        let life = PopoverLife::default();
        let show = life.shown();
        assert!(life.may_show(show));
        let hide = life.hidden();
        assert!(!life.may_show(show), "the slow show must not undo the hide");
        assert!(life.may_destroy(hide, false));
    }

    #[test]
    fn only_the_latest_of_two_quick_shows_commits() {
        let life = PopoverLife::default();
        let first = life.shown();
        let second = life.shown();
        assert!(!life.may_show(first));
        assert!(life.may_show(second));
    }

    #[test]
    fn the_grace_period_is_30_seconds_unless_a_debug_build_overrides_it() {
        assert_eq!(POPOVER_GRACE, Duration::from_secs(30));
        // Not set in the test environment.
        if std::env::var_os("WALLPAPER_ROTATOR_POPOVER_GRACE_MS").is_none() {
            assert_eq!(popover_grace(), POPOVER_GRACE);
        }
    }

    #[test]
    fn a_show_after_a_destroy_waits_for_the_window_to_go() {
        let life = Arc::new(PopoverLife::default());
        let ticket = life.hidden();
        assert!(life.begin_destroy(ticket, false));
        life.shown();
        assert!(
            !life.wait_until_gone(Duration::from_millis(50)),
            "still dying"
        );
        let waiter = {
            let life = life.clone();
            std::thread::spawn(move || life.wait_until_gone(Duration::from_secs(10)))
        };
        life.destroyed();
        assert!(waiter.join().unwrap(), "released by the Destroyed event");
    }

    #[test]
    fn a_stale_destroy_does_not_mark_the_popover_dying() {
        let life = PopoverLife::default();
        let ticket = life.hidden();
        life.shown();
        assert!(!life.begin_destroy(ticket, false));
        assert!(life.wait_until_gone(Duration::ZERO), "nothing to wait for");
    }

    #[test]
    fn a_show_before_setup_runs_once_setup_is_done() {
        use std::sync::atomic::{AtomicBool, AtomicU32};
        let registered = Arc::new(AtomicBool::new(false));
        let shows = Arc::new(AtomicU32::new(0));
        let waiting = {
            let (registered, shows) = (registered.clone(), shows.clone());
            when_ready(
                move || registered.load(Ordering::SeqCst),
                move || {
                    shows.fetch_add(1, Ordering::SeqCst);
                },
                Duration::from_millis(5),
                2000,
            )
        };
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(shows.load(Ordering::SeqCst), 0, "not before setup");
        registered.store(true, Ordering::SeqCst);
        assert!(waiting.join().unwrap());
        assert_eq!(shows.load(Ordering::SeqCst), 1, "exactly once after");
    }

    #[test]
    fn a_show_before_setup_gives_up_if_setup_never_finishes() {
        let ran = Arc::new(Mutex::new(false));
        let flag = ran.clone();
        let waiting = when_ready(
            || false,
            move || *flag.lock().unwrap() = true,
            Duration::from_millis(1),
            5,
        );
        assert!(!waiting.join().unwrap());
        assert!(!*ran.lock().unwrap());
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
    fn only_macos_builds_an_incognito_popover() {
        let built = popover_config(&WindowConfig::default());
        assert_eq!(built.incognito, cfg!(target_os = "macos"));
        let kept = popover_config(&WindowConfig {
            incognito: true,
            ..Default::default()
        });
        assert!(kept.incognito, "never turns a configured incognito off");
    }
}
