//! The render seam shared by the live pipe, `preview` and the configurator: frame plus effective
//! config in, styled lines and ready-to-write ANSI/plain text out. The caller resolves the world
//! into `RenderEnv` first: nothing here probes the environment, spawns a shell or waits on `git`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::collect::{self, FrameData, Spawner};
use crate::config::EffectiveConfig;
use crate::input::parse_stdin_payload;
use crate::items::evaluate_slot_dual;
use crate::platform;
use crate::render::{RenderedFrame, emit_ansi, emit_plain, layout::layout_evaluated_rows};

/// Everything one render needs, resolved once by the caller so the printing path does no ambient
/// discovery of its own. Built per frame; the clock is read here and nowhere else on that path.
pub struct RenderEnv<'a> {
    /// `AGYLINE_HOME`, else `~/.gemini/agyline`: config, presets, palettes and per-session state.
    pub home_dir: PathBuf,
    /// Where `cache_relay.py` writes `agy_cache_<session>.dat`; the line must read the same place.
    pub temp_dir: PathBuf,
    /// agy's own conversation DBs, read for the response model; never relocated by `AGYLINE_HOME`.
    pub conversations_dir: PathBuf,
    /// Environment snapshot, so `AGY_*` overrides can be injected by tests instead of read live.
    pub env_vars: BTreeMap<String, String>,
    /// Unix epoch milliseconds for the whole frame; `collect` divides by 1000 for `now_sec`.
    pub now_ms: u64,
    /// `SystemSpawner` live, `NoopSpawner` in preview; workers are detached and never waited on.
    pub spawner: &'a dyn Spawner,
}

const RENDER_ENV_KEYS: [&str; 15] = [
    "AGYLINE_HOME",
    "AGY_THEME",
    "AGY_STATUSLINE_LINES",
    "AGY_STATUSLINE_INDENT",
    "AGY_SHOW_DEFAULT_MODE",
    "AGY_CACHE_RELAY",
    "USERPROFILE",
    "HOME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "COLORFGBG",
    "NO_COLOR",
    "COLORTERM",
    "TERM",
];

impl<'a> RenderEnv<'a> {
    /// The one place a render touches the process: targeted env keys, clock and directories.
    /// Spawns nothing and reads no config or state file; that happens later against this snapshot.
    pub fn from_process(spawner: &'a dyn Spawner) -> Self {
        let mut env_vars = BTreeMap::new();
        for &key in &RENDER_ENV_KEYS {
            if let Ok(val) = std::env::var(key) {
                env_vars.insert(key.to_string(), val);
            }
        }
        let lookup = |k: &str| env_vars.get(k).cloned();
        let user_home = platform::user_home_dir(&lookup).unwrap_or_else(|| PathBuf::from("."));
        let home_dir = crate::config::resolve_home_dir(&lookup, &user_home);
        let temp_dir = platform::relay_temp_dir(&lookup);
        // Pinned by the reference: `AGYLINE_HOME` moves agyline's own state, never agy's DBs.
        let conversations_dir = user_home
            .join(".gemini")
            .join("antigravity-cli")
            .join("conversations");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            home_dir,
            temp_dir,
            conversations_dir,
            env_vars,
            now_ms,
            spawner,
        }
    }
}

/// One live render. `rendered` is `None` and both strings empty for unusable stdin; otherwise
/// `ansi` and `plain` are complete newline-terminated output, ready to write verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOutput {
    pub rendered: Option<RenderedFrame>,
    pub ansi: String,
    pub plain: String,
}

/// Renders at the width `agy` reported in the payload; no reported width means no narrowing.
pub fn render_lines(frame: &FrameData, config: &EffectiveConfig) -> RenderedFrame {
    render_lines_with_width(frame, config, frame.payload.terminal_width)
}

/// The single renderer behind the live line, `preview` and the configurator, at a simulated width.
/// A `width_override` of `None` or `Some(0)` disables narrowing. `slot_outcomes` records the stage
/// every slot ended at, so the configurator can show what the width budget cost.
pub fn render_lines_with_width(
    frame: &FrameData,
    config: &EffectiveConfig,
    width_override: Option<usize>,
) -> RenderedFrame {
    let mut evaluated_rows = Vec::with_capacity(config.lines.len());
    for row_slots in &config.lines {
        let mut row = Vec::with_capacity(row_slots.len());
        for slot in row_slots {
            // Both variants up front: layout picks full or compact spans per slot while narrowing,
            // so item evaluation runs once per frame rather than once per width it tries.
            row.push(evaluate_slot_dual(slot, frame, config));
        }
        evaluated_rows.push(row);
    }
    let (lines, slot_outcomes) =
        layout_evaluated_rows(evaluated_rows, frame, config, width_override);
    RenderedFrame {
        lines,
        config_broken: config.config_broken,
        slot_outcomes,
    }
}

/// Live-pipe fast entry point: parses stdin, resolves config, collects only demanded sources,
/// renders, and formats ANSI without allocating an unused plain-text copy.
pub fn run_live_frame_ansi(raw_stdin: &[u8], env: &RenderEnv<'_>) -> String {
    let Some(payload) = parse_stdin_payload(raw_stdin) else {
        // `agy` shows our stdout under the conversation: a bad frame must print nothing at all.
        return String::new();
    };
    let config = EffectiveConfig::resolve(&env.home_dir, &env.env_vars);
    let frame = collect::collect_frame(payload, &config, env);
    let rendered = render_lines(&frame, &config);
    emit_ansi(&rendered, &config)
}

/// The whole live pipeline for one piped frame: parse -> config -> collect -> render -> emit.
/// Git refresh and relay touches only ever leave through `env.spawner`'s detached workers, so this
/// call never waits on a subprocess. Unusable stdin yields empty output, not an error.
pub fn run_live_frame(raw_stdin: &[u8], env: &RenderEnv<'_>) -> LiveOutput {
    let Some(payload) = parse_stdin_payload(raw_stdin) else {
        // `agy` shows our stdout under the conversation: a bad frame must print nothing at all.
        return LiveOutput {
            rendered: None,
            ansi: String::new(),
            plain: String::new(),
        };
    };
    let config = EffectiveConfig::resolve(&env.home_dir, &env.env_vars);
    let frame = collect::collect_frame(payload, &config, env);
    let rendered = render_lines(&frame, &config);
    let ansi = emit_ansi(&rendered, &config);
    let plain = emit_plain(&rendered, &config);
    LiveOutput {
        rendered: Some(rendered),
        ansi,
        plain,
    }
}
