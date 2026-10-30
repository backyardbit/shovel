use anyhow::{bail, Result};
use std::io::IsTerminal;
use std::process::{Command, Stdio};

const SERVICE: &str = "dev.shovel.guest-password";

pub fn set(image: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("`shovel password set` prompts for the password; run it in a terminal");
    }
    let status = Command::new("security")
        .args(["add-generic-password", "-U", "-s", SERVICE, "-a", image])
        .args(["-l", &format!("Shovel guest password ({image})")])
        .arg("-w")
        .status()?;
    if !status.success() {
        bail!("could not save the password to the login keychain");
    }
    Ok(())
}

pub fn get(image: &str) -> Result<String> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", SERVICE, "-a", image, "-w"])
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("no password saved for image '{image}'; ask the user to run `shovel password set {image}` in a terminal");
    }
    let mut password = String::from_utf8(output.stdout)?;
    if password.ends_with('\n') {
        password.pop();
    }
    Ok(password)
}

pub fn remove(image: &str) -> Result<bool> {
    let status = Command::new("security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}
