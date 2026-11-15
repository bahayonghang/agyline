use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::collect::GitFrameState;
use crate::config::EffectiveConfig;
use crate::input::Payload;
use crate::platform;
use crate::state::git_cache::acquire_git_lock;
use crate::state::{
    GitCacheEntry, maybe_sweep_stale_state, normalize_cwd_lexical, read_git_cache, write_git_cache,
};
use crate::statusline::RenderEnv;

const GIT_STALE_MAX_MS: u64 = 600_000;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedGitStatus {
    pub branch: String,
    pub dirty: bool,
    pub has_conflict: bool,
    pub ahead: i64,
    pub behind: i64,
}

pub fn resolve_git_frame(
    payload: &Payload,
    config: &EffectiveConfig,
    env: &RenderEnv<'_>,
    delta_out: i64,
) -> Option<GitFrameState> {
    let fresh_ttl_ms = config.git_cache_ms;
    let stale_max_ms = GIT_STALE_MAX_MS.max(fresh_ttl_ms);

    let cache_entry = if !payload.cwd.trim().is_empty() {
        let entry = read_git_cache(&env.home_dir, &payload.cwd);
        let should_spawn = match &entry {
            None => true,
            Some(cached) => {
                let age_ms = env.now_ms.saturating_sub(cached.updated_at_ms);
                if !cached.is_repo {
                    age_ms >= fresh_ttl_ms
                } else {
                    age_ms >= fresh_ttl_ms || delta_out > 0
                }
            }
        };
        if should_spawn {
            env.spawner.spawn_refresh_git(&payload.cwd);
        }
        entry
    } else {
        None
    };

    let valid_cache = cache_entry.as_ref().filter(|cached| {
        cached.is_repo && env.now_ms.saturating_sub(cached.updated_at_ms) <= stale_max_ms
    });

    let branch = if !payload.vcs_branch.trim().is_empty() {
        payload.vcs_branch.trim().to_string()
    } else if let Some(cached) = valid_cache
        && !cached.branch.trim().is_empty()
    {
        cached.branch.trim().to_string()
    } else {
        return None;
    };

    let dirty = payload.vcs_dirty || valid_cache.is_some_and(|c| c.dirty);
    let has_conflict = valid_cache.is_some_and(|c| c.has_conflict);
    let ahead = valid_cache.map_or(0, |c| c.ahead);
    let behind = valid_cache.map_or(0, |c| c.behind);

    Some(GitFrameState {
        branch,
        dirty,
        has_conflict,
        ahead,
        behind,
    })
}

pub fn run_hidden_refresh_git(tail: &[OsString]) {
    let Some(cwd_os) = parse_refresh_git_args(tail) else {
        return;
    };
    let home_dir = resolve_process_home_dir();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    refresh_git_at(&home_dir, &cwd_os, now_ms, GIT_COMMAND_TIMEOUT);
}

pub fn refresh_git_at(home_dir: &Path, cwd: &OsStr, now_ms: u64, git_timeout: Duration) {
    if cwd.is_empty() {
        return;
    }
    let cwd_lossy = cwd.to_string_lossy();
    let norm_cwd = normalize_cwd_lexical(&cwd_lossy);
    if norm_cwd.is_empty() {
        return;
    }

    let Some(lock) = acquire_git_lock(home_dir, &norm_cwd, now_ms) else {
        return;
    };

    let raw_path = Path::new(cwd);
    let is_repo = raw_path.is_dir() && {
        let resolved = fs::canonicalize(raw_path).unwrap_or_else(|_| raw_path.to_path_buf());
        has_git_marker_upward(&resolved)
    };

    if !is_repo {
        let _ = write_git_cache(
            home_dir,
            &GitCacheEntry {
                version: 1,
                cwd: norm_cwd,
                updated_at_ms: now_ms,
                is_repo: false,
                branch: String::new(),
                dirty: false,
                has_conflict: false,
                ahead: 0,
                behind: 0,
            },
        );
    } else if let Some(stdout_bytes) = run_git_porcelain_bounded(cwd, git_timeout) {
        let parsed = parse_porcelain_v2(&stdout_bytes);
        let _ = write_git_cache(
            home_dir,
            &GitCacheEntry {
                version: 1,
                cwd: norm_cwd,
                updated_at_ms: now_ms,
                is_repo: true,
                branch: parsed.branch,
                dirty: parsed.dirty,
                has_conflict: parsed.has_conflict,
                ahead: parsed.ahead,
                behind: parsed.behind,
            },
        );
    }

    maybe_sweep_stale_state(home_dir, now_ms);
    drop(lock);
}

fn parse_refresh_git_args(tail: &[OsString]) -> Option<OsString> {
    if tail.len() == 2 && tail[0] == OsStr::new("--cwd") {
        if tail[1].is_empty() {
            return None;
        }
        return Some(tail[1].clone());
    }
    if tail.len() == 1
        && let Some(val) = tail[0].to_str().and_then(|s| s.strip_prefix("--cwd="))
        && !val.is_empty()
    {
        return Some(OsString::from(val));
    }
    None
}

fn resolve_process_home_dir() -> PathBuf {
    let lookup = |k: &str| std::env::var(k).ok();
    let user_home = platform::user_home_dir(&lookup).unwrap_or_else(|| PathBuf::from("."));
    crate::config::resolve_home_dir(&lookup, &user_home)
}

fn has_git_marker_upward(start_dir: &Path) -> bool {
    let mut cur = start_dir;
    loop {
        let git_entry = cur.join(".git");
        if let Ok(meta) = fs::metadata(&git_entry)
            && (meta.is_dir() || meta.is_file())
        {
            return true;
        }
        match cur.parent() {
            Some(parent) if parent != cur && !parent.as_os_str().is_empty() => {
                cur = parent;
            }
            Some(_) | None => return false,
        }
    }
}

fn run_git_porcelain_bounded(cwd: &OsStr, timeout: Duration) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.quotePath=false")
        .arg("-C")
        .arg(cwd)
        .args([
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "--ignore-submodules=dirty",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    platform::configure_background_command(&mut cmd);

    let mut child = cmd.spawn().ok()?;
    let mut stdout_pipe = child.stdout.take()?;
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut buf = Vec::new();
        let res = stdout_pipe.read_to_end(&mut buf).map(|_| buf);
        let _ = tx.send(res);
    });

    let start = Instant::now();
    let mut poll_ms = 1_u64;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                return rx
                    .recv_timeout(Duration::from_millis(250))
                    .ok()
                    .and_then(Result::ok);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                let remaining = timeout.saturating_sub(start.elapsed());
                let sleep_dur = Duration::from_millis(poll_ms).min(remaining);
                thread::sleep(sleep_dur);
                poll_ms = (poll_ms * 2).min(10);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub fn parse_porcelain_v2(raw: &[u8]) -> ParsedGitStatus {
    let text = String::from_utf8_lossy(raw);
    let mut oid_short = String::new();
    let mut branch = String::new();
    let mut saw_detached = false;
    let mut dirty = false;
    let mut has_conflict = false;
    let mut ahead = 0_i64;
    let mut behind = 0_i64;

    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            let oid = rest.trim();
            if oid != "(initial)" && oid.chars().count() >= 7 {
                oid_short = oid.chars().take(7).collect();
            }
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            let hd = rest.trim();
            if hd == "(detached)" {
                saw_detached = true;
                branch = if !oid_short.is_empty() {
                    oid_short.clone()
                } else {
                    "HEAD".to_string()
                };
            } else {
                saw_detached = false;
                branch = hd.to_string();
            }
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            if let Some((a, b)) = parse_ahead_behind(rest) {
                ahead = a;
                behind = b;
            }
        } else if line.starts_with("u ") {
            has_conflict = true;
            dirty = true;
        } else if !line.starts_with('#') && !line.starts_with('!') {
            dirty = true;
        }
    }

    if saw_detached && branch == "HEAD" && !oid_short.is_empty() {
        branch = oid_short;
    }

    ParsedGitStatus {
        branch,
        dirty,
        has_conflict,
        ahead,
        behind,
    }
}

fn parse_ahead_behind(rest: &str) -> Option<(i64, i64)> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    for pair in tokens.windows(2) {
        if let Some(a_str) = pair[0].strip_prefix('+')
            && let Some(b_str) = pair[1].strip_prefix('-')
            && !a_str.is_empty()
            && a_str.bytes().all(|b| b.is_ascii_digit())
            && !b_str.is_empty()
            && b_str.bytes().all(|b| b.is_ascii_digit())
            && let Ok(a) = a_str.parse::<i64>()
            && let Ok(b) = b_str.parse::<i64>()
        {
            return Some((a, b));
        }
    }
    None
}
