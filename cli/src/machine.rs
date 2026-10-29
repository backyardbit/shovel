use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths;

#[derive(Serialize, Deserialize, Clone)]
pub struct Display {
    pub width: u32,
    pub height: u32,
    pub ppi: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Share {
    pub name: String,
    pub path: String,
    pub read_only: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Spec {
    pub cpus: u32,
    pub memory_mb: u64,
    pub display: Display,
    pub mac: String,
    pub shares: Vec<Share>,
    pub user: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub workspace_source: Option<String>,
    #[serde(default)]
    pub workspace_mode: Option<String>,
}

pub struct Target {
    pub id: String,
    pub dir: PathBuf,
}

impl Target {
    pub fn resolve(name: &str) -> Result<Target> {
        let (id, dir) = match name.strip_prefix("image:") {
            Some(image) => (name.to_string(), paths::image(image)),
            None => (name.to_string(), paths::vm(name)),
        };
        if !dir.join("config.json").exists() {
            bail!("no VM named '{name}' (looked in {})", dir.display());
        }
        Ok(Target { id, dir })
    }

    pub fn spec(&self) -> Result<Spec> {
        load(&self.dir)
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("invalid name '{name}': use letters, digits, '-' or '_'");
    }
    Ok(())
}

pub fn load(dir: &Path) -> Result<Spec> {
    let path = dir.join("config.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn save(dir: &Path, spec: &Spec) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(spec)?)?;
    Ok(())
}

pub fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "admin".into())
}

pub fn default_workspace() -> PathBuf {
    paths::home().join("workspace")
}

pub fn random_mac() -> Result<String> {
    let mut bytes = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[0] = (bytes[0] & 0xfe) | 0x02;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"))
}

extern "C" {
    fn clonefile(src: *const libc_char, dst: *const libc_char, flags: u32) -> i32;
}

#[allow(non_camel_case_types)]
type libc_char = std::os::raw::c_char;

pub fn clone_path(src: &Path, dst: &Path) -> Result<()> {
    let s = CString::new(src.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    let rc = unsafe { clonefile(s.as_ptr(), d.as_ptr(), 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cloning {} -> {}", src.display(), dst.display()));
    }
    Ok(())
}

pub fn ensure_guest_files() -> Result<()> {
    let key = paths::key();
    if !key.exists() {
        std::fs::create_dir_all(key.parent().unwrap())?;
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "shovel", "-f"])
            .arg(&key)
            .status()?;
        if !status.success() {
            bail!("ssh-keygen failed");
        }
    }
    let guest = paths::guest();
    std::fs::create_dir_all(&guest)?;
    std::fs::copy(key.with_extension("pub"), guest.join("authorized_keys"))?;
    let timezone = std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| p.to_string_lossy().split("zoneinfo/").nth(1).map(String::from))
        .unwrap_or_else(|| "UTC".into());
    let script = SETUP_SCRIPT
        .replace("__WORKSPACE__", &default_workspace().to_string_lossy())
        .replace("__TIMEZONE__", &timezone);
    std::fs::write(guest.join("setup.sh"), script)?;
    Ok(())
}

const SETUP_SCRIPT: &str = r#"#!/bin/bash
set -u
SHARE="/Volumes/My Shared Files"
WORKSPACE="__WORKSPACE__"
TIMEZONE="__TIMEZONE__"

read -r -s -p "Password for $USER (used for sudo and automatic login): " PASSWORD
echo
echo "$PASSWORD" | sudo -S -v 2>/dev/null || { echo "wrong password"; exit 1; }

echo "==> Installing Shovel SSH key"
mkdir -p ~/.ssh && chmod 700 ~/.ssh
touch ~/.ssh/authorized_keys
grep -qxF "$(cat "$SHARE/shovel/authorized_keys")" ~/.ssh/authorized_keys || cat "$SHARE/shovel/authorized_keys" >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys

echo "==> Enabling passwordless sudo"
echo "$USER ALL=(ALL) NOPASSWD: ALL" | sudo tee /etc/sudoers.d/shovel >/dev/null
sudo chmod 440 /etc/sudoers.d/shovel

echo "==> Enabling Remote Login (SSH)"
sudo systemsetup -setremotelogin on >/dev/null 2>&1 || sudo launchctl enable system/com.openssh.sshd 2>/dev/null
sudo launchctl bootstrap system /System/Library/LaunchDaemons/ssh.plist 2>/dev/null || true

echo "==> Enabling automatic login"
sudo defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser "$USER"
perl -e 'my @k=(0x7D,0x89,0x52,0x23,0xD2,0xBC,0xDD,0xEA,0xA3,0xB9,0x1F); my @p=unpack("C*",shift); my $n=@p; my $len=$n % 12 ? $n+12-($n % 12) : $n+12; push @p,(0) x ($len-$n); print pack("C*", map { $p[$_] ^ $k[$_ % 11] } 0..$#p);' "$PASSWORD" | sudo tee /etc/kcpassword >/dev/null
sudo chmod 600 /etc/kcpassword

echo "==> Disabling sleep, screensaver, auto-updates and window restore"
sudo pmset -a sleep 0 displaysleep 0 disksleep 0 >/dev/null
defaults -currentHost write com.apple.screensaver idleTime -int 0
defaults write -g NSQuitAlwaysKeepsWindows -bool false
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool false
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticallyInstallMacOSUpdates -bool false
sudo defaults write /Library/Preferences/com.apple.commerce AutoUpdate -bool false
sudo systemsetup -settimezone "$TIMEZONE" >/dev/null 2>&1 || true

echo "==> Linking $WORKSPACE to the shared workspace"
sudo mkdir -p "$(dirname "$WORKSPACE")"
if [ -e "$WORKSPACE" ] && [ ! -L "$WORKSPACE" ]; then
  echo "    $WORKSPACE already exists and is not a symlink; leaving it alone"
else
  sudo ln -sfn "$SHARE/workspace" "$WORKSPACE"
  sudo chown -h "$USER" "$WORKSPACE"
fi

echo "==> Installing Xcode Command Line Tools (git, clang)"
if ! xcode-select -p >/dev/null 2>&1; then
  touch /tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
  LABEL=$(softwareupdate -l 2>/dev/null | grep -E "\* Label: Command Line Tools" | tail -1 | sed "s/^.*Label: //")
  [ -n "$LABEL" ] && sudo softwareupdate -i "$LABEL"
  rm -f /tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
fi

echo
echo "Done. Shut the VM down (Apple menu > Shut Down) or run 'shovel image stop <name>' on the host."
"#;

fn normalize_mac(mac: &str) -> String {
    mac.split(':')
        .map(|p| u8::from_str_radix(p, 16).map(|b| format!("{b:x}")).unwrap_or_else(|_| p.to_lowercase()))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn lookup_ip(mac: &str) -> Option<String> {
    let leases = std::fs::read_to_string("/var/db/dhcpd_leases").ok()?;
    let want = normalize_mac(mac);
    let mut best: Option<(u64, String)> = None;
    for block in leases.split('}') {
        let mut ip = None;
        let mut hw = None;
        let mut lease = 0u64;
        for line in block.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("ip_address=") {
                ip = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("hw_address=") {
                hw = Some(normalize_mac(v.split_once(',').map(|(_, m)| m).unwrap_or(v)));
            } else if let Some(v) = line.strip_prefix("lease=") {
                lease = u64::from_str_radix(v.trim_start_matches("0x"), 16).unwrap_or(0);
            }
        }
        if let (Some(ip), Some(hw)) = (ip, hw) {
            if hw == want && best.as_ref().map_or(true, |(l, _)| lease > *l) {
                best = Some((lease, ip));
            }
        }
    }
    best.map(|(_, ip)| ip)
}

pub fn ssh_options() -> Vec<String> {
    let key = paths::key();
    vec![
        "-i".into(),
        key.to_string_lossy().into_owned(),
        "-o".into(),
        "StrictHostKeyChecking=no".into(),
        "-o".into(),
        "UserKnownHostsFile=/dev/null".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
    ]
}

pub fn ssh_command(spec: &Spec, ip: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(ssh_options()).arg(format!("{}@{}", spec.user, ip));
    cmd
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn ssh_ready(spec: &Spec, ip: &str) -> bool {
    ssh_command(spec, ip)
        .args(["-o", "BatchMode=yes", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn wait_for_ip(spec: &Spec, deadline: Instant) -> Result<String> {
    loop {
        if let Some(ip) = lookup_ip(&spec.mac) {
            return Ok(ip);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for the VM to get an IP address (MAC {})", spec.mac);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub fn wait_for_ssh(spec: &Spec, ip: &str, deadline: Instant) -> Result<()> {
    loop {
        if ssh_ready(spec, ip) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for SSH on {}@{ip}; was the golden image prepared with `shovel image setup`?",
                spec.user
            );
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn dir_size(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.blocks() * 512;
                }
            }
        }
    }
    total
}
