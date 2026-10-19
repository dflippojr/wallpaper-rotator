//! A stalled OS call must fail the OS wallpaper checks in bounded time, and the
//! original wallpaper must still be put back afterwards.
//!
//! These are ordinary tests: the stall is a seam (a child that sleeps, a call
//! that never returns), not a real hang, and nothing here touches the desktop.
//! The same limits are exercised against the real desktop by the
//! `stalled-call` case in `os_wallpaper.rs`.

#![allow(dead_code)]

use std::cell::Cell;
use std::fs;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

#[path = "common/bounded.rs"]
mod bounded;
use bounded::{
    call_within, read_back_with, run_bounded, stall_command, stall_if_asked, with_restoration,
    Stalled,
};

/// How far past its limit a bounded call may return. Generous: it only has to
/// be far below the 20 s the stalled seam would otherwise take.
const SLACK: Duration = Duration::from_secs(8);

/// The entry point the stalled child re-runs; a no-op in a normal test run.
#[test]
fn stall_child() {
    stall_if_asked();
}

fn stalled_child(heartbeat: Option<&std::path::Path>) -> std::process::Command {
    stall_command(&["stall_child", "--exact", "--nocapture"], heartbeat)
}

#[test]
fn a_stalled_subprocess_is_killed_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let beat = dir.path().join("heartbeat");
    let limit = Duration::from_secs(4);

    let start = Instant::now();
    let err = run_bounded(&mut stalled_child(Some(&beat)), limit).unwrap_err();
    let took = start.elapsed();

    assert!(err.is::<Stalled>(), "{err:#}");
    assert!(took >= limit && took < limit + SLACK, "took {took:?}");
    let alive = fs::metadata(&beat).map(|m| m.len()).unwrap_or(0);
    assert!(alive > 0, "the stalled child never started");
    sleep(Duration::from_millis(300));
    assert_eq!(
        fs::metadata(&beat).unwrap().len(),
        alive,
        "the stalled child was still running after the deadline"
    );
}

#[test]
fn a_subprocess_that_finishes_is_not_disturbed() {
    let mut echo = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "echo ok"]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", "echo ok"]);
        c
    };
    let out = run_bounded(&mut echo, Duration::from_secs(30)).unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
}

#[test]
fn a_stalled_in_process_call_is_abandoned_at_its_deadline() {
    let limit = Duration::from_secs(1);
    let start = Instant::now();
    let err = call_within(limit, || {
        sleep(Duration::from_secs(20));
        Ok(())
    })
    .unwrap_err();
    let took = start.elapsed();
    assert!(err.is::<Stalled>(), "{err:#}");
    assert!(took >= limit && took < limit + SLACK, "took {took:?}");

    assert_eq!(call_within(limit, || Ok(7)).unwrap(), 7);
    let err = call_within(limit, || -> Result<()> { Err(anyhow!("real failure")) }).unwrap_err();
    assert!(!err.is::<Stalled>() && err.to_string() == "real failure");
}

/// Polls like the real read-back, with an attempt that never comes back.
fn stalled_read_back(limit: Duration) -> Result<String> {
    read_back_with(
        limit,
        limit,
        |budget| {
            call_within(budget, || {
                sleep(Duration::from_secs(20));
                Ok("never".to_string())
            })
        },
        || "diagnosis".into(),
    )
}

#[test]
fn a_stalled_read_back_fails_in_bounded_time() {
    let limit = Duration::from_secs(1);
    let start = Instant::now();
    let err = stalled_read_back(limit).unwrap_err();
    let took = start.elapsed();
    assert!(err.is::<Stalled>(), "{err:#}");
    assert!(took < limit + SLACK, "took {took:?}");
}

#[test]
fn the_original_wallpaper_is_restored_after_a_stalled_call() {
    let desktop = Cell::new("original");
    let start = Instant::now();
    let result = with_restoration(
        || Ok(desktop.get()),
        || {
            desktop.set("test photo");
            stalled_read_back(Duration::from_secs(1)).map(|_| ())
        },
        |saved| {
            desktop.set(saved);
            Ok(())
        },
    );
    let err = result.unwrap_err();
    assert!(err.is::<Stalled>(), "{err:#}");
    assert!(start.elapsed() < SLACK + Duration::from_secs(1));
    assert_eq!(desktop.get(), "original", "the test photo is still up");
}

#[test]
fn the_original_wallpaper_is_restored_after_a_panic() {
    let desktop = Cell::new("original");
    let err = with_restoration(
        || Ok(desktop.get()),
        || {
            desktop.set("test photo");
            panic!("boom")
        },
        |saved| {
            desktop.set(saved);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("boom"), "{err:#}");
    assert_eq!(desktop.get(), "original");
}

#[test]
fn a_failed_restoration_fails_the_run_and_keeps_the_first_error() {
    let err = with_restoration(
        || Ok(()),
        || Err(anyhow!("check failed")),
        |()| Err(anyhow!("setter refused")),
    )
    .unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("check failed") && text.contains("setter refused"), "{text}");

    let err = with_restoration(|| Ok(()), || Ok(()), |()| Err(anyhow!("setter refused")))
        .unwrap_err();
    assert!(format!("{err:#}").contains("could not restore"), "{err:#}");
}

#[test]
fn nothing_runs_when_the_original_cannot_be_saved() {
    let ran = Cell::new(false);
    let err = with_restoration(
        || -> Result<()> { Err(anyhow!("no access")) },
        || {
            ran.set(true);
            Ok(())
        },
        |()| Ok(()),
    )
    .unwrap_err();
    assert!(!ran.get(), "the checks changed a desktop that was not saved");
    assert!(format!("{err:#}").contains("could not save"), "{err:#}");
}
