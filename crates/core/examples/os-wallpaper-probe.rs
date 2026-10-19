//! JSON-lines bridge to the shared OS test support. It never sets a test photo.
//! Saves the desktop before replying ready; EOF or restore puts it back.
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use rotator_core::wallpaper::Layout;
use serde::Deserialize;
use serde_json::json;

// The opt-in harness also uses screenshot and multi-display helpers.
#[allow(dead_code)]
#[path = "../tests/common/os_wallpaper.rs"]
mod readback;

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    Verify { file: PathBuf, colour: String },
}

fn reply(value: serde_json::Value) -> Result<()> {
    println!("{value}");
    io::stdout().flush()?;
    Ok(())
}

fn serve() -> Result<()> {
    reply(json!({ "ready": true }))?;
    for line in io::stdin().lock().lines() {
        let Command::Verify { file, colour } = serde_json::from_str(&line?)?;
        ensure!(file.is_absolute(), "expected an absolute wallpaper path");
        let colour = match colour.as_str() {
            "red" => readback::Colour::Red,
            "green" => readback::Colour::Green,
            "blue" => readback::Colour::Blue,
            _ => anyhow::bail!("unknown synthetic fixture colour: {colour}"),
        };
        let layout = Layout::Same(readback::os::resolve(&file));
        match readback::read_back(&layout, colour) {
            Ok(evidence) => reply(json!({ "ok": true, "evidence": evidence }))?,
            Err(error) => reply(json!({ "ok": false, "error": format!("{error:#}") }))?,
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    // Remain on the main thread for macOS and Windows COM throughout.
    let saved = readback::os::save().context("saving the original wallpaper")?;
    let result = serve();
    readback::os::restore(saved).context("restoring the original wallpaper")?;
    result?;
    reply(json!({ "restored": true }))
}
