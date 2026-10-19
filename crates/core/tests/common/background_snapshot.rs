//! Captures every screen background before a desktop-changing test starts.
use anyhow::Result;

pub fn capture<S, U, O>(
    screens: impl IntoIterator<Item = S>,
    mut read: impl FnMut(&S) -> (Option<U>, Option<O>),
) -> Result<Vec<(S, U, O)>> {
    Ok(screens
        .into_iter()
        .filter_map(|screen| {
            let (url, options) = read(&screen);
            Some((screen, url?, options?))
        })
        .collect())
}
