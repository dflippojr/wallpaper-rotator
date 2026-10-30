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
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Header only; skip anything that isn't a readable image.
        let Ok((width, height)) = image::image_dimensions(&path) else {
            continue;
        };
        let (author, link) = credit(stem);
        out.push(Candidate {
            source: SourceId::Favorites,
            id: format!("fav-{stem}"),
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

/// The file stem a saved favorite gets, from its already safe author and id:
/// "{author} - {source}-{id}", or "{author} - {id}" for Unsplash, which kept
/// the name it had before other sources existed. [`credit`] reads it back.
pub fn stem(source: SourceId, author: &str, id: &str) -> String {
    match source {
        SourceId::Unsplash => format!("{author} - {id}"),
        other => format!("{author} - {}-{id}", other.key()),
    }
}

/// The author and a link back to the photo's page, from a saved file name.
/// A name the app didn't write, such as the user's own "Paris - Eiffel
/// tower", has no link and the whole stem as its author.
fn credit(stem: &str) -> (String, String) {
    match saved_as(stem) {
        Some((author, source, id)) => {
            let link = match source {
                SourceId::Unsplash => format!("https://unsplash.com/photos/{id}"),
                SourceId::Pexels => format!("https://www.pexels.com/photo/{id}/"),
                SourceId::Wallhaven => format!("https://wallhaven.cc/w/{id}"),
                // Picture-of-the-day ids aren't links.
                SourceId::Daily | SourceId::Favorites => String::new(),
            };
            (author.to_string(), link)
        }
        None => (stem.to_string(), String::new()),
    }
}

/// The author, source and id of a name [`stem`] wrote. Authors may contain
/// " - " and so may photo-of-the-day ids, so this tries the forms in order
/// of how strictly they match: a linked source's id has no spaces, so it
/// can only follow the last " - "; the older Unsplash form needs an
/// Unsplash-style id; photo-of-the-day ids start "bing-" or "commons-".
fn saved_as(stem: &str) -> Option<(&str, SourceId, &str)> {
    // Every " - ", overlapping ones too: the author "Ann -" is saved as
    // "Ann - - pexels-1".
    let splits = || {
        (0..stem.len())
            .filter(|&i| stem.as_bytes()[i..].starts_with(b" - "))
            .map(|i| (&stem[..i], &stem[i + 3..]))
    };
    let linked = splits().find_map(|(author, rest)| {
        let (key, id) = rest.split_once('-')?;
        let source = match key {
            "unsplash" => SourceId::Unsplash,
            "pexels" => SourceId::Pexels,
            "wallhaven" => SourceId::Wallhaven,
            _ => return None,
        };
        is_link_id(id).then_some((author, source, id))
    });
    let legacy = || {
        splits()
            .find(|(_, id)| id.len() == 11 && is_link_id(id))
            .map(|(author, id)| (author, SourceId::Unsplash, id))
    };
    let daily = || {
        splits().find_map(|(author, rest)| {
            let id = rest.strip_prefix("daily-")?;
            (id.starts_with("bing-") || id.starts_with("commons-")).then_some((
                author,
                SourceId::Daily,
                id,
            ))
        })
    };
    linked.or_else(legacy).or_else(daily)
}

/// Ids that go into a link as they are: `[A-Za-z0-9_-]+`.
fn is_link_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::tests::png;

    #[test]
    fn lists_images_and_rebuilds_credit() {
        let dir = tempfile::tempdir().unwrap();
        png(dir.path(), "Ann Lee - abc123DEF45.png", 40, 30);
        png(dir.path(), "Bo - pexels-417074.png", 40, 30);
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        fs::write(dir.path().join("broken.jpg"), "not an image").unwrap();
        let photos = batch(dir.path()).unwrap();
        assert_eq!(photos.len(), 2);
        assert_eq!(photos[0].author, "Ann Lee");
        assert_eq!(
            photos[0].photo_url,
            "https://unsplash.com/photos/abc123DEF45"
        );
        assert_eq!(photos[1].photo_url, "https://www.pexels.com/photo/417074/");
        assert_eq!((photos[0].width, photos[0].height), (40, 30));
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
        assert_eq!(credit("A - daily-bing-x"), ("A".into(), String::new()));
        assert_eq!(
            credit("A - wallhaven-d5wlmj").1,
            "https://wallhaven.cc/w/d5wlmj"
        );
    }

    #[test]
    fn own_photos_named_like_saves_get_no_link() {
        assert_eq!(
            credit("Paris - Eiffel tower"),
            ("Paris - Eiffel tower".into(), String::new())
        );
        assert_eq!(
            credit("A - daily-commons-Paris - Eiffel Tower.jpg"),
            ("A".into(), String::new())
        );
    }

    /// The class behind #61: `credit` must agree with the name `stem`
    /// writes, and nothing else. Every name built from these pieces either
    /// reads back as a save `stem` would write, with a link that is a valid
    /// URL path, or gets no link and the whole stem as its author.
    #[test]
    fn credit_links_only_names_the_app_writes() {
        let authors = [
            "Ann",
            "Ann Lee",
            "Jean - Luc",
            "Paris - pexels",
            "Ann - daily-life",
            "Ann -",
            "- Ann",
            "photo",
        ];
        let ids = [
            (SourceId::Unsplash, "abc123DEF45"),
            (SourceId::Unsplash, "I-B7DT1Sfu8"),
            (SourceId::Pexels, "417074"),
            (SourceId::Wallhaven, "d5wlmj"),
            (SourceId::Daily, "bing-20261030-Fjord"),
            (SourceId::Daily, "commons-Paris - Eiffel Tower.jpg"),
        ];
        for author in authors {
            for (source, id) in ids {
                let name = stem(source, author, id);
                assert_eq!(saved_as(&name), Some((author, source, id)), "{name}");
            }
        }

        let pieces = [
            "Paris",
            "Eiffel tower",
            "abc123DEF45",
            "pexels-1",
            "pexels-my trip",
            "wallhaven-a b",
            "daily-life",
            "daily-bing-x",
            "unsplash-",
            "daily-x",
            "x.jpg",
            "",
        ];
        for a in pieces {
            for b in pieces {
                for c in ["", " - Eiffel", " - abc123DEF45", " - pexels-2"] {
                    let name = format!("{a} - {b}{c}");
                    let (author, link) = credit(&name);
                    match saved_as(&name) {
                        Some((who, source, id)) => {
                            assert_eq!(stem(source, who, id), name);
                            assert_eq!(author, who);
                        }
                        None => assert_eq!((author.as_str(), link.as_str()), (name.as_str(), "")),
                    }
                    assert!(
                        !link.contains(char::is_whitespace),
                        "{name} gave the link {link}"
                    );
                }
            }
        }
    }

    #[test]
    fn app_saved_names_keep_their_links() {
        assert_eq!(
            credit("Ann - abc123DEF45"),
            (
                "Ann".into(),
                "https://unsplash.com/photos/abc123DEF45".into()
            )
        );
        assert_eq!(
            credit("Bo - pexels-417074"),
            ("Bo".into(), "https://www.pexels.com/photo/417074/".into())
        );
        assert_eq!(
            credit("C - wallhaven-d5wlmj"),
            ("C".into(), "https://wallhaven.cc/w/d5wlmj".into())
        );
        // Authors that look like part of a save's name (#65 review).
        assert_eq!(
            credit("Ann - daily-life - pexels-417074"),
            (
                "Ann - daily-life".into(),
                "https://www.pexels.com/photo/417074/".into()
            )
        );
        assert_eq!(
            credit("Ann - - pexels-417074"),
            (
                "Ann -".into(),
                "https://www.pexels.com/photo/417074/".into()
            )
        );
    }
}
