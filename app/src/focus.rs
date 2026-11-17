//! Who is in front when the popover reports a blur (#98). On Windows the OS
//! can say which window is in front now, so the popover can tell a real blur
//! (another window took the foreground) from focus moving inside its own
//! window, which WebView2 can do while it loads. Elsewhere every blur hides
//! the popover and the log line only records the hide.

/// The foreground window when a blur was handled.
pub(super) struct Foreground {
    /// Its handle, to compare with the popover's.
    pub(super) window: isize,
    /// Its class, its process's executable and whether that is this app.
    pub(super) description: String,
}

/// Whether a blur leaves the popover in place: it is still the foreground
/// window, so the focus only moved inside it.
pub(super) fn keeps_popover(front: Option<&Foreground>, popover: Option<isize>) -> bool {
    matches!((front, popover), (Some(front), Some(popover)) if front.window == popover)
}

/// The log line for a popover hidden because it lost focus.
pub(super) fn blur_message(front: Option<&Foreground>) -> String {
    match front {
        Some(front) => format!(
            "popover hidden on blur; focus went to {}",
            front.description
        ),
        None => "popover hidden on blur".to_string(),
    }
}

/// The log line for a blur that left the popover in front.
pub(super) fn kept_message(front: &Foreground) -> String {
    format!(
        "popover kept after a blur: it is still the foreground window ({})",
        front.description
    )
}

/// The popover's window handle, to compare with the foreground window.
#[cfg(windows)]
pub(super) fn handle(window: &tauri::WebviewWindow) -> Option<isize> {
    window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
}

#[cfg(not(windows))]
pub(super) fn handle(_window: &tauri::WebviewWindow) -> Option<isize> {
    None
}

/// The foreground window, as the OS reports it.
#[cfg(windows)]
pub(super) fn foreground() -> Option<Foreground> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
    };

    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return Some(Foreground {
                window: 0,
                description: "no window (the foreground is changing)".to_string(),
            });
        }
        let mut class = [0u16; 256];
        let len = GetClassNameW(window, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let exe = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .ok()
            .and_then(|process| {
                let mut name = [0u16; 1024];
                let mut size = name.len() as u32;
                let named = QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(name.as_mut_ptr()),
                    &mut size,
                );
                let _ = CloseHandle(process);
                named
                    .ok()
                    .map(|()| String::from_utf16_lossy(&name[..size as usize]))
            })
            .unwrap_or_else(|| "an unknown process".to_string());
        let whose = if pid == std::process::id() {
            "this app"
        } else {
            "another process"
        };
        Some(Foreground {
            window: window.0 as isize,
            description: format!("window class {class:?} of {exe} (pid {pid}, {whose})"),
        })
    }
}

#[cfg(not(windows))]
pub(super) fn foreground() -> Option<Foreground> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front(window: isize) -> Foreground {
        Foreground {
            window,
            description: "window class \"Progman\" of explorer.exe".to_string(),
        }
    }

    #[test]
    fn a_blur_keeps_the_popover_only_while_it_is_still_in_front() {
        assert!(
            keeps_popover(Some(&front(7)), Some(7)),
            "focus moved inside"
        );
        assert!(!keeps_popover(Some(&front(8)), Some(7)), "another window");
        assert!(!keeps_popover(Some(&front(0)), Some(7)), "no foreground");
        assert!(!keeps_popover(None, None), "the OS can't say: hide");
        assert!(!keeps_popover(Some(&front(7)), None));
    }

    #[test]
    fn the_blur_line_names_the_window_that_took_the_focus() {
        assert_eq!(
            blur_message(Some(&front(8))),
            "popover hidden on blur; focus went to window class \"Progman\" of explorer.exe"
        );
        assert_eq!(blur_message(None), "popover hidden on blur");
        assert!(kept_message(&front(7)).starts_with("popover kept after a blur"));
    }

    #[test]
    fn the_foreground_is_known_on_windows_only() {
        assert_eq!(foreground().is_some(), cfg!(windows));
    }
}
