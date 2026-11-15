use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::platform;
use crate::state::ensure_state_dir;

pub struct ScopedLock {
    path: PathBuf,
    token: String,
}

impl ScopedLock {
    pub fn acquire(
        home_dir: &Path,
        lock_name: &str,
        now_ms: u64,
        stale_after_ms: u64,
    ) -> Option<Self> {
        if lock_name.is_empty() || lock_name.contains(['/', '\\', '\0']) {
            return None;
        }
        let state_dir = ensure_state_dir(home_dir).ok()?;
        let locks_dir = state_dir.join("locks");
        platform::ensure_private_dir(&locks_dir).ok()?;

        let lock_path = locks_dir.join(lock_name);
        let token = format!("{now_ms} {}\n", std::process::id());

        match try_create_lock(&lock_path, &token) {
            Ok(()) => {
                return Some(Self {
                    path: lock_path,
                    token,
                });
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }

        if !is_lock_stale(&lock_path, now_ms, stale_after_ms) {
            return None;
        }

        let _ = fs::remove_file(&lock_path);
        try_create_lock(&lock_path, &token).ok().map(|()| Self {
            path: lock_path,
            token,
        })
    }
}

impl Drop for ScopedLock {
    fn drop(&mut self) {
        if let Ok(existing) = fs::read_to_string(&self.path)
            && existing == self.token
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn try_create_lock(path: &Path, token: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(err) = file.write_all(token.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(err);
    }
    Ok(())
}

fn is_lock_stale(path: &Path, now_ms: u64, stale_after_ms: u64) -> bool {
    if let Ok(raw) = fs::read_to_string(path)
        && let Some(first) = raw.split_whitespace().next()
        && !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && let Ok(created_ms) = first.parse::<u64>()
    {
        return now_ms.saturating_sub(created_ms) >= stale_after_ms;
    }

    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(dur) = modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let mtime_ms = dur.as_millis() as u64;
    now_ms.saturating_sub(mtime_ms) >= stale_after_ms
}
