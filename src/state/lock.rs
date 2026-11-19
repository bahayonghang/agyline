//! RAII advisory file locks under `<home>/state/locks`. `acquire` creates the lock with `O_EXCL`;
//! if it already exists, only a stale lock (older than `stale_after_ms`) is taken over. `Drop`
//! removes the file only when its token still matches, so a lock stolen and re-acquired by
//! another worker is never deleted by the original owner. Serializes git refresh and touches.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::platform;
use crate::state::ensure_state_dir;

/// A held lock file. Its `token` (created-ms + pid) is written at acquire and re-checked at drop,
/// so only the owner whose token still matches removes the file. Dropping releases the lock.
pub struct ScopedLock {
    path: PathBuf,
    token: String,
}

impl ScopedLock {
    /// Acquires `<home>/state/locks/<lock_name>` exclusively. `None` if the name is empty/has a
    /// separator, the dir cannot be made, or a non-stale lock is held. A lock older than
    /// `stale_after_ms` is removed and retried once, so a crashed worker cannot wedge the lock.
    pub fn acquire(
        home_dir: &Path,
        lock_name: &str,
        now_ms: u64,
        stale_after_ms: u64,
    ) -> Option<Self> {
        // lock_name becomes a filename: reject separators/NUL so it cannot escape the locks dir.
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
            // AlreadyExists is not a failure: fall through to the stale-takeover check below.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }

        if !is_lock_stale(&lock_path, now_ms, stale_after_ms) {
            return None;
        }

        // Stale: remove and re-create with O_EXCL so a race cannot leave two owners.
        let _ = fs::remove_file(&lock_path);
        try_create_lock(&lock_path, &token).ok().map(|()| Self {
            path: lock_path,
            token,
        })
    }
}

impl Drop for ScopedLock {
    fn drop(&mut self) {
        // Remove only if the file still holds our token, never one a peer re-acquired since.
        if let Ok(existing) = fs::read_to_string(&self.path)
            && existing == self.token
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Creates the lock with `O_EXCL` (fails if it exists) and writes the token. On a write error the
/// half-created file is removed, so a failed acquire never leaves a tokenless lock behind.
fn try_create_lock(path: &Path, token: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(err) = file.write_all(token.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(err);
    }
    Ok(())
}

/// True if the lock is older than `stale_after_ms`. Prefers the created-ms embedded in the token;
/// falls back to the file mtime if the content is unreadable/unparseable. Unreadable metadata is
/// treated as not-stale, so a lock we cannot judge is never stolen.
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
