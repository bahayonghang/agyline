use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::platform;

pub mod lock;
pub mod relay;
pub mod session;

const SWEEP_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
const SESSION_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const GIT_RETENTION_MS: u64 = 24 * 60 * 60 * 1000;
const EPHEMERAL_RETENTION_MS: u64 = 10 * 60 * 1000;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn ensure_state_dir(home_dir: &Path) -> io::Result<PathBuf> {
    let dir = home_dir.join("state");
    platform::ensure_private_dir(&dir)?;
    Ok(dir)
}

pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(crate) fn atomic_write_bytes(dir: &Path, target_name: &str, bytes: &[u8]) -> io::Result<()> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!("{target_name}.tmp.{}.{}_{}", std::process::id(), nanos, seq);
    let tmp_path = dir.join(tmp_name);
    let dst_path = dir.join(target_name);

    let write_res = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(bytes)
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    platform::replace_file_with_retry(&tmp_path, &dst_path)
}

pub fn maybe_sweep_stale_state(home_dir: &Path, now_ms: u64) {
    let Ok(state_dir) = ensure_state_dir(home_dir) else {
        return;
    };
    let marker = state_dir.join("last_sweep");
    if let Ok(raw) = fs::read_to_string(&marker)
        && let Ok(last_ms) = raw.trim().parse::<u64>()
        && now_ms.saturating_sub(last_ms) < SWEEP_INTERVAL_MS
    {
        return;
    }

    let _ = atomic_write_bytes(&state_dir, "last_sweep", format!("{now_ms}\n").as_bytes());
    sweep_dir_older_than(
        &state_dir.join("sessions"),
        now_ms,
        SESSION_RETENTION_MS,
        ".json",
    );
    sweep_dir_older_than(&state_dir.join("git"), now_ms, GIT_RETENTION_MS, ".json");
    sweep_dir_older_than(
        &state_dir.join("locks"),
        now_ms,
        EPHEMERAL_RETENTION_MS,
        ".lock",
    );
    sweep_tmp_files(&state_dir.join("sessions"), now_ms, EPHEMERAL_RETENTION_MS);
    sweep_tmp_files(&state_dir.join("git"), now_ms, EPHEMERAL_RETENTION_MS);
    sweep_tmp_files(&state_dir, now_ms, EPHEMERAL_RETENTION_MS);
}

fn sweep_dir_older_than(dir: &Path, now_ms: u64, max_age_ms: u64, suffix: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(suffix) {
            continue;
        }
        if is_entry_older_than(&entry, now_ms, max_age_ms) {
            let _ = fs::remove_file(path);
        }
    }
}

fn sweep_tmp_files(dir: &Path, now_ms: u64, max_age_ms: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.contains(".tmp.") {
            continue;
        }
        if is_entry_older_than(&entry, now_ms, max_age_ms) {
            let _ = fs::remove_file(path);
        }
    }
}

fn is_entry_older_than(entry: &fs::DirEntry, now_ms: u64, max_age_ms: u64) -> bool {
    let Ok(meta) = entry.metadata() else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(dur) = modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let mtime_ms = dur.as_millis() as u64;
    now_ms.saturating_sub(mtime_ms) >= max_age_ms
}
