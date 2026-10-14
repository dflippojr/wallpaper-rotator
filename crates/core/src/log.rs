//! A tiny append-only log that caps its own size, so the app never grows a
//! file on disk without bound.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const MAX_LOG_BYTES: u64 = 512 * 1024;

pub fn write(path: &Path, message: &str) {
    eprintln!("{message}");
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        let _ = fs::rename(path, path.with_extension("log.old"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {message}", crate::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_over_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("rotator.log");
        fs::write(&log, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        write(&log, "hello");
        assert!(fs::metadata(&log).unwrap().len() < 100);
        assert!(dir.path().join("rotator.log.old").exists());
    }
}
