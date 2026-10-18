//! Offline journeys through the shipped CLI, with a separate home for each test.
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use image::{ImageFormat, Rgb, RgbImage};
use rotator_core::config::Config;
use rotator_core::paths::Paths;
use rotator_core::source::SourceId;
use rotator_core::store::State;

struct Cli {
    root: tempfile::TempDir,
    paths: Paths,
    originals: Vec<(PathBuf, Vec<u8>)>,
}

impl Cli {
    fn new(sources: Vec<SourceId>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("rotator cli ")
            .tempdir()
            .unwrap();
        let paths = Paths::under(root.path());
        fs::create_dir_all(&paths.default_favorites_dir).unwrap();
        let originals = ["Ann - a.png", "Bo - b.png", "Cy - c.png"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let path = paths.default_favorites_dir.join(name);
                RgbImage::from_pixel(400, 300, Rgb([30 + i as u8 * 50, 90, 160]))
                    .save_with_format(&path, ImageFormat::Png)
                    .unwrap();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        Config {
            sources,
            favorites_dir: Some(paths.default_favorites_dir.clone()),
            ..Config::default()
        }
        .save(&paths.config_file)
        .unwrap();
        let cli = Self {
            root,
            paths,
            originals,
        };
        // Refuse to rotate unless the binary proves it is using our sandbox.
        // This also makes running these tests against the pre-override binary safe.
        cli.assert_paths();
        cli
    }

    fn run(&self, args: &[&str]) -> Output {
        if args.first() == Some(&"rotate") {
            assert!(
                args.contains(&"--dry-run"),
                "tests must never apply a wallpaper"
            );
        }
        Command::new(env!("CARGO_BIN_EXE_wallpaper-rotator-cli"))
            .args(args)
            .env("WALLPAPER_ROTATOR_HOME", self.root.path())
            .env("ROTATOR_SIZE", "320x200")
            .env_remove("UNSPLASH_ACCESS_KEY")
            .env_remove("PEXELS_API_KEY")
            .output()
            .expect("run the real CLI binary")
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn assert_paths(&self) {
        let output = self.ok(&["paths"]);
        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                format!("config:    {}", self.paths.config_file.display()),
                format!("state:     {}", self.paths.state_file.display()),
                format!("images:    {}", self.paths.images_dir.display()),
                format!("log:       {}", self.paths.log_file.display()),
                format!("favorites: {}", self.paths.default_favorites_dir.display()),
            ]
        );
    }

    fn rotate(&self) -> String {
        let output = self.ok(&["rotate", "--dry-run"]);
        assert!(output.contains("would set "), "{output}");
        assert!(
            output
                .lines()
                .any(|l| l.starts_with("set photo fav-") && l.contains(" by ")),
            "{output}"
        );
        output
    }

    fn state(&self) -> State {
        assert!(self.paths.state_file.exists());
        State::load(&self.paths.state_file)
    }

    fn cached(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = fs::read_dir(&self.paths.images_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        files
    }

    fn assert_cache_and_originals(&self) {
        let files = self.cached();
        assert_eq!(files.len(), 2, "{files:?}");
        for path in files {
            assert_eq!(path.extension().unwrap(), "jpg");
            assert_eq!(
                image::ImageReader::open(&path)
                    .unwrap()
                    .with_guessed_format()
                    .unwrap()
                    .format(),
                Some(ImageFormat::Jpeg)
            );
            assert_eq!(image::image_dimensions(path).unwrap(), (320, 200));
        }
        for (path, bytes) in &self.originals {
            assert_eq!(
                &fs::read(path).unwrap(),
                bytes,
                "{} changed",
                path.display()
            );
        }
    }
}

#[test]
fn paths_use_the_overridden_home() {
    Cli::new(vec![SourceId::Favorites]).assert_paths();
}

#[test]
fn rotate_saves_current_and_next_without_changing_originals() {
    let cli = Cli::new(vec![SourceId::Favorites]);
    cli.rotate();
    let output = cli.ok(&["status"]);
    assert_eq!(
        output.lines().filter(|l| l.starts_with("current:")).count(),
        1,
        "{output}"
    );
    assert_eq!(
        output.lines().filter(|l| l.starts_with("next:")).count(),
        1,
        "{output}"
    );
    assert!(output.contains("cache:   2 files"), "{output}");
    let state = cli.state();
    let current = &state.current.as_ref().unwrap().photos[0];
    let next = &state.next.as_ref().unwrap().photos[0];
    assert!(
        output.contains(&format!(
            "current: {} by {} -> {}",
            current.id,
            current.author,
            current.file.display()
        )),
        "{output}"
    );
    assert!(
        output.contains(&format!("next:    {} by {}", next.id, next.author)),
        "{output}"
    );
    let mut expected = vec![current.file.clone(), next.file.clone()];
    expected.sort();
    assert_eq!(cli.cached(), expected);
    cli.assert_cache_and_originals();
}

#[test]
fn second_rotate_uses_prefetch_and_removes_the_old_file() {
    let cli = Cli::new(vec![SourceId::Favorites]);
    cli.rotate();
    let first = cli.state();
    let old = &first.current.as_ref().unwrap().photos[0];
    let prefetched = &first.next.as_ref().unwrap().photos[0];
    cli.rotate();
    let state = cli.state();
    let current = &state.current.as_ref().unwrap().photos[0];
    assert_ne!(current.id, old.id);
    assert_eq!(current.id, prefetched.id);
    assert_eq!(current.file, prefetched.file);
    assert!(!old.file.exists(), "old wallpaper remains cached");
    assert!(cli.ok(&["status"]).contains("cache:   2 files"));
    cli.assert_cache_and_originals();
}

#[test]
fn cleanup_removes_strays_and_preserves_current() {
    let cli = Cli::new(vec![SourceId::Favorites]);
    cli.rotate();
    let before = cli.cached();
    let current = cli.state().current.unwrap().photos[0].file.clone();
    let bytes = fs::read(&current).unwrap();
    for name in ["old.jpg", "half.jpg.part"] {
        fs::write(cli.paths.images_dir.join(name), b"stray").unwrap();
    }
    let output = cli.ok(&["cleanup"]);
    assert!(output.starts_with("removed 2 files ("), "{output}");
    assert_eq!(fs::read(current).unwrap(), bytes);
    assert_eq!(cli.cached(), before);
    cli.assert_cache_and_originals();
}

#[test]
fn rotate_without_sources_explains_how_to_configure_them() {
    let cli = Cli::new(vec![]);
    let output = cli.run(&["rotate", "--dry-run"]);
    assert!(!output.status.success(), "{output:?}");
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("tick a photo source in Settings"), "{error}");
    assert!(!cli.paths.state_file.exists());
    assert!(!cli.paths.images_dir.exists());
}

#[test]
fn status_without_state_reports_none() {
    let cli = Cli::new(vec![]);
    assert!(!cli.paths.state_file.exists());
    assert_eq!(
        cli.ok(&["status"]),
        "current: none\ncache:   0 files, 0.0 MB\n"
    );
    assert!(!cli.paths.state_file.exists());
}

#[test]
fn missing_or_unknown_subcommand_prints_usage_and_fails() {
    let cli = Cli::new(vec![]);
    for args in [&[][..], &["unknown"][..]] {
        let output = cli.run(args);
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains("usage: wallpaper-rotator-cli <rotate|status|cleanup|displays|paths>"),
            "{error}"
        );
    }
}

#[test]
fn displays_reports_the_platform_contract() {
    let cli = Cli::new(vec![]);
    let output = cli.run(&["displays"]);
    if cfg!(any(target_os = "macos", windows)) {
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.lines()
                .any(|l| l.contains(" (id ") && l.ends_with(')')),
            "{text}"
        );
    } else {
        assert!(!output.status.success(), "{output:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("only from the tray app"), "{error}");
    }
}
