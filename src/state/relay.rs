//! Reads the cache relay's state file (`agy_cache_<sid>.dat` in the OS temp dir) to recover the
//! prompt-cache anchor, touch count, and Live/Fail status. READ-ONLY: agyline never writes this
//! file; `cache_relay.py` owns it. A missing/garbled file is `None`, so the render falls back to
//! its own cache clock instead of failing.
use std::fs;
use std::path::Path;

use crate::collect::RelayStatus;

/// Parsed relay state file (`ANCHOR_TS TOUCHES STATUS`). `anchor_ts` is the last-touch anchor in
/// unix seconds (`None` if absent/non-positive); `touches` is the keepalive count (0 if bad);
/// `status` is Live/Fail (`None` for any other token). Mirrors what `cache_relay.py` writes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelayFileState {
    pub anchor_ts: Option<i64>,
    pub touches: u32,
    pub status: Option<RelayStatus>,
}

/// Maps any char outside `[A-Za-z0-9_-]` to `_`, matching `cache_relay.py`'s `SAFE_ID_RE`. The
/// sanitized id must match the relay's, or agyline reads a `.dat` name the relay never wrote.
pub fn sanitize_relay_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Reads `<temp>/agy_cache_<safe_sid>.dat` (the relay's file) and parses it. READ-ONLY: agyline
/// never creates or writes this file. Returns `None` for an empty id or any read/parse failure,
/// so a missing relay means no anchor override, not an error on the render path.
pub fn read_relay_state(temp_dir: &Path, session_id: &str) -> Option<RelayFileState> {
    if session_id.is_empty() {
        return None;
    }
    let safe_sid = sanitize_relay_session_id(session_id);
    let relay_path = temp_dir.join(format!("agy_cache_{safe_sid}.dat"));
    let raw = fs::read_to_string(relay_path).ok()?;
    parse_relay_state(&raw)
}

/// Parses the relay's `ANCHOR_TS TOUCHES STATUS` line (all three whitespace fields required). A
/// missing/non-numeric anchor rejects the file; a non-positive one leaves `anchor_ts` None.
/// Touches default to 0, unknown status to None: a partly-garbled file still yields an anchor.
fn parse_relay_state(raw: &str) -> Option<RelayFileState> {
    let mut parts = raw.split_whitespace();
    let ts_part = parts.next()?;
    let touches_part = parts.next()?;
    let status_part = parts.next()?;

    if ts_part.is_empty() || !ts_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parsed_ts = ts_part.parse::<i64>().ok()?;
    let anchor_ts = (parsed_ts > 0).then_some(parsed_ts);

    let touches = if !touches_part.is_empty() && touches_part.bytes().all(|b| b.is_ascii_digit()) {
        touches_part.parse::<u32>().unwrap_or(0)
    } else {
        0
    };

    let status = if status_part.eq_ignore_ascii_case("live") {
        Some(RelayStatus::Live)
    } else if status_part.eq_ignore_ascii_case("fail") {
        Some(RelayStatus::Fail)
    } else {
        None
    };

    Some(RelayFileState {
        anchor_ts,
        touches,
        status,
    })
}
