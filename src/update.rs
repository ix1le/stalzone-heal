use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

const REPO: &str = "ix1le/stalzone-heal";
const USER_AGENT: &str = "Mozilla/5.0";

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub tag: String,
    pub url: String,
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    html_url: String,
}

fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

pub fn short_version(v: &str) -> String {
    match v.strip_suffix(".0") {
        Some(rest) if rest.matches('.').count() == 1 => rest.to_string(),
        _ => v.to_string(),
    }
}

pub fn check() -> Option<Update> {
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let page = format!("https://github.com/{REPO}/releases/latest");
    let prefix = format!("https://github.com/{REPO}/");

    let resp = ureq::get(&api)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .call()
        .ok()?;
    let rel: LatestRelease = serde_json::from_reader(resp.into_reader()).ok()?;
    if !is_newer(&rel.tag_name, env!("CARGO_PKG_VERSION")) {
        return None;
    }
    let url = if rel.html_url.starts_with(&prefix) {
        rel.html_url
    } else {
        page
    };
    Some(Update { tag: rel.tag_name, url })
}

pub fn open(url: &str) {
    let _ = Command::new("explorer")
        .arg(OsStr::new(url))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}