use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::wallpaper::{Display, Layout};

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
    /// The display this photo was sized for; `None` means every display.
    #[serde(default)]
    pub display: Option<Display>,
}

/// The photos on screen together: one for every display, or one per display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Set {
    pub photos: Vec<Wallpaper>,
    /// GNOME: the per-display photos stitched into one image.
    #[serde(default)]
    pub spanned: Option<PathBuf>,
}

impl Set {
    pub fn single(photo: Wallpaper) -> Self {
        Self {
            photos: vec![photo],
            spanned: None,
        }
    }

    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.photos
            .iter()
            .map(|w| w.file.as_path())
            .chain(self.spanned.as_deref())
    }

    /// Whether every file is still on disk (the OS may purge caches).
    pub fn on_disk(&self) -> bool {
        self.files().all(Path::exists)
    }

    pub fn layout(&self) -> Layout {
        if let Some(spanned) = &self.spanned {
            return Layout::Spanned(spanned.clone());
        }
        match self.photos.as_slice() {
            [only] if only.display.is_none() => Layout::Same(only.file.clone()),
            photos => Layout::Each(
                photos
                    .iter()
                    .map(|w| {
                        let id = w.display.as_ref().map(|d| d.id.clone());
                        (id.unwrap_or_default(), w.file.clone())
                    })
                    .collect(),
            ),
        }
    }

    /// Whether this set was chosen for these displays: one shared photo when
    /// `each` is off, or exactly one photo per display (same size and place)
    /// when it is on.
    pub fn fits(&self, displays: &[Display], each: bool) -> bool {
        if !each {
            return matches!(self.photos.as_slice(), [only] if only.display.is_none());
        }
        let mut ours: Vec<&Display> = self
            .photos
            .iter()
            .filter_map(|w| w.display.as_ref())
            .collect();
        let mut live: Vec<&Display> = displays.iter().collect();
        ours.sort_by(|a, b| a.id.cmp(&b.id));
        live.sort_by(|a, b| a.id.cmp(&b.id));
        ours.len() == self.photos.len() && ours == live
    }
}

/// What the app remembers between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    #[serde(deserialize_with = "set_or_photo")]
    pub current: Option<Set>,
    /// Downloaded ahead of time so "Next" is instant.
    #[serde(deserialize_with = "set_or_photo")]
    pub next: Option<Set>,
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

/// Reads a set, or a single photo as saved by version 0.1.
fn set_or_photo<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Set>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Set(Set),
        Photo(Box<Wallpaper>),
    }
    Ok(Option::<Saved>::deserialize(d)?.map(|saved| match saved {
        Saved::Set(set) => set,
        Saved::Photo(photo) => Set::single(*photo),
    }))
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

/// Deletes every image except the current and prefetched sets, then drops
/// the prefetched set too if the cache is still over `cap_bytes`. The
/// current set is never deleted.
pub fn cleanup(images_dir: &Path, state: &mut State, cap_bytes: u64) -> Result<CleanupReport> {
    let mut report = CleanupReport::default();
    let keep: HashSet<PathBuf> = [&state.current, &state.next]
        .into_iter()
        .flatten()
        .flat_map(Set::files)
        .map(Path::to_path_buf)
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

    let size_of = |set: &Option<Set>| -> u64 {
        set.iter()
            .flat_map(Set::files)
            .filter_map(|f| fs::metadata(f).ok())
            .map(|m| m.len())
            .sum()
    };
    if size_of(&state.current) + size_of(&state.next) > cap_bytes {
        if let Some(next) = state.next.take() {
            for file in next.files() {
                let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(file).is_ok() {
                    report.removed_files += 1;
                    report.removed_bytes += size;
                }
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
            display: None,
        }
    }

    fn one(dir: &Path, id: &str, bytes: usize) -> Option<Set> {
        Some(Set::single(wallpaper(dir, id, bytes)))
    }

    #[test]
    fn keeps_only_current_and_next() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path();
        let mut state = State {
            current: one(images, "cur", 10),
            next: one(images, "nxt", 10),
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
            current: one(dir.path(), "cur", 600),
            next: one(dir.path(), "nxt", 600),
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
            current: one(dir.path(), "cur", 1),
            last_rotated: 42,
            ..State::default()
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
        fs::write(&path, b"{not json").unwrap();
        assert_eq!(State::load(&path), State::default());
    }

    fn display(id: &str, width: u32) -> Display {
        Display {
            id: id.into(),
            name: id.into(),
            x: 0,
            y: 0,
            width,
            height: 1000,
        }
    }

    #[test]
    fn per_display_sets_keep_every_file_and_match_their_displays() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = wallpaper(dir.path(), "a", 10);
        a.display = Some(display("1", 2000));
        let mut b = wallpaper(dir.path(), "b", 10);
        b.display = Some(display("2", 3000));
        let spanned = dir.path().join("span-a.jpg");
        fs::write(&spanned, b"x").unwrap();
        let set = Set {
            photos: vec![a, b],
            spanned: Some(spanned.clone()),
        };
        let mut state = State {
            current: Some(set.clone()),
            ..State::default()
        };
        wallpaper(dir.path(), "old", 10);
        let report = cleanup(dir.path(), &mut state, u64::MAX).unwrap();
        assert_eq!(report.removed_files, 1);
        assert!(set.on_disk());
        assert_eq!(set.layout(), Layout::Spanned(spanned));

        let live = [display("2", 3000), display("1", 2000)];
        assert!(set.fits(&live, true));
        assert!(!set.fits(&live, false));
        assert!(!set.fits(&live[..1], true));
        assert!(!set.fits(&[display("1", 2000), display("2", 1280)], true));
        let single = Set::single(wallpaper(dir.path(), "s", 1));
        assert!(single.fits(&live, false));
        assert!(!single.fits(&live, true));
    }

    #[test]
    fn reads_state_saved_by_version_0_1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let old = r#"{"current": {"id": "x", "file": "/c/x.jpg", "author": "A",
            "author_url": "", "photo_url": "", "preview_url": "", "location": null,
            "description": null, "download_location": "", "color": null},
            "next": null, "last_rotated": 7, "topic_ids": {}}"#;
        fs::write(&path, old).unwrap();
        let state = State::load(&path);
        assert_eq!(state.last_rotated, 7);
        let current = state.current.unwrap();
        assert_eq!(current.layout(), Layout::Same("/c/x.jpg".into()));
    }
}
