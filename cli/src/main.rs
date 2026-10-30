mod github;
mod host;
mod keys;
mod machine;
mod password;
mod paths;
mod record;
mod vnc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use machine::{Display, Share, Spec, Target};

#[derive(Parser)]
#[command(name = "shovel", version, about = "Disposable macOS VMs for agents")]
struct Cli {
    #[arg(long, global = true, help = "Print machine-readable JSON")]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum ShareMode {
    Clone,
    Rw,
    Ro,
    None,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = "Manage golden macOS images")]
    Image {
        #[command(subcommand)]
        command: ImageCmd,
    },
    #[command(about = "Create (if needed) and boot a VM, then wait until SSH is ready")]
    Up {
        name: String,
        #[arg(long, default_value = "base", help = "Golden image to clone from")]
        from: String,
        #[arg(long, help = "Host directory to expose as the guest workspace [default: ~/workspace]")]
        share: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "clone", help = "clone = private copy-on-write snapshot, rw/ro = live mount")]
        share_mode: ShareMode,
        #[arg(long)]
        cpus: Option<u32>,
        #[arg(long)]
        memory_gb: Option<u64>,
        #[arg(long, help = "Open the live view window")]
        view: bool,
        #[arg(long, help = "Return as soon as the VM is booting")]
        no_wait: bool,
        #[arg(long, default_value_t = 300, help = "Seconds to wait for SSH")]
        timeout: u64,
    },
    #[command(about = "Shut a VM down")]
    Down {
        name: String,
        #[arg(long, help = "Delete the VM after stopping it")]
        rm: bool,
        #[arg(long, help = "Power off immediately")]
        force: bool,
    },
    #[command(about = "Delete a stopped VM and its workspace clone")]
    Rm {
        name: String,
        #[arg(long, help = "Stop it first if running")]
        force: bool,
    },
    #[command(about = "List VMs")]
    Ls,
    #[command(about = "Show VM state, controller and IP")]
    Status { name: String },
    #[command(about = "Print a VM's IP address")]
    Ip { name: String },
    #[command(about = "Run a shell command in the VM (login zsh), e.g. shovel exec t1 -- 'cd ~/workspace && ls'")]
    Exec {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    #[command(about = "Open an interactive SSH session")]
    Ssh { name: String },
    #[command(about = "Copy files; prefix the VM side with NAME:, e.g. shovel cp ./App.app t1:/Applications/")]
    Cp { src: String, dst: String },
    #[command(about = "Save a PNG of the VM screen and print its path")]
    Screenshot {
        name: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    #[command(about = "Click at screen pixel coordinates")]
    Click {
        name: String,
        x: u16,
        y: u16,
        #[arg(long)]
        right: bool,
        #[arg(long)]
        middle: bool,
        #[arg(long)]
        double: bool,
        #[arg(long, help = "Send input even if a human has taken over")]
        force: bool,
    },
    #[command(about = "Move the pointer")]
    Move {
        name: String,
        x: u16,
        y: u16,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Drag with the left button held")]
    Drag {
        name: String,
        x1: u16,
        y1: u16,
        x2: u16,
        y2: u16,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Scroll at a position (positive dy = down, positive dx = right)")]
    Scroll {
        name: String,
        x: u16,
        y: u16,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        dx: i32,
        #[arg(long, default_value_t = 3, allow_hyphen_values = true)]
        dy: i32,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Type text")]
    Type {
        name: String,
        text: String,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Press key combos in order, e.g. shovel key t1 cmd+space return")]
    Key {
        name: String,
        #[arg(required = true)]
        combos: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Open the live view window (view only until you take over)")]
    View { name: String },
    #[command(about = "Take control from the agent; agent GUI input is refused until release")]
    Takeover { name: String },
    #[command(about = "Hand control back to the agent")]
    Release { name: String },
    #[command(about = "Block until the agent has control (e.g. after a human takeover)")]
    WaitControl {
        name: String,
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    #[command(about = "Record the VM screen to MP4 + GIF (agent clicks are highlighted)")]
    Record {
        #[command(subcommand)]
        command: RecordCmd,
    },
    #[command(about = "Upload files (recordings, screenshots) and post them as a comment on a GitHub PR")]
    PrAttach {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[arg(long, help = "PR number or URL")]
        pr: String,
        #[arg(long, help = "owner/name [default: repo of the current directory]")]
        repo: Option<String>,
        #[arg(short, long, help = "Comment text shown above the media")]
        message: Option<String>,
        #[arg(long, help = "Print the comment without uploading")]
        dry_run: bool,
    },
    #[command(about = "Type the guest account password without revealing it, e.g. into a login or auth dialog")]
    Password {
        #[command(subcommand)]
        command: PasswordCmd,
    },
    #[command(about = "Control the Shovel host app")]
    Host {
        #[command(subcommand)]
        command: HostCmd,
    },
}

#[derive(Subcommand)]
enum ImageCmd {
    #[command(about = "Download macOS and install it into a new golden image")]
    Create {
        #[arg(default_value = "base")]
        name: String,
        #[arg(long, default_value = "latest", help = "'latest' or a path to an .ipsw file")]
        ipsw: String,
        #[arg(long, default_value_t = 100)]
        disk_gb: u64,
        #[arg(long, default_value_t = 4)]
        cpus: u32,
        #[arg(long, default_value_t = 8)]
        memory_gb: u64,
        #[arg(long, help = "Return immediately instead of following progress")]
        detach: bool,
        #[arg(long, help = "Overwrite an existing image")]
        force: bool,
    },
    #[command(about = "Boot an image with the viewer to finish one-time setup")]
    Setup {
        #[arg(default_value = "base")]
        name: String,
    },
    #[command(about = "Shut down an image that is being set up")]
    Stop {
        #[arg(default_value = "base")]
        name: String,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "List images")]
    Ls,
    #[command(about = "Delete an image")]
    Rm { name: String },
}

#[derive(Subcommand)]
enum RecordCmd {
    #[command(about = "Start recording in the background")]
    Start {
        name: String,
        #[arg(short, long, help = "Output .mp4 path (a .gif is written next to it)")]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        fps: u32,
        #[arg(long, default_value_t = 600, help = "Stop automatically after this many seconds")]
        max_seconds: u64,
    },
    #[command(about = "Stop recording and print the MP4 and GIF paths")]
    Stop { name: String },
    #[command(hide = true)]
    Worker {
        name: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        fps: u32,
        #[arg(long)]
        max_seconds: u64,
    },
}

#[derive(Subcommand)]
enum PasswordCmd {
    #[command(about = "Type the password into the focused field of a VM")]
    Type {
        name: String,
        #[arg(long, help = "Press Return afterwards")]
        enter: bool,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "Save an image's guest password in the login keychain (prompts)")]
    Set {
        #[arg(default_value = "base")]
        image: String,
    },
    #[command(about = "Forget an image's saved guest password")]
    Rm {
        #[arg(default_value = "base")]
        image: String,
    },
}

#[derive(Subcommand)]
enum HostCmd {
    Start,
    Stop,
    Status,
}

#[derive(Debug)]
struct HumanControl(String);

impl std::fmt::Display for HumanControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a human has taken over {}; wait with `shovel wait-control {}` (or pass --force)",
            self.0, self.0
        )
    }
}

impl std::error::Error for HumanControl {}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(err) = run(cli) {
        let code = if err.downcast_ref::<HumanControl>().is_some() { 3 } else { 1 };
        if json {
            println!("{}", json!({"ok": false, "error": format!("{err:#}"), "human_control": code == 3}));
        } else {
            eprintln!("shovel: {err:#}");
        }
        std::process::exit(code);
    }
}

fn emit(json: bool, value: Value, human: impl FnOnce() -> String) {
    if json {
        println!("{value}");
    } else {
        let text = human();
        if !text.is_empty() {
            println!("{text}");
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.command {
        Cmd::Image { command } => image(json, command),
        Cmd::Up { name, from, share, share_mode, cpus, memory_gb, view, no_wait, timeout } => {
            up(json, &name, &from, share, share_mode, cpus, memory_gb, view, no_wait, timeout)
        }
        Cmd::Down { name, rm, force } => {
            let target = Target::resolve(&name)?;
            down(&target, force)?;
            if rm {
                remove_vm(&name)?;
            }
            emit(json, json!({"ok": true, "name": name, "state": "stopped", "removed": rm}), || {
                format!("{name} stopped{}", if rm { " and removed" } else { "" })
            });
            Ok(())
        }
        Cmd::Rm { name, force } => {
            let target = Target::resolve(&name)?;
            if running(&target.id)? {
                if !force {
                    bail!("{name} is running; use `shovel down {name} --rm` or `shovel rm {name} --force`");
                }
                down(&target, true)?;
            }
            remove_vm(&name)?;
            emit(json, json!({"ok": true, "name": name, "removed": true}), || format!("{name} removed"));
            Ok(())
        }
        Cmd::Ls => list(json),
        Cmd::Status { name } => {
            let target = Target::resolve(&name)?;
            let value = describe(&target)?;
            emit(json, value.clone(), || {
                format!(
                    "{}: {} (control: {}, ip: {})",
                    name,
                    value["state"].as_str().unwrap_or("?"),
                    value["controller"].as_str().unwrap_or("-"),
                    value["ip"].as_str().unwrap_or("-")
                )
            });
            Ok(())
        }
        Cmd::Ip { name } => {
            let target = Target::resolve(&name)?;
            let spec = target.spec()?;
            let ip = machine::lookup_ip(&spec.mac).context("no DHCP lease yet; is the VM running?")?;
            emit(json, json!({"ok": true, "name": name, "ip": ip}), || ip.clone());
            Ok(())
        }
        Cmd::Exec { name, command } => {
            let (spec, ip) = reachable(&name)?;
            let script = command.join(" ");
            let err = machine::ssh_command(&spec, &ip)
                .arg(format!("/bin/zsh -lc {}", machine::shell_quote(&script)))
                .exec();
            Err(err.into())
        }
        Cmd::Ssh { name } => {
            let (spec, ip) = reachable(&name)?;
            let err = machine::ssh_command(&spec, &ip).arg("-t").exec();
            Err(err.into())
        }
        Cmd::Cp { src, dst } => copy(json, &src, &dst),
        Cmd::Screenshot { name, output } => {
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, true)?;
            let path = output.unwrap_or_else(|| {
                let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                target.dir.join("screenshots").join(format!("{ms}.png"))
            });
            let (w, h) = vnc.screenshot(&path)?;
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            emit(json, json!({"ok": true, "path": path, "width": w, "height": h}), || {
                path.display().to_string()
            });
            Ok(())
        }
        Cmd::Click { name, x, y, right, middle, double, force } => {
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, force)?;
            let button = if right {
                vnc::BUTTON_RIGHT
            } else if middle {
                vnc::BUTTON_MIDDLE
            } else {
                vnc::BUTTON_LEFT
            };
            record::note(&target, if right { "right" } else { "click" }, x, y);
            vnc.click(x, y, button, if double { 2 } else { 1 })?;
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Move { name, x, y, force } => {
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, force)?;
            record::note(&target, "move", x, y);
            vnc.pointer(0, x, y)?;
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Drag { name, x1, y1, x2, y2, force } => {
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, force)?;
            record::note(&target, "click", x1, y1);
            vnc.drag((x1, y1), (x2, y2))?;
            record::note(&target, "click", x2, y2);
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Scroll { name, x, y, dx, dy, force } => {
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, force)?;
            record::note(&target, "move", x, y);
            vnc.scroll(x, y, dx, dy)?;
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Type { name, text, force } => {
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, force)?;
            keys::type_text(&mut vnc, &text)?;
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Key { name, combos, force } => {
            for combo in &combos {
                keys::parse_combo(combo)?;
            }
            let target = Target::resolve(&name)?;
            let mut vnc = connect_vnc(&target, force)?;
            for combo in &combos {
                keys::press(&mut vnc, combo)?;
            }
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Password { command } => password_cmd(json, command),
        Cmd::View { name } => {
            let target = Target::resolve(&name)?;
            ensure_running(&target)?;
            host::request(json!({"cmd": "view", "id": target.id}))?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        Cmd::Takeover { name } => set_controller(json, &name, "human"),
        Cmd::Release { name } => set_controller(json, &name, "agent"),
        Cmd::WaitControl { name, timeout } => {
            let target = Target::resolve(&name)?;
            let deadline = Instant::now() + Duration::from_secs(timeout);
            loop {
                let status = host::status(&target.id)?;
                if status["state"] != "running" {
                    bail!("{name} is not running");
                }
                if status["controller"] == "agent" {
                    break;
                }
                if Instant::now() >= deadline {
                    bail!("timed out waiting for the human to release {name}");
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            emit(json, json!({"ok": true, "controller": "agent"}), || "agent has control".into());
            Ok(())
        }
        Cmd::Record { command } => match command {
            RecordCmd::Start { name, output, fps, max_seconds } => {
                let target = Target::resolve(&name)?;
                let state = record::start(&target, output, fps, max_seconds)?;
                emit(json, json!({"ok": true, "recording": true, "mp4": state["mp4"], "gif": state["gif"]}), || {
                    format!("Recording {name}. Stop with `shovel record stop {name}`.")
                });
                Ok(())
            }
            RecordCmd::Stop { name } => {
                let target = Target::resolve(&name)?;
                let mut result = record::stop(&target)?;
                result["ok"] = json!(true);
                emit(json, result.clone(), || {
                    format!(
                        "{}\n{}\n({:.1}s)",
                        result["mp4"].as_str().unwrap_or(""),
                        result["gif"].as_str().unwrap_or(""),
                        result["seconds"].as_f64().unwrap_or(0.0)
                    )
                });
                Ok(())
            }
            RecordCmd::Worker { name, output, fps, max_seconds } => {
                let target = Target::resolve(&name)?;
                record::worker(&target, &output, fps, max_seconds)
            }
        },
        Cmd::PrAttach { files, pr, repo, message, dry_run } => {
            let result = github::attach(&files, &pr, repo, message, dry_run)?;
            emit(json, result.clone(), || {
                if dry_run {
                    result["body"].as_str().unwrap_or("").to_string()
                } else {
                    result["comment"].as_str().unwrap_or("").to_string()
                }
            });
            Ok(())
        }
        Cmd::Host { command } => match command {
            HostCmd::Start => {
                let reply = host::request(json!({"cmd": "ping"}))?;
                emit(json, reply.clone(), || format!("Shovel host running (pid {})", reply["pid"]));
                Ok(())
            }
            HostCmd::Stop => {
                host::request_if_running(json!({"cmd": "quit"}))?;
                emit(json, json!({"ok": true}), || "Shovel host stopped".into());
                Ok(())
            }
            HostCmd::Status => {
                let reply = host::request_if_running(json!({"cmd": "list"}))?;
                let value = match &reply {
                    Some(r) => json!({"ok": true, "running": true, "vms": r["vms"], "jobs": r["jobs"]}),
                    None => json!({"ok": true, "running": false}),
                };
                emit(json, value, || match reply {
                    Some(r) => format!(
                        "Shovel host running, {} VM(s) active",
                        r["vms"].as_array().map_or(0, |v| v.len())
                    ),
                    None => "Shovel host not running".into(),
                });
                Ok(())
            }
        },
    }
}

fn running(id: &str) -> Result<bool> {
    Ok(host::status(id)?["state"] != "stopped")
}

fn describe(target: &Target) -> Result<Value> {
    let spec = target.spec()?;
    let status = host::status(&target.id)?;
    let state = status["state"].as_str().unwrap_or("stopped").to_string();
    let ip = if state == "running" { machine::lookup_ip(&spec.mac) } else { None };
    Ok(json!({
        "ok": true,
        "name": target.id,
        "state": state,
        "controller": status.get("controller").cloned().unwrap_or(Value::Null),
        "viewer_open": status.get("viewer_open").cloned().unwrap_or(Value::Bool(false)),
        "ip": ip,
        "user": spec.user,
        "cpus": spec.cpus,
        "memory_mb": spec.memory_mb,
        "display": {"width": spec.display.width, "height": spec.display.height},
        "image": spec.image,
        "workspace_source": spec.workspace_source,
        "workspace_mode": spec.workspace_mode,
        "dir": target.dir,
    }))
}

fn ensure_running(target: &Target) -> Result<()> {
    if !running(&target.id)? {
        bail!("{} is not running; start it with `shovel up {}`", target.id, target.id);
    }
    Ok(())
}

fn reachable(name: &str) -> Result<(Spec, String)> {
    let target = Target::resolve(name)?;
    ensure_running(&target)?;
    let spec = target.spec()?;
    let ip = machine::lookup_ip(&spec.mac).context("the VM has no IP address yet; wait for `shovel up` to finish")?;
    Ok((spec, ip))
}

fn connect_vnc(target: &Target, force: bool) -> Result<vnc::Vnc> {
    let status = host::status(&target.id)?;
    if status["state"] != "running" {
        bail!("{} is not running; start it with `shovel up {}`", target.id, target.id);
    }
    if status["controller"] == "human" && !force {
        return Err(HumanControl(target.id.clone()).into());
    }
    let port = status["vnc_port"].as_u64().unwrap_or(0) as u16;
    let password = status["vnc_password"].as_str().unwrap_or("");
    vnc::Vnc::connect(port, password)
}

fn set_controller(json: bool, name: &str, controller: &str) -> Result<()> {
    let target = Target::resolve(name)?;
    ensure_running(&target)?;
    host::request(json!({"cmd": "control", "id": target.id, "controller": controller}))?;
    emit(json, json!({"ok": true, "controller": controller}), || {
        if controller == "human" {
            format!("You have control of {name}. Agent GUI input is paused until `shovel release {name}`.")
        } else {
            format!("Agent has control of {name}.")
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn up(
    json: bool,
    name: &str,
    from: &str,
    share: Option<PathBuf>,
    share_mode: ShareMode,
    cpus: Option<u32>,
    memory_gb: Option<u64>,
    view: bool,
    no_wait: bool,
    timeout: u64,
) -> Result<()> {
    let (target, spec) = start_vm(name, from, share, share_mode, cpus, memory_gb, view)?;
    let mut ip = None;
    if !no_wait {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        if !json {
            eprintln!("Waiting for {name} to boot...");
        }
        let addr = machine::wait_for_ip(&spec, deadline)?;
        machine::wait_for_ssh(&spec, &addr, deadline)?;
        ip = Some(addr);
    }
    let mut value = describe(&target)?;
    if ip.is_some() {
        value["ip"] = json!(ip);
    }
    emit(json, value.clone(), || {
        format!(
            "{name} is up (ip: {}, user: {})\n  shovel exec {name} -- <command>\n  shovel screenshot {name}\n  shovel view {name}",
            value["ip"].as_str().unwrap_or("pending"),
            spec.user
        )
    });
    Ok(())
}

fn start_vm(
    name: &str,
    from: &str,
    share: Option<PathBuf>,
    share_mode: ShareMode,
    cpus: Option<u32>,
    memory_gb: Option<u64>,
    view: bool,
) -> Result<(Target, Spec)> {
    machine::validate_name(name)?;
    machine::ensure_guest_files()?;
    let dir = paths::vm(name);
    if !dir.join("config.json").exists() {
        create_vm(name, from, share, share_mode, cpus, memory_gb)?;
    } else if cpus.is_some() || memory_gb.is_some() {
        let mut spec = machine::load(&dir)?;
        if let Some(c) = cpus {
            spec.cpus = c;
        }
        if let Some(m) = memory_gb {
            spec.memory_mb = m * 1024;
        }
        machine::save(&dir, &spec)?;
    }
    let target = Target::resolve(name)?;
    let spec = target.spec()?;
    host::request(json!({"cmd": "start", "id": name, "dir": dir, "view": view}))?;
    Ok((target, spec))
}

fn create_vm(
    name: &str,
    from: &str,
    share: Option<PathBuf>,
    share_mode: ShareMode,
    cpus: Option<u32>,
    memory_gb: Option<u64>,
) -> Result<()> {
    let image_dir = paths::image(from);
    for file in ["disk.img", "aux.img", "hardware.bin", "config.json"] {
        if !image_dir.join(file).exists() {
            bail!("image '{from}' is missing {file}; create it with `shovel image create {from}`");
        }
    }
    if running(&format!("image:{from}"))? {
        bail!("image '{from}' is running (setup in progress); stop it with `shovel image stop {from}` first");
    }
    let image_spec = machine::load(&image_dir)?;
    let dir = paths::vm(name);
    std::fs::create_dir_all(&dir)?;
    let result = (|| -> Result<()> {
        for file in ["disk.img", "aux.img", "hardware.bin"] {
            machine::clone_path(&image_dir.join(file), &dir.join(file))?;
        }
        let source = share.unwrap_or_else(machine::default_workspace);
        let source = std::fs::canonicalize(&source).with_context(|| format!("share directory {}", source.display()))?;
        let mut shares = vec![Share {
            name: "shovel".into(),
            path: paths::guest().to_string_lossy().into_owned(),
            read_only: true,
        }];
        let mode = match share_mode {
            ShareMode::Clone => {
                let clone = dir.join("workspace");
                eprintln!("Cloning {} (copy-on-write)...", source.display());
                let started = Instant::now();
                machine::clone_path(&source, &clone)?;
                eprintln!("Cloned in {:.1}s", started.elapsed().as_secs_f64());
                shares.push(Share { name: "workspace".into(), path: clone.to_string_lossy().into_owned(), read_only: false });
                "clone"
            }
            ShareMode::Rw => {
                shares.push(Share { name: "workspace".into(), path: source.to_string_lossy().into_owned(), read_only: false });
                "rw"
            }
            ShareMode::Ro => {
                shares.push(Share { name: "workspace".into(), path: source.to_string_lossy().into_owned(), read_only: true });
                "ro"
            }
            ShareMode::None => "none",
        };
        let spec = Spec {
            cpus: cpus.unwrap_or(image_spec.cpus),
            memory_mb: memory_gb.map(|g| g * 1024).unwrap_or(image_spec.memory_mb),
            display: image_spec.display.clone(),
            mac: machine::random_mac()?,
            shares,
            user: image_spec.user.clone(),
            image: Some(from.to_string()),
            workspace_source: Some(source.to_string_lossy().into_owned()),
            workspace_mode: Some(mode.to_string()),
        };
        machine::save(&dir, &spec)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

fn down(target: &Target, force: bool) -> Result<()> {
    if !running(&target.id)? {
        return Ok(());
    }
    if !force {
        let spec = target.spec()?;
        if let Some(ip) = machine::lookup_ip(&spec.mac) {
            if let Ok(mut child) = machine::ssh_command(&spec, &ip)
                .args(["-o", "BatchMode=yes", "sudo -n shutdown -h now"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                let deadline = Instant::now() + Duration::from_secs(8);
                while Instant::now() < deadline {
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                let _ = child.kill();
            }
        }
    }
    let wait = if force { 0 } else { 60 };
    host::request(json!({"cmd": "stop", "id": target.id, "wait": wait}))?;
    Ok(())
}

fn remove_vm(name: &str) -> Result<()> {
    if name.starts_with("image:") {
        bail!("use `shovel image rm` to delete images");
    }
    let dir = paths::vm(name);
    if !dir.exists() {
        return Ok(());
    }
    let trash = paths::root().join("trash");
    std::fs::create_dir_all(&trash)?;
    let doomed = trash.join(format!("{name}-{}", record::now_ms()));
    std::fs::rename(&dir, &doomed).with_context(|| format!("removing {}", dir.display()))?;
    std::process::Command::new("rm")
        .arg("-rf")
        .arg(&doomed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

fn list(json: bool) -> Result<()> {
    let rows = vm_rows()?;
    emit(json, json!({"ok": true, "vms": rows}), || {
        if rows.is_empty() {
            return "No VMs. Create one with `shovel up <name>`.".into();
        }
        let mut out = format!("{:<20} {:<10} {:<8} {:<16} {}", "NAME", "STATE", "CONTROL", "IP", "WORKSPACE");
        for r in &rows {
            out.push_str(&format!(
                "\n{:<20} {:<10} {:<8} {:<16} {}",
                r["name"].as_str().unwrap_or(""),
                r["state"].as_str().unwrap_or(""),
                r["controller"].as_str().unwrap_or("-"),
                r["ip"].as_str().unwrap_or("-"),
                r["workspace_mode"].as_str().unwrap_or("-"),
            ));
        }
        out
    });
    Ok(())
}

fn vm_rows() -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let live = host::request_if_running(json!({"cmd": "list"}))?;
    let live_vms: Vec<Value> = live
        .as_ref()
        .and_then(|r| r["vms"].as_array().cloned())
        .unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(paths::vms()) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().join("config.json").exists())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let spec = machine::load(&paths::vm(&name))?;
            let status = live_vms.iter().find(|v| v["id"] == name.as_str());
            let state = status.and_then(|s| s["state"].as_str()).unwrap_or("stopped").to_string();
            let ip = if state == "running" { machine::lookup_ip(&spec.mac) } else { None };
            rows.push(json!({
                "name": name,
                "state": state,
                "controller": status.map(|s| s["controller"].clone()).unwrap_or(Value::Null),
                "ip": ip,
                "image": spec.image,
                "workspace_mode": spec.workspace_mode,
            }));
        }
    }
    for vm in &live_vms {
        if let Some(id) = vm["id"].as_str().filter(|id| id.starts_with("image:")) {
            rows.push(json!({"name": id, "state": vm["state"], "controller": vm["controller"], "ip": Value::Null}));
        }
    }
    Ok(rows)
}

fn split_remote(arg: &str) -> Option<(&str, &str)> {
    let (name, path) = arg.split_once(':')?;
    if name.contains('/') || name.is_empty() {
        return None;
    }
    if paths::vm(name).join("config.json").exists() {
        Some((name, path))
    } else {
        None
    }
}

fn copy(json: bool, src: &str, dst: &str) -> Result<()> {
    let (remote_name, src_arg, dst_arg) = match (split_remote(src), split_remote(dst)) {
        (Some(_), Some(_)) => bail!("copying between two VMs is not supported"),
        (None, None) => bail!("one side must be NAME:path"),
        (Some((name, path)), None) => (name, Some(path), None),
        (None, Some((name, path))) => (name, None, Some(path)),
    };
    let (spec, ip) = reachable(remote_name)?;
    let host_side = |path: &str| format!("{}@{}:{}", spec.user, ip, path);
    let (from, to) = match (src_arg, dst_arg) {
        (Some(path), None) => (host_side(path), dst.to_string()),
        (None, Some(path)) => (src.to_string(), host_side(path)),
        _ => unreachable!(),
    };
    let status = Command::new("scp").args(machine::ssh_options()).arg("-r").arg(&from).arg(&to).status()?;
    if !status.success() {
        bail!("scp failed");
    }
    emit(json, json!({"ok": true, "from": from, "to": to}), String::new);
    Ok(())
}

fn password_cmd(json: bool, command: PasswordCmd) -> Result<()> {
    match command {
        PasswordCmd::Type { name, enter, force } => {
            let target = Target::resolve(&name)?;
            let image = match name.strip_prefix("image:") {
                Some(image) => image.to_string(),
                None => target.spec()?.image.with_context(|| format!("{name} does not record which image it came from"))?,
            };
            let secret = password::get(&image)?;
            let mut vnc = connect_vnc(&target, force)?;
            keys::type_text(&mut vnc, &secret)?;
            if enter {
                keys::press(&mut vnc, "return")?;
            }
            vnc.finish()?;
            emit(json, json!({"ok": true}), String::new);
            Ok(())
        }
        PasswordCmd::Set { image } => {
            if !paths::image(&image).join("config.json").exists() {
                bail!("no image named '{image}'");
            }
            password::set(&image)?;
            emit(json, json!({"ok": true, "image": image}), || format!("password saved for image '{image}'"));
            Ok(())
        }
        PasswordCmd::Rm { image } => {
            let removed = password::remove(&image)?;
            emit(json, json!({"ok": true, "removed": removed}), || {
                if removed { format!("password for image '{image}' removed") } else { format!("no password saved for image '{image}'") }
            });
            Ok(())
        }
    }
}

fn image(json: bool, command: ImageCmd) -> Result<()> {
    match command {
        ImageCmd::Create { name, ipsw, disk_gb, cpus, memory_gb, detach, force } => {
            machine::validate_name(&name)?;
            machine::ensure_guest_files()?;
            let dir = paths::image(&name);
            if dir.join("disk.img").exists() && !force {
                bail!("image '{name}' already exists (use --force to reinstall)");
            }
            let ipsw = if ipsw == "latest" {
                ipsw
            } else {
                std::fs::canonicalize(&ipsw).with_context(|| format!("ipsw {ipsw}"))?.to_string_lossy().into_owned()
            };
            let spec = Spec {
                cpus,
                memory_mb: memory_gb * 1024,
                display: Display { width: 1440, height: 900, ppi: 80 },
                mac: machine::random_mac()?,
                shares: vec![Share {
                    name: "shovel".into(),
                    path: paths::guest().to_string_lossy().into_owned(),
                    read_only: true,
                }],
                user: machine::current_user(),
                image: None,
                workspace_source: None,
                workspace_mode: None,
            };
            machine::save(&dir, &spec)?;
            let id = format!("image:{name}");
            host::request(json!({"cmd": "install", "id": id, "dir": dir, "ipsw": ipsw, "disk_gb": disk_gb}))?;
            if detach {
                emit(json, json!({"ok": true, "job": id}), || {
                    "Installing in the background. Check progress with `shovel host status`.".into()
                });
                return Ok(());
            }
            let mut last = String::new();
            loop {
                let job = host::request(json!({"cmd": "job", "id": id}))?["job"].clone();
                let line = format!(
                    "{} {:>5.1}%",
                    job["phase"].as_str().unwrap_or(""),
                    job["progress"].as_f64().unwrap_or(0.0) * 100.0
                );
                if !json && line != last {
                    eprint!("\r\x1b[2K{line}");
                    let _ = std::io::stderr().flush();
                    last = line;
                }
                if job["done"].as_bool() == Some(true) {
                    if !json {
                        eprintln!();
                    }
                    if let Some(err) = job["error"].as_str() {
                        bail!("install failed: {err}");
                    }
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            emit(json, json!({"ok": true, "image": name}), || {
                format!(
                    "macOS installed into image '{name}'.\nNext: `shovel image setup {name}` to create the user account and run the setup script."
                )
            });
            Ok(())
        }
        ImageCmd::Setup { name } => {
            let dir = paths::image(&name);
            if !dir.join("disk.img").exists() {
                bail!("image '{name}' does not exist; run `shovel image create {name}` first");
            }
            machine::ensure_guest_files()?;
            let spec = machine::load(&dir)?;
            host::request(json!({"cmd": "start", "id": format!("image:{name}"), "dir": dir, "view": true, "takeover": true}))?;
            emit(json, json!({"ok": true, "image": name}), || {
                format!(
                    "The image is booting in a Shovel window. One-time setup:\n\
                     \n  1. Go through Setup Assistant. Create an admin account named '{user}' (same as your Mac,\n     so paths like ~/workspace match). Skip Apple Account sign-in and FileVault.\n\
                     \n  2. Open Terminal in the VM and run:\n       bash \"/Volumes/My Shared Files/shovel/setup.sh\"\n\
                     \n  3. Run `shovel password set {name}` here and enter the same password, so agents can\n     type it into auth dialogs with `shovel password type <vm>` without seeing it.\n\
                     \n  4. Run `shovel image stop {name}`.\n\
                     \nAfter that, `shovel up <vm-name>` clones this image in seconds.",
                    user = spec.user
                )
            });
            Ok(())
        }
        ImageCmd::Stop { name, force } => {
            let target = Target::resolve(&format!("image:{name}"))?;
            down(&target, force)?;
            emit(json, json!({"ok": true}), || format!("image '{name}' stopped"));
            Ok(())
        }
        ImageCmd::Ls => {
            let mut rows = Vec::new();
            if let Ok(entries) = std::fs::read_dir(paths::images()) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if !dir.join("config.json").exists() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let installed = dir.join("disk.img").exists();
                    rows.push(json!({
                        "name": name,
                        "installed": installed,
                        "disk_bytes": machine::dir_size(&dir),
                    }));
                }
            }
            emit(json, json!({"ok": true, "images": rows}), || {
                if rows.is_empty() {
                    return "No images. Create one with `shovel image create`.".into();
                }
                rows.iter()
                    .map(|r| {
                        format!(
                            "{:<16} {:<10} {:.1} GB on disk",
                            r["name"].as_str().unwrap_or(""),
                            if r["installed"] == true { "ready" } else { "incomplete" },
                            r["disk_bytes"].as_u64().unwrap_or(0) as f64 / 1e9
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(())
        }
        ImageCmd::Rm { name } => {
            if running(&format!("image:{name}"))? {
                bail!("image '{name}' is running; stop it first");
            }
            let dir = paths::image(&name);
            if !dir.exists() {
                bail!("no image named '{name}'");
            }
            std::fs::remove_dir_all(&dir)?;
            emit(json, json!({"ok": true}), || format!("image '{name}' removed"));
            Ok(())
        }
    }
}
