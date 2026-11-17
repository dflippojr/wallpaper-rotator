//! Cache file names: a readable, portable stem for each photo ID that
//! never collides with another ID's.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::now;

/// How much of `safe_name` a cache file name keeps. Even in 4-byte characters
/// this leaves the name, with its hash and size, under the usual 255 bytes.
const CACHE_NAME_CHARS: usize = 40;
/// Keeps file names portable: letters, digits, `-`, `_`, spaces and dots.
pub(super) fn safe_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "photo".into()
    } else {
        trimmed.chars().take(80).collect()
    }
}

/// The start of a cache file name, unique per ID: `safe_name` alone maps
/// different IDs to one name, so 128 bits of a hash of the whole ID follow it.
/// A cryptographic hash, because shorter or simpler ones have file names that
/// collide, and a collision puts one photo in another's file.
pub(super) fn cache_stem(id: &str) -> String {
    let readable: String = safe_name(id).chars().take(CACHE_NAME_CHARS).collect();
    let hash: String = Sha256::digest(id)[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{readable}-{hash}")
}

/// A new file for a stitched image, named for its first photo and the time,
/// with a number added while that name is taken. An existing file is never
/// picked: GNOME may be showing it, and stitching over it changes the screen.
pub(super) fn span_path(images_dir: &Path, first: &str) -> PathBuf {
    let stem = format!("span-{}-{}", cache_stem(first), now());
    let mut dest = images_dir.join(format!("{stem}.jpg"));
    let mut n = 1;
    while dest.exists() {
        dest = images_dir.join(format!("{stem}-{n}.jpg"));
        n += 1;
    }
    dest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("Ann / Lee"), "Ann _ Lee");
        assert_eq!(safe_name("../.."), "_");
        assert_eq!(safe_name("..."), "photo");
    }

    #[test]
    fn cache_stems_stay_readable_and_portable() {
        assert_eq!(
            cache_stem("fav-trip (1).png"),
            "fav-trip _1_.png-82654e14180148620f71ce0453b44ab3"
        );
        let long = cache_stem(&"é/".repeat(200));
        assert_eq!(long.chars().count(), CACHE_NAME_CHARS + 33);
        assert!(long
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.')));
    }

    #[test]
    fn ids_differing_in_replaced_characters_or_after_80_get_their_own_cache_names() {
        assert_ne!(cache_stem("fav-trip (1)"), cache_stem("fav-trip [1]"));
        let long = "x".repeat(80);
        assert_ne!(
            cache_stem(&format!("{long}a")),
            cache_stem(&format!("{long}b"))
        );
    }

    /// These two share a 32-bit FNV-1a hash, so a short hash can't be enough.
    #[test]
    fn ids_with_the_same_32_bit_hash_get_their_own_cache_names() {
        let long = "a".repeat(80);
        assert_ne!(
            cache_stem(&format!("fav-{long}38x03df0.png")),
            cache_stem(&format!("fav-{long}rc6mxz06.png"))
        );
    }

    /// The class of bug behind #59: `safe_name` is lossy, so it must never be
    /// all that tells two IDs apart. Every string over the characters it
    /// conflates or trims, alone and past the 80 characters it keeps, must
    /// still give its own cache name.
    #[test]
    fn every_id_safe_name_conflates_gets_its_own_cache_name() {
        const LOSSY: [char; 6] = ['(', '[', '_', '/', '.', ' '];
        let mut suffixes = vec![String::new()];
        let mut last = suffixes.clone();
        for _ in 0..5 {
            last = last
                .iter()
                .flat_map(|s| LOSSY.iter().map(move |c| format!("{s}{c}")))
                .collect();
            suffixes.extend(last.iter().cloned());
        }
        for prefix in ["fav-", "a".repeat(80).as_str()] {
            let mut seen = HashSet::new();
            for suffix in &suffixes {
                let id = format!("{prefix}{suffix}");
                assert!(seen.insert(cache_stem(&id)), "{id:?} shares a cache name");
            }
        }
    }
}
