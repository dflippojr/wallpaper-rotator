//! Copying photos to the favorites folder, and where that folder is.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::names::safe_name;
use super::Service;
use crate::config::{Config, Unparseable};
use crate::paths::Paths;
use crate::source::{favorites, SourceId};
use crate::store::Wallpaper;

/// Where "Save to favorites" copies photos: the folder the config names, or
/// the default one.
fn favorites_dir_in(config: &Config, paths: &Paths) -> PathBuf {
    config
        .favorites_dir
        .clone()
        .unwrap_or_else(|| paths.default_favorites_dir.clone())
}

/// The favorites folder, for the tray app to open in the file manager. It
/// reads the folder from the saved config itself, so a caller can't name any
/// other folder. Like saving a favorite, it creates the folder if it doesn't
/// exist yet. A config file that won't parse means the defaults, as it does
/// for the worker.
pub fn favorites_folder(paths: &Paths) -> Result<PathBuf> {
    let config = match Config::load_or_create(&paths.config_file) {
        Ok(config) => config,
        Err(e) if e.is::<Unparseable>() => Config::default(),
        Err(e) => return Err(e),
    };
    let dir = favorites_dir_in(&config, paths);
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(dir)
}
impl Service {
    pub(super) fn favorites_dir(&self) -> PathBuf {
        favorites_dir_in(&self.config, &self.paths)
    }

    /// Copies one current photo (or all of them) to the favorites folder and
    /// says where they went.
    pub(super) fn favorite(&self, index: Option<usize>) -> Result<String> {
        let set = self
            .state
            .current
            .as_ref()
            .context("no wallpaper to save yet")?;
        let photos: Vec<&Wallpaper> = match index {
            Some(i) => vec![set.photos.get(i).context("that wallpaper has changed")?],
            None => set.photos.iter().collect(),
        };
        let photos: Vec<&Wallpaper> = photos
            .into_iter()
            .filter(|w| w.source != SourceId::Favorites)
            .collect();
        if photos.is_empty() {
            return Ok("Already in your favorites".into());
        }
        let dir = self.favorites_dir();
        fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let mut saved = Vec::new();
        for photo in photos {
            let stem = favorites::stem(
                photo.source,
                &safe_name(&photo.author),
                &safe_name(&photo.id),
            );
            let dest = dir.join(format!("{stem}.jpg"));
            if !dest.exists() {
                fs::copy(&photo.file, &dest).context("could not copy the wallpaper")?;
            }
            if !saved.contains(&dest) {
                saved.push(dest);
            }
        }
        Ok(match saved.as_slice() {
            [one] => format!("Saved to {}", one.display()),
            many => format!("Saved {} photos to {}", many.len(), dir.display()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::test_support::service;

    /// Saves `favorites_dir` (or none) as the only change to the defaults.
    fn save_favorites_dir(paths: &Paths, dir: Option<PathBuf>) {
        let config = Config {
            favorites_dir: dir,
            ..Config::default()
        };
        config.save(&paths.config_file).unwrap();
    }

    #[test]
    fn favorites_folder_is_the_configured_folder_created_on_demand() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        let custom = root.path().join("Pictures").join("My Favorites");
        save_favorites_dir(&paths, Some(custom.clone()));
        assert!(!custom.exists());

        assert_eq!(favorites_folder(&paths).unwrap(), custom);
        assert!(custom.is_dir(), "the folder is created before it is opened");
        assert!(!paths.default_favorites_dir.exists(), "not the default");
        // Opening it again is fine and keeps what's inside.
        fs::write(custom.join("a.jpg"), b"jpeg").unwrap();
        assert_eq!(favorites_folder(&paths).unwrap(), custom);
        assert!(custom.join("a.jpg").is_file());
    }

    #[test]
    fn favorites_folder_is_where_save_to_favorites_puts_photos() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        save_favorites_dir(&paths, Some(root.path().join("saved")));
        let svc = service(root.path());
        assert_eq!(
            favorites_folder(&paths).unwrap().display().to_string(),
            svc.snapshot().favorites_dir
        );
    }

    #[test]
    fn favorites_folder_without_a_configured_folder_creates_the_default() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        save_favorites_dir(&paths, None);
        assert!(!paths.default_favorites_dir.exists());
        assert_eq!(
            favorites_folder(&paths).unwrap(),
            paths.default_favorites_dir
        );
        assert!(paths.default_favorites_dir.is_dir());
    }

    #[test]
    fn favorites_folder_uses_the_default_while_the_config_will_not_parse() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        fs::write(&paths.config_file, "favorites_dir = [broken").unwrap();
        assert_eq!(
            favorites_folder(&paths).unwrap(),
            paths.default_favorites_dir
        );
        assert_eq!(
            fs::read_to_string(&paths.config_file).unwrap(),
            "favorites_dir = [broken",
            "the broken file is left for the user to fix"
        );
    }

    #[test]
    fn favorites_folder_says_why_it_cannot_be_created() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        // A file where the folder should be: create_dir_all fails.
        let blocked = root.path().join("not-a-folder");
        fs::write(&blocked, b"file").unwrap();
        save_favorites_dir(&paths, Some(blocked.clone()));
        let message = format!("{:#}", favorites_folder(&paths).unwrap_err());
        assert!(
            message.starts_with(&format!("could not create {}", blocked.display())),
            "{message}"
        );
    }
}
