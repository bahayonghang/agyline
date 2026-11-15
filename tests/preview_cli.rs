//! Surface and determinism of `agyline preview`: it shares the live renderer but is stateless
//! (NoopSpawner, fixed scenario anchor, zero AGYLINE_HOME/state writes). Pins `-s/--scenario`,
//! `--all`, `--list`, `-i/--input` (path or `-`), `-w/--width` (0 = no narrowing), `--plain`,
//! and the usage/error exit codes.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agyline::{builtin_scenarios, find_scenario, str_display_width};

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
        ]
    );
    assert!(find_scenario("normal").is_some());
    assert!(find_scenario("narrow_stress").is_some());
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
    assert!(all_txt.contains("approval required"));

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
    assert!(String::from_utf8_lossy(&help_out.stdout).contains("Usage: agyline preview"));
}
