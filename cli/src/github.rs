use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::record::now_ms;

const MEDIA_BRANCH: &str = "shovel-media";
const MAX_BYTES: u64 = 95 * 1024 * 1024;

fn gh(args: &[&str], input: Option<&[u8]>) -> Result<String> {
    let mut child = Command::new("gh")
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running gh (install GitHub CLI and run `gh auth login`)")?;
    if let Some(data) = input {
        child.stdin.take().unwrap().write_all(data)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("gh {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn gh_json(args: &[&str], body: &Value) -> Result<Value> {
    let mut full = args.to_vec();
    full.extend(["--input", "-"]);
    let out = gh(&full, Some(body.to_string().as_bytes()))?;
    Ok(serde_json::from_str(&out)?)
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

fn parse_pr(pr: &str, repo: Option<String>) -> Result<(String, String)> {
    if let Some(rest) = pr.split("github.com/").nth(1) {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 4 && parts[2] == "pull" {
            return Ok((format!("{}/{}", parts[0], parts[1]), parts[3].to_string()));
        }
        bail!("not a pull request URL: {pr}");
    }
    let number = pr.trim_start_matches('#').to_string();
    if !number.chars().all(|c| c.is_ascii_digit()) || number.is_empty() {
        bail!("--pr must be a number or a pull request URL");
    }
    let repo = match repo {
        Some(r) => r,
        None => gh(&["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"], None)
            .context("could not infer the repository; pass --repo owner/name")?,
    };
    Ok((repo, number))
}

fn ensure_media_branch(repo: &str) -> Result<()> {
    if gh(&["api", &format!("repos/{repo}/git/ref/heads/{MEDIA_BRANCH}")], None).is_ok() {
        return Ok(());
    }
    let blob = gh_json(
        &["api", "-X", "POST", &format!("repos/{repo}/git/blobs")],
        &json!({"content": "Recordings and screenshots attached to pull requests by shovel.\n", "encoding": "utf-8"}),
    )?;
    let tree = gh_json(
        &["api", "-X", "POST", &format!("repos/{repo}/git/trees")],
        &json!({"tree": [{"path": "README.md", "mode": "100644", "type": "blob", "sha": blob["sha"]}]}),
    )?;
    let commit = gh_json(
        &["api", "-X", "POST", &format!("repos/{repo}/git/commits")],
        &json!({"message": "Create shovel media branch", "tree": tree["sha"], "parents": []}),
    )?;
    gh_json(
        &["api", "-X", "POST", &format!("repos/{repo}/git/refs")],
        &json!({"ref": format!("refs/heads/{MEDIA_BRANCH}"), "sha": commit["sha"]}),
    )?;
    Ok(())
}

fn embed(repo: &str, path: &str, name: &str) -> String {
    let blob = format!("https://github.com/{repo}/blob/{MEDIA_BRANCH}/{path}");
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "gif" | "png" | "jpg" | "jpeg" | "webp" => format!("![{name}]({blob}?raw=true)"),
        "mp4" | "mov" => format!("🎬 [{name}]({blob}?raw=true) (full-quality video)"),
        _ => format!("📎 [{name}]({blob})"),
    }
}

pub fn attach(files: &[PathBuf], pr: &str, repo: Option<String>, message: Option<String>, dry_run: bool) -> Result<Value> {
    if files.is_empty() {
        bail!("nothing to attach");
    }
    for file in files {
        let size = std::fs::metadata(file).with_context(|| format!("{}", file.display()))?.len();
        if size > MAX_BYTES {
            bail!("{} is {} MB; GitHub's contents API accepts up to ~95 MB", file.display(), size / 1_048_576);
        }
    }
    let (repo, number) = parse_pr(pr, repo)?;
    let stamp = now_ms();
    let mut uploads = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        uploads.push((file, format!("pr-{number}/{stamp}-{name}"), name));
    }
    let mut lines = vec![message.unwrap_or_else(|| "Recorded in a Shovel VM:".into()), String::new()];
    lines.extend(uploads.iter().map(|(_, path, name)| embed(&repo, path, name)));
    let body = lines.join("\n");
    if dry_run {
        return Ok(json!({"ok": true, "dry_run": true, "repo": repo, "pr": number, "body": body}));
    }

    ensure_media_branch(&repo)?;
    for (file, path, name) in &uploads {
        let content = base64(&std::fs::read(file)?);
        gh_json(
            &["api", "-X", "PUT", &format!("repos/{repo}/contents/{path}")],
            &json!({"message": format!("Attach {name} to PR #{number}"), "content": content, "branch": MEDIA_BRANCH}),
        )
        .with_context(|| format!("uploading {}", file.display()))?;
    }
    let url = gh(&["pr", "comment", &number, "--repo", &repo, "--body-file", "-"], Some(body.as_bytes()))?;
    Ok(json!({"ok": true, "repo": repo, "pr": number, "comment": url, "body": body}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn parses_pr_urls() {
        let (repo, n) = parse_pr("https://github.com/acme/app/pull/42", None).unwrap();
        assert_eq!((repo.as_str(), n.as_str()), ("acme/app", "42"));
        let (repo, n) = parse_pr("#7", Some("acme/app".into())).unwrap();
        assert_eq!((repo.as_str(), n.as_str()), ("acme/app", "7"));
    }
}
