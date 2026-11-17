//! Helpers shared by the unit tests of the service modules.
//!
//! Pure logic with many branches. Whole journeys through `Command`s,
//! against a local stub of every photo API, are in `tests/worker.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::store::Wallpaper;

pub(super) fn screen(id: &str, width: u32, height: u32) -> Display {
    Display {
        id: id.into(),
        name: format!("Screen {id}"),
        x: 0,
        y: 0,
        width,
        height,
        scale: 1.0,
        layout_scale: None,
    }
}

/// A worker with no network: nothing here may reach a photo API.
pub(super) fn service(root: &Path) -> Service {
    let hooks = Hooks {
        apply: Box::new(|_| Ok(())),
        displays: Box::new(|| vec![screen("1", 100, 100)]),
        notify: Box::new(|_| {}),
    };
    Service::with_http(Paths::under(root), hooks, Http::default()).unwrap()
}

pub(super) fn photo(svc: &Service, id: &str, display: Option<Display>) -> Wallpaper {
    let file = svc.paths.images_dir.join(format!("{id}.jpg"));
    fs::create_dir_all(&svc.paths.images_dir).unwrap();
    fs::write(&file, b"jpeg").unwrap();
    Wallpaper {
        id: id.into(),
        file,
        author: "Ann / Lee".into(),
        author_url: "https://unsplash.com/@ann".into(),
        photo_url: "https://unsplash.com/photos/x".into(),
        preview_url: String::new(),
        location: None,
        description: None,
        download_location: String::new(),
        color: None,
        source: SourceId::Unsplash,
        display,
    }
}

pub(super) fn cached(svc: &Service, id: &str) -> Set {
    Set::single(photo(svc, id, None))
}

/// A stored photo with the given source and credit links.
pub(super) fn stored(source: SourceId, author_url: &str, photo_url: &str) -> Wallpaper {
    Wallpaper {
        id: "p1".into(),
        file: PathBuf::from("p1.jpg"),
        author: "Ann".into(),
        author_url: author_url.into(),
        photo_url: photo_url.into(),
        preview_url: String::new(),
        location: None,
        description: None,
        download_location: String::new(),
        color: None,
        source,
        display: None,
    }
}
