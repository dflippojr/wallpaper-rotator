//! What took the focus when the popover hid on blur, for the app log (#98).
//! On Windows the OS can say which window is in front now; elsewhere the log
//! line only records the hide.

/// The log line for a popover hidden because it lost focus. `taker` names
/// the window that has the focus now, when the OS can say.
pub(super) fn blur_message(taker: Option<&str>) -> String {
    match taker {
        Some(taker) => format!("popover hidden on blur; focus went to {taker}"),
        None => "popover hidden on blur".to_string(),
    }
}

/// The foreground window: its class, its process's executable and whether
/// that process is this app.
#[cfg(windows)]
pub(super) fn taker() -> Option<String> {
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
            return Some("no window (the foreground is changing)".to_string());
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
        Some(format!(
            "window class {class:?} of {exe} (pid {pid}, {whose})"
        ))
    }
}

#[cfg(not(windows))]
pub(super) fn taker() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blur_line_names_the_window_that_took_the_focus() {
        assert_eq!(
            blur_message(Some("window class \"Progman\" of explorer.exe")),
            "popover hidden on blur; focus went to window class \"Progman\" of explorer.exe"
        );
        assert_eq!(blur_message(None), "popover hidden on blur");
    }

    #[test]
    fn the_taker_is_known_on_windows_only() {
        assert_eq!(taker().is_some(), cfg!(windows));
    }
}
