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

use agyline::{CliStreams, MAX_STDIN_BYTES, ThemePolarity, run_cli, system_theme_polarity};

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

    let output = child.wait_with_output().expect("wait agyline");
    let _ = fs::remove_dir_all(&isolated_home);
    output
}

fn run_with_stdin(args: &[&str], stdin_bytes: &[u8]) -> Output {
    run_with_stdin_and_env(args, stdin_bytes, &[])
}

// Pre-session `{}` must still print the default line: stdout only, 0 B stderr, exit 0, adapting
// to COLORFGBG (and host OS light/dark theme when COLORFGBG is unset) with a full-bar scale.
#[test]
fn pipe_renders_pre_session_empty_object_with_zero_stderr_and_exit_zero() {
    let expected_dark = concat!(
        "  \x1b[0m\x1b[0;1;38;2;203;166;247m\u{2726} Model\x1b[0;38;2;108;112;134m \u{2502} ",
        "\x1b[0;38;2;92;121;97m\u{2758}\x1b[0;38;2;101;121;98m\u{2758}\x1b[0;38;2;110;121;99m\u{2758}\x1b[0;38;2;119;121;101m\u{2758}\x1b[0;38;2;128;121;102m\u{2758}\x1b[0;38;2;132;116;99m\u{2758}\x1b[0;38;2;132;106;90m\u{2758}\x1b[0;38;2;132;96;86m\u{2758}\x1b[0;38;2;131;88;93m\u{2758}\x1b[0;38;2;129;79;100m\u{2758}",
        "\x1b[0;38;2;166;227;161m 0.0%\x1b[0m\n",
        "  \x1b[0m\x1b[0;38;2;127;132;156m\u{25cb} idle\x1b[0m\n",
    );
    let expected_light = concat!(
        "  \x1b[0m\x1b[0;1;38;2;136;57;239m\u{2726} Model\x1b[0;38;2;140;143;161m \u{2502} ",
        "\x1b[0;38;2;143;196;134m\u{2758}\x1b[0;38;2;162;194;132m\u{2758}\x1b[0;38;2;182;192;131m\u{2758}\x1b[0;38;2;201;190;129m\u{2758}\x1b[0;38;2;220;188;127m\u{2758}\x1b[0;38;2;234;182;124m\u{2758}\x1b[0;38;2;242;171;120m\u{2758}\x1b[0;38;2;245;159;119m\u{2758}\x1b[0;38;2;234;138;131m\u{2758}\x1b[0;38;2;223;117;142m\u{2758}",
        "\x1b[0;38;2;64;160;43m 0.0%\x1b[0m\n",
        "  \x1b[0m\x1b[0;38;2;124;127;147m\u{25cb} idle\x1b[0m\n",
    );

    let dark_out = run_with_stdin_and_env(&[], b"{}", &[("COLORFGBG", "15;0")]);
    assert_eq!(dark_out.status.code(), Some(0));
    assert!(dark_out.stderr.is_empty(), "stderr must be 0 B");
    assert_eq!(String::from_utf8_lossy(&dark_out.stdout), expected_dark);

    let light_out = run_with_stdin_and_env(&[], b"{}", &[("COLORFGBG", "0;15")]);
    assert_eq!(light_out.status.code(), Some(0));
    assert!(light_out.stderr.is_empty(), "stderr must be 0 B");
    assert_eq!(String::from_utf8_lossy(&light_out.stdout), expected_light);

    let auto_out = run_with_stdin(&[], b"{}");
    assert_eq!(auto_out.status.code(), Some(0));
    assert!(auto_out.stderr.is_empty(), "stderr must be 0 B");
    let expected_auto = match system_theme_polarity() {
        Some(ThemePolarity::Light) => expected_light,
        Some(ThemePolarity::Dark) | None => expected_dark,
    };
    assert_eq!(String::from_utf8_lossy(&auto_out.stdout), expected_auto);
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
    let expected_ver = format!("agyline {}\n", env!("CARGO_PKG_VERSION"));
    for arg in ["-V", "--version", "version"] {
        let out = run_with_stdin(&[arg], b"");
        assert_eq!(out.status.code(), Some(0), "arg {arg:?}");
        assert!(out.stderr.is_empty(), "arg {arg:?}");
        let text = String::from_utf8(out.stdout).expect("utf8");
        assert_eq!(text, expected_ver, "arg {arg:?}");
    }

    let bad_ver = run_with_stdin(&["version", "extra"], b"");
    assert_eq!(bad_ver.status.code(), Some(2));
    assert!(bad_ver.stdout.is_empty());
    assert!(!bad_ver.stderr.is_empty());

    for flag in ["-h", "--help"] {
        let out = run_with_stdin(&[flag], b"");
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stderr.is_empty());
        let text = String::from_utf8(out.stdout).expect("utf8");
        assert!(text.contains("agyline"));
        assert!(text.contains("config"));
        assert!(text.contains("set"));
        assert!(text.contains("version"));
        assert!(text.contains("preview"));
        assert!(text.contains("install"));
        assert!(text.contains("uninstall"));
        assert!(text.contains("update"));
        assert!(!text.contains("__refresh-git"));
        assert!(!text.contains("__touch"));
        assert!(!text.contains("__refresh-transcript"));
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
    assert!(!text.contains(";48;2;"));
    assert_eq!(
        text,
        concat!(
            "    \x1b[0m\x1b[0;1;38;2;136;57;239m\u{2726} Gemini 4 Carbon\x1b[0;38;2;124;127;147m [high]\x1b[0;38;2;140;143;161m \u{2502} ",
            "\x1b[0;38;2;143;196;134m\u{2758}\x1b[0;38;2;162;194;132m\u{2758}\x1b[0;38;2;182;192;131m\u{2758}\x1b[0;38;2;201;190;129m\u{2758}\x1b[0;38;2;220;188;127m\u{2758}\x1b[0;38;2;234;182;124m\u{2758}\x1b[0;38;2;242;171;120m\u{2758}\x1b[0;38;2;245;159;119m\u{2758}\x1b[0;38;2;234;138;131m\u{2758}\x1b[0;38;2;223;117;142m\u{2758}",
            "\x1b[0;38;2;64;160;43m 0.0%\x1b[0;38;2;140;143;161m \u{2502} \x1b[0;38;2;230;69;83m\u{25b8}\u{25b8} best-of-n\x1b[0m\n",
        )
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

// `agyline config` prints active effective settings, resolved config.toml path, and existence
// without opening the TUI or writing files; `-c`/`--config` remains the TUI trigger.
#[test]
fn cli_config_subcommand_prints_effective_settings_and_path() {
    let home = std::env::temp_dir().join(format!(
        "agyline_cli_config_{}_{}",
        std::process::id(),
        SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let home_str = home.to_str().expect("utf8 home");
    let expected_cfg = home.join("config.toml");

    // 1. Missing config.toml -> prints defaults, reports not found, creates nothing.
    let out = run_with_stdin_and_env(&["config"], b"", &[("AGYLINE_HOME", home_str)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    assert!(
        !home.exists(),
        "agyline config must never create AGYLINE_HOME"
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        text.contains(&expected_cfg.display().to_string()),
        "missing resolved config.toml path:\n{text}"
    );
    for needle in [
        "preset",
        "default",
        "palette",
        "catppuccin",
        "theme",
        "auto",
        "item_style",
        "minimal",
        "bar_style",
        "cells",
        "bar_gradient",
        "true",
        "separator",
        "compact_at",
        "88",
        "single_line",
        "false",
        "overflow",
        "fit",
    ] {
        assert!(text.contains(needle), "missing '{needle}' in:\n{text}");
    }
    assert!(
        text.contains("not found") || text.contains("false"),
        "must report that config.toml does not exist on disk:\n{text}"
    );

    // 2. Existing config.toml + env overrides are reflected.
    fs::create_dir_all(&home).expect("mkdir");
    fs::write(
        &expected_cfg,
        "palette = \"nord\"\nitem_style = \"badge\"\ncompact_at = 72\noverflow = \"none\"\n",
    )
    .expect("write config.toml");
    let out2 = run_with_stdin_and_env(
        &["config"],
        b"",
        &[
            ("AGYLINE_HOME", home_str),
            ("AGY_THEME", "light"),
            ("AGY_STATUSLINE_LINES", "1"),
        ],
    );
    assert_eq!(out2.status.code(), Some(0));
    assert!(out2.stderr.is_empty());
    let text2 = String::from_utf8(out2.stdout).expect("utf8");
    assert!(
        text2.contains("exists"),
        "must report config exists:\n{text2}"
    );
    assert!(text2.contains("nord"));
    assert!(text2.contains("badge"));
    assert!(text2.contains("72"));
    assert!(text2.contains("none"));
    assert!(text2.contains("light"));
    assert!(text2.contains("single_line") && text2.contains("true"));

    // 2b. Capability degradation on `item_style` (`pill` + `unicode` -> effective `badge`) is shown.
    fs::write(
        &expected_cfg,
        "item_style = \"pill\"\nglyphs = \"unicode\"\n",
    )
    .expect("write pill+unicode");
    let degraded_out = run_with_stdin_and_env(
        &["config"],
        b"",
        &[("AGYLINE_HOME", home_str), ("COLORTERM", "truecolor")],
    );
    assert_eq!(degraded_out.status.code(), Some(0));
    let degraded_txt = String::from_utf8(degraded_out.stdout).expect("utf8");
    assert!(
        degraded_txt.contains("item_style: pill (effective: badge)"),
        "config output must surface degraded item_style:\n{degraded_txt}"
    );

    // 3. Broken config.toml -> warns on stderr with line/col and still exits 0 with defaults.
    fs::write(&expected_cfg, "version = 1\nbroken == 123\n").expect("write broken");
    let broken_cfg = run_with_stdin_and_env(&["config"], b"", &[("AGYLINE_HOME", home_str)]);
    assert_eq!(broken_cfg.status.code(), Some(0));
    let broken_err = String::from_utf8(broken_cfg.stderr).expect("utf8");
    assert!(broken_err.contains("line 2") && broken_err.contains("column"));
    let broken_out = String::from_utf8(broken_cfg.stdout).expect("utf8");
    assert!(broken_out.contains("syntax error") && broken_out.contains("default"));

    // 4. Help and extra-arg rejection.
    let help = run_with_stdin_and_env(&["config", "--help"], b"", &[("AGYLINE_HOME", home_str)]);
    assert_eq!(help.status.code(), Some(0));
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: agyline config"));

    for bad in [&["config", "extra"][..], &["config", "--bogus"][..]] {
        let res = run_with_stdin_and_env(bad, b"", &[("AGYLINE_HOME", home_str)]);
        assert_eq!(res.status.code(), Some(2), "args {bad:?}");
        assert!(res.stdout.is_empty());
        assert!(!res.stderr.is_empty());
    }

    let _ = fs::remove_dir_all(&home);
}

// `agyline set <key> [value]` lists valid choices when value is omitted, atomically updates
// config.toml while preserving unrelated keys when valid, refuses to overwrite broken TOML with
// exit 1, and rejects invalid keys/values with exit 2 and clear stderr diagnostics.
#[test]
fn cli_set_subcommand_lists_updates_atomically_and_validates() {
    let home = std::env::temp_dir().join(format!(
        "agyline_cli_set_{}_{}",
        std::process::id(),
        SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let home_str = home.to_str().expect("utf8 home");
    let cfg_path = home.join("config.toml");

    // 1. Help prints supported keys and creates nothing.
    let help = run_with_stdin_and_env(&["set", "--help"], b"", &[("AGYLINE_HOME", home_str)]);
    assert_eq!(help.status.code(), Some(0));
    assert!(help.stderr.is_empty());
    let help_txt = String::from_utf8(help.stdout).expect("utf8");
    for k in [
        "preset",
        "palette",
        "theme",
        "style",
        "bar",
        "bar-gradient",
        "sep",
        "compact-at",
    ] {
        assert!(
            help_txt.contains(k),
            "set --help missing '{k}':\n{help_txt}"
        );
    }
    assert!(!home.exists());

    // 2. Omitting <value> lists options for each key without touching disk.
    for (key, expected_tokens) in [
        ("preset", &["default", "hud", "classic", "cometix"][..]),
        (
            "palette",
            &["catppuccin", "tokyo-night", "nord", "mono"][..],
        ),
        ("theme", &["auto", "dark", "light"][..]),
        ("style", &["minimal", "badge", "pill"][..]),
        (
            "bar",
            &["cells", "line", "parallelogram", "braille", "ascii"][..],
        ),
        ("bar-gradient", &["true", "false"][..]),
        ("sep", &["pipe", "dashed", "dot", "square", "none"][..]),
        ("compact-at", &["0", "500"][..]),
    ] {
        let out = run_with_stdin_and_env(&["set", key], b"", &[("AGYLINE_HOME", home_str)]);
        assert_eq!(out.status.code(), Some(0), "set {key}");
        assert!(out.stderr.is_empty(), "set {key}");
        let txt = String::from_utf8(out.stdout).expect("utf8");
        for tok in expected_tokens {
            assert!(txt.contains(tok), "set {key} missing '{tok}':\n{txt}");
        }
    }
    assert!(
        !cfg_path.exists(),
        "listing options must never create config.toml"
    );

    // 3. Seed unrelated settings and update each key sequentially.
    fs::create_dir_all(&home).expect("mkdir");
    fs::write(
        &cfg_path,
        concat!(
            "indent = \"    \"\n",
            "overflow = \"none\"\n",
            "\n[git]\n",
            "cache_ms = 5555\n",
            "\n[cache_relay]\n",
            "url = \"http://127.0.0.1:9999\"\n",
        ),
    )
    .expect("seed config.toml");

    let updates: &[(&str, &str, &str)] = &[
        ("palette", "TOKYO-NIGHT", "palette = \"tokyo-night\""),
        ("theme", "light", "theme = \"light\""),
        ("style", "pill", "item_style = \"pill\""),
        ("bar", "blocks", "bar_style = \"parallelogram\""),
        ("bar-gradient", "off", "bar_gradient = false"),
        ("bar-gradient", "1", "bar_gradient = true"),
        ("sep", "DASHED", "separator = \"dashed\""),
        ("sep", " :: ", "separator = \" :: \""),
        ("sep", "-", "separator = \"-\""),
        ("compact-at", "0", "compact_at = 0"),
        ("compact-at", "120", "compact_at = 120"),
    ];
    for &(key, val, expected_line) in updates {
        let out = run_with_stdin_and_env(&["set", key, val], b"", &[("AGYLINE_HOME", home_str)]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "set {key} {val} failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stderr.is_empty());
        assert!(!out.stdout.is_empty());
        let saved = fs::read_to_string(&cfg_path).expect("read config.toml");
        assert!(
            saved.contains(expected_line),
            "expected '{expected_line}' in config.toml after set {key} {val}:\n{saved}"
        );
        assert!(saved.contains("indent = \"    \""));
        assert!(saved.contains("overflow = \"none\""));
        assert!(saved.contains("cache_ms = 5555"));
        assert!(saved.contains("url = \"http://127.0.0.1:9999\""));
    }

    // Applying preset updates layout/style/thresholds while keeping git/relay/overflow.
    let preset_out = run_with_stdin_and_env(
        &["set", "preset", "HUD"],
        b"",
        &[("AGYLINE_HOME", home_str)],
    );
    assert_eq!(preset_out.status.code(), Some(0));
    let saved_hud = fs::read_to_string(&cfg_path).expect("read hud config.toml");
    assert!(saved_hud.contains("preset = \"hud\""));
    assert!(saved_hud.contains("palette = \"terminal\""));
    assert!(saved_hud.contains("bar_style = \"line\""));
    assert!(saved_hud.contains("overflow = \"none\""));
    assert!(saved_hud.contains("cache_ms = 5555"));
    assert!(saved_hud.contains("url = \"http://127.0.0.1:9999\""));

    // Round-trip via `agyline config` reflects the persisted hud preset.
    let cfg_after = run_with_stdin_and_env(&["config"], b"", &[("AGYLINE_HOME", home_str)]);
    assert_eq!(cfg_after.status.code(), Some(0));
    let cfg_after_txt = String::from_utf8(cfg_after.stdout).expect("utf8");
    assert!(cfg_after_txt.contains("preset: hud") && cfg_after_txt.contains("palette: terminal"));

    // Fresh AGYLINE_HOME creation on first `agyline set` works without pre-existing directory.
    let fresh_home = std::env::temp_dir().join(format!(
        "agyline_cli_set_fresh_{}_{}",
        std::process::id(),
        SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let fresh_str = fresh_home.to_str().expect("utf8 fresh");
    let fresh_res = run_with_stdin_and_env(
        &["set", "palette", "mono"],
        b"",
        &[("AGYLINE_HOME", fresh_str)],
    );
    assert_eq!(fresh_res.status.code(), Some(0));
    assert!(
        fs::read_to_string(fresh_home.join("config.toml"))
            .expect("read fresh config")
            .contains("palette = \"mono\"")
    );
    let _ = fs::remove_dir_all(&fresh_home);

    // No stray temp files left behind in AGYLINE_HOME.
    let entries: Vec<String> = fs::read_dir(&home)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["config.toml".to_string()]);

    // 4. Invalid invocations exit 2, emit 0 B stdout, clear stderr, and leave config untouched.
    let before_invalid = fs::read(&cfg_path).expect("read before invalid");
    let invalid_cases: &[&[&str]] = &[
        &["set"],
        &["set", "--bogus"],
        &["set", "unknown-key"],
        &["set", "unknown-key", "val"],
        &["set", "theme", "dark", "extra"],
        &["set", "preset", "nonexistent_preset"],
        &["set", "palette", "nonexistent_palette"],
        &["set", "theme", "neon"],
        &["set", "style", "box"],
        &["set", "bar", "waves"],
        &["set", "bar-gradient", "maybe"],
        &["set", "sep", ""],
        &["set", "sep", "a\nb"],
        &["set", "compact-at", "-1"],
        &["set", "compact-at", "501"],
        &["set", "compact-at", "abc"],
    ];
    for args in invalid_cases {
        let res = run_with_stdin_and_env(args, b"", &[("AGYLINE_HOME", home_str)]);
        assert_eq!(
            res.status.code(),
            Some(2),
            "args {args:?} must exit 2, stderr: {:?}",
            String::from_utf8_lossy(&res.stderr)
        );
        assert!(res.stdout.is_empty(), "args {args:?} must emit 0 B stdout");
        assert!(!res.stderr.is_empty(), "args {args:?} must emit stderr");
    }
    assert_eq!(
        fs::read(&cfg_path).expect("read after invalid"),
        before_invalid,
        "invalid set calls must not modify config.toml"
    );

    // 5. Existing broken TOML refuses overwrite and exits 1 without modifying the file.
    let broken_bytes = b"version = 1\nbroken == 123\n";
    fs::write(&cfg_path, broken_bytes).expect("write broken");
    let broken_res = run_with_stdin_and_env(
        &["set", "theme", "dark"],
        b"",
        &[("AGYLINE_HOME", home_str)],
    );
    assert_eq!(broken_res.status.code(), Some(1));
    assert!(broken_res.stdout.is_empty());
    assert!(!broken_res.stderr.is_empty());
    assert_eq!(
        fs::read(&cfg_path).expect("read broken after set"),
        broken_bytes,
        "broken config.toml must not be overwritten"
    );

    let _ = fs::remove_dir_all(&home);
}

// Opt-in `AGYLINE_DEBUG=1` writes raw stdin bytes to `<AGYLINE_HOME>/state/last_payload.json`
// best-effort without affecting stdout/stderr/exit-0; when unset or != "1", zero debug I/O runs.
#[test]
fn pipe_agyline_debug_captures_raw_payload_only_when_one_and_never_fails_render() {
    // Omit `cwd` so the default line's `git` slot does not spawn an async `__refresh-git` worker
    // that could race with the zero-directory assertion below.
    let payload = br#"{"model":{"display_name":"Claude Opus 4.6"}}"#;

    // 1. Unset and non-"1" values produce zero debug files and do not create AGYLINE_HOME.
    let run_stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for val in [None, Some("0"), Some("false"), Some("true"), Some("2")] {
        let home = std::env::temp_dir().join(format!(
            "agyline_debug_off_{}_{run_stamp}_{}",
            std::process::id(),
            SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let home_str = home.to_str().expect("utf8 home");
        let mut envs = vec![("AGYLINE_HOME", home_str)];
        if let Some(v) = val {
            envs.push(("AGYLINE_DEBUG", v));
        }
        let out = run_with_stdin_and_env(&[], payload, &envs);
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stderr.is_empty());
        assert!(!out.stdout.is_empty());
        assert!(
            !home.join("state").join("last_payload.json").exists(),
            "AGYLINE_DEBUG={val:?} must not write last_payload.json"
        );
        assert!(
            !home.exists(),
            "AGYLINE_DEBUG={val:?} on pre-session payload must not create AGYLINE_HOME"
        );
    }

    // 2. AGYLINE_DEBUG=1 captures raw stdin verbatim without altering stdout or exit code.
    let home_on = std::env::temp_dir().join(format!(
        "agyline_debug_on_{}_{}",
        std::process::id(),
        SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let home_on_str = home_on.to_str().expect("utf8 home");
    let baseline = run_with_stdin_and_env(&[], payload, &[]);
    let debug_out = run_with_stdin_and_env(
        &[],
        payload,
        &[("AGYLINE_HOME", home_on_str), ("AGYLINE_DEBUG", "1")],
    );
    assert_eq!(debug_out.status.code(), Some(0));
    assert!(debug_out.stderr.is_empty());
    assert_eq!(debug_out.stdout, baseline.stdout);
    let captured =
        fs::read(home_on.join("state").join("last_payload.json")).expect("read last_payload.json");
    assert_eq!(captured, payload);
    let _ = fs::remove_dir_all(&home_on);

    // 3. Unwritable AGYLINE_HOME (a regular file) must still render cleanly and exit 0.
    let blocking_file = std::env::temp_dir().join(format!(
        "agyline_debug_blocked_{}_{}",
        std::process::id(),
        SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&blocking_file, b"not a directory").expect("write blocking file");
    let blocked_out = run_with_stdin_and_env(
        &[],
        payload,
        &[
            ("AGYLINE_HOME", blocking_file.to_str().expect("utf8")),
            ("AGYLINE_DEBUG", "1"),
        ],
    );
    assert_eq!(blocked_out.status.code(), Some(0));
    assert!(blocked_out.stderr.is_empty());
    assert_eq!(blocked_out.stdout, baseline.stdout);
    let _ = fs::remove_file(&blocking_file);
}
