//! Tells people when a newer release of Drift exists.
//!
//! A lookup must never wait on the network, so it only reads a small cache file. At most once a
//! day it starts a short-lived copy of itself that asks git for the release tags and saves the
//! answer, and the next lookup prints a one-line notice to stderr, at most once a day. The only
//! thing sent is an ordinary `git ls-remote` of this project's public repo.
//!
//! Set `DRIFT_NO_UPDATE_CHECK` to turn this off. It's also off when `CI` is set.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "https://github.com/The-Smithy/drift";
const DAY: u64 = 24 * 60 * 60;
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default, Serialize, Deserialize)]
struct State {
    latest: Option<String>,
    #[serde(default)]
    checked_at: u64,
    #[serde(default)]
    notified_at: u64,
}

/// Prints the notice if one is due, and starts a background check if one is due. Never fails.
pub fn after_command() {
    if std::env::var_os("DRIFT_NO_UPDATE_CHECK").is_some() || std::env::var_os("CI").is_some() {
        return;
    }
    let Some(path) = state_path() else { return };
    let mut state = read_state(&path);
    let now = now();

    let check_due = now.saturating_sub(state.checked_at) >= DAY;
    let notice = state
        .latest
        .as_deref()
        .and_then(|latest| notice(CURRENT, latest))
        .filter(|_| now.saturating_sub(state.notified_at) >= DAY);

    if check_due {
        state.checked_at = now;
    }
    if notice.is_some() {
        state.notified_at = now;
    }
    if check_due || notice.is_some() {
        // Saved first, so other lookups running right now don't repeat the notice or the check.
        // If saving fails, stay quiet rather than repeating them on every lookup.
        if write_state(&path, &state).is_err() {
            return;
        }
    }
    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
    if check_due {
        start_background_check();
    }
}

/// The hidden command the background check runs: asks git for the tags and saves the newest.
pub fn refresh() {
    let Some(path) = state_path() else { return };
    if let Ok(latest) = fetch_latest() {
        let mut state = read_state(&path);
        state.latest = latest;
        state.checked_at = now();
        let _ = write_state(&path, &state);
    }
}

/// `drift update-check`: asks now, and says how to update.
pub fn check_now() -> Result<()> {
    let latest = fetch_latest()?;
    if let Some(path) = state_path() {
        let mut state = read_state(&path);
        state.latest.clone_from(&latest);
        state.checked_at = now();
        let _ = write_state(&path, &state);
    }
    match latest {
        Some(latest) => match notice(CURRENT, &latest) {
            Some(notice) => println!("{notice}"),
            None => println!("drift {CURRENT} is up to date (latest release: {latest})."),
        },
        None => println!("drift {CURRENT}: no releases have been published yet."),
    }
    Ok(())
}

fn notice(current: &str, latest: &str) -> Option<String> {
    (parse_version(latest)? > parse_version(current)?).then(|| {
        format!(
            "drift {latest} is available (you have {current}). Update: {}",
            update_instructions(latest)
        )
    })
}

fn update_instructions(latest: &str) -> String {
    let installed_by_cargo = std::env::current_exe().is_ok_and(|exe| {
        exe.components()
            .any(|part| part.as_os_str() == ".cargo" || part.as_os_str() == "cargo")
    });
    if installed_by_cargo {
        format!("cargo install --git {REPO} --tag v{latest} --locked --force")
    } else {
        format!("{REPO}/releases/latest")
    }
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        // A prerelease such as 1.0.0-rc.1 fails here, so nobody is told to install one.
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

// Lines look like: <sha>\trefs/tags/v0.3.0
fn newest_tag(ls_remote: &str) -> Option<String> {
    ls_remote
        .lines()
        .filter_map(|line| line.rsplit("refs/tags/").next())
        .filter_map(|tag| Some((parse_version(tag.trim())?, tag.trim())))
        .max_by_key(|(version, _)| *version)
        .map(|(_, tag)| tag.strip_prefix('v').unwrap_or(tag).to_string())
}

fn fetch_latest() -> Result<Option<String>> {
    // A test can point this at a local repo, so the tests never need the network.
    let remote = std::env::var("DRIFT_UPDATE_REMOTE").unwrap_or_else(|_| REPO.to_string());
    let mut child = Command::new("git")
        .args(["-c", "credential.helper=", "ls-remote", "--tags", "--refs"])
        .arg(remote)
        .arg("v*")
        // Not the current folder, whose git config could belong to a repo nobody has vetted.
        .current_dir(std::env::temp_dir())
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let start = Instant::now();
    while child.try_wait()?.is_none() {
        if start.elapsed() > GIT_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("git took too long to answer");
        }
        sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("git couldn't reach {REPO}");
    }
    Ok(newest_tag(&String::from_utf8_lossy(&output.stdout)))
}

fn start_background_check() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    // Null handles matter: a child holding the caller's pipes would make the caller wait for it.
    command
        .arg("__update-refresh")
        // Not the current folder, which it would otherwise keep from being deleted on Windows.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    let _ = command.spawn();
}

fn state_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("update-check.json"))
}

/// Where Drift keeps what it remembers between runs.
pub(crate) fn cache_dir() -> Option<PathBuf> {
    let environment = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    environment("DRIFT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            environment("LOCALAPPDATA")
                .or_else(|| environment("XDG_CACHE_HOME"))
                .map(|dir| PathBuf::from(dir).join("drift"))
        })
        .or_else(|| environment("HOME").map(|home| Path::new(&home).join(".cache").join("drift")))
}

fn read_state(path: &Path) -> State {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_state(path: &Path, state: &State) -> Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!("update-check.{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_string(state)?)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_as_numbers() {
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("v1.0.0") > parse_version("0.99.0"));
        assert_eq!(parse_version("v0.3.0"), parse_version("0.3.0"));
    }

    #[test]
    fn ignores_prereleases_and_other_tags() {
        assert_eq!(parse_version("1.0.0-rc.1"), None);
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
    }

    #[test]
    fn finds_the_newest_tag() {
        let output = "aaa\trefs/tags/v0.9.0\nbbb\trefs/tags/v0.10.0\n\
                      ccc\trefs/tags/v1.0.0-rc.1\nddd\trefs/tags/vanity\n";
        assert_eq!(newest_tag(output), Some("0.10.0".to_string()));
        assert_eq!(newest_tag(""), None);
    }

    #[test]
    fn only_a_newer_version_gets_a_notice() {
        assert!(notice("0.2.0", "0.3.0").is_some());
        assert!(notice("0.3.0", "0.3.0").is_none());
        assert!(notice("0.4.0", "0.3.0").is_none());
        assert!(notice("0.2.0", "garbage").is_none());
    }

    #[test]
    fn a_notice_says_how_to_update_in_one_line() {
        let text = notice("0.2.0", "0.3.0").unwrap();
        assert!(text.contains("0.3.0") && text.contains("0.2.0"));
        assert!(text.contains(REPO));
        assert!(!text.contains('\n'));
    }
}
