//! Cross-frame session contracts: the output-rate baseline, the `cache_ts` fallback chain, relay
//! touch gating, and SQLite response-model caching. Sequences are the point: one frame cannot
//! show a baseline reset, a countdown, or a cache that must not reopen. Detached `__touch`
//! workers run against a loopback relay stand-in, never a real network.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agyline::{RenderEnv, Spawner, run_live_frame};
use rusqlite::{Connection, params};
use serde_json::Value;

static SEQ_SANDBOX_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Per-test home/temp/conversations sandbox, removed on drop. `home_dir` is deliberately not
/// created, so a case can assert that a frame wrote no state at all.
struct Sandbox {
    root: PathBuf,
    home_dir: PathBuf,
    temp_dir: PathBuf,
    conversations_dir: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = SEQ_SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "agyline_seq_{}_{}_{}_{}",
            std::process::id(),
            nanos,
            id,
            name
        ));
        let home_dir = root.join("home");
        let temp_dir = root.join("temp");
        let conversations_dir = root.join("conversations");
        fs::create_dir_all(&temp_dir).expect("create temp_dir");
        fs::create_dir_all(&conversations_dir).expect("create conversations_dir");
        Self {
            root,
            home_dir,
            temp_dir,
            conversations_dir,
        }
    }

    /// RenderEnv with an injected clock and recording spawner: no real time and no real child
    /// processes, so a frame sequence stays deterministic.
    fn env<'a>(
        &self,
        now_ms: u64,
        env_vars: BTreeMap<String, String>,
        spawner: &'a dyn Spawner,
    ) -> RenderEnv<'a> {
        RenderEnv {
            home_dir: self.home_dir.clone(),
            temp_dir: self.temp_dir.clone(),
            conversations_dir: self.conversations_dir.clone(),
            env_vars,
            now_ms,
            spawner,
        }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.home_dir.join("state").join("sessions")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Counts the work the print path only requests: git refreshes, relay touches, and SQLite opens.
/// `opens()` is the evidence that an idle frame did no database I/O.
#[derive(Default)]
struct RecordingSpawner {
    git_spawns: Mutex<Vec<String>>,
    touch_spawns: Mutex<Vec<(String, String)>>,
    transcript_spawns: Mutex<Vec<(String, String)>>,
    sqlite_opens: AtomicUsize,
}

impl RecordingSpawner {
    fn touches(&self) -> Vec<(String, String)> {
        self.touch_spawns.lock().expect("lock touches").clone()
    }

    fn transcript_spawns(&self) -> Vec<(String, String)> {
        self.transcript_spawns
            .lock()
            .expect("lock transcript_spawns")
            .clone()
    }

    fn opens(&self) -> usize {
        self.sqlite_opens.load(Ordering::SeqCst)
    }
}

impl Spawner for RecordingSpawner {
    fn spawn_refresh_git(&self, cwd: &str) {
        self.git_spawns
            .lock()
            .expect("lock git_spawns")
            .push(cwd.to_string());
    }

    fn spawn_touch(&self, session_id: &str, relay_url: &str) {
        self.touch_spawns
            .lock()
            .expect("lock touch_spawns")
            .push((session_id.to_string(), relay_url.to_string()));
    }

    fn spawn_refresh_transcript(&self, session_id: &str, transcript_path: &str) {
        self.transcript_spawns
            .lock()
            .expect("lock transcript_spawns")
            .push((session_id.to_string(), transcript_path.to_string()));
    }

    fn on_sqlite_open(&self, _db_path: &Path) {
        self.sqlite_opens.fetch_add(1, Ordering::SeqCst);
    }
}

/// Sorted file names, empty when the dir is absent; used to assert no `.tmp` leftovers remain.
fn dir_entries(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn seed_cometix_config(home_dir: &Path) {
    fs::create_dir_all(home_dir).expect("create home_dir");
    fs::write(home_dir.join("config.toml"), "preset = \"cometix\"\n").expect("write cometix");
}

// Frames without a usable session id must leave no state behind, and a startup state is not an
// accepted turn: neither may anchor the five-minute prompt-cache clock.
#[test]
fn pre_session_and_startup_states_isolate_writes_and_never_anchor_warm_cache() {
    let sb = Sandbox::new("pre_session_isolation");
    seed_cometix_config(&sb.home_dir);
    let spawner = RecordingSpawner::default();

    // 1. Pre-session frames (empty/whitespace session_id & conversation_id) never touch state/sessions
    for raw in [
        br#"{}"#.as_slice(),
        br#"{"session_id":"","conversation_id":"","agent_state":"working","context_window":{"total_output_tokens":120}}"#.as_slice(),
        br#"{"session_id":"   ","conversation_id":"\t","agent_state":"working"}"#.as_slice(),
    ] {
        let env = sb.env(1_700_000_000_000, BTreeMap::new(), &spawner);
        let out = run_live_frame(raw, &env);
        assert_eq!(out.plain, "  \u{ef23} Model | \u{f49b} 0% | \u{f1c0} cold\n");
        assert!(
            !sb.sessions_dir().exists(),
            "Pre-session frame must not create state/sessions directory"
        );
        assert!(spawner.touches().is_empty());
    }

    // 2. Real session in authenticating / initializing state never anchors warm Prompt Cache
    for startup_state in ["authenticating", "INITIALIZING"] {
        let payload = format!(
            r#"{{"session_id":"550e8400-e29b-41d4-a716-446655440000","agent_state":"{startup_state}"}}"#
        );
        let env = sb.env(1_700_000_010_000, BTreeMap::new(), &spawner);
        let out = run_live_frame(payload.as_bytes(), &env);
        assert_eq!(
            out.plain,
            "  \u{ef23} Model | \u{f49b} 0% | \u{f1c0} cold\n"
        );
        assert!(
            out.ansi.contains("\x1b[0;90m cold"),
            "startup states must keep dim cold cache, got: {:?}",
            out.ansi
        );
    }
}

// Persisted state is a fixed field whitelist keyed by the session id alone, so payload extras
// cannot leak to disk and a repeated frame stays a no-op.
#[test]
fn session_key_encoding_privacy_and_unchanged_idle_write_skip() {
    let sb = Sandbox::new("session_key_and_skip_write");
    seed_cometix_config(&sb.home_dir);
    let spawner = RecordingSpawner::default();

    // 1. Standard 36-char lowercase UUID passes through directly; email is never persisted
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let payload_uuid = format!(
        r#"{{"session_id":"{uuid}","agent_state":"working","email":"private@example.com"}}"#
    );
    let out1 = run_live_frame(
        payload_uuid.as_bytes(),
        &sb.env(1_700_000_000_000, BTreeMap::new(), &spawner),
    );
    assert_eq!(
        out1.plain,
        "  \u{ef23} Model | \u{f49b} 0% | \u{f1c0} 5:00\n"
    );

    let uuid_file = sb.sessions_dir().join(format!("{uuid}.json"));
    assert!(
        uuid_file.is_file(),
        "lowercase UUID must pass through directly"
    );
    let raw_saved = fs::read_to_string(&uuid_file).expect("read uuid state");
    assert!(!raw_saved.contains("private@example.com"));
    let parsed: Value = serde_json::from_str(&raw_saved).expect("valid JSON");
    assert_eq!(parsed["version"], 1);
    assert_eq!(parsed["session_id"], uuid);

    // 2. Non-UUID ("hello") encodes via 64-bit FNV-1a ("a430d84680aabd0b")
    let out_hello = run_live_frame(
        br#"{"session_id":"hello","agent_state":"idle"}"#,
        &sb.env(1_700_000_000_000, BTreeMap::new(), &spawner),
    );
    assert_eq!(
        out_hello.plain,
        "  \u{ef23} Model | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    let hello_file = sb.sessions_dir().join("id-a430d84680aabd0b.json");
    assert!(
        hello_file.is_file(),
        "expected id-a430d84680aabd0b.json, found {:?}",
        dir_entries(&sb.sessions_dir())
    );

    // 3. Repeating an identical idle frame does not rewrite the file on disk
    let old_time = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&hello_file)
        .expect("open hello_file");
    f.set_times(fs::FileTimes::new().set_modified(old_time))
        .expect("set mtime");
    drop(f);

    let _ = run_live_frame(
        br#"{"session_id":"hello","agent_state":"idle"}"#,
        &sb.env(1_700_000_005_000, BTreeMap::new(), &spawner),
    );
    let after_mtime = fs::metadata(&hello_file)
        .expect("meta")
        .modified()
        .expect("modified");
    assert_eq!(
        after_mtime, old_time,
        "unchanged session state must skip disk rewrite"
    );
}

// Only a sequence shows the two clocks: tok/s keeps its last measured value across idle frames,
// while the 300s cache countdown is re-anchored by accepted output and then runs down.
#[test]
fn stateful_tps_ties_to_even_and_five_minute_prompt_cache_lifecycle() {
    let sb = Sandbox::new("tps_and_cache_lifecycle");
    seed_cometix_config(&sb.home_dir);
    let spawner = RecordingSpawner::default();
    let sid = "550e8400-e29b-41d4-a716-446655440099";

    // Frame 1 (t = 1000.0s): transition idle -> working at base_out = 100 -> warm 5:00
    let f1 = format!(
        r#"{{"session_id":"{sid}","agent_state":"working","context_window":{{"total_output_tokens":100,"current_usage":{{"input_tokens":5000,"cache_read_input_tokens":45000}}}}}}"#
    );
    let out1 = run_live_frame(f1.as_bytes(), &sb.env(1_000_000, BTreeMap::new(), &spawner));
    assert_eq!(
        out1.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 50k tokens | \u{f1c0} 5:00 \u{00b7} 90% hit\n"
    );

    // Frame 2 (t = 1002.0s): +129 tokens in 2.0s => 64.5 tok/s, cache re-anchored at 1002s (5:00)
    let f2 = format!(
        r#"{{"session_id":"{sid}","agent_state":"working","context_window":{{"total_output_tokens":229,"current_usage":{{"input_tokens":5000,"cache_read_input_tokens":45000}}}}}}"#
    );
    let out2 = run_live_frame(f2.as_bytes(), &sb.env(1_002_000, BTreeMap::new(), &spawner));
    assert_eq!(
        out2.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 50k tokens | \u{f1c0} 5:00 \u{00b7} 90% hit | \u{f0e7} 64.5 tok/s\n"
    );

    // Frame 3 (t = 1242.0s): idle 240s after 1002s => 60s remaining (warn yellow 1:00), keeps 64.5 tok/s
    let f3 = format!(
        r#"{{"session_id":"{sid}","agent_state":"idle","context_window":{{"total_output_tokens":229,"current_usage":{{"input_tokens":5000,"cache_read_input_tokens":45000}}}}}}"#
    );
    let out3 = run_live_frame(f3.as_bytes(), &sb.env(1_242_000, BTreeMap::new(), &spawner));
    assert_eq!(
        out3.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 50k tokens | \u{f1c0} 1:00 \u{00b7} 90% hit | \u{f0e7} 64.5 tok/s\n"
    );
    assert!(
        out3.ansi.contains("\x1b[0;33m 1:00"),
        "<= 60s remaining must use warn color: {:?}",
        out3.ansi
    );

    // Frame 4 (t = 1305.0s): expired (>300s after 1002s) => warn yellow cold + hit%, still keeps 64.5 tok/s
    let out4 = run_live_frame(f3.as_bytes(), &sb.env(1_305_000, BTreeMap::new(), &spawner));
    assert_eq!(
        out4.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 50k tokens | \u{f1c0} cold \u{00b7} 90% hit | \u{f0e7} 64.5 tok/s\n"
    );
    assert!(
        out4.ansi.contains("\x1b[0;33m cold"),
        "expired warm cache must render warn-colored cold: {:?}",
        out4.ansi
    );

    // Frame 5 & 6: >=100 tok/s F0 ties-to-even (201 tok / 2.0s = 100.5 -> 100; 203 tok / 2.0s = 101.5 -> 102)
    let f5_start = format!(
        r#"{{"session_id":"{sid}","agent_state":"working","context_window":{{"total_output_tokens":229}}}}"#
    );
    let _ = run_live_frame(
        f5_start.as_bytes(),
        &sb.env(2_000_000, BTreeMap::new(), &spawner),
    );
    let f5_done = format!(
        r#"{{"session_id":"{sid}","agent_state":"idle","context_window":{{"total_output_tokens":430}}}}"#
    );
    let out5 = run_live_frame(
        f5_done.as_bytes(),
        &sb.env(2_002_000, BTreeMap::new(), &spawner),
    );
    assert!(
        out5.plain.contains("\u{f0e7} 100 tok/s"),
        "100.5 tok/s must round ties-to-even to 100, got: {}",
        out5.plain
    );

    let _ = run_live_frame(
        format!(
            r#"{{"session_id":"{sid}","agent_state":"working","context_window":{{"total_output_tokens":430}}}}"#
        )
        .as_bytes(),
        &sb.env(2_010_000, BTreeMap::new(), &spawner),
    );
    let out6 = run_live_frame(
        format!(
            r#"{{"session_id":"{sid}","agent_state":"idle","context_window":{{"total_output_tokens":633}}}}"#
        )
        .as_bytes(),
        &sb.env(2_012_000, BTreeMap::new(), &spawner),
    );
    assert!(
        out6.plain.contains("\u{f0e7} 102 tok/s"),
        "101.5 tok/s must round ties-to-even to 102, got: {}",
        out6.plain
    );
}

// An untrustworthy state file is a cold miss, never a panic or a bogus rate, and the frame that
// rejected it must heal the file atomically.
#[test]
fn corrupt_version_mismatch_and_session_id_mismatch_self_heal_atomically() {
    let sb = Sandbox::new("self_healing");
    seed_cometix_config(&sb.home_dir);
    let spawner = RecordingSpawner::default();
    let sessions_dir = sb.sessions_dir();
    fs::create_dir_all(&sessions_dir).expect("create sessions_dir");
    let hello_file = sessions_dir.join("id-a430d84680aabd0b.json");

    // Four ways state can be untrustworthy: truncated, wrong version, a foreign session id behind
    // a hash collision, and missing required fields.
    let bad_payloads = [
        r#"{"version": 1, "truncated"#.to_string(),
        r#"{"version": 2, "session_id": "hello", "prev_state": "idle", "start_time_ms": 1000, "base_out": 0, "last_tps": "99.9", "resp_model": "", "prev_model_key": "Model", "cache_ts": 1700000000, "db_fingerprint": null}"#.to_string(),
        r#"{"version": 1, "session_id": "collision_other_id", "prev_state": "idle", "start_time_ms": 1000, "base_out": 0, "last_tps": "99.9", "resp_model": "", "prev_model_key": "Model", "cache_ts": 1700000000, "db_fingerprint": null}"#.to_string(),
        r#"{"version": 1, "session_id": "hello"}"#.to_string(),
    ];

    for (idx, broken_content) in bad_payloads.iter().enumerate() {
        fs::write(&hello_file, broken_content).expect("seed broken state");
        let out = run_live_frame(
            br#"{"session_id":"hello","agent_state":"idle"}"#,
            &sb.env(1_700_000_000_000, BTreeMap::new(), &spawner),
        );
        // Must treat as cache miss (cold, no bogus 99.9 tok/s)
        assert_eq!(
            out.plain, "  \u{ef23} Model | \u{f49b} 0% | \u{f1c0} cold\n",
            "case #{idx} must fall back cleanly to cold miss"
        );

        // Must overwrite broken file with healed version 1 state and leave zero .tmp files
        let healed_raw = fs::read_to_string(&hello_file).expect("read healed state");
        let healed: Value = serde_json::from_str(&healed_raw).expect("healed state valid JSON");
        assert_eq!(healed["version"], 1);
        assert_eq!(healed["session_id"], "hello");
        assert_eq!(healed["last_tps"], "--");
        assert_eq!(
            dir_entries(&sessions_dir),
            vec!["id-a430d84680aabd0b.json".to_string()],
            "no leftover .tmp files allowed"
        );
    }
}

// A touch is a conjunction: last 60s of the 300s window, >= 32K prompt tokens, fewer than 4
// touches, and a live relay. Each gate is exercised in turn against one relay state file.
#[test]
fn cache_relay_badges_keepalive_touch_gating_and_off_override() {
    let sb = Sandbox::new("cache_relay_and_touch_gating");
    seed_cometix_config(&sb.home_dir);
    let spawner = RecordingSpawner::default();
    let mut relay_env = BTreeMap::new();
    relay_env.insert(
        "AGY_CACHE_RELAY".to_string(),
        "http://127.0.0.1:18080".to_string(),
    );

    // Pre-session frames must never read relay files or spawn touches
    fs::write(sb.temp_dir.join("agy_cache_.dat"), "1700000000 1 live\n").expect("write");
    fs::write(
        sb.temp_dir.join("agy_cache_default.dat"),
        "1700000000 1 live\n",
    )
    .expect("write");
    let pre = run_live_frame(
        br#"{"session_id":"","context_window":{"current_usage":{"input_tokens":4000,"cache_read_input_tokens":36000}}}"#,
        &sb.env(1_700_000_250_000, relay_env.clone(), &spawner),
    );
    assert_eq!(
        pre.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 40k tokens | \u{f1c0} cold\n"
    );
    assert!(spawner.touches().is_empty());

    // Real session with safe-id sanitization ("sess.alpha" -> agy_cache_sess_alpha.dat)
    let sid = "sess.alpha";
    let relay_dat = sb.temp_dir.join("agy_cache_sess_alpha.dat");
    fs::write(&relay_dat, "1700000000 0 live\n").expect("write relay_dat");

    // Last step of the cache_ts chain: a relay anchor overrides the session-derived clock, so the
    // countdowns below are measured from 1700000000, not from these frames.
    let payload_36k = br#"{"session_id":"sess.alpha","agent_state":"idle","context_window":{"current_usage":{"input_tokens":4000,"cache_read_input_tokens":32000}}}"#;

    // 1. Warm (> 60s remaining: 180s left) => green ↻ badge, zero touches spawned
    let out_warm = run_live_frame(
        payload_36k,
        &sb.env(1_700_000_120_000, relay_env.clone(), &spawner),
    );
    assert_eq!(
        out_warm.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 36k tokens | \u{f1c0} 3:00 \u{21bb} \u{00b7} 88% hit\n"
    );
    assert!(spawner.touches().is_empty());

    // 2. Final 60s window (cache_rem == 60) with >= 32K prompt tokens and touches < 4 => spawns 1 touch
    let out_60s = run_live_frame(
        payload_36k,
        &sb.env(1_700_000_240_000, relay_env.clone(), &spawner),
    );
    assert_eq!(
        out_60s.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 36k tokens | \u{f1c0} 1:00 \u{21bb} \u{00b7} 88% hit\n"
    );
    assert_eq!(
        spawner.touches(),
        vec![(sid.to_string(), "http://127.0.0.1:18080".to_string())]
    );

    // 3. Below 32K prompt threshold (31,999 tokens) in final 60s => does NOT spawn touch
    let payload_sub32k = br#"{"session_id":"sess.alpha","agent_state":"idle","context_window":{"current_usage":{"input_tokens":1999,"cache_read_input_tokens":30000}}}"#;
    let _ = run_live_frame(
        payload_sub32k,
        &sb.env(1_700_000_250_000, relay_env.clone(), &spawner),
    );
    assert_eq!(spawner.touches().len(), 1);

    // 4. Relay reaches 4-touch idle cap ("1700000240 4 live") => renders ↻4, spawns 0 more touches in final 60s
    fs::write(&relay_dat, "1700000240 4 live\n").expect("write 4 touches");
    let out_cap = run_live_frame(
        payload_36k,
        &sb.env(1_700_000_490_000, relay_env.clone(), &spawner),
    );
    assert_eq!(
        out_cap.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 36k tokens | \u{f1c0} 0:50 \u{21bb}4 \u{00b7} 88% hit\n"
    );
    assert_eq!(
        spawner.touches().len(),
        1,
        "must not exceed 4 touches per idle stretch"
    );

    // 5. Relay status = fail => renders red ✗ badge, does not spawn touch
    fs::write(&relay_dat, "1700000500 2 fail\n").expect("write fail");
    let out_fail = run_live_frame(
        payload_36k,
        &sb.env(1_700_000_750_000, relay_env.clone(), &spawner),
    );
    assert_eq!(
        out_fail.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 36k tokens | \u{f1c0} 0:50 \u{2717} \u{00b7} 88% hit\n"
    );
    assert!(out_fail.ansi.contains("\x1b[0;31m \u{2717}"));
    assert_eq!(spawner.touches().len(), 1);

    // 6. AGY_CACHE_RELAY=off hides relay badge and skips relay file while preserving countdown from session state
    let mut off_env = BTreeMap::new();
    off_env.insert("AGY_CACHE_RELAY".to_string(), "off".to_string());
    let out_off = run_live_frame(payload_36k, &sb.env(1_700_000_750_000, off_env, &spawner));
    assert_eq!(
        out_off.plain,
        "  \u{ef23} Model | \u{f49b} 0% \u{00b7} 36k tokens | \u{f1c0} 0:50 \u{00b7} 88% hit\n"
    );
    assert_eq!(spawner.touches().len(), 1);
}

/// Protobuf base-128 varint, so the fixture can hand-build a `gen_metadata` blob without pulling
/// in a protobuf dependency.
fn push_varint(out: &mut Vec<u8>, mut val: u64) {
    loop {
        if val < 0x80 {
            out.push(val as u8);
            break;
        }
        out.push(((val & 0x7f) as u8) | 0x80);
        val >>= 7;
    }
}

/// Minimal shape the CLI writes: outer field 1 wrapping inner field 19, the model id. The decoder
/// only needs those bytes, so the fixture stays independent of the rest of the real schema.
fn encode_chat_model_blob(model_name: &str) -> Vec<u8> {
    let mut inner = Vec::new();
    push_varint(&mut inner, (19 << 3) | 2);
    push_varint(&mut inner, model_name.len() as u64);
    inner.extend_from_slice(model_name.as_bytes());

    let mut outer = Vec::new();
    push_varint(&mut outer, (1 << 3) | 2);
    push_varint(&mut outer, inner.len() as u64);
    outer.extend_from_slice(&inner);
    outer
}

// The DB fingerprint (len + mtime) is what gates a reopen, and a picker switch invalidates the
// cached response model without any SQLite I/O. Only a frame sequence can separate those.
#[test]
fn sqlite_response_model_caching_zero_idle_opens_and_picker_switch_invalidation() {
    let sb = Sandbox::new("sqlite_caching_and_switch");
    seed_cometix_config(&sb.home_dir);
    let spawner = RecordingSpawner::default();
    let sid = "550e8400-e29b-41d4-a716-446655440077";
    let db_path = sb.conversations_dir.join(format!("{sid}.db"));

    // 1. Missing DB falls back to Picker Model (Auto) without opening SQLite
    let f_missing = format!(
        r#"{{"session_id":"{sid}","model":{{"display_name":"Auto"}},"agent_state":"idle","context_window":{{"total_output_tokens":0}}}}"#
    );
    let out0 = run_live_frame(
        f_missing.as_bytes(),
        &sb.env(1_700_000_000_000, BTreeMap::new(), &spawner),
    );
    assert_eq!(
        out0.plain,
        "  \u{ef23} Auto | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(spawner.opens(), 0);

    // 2. Create WAL DB with Opus 4.6; active turn (delta_out > 0) queries SQLite once and caches Opus 4.6
    let conn = Connection::open(&db_path).expect("open wal db");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);",
    )
    .expect("create schema");
    let opus_blob = encode_chat_model_blob("claude-opus-4-6");
    conn.execute(
        "INSERT INTO gen_metadata (idx, data, size) VALUES (1, ?1, ?2)",
        params![opus_blob, opus_blob.len() as i64],
    )
    .expect("insert opus");

    let f_active1 = format!(
        r#"{{"session_id":"{sid}","model":{{"display_name":"Auto"}},"agent_state":"working","context_window":{{"total_output_tokens":30}}}}"#
    );
    let out1 = run_live_frame(
        f_active1.as_bytes(),
        &sb.env(1_700_000_002_000, BTreeMap::new(), &spawner),
    );
    assert!(
        out1.plain.starts_with("  \u{ef23} Opus 4.6 |"),
        "expected Opus 4.6, got: {}",
        out1.plain
    );
    assert_eq!(spawner.opens(), 1);

    // 3. Repeated idle turns with unchanged .db/.db-wal reuse cached Opus 4.6 with ZERO SQLite opens
    let f_idle_auto = format!(
        r#"{{"session_id":"{sid}","model":{{"display_name":"Auto"}},"agent_state":"idle","context_window":{{"total_output_tokens":30}}}}"#
    );
    for step_ms in [1_700_000_005_000_u64, 1_700_000_010_000, 1_700_000_015_000] {
        let out_idle = run_live_frame(
            f_idle_auto.as_bytes(),
            &sb.env(step_ms, BTreeMap::new(), &spawner),
        );
        assert!(
            out_idle.plain.starts_with("  \u{ef23} Opus 4.6 |"),
            "idle frame must retain cached Opus 4.6, got: {}",
            out_idle.plain
        );
        assert_eq!(
            spawner.opens(),
            1,
            "unchanged idle turns must perform ZERO SQLite opens"
        );
    }

    // 4. Picker Model switch while idle (Auto -> Claude Sonnet 4.5) clears cached Response Model with ZERO SQLite opens
    let f_switch_sonnet = format!(
        r#"{{"session_id":"{sid}","model":{{"display_name":"Claude Sonnet 4.5"}},"agent_state":"idle","context_window":{{"total_output_tokens":30}}}}"#
    );
    let out_switch = run_live_frame(
        f_switch_sonnet.as_bytes(),
        &sb.env(1_700_000_020_000, BTreeMap::new(), &spawner),
    );
    assert!(
        out_switch
            .plain
            .starts_with("  \u{ef23} Claude Sonnet 4.5 |"),
        "picker switch must show Claude Sonnet 4.5 instead of stale Opus 4.6, got: {}",
        out_switch.plain
    );
    assert_eq!(
        spawner.opens(),
        1,
        "idle picker switch with unchanged DB fingerprint must not reopen SQLite"
    );

    // 5. Next active turn appends Gemini 2.5 Pro to WAL and increments output tokens -> opens SQLite once more
    let gemini_blob = encode_chat_model_blob("gemini-2.5-pro");
    conn.execute(
        "INSERT INTO gen_metadata (idx, data, size) VALUES (2, ?1, ?2)",
        params![gemini_blob, gemini_blob.len() as i64],
    )
    .expect("insert gemini");

    let f_active2 = format!(
        r#"{{"session_id":"{sid}","model":{{"display_name":"Auto"}},"agent_state":"working","context_window":{{"total_output_tokens":75}}}}"#
    );
    let out2 = run_live_frame(
        f_active2.as_bytes(),
        &sb.env(1_700_000_025_000, BTreeMap::new(), &spawner),
    );
    assert!(
        out2.plain.starts_with("  \u{ef23} Gemini 2.5 Pro |"),
        "expected Gemini 2.5 Pro, got: {}",
        out2.plain
    );
    assert_eq!(spawner.opens(), 2);
}

// A locked conversation DB must degrade to the picker name inside the render budget, exit 0 with
// empty stderr, and leave the fingerprint unpoisoned so the next frame recovers.
#[test]
fn sqlite_exclusive_write_lock_falls_back_fast_with_exit_zero() {
    let sb = Sandbox::new("sqlite_exclusive_lock");
    let sid = "550e8400-e29b-41d4-a716-446655440088";
    // The spawned binary derives conversations_dir from HOME/USERPROFILE, not from an injected
    // RenderEnv, so the DB has to live under this sandbox home.
    let proc_conv_dir = sb
        .root
        .join(".gemini")
        .join("antigravity-cli")
        .join("conversations");
    fs::create_dir_all(&proc_conv_dir).expect("create proc_conv_dir");
    let db_path = proc_conv_dir.join(format!("{sid}.db"));

    let init_conn = Connection::open(&db_path).expect("open db");
    init_conn
        .execute_batch(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);",
        )
        .expect("create table");
    let opus_blob = encode_chat_model_blob("claude-opus-4-6");
    init_conn
        .execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (1, ?1, ?2)",
            params![opus_blob, opus_blob.len() as i64],
        )
        .expect("insert opus");
    drop(init_conn);

    // Hold BEGIN EXCLUSIVE lock with an active write statement in the parent process
    let lock_conn = Connection::open(&db_path).expect("open lock_conn");
    lock_conn
        .execute_batch(
            "BEGIN EXCLUSIVE;
             INSERT INTO gen_metadata (idx, data, size) VALUES (2, X'00', 1);",
        )
        .expect("acquire exclusive write lock");

    let payload = format!(
        r#"{{"session_id":"{sid}","model":{{"display_name":"Auto"}},"agent_state":"idle","context_window":{{"total_output_tokens":40}}}}"#
    );

    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .env("AGYLINE_HOME", &sb.home_dir)
        .env("USERPROFILE", &sb.root)
        .env("HOME", &sb.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agyline under exclusive SQLite lock");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out_locked = child.wait_with_output().expect("wait locked agyline");
    let elapsed = t0.elapsed();

    assert_eq!(out_locked.status.code(), Some(0));
    assert!(
        out_locked.stderr.is_empty(),
        "stderr must be 0B, got: {:?}",
        String::from_utf8_lossy(&out_locked.stderr)
    );
    assert!(
        elapsed < Duration::from_millis(800),
        "exclusive lock fallback took {:?} (expected fast non-blocking fallback)",
        elapsed
    );
    let stdout_locked = String::from_utf8_lossy(&out_locked.stdout);
    assert!(
        stdout_locked.contains("Auto"),
        "expected graceful fallback to Auto while locked, got: {:?}",
        stdout_locked
    );

    // Release exclusive lock; next frame must recover and resolve Opus 4.6 because db_fingerprint wasn't poisoned
    lock_conn.execute_batch("ROLLBACK;").expect("rollback");
    drop(lock_conn);

    let mut child2 = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .env("AGYLINE_HOME", &sb.home_dir)
        .env("USERPROFILE", &sb.root)
        .env("HOME", &sb.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agyline after lock release");
    child2
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out_unlocked = child2.wait_with_output().expect("wait unlocked agyline");
    assert_eq!(out_unlocked.status.code(), Some(0));
    assert!(out_unlocked.stderr.is_empty());
    let stdout_unlocked = String::from_utf8_lossy(&out_unlocked.stdout);
    assert!(
        stdout_unlocked.contains("Opus 4.6"),
        "expected Opus 4.6 once exclusive lock released, got: {:?}",
        stdout_unlocked
    );
}

/// Reads up to the end of the HTTP head under a 2s bound, so a stuck worker fails the test
/// instead of hanging it.
fn read_http_request_head(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set_read_timeout");
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

// `__touch` is the only code that opens a socket: loopback HTTP only, deduplicated by the lock,
// and hostile input rejected silently with exit 0 and no output.
#[test]
fn touch_worker_loopback_http_post_lock_dedup_and_ssrf_rejection() {
    let sb = Sandbox::new("touch_worker_http_and_ssrf");
    // Ephemeral loopback port: the relay stand-in must not need a fixed port or a real network.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let port = listener.local_addr().expect("local_addr").port();

    // 1. Valid UUID over 127.0.0.1 sends exact HTTP/1.1 POST /_touch/<sid>, exits 0 with 0B output, removes lock
    let uuid = "550e8400-e29b-41d4-a716-446655440011";
    let (tx1, rx1) = mpsc::channel();
    let l1 = listener.try_clone().expect("clone listener");
    let srv1 = thread::spawn(move || {
        l1.set_nonblocking(false).expect("blocking");
        if let Ok((mut stream, _)) = l1.accept() {
            let req = read_http_request_head(&mut stream);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
            let _ = tx1.send(req);
        }
    });

    let out1 = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .args([
            "__touch",
            "--session",
            uuid,
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .env("AGYLINE_HOME", &sb.home_dir)
        .output()
        .expect("run __touch");
    assert_eq!(out1.status.code(), Some(0));
    assert!(out1.stdout.is_empty());
    assert!(out1.stderr.is_empty());

    let req1 = rx1
        .recv_timeout(Duration::from_secs(2))
        .expect("loopback server must receive POST /_touch/<uuid>");
    srv1.join().expect("join srv1");
    assert!(
        req1.starts_with(&format!("POST /_touch/{uuid} HTTP/1.1\r\n")),
        "unexpected request line: {req1:?}"
    );
    let req1_lower = req1.to_ascii_lowercase();
    assert!(req1_lower.contains("content-length: 0\r\n"));
    assert!(req1_lower.contains("connection: close\r\n"));

    let locks_dir = sb.home_dir.join("state").join("locks");
    let uuid_lock = locks_dir.join(format!("touch-{uuid}.lock"));
    assert!(
        !uuid_lock.exists(),
        "touch worker must remove its own lock file on completion"
    );

    // 2. Valid localhost URL + trailing slash + non-UUID session ID ("hello")
    let (tx2, rx2) = mpsc::channel();
    let l2 = listener.try_clone().expect("clone listener");
    let srv2 = thread::spawn(move || {
        l2.set_nonblocking(false).expect("blocking");
        if let Ok((mut stream, _)) = l2.accept() {
            let req = read_http_request_head(&mut stream);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            let _ = tx2.send(req);
        }
    });

    let out2 = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .args([
            "__touch",
            "--session",
            "hello",
            "--url",
            &format!("http://localhost:{port}/"),
        ])
        .env("AGYLINE_HOME", &sb.home_dir)
        .output()
        .expect("run __touch localhost");
    assert_eq!(out2.status.code(), Some(0));
    assert!(out2.stdout.is_empty());
    assert!(out2.stderr.is_empty());
    let req2 = rx2
        .recv_timeout(Duration::from_secs(2))
        .expect("localhost touch must reach 127.0.0.1 loopback");
    srv2.join().expect("join srv2");
    assert!(req2.starts_with("POST /_touch/hello HTTP/1.1\r\n"));
    assert!(!locks_dir.join("touch-id-a430d84680aabd0b.lock").exists());

    // 3. Fresh active lock deduplicates concurrent __touch and preserves foreign owner's lock file
    fs::create_dir_all(&locks_dir).expect("create locks_dir");
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_millis() as u64;
    let foreign_token = format!("{now_ms} 999999\n");
    fs::write(&uuid_lock, &foreign_token).expect("seed active lock");

    listener.set_nonblocking(true).expect("nonblocking");
    let out_dedup = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .args([
            "__touch",
            "--session",
            uuid,
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .env("AGYLINE_HOME", &sb.home_dir)
        .output()
        .expect("run deduplicated __touch");
    assert_eq!(out_dedup.status.code(), Some(0));
    assert!(out_dedup.stdout.is_empty());
    assert!(out_dedup.stderr.is_empty());
    assert!(
        listener.accept().is_err(),
        "active lock must prevent duplicate HTTP POST"
    );
    assert_eq!(
        fs::read_to_string(&uuid_lock).expect("foreign lock still intact"),
        foreign_token,
        "non-owner must never delete active foreign lock"
    );

    // 4. Stale lock (>= 125_000 ms old) is taken over, request succeeds, and lock is cleaned up
    let stale_token = format!("{} 999999\n", now_ms.saturating_sub(126_000));
    fs::write(&uuid_lock, stale_token).expect("seed stale lock");

    let (tx3, rx3) = mpsc::channel();
    let l3 = listener.try_clone().expect("clone listener");
    let srv3 = thread::spawn(move || {
        l3.set_nonblocking(false).expect("blocking");
        if let Ok((mut stream, _)) = l3.accept() {
            let req = read_http_request_head(&mut stream);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            let _ = tx3.send(req);
        }
    });

    let out_takeover = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .args([
            "__touch",
            "--session",
            uuid,
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .env("AGYLINE_HOME", &sb.home_dir)
        .output()
        .expect("run stale takeover __touch");
    assert_eq!(out_takeover.status.code(), Some(0));
    assert!(out_takeover.stdout.is_empty());
    assert!(out_takeover.stderr.is_empty());
    let req3 = rx3
        .recv_timeout(Duration::from_secs(2))
        .expect("stale lock takeover must send POST");
    srv3.join().expect("join srv3");
    assert!(req3.starts_with(&format!("POST /_touch/{uuid} HTTP/1.1\r\n")));
    assert!(!uuid_lock.exists());

    // 5. SSRF & invalid input rejection: non-http, non-loopback, userinfo spoofing, bad session IDs
    listener.set_nonblocking(true).expect("nonblocking");
    let rejected_invocations: Vec<Vec<String>> = vec![
        vec![
            "__touch".into(),
            "--session".into(),
            uuid.into(),
            "--url".into(),
            format!("https://127.0.0.1:{port}"),
        ],
        vec![
            "__touch".into(),
            "--session".into(),
            uuid.into(),
            "--url".into(),
            format!("http://127.0.0.2:{port}"),
        ],
        vec![
            "__touch".into(),
            "--session".into(),
            uuid.into(),
            "--url".into(),
            format!("http://127.0.0.1@evil.example:{port}"),
        ],
        vec![
            "__touch".into(),
            "--session".into(),
            uuid.into(),
            "--url".into(),
            format!("http://evil.example:{port}"),
        ],
        vec![
            "__touch".into(),
            "--session".into(),
            "../bad".into(),
            "--url".into(),
            format!("http://127.0.0.1:{port}"),
        ],
        vec![
            "__touch".into(),
            "--session".into(),
            "".into(),
            "--url".into(),
            format!("http://127.0.0.1:{port}"),
        ],
        vec!["__touch".into(), "--bogus".into()],
    ];

    for args in rejected_invocations {
        let out_bad = Command::new(env!("CARGO_BIN_EXE_agyline"))
            .args(&args)
            .env("AGYLINE_HOME", &sb.home_dir)
            .output()
            .expect("run rejected __touch");
        assert_eq!(out_bad.status.code(), Some(0), "args={args:?}");
        assert!(out_bad.stdout.is_empty(), "args={args:?}");
        assert!(out_bad.stderr.is_empty(), "args={args:?}");
        assert!(
            listener.accept().is_err(),
            "rejected args must never connect: {args:?}"
        );
    }
}

// The print path must reach pipe EOF while a 2.5s relay POST is still in flight; an inherited
// stdio handle in the detached child would hold the parent's pipe open.
#[test]
fn live_render_spawns_detached_touch_without_holding_pipe_eof_on_slow_relay() {
    let sb = Sandbox::new("detached_touch_pipe_eof");
    seed_cometix_config(&sb.home_dir);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow relay");
    let port = listener.local_addr().expect("local_addr").port();

    let sid = format!("touch_eof_{}_{}", std::process::id(), port);
    let now_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_secs();
    let relay_content = format!("{} 1 live\n", now_sec.saturating_sub(250));
    // The worker resolves the relay dir itself: Windows honors TMPDIR/TEMP/TMP, Unix always uses
    // /tmp, so the state file is seeded in both places.
    let relay_file_name = format!("agy_cache_{sid}.dat");
    fs::write(sb.temp_dir.join(&relay_file_name), &relay_content).expect("write sb relay file");
    let unix_tmp_relay = Path::new("/tmp").join(&relay_file_name);
    let wrote_unix_tmp = if Path::new("/tmp").is_dir() {
        fs::write(&unix_tmp_relay, &relay_content).is_ok()
    } else {
        false
    };

    let (tx, rx) = mpsc::channel();
    let srv = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let req = read_http_request_head(&mut stream);
            // Hold connection open for 2.5s to prove detached child holds zero parent pipe handles
            thread::sleep(Duration::from_millis(2500));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            let _ = tx.send(req);
        }
    });

    let payload = format!(
        r#"{{"session_id":"{sid}","agent_state":"idle","context_window":{{"current_usage":{{"input_tokens":4000,"cache_read_input_tokens":36000}}}}}}"#
    );

    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .env("AGYLINE_HOME", &sb.home_dir)
        .env("AGY_CACHE_RELAY", format!("http://127.0.0.1:{port}"))
        .env("TMPDIR", &sb.temp_dir)
        .env("TEMP", &sb.temp_dir)
        .env("TMP", &sb.temp_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn live render");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait live render");
    let render_elapsed = t0.elapsed();

    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    assert!(
        render_elapsed < Duration::from_millis(800),
        "live render blocked on slow background __touch for {:?} (leaked pipe handle?)",
        render_elapsed
    );
    let stdout_str = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout_str.contains("\u{21bb}1"),
        "expected relay badge ↻1 in output, got: {stdout_str:?}"
    );

    // Meanwhile the detached __touch worker still delivers the POST in the background
    let req = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("detached __touch worker must deliver POST /_touch/<sid>");
    srv.join().expect("join slow relay server");
    if wrote_unix_tmp {
        let _ = fs::remove_file(&unix_tmp_relay);
    }
    assert!(req.starts_with(&format!("POST /_touch/{sid} HTTP/1.1\r\n")));
}

#[test]
fn bounded_transcript_head_read_and_detached_refresh_worker_handle_large_files() {
    let sb = Sandbox::new("transcript_bounded");
    let tpath = sb.root.join("large_transcript.jsonl");
    let mut file = fs::File::create(&tpath).expect("create transcript");
    writeln!(
        file,
        r#"{{"type":"USER","created_at":"2026-11-23T10:00:00Z","text":"hello"}}"#
    )
    .expect("write head");
    writeln!(
        file,
        r#"{{"type":"PLANNER_RESPONSE","input_tokens":100,"cache_read_tokens":900,"cache_creation_tokens":0,"output_tokens":50}}"#
    )
    .expect("write planner");
    // Pad past 4 MiB so any full-file scan on the render path would show up and be caught.
    let pad = format!(r#"{{"type":"NOTE","payload":"{}"}}"#, "x".repeat(4000));
    for _ in 0..1050 {
        writeln!(file, "{pad}").expect("write pad");
    }
    writeln!(
        file,
        r#"{{"type":"PLANNER_RESPONSE","input_tokens":200,"cache_read_tokens":1800,"cache_creation_input_tokens":0,"output_tokens":150}}"#
    )
    .expect("write tail planner");
    file.flush().expect("flush");

    // 1. O(1) bounded header micro-read finds exact start epoch
    let start_sec = agyline::read_transcript_start_sec_bounded(&tpath).expect("bounded head read");
    assert_eq!(start_sec, 1_795_428_000);

    // 2. Detached `__refresh-transcript` worker parses full totals off the print path
    let sid = "sess_large_t";
    let out = Command::new(env!("CARGO_BIN_EXE_agyline"))
        .args([
            "__refresh-transcript",
            "--session",
            sid,
            "--path",
            &tpath.to_string_lossy(),
        ])
        .env("AGYLINE_HOME", &sb.home_dir)
        .output()
        .expect("run __refresh-transcript");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());

    let session_json_path = sb.sessions_dir().join("id-97592d4e95679199.json");
    // Check via session files in sessions dir (hash-agnostic)
    let files: Vec<_> = fs::read_dir(sb.sessions_dir())
        .expect("sessions_dir")
        .flatten()
        .collect();
    assert_eq!(files.len(), 1);
    let _ = session_json_path;
    let saved: Value =
        serde_json::from_slice(&fs::read(files[0].path()).expect("read session")).expect("json");
    assert_eq!(saved["transcript_first_ts"], 1_795_428_000);
    assert_eq!(saved["transcript_total_in"], 3000);
    assert_eq!(saved["transcript_total_out"], 200);
    assert_eq!(saved["transcript_cache_read"], 2700);
    assert_eq!(saved["transcript_cache_valid"], true);
}

#[test]
fn demand_gating_skips_session_sqlite_relay_and_transcript_when_unused() {
    let sb = Sandbox::new("demand_gate_minimal");
    fs::create_dir_all(&sb.home_dir).expect("mkdir home");
    fs::write(
        sb.home_dir.join("config.toml"),
        "[[line]]\nitems = [\"dir\", \"context\"]\n",
    )
    .expect("write config");

    let sid = "550e8400-e29b-41d4-a716-4466554400aa";
    let db_path = sb.conversations_dir.join(format!("{sid}.db"));
    let conn = Connection::open(&db_path).expect("create db");
    conn.execute_batch(
        "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);",
    )
    .expect("schema");
    let blob = encode_chat_model_blob("claude-opus-4-6");
    conn.execute(
        "INSERT INTO gen_metadata (idx, data, size) VALUES (1, ?1, ?2)",
        params![blob, blob.len() as i64],
    )
    .expect("insert");
    drop(conn);

    let tpath = sb.root.join("unused_transcript.jsonl");
    fs::write(
        &tpath,
        r#"{"type":"USER","created_at":"2026-11-23T10:00:00Z","text":"hi"}"#,
    )
    .expect("write transcript");

    let mut relay_env = BTreeMap::new();
    relay_env.insert(
        "AGY_CACHE_RELAY".to_string(),
        "http://127.0.0.1:18080".to_string(),
    );

    let spawner = RecordingSpawner::default();
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": sid,
        "conversation_id": sid,
        "agent_state": "working",
        "cwd": "D:\\Work\\minimal_proj",
        "transcript_path": tpath.to_string_lossy(),
        "context_window": {
            "total_output_tokens": 50,
            "current_usage": { "input_tokens": 10000, "cache_read_input_tokens": 30000 },
            "context_window_size": 100000
        }
    }))
    .expect("json");

    let out = run_live_frame(&payload, &sb.env(1_700_000_000_000, relay_env, &spawner));
    assert!(out.plain.contains("minimal_proj"));
    assert!(out.plain.contains("40%"));
    assert_eq!(spawner.opens(), 0, "must perform 0 SQLite opens");
    assert!(spawner.touches().is_empty(), "must spawn 0 relay touches");
    assert!(
        spawner.transcript_spawns().is_empty(),
        "must spawn 0 transcript refreshes"
    );
    assert!(
        !sb.sessions_dir().exists(),
        "must skip session state reads/writes when unused"
    );
}
