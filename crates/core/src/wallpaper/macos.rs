use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSScreen, NSWorkspace};
use objc2_foundation::{NSDictionary, NSString, NSURL};

fn main_thread() -> Result<MainThreadMarker> {
    MainThreadMarker::new().ok_or_else(|| anyhow!("macOS wallpaper calls must run on the main thread"))
}

fn file_url(path: &Path) -> Result<Retained<NSURL>> {
    let path = path.to_str().context("wallpaper path is not valid UTF-8")?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

fn set_for(workspace: &NSWorkspace, url: &NSURL, screen: &NSScreen) -> Result<()> {
    let options = NSDictionary::new();
    #[allow(unused_unsafe)]
    unsafe { workspace.setDesktopImageURL_forScreen_options_error(url, screen, &options) }
        .map_err(|e| anyhow!("macOS would not set the wallpaper: {}", e.localizedDescription()))
}

pub fn set(path: &Path) -> Result<()> {
    let mtm = main_thread()?;
    let url = file_url(path)?;
    let workspace = NSWorkspace::sharedWorkspace();
    for screen in NSScreen::screens(mtm).iter() {
        set_for(&workspace, &url, &screen)?;
    }
    Ok(())
}

pub fn reconcile(current: &Path, images_dir: &Path) -> Result<bool> {
    let mtm = main_thread()?;
    let workspace = NSWorkspace::sharedWorkspace();
    let mut changed = false;
    for screen in NSScreen::screens(mtm).iter() {
        let shown = workspace
            .desktopImageURLForScreen(&screen)
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()));
        // Only replace our own stale photos, never a wallpaper the user picked.
        if let Some(shown) = shown {
            if shown != current && shown.starts_with(images_dir) {
                set_for(&workspace, &file_url(current)?, &screen)?;
                changed = true;
            }
        }
    }
    Ok(changed)
}
