use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::platform;
use crate::state::ensure_state_dir;

const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01b3;
const GIT_LOCK_STALE_MS: u64 = 30_000;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

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
}

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

pub fn cwd_hash_hex(normalized_cwd: &str) -> String {
    let mut hash = FNV1A_64_OFFSET_BASIS;
    for &byte in normalized_cwd.as_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    format!("{hash:016x}")
}

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
        })
    }
}

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

pub(crate) struct GitRefreshLock {
    path: PathBuf,
    marker: String,
}

impl Drop for GitRefreshLock {
    fn drop(&mut self) {
        if let Ok(current) = fs::read_to_string(&self.path)
            && current == self.marker
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn acquire_git_lock(
    home_dir: &Path,
    raw_cwd: &str,
    now_ms: u64,
) -> Option<GitRefreshLock> {
    let norm = normalize_cwd_lexical(raw_cwd);
    if norm.is_empty() {
        return None;
    }
    let state_dir = ensure_state_dir(home_dir).ok()?;
    let locks_dir = state_dir.join("locks");
    platform::ensure_private_dir(&locks_dir).ok()?;

    let hash = cwd_hash_hex(&norm);
    let lock_path = locks_dir.join(format!("git-{hash}.lock"));
    let marker = format!("{now_ms} {}\n", std::process::id());

    match try_create_lock(&lock_path, &marker) {
        Ok(lock) => Some(lock),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            if is_lock_stale(&lock_path, now_ms) {
                let _ = fs::remove_file(&lock_path);
                try_create_lock(&lock_path, &marker).ok()
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

fn try_create_lock(lock_path: &Path, marker: &str) -> io::Result<GitRefreshLock> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)?;
    file.write_all(marker.as_bytes())?;
    drop(file);
    Ok(GitRefreshLock {
        path: lock_path.to_path_buf(),
        marker: marker.to_string(),
    })
}

fn is_lock_stale(lock_path: &Path, now_ms: u64) -> bool {
    if let Ok(raw) = fs::read_to_string(lock_path)
        && let Some(first) = raw.split_whitespace().next()
        && let Ok(created_ms) = first.parse::<u64>()
    {
        return now_ms.saturating_sub(created_ms) >= GIT_LOCK_STALE_MS;
    }
    if let Ok(meta) = fs::metadata(lock_path)
        && let Ok(modified) = meta.modified()
        && let Ok(dur) = modified.duration_since(UNIX_EPOCH)
    {
        let mtime_ms = dur.as_millis() as u64;
        return now_ms.saturating_sub(mtime_ms) >= GIT_LOCK_STALE_MS;
    }
    false
}

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

fn strip_trailing_seps_with_min_len(s: &str, min_len: usize) -> String {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > min_len && is_sep(bytes[end - 1]) {
        end -= 1;
    }
    s[..end].to_string()
}
