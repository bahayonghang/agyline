use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::collect::{self, FrameData, Spawner};
use crate::config::EffectiveConfig;
use crate::input::parse_stdin_payload;
use crate::items::evaluate_slot_dual;
use crate::platform;
use crate::render::{RenderedFrame, emit_ansi, emit_plain, layout::layout_evaluated_rows};

pub struct RenderEnv<'a> {
    pub home_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub conversations_dir: PathBuf,
    pub env_vars: BTreeMap<String, String>,
    pub now_ms: u64,
    pub spawner: &'a dyn Spawner,
}

impl<'a> RenderEnv<'a> {
    pub fn from_process(spawner: &'a dyn Spawner) -> Self {
        let env_vars: BTreeMap<String, String> = std::env::vars().collect();
        let lookup = |k: &str| env_vars.get(k).cloned();
        let user_home = platform::user_home_dir(&lookup).unwrap_or_else(|| PathBuf::from("."));
        let home_dir = env_vars
            .get("AGYLINE_HOME")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join(".gemini").join("agyline"));
        let temp_dir = platform::relay_temp_dir(&lookup);
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOutput {
    pub rendered: Option<RenderedFrame>,
    pub ansi: String,
    pub plain: String,
}

pub fn render_lines(frame: &FrameData, config: &EffectiveConfig) -> RenderedFrame {
    render_lines_with_width(frame, config, frame.payload.terminal_width)
}

pub fn render_lines_with_width(
    frame: &FrameData,
    config: &EffectiveConfig,
    width_override: Option<usize>,
) -> RenderedFrame {
    let mut evaluated_rows = Vec::with_capacity(config.lines.len());
    for row_slots in &config.lines {
        let mut row = Vec::with_capacity(row_slots.len());
        for slot in row_slots {
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

pub fn run_live_frame(raw_stdin: &[u8], env: &RenderEnv<'_>) -> LiveOutput {
    let Some(payload) = parse_stdin_payload(raw_stdin) else {
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
