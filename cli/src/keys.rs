use anyhow::{bail, Result};
use std::time::Duration;

use crate::vnc::{pause, Vnc};

const SHIFT: u32 = 0xffe1;
const CONTROL: u32 = 0xffe3;
const OPTION: u32 = 0xffe7;
const COMMAND: u32 = 0xffe9;

fn modifier(name: &str) -> Option<u32> {
    match name {
        "cmd" | "command" | "super" | "meta" | "win" | "windows" | "cmdorctrl" => Some(COMMAND),
        "shift" => Some(SHIFT),
        "ctrl" | "control" => Some(CONTROL),
        "opt" | "option" | "alt" => Some(OPTION),
        _ => None,
    }
}

fn named(name: &str) -> Option<u32> {
    let sym = match name {
        "return" | "enter" | "kpenter" | "ret" => 0xff0d,
        "tab" => 0xff09,
        "esc" | "escape" => 0xff1b,
        "backspace" | "delete" => 0xff08,
        "forwarddelete" | "del" => 0xffff,
        "space" => 0x20,
        "up" | "arrowup" => 0xff52,
        "down" | "arrowdown" => 0xff54,
        "left" | "arrowleft" => 0xff51,
        "right" | "arrowright" => 0xff53,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" | "pgup" | "prior" => 0xff55,
        "pagedown" | "pgdn" | "next" => 0xff56,
        "capslock" => 0xffe5,
        _ => {
            let n: u32 = name.strip_prefix('f')?.parse().ok()?;
            if (1..=20).contains(&n) {
                0xffbe + n - 1
            } else {
                return None;
            }
        }
    };
    Some(sym)
}

const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

fn char_keysym(c: char) -> (u32, bool) {
    match c {
        '\n' => (0xff0d, false),
        '\t' => (0xff09, false),
        c if c.is_ascii_uppercase() => (c as u32, true),
        c if SHIFTED.contains(c) => (c as u32, true),
        c if (c as u32) < 0x100 => (c as u32, false),
        c => (0x0100_0000 + c as u32, false),
    }
}

fn normalize(token: &str) -> String {
    if token.chars().count() > 1 {
        token.chars().filter(|c| !matches!(c, '_' | '-' | ' ')).collect()
    } else {
        token.to_string()
    }
}

pub fn parse_combo(combo: &str) -> Result<(Vec<u32>, u32)> {
    let lower = combo.to_lowercase();
    let parts: Vec<String> = if lower == "+" {
        vec!["+".into()]
    } else if let Some(mods) = lower.strip_suffix("++") {
        mods.split('+').map(normalize).chain(["+".to_string()]).collect()
    } else {
        lower.split('+').map(normalize).collect()
    };
    let (key, mods) = parts.split_last().unwrap();
    let key = key.as_str();
    let mut modifiers = Vec::new();
    for m in mods {
        match modifier(m) {
            Some(sym) => modifiers.push(sym),
            None => bail!("unknown modifier '{m}' in '{combo}'"),
        }
    }
    let sym = if let Some(sym) = named(key) {
        sym
    } else if let Some(sym) = modifier(key) {
        sym
    } else {
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => char_keysym(c).0,
            _ => bail!("unknown key '{key}' in '{combo}'"),
        }
    };
    Ok((modifiers, sym))
}

pub fn parse_modifiers(text: &str) -> Result<Vec<u32>> {
    let mut modifiers = Vec::new();
    for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match modifier(&normalize(&part.to_lowercase())) {
            Some(sym) => modifiers.push(sym),
            None => bail!("unknown modifier '{part}'; use cmd, shift, ctrl, option"),
        }
    }
    Ok(modifiers)
}

pub fn modifiers_down(vnc: &mut Vnc, modifiers: &[u32], down: bool) -> Result<()> {
    let ordered: Vec<u32> = if down { modifiers.to_vec() } else { modifiers.iter().rev().copied().collect() };
    for m in ordered {
        vnc.key(down, m)?;
        pause(20);
    }
    Ok(())
}

pub fn hold(vnc: &mut Vnc, combo: &str, duration: Duration) -> Result<()> {
    let (modifiers, key) = parse_combo(combo)?;
    modifiers_down(vnc, &modifiers, true)?;
    vnc.key(true, key)?;
    std::thread::sleep(duration);
    vnc.key(false, key)?;
    pause(20);
    modifiers_down(vnc, &modifiers, false)?;
    Ok(())
}

pub fn press(vnc: &mut Vnc, combo: &str) -> Result<()> {
    let (modifiers, key) = parse_combo(combo)?;
    for m in &modifiers {
        vnc.key(true, *m)?;
        pause(20);
    }
    vnc.key(true, key)?;
    pause(30);
    vnc.key(false, key)?;
    for m in modifiers.iter().rev() {
        pause(20);
        vnc.key(false, *m)?;
    }
    pause(40);
    Ok(())
}

pub fn type_text(vnc: &mut Vnc, text: &str) -> Result<()> {
    for c in text.chars() {
        let (sym, shift) = char_keysym(c);
        if shift {
            vnc.key(true, SHIFT)?;
        }
        vnc.key(true, sym)?;
        vnc.key(false, sym)?;
        if shift {
            vnc.key(false, SHIFT)?;
        }
        pause(12);
    }
    Ok(())
}
