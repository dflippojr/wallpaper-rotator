//! Captures every screen background before a desktop-changing test starts.
use anyhow::{Context, Result};

pub fn capture<S, U, O>(
    screens: impl IntoIterator<Item = S>,
    mut read: impl FnMut(&S) -> (Option<U>, Option<O>),
) -> Result<Vec<(S, U, O)>> {
    screens
        .into_iter()
        .enumerate()
        .map(|(index, screen)| {
            let (url, options) = read(&screen);
            let number = index + 1;
            let url =
                url.with_context(|| format!("cannot save wallpaper URL for screen {number}"))?;
            let options = options
                .with_context(|| format!("cannot save wallpaper options for screen {number}"))?;
            Ok((screen, url, options))
        })
        .collect()
}
