use std::path::PathBuf;
use tauri::utils::config::WindowConfig;

/// Carry the driver's WebView2 overrides through the API on elevated Windows.
/// Called only by debug Windows builds; the flag is set by tauri-driver.
pub fn configure(
    windows: &mut [WindowConfig],
    automation: bool,
    arguments: Option<String>,
    data_directory: Option<PathBuf>,
) {
    if !automation {
        return;
    }
    // Runtime 150+ ignores the driver's WEBVIEW2_* environment overrides in
    // elevated processes (including GitHub-hosted Windows runners). Supplying
    // them in WindowConfig reaches CreateCoreWebView2EnvironmentWithOptions.
    // https://github.com/MicrosoftEdge/WebView2Feedback/issues/5645
    for window in windows {
        if let Some(arguments) = &arguments {
            let existing = window
                .additional_browser_args
                .as_deref()
                .unwrap_or("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection");
            window.additional_browser_args = Some(format!("{existing} {arguments}"));
        }
        if let Some(data_directory) = &data_directory {
            window.data_directory = Some(data_directory.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webdriver_arguments_and_profile_update_every_window_config() {
        let mut windows = [WindowConfig::default(), WindowConfig::default()];
        let profile = PathBuf::from("isolated webdriver profile");
        configure(
            &mut windows,
            true,
            Some("--remote-debugging-port=0 --test-type=webdriver".into()),
            Some(profile.clone()),
        );
        for window in windows {
            let arguments = window.additional_browser_args.unwrap();
            assert!(arguments.contains("--remote-debugging-port=0"));
            assert!(arguments.contains("--test-type=webdriver"));
            // Setting additionalBrowserArgs replaces Wry's defaults.
            assert!(arguments
                .contains("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection"));
            assert_eq!(window.data_directory, Some(profile.clone()));
        }
    }

    #[test]
    fn webdriver_preserves_configured_browser_arguments() {
        let mut windows = [WindowConfig {
            additional_browser_args: Some("--custom-setting".into()),
            ..Default::default()
        }];
        configure(
            &mut windows,
            true,
            Some("--remote-debugging-port=0".into()),
            None,
        );
        assert_eq!(
            windows[0].additional_browser_args.as_deref(),
            Some("--custom-setting --remote-debugging-port=0")
        );
    }

    #[test]
    fn ordinary_launch_ignores_driver_overrides() {
        let mut windows = [WindowConfig {
            additional_browser_args: Some("--custom-setting".into()),
            data_directory: Some(PathBuf::from("configured profile")),
            ..Default::default()
        }];
        configure(
            &mut windows,
            false,
            Some("--remote-debugging-port=0".into()),
            Some(PathBuf::from("driver profile")),
        );
        assert_eq!(
            windows[0].additional_browser_args.as_deref(),
            Some("--custom-setting")
        );
        assert_eq!(
            windows[0].data_directory,
            Some(PathBuf::from("configured profile"))
        );
    }

    #[test]
    fn absent_driver_overrides_preserve_webview_defaults() {
        let mut windows = [WindowConfig::default()];
        configure(&mut windows, true, None, None);
        assert!(windows[0].additional_browser_args.is_none());
        assert!(windows[0].data_directory.is_none());
    }
}
