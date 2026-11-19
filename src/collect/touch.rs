//! Detached `__touch` worker: replays the prompt-cache keepalive by POSTing the relay's `_touch`
//! route. Never runs on the printing path. The URL must be loopback HTTP and the session id safe,
//! so a hostile frame cannot turn this into SSRF or a path escape. A per-session lock serializes
//! touches; the relay's own state file stays read-only (see `state::relay`).
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::platform;
use crate::state::{self, lock::ScopedLock, session::session_key};

// Stale window = connect (5s) + IO budget (120s), so a still-running touch is never treated as
// stale and a second worker cannot steal its lock and double-touch the relay.
pub const TOUCH_LOCK_STALE_MS: u64 = 125_000;
// TCP connect bound; the relay is loopback, so a slow connect means it is down.
const TOUCH_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
// Total IO budget, matching the reference relay's 120s touch timeout.
const TOUCH_IO_BUDGET: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TouchArgs {
    session_id: String,
    url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoopbackTarget {
    addr: SocketAddr,
    host_header: String,
    base_path: String,
}

/// Entry point for the detached `__touch` worker. Validates args, session id, and loopback URL,
/// takes the per-session lock, POSTs the touch, then sweeps. Always returns 0: a worker must
/// not fail the spawn or print. Bad input or a held lock is a silent no-op (a touch is optional).
pub fn run_touch_worker(tail: &[OsString]) -> u8 {
    let Some(args) = parse_touch_args(tail) else {
        return 0;
    };
    if !is_safe_session_id(&args.session_id) {
        return 0;
    }
    let Some(target) = parse_loopback_http_url(&args.url) else {
        return 0;
    };
    let key = session_key(&args.session_id);
    if key.is_empty() {
        return 0;
    }

    let home_dir = resolve_agyline_home();
    let now_ms = current_epoch_ms();
    let lock_name = format!("touch-{key}.lock");
    // One touch per session at a time: if another worker holds the lock, skip (the relay also
    // rejects concurrent touches). Stale takeover uses TOUCH_LOCK_STALE_MS.
    let Some(_guard) = ScopedLock::acquire(&home_dir, &lock_name, now_ms, TOUCH_LOCK_STALE_MS)
    else {
        return 0;
    };

    send_touch_post(&target, &args.session_id);
    drop(_guard);

    state::maybe_sweep_stale_state(&home_dir, current_epoch_ms());
    0
}

/// Parses `--session`/`--url` (`--k v` and `--k=v`). Rejects duplicates or missing values so a
/// malformed invocation is a silent no-op rather than a touch with partial args.
fn parse_touch_args(tail: &[OsString]) -> Option<TouchArgs> {
    let mut session_id: Option<String> = None;
    let mut url: Option<String> = None;
    let mut i = 0;

    while i < tail.len() {
        let arg = tail.get(i)?.to_str()?;
        if arg == "--session" {
            if session_id.is_some() {
                return None;
            }
            let val = tail.get(i + 1)?.to_str()?;
            session_id = Some(val.to_string());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--session=") {
            if session_id.is_some() {
                return None;
            }
            session_id = Some(val.to_string());
            i += 1;
        } else if arg == "--url" {
            if url.is_some() {
                return None;
            }
            let val = tail.get(i + 1)?.to_str()?;
            url = Some(val.to_string());
            i += 2;
        } else {
            let val = arg.strip_prefix("--url=")?;
            if url.is_some() {
                return None;
            }
            url = Some(val.to_string());
            i += 1;
        }
    }

    Some(TouchArgs {
        session_id: session_id?,
        url: url?,
    })
}

/// Session ids allowed into the touch URL path and lock filename: ASCII alphanumeric, `_`, `-`,
/// at most 256 bytes. Anything else (separators, control chars, whitespace) is rejected to block
/// path traversal and request-line injection from an untrusted id.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 256
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// True if `raw_url` is a loopback HTTP URL the touch worker would accept. The public form of the
/// worker's URL gate, so the configurator can reject a non-loopback relay URL before use.
pub fn is_valid_loopback_relay_url(raw_url: &str) -> bool {
    parse_loopback_http_url(raw_url).is_some()
}

/// Parses and vets the relay URL: `http://` only, loopback authority, no userinfo/query/fragment
/// or control chars. Returns the connect target, or `None` if unsafe. This is the SSRF guard: a
/// touch must only ever reach the local relay, never an arbitrary host.
fn parse_loopback_http_url(raw_url: &str) -> Option<LoopbackTarget> {
    let trimmed = raw_url.trim();
    if !trimmed.get(..7)?.eq_ignore_ascii_case("http://") {
        return None;
    }
    let rest = &trimmed[7..];
    // Reject control/whitespace/?# chars: they could inject headers or hide a second authority.
    if rest.is_empty()
        || rest
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '?' | '#' | '\\'))
    {
        return None;
    }

    let (authority, raw_path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    // No userinfo (`@`): it could disguise the real host and bypass the loopback check.
    if authority.is_empty() || authority.contains('@') {
        return None;
    }

    let (addr, host_header) = parse_loopback_authority(authority)?;
    let base_path = raw_path.trim_end_matches('/').to_string();

    Some(LoopbackTarget {
        addr,
        host_header,
        base_path,
    })
}

/// Resolves the authority to a loopback `SocketAddr` plus the Host header string. Accepts only
/// `127.0.0.1`, `localhost`, or `::1` (bracketed or bare), with an optional non-zero port.
/// The Host header keeps the original spelling so the relay sees the authority the config used.
fn parse_loopback_authority(authority: &str) -> Option<(SocketAddr, String)> {
    if let Some(after_open) = authority.strip_prefix('[') {
        let close_idx = after_open.find(']')?;
        let ipv6_host = &after_open[..close_idx];
        if ipv6_host != "::1" {
            return None;
        }
        let port_part = &after_open[close_idx + 1..];
        let (port, has_explicit_port) = if port_part.is_empty() {
            (80_u16, false)
        } else {
            let p_str = port_part.strip_prefix(':')?;
            (parse_nonzero_port(p_str)?, true)
        };
        let host_header = if has_explicit_port {
            format!("[::1]:{port}")
        } else {
            "[::1]".to_string()
        };
        return Some((SocketAddr::from((Ipv6Addr::LOCALHOST, port)), host_header));
    }

    if authority == "::1" {
        return Some((
            SocketAddr::from((Ipv6Addr::LOCALHOST, 80)),
            "[::1]".to_string(),
        ));
    }

    let (host, port, has_explicit_port) = match authority.split_once(':') {
        Some((h, p_str)) => {
            if p_str.contains(':') {
                return None;
            }
            (h, parse_nonzero_port(p_str)?, true)
        }
        None => (authority, 80_u16, false),
    };

    if host != "127.0.0.1" && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }

    let host_header = if has_explicit_port {
        format!("{host}:{port}")
    } else {
        host.to_string()
    };
    Some((SocketAddr::from((Ipv4Addr::LOCALHOST, port)), host_header))
}

/// Parses a decimal port, rejecting empty/non-digit input and port 0 (not a connectable target).
fn parse_nonzero_port(s: &str) -> Option<u16> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port = s.parse::<u16>().ok()?;
    (port > 0).then_some(port)
}

/// POSTs the keepalive to `<base>/_touch/<session>` (empty body, `Connection: close`), then drains
/// the response. Bounded by a 5s connect and a 120s IO deadline (the reference relay's timeout).
/// Fire-and-forget: errors are ignored, since a missed touch only shortens prompt-cache life.
fn send_touch_post(target: &LoopbackTarget, session_id: &str) {
    let Ok(mut stream) = TcpStream::connect_timeout(&target.addr, TOUCH_CONNECT_TIMEOUT) else {
        return;
    };

    let deadline = Instant::now() + TOUCH_IO_BUDGET;
    let Some(write_rem) = deadline.checked_duration_since(Instant::now()) else {
        return;
    };
    if write_rem.is_zero() || stream.set_write_timeout(Some(write_rem)).is_err() {
        return;
    }

    let request = format!(
        "POST {}/_touch/{session_id} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        target.base_path, target.host_header
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    // Drain until EOF or the deadline; the response body is irrelevant, only the touch matters.
    let mut scratch = [0_u8; 1024];
    while let Some(read_rem) = deadline.checked_duration_since(Instant::now()) {
        if read_rem.is_zero() || stream.set_read_timeout(Some(read_rem)).is_err() {
            break;
        }
        match stream.read(&mut scratch) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

fn resolve_agyline_home() -> PathBuf {
    let lookup = |k: &str| std::env::var(k).ok();
    let user_home = platform::user_home_dir(&lookup).unwrap_or_else(|| PathBuf::from("."));
    crate::config::resolve_home_dir(&lookup, &user_home)
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
