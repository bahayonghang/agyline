use std::collections::BTreeMap;
use std::path::PathBuf;

use agyline::{
    CacheFrameState, EffectiveConfig, FrameData, GitFrameState, NoopSpawner, RelayStatus,
    RenderEnv, emit_ansi, emit_plain, parse_stdin_payload, render_lines, run_live_frame,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct GoldenSuite {
    cases: Vec<GoldenCase>,
}

#[derive(Debug, Deserialize)]
struct GoldenCase {
    id: String,
    description: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    raw_stdin: Option<String>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    expected_plain: String,
    expected_ansi: String,
}

#[test]
fn stateless_default_line_golden_cases_match_plain_and_ansi_bytes() {
    let fixture_raw = include_str!("fixtures/default_line_golden.json");
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

        let env = RenderEnv {
            home_dir: PathBuf::from("nonexistent_agyline_home"),
            temp_dir: PathBuf::from("nonexistent_temp_dir"),
            conversations_dir: PathBuf::from("nonexistent_conversations_dir"),
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

#[test]
fn render_lines_seam_formats_git_conflict_ahead_behind_speed_and_relay_badges() {
    let config = EffectiveConfig::default();

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
        }),
        cache: CacheFrameState::default(),
        last_tps: None,
        response_model: None,
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
        }),
        cache: CacheFrameState::default(),
        last_tps: None,
        response_model: None,
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
    };
    let h08_rendered = render_lines(&h08_frame, &config);
    assert_eq!(
        emit_ansi(&h08_rendered, &config),
        "  \x1b[0m\x1b[0;96m\u{ef23} Opus 4.6\x1b[0;37m | \x1b[0;95m\u{f49b}\x1b[0;95m 0%\x1b[0;37m \u{00b7} \x1b[0;95m40k tokens\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;34m 3:00\x1b[0;92m \u{21bb}2\x1b[0;37m \u{00b7} \x1b[0;34m90% hit\x1b[0;37m | \x1b[0;92m\u{f0e7} 64.5 tok/s\x1b[0m\n"
    );
}
