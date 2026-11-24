//! Zero-blocking transcript helpers: an O(1) bounded header micro-reader (`<= 4 KiB`) for the
//! render path to grab session start time, plus a detached `__refresh-transcript` background worker
//! for optional session-level token and cache totals when stdin omits them.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::input::Payload;
use crate::items::usage::parse_rfc3339_epoch_secs;
use crate::platform;
use crate::state::lock::ScopedLock;
use crate::state::maybe_sweep_stale_state;
use crate::state::session::{
    SESSION_STATE_VERSION, SessionState, read_file_stamp, read_session_state, session_key,
    write_session_state,
};

const TRANSCRIPT_HEADER_BUDGET: u64 = 4096;
const TRANSCRIPT_LOCK_STALE_MS: u64 = 30_000;

#[derive(Debug, Deserialize)]
struct TranscriptHeaderRow {
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TranscriptPlannerRow {
    #[serde(rename = "type")]
    kind: Option<String>,
    created_at: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
}

/// Resolves the transcript path from `payload.transcript_path`, else standard conversation logs.
pub(crate) fn resolve_transcript_path(
    payload: &Payload,
    user_home: Option<&Path>,
) -> Option<PathBuf> {
    if !payload.transcript_path.trim().is_empty() {
        return Some(PathBuf::from(&payload.transcript_path));
    }
    let sid = payload.session_id.as_deref()?;
    if !is_safe_session_segment(sid) {
        return None;
    }
    let home = user_home?;
    let cli_path = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("brain")
        .join(sid)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    if cli_path.is_file() {
        return Some(cli_path);
    }
    let agy_path = home
        .join(".gemini")
        .join("antigravity")
        .join("brain")
        .join(sid)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    Some(agy_path)
}

/// Reads at most 4 KiB from the head of `path` to find the earliest `created_at` RFC3339 timestamp.
/// Guaranteed O(1) I/O regardless of transcript size.
pub fn read_transcript_start_sec_bounded(path: &Path) -> Option<i64> {
    let file = File::open(path).ok()?;
    let mut buf = Vec::with_capacity(TRANSCRIPT_HEADER_BUDGET as usize);
    file.take(TRANSCRIPT_HEADER_BUDGET)
        .read_to_end(&mut buf)
        .ok()?;
    for raw_line in buf.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(raw_line);
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.contains("\"created_at\"") {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<TranscriptHeaderRow>(trimmed)
            && let Some(ts) = row.created_at.as_deref().and_then(parse_rfc3339_epoch_secs)
        {
            return Some(ts);
        }
    }
    None
}

/// Detached `__refresh-transcript` worker: streams the transcript off the printing path and stores
/// session token totals and cache-hit numerator in `SessionState`. Silent on bad args or errors.
pub fn run_hidden_refresh_transcript(tail: &[OsString]) {
    let Some((session_id, path_os)) = parse_transcript_args(tail) else {
        return;
    };
    if !is_safe_session_segment(&session_id) {
        return;
    }
    let path = PathBuf::from(&path_os);
    let Some(stamp) = read_file_stamp(&path) else {
        return;
    };

    let home_dir = resolve_process_home_dir();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let key = session_key(&session_id);
    if key.is_empty() {
        return;
    }
    let Some(_lock) = ScopedLock::acquire(
        &home_dir,
        &format!("transcript-{key}.lock"),
        now_ms,
        TRANSCRIPT_LOCK_STALE_MS,
    ) else {
        return;
    };

    if let Some(existing) = read_session_state(&home_dir, &session_id)
        && existing.transcript_fp.as_ref() == Some(&stamp)
        && existing.transcript_cache_valid
    {
        return;
    }

    let Ok(file) = File::open(&path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut first_ts: Option<i64> = None;
    let mut total_in: i64 = 0;
    let mut total_out: i64 = 0;
    let mut cache_read: i64 = 0;
    let mut saw_tokens = false;
    let mut missing_cache = false;

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return,
        }
        let has_created = first_ts.is_none() && line.contains("\"created_at\"");
        let has_planner = line.contains("PLANNER_RESPONSE");
        if !has_created && !has_planner {
            continue;
        }
        let Ok(row) = serde_json::from_str::<TranscriptPlannerRow>(&line) else {
            continue;
        };
        if has_created && first_ts.is_none() {
            first_ts = row.created_at.as_deref().and_then(parse_rfc3339_epoch_secs);
        }
        if row.kind.as_deref() == Some("PLANNER_RESPONSE") {
            let has_any = row.input_tokens.is_some()
                || row.output_tokens.is_some()
                || row.cache_read_tokens.is_some()
                || row.cache_creation_tokens.is_some()
                || row.cache_creation_input_tokens.is_some();
            if has_any {
                saw_tokens = true;
                let inp = row.input_tokens.unwrap_or(0).max(0);
                let cr = row.cache_read_tokens.unwrap_or(0).max(0);
                let cc = row
                    .cache_creation_tokens
                    .or(row.cache_creation_input_tokens)
                    .unwrap_or(0)
                    .max(0);
                let turn_in = inp.saturating_add(cr).saturating_add(cc);
                total_in = total_in.saturating_add(turn_in);
                total_out = total_out.saturating_add(row.output_tokens.unwrap_or(0).max(0));
                cache_read = cache_read.saturating_add(cr);
                if turn_in > 0 && row.cache_read_tokens.is_none() {
                    missing_cache = true;
                }
            }
        }
    }

    // Re-read state right before writing so concurrent render updates are preserved.
    let mut next_state =
        read_session_state(&home_dir, &session_id).unwrap_or_else(|| SessionState {
            version: SESSION_STATE_VERSION,
            session_id: session_id.clone(),
            prev_state: "idle".to_string(),
            last_tps: "--".to_string(),
            ..SessionState::default()
        });
    if next_state.transcript_first_ts.is_none() {
        next_state.transcript_first_ts = first_ts;
    }
    next_state.transcript_fp = Some(stamp);
    if saw_tokens {
        next_state.transcript_total_in = Some(total_in);
        next_state.transcript_total_out = Some(total_out);
        next_state.transcript_cache_read = Some(cache_read);
        next_state.transcript_cache_valid = !missing_cache;
    } else {
        next_state.transcript_cache_valid = false;
    }
    write_session_state(&home_dir, &next_state);
    maybe_sweep_stale_state(&home_dir, now_ms);
}

fn parse_transcript_args(tail: &[OsString]) -> Option<(String, OsString)> {
    let mut session_id: Option<String> = None;
    let mut path: Option<OsString> = None;
    let mut i = 0;

    while i < tail.len() {
        let arg = tail.get(i)?.to_str()?;
        if arg == "--session" {
            if session_id.is_some() {
                return None;
            }
            session_id = Some(tail.get(i + 1)?.to_str()?.to_string());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--session=") {
            if session_id.is_some() {
                return None;
            }
            session_id = Some(val.to_string());
            i += 1;
        } else if arg == "--path" {
            if path.is_some() {
                return None;
            }
            let val = tail.get(i + 1)?;
            if val.is_empty() {
                return None;
            }
            path = Some(val.clone());
            i += 2;
        } else {
            let val = tail.get(i)?.to_str()?.strip_prefix("--path=")?;
            if path.is_some() || val.is_empty() {
                return None;
            }
            path = Some(OsString::from(val));
            i += 1;
        }
    }

    Some((session_id?, path?))
}

fn is_safe_session_segment(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id != "."
        && session_id != ".."
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn resolve_process_home_dir() -> PathBuf {
    let lookup = |k: &str| std::env::var(k).ok();
    let user_home = platform::user_home_dir(&lookup).unwrap_or_else(|| PathBuf::from("."));
    crate::config::resolve_home_dir(&lookup, &user_home)
}
