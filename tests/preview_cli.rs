//! Surface and determinism of `agyline preview`: it shares the live renderer but is stateless
//! (NoopSpawner, fixed scenario anchor, zero AGYLINE_HOME/state writes). Pins `-s/--scenario`,
//! `--all`, `--list`, `-i/--input` (path or `-`), `-w/--width` (0 = no narrowing), `--plain`,
//! and the usage/error exit codes.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agyline::{
    NoopSpawner, RenderEnv, builtin_scenarios, find_scenario, run_live_frame, str_display_width,
};

static PREVIEW_SEQ: AtomicU64 = AtomicU64::new(0);

struct PreviewSandbox {
    root: PathBuf,
    home: PathBuf,
}

impl PreviewSandbox {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = PREVIEW_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "agyline_preview_test_{}_{nanos}_{seq}_{tag}",
            std::process::id()
        ));
        let home = root.join("agyline_home");
        fs::create_dir_all(&root).expect("create preview sandbox");
        Self { root, home }
    }

    /// Runs the binary against this sandbox's isolated `AGYLINE_HOME` with all `AGY_*`/color env
    /// stripped, so preview output is deterministic and never reads the user's real config.
    fn run(&self, args: &[&str], stdin_bytes: &[u8], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_agyline"));
        cmd.args(args)
            .env("AGYLINE_HOME", &self.home)
            .env_remove("AGY_THEME")
            .env_remove("AGY_STATUSLINE_LINES")
            .env_remove("AGY_STATUSLINE_INDENT")
            .env_remove("AGY_SHOW_DEFAULT_MODE")
            .env_remove("AGY_CACHE_RELAY")
            .env_remove("AGYLINE_DEBUG")
            .env_remove("COLORFGBG")
            .env_remove("NO_COLOR")
            .env_remove("COLORTERM")
            .env_remove("TERM")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for &(k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn agyline");
        {
            let mut stdin = child.stdin.take().expect("stdin");
            let _ = stdin.write_all(stdin_bytes);
        }
        child.wait_with_output().expect("wait agyline")
    }
}

impl Drop for PreviewSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn default_quota_uses_the_selected_model_and_keeps_both_periods_together() {
    let sb = PreviewSandbox::new("model_quota");
    fs::create_dir_all(&sb.home).expect("create classic home");
    fs::write(sb.home.join("config.toml"), "preset = \"classic\"\n").expect("write classic");
    let mut payload = serde_json::json!({
        "model": { "id": "models/GEMINI-4-carbon", "display_name": "Gemini Pro Orchestrator" },
        "cycle_mode": "auto",
        "terminal_width": 120,
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.94, "reset_in_seconds": 8040 },
            "gemini-weekly": { "remaining_fraction": 0.78, "reset_in_seconds": 280800 },
            "3p-5h": { "remaining_fraction": 0.12, "reset_in_seconds": 2520 },
            "3p-weekly": { "remaining_fraction": 0.08, "reset_in_seconds": 259200 },
        },
        "cost": { "total_usd": 1.42 }
    });
    let render = |payload: &serde_json::Value, width: &str, envs: &[(&str, &str)]| {
        let out = sb.run(
            &["preview", "--input", "-", "--plain", "--width", width],
            &serde_json::to_vec(payload).unwrap(),
            envs,
        );
        assert!(out.status.success(), "{:?}", out.stderr);
        let text = String::from_utf8(out.stdout).unwrap();
        let width = width.parse::<usize>().unwrap();
        assert!(text.lines().all(|line| str_display_width(line) <= width));
        text
    };

    let full = render(&payload, "120", &[]);
    let lines: Vec<_> = full.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("$1.42") && !lines[0].contains("94%"));
    assert!(lines[1].contains("94% 5h (2h14m)") && lines[1].contains("78% weekly (3d6h)"));
    assert!(!full.contains("12%") && !full.contains("8% weekly (3d)"));

    let narrow = render(&payload, "60", &[]);
    assert!(narrow.contains("94% 5h") && narrow.contains("78% weekly"));
    assert!(!narrow.contains("2h14m") || !narrow.contains("3d6h"));

    let single = render(&payload, "120", &[("AGY_STATUSLINE_LINES", "1")]);
    assert_eq!(single.lines().count(), 1);
    assert!(single.contains("94% 5h") && single.contains("78% weekly"));
    assert!(!single.contains("2h14m") && !single.contains("3d6h"));

    payload["tool_confirmation_pending"] = true.into();
    let approval = render(&payload, "24", &[]);
    assert!(approval.contains("confirm"));
    assert!(approval.contains("94% 5h") && !approval.contains("weekly"));

    let live = sb.run(
        &[],
        &serde_json::to_vec(&payload).unwrap(),
        &[("NO_COLOR", "1")],
    );
    assert!(live.status.success(), "{:?}", live.stderr);
    let live = String::from_utf8(live.stdout).unwrap();
    assert!(live.lines().nth(1).unwrap().contains("94% 5h (2h14m)"));

    payload["quota"]["gemini-weekly"] = serde_json::json!({
        "remaining_fraction": 0.78, "reset_time": "2023-11-18T04:13:20Z"
    });
    let frame_env = RenderEnv {
        home_dir: sb.home.clone(),
        temp_dir: sb.root.clone(),
        conversations_dir: sb.root.clone(),
        env_vars: BTreeMap::new(),
        now_ms: 1_700_000_000_000,
        spawner: &NoopSpawner,
    };
    let reset_from_timestamp = run_live_frame(&serde_json::to_vec(&payload).unwrap(), &frame_env);
    assert!(reset_from_timestamp.plain.contains("78% weekly (3d6h)"));

    for id in ["claude-sonnet-5-5", "GPT-OSS-120b"] {
        payload["model"]["id"] = id.into();
        let text = render(&payload, "120", &[]);
        assert!(text.contains("12% 5h (42m)") && text.contains("8% weekly (3d)"));
        assert!(!text.contains("94% 5h"));
    }

    payload["model"]["id"] = "opaque-id".into();
    payload["model"]["display_name"] = "Orchestrator (Argon)".into();
    payload["quota"]["gemini-5h"]["disabled"] = true.into();
    payload["quota"]["gemini-weekly"]["remaining_fraction"] = 0.into();
    payload["quota"]["gemini-weekly"]["reset_time"] = "invalid".into();
    let weekly_only = render(&payload, "120", &[]);
    assert!(!weekly_only.contains("% 5h"));
    assert!(weekly_only.contains("0% weekly") && !weekly_only.contains("weekly ("));
    payload["model"]["display_name"] = "Unrecognized".into();
    assert!(!render(&payload, "120", &[]).contains("% weekly"));
}

// The six canonical scenarios exist in fixed order; find_scenario resolves known names and
// rejects unknown ones, so `--list`/`-s` have a stable catalog.
#[test]
fn builtin_scenarios_expose_all_six_canonical_entries() {
    let all = builtin_scenarios();
    let names: Vec<&str> = all.iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        vec![
            "normal",
            "pre_session",
            "cache_warm_touch",
            "busy_subagents",
            "warn_crit",
            "narrow_stress",
            "showcase_wide",
            "showcase_critical",
            "showcase_compact",
        ]
    );
    assert!(find_scenario("normal").is_some());
    assert!(find_scenario("narrow_stress").is_some());
    assert!(find_scenario("showcase_wide").is_some());
    assert!(find_scenario("showcase_critical").is_some());
    assert!(find_scenario("showcase_compact").is_some());
    assert!(find_scenario("nonexistent").is_none());
}

#[test]
fn preview_default_scenario_list_all_plain_width_and_zero_state_writes() {
    let sb = PreviewSandbox::new("modes");

    // 1. Default `agyline preview` renders `normal` with ANSI and leaves AGYLINE_HOME untouched
    let def_out = sb.run(&["preview"], b"", &[]);
    assert_eq!(def_out.status.code(), Some(0));
    assert!(def_out.stderr.is_empty());
    let def_stdout = String::from_utf8(def_out.stdout).expect("utf8");
    assert!(def_stdout.contains("\x1b["));
    assert!(def_stdout.contains("Gemini 2.5 Pro"));
    assert!(
        !sb.home.exists(),
        "stateless preview must never create AGYLINE_HOME or state/: {:?}",
        sb.home
    );

    // 2. `--list` lists all 6 scenarios
    let list_out = sb.run(&["preview", "--list"], b"", &[]);
    assert_eq!(list_out.status.code(), Some(0));
    assert!(list_out.stderr.is_empty());
    let list_txt = String::from_utf8(list_out.stdout).expect("utf8");
    for name in [
        "normal",
        "pre_session",
        "cache_warm_touch",
        "busy_subagents",
        "warn_crit",
        "narrow_stress",
    ] {
        assert!(
            list_txt.contains(name),
            "missing {name} in --list:\n{list_txt}"
        );
    }

    // 3. `--all --plain` renders all 6 scenarios without ANSI sequences
    let all_out = sb.run(&["preview", "--all", "--plain"], b"", &[]);
    assert_eq!(all_out.status.code(), Some(0));
    assert!(all_out.stderr.is_empty());
    let all_txt = String::from_utf8(all_out.stdout).expect("utf8");
    assert!(!all_txt.contains("\x1b["));
    assert!(all_txt.contains("normal"));
    assert!(all_txt.contains("narrow_stress"));
    assert!(all_txt.contains("CONFIRM"));

    // 4. `-s narrow_stress --plain` narrows to <= 74 cols by default, while `-w 0` forces Full
    let narrow_default = sb.run(&["preview", "-s", "narrow_stress", "--plain"], b"", &[]);
    assert_eq!(narrow_default.status.code(), Some(0));
    let narrow_txt = String::from_utf8(narrow_default.stdout).expect("utf8");
    for line in narrow_txt.lines() {
        let w = str_display_width(line);
        assert!(w <= 74, "narrow_stress line width {w} > 74: {line:?}");
    }

    let narrow_full = sb.run(
        &["preview", "-s", "narrow_stress", "-w", "0", "--plain"],
        b"",
        &[],
    );
    assert_eq!(narrow_full.status.code(), Some(0));
    let full_txt = String::from_utf8(narrow_full.stdout).expect("utf8");
    let first_line_w = str_display_width(full_txt.lines().next().unwrap_or(""));
    assert!(
        first_line_w > 90,
        "-w 0 must disable narrowing (got {first_line_w}): {full_txt:?}"
    );
}

// `-i` renders a captured payload through the stateless frame builder (no git/relay/telemetry
// collection, no state/ writes); input read/parse failures exit 1, distinct from usage exit 2.
#[test]
fn preview_input_file_and_stdin_dash_are_stateless_and_handle_errors() {
    let sb = PreviewSandbox::new("input");
    fs::create_dir_all(&sb.home).expect("create home");

    let sample_path = sb.root.join("captured.json");
    fs::write(
        &sample_path,
        r#"{"session_id":"captured_1","model":{"display_name":"Claude Sonnet 4.5","effort":"high"},"vcs":{"branch":"feat/preview","dirty":true},"context_window":{"used_percentage":42}}"#,
    )
    .expect("write sample json");

    let file_out = sb.run(
        &[
            "preview",
            "-i",
            sample_path.to_str().expect("path"),
            "--plain",
        ],
        b"",
        &[],
    );
    assert_eq!(file_out.status.code(), Some(0));
    assert!(file_out.stderr.is_empty());
    let file_txt = String::from_utf8(file_out.stdout).expect("utf8");
    assert!(file_txt.contains("Sonnet 4.5"));
    assert!(file_txt.contains("feat/preview"));
    assert!(
        !sb.home.join("state").exists(),
        "preview -i must not write state/"
    );

    // `-i -` reads from stdin
    let dash_out = sb.run(
        &["preview", "--input", "-", "--plain", "--width", "40"],
        br#"{"session_id":"s2","model":{"display_name":"Gemini 2.5 Pro"},"cwd":"D:\\agyline"}"#,
        &[],
    );
    assert_eq!(dash_out.status.code(), Some(0));
    let dash_txt = String::from_utf8(dash_out.stdout).expect("utf8");
    for line in dash_txt.lines() {
        assert!(str_display_width(line) <= 40);
    }

    // Missing input file -> exit 1, 0 B stdout, error on stderr
    let missing_out = sb.run(
        &[
            "preview",
            "-i",
            sb.root.join("does_not_exist.json").to_str().expect("path"),
        ],
        b"",
        &[],
    );
    assert_eq!(missing_out.status.code(), Some(1));
    assert!(missing_out.stdout.is_empty());
    assert!(!missing_out.stderr.is_empty());

    // Malformed / non-object JSON on `-i -` -> exit 1, 0 B stdout, error on stderr
    for bad in [&b""[..], &b"[1,2,3]"[..], &b"{broken"[..]] {
        let bad_out = sb.run(&["preview", "-i", "-"], bad, &[]);
        assert_eq!(bad_out.status.code(), Some(1));
        assert!(bad_out.stdout.is_empty());
        assert!(!bad_out.stderr.is_empty());
    }
}

// Broken config degrades, not fails: fallback line + " ! config" badge on stdout, a line/column
// diagnostic on stderr, exit 0 (preview surfaces the warning the silent live pipe suppresses).
#[test]
fn preview_broken_config_warns_with_line_col_on_stderr_and_renders_fallback_exit_zero() {
    let sb = PreviewSandbox::new("broken_cfg");
    fs::create_dir_all(&sb.home).expect("create home");
    fs::write(
        sb.home.join("config.toml"),
        "version = 1\ntheme = \"dark\"\nbroken == 123\n",
    )
    .expect("write broken config.toml");

    let out = sb.run(&["preview", "-s", "pre_session", "--plain"], b"", &[]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert!(
        stdout.contains(" ! config"),
        "stdout must show ' ! config' fallback badge: {stdout:?}"
    );
    assert!(
        stderr.contains("line 3") && stderr.contains("column"),
        "stderr must include line/column diagnostic: {stderr:?}"
    );
}

// Invalid flag combinations, unknown scenario, and bad/missing width are usage errors: exit 2,
// 0 B stdout, stderr message; `--help` is the only zero-exit path and prints usage to stdout.
#[test]
fn preview_conflicting_flags_unknown_scenario_and_invalid_width_exit_two() {
    let sb = PreviewSandbox::new("usage_errors");
    let bad_cases: &[&[&str]] = &[
        &["preview", "-s", "unknown_scenario"],
        &["preview", "--all", "-s", "normal"],
        &["preview", "--list", "--all"],
        &["preview", "-i", "-", "-s", "normal"],
        &["preview", "-i", "-", "--list"],
        &["preview", "--showcase", "-s", "normal"],
        &["preview", "--showcase", "--all"],
        &["preview", "--showcase", "--list"],
        &["preview", "--showcase", "-i", "-"],
        &["preview", "-w", "not_a_number"],
        &["preview", "-w", "-5"],
        &["preview", "-w"],
        &["preview", "-s"],
        &["preview", "-i"],
        &["preview", "--bogus"],
        &["preview", "extra_positional"],
    ];

    for args in bad_cases {
        let out = sb.run(args, b"{}", &[]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "args {args:?} must exit with usage code 2 (stderr: {:?})",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty(), "args {args:?} must emit 0 B stdout");
        assert!(!out.stderr.is_empty());
    }

    let help_out = sb.run(&["preview", "--help"], b"", &[]);
    assert_eq!(help_out.status.code(), Some(0));
    assert!(help_out.stderr.is_empty());
    let help_txt = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_txt.contains("Usage: agyline preview"));
    assert!(
        help_txt.contains("--showcase"),
        "preview --help must document --showcase: {help_txt}"
    );
}

// `preview --showcase` statelessly renders built-in palette comparisons, high-usage >90%
// `→ /compact` alert, narrow <88 compact collapse, badge/pill Container Styles, and all semantic
// separator presets with zero writes under AGYLINE_HOME or AGYLINE_HOME/state.
#[test]
fn preview_showcase_renders_gallery_statelessly_and_honors_plain_and_width() {
    let sb = PreviewSandbox::new("showcase");

    // 1. ANSI showcase gallery without touching AGYLINE_HOME.
    let ansi_out = sb.run(&["preview", "--showcase"], b"", &[]);
    assert_eq!(
        ansi_out.status.code(),
        Some(0),
        "stderr: {:?}",
        String::from_utf8_lossy(&ansi_out.stderr)
    );
    assert!(ansi_out.stderr.is_empty());
    assert!(
        !sb.home.exists(),
        "preview --showcase must not create AGYLINE_HOME or state/: {:?}",
        sb.home
    );
    let ansi_txt = String::from_utf8(ansi_out.stdout).expect("utf8");
    for label in [
        "catppuccin",
        "tokyo-night",
        "nord",
        "mono",
        "badge",
        "pill",
        "pipe",
        "dashed",
        "dot",
        "square",
        "none",
    ] {
        assert!(
            ansi_txt.contains(label),
            "showcase missing '{label}' section:\n{ansi_txt}"
        );
    }
    // Distinct RGB anchors for Catppuccin Mocha (Mauve), Tokyo Night (Cyan), Nord (Frost).
    assert!(
        ansi_txt.contains("38;2;203;166;247"),
        "showcase missing Catppuccin RGB"
    );
    assert!(
        ansi_txt.contains("38;2;125;207;255"),
        "showcase missing Tokyo Night RGB"
    );
    assert!(
        ansi_txt.contains("38;2;136;192;208"),
        "showcase missing Nord RGB"
    );
    // High-usage >90% alert, badge/pill surface backgrounds, Powerline pill caps, and separators.
    assert!(
        ansi_txt.contains("→ /compact"),
        "showcase missing >90% → /compact alert"
    );
    assert!(
        ansi_txt.contains(";48;2;"),
        "showcase missing surface background SGR for badge/pill"
    );
    assert!(
        ansi_txt.contains('\u{e0b6}') && ansi_txt.contains('\u{e0b4}'),
        "showcase missing Powerline pill endcaps"
    );
    for sep_ch in ['\u{2502}', '\u{250a}', '\u{2022}', '\u{25aa}'] {
        assert!(
            ansi_txt.contains(sep_ch),
            "showcase missing separator glyph {sep_ch:?}"
        );
    }

    // 2. Plain showcase omits ANSI escapes and still shows compact single-row collapse (<88).
    let plain_out = sb.run(&["preview", "--showcase", "--plain"], b"", &[]);
    assert_eq!(plain_out.status.code(), Some(0));
    assert!(plain_out.stderr.is_empty());
    let plain_txt = String::from_utf8(plain_out.stdout).expect("utf8");
    assert!(!plain_txt.contains("\x1b["));
    assert!(plain_txt.contains("→ /compact"));

    // Locate the narrow <88 section and verify it collapsed to a single row <= 78 cols.
    let mut in_narrow = false;
    let mut narrow_rows = Vec::new();
    for line in plain_txt.lines() {
        if line.starts_with("=== ") {
            in_narrow = line.contains("<88");
            continue;
        }
        if in_narrow && !line.trim().is_empty() {
            narrow_rows.push(line);
        }
    }
    assert_eq!(
        narrow_rows.len(),
        1,
        "narrow <88 showcase section must collapse to 1 row, got: {narrow_rows:?}"
    );
    assert!(
        str_display_width(narrow_rows[0]) <= 78,
        "narrow <88 row width {} > 78: {:?}",
        str_display_width(narrow_rows[0]),
        narrow_rows[0]
    );

    // 3. Explicit `--width` caps every rendered row in the showcase.
    let capped_out = sb.run(
        &["preview", "--showcase", "--plain", "--width", "70"],
        b"",
        &[],
    );
    assert_eq!(capped_out.status.code(), Some(0));
    let capped_txt = String::from_utf8(capped_out.stdout).expect("utf8");
    for line in capped_txt.lines() {
        if line.starts_with("=== ") || line.trim().is_empty() {
            continue;
        }
        let w = str_display_width(line);
        assert!(
            w <= 70,
            "showcase row exceeded --width 70 (got {w}): {line:?}"
        );
    }

    // 4. Showcase still works cleanly even if user config.toml selects another preset, with zero
    // state/ writes.
    fs::create_dir_all(&sb.home).expect("create home");
    fs::write(sb.home.join("config.toml"), "preset = \"cometix\"\n").expect("write cometix");
    let custom_cfg_out = sb.run(&["preview", "--showcase", "--plain"], b"", &[]);
    assert_eq!(custom_cfg_out.status.code(), Some(0));
    let custom_txt = String::from_utf8(custom_cfg_out.stdout).expect("utf8");
    assert!(custom_txt.contains("→ /compact"));
    assert!(
        !sb.home.join("state").exists(),
        "preview --showcase must never write AGYLINE_HOME/state"
    );

    // 5. Dark-only palettes stay RGB under a light terminal (`COLORFGBG=0;15`), and explicit
    // `--width >= 88` updates the narrow section header instead of falsely claiming `<88`.
    let light_out = sb.run(
        &["preview", "--showcase"],
        b"",
        &[("COLORFGBG", "0;15"), ("COLORTERM", "truecolor")],
    );
    assert_eq!(light_out.status.code(), Some(0));
    let light_txt = String::from_utf8(light_out.stdout).expect("utf8");
    assert!(
        light_txt.contains("38;2;136;57;239"),
        "dual-polarity catppuccin showcase must adapt to Catppuccin Latte under light COLORFGBG"
    );
    assert!(
        light_txt.contains("38;2;125;207;255"),
        "tokyo-night showcase must keep its dark TrueColor RGB under light COLORFGBG"
    );
    assert!(
        light_txt.contains("38;2;136;192;208"),
        "nord showcase must keep its dark TrueColor RGB under light COLORFGBG"
    );

    let wide_override = sb.run(
        &["preview", "--showcase", "--plain", "--width", "92"],
        b"",
        &[],
    );
    assert_eq!(wide_override.status.code(), Some(0));
    let wide_override_txt = String::from_utf8(wide_override.stdout).expect("utf8");
    assert!(
        wide_override_txt.contains("=== Narrow scenario (overridden to 92 cols) ==="),
        "wide --width override must not emit a misleading <88 header:\n{wide_override_txt}"
    );
}

#[test]
fn hud_default_preset_renders_three_rows_and_collapses_on_single_line() {
    // 1. Default preset renders the new 2-row Catppuccin layout.
    let sb_def = PreviewSandbox::new("agybar_default");
    let def_full = sb_def.run(
        &["preview", "-s", "showcase_wide", "--plain", "-w", "0"],
        b"",
        &[],
    );
    assert_eq!(def_full.status.code(), Some(0));
    let def_txt = String::from_utf8(def_full.stdout).expect("utf8");
    let def_lines: Vec<&str> = def_txt.lines().collect();
    assert_eq!(
        def_lines.len(),
        2,
        "Default preset must emit 2 rows: {def_txt:?}"
    );
    assert!(def_lines[0].contains("✦ Claude Opus 4.6 [high]") && def_lines[0].contains('▰'));
    assert!(def_lines[1].contains("● working") && def_lines[1].contains("⎇ main*"));

    // 2. Explicit `hud` preset preserves the 3-row HUD layout and single-line collapse.
    let sb = PreviewSandbox::new("hud_preset");
    fs::create_dir_all(&sb.home).expect("mkdir");
    fs::write(sb.home.join("config.toml"), "preset = \"hud\"\n").expect("write hud preset");
    let full = sb.run(&["preview", "-s", "normal", "--plain", "-w", "0"], b"", &[]);
    assert_eq!(full.status.code(), Some(0));
    let full_txt = String::from_utf8(full.stdout).expect("utf8");
    let lines: Vec<&str> = full_txt.lines().collect();
    assert_eq!(lines.len(), 3, "HUD preset must emit 3 rows: {full_txt:?}");
    assert!(lines[0].contains("WORKING") && lines[0].contains("AUTO"));
    assert!(lines[0].contains("Gemini 2.5 Pro (High)") && lines[0].contains("main*"));
    assert!(lines[1].contains("Gemini 5h") && lines[1].contains("3P 5h"));
    assert!(
        lines[2].contains("sandbox") && lines[2].contains("token 4.6M") && lines[2].contains("58m")
    );

    let single = sb.run(
        &["preview", "-s", "normal", "--plain", "-w", "0"],
        b"",
        &[("AGY_STATUSLINE_LINES", "1")],
    );
    assert_eq!(single.status.code(), Some(0));
    let single_txt = String::from_utf8(single.stdout).expect("utf8");
    assert_eq!(single_txt.lines().count(), 1);
    assert!(single_txt.contains("WORKING") && single_txt.contains("AUTO"));
    assert!(!single_txt.contains("Gemini 5h") && !single_txt.contains("token 4.6M"));
}
