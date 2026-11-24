//! Per-session render state persisted under `<home>/state/sessions/<key>.json`, so the output-rate
//! baseline, cache clock, and last response model survive between renders. The file key is the
//! session id (a lowercase UUID) or an FNV-1a hash of it; reads reject a version or id mismatch,
//! so a stale or colliding file is treated as absent, never applied to the wrong session.
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::platform;
use crate::state::{atomic_write_bytes, ensure_state_dir, fnv1a64};

// Bumped when the persisted SessionState shape changes; a file with any other version is rejected
// on read (treated as absent) so an old layout can never be misread into the new fields.
pub const SESSION_STATE_VERSION: u32 = 1;

/// A file's size + mtime (ms since epoch): detects conversation-DB changes without reading it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbFileStamp {
    pub len: u64,
    pub mtime_ms: u64,
}

/// Fingerprint of the conversation DB and its `-wal` sidecar. Both are tracked because SQLite can
/// commit to the WAL without touching the main file, so either changing signals new data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbFingerprint {
    pub db: Option<DbFileStamp>,
    pub wal: Option<DbFileStamp>,
}

/// The persisted per-session render state. `version`/`session_id` are checked on read; the rest
/// carry the output-rate baseline, cache clock, last response model, and bounded transcript stats.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub session_id: String,
    pub prev_state: String,
    pub start_time_ms: u64,
    pub base_out: i64,
    pub last_tps: String,
    pub resp_model: String,
    pub prev_model_key: String,
    pub cache_ts: i64,
    pub db_fingerprint: Option<DbFingerprint>,
    #[serde(default)]
    pub first_seen_sec: i64,
    #[serde(default)]
    pub transcript_first_ts: Option<i64>,
    #[serde(default)]
    pub transcript_fp: Option<DbFileStamp>,
    #[serde(default)]
    pub transcript_total_in: Option<i64>,
    #[serde(default)]
    pub transcript_total_out: Option<i64>,
    #[serde(default)]
    pub transcript_cache_read: Option<i64>,
    #[serde(default)]
    pub transcript_cache_valid: bool,
}

/// File key for a session: a lowercase UUID is used verbatim (safe and unique); any other id
/// becomes `id-<fnv1a64:016x>`. Callers must still verify the embedded session_id on read: the
/// 64-bit hash can collide, so a key match alone is not proof of identity.
pub fn session_key(session_id: &str) -> String {
    if is_lowercase_uuid(session_id) {
        session_id.to_string()
    } else {
        format!("id-{:016x}", fnv1a64(session_id.as_bytes()))
    }
}

/// Loads `<home>/state/sessions/<key>.json`. `None` if absent/unreadable, the version differs from
/// SESSION_STATE_VERSION, or the stored session_id differs from the requested one. The id check
/// guards against an FNV-1a key collision applying another session's state.
pub fn read_session_state(home_dir: &Path, session_id: &str) -> Option<SessionState> {
    if session_id.is_empty() {
        return None;
    }
    let key = session_key(session_id);
    let path = home_dir
        .join("state")
        .join("sessions")
        .join(format!("{key}.json"));
    let bytes = fs::read(path).ok()?;
    let parsed: SessionState = serde_json::from_slice(&bytes).ok()?;
    if parsed.version != SESSION_STATE_VERSION || parsed.session_id != session_id {
        return None;
    }
    Some(parsed)
}

/// Persists `state` to `<home>/state/sessions/<key>.json` via the atomic tmp-then-rename write.
/// Best-effort: an empty id, dir/serialize failure, or write error is silently ignored. A missing
/// state file only costs a cold baseline on the next render, never a wrong line.
pub fn write_session_state(home_dir: &Path, state: &SessionState) {
    if state.session_id.is_empty() {
        return;
    }
    let Ok(state_dir) = ensure_state_dir(home_dir) else {
        return;
    };
    let sessions_dir = state_dir.join("sessions");
    if platform::ensure_private_dir(&sessions_dir).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(state) else {
        return;
    };
    let target_name = format!("{}.json", session_key(&state.session_id));
    let _ = atomic_write_bytes(&sessions_dir, &target_name, &bytes);
}

/// Stamps the conversation DB and `-wal` sidecar (size + mtime). `None` if the DB is not a file.
/// Cheap metadata only: this runs on the render path, so it must not open or scan the database.
pub fn read_db_fingerprint(conv_db: &Path) -> Option<DbFingerprint> {
    let db_meta = fs::metadata(conv_db).ok().filter(|m| m.is_file())?;
    let db_stamp = file_stamp(&db_meta);

    let mut wal_os = conv_db.as_os_str().to_os_string();
    wal_os.push("-wal");
    let wal_stamp = fs::metadata(Path::new(&wal_os))
        .ok()
        .filter(|m| m.is_file())
        .map(|m| file_stamp(&m));

    Some(DbFingerprint {
        db: Some(db_stamp),
        wal: wal_stamp,
    })
}

/// Stamps one file (size + mtime) without reading its contents.
pub fn read_file_stamp(path: &Path) -> Option<DbFileStamp> {
    let meta = fs::metadata(path).ok().filter(|m| m.is_file())?;
    Some(file_stamp(&meta))
}

fn file_stamp(meta: &fs::Metadata) -> DbFileStamp {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    DbFileStamp {
        len: meta.len(),
        mtime_ms,
    }
}

/// True for a canonical lowercase UUID (36 chars, dashes at 8/13/18/23, hex elsewhere). These are
/// used as file keys verbatim; the strict check keeps a non-UUID id from becoming a raw filename.
fn is_lowercase_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    for (idx, b) in s.bytes().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if b != b'-' {
                return false;
            }
        } else if !matches!(b, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
    }
    true
}
