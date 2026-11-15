use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags};

const SQLITE_LOOKUP_BUDGET: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteLookup {
    Found(String),
    Empty,
    Unavailable,
}

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

    match rx.recv_timeout(SQLITE_LOOKUP_BUDGET) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            SqliteLookup::Unavailable
        }
    }
}

fn query_response_model_sync(db_path: &Path) -> SqliteLookup {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = Connection::open_with_flags(db_path, flags) else {
        return SqliteLookup::Unavailable;
    };
    if conn.busy_timeout(Duration::ZERO).is_err() {
        return SqliteLookup::Unavailable;
    }

    let mut stmt = match conn.prepare("SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 10") {
        Ok(s) => s,
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
