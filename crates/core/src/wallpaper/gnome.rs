use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

const SCHEMA: &str = "org.gnome.desktop.background";

pub fn set(path: &Path) -> Result<()> {
    let uri = file_uri(path);
    gsettings("picture-uri", &uri)?;
    // Older GNOME releases have no dark-mode key; that's fine.
    let _ = gsettings("picture-uri-dark", &uri);
    gsettings("picture-options", "zoom")
}

fn gsettings(key: &str, value: &str) -> Result<()> {
    let output = Command::new("gsettings")
        .args(["set", SCHEMA, key, value])
        .output()
        .context("could not run gsettings; is this a GNOME desktop?")?;
    if !output.status.success() {
        bail!(
            "gsettings set {key} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// A `file://` URI with everything outside the unreserved set escaped.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_spaces_and_unicode() {
        assert_eq!(
            file_uri(Path::new("/home/d/.cache/Wallpaper Rotator/images/ä.jpg")),
            "file:///home/d/.cache/Wallpaper%20Rotator/images/%C3%A4.jpg"
        );
    }
}
