//! Process entry: the mode comes from argv plus stdin/stdout TTY-ness. No args over a pipe is the
//! live render `agy` invokes (with opt-in `AGYLINE_DEBUG=1` payload capture); a TTY opens the
//! configurator; `config`/`set`/`version`/`preview`/`install`/`uninstall` are user subcommands.
//! `__refresh-git`, `__touch`, and `__refresh-transcript` are hidden re-entries for detached
//! workers, the mechanism keeping git, relay HTTP, and transcript reads off the printing path.
//! Exit: 0 ok, 1 failure, 2 bad args.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::collect::{FrameData, NoopSpawner, SystemSpawner};
use crate::config::{
    ConfigDoc, DEFAULT_COMPACT_AT, EffectiveConfig, ItemStyle, PresetScope, ThemePolarity,
    ThemeSetting, load_config_doc,
    presets::{apply_preset, build_preset_doc, builtin_presets, discover_presets},
    serialize_config_doc,
};
use crate::input::{MAX_STDIN_BYTES, parse_stdin_payload};
use crate::install::{
    InstallEnv, InstallOptions, UninstallOptions, run_install_cli, run_uninstall_cli,
};
use crate::platform;
use crate::render::{BarStyle, discover_palettes, emit_ansi, emit_plain};
use crate::scenarios::{builtin_scenarios, find_scenario, stateless_frame_from_payload};
use crate::state::{atomic_write_bytes, ensure_state_dir};
use crate::statusline::{RenderEnv, render_lines_with_width, run_live_frame_ansi};

const MAX_COMPACT_AT: usize = 500;
const SET_KEYS_LIST: &str = "preset, palette, theme, style, bar, bar-gradient, sep, compact-at";
const SEMANTIC_SEPARATORS: [(&str, &str); 5] = [
    ("pipe", " │ "),
    ("dashed", " ┊ "),
    ("dot", " • "),
    ("square", " ▪ "),
    ("none", "  "),
];

static CLI_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

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
    Config,
    Set,
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
    Showcase,
    InputPath(OsString),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewOptions {
    source: PreviewSource,
    width: Option<usize>,
    plain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetKey {
    Preset,
    Palette,
    Theme,
    Style,
    Bar,
    BarGradient,
    Sep,
    CompactAt,
}

impl SetKey {
    const fn as_key(self) -> &'static str {
        match self {
            Self::Preset => "preset",
            Self::Palette => "palette",
            Self::Theme => "theme",
            Self::Style => "style",
            Self::Bar => "bar",
            Self::BarGradient => "bar-gradient",
            Self::Sep => "sep",
            Self::CompactAt => "compact-at",
        }
    }

    fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "preset" => Some(Self::Preset),
            "palette" => Some(Self::Palette),
            "theme" => Some(Self::Theme),
            "style" => Some(Self::Style),
            "bar" => Some(Self::Bar),
            "bar-gradient" => Some(Self::BarGradient),
            "sep" => Some(Self::Sep),
            "compact-at" => Some(Self::CompactAt),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SetOptions {
    key: SetKey,
    value: Option<String>,
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
    ShowConfig,
    Set(SetOptions),
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
        CliAction::ShowConfig => execute_show_config(streams),
        CliAction::Set(opts) => execute_set(opts, streams),
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
    if std::env::var_os("AGYLINE_DEBUG").as_deref() == Some(OsStr::new("1")) {
        capture_debug_payload(&env.home_dir, &buf);
    }
    let ansi = run_live_frame_ansi(&buf, &env);
    if !ansi.is_empty() {
        let _ = streams.stdout.write_all(ansi.as_bytes());
        let _ = streams.stdout.flush();
    }
    0
}

/// Best-effort capture of raw stdin bytes under `<home>/state/last_payload.json`.
/// All I/O errors are ignored so opt-in debug capture never alters stdout or exit code 0.
fn capture_debug_payload(home_dir: &Path, raw_stdin: &[u8]) {
    if let Ok(state_dir) = ensure_state_dir(home_dir) {
        let _ = atomic_write_bytes(&state_dir, "last_payload.json", raw_stdin);
    }
}

fn execute_show_config(streams: &mut CliStreams<'_>) -> u8 {
    let spawner = NoopSpawner;
    let env = RenderEnv::from_process(&spawner);
    let (config, loaded) = EffectiveConfig::resolve_with_diagnostics(&env.home_dir, &env.env_vars);
    let cfg_path = env.home_dir.join("config.toml");

    if let Some(ref err) = loaded.syntax_error {
        let _ = writeln!(
            streams.stderr,
            "warning: failed to parse {} ({err}); showing fallback default configuration",
            cfg_path.display()
        );
    }

    let file_status = if loaded.syntax_error.is_some() {
        "exists, syntax error"
    } else if loaded.config_exists {
        "exists"
    } else {
        "not found; using defaults"
    };
    let polarity = match config.polarity {
        ThemePolarity::Dark => "dark",
        ThemePolarity::Light => "light",
    };
    let sep_display = if loaded.doc.separator == config.separator {
        format!("{:?}", config.separator)
    } else {
        format!("{} ({:?})", loaded.doc.separator, config.separator)
    };

    let _ = writeln!(
        streams.stdout,
        "config: {} ({file_status})",
        cfg_path.display()
    );
    let _ = writeln!(streams.stdout, "preset: {}", config.preset);
    let _ = writeln!(streams.stdout, "palette: {}", config.palette);
    let _ = writeln!(
        streams.stdout,
        "theme: {} ({polarity})",
        config.theme_setting.as_key()
    );
    let _ = writeln!(streams.stdout, "item_style: {}", config.item_style.as_key());
    let _ = writeln!(streams.stdout, "bar_style: {}", config.bar_style.as_key());
    let _ = writeln!(streams.stdout, "bar_gradient: {}", config.bar_gradient);
    let _ = writeln!(streams.stdout, "separator: {sep_display}");
    let _ = writeln!(streams.stdout, "compact_at: {}", config.compact_at);
    let _ = writeln!(streams.stdout, "single_line: {}", config.single_line);
    let _ = writeln!(streams.stdout, "overflow: {}", config.overflow.as_key());
    let _ = streams.stdout.flush();
    0
}

enum ValidatedSetValue {
    Preset(Box<ConfigDoc>),
    Palette(String),
    Theme(ThemeSetting),
    Style(ItemStyle),
    Bar(BarStyle),
    BarGradient(bool),
    Sep(String),
    CompactAt(usize),
}

fn execute_set(opts: SetOptions, streams: &mut CliStreams<'_>) -> u8 {
    let spawner = NoopSpawner;
    let env = RenderEnv::from_process(&spawner);
    let home_dir = &env.home_dir;

    let Some(raw_val) = opts.value else {
        print_set_key_options(opts.key, home_dir, streams.stdout);
        let _ = streams.stdout.flush();
        return 0;
    };

    let validated = match validate_set_value(opts.key, &raw_val, home_dir) {
        Ok(v) => v,
        Err(msg) => {
            let _ = writeln!(streams.stderr, "error: {msg}");
            return 2;
        }
    };

    let cfg_path = home_dir.join("config.toml");
    let loaded = load_config_doc(home_dir);
    if let Some(err) = loaded.syntax_error {
        let _ = writeln!(
            streams.stderr,
            "error: refusing to overwrite broken {} ({err})",
            cfg_path.display()
        );
        return 1;
    }

    let mut doc = loaded.doc;
    let canonical_display = match validated {
        ValidatedSetValue::Preset(built) => {
            let name = built.preset.clone();
            doc.thresholds = built.thresholds;
            let _ = apply_preset(&mut doc, &name, PresetScope::All, Some(home_dir));
            name
        }
        ValidatedSetValue::Palette(name) => {
            doc.palette = name.clone();
            name
        }
        ValidatedSetValue::Theme(theme) => {
            doc.theme = theme;
            theme.as_key().to_string()
        }
        ValidatedSetValue::Style(style) => {
            doc.item_style = style;
            style.as_key().to_string()
        }
        ValidatedSetValue::Bar(bar) => {
            doc.bar_style = bar;
            bar.as_key().to_string()
        }
        ValidatedSetValue::BarGradient(enabled) => {
            doc.bar_gradient = enabled;
            enabled.to_string()
        }
        ValidatedSetValue::Sep(sep) => {
            doc.separator = sep.clone();
            sep
        }
        ValidatedSetValue::CompactAt(cols) => {
            doc.compact_at = cols;
            cols.to_string()
        }
    };

    let serialized = serialize_config_doc(&doc);
    let written_path = match atomic_write_config(home_dir, &serialized) {
        Ok(p) => p,
        Err(err) => {
            let _ = writeln!(
                streams.stderr,
                "error: failed to write {}: {err}",
                cfg_path.display()
            );
            return 1;
        }
    };

    let _ = writeln!(
        streams.stdout,
        "Set {} = {} ({})",
        opts.key.as_key(),
        canonical_display,
        written_path.display()
    );
    let _ = streams.stdout.flush();
    0
}

fn print_set_key_options(key: SetKey, home_dir: &Path, stdout: &mut dyn Write) {
    match key {
        SetKey::Preset => {
            let _ = writeln!(stdout, "Valid presets (agyline set preset <name>):");
            let builtins = builtin_presets();
            for name in discover_presets(home_dir) {
                if let Some(info) = builtins.iter().find(|p| p.name == name) {
                    let _ = writeln!(stdout, "  {:<14} {}", info.name, info.summary);
                } else {
                    let _ = writeln!(stdout, "  {:<14} custom preset", name);
                }
            }
        }
        SetKey::Palette => {
            let _ = writeln!(stdout, "Valid palettes (agyline set palette <name>):");
            for name in discover_palettes(home_dir) {
                let _ = writeln!(stdout, "  {name}");
            }
        }
        SetKey::Theme => {
            let _ = writeln!(stdout, "Valid themes (agyline set theme <value>):");
            let _ = writeln!(stdout, "  auto           follow terminal COLORFGBG");
            let _ = writeln!(stdout, "  dark           pin dark polarity");
            let _ = writeln!(stdout, "  light          pin light polarity");
        }
        SetKey::Style => {
            let _ = writeln!(
                stdout,
                "Valid container styles (agyline set style <value>):"
            );
            let _ = writeln!(stdout, "  minimal        separator-divided items");
            let _ = writeln!(stdout, "  badge          colored surface badges");
            let _ = writeln!(stdout, "  pill           Powerline rounded capsules");
        }
        SetKey::Bar => {
            let _ = writeln!(stdout, "Valid Usage Bar styles (agyline set bar <value>):");
            let _ = writeln!(stdout, "  line           ━╸─ half-cell line");
            let _ = writeln!(stdout, "  parallelogram  ▰▱ slanted blocks (alias: blocks)");
            let _ = writeln!(stdout, "  braille        ⣿⣤⣀ half-cell braille");
            let _ = writeln!(stdout, "  solid          █ full block on track background");
            let _ = writeln!(stdout, "  block          █░ shaded blocks");
            let _ = writeln!(stdout, "  ascii          #- plain ASCII");
        }
        SetKey::BarGradient => {
            let _ = writeln!(
                stdout,
                "Valid bar-gradient values (agyline set bar-gradient <bool>):"
            );
            let _ = writeln!(
                stdout,
                "  true           enable per-cell gradient (aliases: on, 1)"
            );
            let _ = writeln!(
                stdout,
                "  false          disable gradient (aliases: off, 0)"
            );
        }
        SetKey::Sep => {
            let _ = writeln!(
                stdout,
                "Valid separators (agyline set sep <preset|string>):"
            );
            for (alias, resolved) in SEMANTIC_SEPARATORS {
                let _ = writeln!(stdout, "  {:<14} {:?}", alias, resolved);
            }
            let _ = writeln!(
                stdout,
                "  <custom>       any non-empty string without control characters"
            );
        }
        SetKey::CompactAt => {
            let _ = writeln!(
                stdout,
                "Valid compact-at values (agyline set compact-at <cols>):"
            );
            let _ = writeln!(
                stdout,
                "  0..={MAX_COMPACT_AT}        columns below which multi-row collapses to 1 row (0 disables, default: {DEFAULT_COMPACT_AT})"
            );
        }
    }
}

fn validate_set_value(
    key: SetKey,
    raw_val: &str,
    home_dir: &Path,
) -> Result<ValidatedSetValue, String> {
    match key {
        SetKey::Preset => {
            let trimmed = raw_val.trim();
            let presets = discover_presets(home_dir);
            let lookup_name = presets
                .iter()
                .find(|p| p.eq_ignore_ascii_case(trimmed))
                .map(String::as_str)
                .unwrap_or(trimmed);
            let Some(built) = build_preset_doc(lookup_name, Some(home_dir)) else {
                return Err(format!(
                    "unknown preset '{raw_val}'; valid presets: {}",
                    presets.join(", ")
                ));
            };
            Ok(ValidatedSetValue::Preset(Box::new(built)))
        }
        SetKey::Palette => {
            let trimmed = raw_val.trim();
            let palettes = discover_palettes(home_dir);
            let Some(matched) = palettes.iter().find(|p| p.eq_ignore_ascii_case(trimmed)) else {
                return Err(format!(
                    "unknown palette '{raw_val}'; valid palettes: {}",
                    palettes.join(", ")
                ));
            };
            Ok(ValidatedSetValue::Palette(matched.clone()))
        }
        SetKey::Theme => ThemeSetting::from_key(raw_val)
            .map(ValidatedSetValue::Theme)
            .ok_or_else(|| format!("invalid theme '{raw_val}'; expected one of: auto, dark, light")),
        SetKey::Style => ItemStyle::from_key(raw_val)
            .map(ValidatedSetValue::Style)
            .ok_or_else(|| {
                format!("invalid style '{raw_val}'; expected one of: minimal, badge, pill")
            }),
        SetKey::Bar => BarStyle::from_key(raw_val)
            .map(ValidatedSetValue::Bar)
            .ok_or_else(|| {
                format!(
                    "invalid bar '{raw_val}'; expected one of: line, parallelogram (blocks), braille, solid, block, ascii"
                )
            }),
        SetKey::BarGradient => parse_cli_bool(raw_val)
            .map(ValidatedSetValue::BarGradient)
            .ok_or_else(|| {
                format!(
                    "invalid bar-gradient '{raw_val}'; expected boolean: true, false, on, off, 1, or 0"
                )
            }),
        SetKey::Sep => {
            if raw_val.is_empty() || raw_val.chars().any(char::is_control) {
                return Err(format!(
                    "invalid separator '{raw_val}'; expected pipe, dashed, dot, square, none, or a non-empty string without control characters"
                ));
            }
            let normalized = raw_val.trim().to_ascii_lowercase();
            let stored = SEMANTIC_SEPARATORS
                .iter()
                .find(|(alias, _)| *alias == normalized)
                .map(|(alias, _)| (*alias).to_string())
                .unwrap_or_else(|| raw_val.to_string());
            Ok(ValidatedSetValue::Sep(stored))
        }
        SetKey::CompactAt => match raw_val.trim().parse::<usize>() {
            Ok(cols) if cols <= MAX_COMPACT_AT => Ok(ValidatedSetValue::CompactAt(cols)),
            _ => Err(format!(
                "invalid compact-at '{raw_val}'; expected integer in 0..={MAX_COMPACT_AT} (0 disables auto-collapse)"
            )),
        },
    }
}

fn parse_cli_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "1" => Some(true),
        "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Atomically writes `<home_dir>/config.toml` via a private temp file and retryable replace so
/// readers never see a truncated file and failures leave no stray `.tmp` files behind.
fn atomic_write_config(home_dir: &Path, content: &str) -> io::Result<PathBuf> {
    platform::ensure_private_dir(home_dir)?;
    let dst_path = home_dir.join("config.toml");
    let seq = CLI_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Matches `install::is_config_tmp` (`.config.toml.<pid>.<nanos>.<seq>.tmp`).
    let tmp_path = home_dir.join(format!(
        ".config.toml.{}.{nanos}.{seq}.tmp",
        std::process::id()
    ));

    let write_res = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        let _ = f.sync_all();
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = platform::replace_file_with_retry(&tmp_path, &dst_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(dst_path)
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

    if let PreviewSource::Showcase = opts.source {
        execute_showcase(&opts, &env, streams);
        let _ = streams.stdout.flush();
        return 0;
    }

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
        PreviewSource::ListScenarios | PreviewSource::Showcase => 0,
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

/// Stateless gallery comparing built-in palettes, high-usage `>90%` `/compact` alert, narrow
/// `<88` single-row collapse, `badge`/`pill` containers, and semantic separator presets.
fn execute_showcase(opts: &PreviewOptions, env: &RenderEnv<'_>, streams: &mut CliStreams<'_>) {
    let Some(sc_wide) = find_scenario("showcase_wide") else {
        return;
    };
    let Some(sc_crit) = find_scenario("showcase_critical") else {
        return;
    };
    let Some(sc_compact) = find_scenario("showcase_compact") else {
        return;
    };

    let frame_wide = sc_wide.frame_for_env(&env.env_vars);
    let frame_crit = sc_crit.frame_for_env(&env.env_vars);
    let frame_compact = sc_compact.frame_for_env(&env.env_vars);
    let base_doc = ConfigDoc::default();
    let mut first_section = true;

    let mut emit_section = |title: &str, doc: &ConfigDoc, frame: &FrameData, default_w: usize| {
        if !first_section {
            let _ = writeln!(streams.stdout);
        }
        first_section = false;
        let _ = writeln!(streams.stdout, "=== {title} ===");
        // Skip AGY_* layout/theme overrides so the gallery consistently demonstrates every
        // built-in showcase variant while still honoring terminal color-depth and polarity hints.
        let cfg = EffectiveConfig::from_doc(doc, Some(&env.home_dir), &env.env_vars, false);
        write_preview_frame(
            frame,
            &cfg,
            opts.width.or(Some(default_w)),
            opts.plain,
            streams.stdout_is_terminal,
            streams.stdout,
        );
    };

    for palette in ["catppuccin", "tokyo-night", "nord", "mono"] {
        let mut doc = base_doc.clone();
        doc.palette = palette.to_string();
        emit_section(
            &format!("Palette: {palette}"),
            &doc,
            &frame_wide,
            sc_wide.default_width,
        );
    }

    emit_section(
        "High Usage (>90% /compact alert)",
        &base_doc,
        &frame_crit,
        sc_crit.default_width,
    );

    emit_section(
        &format!(
            "Narrow (<88 cols auto-compact, {} cols)",
            sc_compact.default_width
        ),
        &base_doc,
        &frame_compact,
        sc_compact.default_width,
    );

    for style in [ItemStyle::Badge, ItemStyle::Pill] {
        let mut doc = base_doc.clone();
        doc.item_style = style;
        emit_section(
            &format!("Container Style: {}", style.as_key()),
            &doc,
            &frame_wide,
            sc_wide.default_width,
        );
    }

    for (alias, _) in SEMANTIC_SEPARATORS {
        let mut doc = base_doc.clone();
        doc.separator = alias.to_string();
        emit_section(
            &format!("Separator: {alias}"),
            &doc,
            &frame_wide,
            sc_wide.default_width,
        );
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
        "-V" | "--version" | "version" => {
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
        "config" => parse_config_args(&args[2..]),
        "set" => parse_set_args(&args[2..]),
        "preview" => parse_preview_args(&args[2..]),
        "install" => parse_install_args(&args[2..]),
        "uninstall" => parse_uninstall_args(&args[2..]),
        other if other.starts_with('-') => {
            CliAction::UsageError(format!("unknown flag '{other}'"))
        }
        other => CliAction::UsageError(format!("unknown subcommand '{other}'")),
    }
}

fn parse_config_args(tail: &[OsString]) -> CliAction {
    for arg in tail {
        let s = arg.to_string_lossy();
        if s == "--" {
            break;
        }
        if s == "-h" || s == "--help" {
            return CliAction::Help(HelpTopic::Config);
        }
    }

    if let Some((first, rest)) = tail.split_first() {
        let s = first.to_string_lossy();
        match s.as_ref() {
            "--" => {
                if let Some(extra) = rest.first() {
                    return CliAction::UsageError(format!(
                        "unexpected argument '{}' for 'config'",
                        extra.to_string_lossy()
                    ));
                }
            }
            flag if flag.starts_with('-') => {
                return CliAction::UsageError(format!("unknown flag '{flag}' for 'config'"));
            }
            arg => {
                return CliAction::UsageError(format!("unexpected argument '{arg}' for 'config'"));
            }
        }
    }

    CliAction::ShowConfig
}

fn parse_set_args(tail: &[OsString]) -> CliAction {
    if tail.is_empty() {
        return CliAction::UsageError(format!(
            "missing setting key for 'set'; valid keys: {SET_KEYS_LIST}"
        ));
    }

    let first = tail[0].to_string_lossy();
    if first == "-h" || first == "--help" {
        return CliAction::Help(HelpTopic::Set);
    }

    let (raw_key, value_slice): (&OsString, &[OsString]) = if first == "--" {
        let Some((k, r)) = tail[1..].split_first() else {
            return CliAction::UsageError(format!(
                "missing setting key for 'set'; valid keys: {SET_KEYS_LIST}"
            ));
        };
        (k, r)
    } else if first.starts_with('-') {
        return CliAction::UsageError(format!("unknown flag '{first}' for 'set'"));
    } else {
        let rest = &tail[1..];
        // Allow `agyline set <key> --help` without blocking values like `-` or `-1` via `--`.
        let values = if let Some((v0, after_v0)) = rest.split_first() {
            let v0_str = v0.to_string_lossy();
            if v0_str == "-h" || v0_str == "--help" {
                return CliAction::Help(HelpTopic::Set);
            } else if v0_str == "--" {
                after_v0
            } else {
                rest
            }
        } else {
            rest
        };
        (&tail[0], values)
    };

    if value_slice.len() > 1 {
        return CliAction::UsageError(format!(
            "unexpected argument '{}' for 'set'",
            value_slice[1].to_string_lossy()
        ));
    }

    let key_str = raw_key.to_string_lossy();
    let Some(key) = SetKey::from_key(&key_str) else {
        return CliAction::UsageError(format!(
            "unknown setting '{key_str}'; valid keys: {SET_KEYS_LIST}"
        ));
    };

    let value = value_slice
        .first()
        .map(|v| v.to_string_lossy().into_owned());
    CliAction::Set(SetOptions { key, value })
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
            "--showcase" => chosen_sources.push(PreviewSource::Showcase),
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
            "conflicting flags for 'preview': pass at most one of '--scenario', '--all', '--list', '--showcase', or '--input'"
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
  config                  Show active effective settings and config.toml path
  set <key> [value]       List choices or update a setting in config.toml
  preview                 Statelessly preview scenarios, showcase gallery, or a payload
  version                 Print version
  install                 Configure agy settings.json to run agyline
  uninstall               Restore previous agy statusLine configuration
"
        }
        HelpTopic::Config => {
            "\
Usage: agyline config

Prints the resolved config.toml path, whether it exists on disk, and the active
effective settings (preset, palette, theme, item_style, bar_style, bar_gradient,
separator, compact_at, single_line, overflow).

Options:
  -h, --help              Print help
"
        }
        HelpTopic::Set => {
            "\
Usage: agyline set <key> [value]

Omit <value> to list valid choices for <key>. Pass <value> to validate and
atomically update <AGYLINE_HOME>/config.toml while preserving unrelated settings.

Keys:
  preset                  Built-in or custom preset (updates layout, style, and thresholds)
  palette                 Built-in or custom palette name
  theme                   auto, dark, or light
  style                   Container Style: minimal, badge, or pill
  bar                     Usage Bar style: line, parallelogram (blocks), braille, solid, block, ascii
  bar-gradient            Per-cell Usage Bar gradient: true/false (on/off, 1/0)
  sep                     Separator preset (pipe, dashed, dot, square, none) or custom string
  compact-at              Compact Threshold in columns (0..=500; 0 disables auto-collapse)

Options:
  -h, --help              Print help
"
        }
        HelpTopic::Preview => {
            "\
Usage: agyline preview [options]

Options:
  -s, --scenario <name>   Render a built-in preview scenario (default: normal)
      --all               Render all built-in scenarios
      --list              List built-in scenarios
      --showcase          Render multi-palette, critical, compact, container, and separator gallery
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
