//! Core of Wallpaper Rotator: fetch a photo from Unsplash, set it as the
//! desktop wallpaper, and keep the image cache bounded.
//!
//! The cache only ever holds the wallpaper in use and one prefetched photo.
//! Everything else in the app-owned images folder is deleted after each
//! rotation, so disk use stays flat no matter how long the app runs.

pub mod config;
pub mod log;
pub mod net;
pub mod paths;
pub mod probe;
pub mod service;
pub mod source;
pub mod store;
pub mod wallpaper;

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
