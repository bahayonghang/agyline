//! Process contract for the compiled `agyline` binary, driven over a real stdin pipe.
//! Pins the print path: no shell, no waiting on `git`, clean stdout/stderr separation, and
//! exit 0 even on malformed input. Git/relay work is confined to the silent `__refresh-git`
//! and `__touch` worker modes, so a slow background call can never hold the printed line.

use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Write};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agyline::{CliStreams, MAX_STDIN_BYTES, run_cli};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(0);

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_agyline")
}

/// Runs the real binary with a throwaway `AGYLINE_HOME` and all `AGY_*`/color env stripped, so
/// each case renders from a clean, isolated state and never reads or writes the user's config.
fn run_with_stdin_and_env(args: &[&str], stdin_bytes: &[u8], envs: &[(&str, &str)]) -> Output {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
    let isolated_home = std::env::temp_dir().join(format!(
        "agyline_proc_contract_{}_{nanos}_{seq}",
        std::process::id()
    ));

    let mut cmd = Command::new(bin_path());
    cmd.args(args)
        .env("AGYLINE_HOME", &isolated_home)
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

    let output = child.wait_with_output().expect("wait agyline");
    let _ = fs::remove_dir_all(&isolated_home);
    output
}

fn run_with_stdin(args: &[&str], stdin_bytes: &[u8]) -> Output {
    run_with_stdin_and_env(args, stdin_bytes, &[])
}

// Pre-session `{}` must still print the default line: stdout only, 0 B stderr, exit 0.
#[test]
fn pipe_renders_pre_session_empty_object_with_zero_stderr_and_exit_zero() {
    let output = run_with_stdin(&[], b"{}");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "stderr must be 0 B");
    assert_eq!(
        output.stdout,
        "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n"
            .as_bytes()
    );
}

// Any malformed or oversized frame is swallowed: exit 0 with 0 B on both streams, so a corrupt
// agy frame can never crash the line. The oversized case pins the 4 MiB stdin cap.
#[test]
fn pipe_malformed_utf8_and_oversized_inputs_exit_zero_with_empty_streams() {
    let mut oversized = Vec::with_capacity(MAX_STDIN_BYTES + 32);
    oversized.extend_from_slice(b"{\"pad\":\"");
    oversized.resize(MAX_STDIN_BYTES + 10, b'a');
    oversized.extend_from_slice(b"\"}");

    let bad_inputs: &[&[u8]] = &[
        b"",
        b"   \r\n\t ",
        b"\xff\xfe{\"session_id\":\"bad\"}",
        b"{\"unterminated\": ",
        b"[]",
        b"[1, 2]",
        b"\"scalar\"",
        b"42",
        b"true",
        b"null",
        &oversized,
    ];

    for (idx, payload) in bad_inputs.iter().enumerate() {
        let out = run_with_stdin(&[], payload);
        assert_eq!(out.status.code(), Some(0), "bad input #{idx} must exit 0");
        assert!(
            out.stdout.is_empty(),
            "bad input #{idx} must emit 0 B stdout"
        );
        assert!(
            out.stderr.is_empty(),
            "bad input #{idx} must emit 0 B stderr"
        );
    }
}

// Version/help are user surface: stdout, exit 0, and must not leak the internal worker modes.
#[test]
fn cli_version_and_help_print_to_stdout_and_omit_hidden_workers() {
    for flag in ["-V", "--version"] {
        let out = run_with_stdin(&[flag], b"");
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stderr.is_empty());
        let text = String::from_utf8(out.stdout).expect("utf8");
        assert_eq!(text, format!("agyline {}\n", env!("CARGO_PKG_VERSION")));
    }

    for flag in ["-h", "--help"] {
        let out = run_with_stdin(&[flag], b"");
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stderr.is_empty());
        let text = String::from_utf8(out.stdout).expect("utf8");
        assert!(text.contains("agyline"));
        assert!(text.contains("preview"));
        assert!(text.contains("install"));
        assert!(text.contains("uninstall"));
        assert!(!text.contains("__refresh-git"));
        assert!(!text.contains("__touch"));
    }
}

// Usage errors exit 2 with empty stdout and a --help hint on stderr. `--config` takes no path;
// its error must point at AGYLINE_HOME, the real config-relocation knob.
#[test]
fn cli_invalid_arguments_and_valued_config_flag_exit_two() {
    let invalid_invocations: &[&[&str]] = &[
        &["--unknown-flag"],
        &["-x"],
        &["doctor"],
        &["init"],
        &["--subagent"],
        &["--config", "foo.toml"],
        &["--config=foo.toml"],
        &["-c", "foo.toml"],
    ];

    for args in invalid_invocations {
        let out = run_with_stdin(args, b"{}");
        assert_eq!(
            out.status.code(),
            Some(2),
            "args {args:?} must return usage exit code 2"
        );
        assert!(out.stdout.is_empty(), "args {args:?} must emit 0 B stdout");
        let err = String::from_utf8(out.stderr).expect("utf8 stderr");
        assert!(
            !err.is_empty() && err.contains("--help"),
            "args {args:?} stderr should include --help hint, got: {err:?}"
        );
        if args[0].starts_with("-c") || args[0].starts_with("--config") {
            assert!(
                err.contains("AGYLINE_HOME"),
                "valued --config error must point to AGYLINE_HOME, got: {err:?}"
            );
        }
    }
}

// The configurator needs a TTY; on a pipe `-c` must refuse (exit 1, no stdout) rather than
// spill TUI escape codes into a non-terminal.
#[test]
fn cli_config_flag_on_non_tty_pipe_exits_one_without_tui_output() {
    for flag in ["-c", "--config"] {
        let out = run_with_stdin(&[flag], b"");
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}

// Detached workers must stay silent and exit 0 even on bogus args, an unsafe `bad/id` session
// (path traversal), or a non-loopback URL: their guards reject quietly, never leaking output.
#[test]
fn cli_hidden_workers_are_always_silent_and_exit_zero() {
    let hidden_invocations: &[&[&str]] = &[
        &["__refresh-git"],
        &["__refresh-git", "--cwd", "nonexistent"],
        &["__refresh-git", "--bogus"],
        &["__touch"],
        &["__touch", "--session", "x", "--url", "中文中文"],
        &[
            "__touch",
            "--session",
            "bad/id",
            "--url",
            "https://example.com",
        ],
    ];

    for args in hidden_invocations {
        let out = run_with_stdin(args, b"");
        assert_eq!(out.status.code(), Some(0), "hidden {args:?} must exit 0");
        assert!(
            out.stdout.is_empty(),
            "hidden {args:?} must emit 0 B stdout"
        );
        assert!(
            out.stderr.is_empty(),
            "hidden {args:?} must emit 0 B stderr"
        );
    }
}

// AGY_THEME/LINES/INDENT override the live render, and payload `email` PII must never print.
#[test]
fn pipe_respects_agy_env_overrides_and_omits_email() {
    let payload = br#"{"session_id":"","model":{"display_name":"Gemini 4 Carbon (High)"},"cycle_mode":"bestn","email":"secret@example.com"}"#;
    let out = run_with_stdin_and_env(
        &[],
        payload,
        &[
            ("AGY_THEME", "light "),
            ("AGY_STATUSLINE_LINES", "1"),
            ("AGY_STATUSLINE_INDENT", "    "),
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(!text.contains("secret@example.com"));
    assert_eq!(
        text,
        "    \x1b[0m\x1b[0;36m\u{ef23} Gemini 4 Carbon\x1b[0;90m \u{00b7} \x1b[0;36mHigh\x1b[0;90m | \x1b[0;35m\u{f49b} 0%\x1b[0;90m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0;90m | \x1b[0;31m\u{25b8}\u{25b8} best-of-n\x1b[0m\n"
    );
}

// A corrupt unicode reset_time must degrade quietly: quota still renders, the bad countdown is
// dropped (no `(`), exit 0 and 0 B stderr.
#[test]
fn pipe_ignores_malformed_unicode_quota_reset_times() {
    let home_dir = std::env::temp_dir().join(format!(
        "agyline_bad_reset_{}_{}",
        std::process::id(),
        SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&home_dir).expect("create config home");
    fs::write(
        home_dir.join("config.toml"),
        "[[line]]\nitems = [{ id = \"quota\", bucket = \"gemini-5h\", label = \"gem\", show_reset = true }]\n",
    )
    .expect("write config");
    for reset_time in ["2026-01-01T00:00:中", "2099-01-01T00:00:00+中0"] {
        let payload = serde_json::json!({
            "quota": { "gemini-5h": { "remaining_fraction": 0.53, "reset_time": reset_time } }
        });
        let out = run_with_stdin_and_env(
            &[],
            payload.to_string().as_bytes(),
            &[(
                "AGYLINE_HOME",
                home_dir.to_str().expect("config home UTF-8"),
            )],
        );
        assert_eq!(out.status.code(), Some(0), "reset time {reset_time:?}");
        assert!(out.stderr.is_empty(), "malformed reset must be silent");
        let text = String::from_utf8(out.stdout).expect("UTF-8 output");
        assert!(
            text.contains("53% gem"),
            "quota must still render: {text:?}"
        );
        assert!(
            !text.contains('('),
            "malformed reset must be omitted: {text:?}"
        );
    }
    fs::remove_dir_all(home_dir).expect("remove config home");
}

// In-process CliStreams: the configurator needs BOTH ends terminal, so a TTY stdin with a
// redirected stdout still refuses (exit 1, 0 B stdout) instead of drawing into a pipe.
#[test]
fn cli_tty_stdin_with_redirected_non_tty_stdout_exits_one_with_zero_stdout() {
    let args = vec![OsString::from("agyline")];
    let mut stdin = Cursor::new(Vec::<u8>::new());
    let mut stdout = Vec::<u8>::new();
    let mut stderr = Vec::<u8>::new();
    let code = {
        let mut streams = CliStreams {
            stdin: &mut stdin,
            stdin_is_terminal: true,
            stdout: &mut stdout,
            stdout_is_terminal: false,
            stderr: &mut stderr,
        };
        run_cli(&args, &mut streams)
    };
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "redirected stdout must remain 0 B");
    assert!(
        !stderr.is_empty(),
        "stderr must explain terminal requirement"
    );
}
