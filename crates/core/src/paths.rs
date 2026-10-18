use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};

/// Where the app keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub state_file: PathBuf,
    pub log_file: PathBuf,
    /// App-owned folder for downloaded wallpapers. Cleanup deletes anything in
    /// here that is not the current or prefetched wallpaper.
    pub images_dir: PathBuf,
    /// Used when the config does not name a favorites folder.
    pub default_favorites_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        if let Some(root) = std::env::var_os("WALLPAPER_ROTATOR_HOME") {
            return Ok(Self::under(Path::new(&root)));
        }
        let dirs = ProjectDirs::from("dev", "dflippojr", "Wallpaper Rotator")
            .context("could not find a home directory")?;
        let favorites = UserDirs::new()
            .and_then(|u| u.picture_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| dirs.data_dir().to_path_buf())
            .join("Wallpaper Favorites");
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            state_file: dirs.data_dir().join("state.json"),
            log_file: dirs.data_dir().join("rotator.log"),
            images_dir: dirs.cache_dir().join("images"),
            default_favorites_dir: favorites,
        })
    }

    /// Everything under one root folder, including when WALLPAPER_ROTATOR_HOME is set.
    pub fn under(root: &Path) -> Self {
        Self {
            config_file: root.join("config.toml"),
            state_file: root.join("state.json"),
            log_file: root.join("rotator.log"),
            images_dir: root.join("images"),
            default_favorites_dir: root.join("favorites"),
        }
    }
}
