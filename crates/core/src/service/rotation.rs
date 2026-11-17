//! Rotation and prefetch: fetching a set of photos, placing them on the
//! displays, and stitching one image across them on GNOME.

use std::path::PathBuf;

use anyhow::{bail, Result};

use super::names::span_path;
use super::picker::ids;
use super::view::fallback_notice;
use super::{Service, FALLBACK_SIZE, RETRY_AFTER_SECS};
use crate::source::{unsplash, Candidate, SourceId};
use crate::store::{Set, Wallpaper};
use crate::wallpaper::{Display, Layout};
use crate::{now, store, wallpaper};

pub(super) fn wallpaper_from(c: Candidate, file: PathBuf, display: Option<Display>) -> Wallpaper {
    Wallpaper {
        id: c.id,
        file,
        author: c.author,
        author_url: c.author_url,
        photo_url: c.photo_url,
        preview_url: c.preview_url,
        location: c.location,
        description: c.description,
        download_location: c.track_url.unwrap_or_default(),
        color: c.color,
        source: c.source,
        display,
    }
}
/// The size that covers every display: the widest width and tallest height.
pub(super) fn largest(displays: &[Display]) -> Option<(u32, u32)> {
    store::covering(displays)
}
impl Service {
    pub(super) fn rotate_and_prefetch(&mut self, only: Option<usize>) {
        self.busy = true;
        self.publish();
        let result = match only {
            Some(index) => self.rotate_one(index),
            None => self.rotate(),
        };
        self.busy = false;
        match result {
            Ok(()) => self.error = None,
            Err(e) => {
                self.last_failed_fetch = now();
                self.fail(e);
            }
        }
        self.publish();
        if self.error.is_none() {
            self.prefetch_if_needed();
            self.publish();
        }
    }

    fn rotate(&mut self) -> Result<()> {
        if !self.can_fetch() {
            bail!("tick a photo source in Settings (and add its key if it needs one)");
        }
        let displays = self.live_displays();
        let each = self.each(&displays);
        let next = match self.state.next.take() {
            Some(set) if set.on_disk() && set.fits(&displays, each) => set,
            _ => {
                let (set, notes) = self.fetch(&displays, each)?;
                if !notes.is_empty() {
                    self.notice = Some(fallback_notice(&notes, &set));
                }
                set
            }
        };
        if let Err(e) = (self.hooks.apply)(&next.layout()) {
            self.state.next = Some(next);
            return Err(e);
        }
        for photo in &next.photos {
            self.used(photo);
        }
        self.state.current = Some(next);
        self.state.last_rotated = now();
        self.state.keep_cache = false;
        self.save_state();
        self.cleanup();
        Ok(())
    }

    /// Reports a photo that just went on screen (Unsplash's terms) and logs it.
    fn used(&self, photo: &Wallpaper) {
        if photo.source == SourceId::Unsplash && !photo.download_location.is_empty() {
            let key = self.keys().unsplash;
            if let Err(e) = unsplash::track(&self.http, &key, &photo.download_location) {
                self.log(&format!("download tracking failed: {e:#}"));
            }
        }
        let place = photo
            .display
            .as_ref()
            .map(|d| format!(" on {}", d.name))
            .unwrap_or_default();
        self.log(&format!(
            "wallpaper set: {} by {} ({}){place}",
            photo.id,
            photo.author,
            photo.source.name()
        ));
    }

    /// Replaces only the photo at `index` (one display's), keeping the
    /// others and the schedule. Falls back to rotating everything when the
    /// photos aren't per display or that display is gone.
    fn rotate_one(&mut self, index: usize) -> Result<()> {
        let displays = self.live_displays();
        let target = self
            .state
            .current
            .as_ref()
            .and_then(|s| s.photos.get(index))
            .and_then(|w| w.display.as_ref())
            .and_then(|d| displays.iter().find(|live| live.id == d.id))
            .cloned();
        let (Some(target), Some(current)) = (target, self.state.current.clone()) else {
            return self.rotate();
        };
        // This display's prefetched photo, if it's ready: instant. Its slot
        // in the prefetched set is filled again in the background.
        let ready = self.state.next.as_mut().and_then(|next| {
            let i = next
                .photos
                .iter()
                .position(|w| w.display.as_ref() == Some(&target) && w.file.exists())?;
            next.spanned = None;
            Some(next.photos.remove(i))
        });
        if self
            .state
            .next
            .as_ref()
            .is_some_and(|n| n.photos.is_empty())
        {
            self.state.next = None;
        }
        let photo = match ready {
            Some(photo) => photo,
            None => {
                let mut used = ids(self.state.current.iter().chain(&self.state.next));
                let mut notes = Vec::new();
                let size = (target.width, target.height);
                let photo = self.photo_for(size, Some(target.clone()), &mut used, &mut notes)?;
                if !notes.is_empty() {
                    self.notice = Some(fallback_notice(&notes, &Set::single(photo.clone())));
                }
                photo
            }
        };
        let mut set = current;
        set.photos[index] = photo;
        let set = self.restitch(set, &displays)?;
        (self.hooks.apply)(&set.layout())?;
        self.used(&set.photos[index]);
        self.state.current = Some(set);
        self.save_state();
        self.cleanup();
        Ok(())
    }

    /// Fills the displays a prefetched set is missing (after one display got
    /// its photo early), instead of fetching a whole new set.
    fn top_up_next(&mut self, displays: &[Display]) -> Result<()> {
        let partial = self.state.next.as_ref().is_some_and(|next| {
            !next.photos.is_empty()
                && next.photos.len() < displays.len()
                && next.on_disk()
                && next
                    .photos
                    .iter()
                    .all(|w| w.display.as_ref().is_some_and(|d| displays.contains(d)))
        });
        if !partial {
            return Ok(());
        }
        let Some(mut set) = self.state.next.take() else {
            return Ok(());
        };
        let mut used = ids(self.state.current.iter().chain([&set]));
        let mut notes = Vec::new();
        for display in displays {
            if set
                .photos
                .iter()
                .any(|w| w.display.as_ref() == Some(display))
            {
                continue;
            }
            let size = (display.width, display.height);
            match self.photo_for(size, Some(display.clone()), &mut used, &mut notes) {
                Ok(photo) => set.photos.push(photo),
                Err(e) => {
                    self.state.next = Some(set);
                    return Err(e);
                }
            }
        }
        // Display order, as the popover lists them.
        set.photos.sort_by_key(|w| {
            displays
                .iter()
                .position(|d| w.display.as_ref() == Some(d))
                .unwrap_or(usize::MAX)
        });
        self.state.next = Some(self.restitch(set, displays)?);
        self.save_state();
        self.cleanup();
        Ok(())
    }

    pub(super) fn prefetch_if_needed(&mut self) {
        if !self.can_fetch() || now() < self.last_failed_fetch + RETRY_AFTER_SECS {
            return;
        }
        let displays = self.live_displays();
        // Compared here, not when displays change: `Next` can update the
        // display list before a tick notices the change. A pause for other
        // displays is over for good, even if that layout comes back.
        match &self.prefetch_over_cap {
            Some(paused) if *paused == displays => return,
            Some(_) => self.prefetch_over_cap = None,
            None => {}
        }
        let each = self.each(&displays);
        if each {
            if let Err(e) = self.top_up_next(&displays) {
                self.last_failed_fetch = now();
                self.log(&format!("prefetch failed: {e:#}"));
                return;
            }
        }
        let ready = self
            .state
            .next
            .as_ref()
            .is_some_and(|s| s.on_disk() && s.fits(&displays, each));
        if ready {
            return;
        }
        match self.fetch(&displays, each) {
            Ok((set, notes)) => {
                if !notes.is_empty() {
                    self.log(&format!("prefetch: {}", notes.join(", ")));
                }
                self.state.next = Some(set);
                self.save_state();
                self.cleanup();
            }
            Err(e) => {
                self.last_failed_fetch = now();
                self.log(&format!("prefetch failed: {e:#}"));
            }
        }
    }

    /// Picks and downloads photos for these displays: one for all of them,
    /// or one per display when `each` is set. Also returns notes on sources
    /// that failed and were skipped.
    pub(super) fn fetch(&mut self, displays: &[Display], each: bool) -> Result<(Set, Vec<String>)> {
        let shared = largest(displays).unwrap_or(FALLBACK_SIZE);
        let targets: Vec<Option<Display>> = if each {
            displays.iter().cloned().map(Some).collect()
        } else {
            vec![None]
        };
        let sizes: Vec<(u32, u32)> = targets
            .iter()
            .map(|t| t.as_ref().map_or(shared, |d| (d.width, d.height)))
            .collect();
        // Bigger displays choose first.
        let mut order: Vec<usize> = (0..targets.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(u64::from(sizes[i].0) * u64::from(sizes[i].1)));

        // Don't show what's already on screen again.
        let mut used = ids(&self.state.current);
        let mut notes = Vec::new();
        let mut photos: Vec<Option<Wallpaper>> = vec![None; targets.len()];
        for i in order {
            let photo = self.photo_for(sizes[i], targets[i].clone(), &mut used, &mut notes)?;
            photos[i] = Some(photo);
        }
        let mut set = Set {
            photos: photos.into_iter().flatten().collect(),
            spanned: None,
        };
        if each {
            set = self.restitch(set, displays)?;
        }
        Ok((set, notes))
    }

    /// GNOME: stitches a set's photos into one image laid out like
    /// `displays`. A display without its own photo borrows one in turn.
    pub(super) fn stitch(&self, mut set: Set, displays: &[Display]) -> Result<Set> {
        let layout = Layout::Each(
            set.photos
                .iter()
                .map(|w| {
                    let id = w.display.as_ref().map(|d| d.id.clone());
                    (id.unwrap_or_default(), w.file.clone())
                })
                .collect(),
        );
        let parts: Vec<(Display, PathBuf)> = displays
            .iter()
            .enumerate()
            .filter_map(|(i, d)| Some((d.clone(), layout.file_for(&d.id, i)?.to_path_buf())))
            .collect();
        // A new name each time, so GNOME notices the change.
        let first = set.photos.first().map_or("photo", |w| w.id.as_str());
        let dest = span_path(&self.paths.images_dir, first);
        wallpaper::compose(&parts, &dest)?;
        set.spanned = Some(dest);
        Ok(set)
    }

    /// Drops a changed set's stitched image and, on GNOME, stitches it again.
    fn restitch(&self, mut set: Set, displays: &[Display]) -> Result<Set> {
        set.spanned = None;
        if wallpaper::SPANS_ONE_IMAGE {
            set = self.stitch(set, displays)?;
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::test_support::{photo, screen, service};
    use crate::source::tests::png;

    /// The prefetch can stitch a set in the same second as the one on screen,
    /// with the same first photo. Each stitched image needs its own file, or
    /// the prefetch overwrites the file that is showing. Only GNOME stitches
    /// (`wallpaper::compose` fails elsewhere), so the test runs on Linux only.
    #[cfg(target_os = "linux")]
    #[test]
    fn stitched_images_in_the_same_second_get_their_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let left = Display {
            x: 0,
            ..screen("1", 160, 90)
        };
        let right = Display {
            x: 160,
            ..screen("2", 160, 90)
        };
        let mut set = Set {
            photos: vec![
                photo(&svc, "same", Some(left.clone())),
                photo(&svc, "other", Some(right.clone())),
            ],
            spanned: None,
        };
        for photo in &mut set.photos {
            photo.file = png(&svc.paths.images_dir, &format!("{}.png", photo.id), 160, 90);
        }
        let displays = [left, right];

        // Both stitches must fall in one second, and the clock can't be held
        // still here, so retry until it reads the same before and after both.
        for _ in 0..100 {
            let before = now();
            let first = svc.stitch(set.clone(), &displays).unwrap().spanned.unwrap();
            let second = svc.stitch(set.clone(), &displays).unwrap().spanned.unwrap();
            if now() == before {
                assert_ne!(first, second, "two stitched sets share one file");
                assert!(first.exists() && second.exists());
                return;
            }
        }
        panic!("the clock moved on every attempt, so no two stitches shared a second");
    }
}
