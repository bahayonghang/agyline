use std::fs;
use std::path::Path;

use crate::collect::RelayStatus;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelayFileState {
    pub anchor_ts: Option<i64>,
    pub touches: u32,
    pub status: Option<RelayStatus>,
}

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

pub fn read_relay_state(temp_dir: &Path, session_id: &str) -> Option<RelayFileState> {
    if session_id.is_empty() {
        return None;
    }
    let safe_sid = sanitize_relay_session_id(session_id);
    let relay_path = temp_dir.join(format!("agy_cache_{safe_sid}.dat"));
    let raw = fs::read_to_string(relay_path).ok()?;
    parse_relay_state(&raw)
}

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
