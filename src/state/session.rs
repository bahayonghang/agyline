use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::platform;
use crate::state::{atomic_write_bytes, ensure_state_dir, fnv1a64};

pub const SESSION_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbFileStamp {
    pub len: u64,
    pub mtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbFingerprint {
    pub db: Option<DbFileStamp>,
    pub wal: Option<DbFileStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

pub fn session_key(session_id: &str) -> String {
    if is_lowercase_uuid(session_id) {
        session_id.to_string()
    } else {
        format!("id-{:016x}", fnv1a64(session_id.as_bytes()))
    }
}

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
