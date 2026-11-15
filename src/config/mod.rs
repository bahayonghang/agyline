use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::render::palette::{
    ColorSpec, ColorToken, EffectiveColorDepth, ResolvedColor, ResolvedPalette, parse_color_spec,
    resolve_effective_color_depth, resolve_palette, toml_value_to_color_spec,
};

pub mod presets;

pub(crate) fn resolve_home_dir(
    lookup: &dyn Fn(&str) -> Option<String>,
    user_home: &Path,
) -> PathBuf {
    lookup("AGYLINE_HOME")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".gemini").join("agyline"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSetting {
    Auto,
    Dark,
    Light,
}

impl ThemeSetting {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
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

impl GlyphSet {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Nerd => "nerd",
            Self::Unicode => "unicode",
            Self::Ascii => "ascii",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "nerd" => Some(Self::Nerd),
            "unicode" => Some(Self::Unicode),
            "ascii" => Some(Self::Ascii),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    Auto,
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

impl ColorDepth {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::TrueColor => "truecolor",
            Self::Ansi256 => "256",
            Self::Ansi16 => "16",
            Self::None => "none",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "truecolor" | "24bit" => Some(Self::TrueColor),
            "256" | "ansi256" => Some(Self::Ansi256),
            "16" | "ansi16" => Some(Self::Ansi16),
            "none" | "plain" | "off" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowMode {
    Fit,
    None,
}

impl OverflowMode {
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::None => "none",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fit" => Some(Self::Fit),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetScope {
    All,
    Layout,
    Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    Quota,
    Cost,
    Vim,
    Title,
    Agent,
    Artifacts,
    Version,
    Plan,
}

impl ItemId {
    pub const ALL: [Self; 20] = [
        Self::Model,
        Self::Dir,
        Self::Git,
        Self::Context,
        Self::Cache,
        Self::Speed,
        Self::Budget,
        Self::Mode,
        Self::Subagents,
        Self::Tasks,
        Self::Queue,
        Self::Sandbox,
        Self::Quota,
        Self::Cost,
        Self::Vim,
        Self::Title,
        Self::Agent,
        Self::Artifacts,
        Self::Version,
        Self::Plan,
    ];

    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Dir => "dir",
            Self::Git => "git",
            Self::Context => "context",
            Self::Cache => "cache",
            Self::Speed => "speed",
            Self::Budget => "budget",
            Self::Mode => "mode",
            Self::Subagents => "subagents",
            Self::Tasks => "tasks",
            Self::Queue => "queue",
            Self::Sandbox => "sandbox",
            Self::Quota => "quota",
            Self::Cost => "cost",
            Self::Vim => "vim",
            Self::Title => "title",
            Self::Agent => "agent",
            Self::Artifacts => "artifacts",
            Self::Version => "version",
            Self::Plan => "plan",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "model" => Some(Self::Model),
            "dir" => Some(Self::Dir),
            "git" => Some(Self::Git),
            "context" => Some(Self::Context),
            "cache" => Some(Self::Cache),
            "speed" => Some(Self::Speed),
            "budget" => Some(Self::Budget),
            "mode" => Some(Self::Mode),
            "subagents" => Some(Self::Subagents),
            "tasks" => Some(Self::Tasks),
            "queue" => Some(Self::Queue),
            "sandbox" => Some(Self::Sandbox),
            "quota" => Some(Self::Quota),
            "cost" => Some(Self::Cost),
            "vim" => Some(Self::Vim),
            "title" => Some(Self::Title),
            "agent" => Some(Self::Agent),
            "artifacts" => Some(Self::Artifacts),
            "version" => Some(Self::Version),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }

    pub const fn default_priority(self) -> u8 {
        match self {
            Self::Model | Self::Context | Self::Mode => 1,
            Self::Git | Self::Subagents | Self::Queue | Self::Vim | Self::Agent => 2,
            Self::Dir | Self::Budget | Self::Quota | Self::Cost | Self::Tasks => 3,
            Self::Cache | Self::Sandbox | Self::Title | Self::Artifacts => 4,
            Self::Speed | Self::Version | Self::Plan => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSlot {
    pub id: ItemId,
    pub icon: Option<String>,
    pub fg: Option<String>,
    pub icon_color: Option<String>,
    pub priority: Option<u8>,
    pub show_remaining: Option<bool>,
    pub bucket: Option<String>,
    pub label: Option<String>,
    pub show_reset: Option<bool>,
    pub show_amount: Option<bool>,
    pub show_subagent: Option<bool>,
    pub show_default: Option<bool>,
    pub compact: Option<bool>,
    pub show_detail: Option<bool>,
}

impl ItemSlot {
    pub const fn new(id: ItemId) -> Self {
        Self {
            id,
            icon: None,
            fg: None,
            icon_color: None,
            priority: None,
            show_remaining: None,
            bucket: None,
            label: None,
            show_reset: None,
            show_amount: None,
            show_subagent: None,
            show_default: None,
            compact: None,
            show_detail: None,
        }
    }

    pub fn is_shorthand(&self) -> bool {
        self.icon.is_none()
            && self.fg.is_none()
            && self.icon_color.is_none()
            && self.priority.is_none()
            && self.show_remaining.is_none()
            && self.bucket.is_none()
            && self.label.is_none()
            && self.show_reset.is_none()
            && self.show_amount.is_none()
            && self.show_subagent.is_none()
            && self.show_default.is_none()
            && self.compact.is_none()
            && self.show_detail.is_none()
    }

    pub fn effective_priority(&self) -> u8 {
        self.priority
            .filter(|p| (1..=5).contains(p))
            .unwrap_or_else(|| self.id.default_priority())
    }
}

impl From<ItemId> for ItemSlot {
    fn from(id: ItemId) -> Self {
        Self::new(id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteOverrides {
    entries: BTreeMap<ColorToken, String>,
}

impl PaletteOverrides {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, token: ColorToken) -> Option<&str> {
        self.entries.get(&token).map(String::as_str)
    }

    pub fn set(&mut self, token: ColorToken, spec: impl Into<String>) {
        let s = spec.into();
        if s.trim().is_empty() {
            self.entries.remove(&token);
        } else {
            self.entries.insert(token, s);
        }
    }

    pub fn remove(&mut self, token: ColorToken) {
        self.entries.remove(&token);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (ColorToken, &str)> {
        self.entries.iter().map(|(&tok, val)| (tok, val.as_str()))
    }
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
pub struct ConfigDoc {
    pub version: u32,
    pub preset: String,
    pub palette: String,
    pub theme: ThemeSetting,
    pub glyphs: GlyphSet,
    pub color_depth: ColorDepth,
    pub separator: String,
    pub indent: String,
    pub single_line: bool,
    pub overflow: OverflowMode,
    pub git_cache_ms: u64,
    pub cache_relay_url: String,
    pub thresholds: ThresholdsConfig,
    pub palette_overrides: PaletteOverrides,
    pub lines: Vec<Vec<ItemSlot>>,
}

impl ConfigDoc {
    pub fn cometix_base() -> Self {
        Self {
            version: 1,
            preset: "cometix".to_string(),
            palette: "terminal".to_string(),
            theme: ThemeSetting::Auto,
            glyphs: GlyphSet::Nerd,
            color_depth: ColorDepth::Auto,
            separator: " | ".to_string(),
            indent: "  ".to_string(),
            single_line: false,
            overflow: OverflowMode::Fit,
            git_cache_ms: 2000,
            cache_relay_url: String::new(),
            thresholds: ThresholdsConfig::default(),
            palette_overrides: PaletteOverrides::default(),
            lines: presets::default_cometix_lines(),
        }
    }
}

impl Default for ConfigDoc {
    fn default() -> Self {
        Self::cometix_base()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSyntaxError {
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for ConfigSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(col)) => write!(f, "line {line}, column {col}: {}", self.message),
            (Some(line), None) => write!(f, "line {line}: {}", self.message),
            (None, _) => write!(f, "{}", self.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLoadResult {
    pub doc: ConfigDoc,
    pub config_exists: bool,
    pub syntax_error: Option<ConfigSyntaxError>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEnvOverride {
    pub var_name: &'static str,
    pub raw_value: String,
    pub target_key: &'static str,
}

pub fn inspect_env_overrides(env_vars: &BTreeMap<String, String>) -> Vec<ActiveEnvOverride> {
    let mut out = Vec::new();
    if let Some(v) = env_vars.get("AGY_THEME")
        && ThemeSetting::from_key(v).is_some()
    {
        out.push(ActiveEnvOverride {
            var_name: "AGY_THEME",
            raw_value: v.clone(),
            target_key: "theme",
        });
    }
    if let Some(v) = env_vars.get("AGY_STATUSLINE_LINES") {
        let t = v.trim();
        if t == "1" || t == "2" || t.eq_ignore_ascii_case("auto") {
            out.push(ActiveEnvOverride {
                var_name: "AGY_STATUSLINE_LINES",
                raw_value: v.clone(),
                target_key: "single_line",
            });
        }
    }
    if let Some(v) = env_vars.get("AGY_STATUSLINE_INDENT") {
        out.push(ActiveEnvOverride {
            var_name: "AGY_STATUSLINE_INDENT",
            raw_value: v.clone(),
            target_key: "indent",
        });
    }
    if let Some(v) = env_vars.get("AGY_SHOW_DEFAULT_MODE")
        && parse_env_bool(v).is_some()
    {
        out.push(ActiveEnvOverride {
            var_name: "AGY_SHOW_DEFAULT_MODE",
            raw_value: v.clone(),
            target_key: "mode.show_default",
        });
    }
    if let Some(v) = env_vars.get("AGY_CACHE_RELAY") {
        out.push(ActiveEnvOverride {
            var_name: "AGY_CACHE_RELAY",
            raw_value: v.clone(),
            target_key: "cache_relay.url",
        });
    }
    out
}

pub fn parse_config_doc(raw: &str) -> Result<(ConfigDoc, Vec<String>), ConfigSyntaxError> {
    parse_config_doc_with_home(raw, None)
}

pub fn parse_config_doc_with_home(
    raw: &str,
    home_dir: Option<&Path>,
) -> Result<(ConfigDoc, Vec<String>), ConfigSyntaxError> {
    let value: toml::Value =
        toml::from_str(raw).map_err(|err| toml_error_to_syntax_error(raw, &err))?;
    let Some(tbl) = value.as_table() else {
        return Err(ConfigSyntaxError {
            line: Some(1),
            column: Some(1),
            message: "expected top-level TOML table".to_string(),
        });
    };
    let mut doc = ConfigDoc::cometix_base();
    let mut warnings = Vec::new();
    parse_config_overlay_with_home(tbl, &mut doc, &mut warnings, true, home_dir);
    Ok((doc, warnings))
}

pub fn load_config_doc(home_dir: &Path) -> ConfigLoadResult {
    let config_path = home_dir.join("config.toml");
    if !config_path.exists() {
        return ConfigLoadResult {
            doc: ConfigDoc::cometix_base(),
            config_exists: false,
            syntax_error: None,
            warnings: Vec::new(),
        };
    }

    let raw = match fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(err) => {
            return ConfigLoadResult {
                doc: ConfigDoc::cometix_base(),
                config_exists: true,
                syntax_error: Some(ConfigSyntaxError {
                    line: None,
                    column: None,
                    message: format!("failed to read {}: {err}", config_path.display()),
                }),
                warnings: Vec::new(),
            };
        }
    };

    match parse_config_doc_with_home(&raw, Some(home_dir)) {
        Ok((doc, warnings)) => ConfigLoadResult {
            doc,
            config_exists: true,
            syntax_error: None,
            warnings,
        },
        Err(err) => ConfigLoadResult {
            doc: ConfigDoc::cometix_base(),
            config_exists: true,
            syntax_error: Some(err),
            warnings: Vec::new(),
        },
    }
}

pub fn serialize_config_doc(doc: &ConfigDoc) -> String {
    let mut out = String::with_capacity(512);
    let _ = writeln!(out, "version = {}", doc.version);
    let _ = writeln!(out, "preset = {}", quote_toml_str(&doc.preset));
    let _ = writeln!(out, "palette = {}", quote_toml_str(&doc.palette));
    let _ = writeln!(out, "theme = {}", quote_toml_str(doc.theme.as_key()));
    let _ = writeln!(out, "glyphs = {}", quote_toml_str(doc.glyphs.as_key()));
    let _ = writeln!(
        out,
        "color_depth = {}",
        quote_toml_str(doc.color_depth.as_key())
    );
    let _ = writeln!(out, "separator = {}", quote_toml_str(&doc.separator));
    let _ = writeln!(out, "indent = {}", quote_toml_str(&doc.indent));
    let _ = writeln!(out, "single_line = {}", doc.single_line);
    let _ = writeln!(out, "overflow = {}", quote_toml_str(doc.overflow.as_key()));

    out.push_str("\n[thresholds]\n");
    let _ = writeln!(
        out,
        "context_warn = {}",
        format_toml_num(doc.thresholds.context_warn)
    );
    let _ = writeln!(
        out,
        "context_crit = {}",
        format_toml_num(doc.thresholds.context_crit)
    );
    let _ = writeln!(out, "quota_warn = {}", doc.thresholds.quota_warn);
    let _ = writeln!(out, "quota_crit = {}", doc.thresholds.quota_crit);
    let _ = writeln!(out, "cache_warn_secs = {}", doc.thresholds.cache_warn_secs);

    out.push_str("\n[git]\n");
    let _ = writeln!(out, "cache_ms = {}", doc.git_cache_ms);

    out.push_str("\n[cache_relay]\n");
    let _ = writeln!(out, "url = {}", quote_toml_str(&doc.cache_relay_url));

    if !doc.palette_overrides.is_empty() {
        out.push_str("\n[palette_overrides]\n");
        for (tok, spec) in doc.palette_overrides.iter() {
            let _ = writeln!(out, "{} = {}", tok.as_key(), quote_toml_str(spec));
        }
    }

    for row in &doc.lines {
        out.push_str("\n[[line]]\n");
        if row.is_empty() {
            out.push_str("items = []\n");
            continue;
        }
        if row.iter().all(ItemSlot::is_shorthand) {
            let joined: Vec<String> = row
                .iter()
                .map(|slot| quote_toml_str(slot.id.as_key()))
                .collect();
            let _ = writeln!(out, "items = [{}]", joined.join(", "));
        } else {
            out.push_str("items = [\n");
            for slot in row {
                let _ = writeln!(out, "  {},", format_item_slot_toml(slot));
            }
            out.push_str("]\n");
        }
    }

    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveConfig {
    pub preset: String,
    pub palette: String,
    pub theme_setting: ThemeSetting,
    pub polarity: ThemePolarity,
    pub glyphs: GlyphSet,
    pub color_depth: ColorDepth,
    pub effective_color_depth: EffectiveColorDepth,
    pub separator: String,
    pub indent: String,
    pub single_line: bool,
    pub overflow: OverflowMode,
    pub show_default_mode: bool,
    pub force_show_default_mode: Option<bool>,
    pub git_cache_ms: u64,
    pub cache_relay_url: Option<String>,
    pub thresholds: ThresholdsConfig,
    pub palette_overrides: PaletteOverrides,
    pub resolved_palette: ResolvedPalette,
    pub lines: Vec<Vec<ItemSlot>>,
    pub config_broken: bool,
}

impl Default for EffectiveConfig {
    fn default() -> Self {
        Self::from_doc(&ConfigDoc::cometix_base(), None, &BTreeMap::new(), false)
    }
}

impl EffectiveConfig {
    pub fn resolve(home_dir: &Path, env_vars: &BTreeMap<String, String>) -> Self {
        let (cfg, _) = Self::resolve_with_diagnostics(home_dir, env_vars);
        cfg
    }

    pub fn resolve_with_diagnostics(
        home_dir: &Path,
        env_vars: &BTreeMap<String, String>,
    ) -> (Self, ConfigLoadResult) {
        let loaded = load_config_doc(home_dir);
        let mut cfg = Self::from_doc(&loaded.doc, Some(home_dir), env_vars, true);
        if loaded.syntax_error.is_some() {
            cfg.config_broken = true;
        }
        (cfg, loaded)
    }

    pub fn from_doc(
        doc: &ConfigDoc,
        home_dir: Option<&Path>,
        env_vars: &BTreeMap<String, String>,
        apply_env_overrides: bool,
    ) -> Self {
        let mut theme_setting = doc.theme;
        let mut single_line = doc.single_line;
        let mut indent = doc.indent.clone();
        let mut force_show_default_mode = None;
        let mut cache_relay_url = if doc.cache_relay_url.trim().is_empty()
            || doc.cache_relay_url.trim().eq_ignore_ascii_case("off")
        {
            None
        } else {
            Some(doc.cache_relay_url.trim().to_string())
        };

        if apply_env_overrides {
            if let Some(raw_theme) = env_vars.get("AGY_THEME")
                && let Some(parsed) = ThemeSetting::from_key(raw_theme)
            {
                theme_setting = parsed;
            }

            if let Some(raw_lines) = env_vars.get("AGY_STATUSLINE_LINES").map(|s| s.trim()) {
                if raw_lines == "1" {
                    single_line = true;
                } else if raw_lines == "2" || raw_lines.eq_ignore_ascii_case("auto") {
                    single_line = false;
                }
            }

            if let Some(raw_indent) = env_vars.get("AGY_STATUSLINE_INDENT") {
                indent = raw_indent.clone();
            }

            if let Some(raw_show_def) = env_vars.get("AGY_SHOW_DEFAULT_MODE")
                && let Some(flag) = parse_env_bool(raw_show_def)
            {
                force_show_default_mode = Some(flag);
            }

            if let Some(raw_relay) = env_vars.get("AGY_CACHE_RELAY").map(|s| s.trim()) {
                if raw_relay.is_empty() || raw_relay.eq_ignore_ascii_case("off") {
                    cache_relay_url = None;
                } else {
                    cache_relay_url = Some(raw_relay.to_string());
                }
            }
        }

        let polarity = match theme_setting {
            ThemeSetting::Light => ThemePolarity::Light,
            ThemeSetting::Dark => ThemePolarity::Dark,
            ThemeSetting::Auto => resolve_auto_polarity(env_vars),
        };

        let resolved_palette =
            resolve_palette(&doc.palette, polarity, &doc.palette_overrides, home_dir);
        let effective_color_depth = resolve_effective_color_depth(doc.color_depth, env_vars);

        let any_slot_shows_default = doc
            .lines
            .iter()
            .flatten()
            .any(|slot| slot.id == ItemId::Mode && slot.show_default == Some(true));
        let show_default_mode = force_show_default_mode.unwrap_or(any_slot_shows_default);

        Self {
            preset: doc.preset.clone(),
            palette: doc.palette.clone(),
            theme_setting,
            polarity,
            glyphs: doc.glyphs,
            color_depth: doc.color_depth,
            effective_color_depth,
            separator: doc.separator.clone(),
            indent,
            single_line,
            overflow: doc.overflow,
            show_default_mode,
            force_show_default_mode,
            git_cache_ms: doc.git_cache_ms,
            cache_relay_url,
            thresholds: doc.thresholds.clone(),
            palette_overrides: doc.palette_overrides.clone(),
            resolved_palette,
            lines: doc.lines.clone(),
            config_broken: false,
        }
    }

    pub fn resolve_span_color(&self, spec: impl Into<ColorSpec>) -> Option<ResolvedColor> {
        let resolved = self.resolved_palette.resolve_spec(spec.into());
        resolved.quantize(self.effective_color_depth)
    }
}

pub(crate) fn parse_config_overlay(
    tbl: &toml::map::Map<String, toml::Value>,
    doc: &mut ConfigDoc,
    warnings: &mut Vec<String>,
    allow_preset: bool,
) {
    parse_config_overlay_with_home(tbl, doc, warnings, allow_preset, None);
}

fn parse_config_overlay_with_home(
    tbl: &toml::map::Map<String, toml::Value>,
    doc: &mut ConfigDoc,
    warnings: &mut Vec<String>,
    allow_preset: bool,
    home_dir: Option<&Path>,
) {
    if allow_preset && let Some(preset_val) = tbl.get("preset") {
        if let Some(preset_name) = preset_val.as_str() {
            let trimmed = preset_name.trim();
            if !trimmed.is_empty()
                && !presets::apply_preset(doc, trimmed, PresetScope::All, home_dir)
            {
                warnings.push(format!(
                    "unknown preset '{trimmed}', falling back to 'cometix'"
                ));
            }
        } else {
            warnings.push("invalid 'preset': expected string".to_string());
        }
    }

    for (key, val) in tbl {
        match key.as_str() {
            "version" => {
                if let Some(i) = val.as_integer() {
                    if i == 1 {
                        doc.version = 1;
                    } else {
                        warnings.push(format!(
                            "unsupported config version '{i}', interpreting known v1 keys"
                        ));
                    }
                } else {
                    warnings.push("invalid 'version': expected integer".to_string());
                }
            }
            "preset" => {}
            "palette" => {
                if let Some(s) = val.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        doc.palette = trimmed.to_string();
                    } else {
                        warnings.push("empty 'palette' ignored".to_string());
                    }
                } else {
                    warnings.push("invalid 'palette': expected string".to_string());
                }
            }
            "theme" => match val.as_str().and_then(ThemeSetting::from_key) {
                Some(t) => doc.theme = t,
                None => warnings.push("invalid 'theme': expected auto, dark, or light".to_string()),
            },
            "glyphs" => match val.as_str().and_then(GlyphSet::from_key) {
                Some(g) => doc.glyphs = g,
                None => {
                    warnings.push("invalid 'glyphs': expected nerd, unicode, or ascii".to_string());
                }
            },
            "color_depth" => match val.as_str().and_then(ColorDepth::from_key) {
                Some(cd) => doc.color_depth = cd,
                None => warnings.push(
                    "invalid 'color_depth': expected auto, truecolor, 256, 16, or none".to_string(),
                ),
            },
            "separator" => match val.as_str() {
                Some(s) => doc.separator = s.to_string(),
                None => warnings.push("invalid 'separator': expected string".to_string()),
            },
            "indent" => match val.as_str() {
                Some(s) => doc.indent = s.to_string(),
                None => warnings.push("invalid 'indent': expected string".to_string()),
            },
            "single_line" => match val.as_bool() {
                Some(b) => doc.single_line = b,
                None => warnings.push("invalid 'single_line': expected boolean".to_string()),
            },
            "overflow" => match val.as_str().and_then(OverflowMode::from_key) {
                Some(om) => doc.overflow = om,
                None => warnings.push("invalid 'overflow': expected fit or none".to_string()),
            },
            "thresholds" => {
                if let Some(th_tbl) = val.as_table() {
                    parse_thresholds_table(th_tbl, &mut doc.thresholds, warnings);
                } else {
                    warnings.push("invalid '[thresholds]': expected table".to_string());
                }
            }
            "git" => {
                if let Some(git_tbl) = val.as_table() {
                    parse_git_table(git_tbl, doc, warnings);
                } else {
                    warnings.push("invalid '[git]': expected table".to_string());
                }
            }
            "cache_relay" => {
                if let Some(cr_tbl) = val.as_table() {
                    parse_cache_relay_table(cr_tbl, doc, warnings);
                } else {
                    warnings.push("invalid '[cache_relay]': expected table".to_string());
                }
            }
            "palette_overrides" => {
                if let Some(po_tbl) = val.as_table() {
                    parse_palette_overrides_table(po_tbl, &mut doc.palette_overrides, warnings);
                } else {
                    warnings.push("invalid '[palette_overrides]': expected table".to_string());
                }
            }
            "line" => {
                if let Some(arr) = val.as_array() {
                    let mut parsed_lines = Vec::with_capacity(arr.len());
                    for (row_idx, row_val) in arr.iter().enumerate() {
                        let Some(row_tbl) = row_val.as_table() else {
                            warnings.push(format!("invalid [[line]] #{row_idx}: expected table"));
                            continue;
                        };
                        for k in row_tbl.keys() {
                            if k != "items" {
                                warnings.push(format!("unknown key '{k}' in [[line]] #{row_idx}"));
                            }
                        }
                        if let Some(items_val) = row_tbl.get("items") {
                            if let Some(items_arr) = items_val.as_array() {
                                let mut row_slots = Vec::with_capacity(items_arr.len());
                                for item_entry in items_arr {
                                    if let Some(slot) = parse_item_slot(item_entry, warnings) {
                                        row_slots.push(slot);
                                    }
                                }
                                parsed_lines.push(row_slots);
                            } else {
                                warnings.push(format!(
                                    "invalid 'items' in [[line]] #{row_idx}: expected array"
                                ));
                            }
                        } else {
                            parsed_lines.push(Vec::new());
                        }
                    }
                    doc.lines = parsed_lines;
                } else {
                    warnings.push("invalid '[[line]]': expected array of tables".to_string());
                }
            }
            unknown => {
                warnings.push(format!("unknown top-level key '{unknown}' ignored"));
            }
        }
    }
}

fn parse_thresholds_table(
    tbl: &toml::map::Map<String, toml::Value>,
    thresholds: &mut ThresholdsConfig,
    warnings: &mut Vec<String>,
) {
    let mut cand_ctx_warn = thresholds.context_warn;
    let mut cand_ctx_crit = thresholds.context_crit;
    let mut ctx_touched = false;
    let mut ctx_valid = true;

    let mut cand_q_warn = thresholds.quota_warn;
    let mut cand_q_crit = thresholds.quota_crit;
    let mut q_touched = false;
    let mut q_valid = true;

    for (k, v) in tbl {
        match k.as_str() {
            "context_warn" => {
                ctx_touched = true;
                if let Some(num) = toml_as_f64(v).filter(|n| (0.0..=100.0).contains(n)) {
                    cand_ctx_warn = num;
                } else {
                    ctx_valid = false;
                    warnings.push("invalid 'thresholds.context_warn'".to_string());
                }
            }
            "context_crit" => {
                ctx_touched = true;
                if let Some(num) = toml_as_f64(v).filter(|n| (0.0..=100.0).contains(n)) {
                    cand_ctx_crit = num;
                } else {
                    ctx_valid = false;
                    warnings.push("invalid 'thresholds.context_crit'".to_string());
                }
            }
            "quota_warn" => {
                q_touched = true;
                if let Some(num) = v
                    .as_integer()
                    .and_then(|i| i32::try_from(i).ok())
                    .filter(|n| (0..=100).contains(n))
                {
                    cand_q_warn = num;
                } else {
                    q_valid = false;
                    warnings.push("invalid 'thresholds.quota_warn'".to_string());
                }
            }
            "quota_crit" => {
                q_touched = true;
                if let Some(num) = v
                    .as_integer()
                    .and_then(|i| i32::try_from(i).ok())
                    .filter(|n| (0..=100).contains(n))
                {
                    cand_q_crit = num;
                } else {
                    q_valid = false;
                    warnings.push("invalid 'thresholds.quota_crit'".to_string());
                }
            }
            "cache_warn_secs" => {
                if let Some(secs) = v
                    .as_integer()
                    .and_then(|i| u64::try_from(i).ok())
                    .filter(|&s| s <= 86_400)
                {
                    thresholds.cache_warn_secs = secs;
                } else {
                    warnings.push("invalid 'thresholds.cache_warn_secs'".to_string());
                }
            }
            other => warnings.push(format!("unknown key 'thresholds.{other}' ignored")),
        }
    }

    if ctx_touched {
        if ctx_valid && cand_ctx_warn < cand_ctx_crit {
            thresholds.context_warn = cand_ctx_warn;
            thresholds.context_crit = cand_ctx_crit;
        } else if ctx_valid {
            warnings.push(
                "inverted context thresholds (require context_warn < context_crit); using defaults"
                    .to_string(),
            );
        }
    }

    if q_touched {
        if q_valid && cand_q_warn > cand_q_crit {
            thresholds.quota_warn = cand_q_warn;
            thresholds.quota_crit = cand_q_crit;
        } else if q_valid {
            warnings.push(
                "inverted quota thresholds (require quota_warn > quota_crit); using defaults"
                    .to_string(),
            );
        }
    }
}

fn parse_git_table(
    tbl: &toml::map::Map<String, toml::Value>,
    doc: &mut ConfigDoc,
    warnings: &mut Vec<String>,
) {
    for (k, v) in tbl {
        match k.as_str() {
            "cache_ms" => {
                if let Some(ms) = v.as_integer().and_then(|i| u64::try_from(i).ok()) {
                    doc.git_cache_ms = ms;
                } else {
                    warnings
                        .push("invalid 'git.cache_ms': expected non-negative integer".to_string());
                }
            }
            other => warnings.push(format!("unknown key 'git.{other}' ignored")),
        }
    }
}

fn parse_cache_relay_table(
    tbl: &toml::map::Map<String, toml::Value>,
    doc: &mut ConfigDoc,
    warnings: &mut Vec<String>,
) {
    for (k, v) in tbl {
        match k.as_str() {
            "url" => match v.as_str() {
                Some(s) => doc.cache_relay_url = s.trim().to_string(),
                None => warnings.push("invalid 'cache_relay.url': expected string".to_string()),
            },
            other => warnings.push(format!("unknown key 'cache_relay.{other}' ignored")),
        }
    }
}

fn parse_palette_overrides_table(
    tbl: &toml::map::Map<String, toml::Value>,
    overrides: &mut PaletteOverrides,
    warnings: &mut Vec<String>,
) {
    for (k, v) in tbl {
        let Some(token) = ColorToken::from_key(k) else {
            warnings.push(format!("unknown palette_overrides token '{k}' ignored"));
            continue;
        };
        if let Some(normalized) = normalize_color_toml_value(v) {
            overrides.set(token, normalized);
        } else {
            warnings.push(format!("invalid color value for 'palette_overrides.{k}'"));
        }
    }
}

fn parse_item_slot(val: &toml::Value, warnings: &mut Vec<String>) -> Option<ItemSlot> {
    match val {
        toml::Value::String(s) => {
            if let Some(id) = ItemId::from_key(s) {
                Some(ItemSlot::new(id))
            } else {
                warnings.push(format!("unknown item '{s}' ignored"));
                None
            }
        }
        toml::Value::Table(tbl) => {
            let raw_id = tbl
                .get("id")
                .or_else(|| tbl.get("item"))
                .and_then(toml::Value::as_str);
            let Some(id_str) = raw_id else {
                warnings.push("item table missing string 'id' ignored".to_string());
                return None;
            };
            let Some(id) = ItemId::from_key(id_str) else {
                warnings.push(format!("unknown item '{id_str}' ignored"));
                return None;
            };
            let mut slot = ItemSlot::new(id);
            for (k, v) in tbl {
                match k.as_str() {
                    "id" | "item" => {}
                    "icon" => match v.as_str() {
                        Some(s) => slot.icon = Some(s.to_string()),
                        None => warnings.push(format!("invalid '{id_str}.icon': expected string")),
                    },
                    "fg" => match normalize_color_toml_value(v) {
                        Some(c) => slot.fg = Some(c),
                        None => warnings.push(format!("invalid '{id_str}.fg' color")),
                    },
                    "icon_color" => match normalize_color_toml_value(v) {
                        Some(c) => slot.icon_color = Some(c),
                        None => warnings.push(format!("invalid '{id_str}.icon_color' color")),
                    },
                    "priority" => {
                        if let Some(p) = v
                            .as_integer()
                            .and_then(|i| u8::try_from(i).ok())
                            .filter(|p| (1..=5).contains(p))
                        {
                            slot.priority = Some(p);
                        } else {
                            warnings.push(format!(
                                "invalid '{id_str}.priority': expected integer 1..=5"
                            ));
                        }
                    }
                    "show_remaining" => match v.as_bool() {
                        Some(b) => slot.show_remaining = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_remaining'")),
                    },
                    "bucket" => match v.as_str() {
                        Some(s) => slot.bucket = Some(s.to_string()),
                        None => warnings.push(format!("invalid '{id_str}.bucket'")),
                    },
                    "label" => match v.as_str() {
                        Some(s) => slot.label = Some(s.to_string()),
                        None => warnings.push(format!("invalid '{id_str}.label'")),
                    },
                    "show_reset" => match v.as_bool() {
                        Some(b) => slot.show_reset = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_reset'")),
                    },
                    "show_amount" => match v.as_bool() {
                        Some(b) => slot.show_amount = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_amount'")),
                    },
                    "show_subagent" => match v.as_bool() {
                        Some(b) => slot.show_subagent = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_subagent'")),
                    },
                    "show_default" => match v.as_bool() {
                        Some(b) => slot.show_default = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_default'")),
                    },
                    "compact" => match v.as_bool() {
                        Some(b) => slot.compact = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.compact'")),
                    },
                    "show_detail" => match v.as_bool() {
                        Some(b) => slot.show_detail = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_detail'")),
                    },
                    other => warnings.push(format!("unknown option '{id_str}.{other}' ignored")),
                }
            }
            Some(slot)
        }
        toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_)
        | toml::Value::Array(_) => {
            warnings
                .push("invalid item entry in [[line]].items: expected string or table".to_string());
            None
        }
    }
}

fn normalize_color_toml_value(val: &toml::Value) -> Option<String> {
    let _ = toml_value_to_color_spec(val)?;
    match val {
        toml::Value::String(s) => {
            let trimmed = s.trim();
            parse_color_spec(trimmed)?;
            Some(trimmed.to_string())
        }
        toml::Value::Integer(i) => u8::try_from(*i).ok().map(|idx| idx.to_string()),
        toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_)
        | toml::Value::Array(_)
        | toml::Value::Table(_) => None,
    }
}

fn toml_as_f64(val: &toml::Value) -> Option<f64> {
    match val {
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(_)
        | toml::Value::String(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_)
        | toml::Value::Array(_)
        | toml::Value::Table(_) => None,
    }
}

fn toml_error_to_syntax_error(raw: &str, err: &toml::de::Error) -> ConfigSyntaxError {
    let (line, column) = match err.span() {
        Some(span) => byte_offset_to_line_col(raw, span.start),
        None => (None, None),
    };
    let message = err
        .message()
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    ConfigSyntaxError {
        line,
        column,
        message,
    }
}

fn byte_offset_to_line_col(raw: &str, offset: usize) -> (Option<usize>, Option<usize>) {
    let clamped = offset.min(raw.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (idx, ch) in raw.char_indices() {
        if idx >= clamped {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (Some(line), Some(col))
}

fn format_item_slot_toml(slot: &ItemSlot) -> String {
    if slot.is_shorthand() {
        return quote_toml_str(slot.id.as_key());
    }
    let mut fields = Vec::with_capacity(8);
    fields.push(format!("id = {}", quote_toml_str(slot.id.as_key())));
    if let Some(ref b) = slot.bucket {
        fields.push(format!("bucket = {}", quote_toml_str(b)));
    }
    if let Some(ref l) = slot.label {
        fields.push(format!("label = {}", quote_toml_str(l)));
    }
    if let Some(v) = slot.show_remaining {
        fields.push(format!("show_remaining = {v}"));
    }
    if let Some(v) = slot.show_reset {
        fields.push(format!("show_reset = {v}"));
    }
    if let Some(v) = slot.show_amount {
        fields.push(format!("show_amount = {v}"));
    }
    if let Some(v) = slot.show_subagent {
        fields.push(format!("show_subagent = {v}"));
    }
    if let Some(v) = slot.show_default {
        fields.push(format!("show_default = {v}"));
    }
    if let Some(v) = slot.compact {
        fields.push(format!("compact = {v}"));
    }
    if let Some(v) = slot.show_detail {
        fields.push(format!("show_detail = {v}"));
    }
    if let Some(ref ic) = slot.icon {
        fields.push(format!("icon = {}", quote_toml_str(ic)));
    }
    if let Some(ref fg) = slot.fg {
        fields.push(format!("fg = {}", quote_toml_str(fg)));
    }
    if let Some(ref ic_col) = slot.icon_color {
        fields.push(format!("icon_color = {}", quote_toml_str(ic_col)));
    }
    if let Some(p) = slot.priority {
        fields.push(format!("priority = {p}"));
    }
    format!("{{ {} }}", fields.join(", "))
}

fn quote_toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if c.is_control() => {
                let code = c as u32;
                if code <= 0xFFFF {
                    let _ = write!(out, "\\u{code:04X}");
                } else {
                    let _ = write!(out, "\\U{code:08X}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn format_toml_num(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    let trimmed = raw.trim();
    if trimmed == "1"
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed.eq_ignore_ascii_case("on")
        || trimmed.eq_ignore_ascii_case("yes")
    {
        Some(true)
    } else if trimmed == "0"
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("no")
    {
        Some(false)
    } else {
        None
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
