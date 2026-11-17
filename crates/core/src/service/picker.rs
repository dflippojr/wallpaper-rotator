//! Choosing photos: source order, each source's queue and resting, and the
//! random picks that mix the sources.

use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};

use super::names::cache_stem;
use super::rotation::wallpaper_from;
use super::Service;
use crate::source::{daily, favorites, pexels, unsplash, wallhaven, Candidate, SourceId};
use crate::store::{Set, Wallpaper};
use crate::wallpaper::Display;
use crate::{net, now, source};

/// How long a source rests after a rate limit, or after another error.
const RATE_LIMIT_REST_SECS: u64 = 20 * 60;
const ERROR_REST_SECS: u64 = 10 * 60;
/// Ask a source for a fresh batch when its best queued photo covers less
/// of the display than this.
const GOOD_ENOUGH: f64 = 0.75;
/// Photos to try from one source before moving on to the next.
const TRIES_PER_SOURCE: usize = 3;
/// Queued candidates kept per source.
const QUEUE_LIMIT: usize = 120;
/// The IDs of every photo in `sets`.
pub(super) fn ids<'a>(sets: impl IntoIterator<Item = &'a Set>) -> HashSet<String> {
    sets.into_iter()
        .flat_map(|s| &s.photos)
        .map(|w| w.id.clone())
        .collect()
}
/// How much of a display a photo covers without upscaling, up to 1.0.
/// Unknown sizes count as half.
fn coverage(photo: (u32, u32), size: (u32, u32)) -> f64 {
    if photo.0 == 0 || photo.1 == 0 {
        return 0.5;
    }
    let w = f64::from(photo.0) / f64::from(size.0.max(1));
    let h = f64::from(photo.1) / f64::from(size.1.max(1));
    w.min(h).min(1.0)
}
impl Service {
    /// Whether a source can be asked at all (it has the key it needs).
    fn usable(&self, source: SourceId) -> bool {
        match source {
            SourceId::Unsplash => !self.keys().unsplash.is_empty(),
            SourceId::Pexels => !self.keys().pexels.is_empty(),
            SourceId::Wallhaven | SourceId::Daily | SourceId::Favorites => true,
        }
    }

    pub(super) fn can_fetch(&self) -> bool {
        self.config.sources.iter().any(|&s| self.usable(s))
    }

    fn random(&mut self) -> u64 {
        // xorshift64: plenty for shuffling photo sources.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.random() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    /// Ticked, usable sources not resting, in random order, then the
    /// favorites folder as a last resort even when it isn't ticked.
    fn source_order(&mut self) -> Vec<SourceId> {
        let now = now();
        let mut order: Vec<SourceId> = self
            .config
            .sources
            .clone()
            .into_iter()
            .filter(|&s| self.usable(s))
            .filter(|s| self.resting.get(s).is_none_or(|&until| now >= until))
            .collect();
        order.dedup();
        self.shuffle(&mut order);
        if !order.contains(&SourceId::Favorites) {
            order.push(SourceId::Favorites);
        }
        order
    }

    fn rest(&mut self, source: SourceId, e: &anyhow::Error) {
        let secs = if net::is_rate_limited(e) {
            RATE_LIMIT_REST_SECS
        } else {
            ERROR_REST_SECS
        };
        self.resting.insert(source, now() + secs);
    }

    /// One photo at `size`, from the first source that can give one.
    pub(super) fn photo_for(
        &mut self,
        size: (u32, u32),
        display: Option<Display>,
        used: &mut HashSet<String>,
        notes: &mut Vec<String>,
    ) -> Result<Wallpaper> {
        let mut first_error = None;
        for source in self.source_order() {
            // Favorites fill in quietly when they weren't asked for.
            let ticked = self.config.sources.contains(&source);
            match self.photo_from(source, size, &display, used) {
                Ok(photo) => return Ok(photo),
                Err(e) => {
                    self.log(&format!("{} skipped: {e:#}", source.name()));
                    if ticked {
                        self.rest(source, &e);
                        let note = if net::is_rate_limited(&e) {
                            format!("{} is rate-limited", source.name())
                        } else {
                            format!("{} failed", source.name())
                        };
                        if !notes.contains(&note) {
                            notes.push(note);
                        }
                        first_error.get_or_insert(e);
                    }
                }
            }
        }
        Err(first_error.unwrap_or_else(|| {
            let wait = self
                .resting
                .values()
                .min()
                .map(|until| until.saturating_sub(now()).div_ceil(60))
                .unwrap_or(0);
            anyhow!("every photo source is resting after an error; trying again in {wait} min")
        }))
    }

    /// The best-fitting queued photo from `source` (refilling the queue
    /// when nothing fits well), downloaded at `size`.
    fn photo_from(
        &mut self,
        source: SourceId,
        (width, height): (u32, u32),
        display: &Option<Display>,
        used: &mut HashSet<String>,
    ) -> Result<Wallpaper> {
        let mut refilled = false;
        let mut tries = 0;
        loop {
            let best = self.best_queued(source, (width, height), used);
            if !refilled && best.is_none_or(|(_, cover)| cover < GOOD_ENOUGH) {
                self.refill(source, (width, height))?;
                refilled = true;
                continue;
            }
            // A small pool (a few favorites, say) may have nothing unused
            // left: repeat one rather than fail, preferring one that isn't
            // on screen right now.
            let best = best.or_else(|| {
                let on_screen = ids(&self.state.current);
                self.best_queued(source, (width, height), &on_screen)
                    .or_else(|| self.best_queued(source, (width, height), &HashSet::new()))
            });
            let Some((index, _)) = best else {
                bail!("{} had no photos", source.name());
            };
            let candidate = self
                .state
                .queue
                .get_mut(&source)
                .map(|q| q.remove(index))
                .context("queue changed")?;
            used.insert(candidate.id.clone());
            let file = self.paths.images_dir.join(format!(
                "{}-{width}x{height}.jpg",
                cache_stem(&candidate.id)
            ));
            match source::fetch_image(&self.http, &candidate, width, height, &file) {
                Ok(()) => return Ok(wallpaper_from(candidate, file, display.clone())),
                Err(e) => {
                    tries += 1;
                    self.log(&format!("skipped {}: {e:#}", candidate.id));
                    if net::is_rate_limited(&e) || tries >= TRIES_PER_SOURCE {
                        return Err(e);
                    }
                }
            }
        }
    }

    fn best_queued(
        &self,
        source: SourceId,
        size: (u32, u32),
        used: &HashSet<String>,
    ) -> Option<(usize, f64)> {
        let queue = self.state.queue.get(&source)?;
        queue
            .iter()
            .enumerate()
            .filter(|(_, c)| !used.contains(&c.id))
            .map(|(i, c)| (i, coverage((c.width, c.height), size)))
            .fold(None, |best, (i, cover)| match best {
                Some((_, b)) if b >= cover => best,
                _ => Some((i, cover)),
            })
    }

    /// Asks `source` for a new batch and queues the photos it didn't have.
    fn refill(&mut self, source: SourceId, (width, height): (u32, u32)) -> Result<()> {
        let query = self.config.query.clone();
        let batch: Vec<Candidate> = match source {
            SourceId::Unsplash => {
                let key = self.keys().unsplash;
                let topics = self.topic_ids(&key)?;
                unsplash::batch(
                    &self.http,
                    &key,
                    &query,
                    &topics,
                    &self.config.collections,
                    &self.config.username,
                )?
            }
            SourceId::Pexels => {
                let key = self.keys().pexels;
                let random = self.random();
                let (batch, pages) =
                    pexels::batch(&self.http, &key, &query, self.state.pexels_pages, random)?;
                self.state.pexels_pages = Some(pages);
                batch
            }
            SourceId::Wallhaven => wallhaven::batch(&self.http, &query, width, height)?,
            SourceId::Daily => daily::batch(&self.http, now())?,
            SourceId::Favorites => favorites::batch(&self.favorites_dir())?,
        };
        let mut fresh: Vec<Candidate> = {
            let queued: HashSet<&str> = self
                .state
                .queue
                .get(&source)
                .into_iter()
                .flatten()
                .map(|c| c.id.as_str())
                .collect();
            batch
                .into_iter()
                .filter(|c| !queued.contains(c.id.as_str()))
                .collect()
        };
        self.shuffle(&mut fresh);
        let queue = self.state.queue.entry(source).or_default();
        queue.extend(fresh);
        let excess = queue.len().saturating_sub(QUEUE_LIMIT);
        queue.drain(..excess);
        self.save_state();
        Ok(())
    }

    fn topic_ids(&mut self, key: &str) -> Result<Vec<String>> {
        let c = &self.config;
        if unsplash::selection(&c.query, &c.collections, &c.username) != unsplash::Selection::Topics
        {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for topic in self.config.topics.clone() {
            let id = match self.state.topic_ids.get(&topic) {
                Some(id) => id.clone(),
                None => {
                    let id = unsplash::topic_id(&self.http, key, &topic)?;
                    self.state.topic_ids.insert(topic, id.clone());
                    id
                }
            };
            ids.push(id);
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::test_support::{photo, screen, service};
    use crate::source::tests::png;
    use crate::source::Image;

    #[test]
    fn small_unsplash_pool_prefers_photos_outside_the_current_set() {
        for each in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut svc = service(dir.path());
            let screens = [screen("1", 100, 100), screen("2", 100, 100)];
            let current = photo(&svc, "current", each.then(|| screens[0].clone()));
            svc.state.current = Some(Set::single(current));
            let image = png(dir.path(), "image.png", 100, 100);
            let candidate = |id: &str| Candidate {
                source: SourceId::Unsplash,
                id: id.into(),
                width: 100,
                height: 100,
                author: String::new(),
                author_url: String::new(),
                photo_url: String::new(),
                preview_url: String::new(),
                location: None,
                description: None,
                color: None,
                image: Image::File {
                    path: image.clone(),
                },
                track_url: None,
            };
            svc.config.access_key = "unused-test-key".into();
            svc.state.queue.insert(
                SourceId::Unsplash,
                vec![candidate("current"), candidate("other"), candidate("third")],
            );
            let (next, _) = svc.fetch(&screens, each).unwrap();
            assert!(next.photos.iter().all(|w| w.id != "current"));
            let ids: HashSet<_> = next.photos.iter().map(|w| &w.id).collect();
            assert_eq!(ids.len(), next.photos.len());
        }
    }

    #[test]
    fn coverage_prefers_photos_that_fill_the_display() {
        let monitor = (3840, 2160);
        assert_eq!(coverage((6000, 4000), monitor), 1.0);
        assert!((coverage((1920, 1080), monitor) - 0.5).abs() < 1e-9);
        assert_eq!(coverage((0, 0), monitor), 0.5);
    }
}
