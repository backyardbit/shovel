use anyhow::{bail, Result};

use crate::vnc::{pause, Vnc};

const SHIFT: u32 = 0xffe1;
const CONTROL: u32 = 0xffe3;
const OPTION: u32 = 0xffe7;
const COMMAND: u32 = 0xffe9;

fn modifier(name: &str) -> Option<u32> {
    match name {
        "cmd" | "command" | "super" | "meta" => Some(COMMAND),
        "shift" => Some(SHIFT),
        "ctrl" | "control" => Some(CONTROL),
        "opt" | "option" | "alt" => Some(OPTION),
        _ => None,
    }
}

fn named(name: &str) -> Option<u32> {
    let sym = match name {
        "return" | "enter" => 0xff0d,
        "tab" => 0xff09,
        "esc" | "escape" => 0xff1b,
        "backspace" | "delete" => 0xff08,
        "forwarddelete" | "del" => 0xffff,
        "space" => 0x20,
        "up" => 0xff52,
        "down" => 0xff54,
        "left" => 0xff51,
        "right" => 0xff53,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
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

pub fn parse_combo(combo: &str) -> Result<(Vec<u32>, u32)> {
    let lower = combo.to_lowercase();
    let parts: Vec<&str> = if lower == "+" { vec!["+"] } else { lower.split('+').collect() };
    let (key, mods) = parts.split_last().unwrap();
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
