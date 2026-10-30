use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::fd::FromRawFd;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::machine::{self, Target};
use crate::vnc::{self, Vnc};
use crate::{host, keys, password, paths, record, ShareMode};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = r#"Shovel gives you a disposable macOS virtual machine that you operate like a computer: look with `screenshot`, act with `left_click`, `type`, `key`, `scroll`, `left_click_drag`, then `screenshot` again to verify. The screen is the only ground truth for GUI state.
1. VM: if a default VM is bound (vm_list shows it), just call the tools. Otherwise call vm_up with a short new name such as "agent-1"; it boots (about a minute the first time) and becomes the default. At most 2 macOS VMs can run at once. Every tool also accepts an optional "vm".
2. Coordinates are pixels in the most recent full screenshot (origin top-left); the server maps them to the VM display. `zoom` shows a region in more detail but its pixels are not click coordinates.
3. The guest is macOS: shortcuts use cmd (cmd+space Spotlight, cmd+tab switch apps, cmd+q quit, cmd+w close). Use `exec` for anything scriptable (zsh over SSH, passwordless sudo, Homebrew on PATH) and the GUI for what needs the screen. Do not run osascript/AppleScript through exec (it can hang on a permission prompt); use open_application or the GUI instead.
4. When a dialog asks for the user's password, click the password field and call type_password; it types the guest password without revealing it. Never ask for the password.
5. A human may take over the VM. Input tools then fail with a "human has taken control" error: call wait_for_control, then screenshot before continuing. screenshot and exec keep working.
6. When you are done with a VM you created with vm_up, call vm_down with remove=true."#;

const HELD: &str = "The left button is held (left_mouse_down); call left_mouse_up first.";

extern "C" {
    fn dup(fd: i32) -> i32;
    fn dup2(src: i32, dst: i32) -> i32;
}

struct Server {
    default_vm: Option<String>,
    max: (u32, u32),
    pointer: HashMap<String, (u16, u16)>,
    held: Option<(String, Vnc)>,
    created: HashSet<String>,
}

#[derive(Clone, Copy)]
struct Geo {
    w: u32,
    h: u32,
    sw: u32,
    sh: u32,
}

pub fn serve(vm: Option<String>, max_width: u32, max_height: u32) -> Result<()> {
    let fd = unsafe { dup(1) };
    if fd < 0 {
        bail!("cannot duplicate stdout");
    }
    let mut out = unsafe { File::from_raw_fd(fd) };
    unsafe { dup2(2, 1) };
    let mut server = Server {
        default_vm: vm,
        max: (max_width.max(1), max_height.max(1)),
        pointer: HashMap::new(),
        held: None,
        created: HashSet::new(),
    };
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Err(_) => Some(error(Value::Null, -32700, "Parse error")),
            Ok(Value::Array(batch)) if !batch.is_empty() => {
                let replies: Vec<Value> = batch.into_iter().filter_map(|m| server.handle(m)).collect();
                if replies.is_empty() {
                    None
                } else {
                    Some(Value::Array(replies))
                }
            }
            Ok(msg) => server.handle(msg),
        };
        if let Some(reply) = reply {
            writeln!(out, "{}", serde_json::to_string(&reply)?)?;
            out.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn negotiate(requested: Option<&str>) -> &'static str {
    VERSIONS.iter().find(|v| Some(**v) == requested).copied().unwrap_or(VERSIONS[0])
}

impl Server {
    fn handle(&mut self, msg: Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(error(Value::Null, -32600, "Invalid Request"));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return id.filter(|_| !obj.contains_key("result") && !obj.contains_key("error")).map(|id| error(id, -32600, "Invalid Request"));
        };
        let id = id?;
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let mut instructions = INSTRUCTIONS.to_string();
                if let Some(vm) = &self.default_vm {
                    instructions.push_str(&format!("\nThis server is bound to VM '{vm}'."));
                }
                json!({
                    "protocolVersion": negotiate(params["protocolVersion"].as_str()),
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "shovel", "title": "Shovel macOS VMs", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": instructions,
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let Some(tool) = tools().into_iter().find(|t| t["name"] == name) else {
                    return Some(error(id, -32602, &format!("Unknown tool: {name}")));
                };
                let args = match &params["arguments"] {
                    Value::Object(_) => params["arguments"].clone(),
                    _ => json!({}),
                };
                let started = Instant::now();
                let outcome = check_arguments(&tool, &args).and_then(|_| self.call(name, &args));
                eprintln!("shovel mcp: {name} {}ms", started.elapsed().as_millis());
                match outcome {
                    Ok(content) => json!({"content": content, "isError": false}),
                    Err(err) => json!({"content": [text(format!("{err:#}"))], "isError": true}),
                }
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Vec<Value>> {
        match name {
            "screenshot" => self.screenshot(args),
            "zoom" => self.zoom(args),
            "left_click" => self.click(args, vnc::BUTTON_LEFT, 1, "Left-clicked"),
            "right_click" => self.click(args, vnc::BUTTON_RIGHT, 1, "Right-clicked"),
            "middle_click" => self.click(args, vnc::BUTTON_MIDDLE, 1, "Middle-clicked"),
            "double_click" => self.click(args, vnc::BUTTON_LEFT, 2, "Double-clicked"),
            "triple_click" => self.click(args, vnc::BUTTON_LEFT, 3, "Triple-clicked"),
            "mouse_move" => self.mouse_move(args),
            "left_click_drag" => self.drag(args),
            "left_mouse_down" => self.mouse_down(args),
            "left_mouse_up" => self.mouse_up(args),
            "scroll" => self.scroll(args),
            "type" => {
                let input = string(args, "text")?.context("\"text\" is required")?;
                let (target, mut vnc) = self.input(args)?;
                let mut runs: Vec<(bool, String)> = Vec::new();
                for c in input.chars() {
                    match runs.last_mut() {
                        Some((ascii, run)) if *ascii == c.is_ascii() => run.push(c),
                        _ => runs.push((c.is_ascii(), c.to_string())),
                    }
                }
                let mut pasted = false;
                for (ascii, chunk) in &runs {
                    if *ascii {
                        keys::type_text(&mut vnc, chunk)?;
                        continue;
                    }
                    let (code, _, stderr) = run(&target.id, "LC_ALL=en_US.UTF-8 pbcopy", 15, Some(chunk.as_bytes()))?;
                    if code != 0 {
                        bail!("could not put non-ASCII text on the VM clipboard: {}", String::from_utf8_lossy(&stderr).trim());
                    }
                    keys::press(&mut vnc, "cmd+v")?;
                    vnc::pause(200);
                    pasted = true;
                }
                vnc.finish()?;
                let mut out = format!("Typed {} characters.", input.chars().count());
                if pasted {
                    out.push_str(" Non-ASCII parts were pasted with cmd+v through the VM clipboard, which now holds the last of them.");
                }
                Ok(vec![text(out)])
            }
            "key" => {
                let combos = string(args, "keys")?.context("\"keys\" is required")?;
                let combos: Vec<&str> = combos.split_whitespace().collect();
                if combos.is_empty() {
                    bail!("\"keys\" is empty");
                }
                for combo in &combos {
                    keys::parse_combo(combo)?;
                }
                let repeat = int(args, "repeat", 1, 100, 1)?;
                let (_, mut vnc) = self.input(args)?;
                for _ in 0..repeat {
                    for combo in &combos {
                        keys::press(&mut vnc, combo)?;
                    }
                }
                vnc.finish()?;
                let times = if repeat > 1 { format!(" {repeat} times") } else { String::new() };
                Ok(vec![text(format!("Pressed {}{times}.", combos.join(" ")))])
            }
            "hold_key" => {
                let combo = string(args, "keys")?.context("\"keys\" is required")?;
                let combo = combo.trim().to_string();
                if combo.is_empty() || combo.contains(char::is_whitespace) {
                    bail!("\"keys\" must be one key or chord, e.g. \"shift\" or \"cmd+tab\"");
                }
                keys::parse_combo(&combo)?;
                let seconds = number(args, "duration_seconds")?.context("\"duration_seconds\" is required")?;
                if !(0.0..=10.0).contains(&seconds) {
                    bail!("\"duration_seconds\" must be from 0 to 10");
                }
                let (_, mut vnc) = self.input(args)?;
                keys::hold(&mut vnc, &combo, Duration::from_secs_f64(seconds))?;
                vnc.finish()?;
                Ok(vec![text(format!("Held {combo} for {seconds} s."))])
            }
            "cursor_position" => {
                let target = self.target(args)?;
                let vnc = crate::connect_vnc(&target, true)?;
                let geo = self.geo(&vnc);
                Ok(vec![text(match self.pointer.get(&target.id) {
                    Some(&(nx, ny)) => {
                        let (x, y) = geo.to_screen(nx, ny);
                        format!("Pointer at ({x}, {y}) in screenshot pixels.")
                    }
                    None => "Pointer position unknown: the VM does not report it and no mouse tool has moved it since this server started. Use mouse_move to place it.".into(),
                })])
            }
            "wait" => {
                let seconds = number(args, "duration_seconds")?.context("\"duration_seconds\" is required")?;
                if !(0.0..=60.0).contains(&seconds) {
                    bail!("\"duration_seconds\" must be from 0 to 60");
                }
                std::thread::sleep(Duration::from_secs_f64(seconds));
                Ok(vec![text(format!("Waited {seconds} s."))])
            }
            "exec" => {
                let command = string(args, "command")?.context("\"command\" is required")?;
                let timeout = int(args, "timeout_seconds", 1, 600, 50)?;
                let target = self.target(args)?;
                let (code, stdout, stderr) = run(&target.id, &command, timeout, None)?;
                let mut out = format!("exit code: {code}");
                for (label, stream) in [("stdout", stdout), ("stderr", stderr)] {
                    if !stream.is_empty() {
                        out.push_str(&format!("\n--- {label} ---\n{}", tail(&stream)));
                    }
                }
                Ok(vec![text(out)])
            }
            "open_application" => {
                let app = string(args, "app")?.context("\"app\" is required")?;
                let target = self.target(args)?;
                let flag = if is_bundle_id(&app) { "-b" } else { "-a" };
                let (code, _, stderr) = run(&target.id, &format!("open {flag} {}", machine::shell_quote(&app)), 30, None)?;
                if code != 0 {
                    bail!("could not open {app}: {}", String::from_utf8_lossy(&stderr).trim());
                }
                Ok(vec![text(format!("Opened {app}. Take a screenshot to see it."))])
            }
            "type_password" => {
                let enter = boolean(args, "press_enter")?;
                let target = self.target(args)?;
                let image = target.spec()?.image.with_context(|| format!("{} does not record which image it came from", target.id))?;
                let secret = password::get(&image)?;
                let (_, mut vnc) = self.input(args)?;
                keys::type_text(&mut vnc, &secret)?;
                drop(secret);
                if enter {
                    keys::press(&mut vnc, "return")?;
                }
                vnc.finish()?;
                Ok(vec![text("Typed the guest password into the focused field.".into())])
            }
            "wait_for_control" => {
                let timeout = int(args, "timeout_seconds", 1, 3600, 50)?;
                let target = self.target(args)?;
                let vm = target.id;
                let deadline = Instant::now() + Duration::from_secs(timeout);
                loop {
                    let status = host::status(&vm)?;
                    if status["state"] != "running" {
                        bail!("VM '{vm}' is not running. Call vm_up with {{\"name\": \"{vm}\"}} to boot it.");
                    }
                    if status["controller"] != "human" {
                        return Ok(vec![text(format!("Agent has control of VM '{vm}' again. Take a screenshot before acting; the human may have changed the screen."))]);
                    }
                    if Instant::now() >= deadline {
                        return Ok(vec![text(format!("VM '{vm}' is still under human control after {timeout} s. Call wait_for_control again, or ask the user to click Release in the Shovel viewer."))]);
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            "vm_view" => {
                let target = self.target(args)?;
                crate::ensure_running(&target)?;
                host::request(json!({"cmd": "view", "id": target.id}))?;
                Ok(vec![text(format!("Opened the live view of '{}' on the user's Mac; they can watch and click Take Over.", target.id))])
            }
            "vm_list" => {
                let listing = json!({"default_vm": self.default_vm, "vms": crate::vm_rows()?});
                Ok(vec![text(serde_json::to_string_pretty(&listing)?)])
            }
            "vm_up" => self.vm_up(args),
            "vm_down" => self.vm_down(args),
            _ => bail!("Unknown tool: {name}"),
        }
    }

    fn vm_name(&self, args: &Value, key: &str) -> Result<String> {
        let name = match string(args, key)?.filter(|s| !s.is_empty()) {
            Some(name) => name,
            None => match &self.default_vm {
                Some(vm) => vm.clone(),
                None => {
                    let rows = crate::vm_rows().unwrap_or_default();
                    let existing = if rows.is_empty() {
                        "none".to_string()
                    } else {
                        rows.iter()
                            .map(|r| format!("{} ({})", r["name"].as_str().unwrap_or("?"), r["state"].as_str().unwrap_or("?")))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    bail!("No VM selected. Call vm_up with a new name (e.g. {{\"name\": \"agent-1\"}}) to create and boot one; it becomes the default. Or pass \"{key}\". Existing VMs: {existing}.");
                }
            },
        };
        machine::validate_name(&name)?;
        Ok(name)
    }

    fn target(&self, args: &Value) -> Result<Target> {
        let name = self.vm_name(args, "vm")?;
        if !paths::vm(&name).join("config.json").exists() {
            bail!("No VM named '{name}'. Call vm_up with {{\"name\": \"{name}\"}} to create and boot it.");
        }
        let target = Target::resolve(&name)?;
        if host::status(&name)?["state"] != "running" {
            bail!("VM '{name}' is not running. Call vm_up with {{\"name\": \"{name}\"}} to boot it.");
        }
        Ok(target)
    }

    fn input(&mut self, args: &Value) -> Result<(Target, Vnc)> {
        let target = self.target(args)?;
        match crate::connect_vnc(&target, false) {
            Ok(vnc) => Ok((target, vnc)),
            Err(err) if err.downcast_ref::<crate::HumanControl>().is_some() => Err(self.human(&target.id)),
            Err(err) => Err(err),
        }
    }

    fn human(&mut self, vm: &str) -> anyhow::Error {
        self.release_held(vm);
        anyhow!("A human has taken control of VM '{vm}' and is using its screen, so this input was NOT sent. Call wait_for_control to wait until they hand control back, then take a fresh screenshot before acting (the screen may have changed). screenshot and exec still work meanwhile.")
    }

    fn release_held(&mut self, vm: &str) {
        if self.held.as_ref().is_some_and(|(v, _)| v == vm) {
            if let Some((_, mut vnc)) = self.held.take() {
                if let Some(&(x, y)) = self.pointer.get(vm) {
                    let _ = vnc.pointer(0, x, y);
                }
                let _ = vnc.finish();
            }
        }
    }

    fn not_held(&mut self, vm: &str) -> Result<()> {
        if !self.is_held(vm) {
            return Ok(());
        }
        if host::status(vm)?["controller"] == "human" {
            return Err(self.human(vm));
        }
        bail!(HELD)
    }

    fn is_held(&self, vm: &str) -> bool {
        self.held.as_ref().is_some_and(|(v, _)| v == vm)
    }

    fn geo(&self, vnc: &Vnc) -> Geo {
        Geo::new(vnc.width as u32, vnc.height as u32, self.max)
    }

    fn screenshot(&mut self, args: &Value) -> Result<Vec<Value>> {
        let target = self.target(args)?;
        let mut vnc = crate::connect_vnc(&target, true)?;
        let (w, h, rgb) = vnc.capture()?;
        let geo = Geo::new(w, h, self.max);
        let small = downscale(w, h, &rgb, geo.sw, geo.sh);
        let png = vnc::encode_png(geo.sw, geo.sh, &small, png::Compression::Default)?;
        let pointer = match self.pointer.get(&target.id) {
            Some(&(nx, ny)) => {
                let (x, y) = geo.to_screen(nx, ny);
                format!("({x}, {y})")
            }
            None => "unknown".into(),
        };
        let mut note = format!(
            "Screen of VM '{}': {}x{} screenshot pixels (VM display {w}x{h}). Use these pixel coordinates for click/move/drag/scroll. Pointer: {pointer}.",
            target.id, geo.sw, geo.sh
        );
        if host::status(&target.id)?["controller"] == "human" {
            note.push_str("\nA human currently controls this VM; input tools will fail until wait_for_control returns.");
        }
        Ok(vec![json!({"type": "image", "data": base64(&png), "mimeType": "image/png"}), text(note)])
    }

    fn zoom(&mut self, args: &Value) -> Result<Vec<Value>> {
        let region = numbers(args, "region", 4)?.context("\"region\" is required: [x0, y0, x1, y1]")?;
        let (x0, y0, x1, y1) = (region[0], region[1], region[2], region[3]);
        if x1 <= x0 || y1 <= y0 {
            bail!("\"region\" must be [x0, y0, x1, y1] with x1 > x0 and y1 > y0");
        }
        let target = self.target(args)?;
        let mut vnc = crate::connect_vnc(&target, true)?;
        let (w, h, rgb) = vnc.capture()?;
        let geo = Geo::new(w, h, self.max);
        geo.check(x0, y0)?;
        geo.check(x1, y1)?;
        let (nx0, ny0) = geo.to_native(x0, y0);
        let nx1 = ((x1 * w as f64 / geo.sw as f64).round() as u32).clamp(nx0 as u32 + 1, w);
        let ny1 = ((y1 * h as f64 / geo.sh as f64).round() as u32).clamp(ny0 as u32 + 1, h);
        let (cw, ch) = (nx1 - nx0 as u32, ny1 - ny0 as u32);
        let mut crop = Vec::with_capacity((cw * ch * 3) as usize);
        for y in ny0 as u32..ny1 {
            let start = ((y * w + nx0 as u32) * 3) as usize;
            crop.extend_from_slice(&rgb[start..start + cw as usize * 3]);
        }
        let fit = Geo::new(cw, ch, self.max);
        let image = downscale(cw, ch, &crop, fit.sw, fit.sh);
        let png = vnc::encode_png(fit.sw, fit.sh, &image, png::Compression::Default)?;
        let shown = if (fit.sw, fit.sh) != (cw, ch) { format!(", shown at {}x{}", fit.sw, fit.sh) } else { String::new() };
        let note = format!(
            "Zoom of screenshot region [{x0},{y0},{x1},{y1}] at {cw}x{ch} (native pixels{shown}). Coordinates inside this image are NOT click coordinates; keep using full-screenshot coordinates."
        );
        Ok(vec![json!({"type": "image", "data": base64(&png), "mimeType": "image/png"}), text(note)])
    }

    fn click(&mut self, args: &Value, button: u8, count: u32, label: &str) -> Result<Vec<Value>> {
        let (x, y) = coord(args, "coordinate")?.context("\"coordinate\" is required: [x, y]")?;
        let modifiers = keys::parse_modifiers(&string(args, "modifiers")?.unwrap_or_default())?;
        let name = self.vm_name(args, "vm")?;
        self.not_held(&name)?;
        let (target, mut vnc) = self.input(args)?;
        let geo = self.geo(&vnc);
        geo.check(x, y)?;
        let (nx, ny) = geo.to_native(x, y);
        record::note(&target, if button == vnc::BUTTON_RIGHT { "right" } else { "click" }, nx, ny);
        keys::modifiers_down(&mut vnc, &modifiers, true)?;
        vnc.click(nx, ny, button, count)?;
        keys::modifiers_down(&mut vnc, &modifiers, false)?;
        vnc.finish()?;
        self.pointer.insert(target.id, (nx, ny));
        Ok(vec![text(format!("{label} at ({x}, {y})."))])
    }

    fn mouse_move(&mut self, args: &Value) -> Result<Vec<Value>> {
        let (x, y) = coord(args, "coordinate")?.context("\"coordinate\" is required: [x, y]")?;
        let name = self.vm_name(args, "vm")?;
        if self.is_held(&name) {
            let target = self.target(args)?;
            if host::status(&target.id)?["controller"] == "human" {
                return Err(self.human(&target.id));
            }
            let from = self.pointer.get(&name).copied().unwrap_or((0, 0));
            let Some((_, vnc)) = self.held.as_mut() else { unreachable!() };
            let geo = Geo::new(vnc.width as u32, vnc.height as u32, self.max);
            geo.check(x, y)?;
            let to = geo.to_native(x, y);
            let steps = 10;
            for i in 1..=steps {
                let px = from.0 as i32 + (to.0 as i32 - from.0 as i32) * i / steps;
                let py = from.1 as i32 + (to.1 as i32 - from.1 as i32) * i / steps;
                vnc.pointer(vnc::BUTTON_LEFT, px as u16, py as u16)?;
                vnc::pause(15);
            }
            record::note(&target, "move", to.0, to.1);
            self.pointer.insert(name, to);
            return Ok(vec![text(format!("Moved the pointer to ({x}, {y}) with the left button held."))]);
        }
        let (target, mut vnc) = self.input(args)?;
        let geo = self.geo(&vnc);
        geo.check(x, y)?;
        let (nx, ny) = geo.to_native(x, y);
        record::note(&target, "move", nx, ny);
        vnc.pointer(0, nx, ny)?;
        vnc.finish()?;
        self.pointer.insert(target.id, (nx, ny));
        Ok(vec![text(format!("Moved the pointer to ({x}, {y})."))])
    }

    fn drag(&mut self, args: &Value) -> Result<Vec<Value>> {
        let (x, y) = coord(args, "coordinate")?.context("\"coordinate\" is required: [x, y] (the end point)")?;
        let start = coord(args, "start_coordinate")?;
        let name = self.vm_name(args, "vm")?;
        self.not_held(&name)?;
        if start.is_none() && !self.pointer.contains_key(&name) {
            bail!("pointer position unknown; pass start_coordinate");
        }
        let (target, mut vnc) = self.input(args)?;
        let geo = self.geo(&vnc);
        geo.check(x, y)?;
        let from = match start {
            Some((sx, sy)) => {
                geo.check(sx, sy)?;
                geo.to_native(sx, sy)
            }
            None => self.pointer[&name],
        };
        let to = geo.to_native(x, y);
        record::note(&target, "click", from.0, from.1);
        vnc.drag(from, to)?;
        record::note(&target, "click", to.0, to.1);
        vnc.finish()?;
        self.pointer.insert(target.id, to);
        let (fx, fy) = geo.to_screen(from.0, from.1);
        Ok(vec![text(format!("Dragged from ({fx}, {fy}) to ({x}, {y})."))])
    }

    fn mouse_down(&mut self, args: &Value) -> Result<Vec<Value>> {
        let name = self.vm_name(args, "vm")?;
        if self.held.is_some() {
            bail!("The left button is already held; call left_mouse_up first.");
        }
        let Some(&(nx, ny)) = self.pointer.get(&name) else {
            bail!("pointer position unknown; call mouse_move first");
        };
        let (target, mut vnc) = self.input(args)?;
        record::note(&target, "click", nx, ny);
        vnc.pointer(vnc::BUTTON_LEFT, nx, ny)?;
        let (x, y) = self.geo(&vnc).to_screen(nx, ny);
        self.held = Some((target.id, vnc));
        Ok(vec![text(format!("Left button down at ({x}, {y}). Move with mouse_move, then call left_mouse_up."))])
    }

    fn mouse_up(&mut self, args: &Value) -> Result<Vec<Value>> {
        let name = self.vm_name(args, "vm")?;
        let current = self.pointer.get(&name).copied();
        if self.is_held(&name) {
            let Some((_, mut vnc)) = self.held.take() else { unreachable!() };
            let (nx, ny) = current.unwrap_or((0, 0));
            vnc.pointer(0, nx, ny)?;
            let (x, y) = self.geo(&vnc).to_screen(nx, ny);
            vnc.finish()?;
            return Ok(vec![text(format!("Left button released at ({x}, {y})."))]);
        }
        let (_, mut vnc) = self.input(args)?;
        if let Some((nx, ny)) = current {
            vnc.pointer(0, nx, ny)?;
        }
        vnc.finish()?;
        Ok(vec![text("Left button released.".into())])
    }

    fn scroll(&mut self, args: &Value) -> Result<Vec<Value>> {
        let (x, y) = coord(args, "coordinate")?.context("\"coordinate\" is required: [x, y]")?;
        let direction = string(args, "scroll_direction")?.context("\"scroll_direction\" is required: up, down, left or right")?;
        let n = int(args, "scroll_amount", 1, 50, 3)? as i32;
        let (dx, dy) = match direction.to_lowercase().as_str() {
            "up" => (0, -n),
            "down" => (0, n),
            "left" => (-n, 0),
            "right" => (n, 0),
            other => bail!("unknown scroll_direction '{other}'; use up, down, left or right"),
        };
        let modifiers = keys::parse_modifiers(&string(args, "modifiers")?.unwrap_or_default())?;
        let name = self.vm_name(args, "vm")?;
        self.not_held(&name)?;
        let (target, mut vnc) = self.input(args)?;
        let geo = self.geo(&vnc);
        geo.check(x, y)?;
        let (nx, ny) = geo.to_native(x, y);
        record::note(&target, "move", nx, ny);
        keys::modifiers_down(&mut vnc, &modifiers, true)?;
        vnc.scroll(&target.id, nx, ny, dx, dy)?;
        keys::modifiers_down(&mut vnc, &modifiers, false)?;
        vnc.finish()?;
        self.pointer.insert(target.id, (nx, ny));
        Ok(vec![text(format!("Scrolled {} {n} ticks at ({x}, {y}).", direction.to_lowercase()))])
    }

    fn vm_up(&mut self, args: &Value) -> Result<Vec<Value>> {
        let name = self.vm_name(args, "name")?;
        let image = string(args, "image")?.unwrap_or_else(|| "base".into());
        machine::validate_name(&image)?;
        let mode = string(args, "workspace_mode")?.unwrap_or_else(|| "clone".into());
        let mode = ShareMode::from_str(&mode, true).map_err(|_| anyhow!("unknown workspace_mode '{mode}'; use clone, rw, ro or none"))?;
        let view = boolean(args, "view")?;
        let timeout = int(args, "timeout_seconds", 1, 600, 50)?;
        let existed = paths::vm(&name).join("config.json").exists();
        let (target, spec) = match crate::start_vm(&name, &image, None, mode, None, None, view) {
            Ok(started) => started,
            Err(err) => {
                let running = crate::vm_rows().unwrap_or_default().iter().filter(|r| r["state"] != "stopped").count();
                if running >= 2 {
                    bail!("{err:#} At most 2 macOS VMs can run at once; stop one with vm_down.");
                }
                return Err(err);
            }
        };
        if !existed {
            self.created.insert(name.clone());
        }
        self.default_vm = Some(name.clone());
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let ready = machine::wait_for_ip(&spec, deadline).and_then(|ip| machine::wait_for_ssh(&spec, &ip, deadline).map(|_| ip));
        match ready {
            Ok(ip) => {
                let mut value = crate::describe(&target)?;
                value["ip"] = json!(ip);
                if let Some(obj) = value.as_object_mut() {
                    obj.remove("ok");
                }
                Ok(vec![text(format!("{}\nVM '{name}' is up and is now the default VM.", serde_json::to_string_pretty(&value)?))])
            }
            Err(err) => {
                if host::status(&name)?["state"] == "running" {
                    Ok(vec![text(format!("VM '{name}' is still booting. Call vm_up with the same name again to keep waiting; GUI tools may already work."))])
                } else {
                    Err(err)
                }
            }
        }
    }

    fn vm_down(&mut self, args: &Value) -> Result<Vec<Value>> {
        let name = self.vm_name(args, "name")?;
        let remove = boolean(args, "remove")?;
        if remove && !self.created.contains(&name) {
            bail!("VM '{name}' was not created in this session, so it was not removed. Ask the user to run `shovel down {name} --rm` if they want it deleted.");
        }
        let target = Target::resolve(&name)?;
        self.release_held(&name);
        crate::down(&target, remove)?;
        self.pointer.remove(&name);
        if remove {
            crate::remove_vm(&name)?;
            self.created.remove(&name);
            if self.default_vm.as_deref() == Some(name.as_str()) {
                self.default_vm = None;
            }
        }
        Ok(vec![text(format!("VM '{name}' stopped{}.", if remove { " and removed" } else { "" }))])
    }
}

impl Geo {
    fn new(w: u32, h: u32, max: (u32, u32)) -> Geo {
        let s = (max.0 as f64 / w as f64).min(max.1 as f64 / h as f64).min(1.0);
        let sw = ((w as f64 * s).round() as u32).max(1);
        let sh = ((h as f64 * s).round() as u32).max(1);
        Geo { w, h, sw, sh }
    }

    fn check(&self, x: f64, y: f64) -> Result<()> {
        if !(0.0..=self.sw as f64).contains(&x) || !(0.0..=self.sh as f64).contains(&y) {
            bail!("Coordinate ({x}, {y}) is outside the screen, which is {}x{} in screenshot pixels.", self.sw, self.sh);
        }
        Ok(())
    }

    fn to_native(self, x: f64, y: f64) -> (u16, u16) {
        let nx = ((x * self.w as f64 / self.sw as f64).round() as u32).min(self.w - 1);
        let ny = ((y * self.h as f64 / self.sh as f64).round() as u32).min(self.h - 1);
        (nx as u16, ny as u16)
    }

    fn to_screen(self, nx: u16, ny: u16) -> (u32, u32) {
        let x = (nx as f64 * self.sw as f64 / self.w as f64).round() as u32;
        let y = (ny as f64 * self.sh as f64 / self.h as f64).round() as u32;
        (x, y)
    }
}

fn downscale(w: u32, h: u32, rgb: &[u8], dw: u32, dh: u32) -> Vec<u8> {
    if (dw, dh) == (w, h) {
        return rgb.to_vec();
    }
    let (w, h, dw, dh) = (w as usize, h as usize, dw as usize, dh as usize);
    let mut out = Vec::with_capacity(dw * dh * 3);
    for dy in 0..dh {
        let y0 = dy * h / dh;
        let y1 = ((dy + 1) * h / dh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx * w / dw;
            let x1 = ((dx + 1) * w / dw).max(x0 + 1);
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 3;
                    sum[0] += rgb[i] as u32;
                    sum[1] += rgb[i + 1] as u32;
                    sum[2] += rgb[i + 2] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
    }
    out
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn is_bundle_id(app: &str) -> bool {
    let parts: Vec<&str> = app.split('.').collect();
    parts.len() >= 2 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

fn run(vm: &str, command: &str, timeout: u64, input: Option<&[u8]>) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let (spec, ip) = crate::reachable(vm)?;
    let mut child = machine::ssh_command(&spec, &ip)
        .args(["-o", "BatchMode=yes"])
        .arg(format!("/bin/zsh -lc {}", machine::shell_quote(command)))
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(data)?;
    }
    let pipe = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stream {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = pipe(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let stderr = pipe(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Timed out after {timeout} s (the command may still be running in the VM). For long jobs run them in the background, e.g. \"nohup make > /tmp/build.log 2>&1 &\", and poll.");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), out, err))
}

fn tail(bytes: &[u8]) -> String {
    const LIMIT: usize = 40_000;
    let text = String::from_utf8_lossy(bytes);
    let count = text.chars().count();
    if count <= LIMIT {
        return text.into_owned();
    }
    let start = text.char_indices().nth(count - LIMIT).map(|(i, _)| i).unwrap_or(0);
    format!("[...truncated {start} bytes...]{}", &text[start..])
}

fn check_arguments(tool: &Value, args: &Value) -> Result<()> {
    let properties = tool["inputSchema"]["properties"].as_object().cloned().unwrap_or_default();
    let unknown: Vec<&String> = args.as_object().into_iter().flatten().map(|(k, _)| k).filter(|k| !properties.contains_key(*k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let names = |keys: Vec<&String>| keys.iter().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(", ");
    let valid = if properties.is_empty() { "none".to_string() } else { names(properties.keys().collect()) };
    bail!("Unknown argument {} for {}; valid arguments: {valid}.", names(unknown), tool["name"].as_str().unwrap_or("?"))
}

fn text(s: String) -> Value {
    json!({"type": "text", "text": s})
}

fn lenient(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s.trim()).unwrap_or_else(|_| v.clone()),
        other => other.clone(),
    }
}

fn string(args: &Value, key: &str) -> Result<Option<String>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => bail!("\"{key}\" must be a string"),
    }
}

fn number(args: &Value, key: &str) -> Result<Option<f64>> {
    match lenient(&args[key]) {
        Value::Null => Ok(None),
        v => v.as_f64().map(Some).with_context(|| format!("\"{key}\" must be a number")),
    }
}

fn int(args: &Value, key: &str, min: u64, max: u64, default: u64) -> Result<u64> {
    match number(args, key)? {
        None => Ok(default),
        Some(n) if n.fract() == 0.0 && n >= min as f64 && n <= max as f64 => Ok(n as u64),
        Some(_) => bail!("\"{key}\" must be an integer from {min} to {max}"),
    }
}

fn boolean(args: &Value, key: &str) -> Result<bool> {
    match lenient(&args[key]) {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(b),
        _ => bail!("\"{key}\" must be true or false"),
    }
}

fn numbers(args: &Value, key: &str, len: usize) -> Result<Option<Vec<f64>>> {
    match lenient(&args[key]) {
        Value::Null => Ok(None),
        Value::Array(items) if items.len() == len => {
            items.iter().map(|v| lenient(v).as_f64()).collect::<Option<Vec<f64>>>().map(Some).with_context(|| format!("\"{key}\" must contain only numbers"))
        }
        _ => bail!("\"{key}\" must be an array of {len} numbers"),
    }
}

fn coord(args: &Value, key: &str) -> Result<Option<(f64, f64)>> {
    Ok(numbers(args, key, 2)?.map(|v| (v[0], v[1])))
}

fn tool(name: &str, title: &str, description: &str, mut properties: Value, required: &[&str], vm: bool) -> Value {
    if vm {
        properties["vm"] = json!({"type": "string", "description": "VM name. Optional when a default VM is bound or was started with vm_up."});
    }
    let read_only = matches!(name, "screenshot" | "zoom" | "cursor_position" | "vm_list" | "wait");
    let mut annotations = json!({"title": title, "readOnlyHint": read_only, "openWorldHint": false});
    if name == "vm_down" {
        annotations["destructiveHint"] = json!(true);
    }
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required, "additionalProperties": false},
        "annotations": annotations,
    })
}

fn tools() -> Vec<Value> {
    let coord = json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "[x, y] in pixels of the most recent screenshot, origin top-left."});
    let mods = json!({"type": "string", "description": "Modifier keys to hold during the action, joined with '+', e.g. \"cmd\", \"shift\", \"cmd+shift\". Names: cmd, shift, ctrl, option (alt)."});
    let click = |name: &str, title: &str, description: &str| {
        tool(name, title, description, json!({"coordinate": coord, "modifiers": mods}), &["coordinate"], true)
    };
    let seconds = |max: u32| json!({"type": "integer", "minimum": 1, "maximum": max, "default": 50, "description": "Seconds to wait before returning"});
    vec![
        tool("screenshot", "Screenshot", "Capture the VM screen as a PNG image (scaled to at most 1280x800). Pixel coordinates in this image are the coordinates every mouse tool takes. Take one before acting and after each action you need to verify.", json!({}), &[], true),
        tool("zoom", "Zoom", "Show a region of the screen at higher (native) resolution to read small text or icons. Read-only; its pixels are not click coordinates.", json!({"region": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4, "description": "[x0, y0, x1, y1] rectangle in full-screenshot pixels"}}), &["region"], true),
        click("left_click", "Left click", "Left-click at a point of the latest screenshot, optionally with modifier keys held (e.g. cmd-click)."),
        click("right_click", "Right click", "Right-click at a point, which opens the context menu there."),
        click("middle_click", "Middle click", "Middle-click at a point."),
        click("double_click", "Double click", "Double-click at a point. Selects a word / opens an item in Finder."),
        click("triple_click", "Triple click", "Triple-click at a point. Selects a line or paragraph."),
        tool("mouse_move", "Move mouse", "Move the pointer without clicking, e.g. to hover. While the left button is held (left_mouse_down) it drags.", json!({"coordinate": coord}), &["coordinate"], true),
        tool("left_click_drag", "Drag", "Press the left button at start_coordinate (default: the current pointer), drag to coordinate and release. Moves windows, selects text, drags files.", json!({"coordinate": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "End point [x, y] in pixels of the most recent screenshot."}, "start_coordinate": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "Start point [x, y]; defaults to the current pointer position."}}), &["coordinate"], true),
        tool("left_mouse_down", "Left button down", "Press and hold the left button at the current pointer (place it with mouse_move first). Follow with mouse_move calls and left_mouse_up.", json!({}), &[], true),
        tool("left_mouse_up", "Left button up", "Release the left button at the current pointer.", json!({}), &[], true),
        tool("scroll", "Scroll", "Scroll with the mouse wheel at a point (the pointer moves there first), scrolling whatever view is under it. One tick is one wheel notch, roughly 10 lines of text; take a screenshot to see where it landed.", json!({"coordinate": coord, "scroll_direction": {"type": "string", "enum": ["up", "down", "left", "right"]}, "scroll_amount": {"type": "integer", "minimum": 1, "maximum": 50, "default": 3, "description": "Wheel ticks (notches)"}, "modifiers": mods}), &["coordinate", "scroll_direction"], true),
        tool("type", "Type text", "Type text with the keyboard into the focused element.", json!({"text": {"type": "string", "description": "Text to type literally into the focused element. \\n presses Return, \\t presses Tab. For shortcuts use key. Non-ASCII characters (é, ✓, emoji) are pasted through the VM clipboard."}}), &["text"], true),
        tool("key", "Press keys", "Press a key or key combination (shortcut), or a sequence of them.", json!({"keys": {"type": "string", "description": "One key or chord, or several separated by spaces to press in order, e.g. \"cmd+a\", \"return\", \"cmd+space\", \"escape tab tab return\". Keys: return, tab, escape, backspace (Mac delete), forwarddelete, space, up, down, left, right, home, end, pageup, pagedown, f1-f20, or one character. Modifiers: cmd, shift, ctrl, option."}, "repeat": {"type": "integer", "minimum": 1, "maximum": 100, "default": 1, "description": "How many times to press the whole sequence"}}), &["keys"], true),
        tool("hold_key", "Hold key", "Hold a key or chord down for a duration, then release it.", json!({"keys": {"type": "string", "description": "One key or chord, e.g. \"shift\" or \"cmd+tab\""}, "duration_seconds": {"type": "number", "minimum": 0, "maximum": 10}}), &["keys", "duration_seconds"], true),
        tool("cursor_position", "Cursor position", "Report the pointer position in screenshot pixels. This is the last position set by this server's mouse tools; the VM does not report moves made by a human.", json!({}), &[], true),
        tool("wait", "Wait", "Wait for a number of seconds, e.g. for an app to launch or a page to load, then take a screenshot.", json!({"duration_seconds": {"type": "number", "minimum": 0, "maximum": 60}}), &["duration_seconds"], false),
        tool("exec", "Run shell command", "Run a shell command in the VM and return its exit code, stdout and stderr. Use it for anything scriptable: files, builds, installs, `open <url-or-path>`. Do not use osascript/AppleScript (it can hang on a permission prompt); use open_application or the GUI.", json!({"command": {"type": "string", "description": "zsh command line run in a login shell over SSH as the VM user (passwordless sudo, Homebrew on PATH), e.g. \"cd ~/workspace && ls\""}, "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600, "default": 50, "description": "Kill the command after this many seconds"}}), &["command"], true),
        tool("open_application", "Open application", "Launch or bring an app to the front in the VM. For URLs and files use exec with `open <url-or-path>`.", json!({"app": {"type": "string", "description": "App name like \"TextEdit\" or bundle id like \"com.apple.TextEdit\""}}), &["app"], true),
        tool("type_password", "Type password", "Type the VM user's password into the focused field (login window, sudo or authorization dialog) without revealing it. Click the password field first. Never ask the user for the password.", json!({"press_enter": {"type": "boolean", "default": false, "description": "Press Return after typing"}}), &[], true),
        tool("wait_for_control", "Wait for control", "Wait until a human who took over the VM hands control back to you. Returns early when you have control; take a screenshot afterwards.", json!({"timeout_seconds": seconds(3600)}), &[], true),
        tool("vm_view", "Show VM to user", "Show the VM in a window on the user's Mac. Use it when you need the user to look at or do something in the VM (e.g. sign in), then call wait_for_control.", json!({}), &[], true),
        tool("vm_list", "List VMs", "List the Shovel VMs with their state, controller and IP, and which one is the default.", json!({}), &[], false),
        tool("vm_up", "Start VM", "Create (if new) and boot a macOS VM cloned from a golden image, wait until it is ready, and make it the default VM for the other tools. If it returns \"still booting\", call it again with the same name.", json!({
            "name": {"type": "string", "description": "VM name (letters, digits, '-', '_'), e.g. \"agent-1\"; optional if a default VM is bound"},
            "image": {"type": "string", "default": "base", "description": "golden image to clone"},
            "workspace_mode": {"type": "string", "enum": ["clone", "rw", "ro", "none"], "default": "clone", "description": "clone = private copy-on-write copy of the host ~/workspace at ~/workspace in the guest; rw/ro = live mount; none = no workspace"},
            "view": {"type": "boolean", "default": false, "description": "Also open a live view window on the user's Mac"},
            "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600, "default": 50, "description": "Seconds to wait for the VM to be ready"},
        }), &[], false),
        tool("vm_down", "Stop VM", "Shut a VM down; with remove=true also delete it (only VMs created by vm_up in this session).", json!({
            "name": {"type": "string", "description": "VM name; defaults to the default VM"},
            "remove": {"type": "boolean", "default": false, "description": "also delete the VM; only allowed for VMs created by vm_up in this session"},
        }), &[], false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        for (input, expected) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
    }

    #[test]
    fn scaling() {
        let geo = Geo::new(1440, 900, (1280, 800));
        assert_eq!((geo.sw, geo.sh), (1280, 800));
        assert_eq!(geo.to_native(640.0, 400.0), (720, 450));
        assert_eq!(geo.to_native(1280.0, 800.0), (1439, 899));
        assert_eq!(geo.to_screen(720, 450), (640, 400));
        let small = Geo::new(1024, 768, (1280, 800));
        assert_eq!((small.sw, small.sh), (1024, 768));
        assert!(geo.check(1280.5, 10.0).is_err());
    }

    #[test]
    fn box_filter() {
        let rgb = vec![0, 0, 0, 255, 255, 255, 100, 100, 100, 100, 100, 100];
        assert_eq!(downscale(2, 2, &rgb, 1, 1), vec![114, 114, 114]);
    }

    #[test]
    fn versions() {
        assert_eq!(negotiate(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate(Some("1999-01-01")), "2025-11-25");
        assert_eq!(negotiate(None), "2025-11-25");
    }

    #[test]
    fn combos() {
        assert_eq!(keys::parse_combo("Page_Down").unwrap().1, 0xff56);
        assert_eq!(keys::parse_combo("BackSpace").unwrap().1, 0xff08);
        assert_eq!(keys::parse_combo("KP_Enter").unwrap().1, 0xff0d);
        assert_eq!(keys::parse_combo("cmd+shift+a").unwrap(), (vec![0xffe9, 0xffe1], 'a' as u32));
        assert_eq!(keys::parse_combo("+").unwrap().1, '+' as u32);
        assert_eq!(keys::parse_combo("ctrl+-").unwrap(), (vec![0xffe3], '-' as u32));
        assert_eq!(keys::parse_combo("cmd++").unwrap(), (vec![0xffe9], '+' as u32));
        assert!(keys::parse_modifiers("cmd+hyper").is_err());
        assert_eq!(keys::parse_modifiers(" cmd + Shift ").unwrap(), vec![0xffe9, 0xffe1]);
    }

    #[test]
    fn unknown_arguments() {
        let wait = tools().into_iter().find(|t| t["name"] == "wait").unwrap();
        assert!(check_arguments(&wait, &json!({"duration_seconds": 1})).is_ok());
        let err = check_arguments(&wait, &json!({"seconds": "1"})).unwrap_err().to_string();
        assert!(err.contains("\"seconds\"") && err.contains("\"duration_seconds\""));
    }

    #[test]
    fn tool_list() {
        let list = tools();
        assert_eq!(list.len(), 25);
        for t in &list {
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }
}
