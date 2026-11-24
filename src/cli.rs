//! Process entry: the mode comes from argv plus stdin/stdout TTY-ness. No args over a pipe is the
//! live render `agy` invokes; a TTY opens the configurator; `preview`/`install`/`uninstall` are
//! user subcommands. `__refresh-git` and `__touch` are hidden re-entries for detached workers, the
//! mechanism keeping git and relay HTTP off the printing path. Exit: 0 ok, 1 failure, 2 bad args.

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

use crate::collect::{FrameData, NoopSpawner, SystemSpawner};
use crate::config::EffectiveConfig;
use crate::input::{MAX_STDIN_BYTES, parse_stdin_payload};
use crate::install::{
    InstallEnv, InstallOptions, UninstallOptions, run_install_cli, run_uninstall_cli,
};
use crate::platform;
use crate::render::{emit_ansi, emit_plain};
use crate::scenarios::{builtin_scenarios, find_scenario, stateless_frame_from_payload};
use crate::statusline::{RenderEnv, render_lines_with_width, run_live_frame_ansi};

/// The three streams and their TTY flags, injected so tests can drive every mode without a real
/// terminal. Mode selection reads the flags, never the stream contents.
pub struct CliStreams<'a> {
    pub stdin: &'a mut dyn Read,
    pub stdin_is_terminal: bool,
    pub stdout: &'a mut dyn Write,
    pub stdout_is_terminal: bool,
    pub stderr: &'a mut dyn Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Root,
    Preview,
    Install,
    Uninstall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreviewSource {
    DefaultScenario,
    NamedScenario(String),
    AllScenarios,
    ListScenarios,
    InputPath(OsString),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewOptions {
    source: PreviewSource,
    width: Option<usize>,
    plain: bool,
}

/// Parsed intent. The `Hidden*` variants are worker re-entry and stay out of help; `UsageError`
/// carries its message so parsing stays pure and the caller decides what to print.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    LiveRender,
    HiddenRefreshGit(Vec<OsString>),
    HiddenTouch(Vec<OsString>),
    HiddenRefreshTranscript(Vec<OsString>),
    Help(HelpTopic),
    Version,
    Configurator,
    Preview(PreviewOptions),
    Install(InstallOptions),
    Uninstall(UninstallOptions),
    UsageError(String),
}

/// Binds the real std streams, samples their TTY-ness and returns the exit code for `main`.
pub fn run_cli_from_process() -> u8 {
    let args: Vec<OsString> = std::env::args_os().collect();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let stdin_is_terminal = stdin.is_terminal();
    let stdout_is_terminal = stdout.is_terminal();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();
    let mut stderr_lock = stderr.lock();
    let mut streams = CliStreams {
        stdin: &mut stdin_lock,
        stdin_is_terminal,
        stdout: &mut stdout_lock,
        stdout_is_terminal,
        stderr: &mut stderr_lock,
    };
    run_cli(&args, &mut streams)
}

/// Dispatches one invocation and returns its exit code. The live render and hidden workers must
/// stay silent on stderr; the interactive paths may explain themselves there.
pub fn run_cli(args: &[OsString], streams: &mut CliStreams<'_>) -> u8 {
    // Fast path: zero arguments over a non-terminal stdin pipe.
    // The `agy` hook always invokes this shape, so it skips argument parsing entirely.
    if args.len() <= 1 && !streams.stdin_is_terminal {
        return execute_live_render(streams);
    }

    let action = parse_args(args, streams.stdin_is_terminal);
    match action {
        CliAction::LiveRender => execute_live_render(streams),
        // Worker re-entry: the git call and relay POST happen here, never on the printing path.
        CliAction::HiddenRefreshGit(tail) => {
            crate::collect::run_hidden_refresh_git(&tail);
            0
        }
        CliAction::HiddenTouch(tail) => crate::collect::touch::run_touch_worker(&tail),
        CliAction::HiddenRefreshTranscript(tail) => {
            crate::collect::run_hidden_refresh_transcript(&tail);
            0
        }
        CliAction::Help(topic) => {
            let text = help_text(topic);
            let _ = streams.stdout.write_all(text.as_bytes());
            let _ = streams.stdout.flush();
            0
        }
        CliAction::Version => {
            let line = format!("agyline {}\n", env!("CARGO_PKG_VERSION"));
            let _ = streams.stdout.write_all(line.as_bytes());
            let _ = streams.stdout.flush();
            0
        }
        CliAction::Configurator => {
            // Raw-mode keys come from stdin and the alternate screen goes to stdout: a redirected
            // stream on either side would leave the UI unreadable or spill escapes into a pipe.
            if !streams.stdin_is_terminal || !streams.stdout_is_terminal {
                let _ = writeln!(
                    streams.stderr,
                    "error: interactive terminal required on both stdin and stdout to open the configurator"
                );
                return 1;
            }
            match crate::tui::run_configurator() {
                Ok(_) => 0,
                Err(err) => {
                    let _ = writeln!(streams.stderr, "error: {err}");
                    1
                }
            }
        }
        CliAction::Preview(opts) => execute_preview(opts, streams),
        CliAction::Install(opts) => match InstallEnv::from_process() {
            Ok(env) => run_install_cli(&env, opts, streams),
            Err(err) => {
                let _ = writeln!(streams.stderr, "error: {err}");
                1
            }
        },
        CliAction::Uninstall(opts) => match InstallEnv::from_process() {
            Ok(env) => run_uninstall_cli(&env, opts, streams),
            Err(err) => {
                let _ = writeln!(streams.stderr, "error: {err}");
                1
            }
        },
        CliAction::UsageError(msg) => {
            let _ = writeln!(streams.stderr, "error: {msg}");
            let _ = writeln!(streams.stderr, "Run 'agyline --help' for usage.");
            2
        }
    }
}

/// The hot path: read one bounded frame, render it, write it. Never fails the invocation, because
/// `agy` shows a statusline error banner on a non-zero exit instead of the line.
fn execute_live_render(streams: &mut CliStreams<'_>) -> u8 {
    let mut buf = Vec::with_capacity(4096);
    // One byte past the cap, so an oversized frame is detected rather than silently truncated.
    let mut limited = streams.stdin.take((MAX_STDIN_BYTES + 1) as u64);
    if limited.read_to_end(&mut buf).is_err() || buf.len() > MAX_STDIN_BYTES {
        // Unreadable or oversized stdin: write nothing rather than a partial line, and exit 0.
        return 0;
    }
    let spawner = SystemSpawner;
    let env = RenderEnv::from_process(&spawner);
    let ansi = run_live_frame_ansi(&buf, &env);
    if !ansi.is_empty() {
        let _ = streams.stdout.write_all(ansi.as_bytes());
        let _ = streams.stdout.flush();
    }
    0
}

fn execute_preview(opts: PreviewOptions, streams: &mut CliStreams<'_>) -> u8 {
    if let PreviewSource::ListScenarios = opts.source {
        for sc in builtin_scenarios() {
            let _ = writeln!(streams.stdout, "{:<18}  {}", sc.name, sc.description);
        }
        let _ = streams.stdout.flush();
        return 0;
    }

    // Preview spawns no workers and writes no state: AGYLINE_HOME must stay untouched.
    let spawner = NoopSpawner;
    let env = RenderEnv::from_process(&spawner);
    let (config, loaded) = EffectiveConfig::resolve_with_diagnostics(&env.home_dir, &env.env_vars);

    // Unlike the silent live path, preview may report a broken config.toml and use defaults.
    let emit_broken_warning = |stderr: &mut dyn Write| {
        if let Some(ref err) = loaded.syntax_error {
            let cfg_path = env.home_dir.join("config.toml");
            let _ = writeln!(
                stderr,
                "warning: failed to parse {} ({err}); rendering fallback default configuration",
                cfg_path.display()
            );
        }
    };

    match opts.source {
        PreviewSource::ListScenarios => 0,
        PreviewSource::DefaultScenario => {
            emit_broken_warning(streams.stderr);
            if let Some(sc) = find_scenario("normal") {
                let frame = sc.frame_for_env(&env.env_vars);
                write_preview_frame(
                    &frame,
                    &config,
                    opts.width,
                    opts.plain,
                    streams.stdout_is_terminal,
                    streams.stdout,
                );
            }
            let _ = streams.stdout.flush();
            0
        }
        PreviewSource::NamedScenario(ref name) => {
            emit_broken_warning(streams.stderr);
            if let Some(sc) = find_scenario(name) {
                let frame = sc.frame_for_env(&env.env_vars);
                write_preview_frame(
                    &frame,
                    &config,
                    opts.width,
                    opts.plain,
                    streams.stdout_is_terminal,
                    streams.stdout,
                );
            }
            let _ = streams.stdout.flush();
            0
        }
        PreviewSource::AllScenarios => {
            emit_broken_warning(streams.stderr);
            for (idx, sc) in builtin_scenarios().iter().enumerate() {
                if idx > 0 {
                    let _ = writeln!(streams.stdout);
                }
                let _ = writeln!(streams.stdout, "=== {} ({}) ===", sc.name, sc.description);
                let frame = sc.frame_for_env(&env.env_vars);
                write_preview_frame(
                    &frame,
                    &config,
                    opts.width,
                    opts.plain,
                    streams.stdout_is_terminal,
                    streams.stdout,
                );
            }
            let _ = streams.stdout.flush();
            0
        }
        PreviewSource::InputPath(ref path) => {
            let raw_bytes = match read_preview_input(path, streams.stdin) {
                Ok(b) => b,
                Err(msg) => {
                    let _ = writeln!(streams.stderr, "error: {msg}");
                    return 1;
                }
            };
            let Some(payload) = parse_stdin_payload(&raw_bytes) else {
                let _ = writeln!(
                    streams.stderr,
                    "error: preview input must be a valid UTF-8 JSON object (<= 4 MiB)"
                );
                return 1;
            };
            emit_broken_warning(streams.stderr);
            // A captured payload uses the real clock; only built-in scenarios are anchor-pinned.
            let frame = stateless_frame_from_payload(payload, (env.now_ms / 1000) as i64);
            write_preview_frame(
                &frame,
                &config,
                opts.width,
                opts.plain,
                streams.stdout_is_terminal,
                streams.stdout,
            );
            let _ = streams.stdout.flush();
            0
        }
    }
}

/// Reads a preview payload from `path`, or from stdin when `path` is `-`. The error string is
/// user-facing; anything over `MAX_STDIN_BYTES` is refused rather than read.
fn read_preview_input(path: &OsString, stdin: &mut dyn Read) -> Result<Vec<u8>, String> {
    if path.as_os_str() == "-" {
        let mut buf = Vec::with_capacity(4096);
        let mut limited = stdin.take((MAX_STDIN_BYTES + 1) as u64);
        limited
            .read_to_end(&mut buf)
            .map_err(|e| format!("failed to read stdin: {e}"))?;
        if buf.len() > MAX_STDIN_BYTES {
            return Err("stdin payload exceeds 4 MiB limit".to_string());
        }
        return Ok(buf);
    }

    let p = Path::new(path);
    // Size first, so an oversized file is refused without buffering it into memory.
    let meta = fs::metadata(p).map_err(|e| format!("failed to read '{}': {e}", p.display()))?;
    if meta.len() > MAX_STDIN_BYTES as u64 {
        return Err(format!("'{}' exceeds 4 MiB limit", p.display()));
    }
    fs::read(p).map_err(|e| format!("failed to read '{}': {e}", p.display()))
}

/// Emits one preview frame through the live renderer. Width precedence: `--width` (0 means no
/// narrowing), then the payload's `terminal_width`, then the real terminal width when stdout is a
/// TTY. A pipe with no reported width stays un-narrowed.
fn write_preview_frame(
    frame: &FrameData,
    config: &EffectiveConfig,
    width_flag: Option<usize>,
    plain_flag: bool,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
) {
    let width_budget = match width_flag {
        Some(0) => None,
        Some(cols) => Some(cols),
        None => frame.payload.terminal_width.or_else(|| {
            stdout_is_terminal
                .then(platform::stdout_terminal_columns)
                .flatten()
        }),
    };
    let rendered = render_lines_with_width(frame, config, width_budget);
    let text = if plain_flag {
        emit_plain(&rendered, config)
    } else {
        emit_ansi(&rendered, config)
    };
    if !text.is_empty() {
        let _ = stdout.write_all(text.as_bytes());
    }
}

fn parse_args(args: &[OsString], stdin_is_terminal: bool) -> CliAction {
    if args.len() <= 1 {
        return if stdin_is_terminal {
            CliAction::Configurator
        } else {
            CliAction::LiveRender
        };
    }

    let first = args[1].to_string_lossy();
    // Worker modes are matched before flags, so their own --cwd/--session are not usage errors.
    if first == "__refresh-git" {
        return CliAction::HiddenRefreshGit(args[2..].to_vec());
    }
    if first == "__touch" {
        return CliAction::HiddenTouch(args[2..].to_vec());
    }
    if first == "__refresh-transcript" {
        return CliAction::HiddenRefreshTranscript(args[2..].to_vec());
    }

    match first.as_ref() {
        "-h" | "--help" => {
            if args.len() == 2 {
                CliAction::Help(HelpTopic::Root)
            } else {
                CliAction::UsageError(format!(
                    "unexpected argument '{}' after '{}'",
                    args[2].to_string_lossy(),
                    first
                ))
            }
        }
        "-V" | "--version" => {
            if args.len() == 2 {
                CliAction::Version
            } else {
                CliAction::UsageError(format!(
                    "unexpected argument '{}' after '{}'",
                    args[2].to_string_lossy(),
                    first
                ))
            }
        }
        "-c" | "--config" => {
            if args.len() == 2 {
                CliAction::Configurator
            } else {
                CliAction::UsageError(
                    "'--config' does not take a file path; set AGYLINE_HOME to relocate the config directory"
                        .to_string(),
                )
            }
        }
        s if s.starts_with("--config=") || (s.starts_with("-c") && s.len() > 2) => {
            CliAction::UsageError(
                "'--config' does not take a file path; set AGYLINE_HOME to relocate the config directory"
                    .to_string(),
            )
        }
        "preview" => parse_preview_args(&args[2..]),
        "install" => parse_install_args(&args[2..]),
        "uninstall" => parse_uninstall_args(&args[2..]),
        other if other.starts_with('-') => {
            CliAction::UsageError(format!("unknown flag '{other}'"))
        }
        other => CliAction::UsageError(format!("unknown subcommand '{other}'")),
    }
}

fn parse_preview_args(tail: &[OsString]) -> CliAction {
    // Help is scanned first so `preview --help --bogus` prints help, not a usage error.
    for arg in tail {
        let s = arg.to_string_lossy();
        if s == "--" {
            break;
        }
        if s == "-h" || s == "--help" {
            return CliAction::Help(HelpTopic::Preview);
        }
    }

    // Every source flag is collected so two of them are a usage error, not last-one-wins.
    let mut chosen_sources: Vec<PreviewSource> = Vec::new();
    let mut width: Option<usize> = None;
    let mut plain = false;

    let mut idx = 0usize;
    while idx < tail.len() {
        let s = tail[idx].to_string_lossy();
        match s.as_ref() {
            "--all" => chosen_sources.push(PreviewSource::AllScenarios),
            "--list" => chosen_sources.push(PreviewSource::ListScenarios),
            "--plain" => plain = true,
            "-s" | "--scenario" => {
                let Some(next) = tail.get(idx + 1) else {
                    return CliAction::UsageError(format!("missing scenario name after '{s}'"));
                };
                idx += 1;
                chosen_sources.push(PreviewSource::NamedScenario(
                    next.to_string_lossy().into_owned(),
                ));
            }
            flag if flag.starts_with("--scenario=") => {
                let val = &flag["--scenario=".len()..];
                if val.is_empty() {
                    return CliAction::UsageError(
                        "missing scenario name after '--scenario='".to_string(),
                    );
                }
                chosen_sources.push(PreviewSource::NamedScenario(val.to_string()));
            }
            "-i" | "--input" => {
                let Some(next) = tail.get(idx + 1) else {
                    return CliAction::UsageError(format!("missing path after '{s}'"));
                };
                idx += 1;
                chosen_sources.push(PreviewSource::InputPath(next.clone()));
            }
            flag if flag.starts_with("--input=") => {
                let val = &flag["--input=".len()..];
                if val.is_empty() {
                    return CliAction::UsageError("missing path after '--input='".to_string());
                }
                chosen_sources.push(PreviewSource::InputPath(OsString::from(val)));
            }
            "-w" | "--width" => {
                let Some(next) = tail.get(idx + 1) else {
                    return CliAction::UsageError(format!("missing column count after '{s}'"));
                };
                idx += 1;
                let val_str = next.to_string_lossy();
                let Ok(cols) = val_str.trim().parse::<usize>() else {
                    return CliAction::UsageError(format!(
                        "invalid '--width' value '{val_str}': expected non-negative integer"
                    ));
                };
                width = Some(cols);
            }
            flag if flag.starts_with("--width=") => {
                let val_str = &flag["--width=".len()..];
                let Ok(cols) = val_str.trim().parse::<usize>() else {
                    return CliAction::UsageError(format!(
                        "invalid '--width' value '{val_str}': expected non-negative integer"
                    ));
                };
                width = Some(cols);
            }
            "--" => {
                if idx + 1 < tail.len() {
                    return CliAction::UsageError(format!(
                        "unexpected argument '{}' for 'preview'",
                        tail[idx + 1].to_string_lossy()
                    ));
                }
                break;
            }
            flag if flag.starts_with('-') => {
                return CliAction::UsageError(format!("unknown flag '{flag}' for 'preview'"));
            }
            arg => {
                return CliAction::UsageError(format!("unexpected argument '{arg}' for 'preview'"));
            }
        }
        idx += 1;
    }

    if chosen_sources.len() > 1 {
        return CliAction::UsageError(
            "conflicting flags for 'preview': pass at most one of '--scenario', '--all', '--list', or '--input'"
                .to_string(),
        );
    }

    let source = chosen_sources
        .into_iter()
        .next()
        .unwrap_or(PreviewSource::DefaultScenario);

    if let PreviewSource::NamedScenario(ref name) = source
        && find_scenario(name).is_none()
    {
        return CliAction::UsageError(format!(
            "unknown scenario '{name}'; run 'agyline preview --list' to see built-in scenarios"
        ));
    }

    CliAction::Preview(PreviewOptions {
        source,
        width,
        plain,
    })
}

fn parse_install_args(tail: &[OsString]) -> CliAction {
    let mut opts = InstallOptions::default();
    let mut idx = 0;
    while idx < tail.len() {
        let s = tail[idx].to_string_lossy();
        match s.as_ref() {
            "-h" | "--help" => return CliAction::Help(HelpTopic::Install),
            "--dry-run" => opts.dry_run = true,
            "-y" | "--yes" => opts.yes = true,
            "--" => {
                if idx + 1 < tail.len() {
                    return CliAction::UsageError(format!(
                        "unexpected argument '{}' for 'install'",
                        tail[idx + 1].to_string_lossy()
                    ));
                }
                break;
            }
            flag if flag.starts_with('-') => {
                return CliAction::UsageError(format!("unknown flag '{flag}' for 'install'"));
            }
            arg => {
                return CliAction::UsageError(format!("unexpected argument '{arg}' for 'install'"));
            }
        }
        idx += 1;
    }
    CliAction::Install(opts)
}

fn parse_uninstall_args(tail: &[OsString]) -> CliAction {
    let mut opts = UninstallOptions::default();
    let mut idx = 0;
    while idx < tail.len() {
        let s = tail[idx].to_string_lossy();
        match s.as_ref() {
            "-h" | "--help" => return CliAction::Help(HelpTopic::Uninstall),
            "--dry-run" => opts.dry_run = true,
            "-y" | "--yes" => opts.yes = true,
            "--reset" => opts.reset = true,
            "--purge" => opts.purge = true,
            "--" => {
                if idx + 1 < tail.len() {
                    return CliAction::UsageError(format!(
                        "unexpected argument '{}' for 'uninstall'",
                        tail[idx + 1].to_string_lossy()
                    ));
                }
                break;
            }
            flag if flag.starts_with('-') => {
                return CliAction::UsageError(format!("unknown flag '{flag}' for 'uninstall'"));
            }
            arg => {
                return CliAction::UsageError(format!(
                    "unexpected argument '{arg}' for 'uninstall'"
                ));
            }
        }
        idx += 1;
    }
    CliAction::Uninstall(opts)
}

fn help_text(topic: HelpTopic) -> &'static str {
    // The hidden worker modes are deliberately absent: internal re-entry, not user commands.
    match topic {
        HelpTopic::Root => {
            "\
agyline - Fast, configurable status line for the Antigravity CLI (agy)

Usage:
  agyline                 Render status line from stdin pipe, or open configurator in a terminal
  agyline -c, --config    Open the interactive terminal configurator
  agyline <subcommand>    Run a subcommand

Options:
  -c, --config            Open the interactive configurator (uses AGYLINE_HOME)
  -h, --help              Print help
  -V, --version           Print version

Subcommands:
  preview                 Statelessly preview built-in scenarios or a captured payload
  install                 Configure agy settings.json to run agyline
  uninstall               Restore previous agy statusLine configuration
"
        }
        HelpTopic::Preview => {
            "\
Usage: agyline preview [options]

Options:
  -s, --scenario <name>   Render a built-in preview scenario (default: normal)
      --all               Render all built-in scenarios
      --list              List built-in scenarios
  -i, --input <path|->    Render from a JSON payload file or '-' for stdin
  -w, --width <cols>      Override terminal width budget (0 disables narrowing)
      --plain             Emit plain text without ANSI escape sequences
  -h, --help              Print help
"
        }
        HelpTopic::Install => {
            "\
Usage: agyline install [--dry-run] [-y|--yes]

Options:
      --dry-run           Show settings.json diff without writing
  -y, --yes               Apply without interactive prompt
  -h, --help              Print help
"
        }
        HelpTopic::Uninstall => {
            "\
Usage: agyline uninstall [--dry-run] [-y|--yes] [--reset] [--purge]

Options:
      --dry-run           Show settings.json diff without writing
  -y, --yes               Apply without interactive prompt
      --reset             Remove statusLine instead of restoring previous snapshot
      --purge             Remove a dedicated <AGYLINE_HOME>; refuses home, .gemini, and mixed directories
  -h, --help              Print help
"
        }
    }
}
