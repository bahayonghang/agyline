use std::collections::BTreeMap;
use std::path::Path;

pub mod presets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSetting {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePolarity {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    Nerd,
    Unicode,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    Auto,
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowMode {
    Fit,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemId {
    Model,
    Dir,
    Git,
    Context,
    Cache,
    Speed,
    Budget,
    Mode,
    Subagents,
    Tasks,
    Queue,
    Sandbox,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThresholdsConfig {
    pub context_warn: f64,
    pub context_crit: f64,
    pub quota_warn: i32,
    pub quota_crit: i32,
    pub cache_warn_secs: u64,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            context_warn: 50.0,
            context_crit: 80.0,
            quota_warn: 45,
            quota_crit: 20,
            cache_warn_secs: 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveConfig {
    pub preset: String,
    pub palette: String,
    pub theme_setting: ThemeSetting,
    pub polarity: ThemePolarity,
    pub glyphs: GlyphSet,
    pub color_depth: ColorDepth,
    pub separator: String,
    pub indent: String,
    pub single_line: bool,
    pub overflow: OverflowMode,
    pub show_default_mode: bool,
    pub git_cache_ms: u64,
    pub cache_relay_url: Option<String>,
    pub thresholds: ThresholdsConfig,
    pub lines: Vec<Vec<ItemId>>,
    pub config_broken: bool,
}

impl Default for EffectiveConfig {
    fn default() -> Self {
        Self {
            preset: "cometix".to_string(),
            palette: "terminal".to_string(),
            theme_setting: ThemeSetting::Auto,
            polarity: ThemePolarity::Dark,
            glyphs: GlyphSet::Nerd,
            color_depth: ColorDepth::Auto,
            separator: " | ".to_string(),
            indent: "  ".to_string(),
            single_line: false,
            overflow: OverflowMode::Fit,
            show_default_mode: false,
            git_cache_ms: 2000,
            cache_relay_url: None,
            thresholds: ThresholdsConfig::default(),
            lines: presets::default_cometix_lines(),
            config_broken: false,
        }
    }
}

impl EffectiveConfig {
    pub fn resolve(_home_dir: &Path, env_vars: &BTreeMap<String, String>) -> Self {
        let mut cfg = Self::default();

        if let Some(raw_theme) = env_vars.get("AGY_THEME").map(|s| s.trim()) {
            if raw_theme.eq_ignore_ascii_case("light") {
                cfg.theme_setting = ThemeSetting::Light;
            } else if raw_theme.eq_ignore_ascii_case("dark") {
                cfg.theme_setting = ThemeSetting::Dark;
            } else if raw_theme.eq_ignore_ascii_case("auto") {
                cfg.theme_setting = ThemeSetting::Auto;
            }
        }

        cfg.polarity = match cfg.theme_setting {
            ThemeSetting::Light => ThemePolarity::Light,
            ThemeSetting::Dark => ThemePolarity::Dark,
            ThemeSetting::Auto => resolve_auto_polarity(env_vars),
        };

        if let Some(raw_lines) = env_vars.get("AGY_STATUSLINE_LINES").map(|s| s.trim()) {
            if raw_lines == "1" {
                cfg.single_line = true;
            } else if raw_lines == "2" || raw_lines.eq_ignore_ascii_case("auto") {
                cfg.single_line = false;
            }
        }

        if let Some(raw_indent) = env_vars.get("AGY_STATUSLINE_INDENT") {
            cfg.indent = raw_indent.clone();
        }

        if let Some(raw_show_def) = env_vars.get("AGY_SHOW_DEFAULT_MODE").map(|s| s.trim()) {
            if raw_show_def == "1"
                || raw_show_def.eq_ignore_ascii_case("true")
                || raw_show_def.eq_ignore_ascii_case("on")
            {
                cfg.show_default_mode = true;
            } else if raw_show_def == "0"
                || raw_show_def.eq_ignore_ascii_case("false")
                || raw_show_def.eq_ignore_ascii_case("off")
            {
                cfg.show_default_mode = false;
            }
        }

        if let Some(raw_relay) = env_vars.get("AGY_CACHE_RELAY").map(|s| s.trim()) {
            if raw_relay.is_empty() || raw_relay.eq_ignore_ascii_case("off") {
                cfg.cache_relay_url = None;
            } else {
                cfg.cache_relay_url = Some(raw_relay.to_string());
            }
        }

        cfg
    }
}

fn resolve_auto_polarity(env_vars: &BTreeMap<String, String>) -> ThemePolarity {
    if let Some(colorfgbg) = env_vars
        .get("COLORFGBG")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        && let Some(bg_part) = colorfgbg.rsplit(';').next()
        && !bg_part.is_empty()
        && bg_part.bytes().all(|b| b.is_ascii_digit())
        && bg_part.parse::<u32>().is_ok_and(|bg| bg >= 7)
    {
        return ThemePolarity::Light;
    }
    ThemePolarity::Dark
}
