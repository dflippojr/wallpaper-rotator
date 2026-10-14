use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSImageScaling, NSScreen, NSWorkspace, NSWorkspaceDesktopImageAllowClippingKey,
    NSWorkspaceDesktopImageScalingKey,
};
use objc2_foundation::{ns_string, NSDictionary, NSNumber, NSString, NSURL};

use super::{Display, Layout};

fn main_thread() -> Result<MainThreadMarker> {
    MainThreadMarker::new()
        .ok_or_else(|| anyhow!("macOS wallpaper calls must run on the main thread"))
}

fn file_url(path: &Path) -> Result<Retained<NSURL>> {
    let path = path.to_str().context("wallpaper path is not valid UTF-8")?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

/// The display ID macOS uses for the screen, which stays the same while it
/// is connected.
fn screen_id(screen: &NSScreen) -> String {
    screen
        .deviceDescription()
        .objectForKey(ns_string!("NSScreenNumber"))
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .map(|n| n.unsignedIntValue().to_string())
        .unwrap_or_else(|| screen.localizedName().to_string())
}

/// "Fill screen": scale to cover, cropping the overflow. Photos are sized
/// for their screen, so this only matters when one is borrowed by a screen
/// of another shape.
fn fill_options() -> Retained<NSDictionary<NSString, AnyObject>> {
    let scaling = NSNumber::new_usize(NSImageScaling::ScaleProportionallyUpOrDown.0);
    let clipping = NSNumber::new_bool(true);
    let keys = unsafe {
        [
            NSWorkspaceDesktopImageScalingKey,
            NSWorkspaceDesktopImageAllowClippingKey,
        ]
    };
    let values: [&AnyObject; 2] = [&scaling, &clipping];
    NSDictionary::from_slices(&keys, &values)
}

fn set_for(
    workspace: &NSWorkspace,
    file: &Path,
    screen: &NSScreen,
    options: &NSDictionary<NSString, AnyObject>,
) -> Result<()> {
    let url = file_url(file)?;
    #[allow(unused_unsafe)]
    unsafe { workspace.setDesktopImageURL_forScreen_options_error(&url, screen, options) }.map_err(
        |e| {
            anyhow!(
                "macOS would not set the wallpaper: {}",
                e.localizedDescription()
            )
        },
    )
}

pub fn displays() -> Result<Vec<Display>> {
    let mtm = main_thread()?;
    Ok(NSScreen::screens(mtm)
        .iter()
        .map(|screen| {
            // Frames are in points; the backing scale gives real pixels.
            let frame = screen.frame();
            let scale = screen.backingScaleFactor();
            Display {
                id: screen_id(&screen),
                name: screen.localizedName().to_string(),
                x: (frame.origin.x * scale).round() as i32,
                y: (frame.origin.y * scale).round() as i32,
                width: (frame.size.width * scale).round() as u32,
                height: (frame.size.height * scale).round() as u32,
            }
        })
        .collect())
}

pub fn set(layout: &Layout) -> Result<()> {
    let mtm = main_thread()?;
    let workspace = NSWorkspace::sharedWorkspace();
    let options = fill_options();
    for (i, screen) in NSScreen::screens(mtm).iter().enumerate() {
        if let Some(file) = layout.file_for(&screen_id(&screen), i) {
            set_for(&workspace, file, &screen, &options)?;
        }
    }
    Ok(())
}

pub fn reconcile(layout: &Layout, images_dir: &Path) -> Result<bool> {
    let mtm = main_thread()?;
    let workspace = NSWorkspace::sharedWorkspace();
    let options = fill_options();
    let mut changed = false;
    for (i, screen) in NSScreen::screens(mtm).iter().enumerate() {
        let Some(want) = layout.file_for(&screen_id(&screen), i) else {
            continue;
        };
        let shown = workspace
            .desktopImageURLForScreen(&screen)
            .and_then(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()));
        // Only replace our own stale photos, never a wallpaper the user picked.
        if let Some(shown) = shown {
            if shown != want && shown.starts_with(images_dir) {
                set_for(&workspace, want, &screen, &options)?;
                changed = true;
            }
        }
    }
    Ok(changed)
}
