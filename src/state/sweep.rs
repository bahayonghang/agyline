use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use crate::state::git_cache::atomic_write_in_dir;

const SWEEP_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
const SESSION_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const GIT_CACHE_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;
const TMP_AND_LOCK_MAX_AGE_MS: u64 = 10 * 60 * 1000;

#[derive(Deserialize)]
struct TimestampHeader {
    updated_at_ms: Option<u64>,
}

pub fn maybe_sweep_stale_state(home_dir: &Path, now_ms: u64) {
    let state_dir = home_dir.join("state");
    if !state_dir.is_dir() {
        return;
    }

    let last_sweep_path = state_dir.join("last_sweep");
    if let Ok(raw) = fs::read_to_string(&last_sweep_path)
        && let Ok(last_ms) = raw.trim().parse::<u64>()
        && now_ms.saturating_sub(last_ms) < SWEEP_INTERVAL_MS
    {
        return;
    }

    let stamp = format!("{now_ms}\n");
    let _ = atomic_write_in_dir(&state_dir, "last_sweep", &last_sweep_path, stamp.as_bytes());

    sweep_tmp_files_in_dir(&state_dir, now_ms);
    sweep_sessions_dir(&state_dir.join("sessions"), now_ms);
    sweep_git_dir(&state_dir.join("git"), now_ms);
    sweep_locks_dir(&state_dir.join("locks"), now_ms);
}

fn sweep_tmp_files_in_dir(dir: &Path, now_ms: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains(".tmp.")
            && let Some(age) = mtime_age_ms(&entry.path(), now_ms)
            && age > TMP_AND_LOCK_MAX_AGE_MS
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn sweep_sessions_dir(dir: &Path, now_ms: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains(".tmp.") {
            if mtime_age_ms(&path, now_ms).is_some_and(|age| age > TMP_AND_LOCK_MAX_AGE_MS) {
                let _ = fs::remove_file(&path);
            }
        } else if name_str.ends_with(".json")
            && mtime_age_ms(&path, now_ms).is_some_and(|age| age > SESSION_MAX_AGE_MS)
        {
            let _ = fs::remove_file(&path);
        }
    }
}

fn sweep_git_dir(dir: &Path, now_ms: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains(".tmp.") {
            if mtime_age_ms(&path, now_ms).is_some_and(|age| age > TMP_AND_LOCK_MAX_AGE_MS) {
                let _ = fs::remove_file(&path);
            }
        } else if name_str.ends_with(".json") {
            let age =
                embedded_git_updated_age_ms(&path, now_ms).or_else(|| mtime_age_ms(&path, now_ms));
            if age.is_some_and(|a| a > GIT_CACHE_MAX_AGE_MS) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn sweep_locks_dir(dir: &Path, now_ms: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.contains(".tmp.") {
            if mtime_age_ms(&path, now_ms).is_some_and(|age| age > TMP_AND_LOCK_MAX_AGE_MS) {
                let _ = fs::remove_file(&path);
            }
        } else if name_str.ends_with(".lock") {
            let age = embedded_lock_age_ms(&path, now_ms).or_else(|| mtime_age_ms(&path, now_ms));
            if age.is_some_and(|a| a > TMP_AND_LOCK_MAX_AGE_MS) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn embedded_git_updated_age_ms(path: &Path, now_ms: u64) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    let hdr: TimestampHeader = serde_json::from_str(&raw).ok()?;
    Some(now_ms.saturating_sub(hdr.updated_at_ms?))
}

fn embedded_lock_age_ms(path: &Path, now_ms: u64) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    let created_ms = raw.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(now_ms.saturating_sub(created_ms))
}

fn mtime_age_ms(path: &Path, now_ms: u64) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let mtime_ms = modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    Some(now_ms.saturating_sub(mtime_ms))
}
