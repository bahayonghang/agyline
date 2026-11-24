//! Byte-exact golden parity against the reference line (`ref/agy-patch/statusline.ps1`/`.py`).
//! Each fixture's `expected_plain`/`expected_ansi` is the reference output for that frame; any
//! mismatch means default-line parity broke. Cases run at a fixed clock anchor with a NoopSpawner
//! so the printed text is deterministic and no background git/relay work is triggered.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agyline::{
    CacheFrameState, ColorDepth, ConfigDoc, EffectiveConfig, FrameData, GitCacheEntry,
    GitFrameState, GlyphSet, NoopSpawner, RelayStatus, RenderEnv, emit_ansi, emit_plain,
    parse_stdin_payload, render_lines, render_lines_with_width, run_live_frame, str_display_width,
    write_git_cache,
};
use rusqlite::{Connection, params};
use serde::Deserialize;

static SANDBOX_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempSandbox {
    root: PathBuf,
}

impl TempSandbox {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "agyline_golden_{}_{}_{}_{}",
            std::process::id(),
            nanos,
            seq,
            label
        ));
        fs::create_dir_all(&dir).expect("create golden sandbox");
        Self { root: dir }
    }
}

impl Drop for TempSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Deserialize)]
struct GoldenSuite {
    cases: Vec<GoldenCase>,
}

#[derive(Debug, Deserialize)]
struct SessionStateFixture {
    file_name: String,
    json: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RelayFileFixture {
    file_name: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct GenRowFixture {
    idx: i64,
    chat_model: String,
    #[serde(default)]
    trailing_top_field19: Option<String>,
    #[serde(default)]
    trailing_pad_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct DbSetupFixture {
    kind: String,
    #[serde(default)]
    mtime_sec: Option<u64>,
    #[serde(default)]
    gen_rows: Vec<GenRowFixture>,
    #[serde(default)]
    steps_pad_bytes: usize,
    #[serde(default)]
    steps_fake_model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoldenCase {
    id: String,
    description: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    config_toml: Option<String>,
    #[serde(default)]
    raw_stdin: Option<String>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    session_state: Option<SessionStateFixture>,
    #[serde(default)]
    relay_file: Option<RelayFileFixture>,
    #[serde(default)]
    db_setup: Option<DbSetupFixture>,
    expected_plain: String,
    expected_ansi: String,
}

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

fn push_len_field(out: &mut Vec<u8>, field_num: u64, bytes: &[u8]) {
    push_varint(out, (field_num << 3) | 2);
    push_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// Builds a `gen_metadata.data` protobuf blob: nested field 1.19 carries `chat_model`, while the
/// optional top-level field 19 and padding simulate trailing bytes the bounded decoder must skip.
fn encode_gen_metadata_blob(
    chat_model: &str,
    trailing_top_field19: Option<&str>,
    trailing_pad_bytes: usize,
) -> Vec<u8> {
    let mut inner_chat_model = Vec::new();
    push_len_field(&mut inner_chat_model, 19, chat_model.as_bytes());

    let mut outer = Vec::new();
    push_len_field(&mut outer, 1, &inner_chat_model);
    if let Some(top19) = trailing_top_field19 {
        push_len_field(&mut outer, 19, top19.as_bytes());
    }
    if trailing_pad_bytes > 0 {
        let pad = vec![b'x'; trailing_pad_bytes];
        push_len_field(&mut outer, 2, &pad);
    }
    outer
}

/// Seeds `conversations/<sid>.db` for the bounded response-model lookup; `wal_stale_restart` keeps
/// the newest row only in the WAL, so the reader must follow idx DESC, not a stale main-db tail.
fn prepare_db_setup(
    conversations_dir: &Path,
    session_id: &str,
    setup: &DbSetupFixture,
) -> Option<Connection> {
    fs::create_dir_all(conversations_dir).expect("create conversations_dir");
    let db_path = conversations_dir.join(format!("{session_id}.db"));

    if setup.kind == "wal_stale_restart" {
        let conn = Connection::open(&db_path).expect("open wal db");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_payload BLOB);",
        )
        .expect("init wal schema");
        let r1 = encode_gen_metadata_blob("claude-opus-4-6", None, 0);
        let r2 = encode_gen_metadata_blob("claude-haiku-4-5", None, 0);
        let r3 = encode_gen_metadata_blob("gemini-2.5-pro", None, 0);
        let big_step = vec![b'y'; 24_000];
        conn.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, ?3)",
            params![1_i64, r1, r1.len() as i64],
        )
        .expect("insert r1");
        conn.execute(
            "INSERT INTO steps (idx, step_payload) VALUES (1, ?1)",
            params![big_step],
        )
        .expect("insert step");
        conn.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, ?3)",
            params![2_i64, r2, r2.len() as i64],
        )
        .expect("insert r2");
        conn.execute_batch("PRAGMA wal_checkpoint(RESTART);")
            .expect("wal_checkpoint(RESTART)");
        conn.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, ?3)",
            params![3_i64, r3, r3.len() as i64],
        )
        .expect("insert r3");
        return Some(conn);
    }

    let conn = Connection::open(&db_path).expect("open standard db");
    conn.execute_batch(
        "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_payload BLOB);",
    )
    .expect("init standard schema");
    for row in &setup.gen_rows {
        let blob = encode_gen_metadata_blob(
            &row.chat_model,
            row.trailing_top_field19.as_deref(),
            row.trailing_pad_bytes,
        );
        conn.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, ?3)",
            params![row.idx, blob, blob.len() as i64],
        )
        .expect("insert gen_metadata row");
    }
    let mut step_idx = 1_i64;
    if setup.steps_pad_bytes > 0 {
        let pad = vec![b'x'; setup.steps_pad_bytes];
        conn.execute(
            "INSERT INTO steps (idx, step_payload) VALUES (?1, ?2)",
            params![step_idx, pad],
        )
        .expect("insert padded step");
        step_idx += 1;
    }
    if let Some(fake_model) = &setup.steps_fake_model {
        let mut polluted = vec![b'z'; 4096];
        polluted.extend_from_slice(&encode_gen_metadata_blob(fake_model, None, 0));
        conn.execute(
            "INSERT INTO steps (idx, step_payload) VALUES (?1, ?2)",
            params![step_idx, polluted],
        )
        .expect("insert polluted step");
    }
    drop(conn);

    if let Some(mtime_sec) = setup.mtime_sec {
        let target_time = UNIX_EPOCH + Duration::from_secs(mtime_sec);
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&db_path)
            .expect("open db for set_times");
        f.set_times(fs::FileTimes::new().set_modified(target_time))
            .expect("set db mtime");
    }
    None
}

/// Materializes each case's config/session/relay/DB seeds in an isolated sandbox, renders at the
/// fixed anchor with NoopSpawner, and asserts both plain and ANSI bytes equal the golden fixture.
fn run_golden_suite(fixture_raw: &str, config_fallback: Option<&str>) {
    let suite: GoldenSuite =
        serde_json::from_str(fixture_raw).expect("golden fixture JSON must parse");

    let spawner = NoopSpawner;
    for case in &suite.cases {
        let stdin_bytes = if let Some(raw) = &case.raw_stdin {
            raw.as_bytes().to_vec()
        } else if let Some(payload) = &case.payload {
            serde_json::to_vec(payload).expect("serialize payload")
        } else {
            panic!("case {} missing both raw_stdin and payload", case.id);
        };

        let sandbox = TempSandbox::new(&case.id);
        let home_dir = sandbox.root.join("agyline_home");
        let temp_dir = sandbox.root.join("relay_temp");
        let conversations_dir = sandbox.root.join("conversations");
        fs::create_dir_all(&temp_dir).expect("create temp_dir");

        if let Some(cfg_toml) = case.config_toml.as_deref().or(config_fallback) {
            fs::create_dir_all(&home_dir).expect("create home_dir");
            fs::write(home_dir.join("config.toml"), cfg_toml).expect("write config.toml");
        }

        if let Some(seed) = &case.session_state {
            let sessions_dir = home_dir.join("state").join("sessions");
            fs::create_dir_all(&sessions_dir).expect("create sessions_dir");
            let bytes = serde_json::to_vec(&seed.json).expect("serialize session_state");
            fs::write(sessions_dir.join(&seed.file_name), bytes).expect("write session_state");
        }

        if let Some(relay) = &case.relay_file {
            fs::write(temp_dir.join(&relay.file_name), &relay.content).expect("write relay_file");
        }

        let _held_wal_conn = case.db_setup.as_ref().map(|db_setup| {
            let sid = case
                .payload
                .as_ref()
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str())
                .expect("db_setup case must specify payload.session_id");
            prepare_db_setup(&conversations_dir, sid, db_setup)
        });

        let env = RenderEnv {
            home_dir,
            temp_dir,
            conversations_dir,
            env_vars: case.env.clone(),
            now_ms: 1_700_000_000_000,
            spawner: &spawner,
        };

        let out = run_live_frame(&stdin_bytes, &env);
        assert_eq!(
            out.plain, case.expected_plain,
            "plain mismatch on {} ({})",
            case.id, case.description
        );
        assert_eq!(
            out.ansi.as_bytes(),
            case.expected_ansi.as_bytes(),
            "ANSI byte mismatch on {} ({}):\n  actual: {:?}\nexpected: {:?}",
            case.id,
            case.description,
            out.ansi,
            case.expected_ansi
        );
    }
}

// Cometix reference parity (A-I cases): stdin/model/workspace/git/context/quota/cost and
// env overrides, plus seeded telemetry, relay, and SQLite state that alter the cache countdown.
#[test]
fn cometix_line_golden_cases_match_plain_and_ansi_bytes() {
    run_golden_suite(
        include_str!("fixtures/default_line_golden.json"),
        Some("preset = \"cometix\"\n"),
    );
}

// Opt-in items, glyph sets, palettes/color depth, env precedence, broken-TOML fallback, and
// width narrowing, each compared byte-for-byte to the reference.
#[test]
fn items_and_config_golden_cases_match_plain_and_ansi_bytes() {
    run_golden_suite(include_str!("fixtures/items_and_config.json"), None);
}

// Exercises the shared `render_lines` seam directly with a hand-built FrameData, pinning exact
// bytes for git/cache/relay/speed formatting independent of the collection path.
#[test]
fn render_lines_seam_formats_git_conflict_ahead_behind_speed_and_relay_badges() {
    let config =
        EffectiveConfig::from_doc(&ConfigDoc::cometix_base(), None, &BTreeMap::new(), false);

    // D05 exact byte check: long branch truncation + dirty + ahead/behind
    let d05_payload =
        parse_stdin_payload(br#"{"session_id":"probe_d05","cwd":"D:\\Temp\\ab-repo"}"#)
            .expect("parse D05");
    let d05_frame = FrameData {
        payload: d05_payload,
        now_sec: 1_700_000_000,
        git: Some(GitFrameState {
            branch: "feature/twenty-five-chars".to_string(),
            dirty: true,
            has_conflict: false,
            ahead: 2,
            behind: 1,
            ..GitFrameState::default()
        }),
        ..FrameData::default()
    };
    let d05_rendered = render_lines(&d05_frame, &config);
    assert_eq!(
        emit_plain(&d05_rendered, &config),
        "  \u{ef23} Model | \u{f024b} ab-repo | \u{f02a2} feature/twenty-five-cha\u{2026} \u{25cf} \u{2191}2 \u{2193}1 | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(
        emit_ansi(&d05_rendered, &config),
        "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m ab-repo\x1b[0;37m | \x1b[0;94m\u{f02a2} feature/twenty-five-cha\u{2026} \u{25cf} \u{2191}2 \u{2193}1\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n"
    );

    // D06 exact byte check: merge conflict (red ⚠)
    let d06_payload =
        parse_stdin_payload(br#"{"session_id":"probe_d06","cwd":"D:\\Temp\\conf-repo"}"#)
            .expect("parse D06");
    let d06_frame = FrameData {
        payload: d06_payload,
        now_sec: 1_700_000_000,
        git: Some(GitFrameState {
            branch: "main".to_string(),
            dirty: true,
            has_conflict: true,
            ahead: 0,
            behind: 0,
            ..GitFrameState::default()
        }),
        ..FrameData::default()
    };
    let d06_rendered = render_lines(&d06_frame, &config);
    assert_eq!(
        emit_ansi(&d06_rendered, &config),
        "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m conf-repo\x1b[0;37m | \x1b[0;94m\u{f02a2}\x1b[0;31m main \u{26a0}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n"
    );

    // F01 + H08 exact byte check: warm 3:00 TTL, relay live badge ↻2, hit %, and speed tok/s
    let f01_payload = parse_stdin_payload(
        br#"{"session_id":"probe_h08","context_window":{"current_usage":{"input_tokens":4000,"cache_read_input_tokens":36000}}}"#,
    )
    .expect("parse H08");
    let h08_frame = FrameData {
        payload: f01_payload,
        now_sec: 1_700_000_120,
        git: None,
        cache: CacheFrameState {
            cache_ts: Some(1_700_000_000),
            relay_touches: 2,
            relay_status: Some(RelayStatus::Live),
        },
        last_tps: Some("64.5".to_string()),
        response_model: Some("claude-opus-4-6".to_string()),
        ..FrameData::default()
    };
    let h08_rendered = render_lines(&h08_frame, &config);
    assert_eq!(
        emit_ansi(&h08_rendered, &config),
        "  \x1b[0m\x1b[0;96m\u{ef23} Opus 4.6\x1b[0;37m | \x1b[0;95m\u{f49b}\x1b[0;95m 0%\x1b[0;37m \u{00b7} \x1b[0;95m40k tokens\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;34m 3:00\x1b[0;92m \u{21bb}2\x1b[0;37m \u{00b7} \x1b[0;34m90% hit\x1b[0;37m | \x1b[0;92m\u{f0e7} 64.5 tok/s\x1b[0m\n"
    );
}

// Seeds a fresh git cache (now-500ms, inside the 2s window) per case so the cached branch state
// drives the printed git text; D04-D12 pin clean/dirty/conflict/detached/junction/CJK bytes.
#[test]
fn stateful_git_cache_golden_cases_d04_through_d12_match_exact_bytes() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let home_dir =
        std::env::temp_dir().join(format!("agyline_golden_git_{}_{nanos}", std::process::id()));
    fs::create_dir_all(&home_dir).expect("create temp golden home");
    fs::write(home_dir.join("config.toml"), "preset = \"cometix\"\n").expect("write cometix");

    let now_ms = 1_700_000_000_000_u64;
    let spawner = NoopSpawner;

    let cases: &[(&str, serde_json::Value, GitCacheEntry, &str, &str)] = &[
        (
            "D04_git_clean_repo",
            serde_json::json!({
                "session_id": "probe_d04",
                "vcs": { "type": "git" },
                "cwd": "D:\\Temp\\clean-repo"
            }),
            GitCacheEntry {
                version: 1,
                cwd: "D:\\Temp\\clean-repo".to_string(),
                updated_at_ms: now_ms - 500,
                is_repo: true,
                branch: "main".to_string(),
                dirty: false,
                has_conflict: false,
                ahead: 0,
                behind: 0,
                ..GitCacheEntry::default()
            },
            "  \u{ef23} Model | \u{f024b} clean-repo | \u{f02a2} main \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n",
            "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m clean-repo\x1b[0;37m | \x1b[0;94m\u{f02a2} main \u{2713}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n",
        ),
        (
            "D05_git_dirty_ahead_behind",
            serde_json::json!({
                "session_id": "probe_d05",
                "vcs": { "dirty": true },
                "cwd": "D:\\Temp\\ab-repo"
            }),
            GitCacheEntry {
                version: 1,
                cwd: "D:\\Temp\\ab-repo".to_string(),
                updated_at_ms: now_ms - 500,
                is_repo: true,
                branch: "feature/twenty-five-chars".to_string(),
                dirty: false,
                has_conflict: false,
                ahead: 2,
                behind: 1,
                ..GitCacheEntry::default()
            },
            "  \u{ef23} Model | \u{f024b} ab-repo | \u{f02a2} feature/twenty-five-cha\u{2026} \u{25cf} \u{2191}2 \u{2193}1 | \u{f49b} 0% | \u{f1c0} cold\n",
            "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m ab-repo\x1b[0;37m | \x1b[0;94m\u{f02a2} feature/twenty-five-cha\u{2026} \u{25cf} \u{2191}2 \u{2193}1\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n",
        ),
        (
            "D06_git_conflict_and_detached",
            serde_json::json!({
                "session_id": "probe_d06",
                "cwd": "D:\\Temp\\conf-repo"
            }),
            GitCacheEntry {
                version: 1,
                cwd: "D:\\Temp\\conf-repo".to_string(),
                updated_at_ms: now_ms - 500,
                is_repo: true,
                branch: "main".to_string(),
                dirty: true,
                has_conflict: true,
                ahead: 0,
                behind: 0,
                ..GitCacheEntry::default()
            },
            "  \u{ef23} Model | \u{f024b} conf-repo | \u{f02a2} main \u{26a0} | \u{f49b} 0% | \u{f1c0} cold\n",
            "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m conf-repo\x1b[0;37m | \x1b[0;94m\u{f02a2}\x1b[0;31m main \u{26a0}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n",
        ),
        (
            "D07_git_detached_head",
            serde_json::json!({
                "session_id": "probe_d07",
                "cwd": "D:\\Temp\\det-repo"
            }),
            GitCacheEntry {
                version: 1,
                cwd: "D:\\Temp\\det-repo".to_string(),
                updated_at_ms: now_ms - 500,
                is_repo: true,
                branch: "fb7464b".to_string(),
                dirty: false,
                has_conflict: false,
                ahead: 0,
                behind: 0,
                ..GitCacheEntry::default()
            },
            "  \u{ef23} Model | \u{f024b} det-repo | \u{f02a2} fb7464b \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n",
            "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m det-repo\x1b[0;37m | \x1b[0;94m\u{f02a2} fb7464b \u{2713}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n",
        ),
        (
            "D08_git_junction_into_repo",
            serde_json::json!({
                "session_id": "probe_d08",
                "cwd": "D:\\Temp\\outer_plain\\linked_pkg"
            }),
            GitCacheEntry {
                version: 1,
                cwd: "D:\\Temp\\outer_plain\\linked_pkg".to_string(),
                updated_at_ms: now_ms - 500,
                is_repo: true,
                branch: "main".to_string(),
                dirty: false,
                has_conflict: false,
                ahead: 0,
                behind: 0,
                ..GitCacheEntry::default()
            },
            "  \u{ef23} Model | \u{f024b} linked_pkg | \u{f02a2} main \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n",
            "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m linked_pkg\x1b[0;37m | \x1b[0;94m\u{f02a2} main \u{2713}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n",
        ),
        (
            "D12_cp437_non_ascii_git_branch",
            serde_json::json!({
                "session_id": "probe_d12",
                "cwd": "D:\\Temp\\cn-branch-repo"
            }),
            GitCacheEntry {
                version: 1,
                cwd: "D:\\Temp\\cn-branch-repo".to_string(),
                updated_at_ms: now_ms - 500,
                is_repo: true,
                branch: "feat/中文分支".to_string(),
                dirty: false,
                has_conflict: false,
                ahead: 0,
                behind: 0,
                ..GitCacheEntry::default()
            },
            "  \u{ef23} Model | \u{f024b} cn-branch-repo | \u{f02a2} feat/中文分支 \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n",
            "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m cn-branch-repo\x1b[0;37m | \x1b[0;94m\u{f02a2} feat/中文分支 \u{2713}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n",
        ),
    ];

    for (id, payload_json, cache_entry, expected_plain, expected_ansi) in cases {
        write_git_cache(&home_dir, cache_entry).expect("seed golden git cache");
        let stdin_bytes = serde_json::to_vec(payload_json).expect("serialize");
        // Nonexistent temp/conversations dirs prove the warm print path reads only the git cache.
        let env = RenderEnv {
            home_dir: home_dir.clone(),
            temp_dir: PathBuf::from("nonexistent_temp_dir"),
            conversations_dir: PathBuf::from("nonexistent_conversations_dir"),
            env_vars: BTreeMap::new(),
            now_ms,
            spawner: &spawner,
        };
        let out = run_live_frame(&stdin_bytes, &env);
        assert_eq!(&out.plain, expected_plain, "plain mismatch on {id}");
        assert_eq!(
            out.ansi.as_bytes(),
            expected_ansi.as_bytes(),
            "ANSI mismatch on {id}"
        );
    }

    let _ = fs::remove_dir_all(&home_dir);
}

// Screenshot-parity 3-row HUD `Default Line` golden + 74-column narrowing verification:
// Row 1 italic model + usage bar + ~/dir + per-count git; Row 2 dual Gemini/3P used% bars;
// Row 3 sandbox off + token breakdown + cache hit rate + elapsed duration; narrow_stress fits <= 74 cols.
#[test]
fn hud_default_three_row_golden_and_narrowing_match_exact_bytes() {
    let mut doc = agyline::build_preset_doc("hud", None).expect("hud preset");
    doc.color_depth = ColorDepth::TrueColor;
    let mut env = BTreeMap::new();
    env.insert("AGY_THEME".to_string(), "dark".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    let config = EffectiveConfig::from_doc(&doc, None, &env, false);

    let now_sec = 1_700_000_000_i64;
    let payload_bytes = serde_json::to_vec(&serde_json::json!({
        "session_id": "hud_parity",
        "agent_state": "working",
        "execution_mode": "accept-edits",
        "model": {
            "display_name": "Gemini 4 Neon",
            "effort": "xHigh"
        },
        "cwd": "/home/dev/projects/antigravity-hud",
        "sandbox": { "enabled": false },
        "context_window": {
            "current_usage": {
                "input_tokens": 9_600,
                "cache_read_input_tokens": 187_400,
                "cache_creation_input_tokens": 0
            },
            "total_input_tokens": 4_480_000,
            "total_output_tokens": 122_000,
            "context_window_size": 1_000_000,
            "used_percentage": 19.7
        },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.927, "reset_in_seconds": 2 * 3600 + 44 * 60 },
            "gemini-weekly": { "remaining_fraction": 0.797, "reset_in_seconds": 5 * 86_400 + 4 * 3600 },
            "3p-5h": { "remaining_fraction": 1.0, "reset_in_seconds": 5 * 3600 },
            "3p-weekly": { "remaining_fraction": 0.48, "reset_in_seconds": 2 * 86_400 + 23 * 3600 }
        }
    }))
    .expect("serialize hud payload");
    let payload = parse_stdin_payload(&payload_bytes).expect("parse hud payload");

    let frame = FrameData {
        payload,
        now_sec,
        git: Some(GitFrameState {
            branch: "main".to_string(),
            dirty: true,
            modified: 1,
            untracked: 7,
            ..GitFrameState::default()
        }),
        cache: CacheFrameState {
            cache_ts: Some(now_sec - 120),
            relay_touches: 0,
            relay_status: None,
        },
        session_start_sec: Some(now_sec - 58 * 60),
        user_home: Some(PathBuf::from("/home/dev")),
        ..FrameData::default()
    };

    let full = render_lines_with_width(&frame, &config, None);
    let plain = emit_plain(&full, &config);
    assert_eq!(
        plain,
        concat!(
            "  \u{2699} WORKING \u{2502} EDITS \u{2502} \u{ef23} Gemini 4 Neon (XHigh) \u{2502} \u{f49b} ctx \u{2501}\u{2501}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 19.7% \u{2502} \u{f024b} ~/projects/antigravity-hud \u{2502} \u{f02a2} main* !1 ?7\n",
            "  \u{f0081} Gemini 5h \u{2578}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 7.3% 2h44m / week \u{2501}\u{2501}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 20.3% 5d4h \u{2502} \u{f007e} 3P 5h \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 0.0% 5h0m / week \u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 52.0% 2d23h\n",
            "  \u{f132} sandbox off \u{2502} \u{e26b} token 4.6M (4.5M\u{2191} 122k\u{2193}) \u{2502} \u{f1c0} cache 95.1% \u{2502} \u{f43a} 58m\n",
        )
    );

    let ansi = emit_ansi(&full, &config);
    assert!(
        !ansi.contains(";48;2;"),
        "HUD default line usage bar must not emit background slab SGR: {ansi:?}"
    );
    assert!(
        ansi.contains(";3;") && ansi.contains("Gemini 4 Neon (XHigh)"),
        "HUD model must emit italic SGR: {ansi:?}"
    );
    assert!(
        ansi.contains(";93m\u{f024b}")
            && ansi.contains(";95m\u{f49b}")
            && ansi.contains(";34m\u{f1c0}")
            && ansi.contains(";92m\u{f0081}"),
        "HUD restored icons must keep semantic colors: {ansi:?}"
    );
    assert!(
        ansi.contains(";1;") && ansi.contains("4.6M") && ansi.contains("95.1%"),
        "HUD tokens and cache rate must emit bold numbers: {ansi:?}"
    );

    // 74-column narrowing must keep every row <= 74 cols and compact items before dropping them.
    let narrow = render_lines_with_width(&frame, &config, Some(74));
    let narrow_plain = emit_plain(&narrow, &config);
    let narrow_lines: Vec<&str> = narrow_plain.lines().collect();
    assert_eq!(narrow_lines.len(), 3);
    for (idx, line) in narrow_lines.iter().enumerate() {
        let w = str_display_width(line);
        assert!(w <= 74, "HUD line {} width {w} > 74: {line:?}", idx + 1);
    }
    assert!(narrow_lines[0].contains("19.7%"));
    assert!(narrow_lines[1].contains("Gemini 5h 7.3%"));
    assert!(narrow_lines[2].contains("token 4.6M"));

    // Ascii glyph mode uses #- bars and suppresses duplicate word prefixes like `ctx: ctx`.
    let mut asc_doc = doc.clone();
    asc_doc.glyphs = GlyphSet::Ascii;
    let asc_cfg = EffectiveConfig::from_doc(&asc_doc, None, &env, false);
    let asc_plain = emit_plain(&render_lines_with_width(&frame, &asc_cfg, None), &asc_cfg);
    assert!(asc_plain.contains("ctx ##-------- 19.7%"));
    assert!(!asc_plain.contains("ctx: ctx"));
    assert!(!asc_plain.contains("cache: cache"));
    assert!(!asc_plain.contains("sbx: sandbox"));
    assert!(!asc_plain.contains("qta:"));
    assert!(asc_plain.contains("token 4.6M") && asc_plain.ends_with("| 58m\n"));

    // Explicit `bar_style = "solid"` under light theme emits softened #e2e5ec track bg.
    let mut solid_light_doc = doc;
    solid_light_doc.lines[0][3].bar_style = Some("solid".to_string());
    let mut light_env = BTreeMap::new();
    light_env.insert("AGY_THEME".to_string(), "light".to_string());
    light_env.insert("COLORTERM".to_string(), "truecolor".to_string());
    let solid_light_cfg = EffectiveConfig::from_doc(&solid_light_doc, None, &light_env, true);
    let solid_light_ansi = emit_ansi(
        &render_lines_with_width(&frame, &solid_light_cfg, None),
        &solid_light_cfg,
    );
    assert!(
        solid_light_ansi.contains(";48;2;226;229;236m"),
        "solid light track must emit softened #e2e5ec bg: {solid_light_ansi:?}"
    );
}

// Ticket #43: new 2-row Catppuccin gradient `Default Line` and showcase scenarios (wide normal,
// high-context >90% `→ /compact`, and narrow <88 compact single-row collapse).
#[test]
fn agybar_default_two_row_and_showcase_scenarios_golden() {
    let doc = ConfigDoc::default();
    let mut env = BTreeMap::new();
    env.insert("AGY_THEME".to_string(), "dark".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    let cfg = EffectiveConfig::from_doc(&doc, None, &env, true);

    // 1. Wide normal Catppuccin showcase scenario.
    let sc_wide = agyline::find_scenario("showcase_wide").expect("showcase_wide scenario");
    let wide_frame = sc_wide.frame_for_env(&env);
    let wide_rendered = render_lines_with_width(&wide_frame, &cfg, Some(sc_wide.default_width));
    let wide_plain = emit_plain(&wide_rendered, &cfg);
    let wide_rows: Vec<&str> = wide_plain.lines().collect();
    assert_eq!(
        wide_rows.len(),
        2,
        "wide default must render 2 rows: {wide_plain:?}"
    );
    assert!(wide_rows[0].contains("✦ Claude Opus 4.6 [high]"));
    assert!(wide_rows[0].contains('▰') && wide_rows[0].contains('▱'));
    assert!(wide_rows[0].contains("48.0% (96k/200k)"));
    assert!(wide_rows[0].contains("↑48k ↓12k"));
    assert!(wide_rows[0].contains("⚡ 7d:64%(3d) · 5h:82%(2h15m)"));
    assert!(wide_rows[1].contains("● working"));
    assert!(wide_rows[1].contains("⎇ main*"));
    assert!(wide_rows[1].contains("~/…/clients/agyline"));

    let wide_ansi = emit_ansi(&wide_rendered, &cfg);
    // Catppuccin Mocha Mauve bold model (`38;2;203;166;247`) and Blue git (`38;2;137;180;250`).
    assert!(
        wide_ansi.contains(";1;38;2;203;166;247m✦ Claude Opus 4.6"),
        "ansi: {wide_ansi:?}"
    );
    assert!(
        wide_ansi.contains(";38;2;137;180;250m⎇ main"),
        "ansi: {wide_ansi:?}"
    );

    // 2. High context (>90%) triggers bold critical `→ /compact` and surfaces active runtime items.
    let sc_crit = agyline::find_scenario("showcase_critical").expect("showcase_critical scenario");
    let crit_frame = sc_crit.frame_for_env(&env);
    let crit_rendered = render_lines_with_width(&crit_frame, &cfg, Some(sc_crit.default_width));
    let crit_plain = emit_plain(&crit_rendered, &cfg);
    let crit_rows: Vec<&str> = crit_plain.lines().collect();
    assert_eq!(
        crit_rows.len(),
        2,
        "critical default must render 2 rows: {crit_plain:?}"
    );
    assert!(
        crit_rows[0].contains("→ /compact"),
        "row0: {:?}",
        crit_rows[0]
    );
    assert!(crit_rows[0].contains("cr:420"), "row0: {:?}", crit_rows[0]);
    assert!(
        crit_rows[1].contains("◉ thinking"),
        "row1: {:?}",
        crit_rows[1]
    );
    assert!(crit_rows[1].contains("agents"), "row1: {:?}", crit_rows[1]);
    assert!(crit_rows[1].contains("tasks"), "row1: {:?}", crit_rows[1]);
    assert!(
        crit_rows[1].contains("artifacts"),
        "row1: {:?}",
        crit_rows[1]
    );
    assert!(crit_rows[1].contains("sandbox"), "row1: {:?}", crit_rows[1]);

    // 3. Narrow (<88 cols) automatically collapses to 1 compact row with zero wrapping.
    let sc_compact = agyline::find_scenario("showcase_compact").expect("showcase_compact scenario");
    assert!(sc_compact.default_width < 88);
    let comp_frame = sc_compact.frame_for_env(&env);
    let comp_rendered = render_lines_with_width(&comp_frame, &cfg, Some(sc_compact.default_width));
    let comp_plain = emit_plain(&comp_rendered, &cfg);
    let comp_rows: Vec<&str> = comp_plain.lines().collect();
    assert_eq!(
        comp_rows.len(),
        1,
        "narrow <88 must collapse to 1 row: {comp_plain:?}"
    );
    let w = str_display_width(comp_rows[0]);
    assert!(
        w <= sc_compact.default_width,
        "collapsed row width {w} > {}: {:?}",
        sc_compact.default_width,
        comp_rows[0]
    );
    assert!(
        !comp_rows[0].contains("· 5h:"),
        "compact quota capped at 1 bucket: {:?}",
        comp_rows[0]
    );
    assert!(
        !comp_rows[0].contains("working"),
        "compact status collapses to dot-only: {:?}",
        comp_rows[0]
    );
}
