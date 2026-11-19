//! Shared helpers for the `state/` submodules: the private state dir, the FNV-1a hash behind
//! collision-checked file keys, and the atomic tmp-then-rename write all state files use.
//! Nothing here touches the relay's read-only `agy_cache_*.dat` in the OS temp dir.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub use self::git_cache::{
    GitCacheEntry, cwd_hash_hex, git_cache_path, git_lock_path, normalize_cwd_lexical,
    read_git_cache, write_git_cache,
};
pub use self::sweep::maybe_sweep_stale_state;
use crate::platform;

pub mod git_cache;
pub mod lock;
pub mod relay;
pub mod session;
pub mod sweep;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Creates `<home>/state` as a private (owner-only) directory and returns it. All session, git,
/// lock, and sweep files live here, never in the world-readable temp dir the relay uses.
pub fn ensure_state_dir(home_dir: &Path) -> io::Result<PathBuf> {
    let dir = home_dir.join("state");
    platform::ensure_private_dir(&dir)?;
    Ok(dir)
}

/// FNV-1a 64-bit hash (offset basis `0xcbf29ce484222325`, prime `0x100000001b3`). Used to derive
/// short file keys from cwd/session ids. Callers also embed the original value: a 64-bit hash can
/// collide, so a hash match alone is not proof of identity (treat a collision as a miss).
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Atomically replaces `<dir>/<target_name>`: write a unique `O_EXCL` tmp file, then rename it
/// over the target, so a reader never sees a half-written file. The tmp name embeds pid+nanos+seq
/// so concurrent writers cannot collide; on write failure the tmp is removed (no orphan left).
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
