use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A downloaded photo and the credit Unsplash asks apps to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wallpaper {
    pub id: String,
    pub file: PathBuf,
    pub author: String,
    pub author_url: String,
    pub photo_url: String,
    /// Small hotlinked image for the popover preview.
    pub preview_url: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub download_location: String,
    pub color: Option<String>,
}

/// What the app remembers between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub current: Option<Wallpaper>,
    /// Downloaded ahead of time so "Next" is instant.
    pub next: Option<Wallpaper>,
    pub last_rotated: u64,
    /// Topic slug to Unsplash topic ID.
    pub topic_ids: BTreeMap<String, String>,
}

impl State {
    /// Loads saved state. A missing or unreadable file starts fresh rather
    /// than blocking rotation.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("could not write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))
}

/// Files this app writes into the images folder.
fn is_ours(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    name.ends_with(".jpg") || name.ends_with(".jpg.part")
}

#[derive(Debug, Default, PartialEq)]
pub struct CleanupReport {
    pub removed_files: usize,
    pub removed_bytes: u64,
    /// The prefetched photo was dropped to stay under the cap.
    pub dropped_next: bool,
}

/// Deletes every image except the current and prefetched wallpapers, then
/// drops the prefetched one too if the cache is still over `cap_bytes`.
/// The current wallpaper is never deleted.
pub fn cleanup(images_dir: &Path, state: &mut State, cap_bytes: u64) -> Result<CleanupReport> {
    let mut report = CleanupReport::default();
    let keep: HashSet<PathBuf> = [&state.current, &state.next]
        .into_iter()
        .flatten()
        .map(|w| w.file.clone())
        .collect();

    let entries = match fs::read_dir(images_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => {
            return Err(e).with_context(|| format!("could not list {}", images_dir.display()))
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_ours(&path) || keep.contains(&path) {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if fs::remove_file(&path).is_ok() {
            report.removed_files += 1;
            report.removed_bytes += size;
        }
    }

    let size_of = |w: &Option<Wallpaper>| {
        w.as_ref()
            .and_then(|w| fs::metadata(&w.file).ok())
            .map(|m| m.len())
            .unwrap_or(0)
    };
    if size_of(&state.current) + size_of(&state.next) > cap_bytes {
        if let Some(next) = state.next.take() {
            let size = size_of(&Some(next.clone()));
            if fs::remove_file(&next.file).is_ok() {
                report.removed_files += 1;
                report.removed_bytes += size;
            }
            report.dropped_next = true;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallpaper(dir: &Path, id: &str, bytes: usize) -> Wallpaper {
        let file = dir.join(format!("{id}.jpg"));
        fs::write(&file, vec![0u8; bytes]).unwrap();
        Wallpaper {
            id: id.into(),
            file,
            author: "A".into(),
            author_url: String::new(),
            photo_url: String::new(),
            preview_url: String::new(),
            location: None,
            description: None,
            download_location: String::new(),
            color: None,
        }
    }

    #[test]
    fn keeps_only_current_and_next() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path();
        let mut state = State {
            current: Some(wallpaper(images, "cur", 10)),
            next: Some(wallpaper(images, "nxt", 10)),
            ..State::default()
        };
        for old in ["a", "b", "c"] {
            wallpaper(images, old, 100);
        }
        fs::write(images.join("half.jpg.part"), b"xx").unwrap();

        let report = cleanup(images, &mut state, u64::MAX).unwrap();

        assert_eq!(report.removed_files, 4);
        assert_eq!(report.removed_bytes, 302);
        let mut left: Vec<_> = fs::read_dir(images)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["cur.jpg", "nxt.jpg"]);
    }

    #[test]
    fn leaves_files_it_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), b"mine").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        cleanup(dir.path(), &mut State::default(), u64::MAX).unwrap();
        assert!(dir.path().join("notes.txt").exists());
        assert!(dir.path().join("sub").exists());
    }

    #[test]
    fn over_cap_drops_next_but_never_current() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State {
            current: Some(wallpaper(dir.path(), "cur", 600)),
            next: Some(wallpaper(dir.path(), "nxt", 600)),
            ..State::default()
        };
        let report = cleanup(dir.path(), &mut state, 1000).unwrap();
        assert!(report.dropped_next);
        assert!(state.next.is_none());
        assert!(dir.path().join("cur.jpg").exists());
        assert!(!dir.path().join("nxt.jpg").exists());

        // Current alone over the cap stays.
        let report = cleanup(dir.path(), &mut state, 100).unwrap();
        assert_eq!(report, CleanupReport::default());
        assert!(dir.path().join("cur.jpg").exists());
    }

    #[test]
    fn missing_images_dir_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let report = cleanup(&dir.path().join("nope"), &mut State::default(), 1).unwrap();
        assert_eq!(report, CleanupReport::default());
    }

    #[test]
    fn state_round_trips_and_survives_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = State {
            current: Some(wallpaper(dir.path(), "cur", 1)),
            last_rotated: 42,
            ..State::default()
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
        fs::write(&path, b"{not json").unwrap();
        assert_eq!(State::load(&path), State::default());
    }
}
