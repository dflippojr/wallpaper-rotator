//! Time limits for the OS wallpaper checks, and the guard that puts the
//! original wallpaper back whatever the checks did.
//!
//! Shared by the opt-in OS harness, the real-app probe and the ordinary tests
//! in `stalled_os_call.rs`, so none of it touches the desktop.

// Each includer uses a different part (macOS never abandons a call).
#![allow(dead_code)]

use std::env;
use std::fmt;
use std::fs;
use std::io::Read;
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

/// How long a helper subprocess (gsettings, gdbus, PowerShell, screencapture
/// and the like) may run before it is killed.
pub const COMMAND_LIMIT: Duration = Duration::from_secs(30);

/// The error for a call that did not come back in time.
#[derive(Debug)]
pub struct Stalled(pub String);

impl fmt::Display for Stalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Stalled {}

/// Runs `cmd` to completion, but not for longer than `limit`: after that the
/// child is killed and the error is [`Stalled`].
pub fn run_bounded(cmd: &mut Command, limit: Duration) -> Result<Output> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {name}"))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Stalled(format!(
                "{name} did not finish within {:.0} s; killed it",
                limit.as_secs_f64()
            ))
            .into());
        }
        sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: stdout
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default(),
        stderr: stderr
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_default(),
    })
}

/// Reads a child's pipe on its own thread, so a full pipe can't block the
/// child while the deadline runs. A grandchild that keeps the pipe open
/// after the kill only strands this thread, not the caller.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// Runs `call` and gives up on it after `limit`. The call keeps running on
/// its worker thread; the caller moves on (and still gets to restore the
/// desktop). Only for calls that may run off the main thread.
pub fn call_within<T: Send + 'static>(
    limit: Duration,
    call: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(call());
    });
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(Stalled(format!(
            "an OS call did not return within {:.0} s; left it on its worker thread",
            limit.as_secs_f64()
        ))
        .into()),
        Err(RecvTimeoutError::Disconnected) => Err(anyhow!("an OS call panicked")),
    }
}

/// Asks the OS for what the desktop shows until `attempt` says it matches:
/// the shell may apply a new wallpaper after the call returns. A wrong
/// answer is retried until `limit` has passed, checked between attempts, so
/// this is a retry window and not a wall-clock bound: the last attempt may
/// run for up to `attempt_limit` more, and an attempt that can't be
/// abandoned (macOS) may take longer still. Each attempt is expected to
/// enforce `attempt_limit`; one that stalls ends the wait at once.
pub fn read_back_with(
    limit: Duration,
    attempt_limit: Duration,
    mut attempt: impl FnMut(Duration) -> Result<String>,
    diagnose: impl FnOnce() -> String,
) -> Result<String> {
    let start = Instant::now();
    loop {
        match attempt(attempt_limit) {
            Ok(says) if start.elapsed() < Duration::from_millis(100) => return Ok(says),
            Ok(says) => {
                return Ok(format!(
                    "{says} (after {:.1} s)",
                    start.elapsed().as_secs_f64()
                ))
            }
            Err(e) if e.is::<Stalled>() => return Err(e),
            Err(e) if start.elapsed() > limit => {
                return Err(e.context(format!(
                    "still wrong after retrying for {} s (a retry window checked between attempts); {}",
                    limit.as_secs(),
                    diagnose()
                )));
            }
            Err(_) => sleep(Duration::from_millis(250)),
        }
    }
}

/// Saves the original state, runs `body`, then restores it, whatever
/// `body` did, including panic. A failure to save skips `body`; a failure to
/// restore fails the whole run.
pub fn with_restoration<S>(
    save: impl FnOnce() -> Result<S>,
    body: impl FnOnce() -> Result<()>,
    restore: impl FnOnce(S) -> Result<()>,
) -> Result<()> {
    let saved = save().context("could not save the original wallpaper")?;
    let result = panic::catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(anyhow!("the checks panicked: {what}"))
    });
    match (result, restore(saved)) {
        (result, Ok(())) => result,
        (Ok(()), Err(e)) => Err(e.context("could not restore the original wallpaper")),
        (Err(body), Err(e)) => Err(body.context(format!(
            "and then the original wallpaper could not be restored: {e:#}"
        ))),
    }
}

/// Set to make a process stall: the heartbeat file to append to, or `-` for
/// none. The seam for a call that never comes back; a real hang on a CI
/// runner would be a flake, not a test.
pub const STALL_ENV: &str = "ROTATOR_STALL_CHILD";

/// How long the stalled child keeps going if nobody kills it.
const STALL_FOR: Duration = Duration::from_secs(20);

/// The child side of [`stall_command`]. Call it first thing in the process
/// (or test) that the command re-runs; it does nothing without
/// [`STALL_ENV`].
pub fn stall_if_asked() {
    let Some(beat) = env::var_os(STALL_ENV) else {
        return;
    };
    let beat = (beat != "-").then(|| PathBuf::from(beat));
    let end = Instant::now() + STALL_FOR;
    while Instant::now() < end {
        if let Some(file) = &beat {
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(file) {
                let _ = f.write_all(b".");
            }
        }
        sleep(Duration::from_millis(50));
    }
    process::exit(0);
}

/// A command that stalls: this very executable, running [`stall_if_asked`]
/// with `args`. It appends to `heartbeat` while alive, so a test can tell it
/// was killed.
pub fn stall_command(args: &[&str], heartbeat: Option<&Path>) -> Command {
    let mut cmd = Command::new(env::current_exe().expect("the current executable"));
    cmd.args(args).env(
        STALL_ENV,
        heartbeat.map_or("-".into(), |p| p.as_os_str().to_owned()),
    );
    cmd
}
