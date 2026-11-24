//! Git-status cache keyed by a lexical-normalized cwd: the FNV-1a hash names the file, and the
//! normalized cwd is embedded so a hash collision is detected as a miss. Reads are stale-tolerant
//! (the render never waits on git); a detached worker refreshes under a `ScopedLock`, writing via
//! a tmp-then-rename. Locking reuses `state::lock`; the atomic write here is a separate helper.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::platform;
use crate::state::{ensure_state_dir, lock::ScopedLock};

// Standard FNV-1a 64-bit parameters; same hash as `state::fnv1a64`, keyed on the normalized cwd.
const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01b3;
// 30s: comfortably above the 10s git command timeout, so a live refresh's lock is never stolen.
const GIT_LOCK_STALE_MS: u64 = 30_000;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A cached git snapshot for one cwd. `version`/`cwd` are validated on read (mismatch = miss);
/// `updated_at_ms` drives staleness. Written by the refresh worker, read on the render path.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GitCacheEntry {
    pub version: u32,
    pub cwd: String,
    pub updated_at_ms: u64,
    pub is_repo: bool,
    pub branch: String,
    pub dirty: bool,
    pub has_conflict: bool,
    pub ahead: i64,
    pub behind: i64,
    #[serde(default)]
    pub conflicted: u32,
    #[serde(default)]
    pub staged: u32,
    #[serde(default)]
    pub modified: u32,
    #[serde(default)]
    pub deleted: u32,
    #[serde(default)]
    pub renamed: u32,
    #[serde(default)]
    pub untracked: u32,
    #[serde(default)]
    pub stashed: u32,
}

/// On-disk shape with optional git fields: a non-repo entry omits branch/dirty/etc. The read path
/// requires them only when `is_repo`, so a partial entry for a non-repo still parses.
#[derive(Debug, Deserialize)]
struct RawGitCacheEntry {
    version: u32,
    cwd: String,
    updated_at_ms: u64,
    is_repo: bool,
    branch: Option<String>,
    dirty: Option<bool>,
    has_conflict: Option<bool>,
    ahead: Option<i64>,
    behind: Option<i64>,
    #[serde(default)]
    conflicted: u32,
    #[serde(default)]
    staged: u32,
    #[serde(default)]
    modified: u32,
    #[serde(default)]
    deleted: u32,
    #[serde(default)]
    renamed: u32,
    #[serde(default)]
    untracked: u32,
    #[serde(default)]
    stashed: u32,
}

/// Normalizes a cwd for hashing: strips trailing separators, keeping a minimum length so roots
/// like `/`, a drive root, or a UNC share survive. Extended-length Windows paths are kept intact.
/// Filesystem access is avoided, so this is safe and cheap on the render path.
pub fn normalize_cwd_lexical(cwd: &str) -> String {
    if cwd.trim().is_empty() {
        return String::new();
    }

    let bytes = cwd.as_bytes();

    // If the entire string consists solely of path separators, preserve root separator.
    if bytes.iter().all(|&b| b == b'/' || b == b'\\') {
        if cwd.starts_with("\\\\") {
            return "\\\\".to_string();
        }
        return cwd[..1].to_string();
    }

    // Extended-length Windows drive path: `\\?\D:\...`
    if bytes.len() >= 7
        && is_sep(bytes[0])
        && is_sep(bytes[1])
        && bytes[2] == b'?'
        && is_sep(bytes[3])
        && bytes[4].is_ascii_alphabetic()
        && bytes[5] == b':'
        && is_sep(bytes[6])
    {
        return strip_trailing_seps_with_min_len(cwd, 7);
    }

    // Standard Windows drive path: `D:\...` or `C:/...`
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && is_sep(bytes[2]) {
        return strip_trailing_seps_with_min_len(cwd, 3);
    }

    // UNC share path (`\\server\share\...`) or POSIX/normal path:
    // stripping trailing `/` and `\` turns `\\server\share\` into `\\server\share`
    // and `/usr/local/` into `/usr/local` while keeping minimum length >= 2 for UNC.
    let min_len = if cwd.starts_with("\\\\") { 2 } else { 1 };
    strip_trailing_seps_with_min_len(cwd, min_len)
}

/// FNV-1a 64-bit hash of the normalized cwd, as 16 lowercase hex digits. It names the cache/lock
/// files; it is not a security hash, and collisions are caught by the embedded-cwd check on read.
pub fn cwd_hash_hex(normalized_cwd: &str) -> String {
    let mut hash = FNV1A_64_OFFSET_BASIS;
    for &byte in normalized_cwd.as_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    format!("{hash:016x}")
}

/// Cache file path `<home>/state/git/<hash>.json`, or `None` if the cwd normalizes to empty.
pub fn git_cache_path(home_dir: &Path, raw_cwd: &str) -> Option<PathBuf> {
    let norm = normalize_cwd_lexical(raw_cwd);
    if norm.is_empty() {
        return None;
    }
    let hash = cwd_hash_hex(&norm);
    Some(
        home_dir
            .join("state")
            .join("git")
            .join(format!("{hash}.json")),
    )
}

/// Lock file path `<home>/state/locks/git-<hash>.lock`, or `None` if the cwd normalizes to empty.
pub fn git_lock_path(home_dir: &Path, raw_cwd: &str) -> Option<PathBuf> {
    let norm = normalize_cwd_lexical(raw_cwd);
    if norm.is_empty() {
        return None;
    }
    let hash = cwd_hash_hex(&norm);
    Some(
        home_dir
            .join("state")
            .join("locks")
            .join(format!("git-{hash}.lock")),
    )
}

/// Reads the cached git snapshot for `raw_cwd`. `None` (miss) if absent, version not 1, or the
/// embedded cwd differs from the normalized cwd. That check turns an FNV-1a hash collision into a
/// miss rather than applying another repo's status to this cwd.
pub fn read_git_cache(home_dir: &Path, raw_cwd: &str) -> Option<GitCacheEntry> {
    let norm = normalize_cwd_lexical(raw_cwd);
    if norm.is_empty() {
        return None;
    }
    let hash = cwd_hash_hex(&norm);
    let path = home_dir
        .join("state")
        .join("git")
        .join(format!("{hash}.json"));
    let raw_text = fs::read_to_string(path).ok()?;
    let parsed: RawGitCacheEntry = serde_json::from_str(&raw_text).ok()?;
    if parsed.version != 1 || parsed.cwd != norm {
        return None;
    }

    if parsed.is_repo {
        Some(GitCacheEntry {
            version: parsed.version,
            cwd: parsed.cwd,
            updated_at_ms: parsed.updated_at_ms,
            is_repo: true,
            branch: parsed.branch?,
            dirty: parsed.dirty?,
            has_conflict: parsed.has_conflict?,
            ahead: parsed.ahead?,
            behind: parsed.behind?,
            conflicted: parsed.conflicted,
            staged: parsed.staged,
            modified: parsed.modified,
            deleted: parsed.deleted,
            renamed: parsed.renamed,
            untracked: parsed.untracked,
            stashed: parsed.stashed,
        })
    } else {
        Some(GitCacheEntry {
            version: parsed.version,
            cwd: parsed.cwd,
            updated_at_ms: parsed.updated_at_ms,
            is_repo: false,
            branch: parsed.branch.unwrap_or_default(),
            dirty: parsed.dirty.unwrap_or(false),
            has_conflict: parsed.has_conflict.unwrap_or(false),
            ahead: parsed.ahead.unwrap_or(0),
            behind: parsed.behind.unwrap_or(0),
            conflicted: parsed.conflicted,
            staged: parsed.staged,
            modified: parsed.modified,
            deleted: parsed.deleted,
            renamed: parsed.renamed,
            untracked: parsed.untracked,
            stashed: parsed.stashed,
        })
    }
}

/// Writes `entry` to the git cache atomically, forcing version 1 and the normalized cwd so the
/// read path's collision check passes. An empty cwd is a no-op. Errors propagate to the refresh
/// worker, which ignores them: a failed write only costs a re-refresh, never a wrong line.
pub fn write_git_cache(home_dir: &Path, entry: &GitCacheEntry) -> io::Result<()> {
    let norm = normalize_cwd_lexical(&entry.cwd);
    if norm.is_empty() {
        return Ok(());
    }
    let state_dir = ensure_state_dir(home_dir)?;
    let git_dir = state_dir.join("git");
    platform::ensure_private_dir(&git_dir)?;

    let hash = cwd_hash_hex(&norm);
    let dst_path = git_dir.join(format!("{hash}.json"));
    let mut normalized_entry = entry.clone();
    normalized_entry.version = 1;
    normalized_entry.cwd = norm;

    let bytes = serde_json::to_vec(&normalized_entry)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    atomic_write_in_dir(&git_dir, &format!("{hash}.json"), &dst_path, &bytes)
}

/// Acquires the per-cwd git refresh lock (`git-<hash>.lock`) via the shared `ScopedLock`, with a
/// 30s stale window. Serializes refresh workers for one cwd; `None` if the cwd is empty or a live
/// worker holds it.
pub(crate) fn acquire_git_lock(home_dir: &Path, raw_cwd: &str, now_ms: u64) -> Option<ScopedLock> {
    let norm = normalize_cwd_lexical(raw_cwd);
    if norm.is_empty() {
        return None;
    }
    let lock_name = format!("git-{}.lock", cwd_hash_hex(&norm));
    ScopedLock::acquire(home_dir, &lock_name, now_ms, GIT_LOCK_STALE_MS)
}

/// Atomic tmp-then-rename write for the git cache and the sweep's `last_sweep` stamp; tmp name
/// embeds pid+nanos+counter to avoid collisions. Intended invariant: no orphan tmp on failure.
pub(crate) fn atomic_write_in_dir(
    dir: &Path,
    file_name: &str,
    dst_path: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        .wrapping_add(TMP_COUNTER.fetch_add(1, Ordering::Relaxed));
    let tmp_path = dir.join(format!("{file_name}.tmp.{pid}.{nanos}"));

    // Unlike atomic_write_bytes, a write error here returns without removing tmp_path: a failed
    // write can leave a `.tmp.*` orphan; only the rename-failure path below removes the tmp.
    fs::write(&tmp_path, bytes)?;
    match platform::replace_file_with_retry(&tmp_path, dst_path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(err)
        }
    }
}

fn is_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

/// Strips trailing separators but never below `min_len`, so a root path keeps its separator.
fn strip_trailing_seps_with_min_len(s: &str, min_len: usize) -> String {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > min_len && is_sep(bytes[end - 1]) {
        end -= 1;
    }
    s[..end].to_string()
}
