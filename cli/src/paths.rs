use std::path::PathBuf;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn root() -> PathBuf {
    match std::env::var("SHOVEL_HOME") {
        Ok(custom) if !custom.is_empty() => PathBuf::from(custom),
        _ => home().join(".shovel"),
    }
}

pub fn images() -> PathBuf {
    root().join("images")
}

pub fn vms() -> PathBuf {
    root().join("vms")
}

pub fn image(name: &str) -> PathBuf {
    images().join(name)
}

pub fn vm(name: &str) -> PathBuf {
    vms().join(name)
}

pub fn socket() -> PathBuf {
    root().join("host.sock")
}

pub fn key() -> PathBuf {
    root().join("keys").join("id_ed25519")
}

pub fn guest() -> PathBuf {
    root().join("guest")
}
