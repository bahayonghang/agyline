//! Bounded response-model lookup from the conversation SQLite DB. The picker name and upstream
//! model can differ, so the line reads the real model from `gen_metadata`. The lookup runs on a
//! worker thread with a hard time budget: a locked or slow DB must never hang the printing path.

use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags};

// Hard budget for the whole lookup; past it the render gives up and shows no response model.
const SQLITE_LOOKUP_BUDGET: Duration = Duration::from_millis(30);

/// Outcome of a bounded lookup. `Found` carries the model; `Empty` means the DB had no model row
/// (cache it); `Unavailable` means timeout/locked/error: retry next frame, keep the prior model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteLookup {
    Found(String),
    Empty,
    Unavailable,
}

/// Looks up the response model on a detached thread and waits at most `SQLITE_LOOKUP_BUDGET`.
/// Returns `Unavailable` on timeout, spawn failure, or any DB error, so the caller never blocks
/// longer than the budget even if the DB is locked; the worker thread is left to finish alone.
pub fn lookup_response_model_bounded(db_path: &Path) -> SqliteLookup {
    let owned_path = db_path.to_path_buf();
    let (tx, rx) = mpsc::sync_channel(1);
    let spawn_res = thread::Builder::new()
        .name("agyline-sqlite".to_string())
        .spawn(move || {
            let res = query_response_model_sync(&owned_path);
            let _ = tx.send(res);
        });

    if spawn_res.is_err() {
        return SqliteLookup::Unavailable;
    }

    // A thread cannot be cancelled; on timeout we abandon it and return Unavailable so the
    // render is never held past the budget. The orphaned worker finishes or errors on its own.
    match rx.recv_timeout(SQLITE_LOOKUP_BUDGET) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            SqliteLookup::Unavailable
        }
    }
}

/// Opens the DB read-only with no busy wait and reads the newest `gen_metadata` rows (LIMIT 10).
/// `no such table` is `Empty`, not an error; `busy_timeout(0)` makes a locked DB fail fast rather
/// than block. Decodes the first response model found, else `Empty` or `Unavailable`.
fn query_response_model_sync(db_path: &Path) -> SqliteLookup {
    // Read-only: agyline never writes the conversation DB. NO_MUTEX: only this one worker thread
    // touches the connection, so SQLite's serialization mutex is needless overhead.
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = Connection::open_with_flags(db_path, flags) else {
        return SqliteLookup::Unavailable;
    };
    if conn.busy_timeout(Duration::ZERO).is_err() {
        return SqliteLookup::Unavailable;
    }

    let mut stmt = match conn.prepare("SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 10") {
        Ok(s) => s,
        // SQLite gives a missing table a generic code, so match the message: a fresh DB is Empty.
        Err(rusqlite::Error::SqliteFailure(err, Some(msg)))
            if err.code == ErrorCode::Unknown && msg.contains("no such table") =>
        {
            return SqliteLookup::Empty;
        }
        Err(_) => return SqliteLookup::Unavailable,
    };

    let Ok(mut rows) = stmt.query([]) else {
        return SqliteLookup::Unavailable;
    };

    // Newest first: the latest row may not carry a model yet, so fall back through recent rows.
    loop {
        match rows.next() {
            Ok(Some(row)) => match row.get_ref(0) {
                Ok(ValueRef::Blob(bytes)) => {
                    if let Some(model) = decode_response_model_from_gen_metadata(bytes) {
                        return SqliteLookup::Found(model);
                    }
                }
                Ok(ValueRef::Text(bytes)) => {
                    if let Some(model) = decode_response_model_from_gen_metadata(bytes) {
                        return SqliteLookup::Found(model);
                    }
                }
                Ok(ValueRef::Null) | Ok(ValueRef::Integer(_)) | Ok(ValueRef::Real(_)) => {}
                Err(_) => return SqliteLookup::Unavailable,
            },
            Ok(None) => return SqliteLookup::Empty,
            Err(_) => return SqliteLookup::Unavailable,
        }
    }
}

/// Decodes the response model from a `gen_metadata` protobuf blob. Walks top-level fields and
/// descends into field 1 (the nested chat-model message) and returns the last model found, or
/// `None` if the blob is truncated/malformed. A wire-format walk, so unknown fields are skipped.
pub fn decode_response_model_from_gen_metadata(blob: &[u8]) -> Option<String> {
    let mut pos = 0;
    let mut found: Option<String> = None;

    while pos < blob.len() {
        let Some(tag) = read_varint(blob, &mut pos) else {
            break;
        };
        let field_num = tag >> 3;
        let wire_type = (tag & 0x07) as u8;
        if field_num == 0 {
            break;
        }

        // Wire types: 0 varint, 1 fixed64, 2 length-delimited, 5 fixed32; groups are unsupported.
        match wire_type {
            0 => {
                if read_varint(blob, &mut pos).is_none() {
                    break;
                }
            }
            1 => {
                if blob.len().saturating_sub(pos) < 8 {
                    break;
                }
                pos += 8;
            }
            2 => {
                let Some(len_u64) = read_varint(blob, &mut pos) else {
                    break;
                };
                let Ok(len) = usize::try_from(len_u64) else {
                    break;
                };
                if blob.len().saturating_sub(pos) < len {
                    break;
                }
                let payload = &blob[pos..pos + len];
                pos += len;
                // Field 1 is the nested chat-model message; the response model lives inside it.
                if field_num == 1
                    && let Some(model) = decode_chat_model_response_model(payload)
                {
                    found = Some(model);
                }
            }
            5 => {
                if blob.len().saturating_sub(pos) < 4 {
                    break;
                }
                pos += 4;
            }
            _ => break,
        }
    }

    found
}

/// Decodes the inner chat-model message: field 19 is the response-model string. This is the same
/// field the reference finds by its `\x9a\x01` tag (19 << 3 | 2). Rejects empty or control-char
/// strings; returns the last valid one, or `None` if the message is truncated/malformed.
fn decode_chat_model_response_model(bytes: &[u8]) -> Option<String> {
    let mut pos = 0;
    let mut found: Option<String> = None;

    while pos < bytes.len() {
        let Some(tag) = read_varint(bytes, &mut pos) else {
            break;
        };
        let field_num = tag >> 3;
        let wire_type = (tag & 0x07) as u8;
        if field_num == 0 {
            break;
        }

        match wire_type {
            0 => {
                if read_varint(bytes, &mut pos).is_none() {
                    break;
                }
            }
            1 => {
                if bytes.len().saturating_sub(pos) < 8 {
                    break;
                }
                pos += 8;
            }
            2 => {
                let Some(len_u64) = read_varint(bytes, &mut pos) else {
                    break;
                };
                let Ok(len) = usize::try_from(len_u64) else {
                    break;
                };
                if bytes.len().saturating_sub(pos) < len {
                    break;
                }
                let payload = &bytes[pos..pos + len];
                pos += len;
                if field_num == 19
                    && let Ok(raw_str) = std::str::from_utf8(payload)
                {
                    let trimmed = raw_str.trim();
                    // Reject control chars: a model name is plain text, not binary.
                    if !trimmed.is_empty() && !trimmed.chars().any(char::is_control) {
                        found = Some(trimmed.to_string());
                    }
                }
            }
            5 => {
                if bytes.len().saturating_sub(pos) < 4 {
                    break;
                }
                pos += 4;
            }
            _ => break,
        }
    }

    found
}

/// Reads a protobuf base-128 varint, advancing `pos`. `None` at end-of-buffer or on overflow: a
/// varint is at most 10 bytes, and the 10th may only contribute bit 63 (`byte > 1` guard).
fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for _ in 0..10 {
        let &byte = bytes.get(*pos)?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        result |= u64::from(byte & 0x7f) << shift;
        if (byte & 0x80) == 0 {
            return Some(result);
        }
        shift += 7;
    }
    None
}
