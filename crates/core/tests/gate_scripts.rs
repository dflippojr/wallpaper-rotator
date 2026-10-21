//! The gate wrappers keep test scratch in the worktree and remove it on exit (AGENTS.md, "Scratch space").
//! Each test runs the wrapper for its own OS against a mock `cargo`, so every CI test job checks the wrapper.
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The wrapper this OS's test job runs.
const WRAPPER: &str = if cfg!(windows) {
    "check.ps1"
} else {
    "check.sh"
};

/// A scratch copy of one wrapper, with a mock `cargo` first on PATH.
struct GateTree {
    dir: tempfile::TempDir,
}

impl GateTree {
    fn new(wrapper: &str) -> Self {
        Self::in_dir(wrapper, tempfile::tempdir().unwrap())
    }

    /// Builds the tree inside `dir`, so a test can choose the path the wrapper runs from.
    fn in_dir(wrapper: &str, dir: tempfile::TempDir) -> Self {
        let root = dir.path();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(root.join(".github/workflows")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(
            root.join(".github/workflows/ci.yml"),
            "env:\n  COVERAGE_FLOOR: 80\n",
        )
        .unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts")
            .join(wrapper);
        fs::copy(source, root.join("scripts").join(wrapper)).unwrap();
        write_mock_cargo(&root.join("bin"));
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn script(&self, wrapper: &str) -> PathBuf {
        self.root().join("scripts").join(wrapper)
    }

    fn log(&self) -> PathBuf {
        self.root().join("gate.log")
    }

    /// The mock's PATH: its own `bin` first, so the wrapper's cargo calls reach it.
    fn path(&self) -> std::ffi::OsString {
        let mut paths = vec![self.root().join("bin")];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        std::env::join_paths(paths).unwrap()
    }

    /// Every cargo call saw scratch in a `.verify` directory, and nothing is left in this tree's `.verify/` afterwards.
    fn assert_scratch_contained(&self) {
        let log = fs::read_to_string(self.log()).unwrap();
        let verify = self.root().join(".verify");
        let mut calls = 0;
        for line in log.lines() {
            let scratch = line
                .split("scratch=")
                .nth(1)
                .expect("mock cargo logs scratch=");
            let parent = Path::new(scratch).parent().and_then(Path::file_name);
            assert_eq!(
                parent,
                Some(OsStr::new(".verify")),
                "scratch {scratch} is not under .verify"
            );
            calls += 1;
        }
        assert!(calls > 0, "mock cargo never ran");
        let left: Vec<_> = fs::read_dir(verify)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert!(left.is_empty(), "scratch left behind: {left:?}");
    }
}

/// The mock logs the scratch it was given and creates a file in it, as a real test run would.
/// It fails the step named in GATE_FAIL.
#[cfg(unix)]
fn write_mock_cargo(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        r#"#!/bin/sh
echo "$1 scratch=${TMPDIR:-unset}" >> "$GATE_LOG"
if [ -n "${TMPDIR:-}" ]; then mkdir -p "$TMPDIR/sim" && echo x > "$TMPDIR/sim/file"; fi
if [ "$1" = "${GATE_FAIL:-}" ]; then exit 101; fi
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(cargo, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(windows)]
fn write_mock_cargo(bin: &Path) {
    fs::write(
        bin.join("cargo.cmd"),
        r#"@echo off
echo %1 scratch=%TMP%>>"%GATE_LOG%"
if defined TMP if not exist "%TMP%\sim" mkdir "%TMP%\sim"
if defined TMP echo x>"%TMP%\sim\file"
if "%GATE_FAIL%"=="%1" exit /b 101
exit /b 0
"#,
    )
    .unwrap();
}

/// Runs the wrapper the way a developer does: `bash scripts/check.sh` on Unix, `check.ps1` on Windows.
fn run_gate(tree: &GateTree, fail_on: Option<&str>) -> Output {
    let _ = fs::remove_file(tree.log());
    let mut command = Command::new(if cfg!(windows) { "powershell" } else { "bash" });
    if cfg!(windows) {
        command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
    }
    command
        .arg(tree.script(WRAPPER))
        .current_dir(tree.root())
        .env("PATH", tree.path())
        .env("GATE_LOG", tree.log())
        .env("GATE_FAIL", fail_on.unwrap_or(""));
    command.output().unwrap()
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn gate_wrapper_keeps_scratch_in_the_worktree_and_removes_it() {
    let tree = GateTree::new(WRAPPER);

    let passed = run_gate(&tree, None);
    assert!(passed.status.success(), "{}", output_text(&passed));
    tree.assert_scratch_contained();

    let failed = run_gate(&tree, Some("clippy"));
    assert!(!failed.status.success(), "{}", output_text(&failed));
    tree.assert_scratch_contained();
}

/// Runs check.ps1 from an existing PowerShell session, as a caller does, with the caller's TMP and TEMP set first.
/// The script path goes in through an environment variable, so no path characters are parsed as PowerShell.
#[cfg(windows)]
fn run_from_caller_session(tree: &GateTree) -> Output {
    Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg("$env:TMP = 'caller-tmp'; $env:TEMP = 'caller-temp'; & $env:GATE_SCRIPT; Write-Output ('after TMP=' + $env:TMP + ' TEMP=' + $env:TEMP)")
        .env("GATE_SCRIPT", tree.script("check.ps1"))
        .current_dir(tree.root())
        .env("PATH", tree.path())
        .env("GATE_LOG", tree.log())
        .output()
        .unwrap()
}

#[cfg(windows)]
fn assert_caller_temp_restored(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("after TMP=caller-tmp TEMP=caller-temp"),
        "{}",
        output_text(output)
    );
}

/// A caller who runs check.ps1 in an existing session keeps its own TMP and TEMP afterwards.
#[cfg(windows)]
#[test]
fn check_ps1_restores_the_callers_temp_variables() {
    let tree = GateTree::new("check.ps1");
    assert_caller_temp_restored(&run_from_caller_session(&tree));
}

/// The same invocation from a path with an apostrophe, as in a user profile such as `C:\Users\O'Brien`.
#[cfg(windows)]
#[test]
fn check_ps1_restores_the_callers_temp_variables_under_an_apostrophe_path() {
    let dir = tempfile::Builder::new()
        .prefix("o'brien-")
        .tempdir()
        .unwrap();
    let tree = GateTree::in_dir("check.ps1", dir);
    assert_caller_temp_restored(&run_from_caller_session(&tree));
}
