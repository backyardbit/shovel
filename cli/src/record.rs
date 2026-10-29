use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::host;
use crate::machine::Target;
use crate::vnc::Vnc;

fn state_path(target: &Target) -> PathBuf {
    target.dir.join("recording.json")
}

fn stop_path(target: &Target) -> PathBuf {
    target.dir.join("recording.stop")
}

fn result_path(target: &Target) -> PathBuf {
    target.dir.join("recording.result.json")
}

fn events_path(target: &Target) -> PathBuf {
    target.dir.join("recording.events.jsonl")
}

fn log_path(target: &Target) -> PathBuf {
    target.dir.join("recording.log")
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn read_state(target: &Target) -> Option<Value> {
    let text = std::fs::read_to_string(state_path(target)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn active(target: &Target) -> Option<Value> {
    let state = read_state(target)?;
    if alive(state["pid"].as_u64()?) {
        Some(state)
    } else {
        None
    }
}

pub fn note(target: &Target, kind: &str, x: u16, y: u16) {
    if !state_path(target).exists() {
        return;
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(events_path(target)) {
        let _ = writeln!(file, "{}", json!({"t": now_ms(), "kind": kind, "x": x, "y": y}));
    }
}

fn encoder_binary() -> Result<PathBuf> {
    let app = host::app_path().context("Shovel.app not found; it encodes recordings")?;
    Ok(app.join("Contents/MacOS/Shovel"))
}

pub fn start(target: &Target, output: Option<PathBuf>, fps: u32, max_seconds: u64) -> Result<Value> {
    if let Some(state) = active(target) {
        bail!("{} is already recording to {}", target.id, state["mp4"].as_str().unwrap_or("?"));
    }
    if host::status(&target.id)?["state"] != "running" {
        bail!("{} is not running", target.id);
    }
    encoder_binary()?;
    let mp4 = match output {
        Some(path) => path.with_extension("mp4"),
        None => target.dir.join("recordings").join(format!("{}.mp4", now_ms())),
    };
    if let Some(parent) = mp4.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mp4 = std::fs::canonicalize(mp4.parent().unwrap())?.join(mp4.file_name().unwrap());
    for path in [stop_path(target), result_path(target), events_path(target)] {
        let _ = std::fs::remove_file(path);
    }
    std::fs::write(events_path(target), "")?;
    let log = std::fs::File::create(log_path(target))?;
    let child = Command::new(std::env::current_exe()?)
        .args(["record", "worker", &target.id, "--output"])
        .arg(&mp4)
        .args(["--fps", &fps.to_string(), "--max-seconds", &max_seconds.to_string()])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    let state = json!({"pid": child.id(), "mp4": mp4, "gif": mp4.with_extension("gif"), "started": now_ms(), "fps": fps});
    std::fs::write(state_path(target), state.to_string())?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if target.dir.join("recording.ready").exists() {
            let _ = std::fs::remove_file(target.dir.join("recording.ready"));
            return Ok(state);
        }
        if !alive(child.id() as u64) {
            let _ = std::fs::remove_file(state_path(target));
            let log = std::fs::read_to_string(log_path(target)).unwrap_or_default();
            bail!("recorder exited: {}", log.trim());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(state)
}

pub fn stop(target: &Target) -> Result<Value> {
    let state = read_state(target).with_context(|| format!("{} is not recording", target.id))?;
    std::fs::write(stop_path(target), "")?;
    let pid = state["pid"].as_u64().unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(300);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(stop_path(target));
    let _ = std::fs::remove_file(state_path(target));
    match std::fs::read_to_string(result_path(target)) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(_) => {
            let log = std::fs::read_to_string(log_path(target)).unwrap_or_default();
            bail!("recording failed: {}", log.trim())
        }
    }
}

struct Marker {
    kind: String,
    x: i32,
    y: i32,
    at: u64,
}

struct Overlay {
    offset: u64,
    pointer: Option<(i32, i32)>,
    markers: Vec<Marker>,
}

impl Overlay {
    fn poll(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::File::open(path) else { return };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut text = String::new();
        if file.read_to_string(&mut text).is_err() {
            return;
        }
        let Some(end) = text.rfind('\n') else { return };
        self.offset += end as u64 + 1;
        for line in text[..end].lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else { continue };
            let x = event["x"].as_i64().unwrap_or(0) as i32;
            let y = event["y"].as_i64().unwrap_or(0) as i32;
            let kind = event["kind"].as_str().unwrap_or("").to_string();
            self.pointer = Some((x, y));
            if kind != "move" {
                self.markers.push(Marker { kind, x, y, at: event["t"].as_u64().unwrap_or(0) });
            }
        }
    }

    fn draw(&mut self, frame: &mut [u8], width: usize, height: usize) {
        let now = now_ms();
        self.markers.retain(|m| now.saturating_sub(m.at) < 700);
        for m in &self.markers {
            let age = now.saturating_sub(m.at) as f32 / 700.0;
            let radius = 10.0 + 22.0 * age;
            let color = if m.kind == "right" { [255, 120, 40] } else { [40, 40, 255] };
            ring(frame, width, height, m.x, m.y, radius, 3.0, color);
        }
        if let Some((x, y)) = self.pointer {
            disc(frame, width, height, x, y, 7.0, [0, 0, 0]);
            disc(frame, width, height, x, y, 5.0, [255, 255, 255]);
        }
    }
}

fn plot(frame: &mut [u8], width: usize, height: usize, x: i32, y: i32, bgr: [u8; 3]) {
    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
        return;
    }
    let i = (y as usize * width + x as usize) * 4;
    frame[i..i + 3].copy_from_slice(&bgr);
}

fn disc(frame: &mut [u8], width: usize, height: usize, cx: i32, cy: i32, r: f32, bgr: [u8; 3]) {
    let ri = r.ceil() as i32;
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            if ((dx * dx + dy * dy) as f32) <= r * r {
                plot(frame, width, height, cx + dx, cy + dy, bgr);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ring(frame: &mut [u8], width: usize, height: usize, cx: i32, cy: i32, r: f32, thickness: f32, bgr: [u8; 3]) {
    let ri = (r + thickness).ceil() as i32;
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            if (d - r).abs() <= thickness / 2.0 {
                plot(frame, width, height, cx + dx, cy + dy, bgr);
            }
        }
    }
}

pub fn worker(target: &Target, mp4: &Path, fps: u32, max_seconds: u64) -> Result<()> {
    let status = host::status(&target.id)?;
    let port = status["vnc_port"].as_u64().unwrap_or(0) as u16;
    let password = status["vnc_password"].as_str().unwrap_or("").to_string();
    let mut vnc = Vnc::connect(port, &password)?;
    let width = vnc.width as usize;
    let height = vnc.height as usize;
    let first = vnc.capture_bgrx()?;
    let shared = Arc::new(Mutex::new(first.clone()));
    let failed = Arc::new(AtomicBool::new(false));

    {
        let shared = shared.clone();
        let failed = failed.clone();
        std::thread::spawn(move || {
            let mut local = first;
            let result = (|| -> Result<()> {
                loop {
                    vnc.request(true)?;
                    loop {
                        if vnc.read_update(&mut local)?.is_some() {
                            break;
                        }
                    }
                    if local.len() == width * height * 4 {
                        shared.lock().unwrap().copy_from_slice(&local);
                    }
                }
            })();
            if let Err(err) = result {
                eprintln!("frame reader stopped: {err:#}");
                failed.store(true, Ordering::SeqCst);
            }
        });
    }

    let gif = mp4.with_extension("gif");
    let mut encoder = Command::new(encoder_binary()?)
        .arg("--encode")
        .arg(mp4)
        .args(["--width", &width.to_string(), "--height", &height.to_string(), "--fps", &fps.to_string(), "--gif"])
        .arg(&gif)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = encoder.stdin.take().unwrap();

    std::fs::write(target.dir.join("recording.ready"), "")?;
    let mut overlay = Overlay { offset: 0, pointer: None, markers: Vec::new() };
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let started = Instant::now();
    let mut next = started;
    let mut frame = vec![0u8; width * height * 4];
    loop {
        if stop_path(target).exists() || failed.load(Ordering::SeqCst) || started.elapsed().as_secs() >= max_seconds {
            break;
        }
        frame.copy_from_slice(&shared.lock().unwrap());
        overlay.poll(&events_path(target));
        overlay.draw(&mut frame, width, height);
        stdin.write_all(&frame)?;
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    drop(stdin);
    let output = encoder.wait_with_output()?;
    if !output.status.success() {
        bail!("encoder failed");
    }
    let result: Value = serde_json::from_slice(&output.stdout).context("encoder output")?;
    std::fs::write(result_path(target), result.to_string())?;
    let _ = std::fs::remove_file(state_path(target));
    Ok(())
}
