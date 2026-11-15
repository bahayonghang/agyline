use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};

use crate::collect::SystemSpawner;
use crate::input::MAX_STDIN_BYTES;
use crate::install::{
    InstallEnv, InstallOptions, UninstallOptions, run_install_cli, run_uninstall_cli,
};
use crate::statusline::{RenderEnv, run_live_frame};

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
enum CliAction {
    LiveRender,
    HiddenRefreshGit(Vec<OsString>),
    HiddenTouch(Vec<OsString>),
    Help(HelpTopic),
    Version,
    Configurator,
    Install(InstallOptions),
    Uninstall(UninstallOptions),
    SubcommandNotImplemented(&'static str),
    UsageError(String),
}

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

pub fn run_cli(args: &[OsString], streams: &mut CliStreams<'_>) -> u8 {
    // Fast path: zero arguments over a non-terminal stdin pipe.
    if args.len() <= 1 && !streams.stdin_is_terminal {
        return execute_live_render(streams);
    }

    let action = parse_args(args, streams.stdin_is_terminal);
    match action {
        CliAction::LiveRender => execute_live_render(streams),
        CliAction::HiddenRefreshGit(tail) => {
            crate::collect::run_hidden_refresh_git(&tail);
            0
        }
        CliAction::HiddenTouch(_tail) => 0,
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
            if !streams.stdin_is_terminal || !streams.stdout_is_terminal {
                let _ = writeln!(
                    streams.stderr,
                    "error: interactive terminal required on both stdin and stdout to open the configurator"
                );
                return 1;
            }
            let _ = writeln!(
                streams.stderr,
                "error: interactive configurator is not yet available in this build"
            );
            1
        }
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
        CliAction::SubcommandNotImplemented(subcmd) => {
            let _ = writeln!(
                streams.stderr,
                "error: subcommand '{subcmd}' is not yet available in this build"
            );
            1
        }
        CliAction::UsageError(msg) => {
            let _ = writeln!(streams.stderr, "error: {msg}");
            let _ = writeln!(streams.stderr, "Run 'agyline --help' for usage.");
            2
        }
    }
}

fn execute_live_render(streams: &mut CliStreams<'_>) -> u8 {
    let mut buf = Vec::with_capacity(4096);
    let mut limited = streams.stdin.take((MAX_STDIN_BYTES + 1) as u64);
    if limited.read_to_end(&mut buf).is_err() || buf.len() > MAX_STDIN_BYTES {
        return 0;
    }
    let spawner = SystemSpawner;
    let env = RenderEnv::from_process(&spawner);
    let out = run_live_frame(&buf, &env);
    if !out.ansi.is_empty() {
        let _ = streams.stdout.write_all(out.ansi.as_bytes());
        let _ = streams.stdout.flush();
    }
    0
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
    if first == "__refresh-git" {
        return CliAction::HiddenRefreshGit(args[2..].to_vec());
    }
    if first == "__touch" {
        return CliAction::HiddenTouch(args[2..].to_vec());
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
        "preview" => parse_subcommand_stub("preview", HelpTopic::Preview, &args[2..]),
        "install" => parse_install_args(&args[2..]),
        "uninstall" => parse_uninstall_args(&args[2..]),
        other if other.starts_with('-') => {
            CliAction::UsageError(format!("unknown flag '{other}'"))
        }
        other => CliAction::UsageError(format!("unknown subcommand '{other}'")),
    }
}

fn parse_subcommand_stub(name: &'static str, topic: HelpTopic, tail: &[OsString]) -> CliAction {
    for arg in tail {
        let s = arg.to_string_lossy();
        if s == "-h" || s == "--help" {
            return CliAction::Help(topic);
        }
    }
    CliAction::SubcommandNotImplemented(name)
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
  -s, --scenario <name>   Render a built-in preview scenario
      --all               Render all built-in scenarios
      --list              List built-in scenarios
  -i, --input <path|->    Render from a JSON payload file or '-' for stdin
  -w, --width <cols>      Override terminal width budget
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
      --purge             Remove <AGYLINE_HOME> directory
  -h, --help              Print help
"
        }
    }
}
