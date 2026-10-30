//! The favorites folder as a source: no network, no key, never
//! rate-limited. Photos are cropped into the cache; the originals are never
//! moved, changed or deleted.

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};

use super::{Candidate, Image, SourceId};

const EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// Every image in `dir`, with credit rebuilt from the file name where the
/// app saved it ("{author} - {source}-{id}.jpg", or "{author} - {id}.jpg"
/// from Unsplash before other sources existed).
pub fn batch(dir: &Path) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("the favorites folder is empty: save some favorites first")
        }
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !path.is_file() || !EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let (Some(name), Some(stem)) = (
            path.file_name().and_then(|s| s.to_str()),
            path.file_stem().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        // Header only; skip anything that isn't a readable image.
        let Ok((width, height)) = image::image_dimensions(&path) else {
            continue;
        };
        let (author, link) = credit(stem);
        out.push(Candidate {
            source: SourceId::Favorites,
            // The whole file name: "a.jpg" and "a.png" are two photos.
            id: format!("fav-{name}"),
            width,
            height,
            author,
            author_url: link.clone(),
            photo_url: link,
            preview_url: String::new(),
            location: None,
            description: None,
            color: None,
            image: Image::File { path },
            track_url: None,
        });
    }
    if out.is_empty() {
        bail!("the favorites folder is empty: save some favorites first");
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// The author and a link back to the photo's page, from a saved file name.
fn credit(stem: &str) -> (String, String) {
    let Some((author, id)) = stem.rsplit_once(" - ") else {
        return (stem.to_string(), String::new());
    };
    let link = match id.split_once('-') {
        Some(("pexels", id)) => format!("https://www.pexels.com/photo/{id}/"),
        Some(("wallhaven", id)) => format!("https://wallhaven.cc/w/{id}"),
        // Picture-of-the-day ids aren't links.
        Some(("daily", _)) => String::new(),
        // Older favorites were all Unsplash, named "{author} - {id}".
        Some(("unsplash", id)) => format!("https://unsplash.com/photos/{id}"),
        _ => format!("https://unsplash.com/photos/{id}"),
    };
    (author.to_string(), link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::tests::png;

    #[test]
    fn lists_images_and_rebuilds_credit() {
        let dir = tempfile::tempdir().unwrap();
        png(dir.path(), "Ann Lee - abc123.png", 40, 30);
        png(dir.path(), "Bo - pexels-417074.png", 40, 30);
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        fs::write(dir.path().join("broken.jpg"), "not an image").unwrap();
        let photos = batch(dir.path()).unwrap();
        assert_eq!(photos.len(), 2);
        assert_eq!(photos[0].author, "Ann Lee");
        assert_eq!(photos[0].photo_url, "https://unsplash.com/photos/abc123");
        assert_eq!(photos[1].photo_url, "https://www.pexels.com/photo/417074/");
        assert_eq!((photos[0].width, photos[0].height), (40, 30));
    }

    #[test]
    fn every_file_gets_its_own_id() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["sunset.jpg", "sunset.png", "a (1).png", "a [1].png"] {
            png(dir.path(), name, 40, 30);
        }
        let photos = batch(dir.path()).unwrap();
        let ids: Vec<&str> = photos.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "fav-a (1).png",
                "fav-a [1].png",
                "fav-sunset.jpg",
                "fav-sunset.png"
            ]
        );
        assert_eq!(photos[2].author, "sunset");
    }

    #[test]
    fn empty_or_missing_folders_say_so() {
        let dir = tempfile::tempdir().unwrap();
        assert!(batch(dir.path())
            .unwrap_err()
            .to_string()
            .contains("save some favorites"));
        assert!(batch(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn names_without_a_credit() {
        assert_eq!(credit("sunset"), ("sunset".into(), String::new()));
        assert_eq!(credit("A - daily-bing-x").1, "");
        assert_eq!(
            credit("A - wallhaven-d5wlmj").1,
            "https://wallhaven.cc/w/d5wlmj"
        );
    }
}
