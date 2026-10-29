use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::paths;

pub fn request(body: Value) -> Result<Value> {
    let stream = connect(true)?;
    send(stream, body)
}

pub fn request_if_running(body: Value) -> Result<Option<Value>> {
    match connect(false) {
        Ok(stream) => send(stream, body).map(Some),
        Err(_) => Ok(None),
    }
}

fn send(mut stream: UnixStream, body: Value) -> Result<Value> {
    writeln!(stream, "{body}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).context("invalid reply from Shovel host")?;
    if reply["ok"].as_bool() != Some(true) {
        bail!("{}", reply["error"].as_str().unwrap_or("Shovel host returned an error"));
    }
    Ok(reply)
}

fn connect(launch_if_needed: bool) -> Result<UnixStream> {
    if paths::socket().as_os_str().len() > 103 {
        bail!("socket path {} is too long for a Unix socket; use a shorter SHOVEL_HOME", paths::socket().display());
    }
    if let Ok(stream) = UnixStream::connect(paths::socket()) {
        return Ok(stream);
    }
    if !launch_if_needed {
        bail!("Shovel host is not running");
    }
    launch()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(paths::socket()) {
            return Ok(stream);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("Shovel host did not start; try `open -a Shovel` and check Console.app for errors")
}

pub fn app_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(custom) = std::env::var("SHOVEL_APP") {
        candidates.push(PathBuf::from(custom));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("../../../build/Shovel.app"));
        }
    }
    candidates.push(paths::home().join("Applications/Shovel.app"));
    candidates.push(PathBuf::from("/Applications/Shovel.app"));
    candidates.into_iter().find(|p| p.join("Contents/MacOS/Shovel").exists())
}

fn launch() -> Result<()> {
    std::fs::create_dir_all(paths::root())?;
    let app = app_path().context("Shovel.app not found; run `make install` in the shovel repo or set SHOVEL_APP")?;
    let mut open = Command::new("open");
    open.arg("-g");
    if let Ok(custom) = std::env::var("SHOVEL_HOME") {
        open.arg("--env").arg(format!("SHOVEL_HOME={custom}"));
    }
    let status = open.arg("-a").arg(&app).status()?;
    if !status.success() {
        bail!("failed to launch {}", app.display());
    }
    Ok(())
}

pub fn status(id: &str) -> Result<Value> {
    match request_if_running(json!({"cmd": "status", "id": id}))? {
        Some(reply) => Ok(reply["vm"].clone()),
        None => Ok(json!({"id": id, "state": "stopped"})),
    }
}
