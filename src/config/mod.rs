//! Config layer. `ConfigDoc` is the TOML document as written, round-trippable for the
//! configurator; `EffectiveConfig` is that doc plus env overrides, resolved polarity, palette and
//! color depth, ready to render. Explicit `AGY_*` vars beat config.toml, `COLORFGBG` applies only
//! when `theme = "auto"`, and a broken file still renders the default line with a warning badge.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::render::palette::{
    ColorSpec, ColorToken, EffectiveColorDepth, ResolvedColor, ResolvedPalette, parse_color_spec,
    resolve_effective_color_depth, resolve_palette, toml_value_to_color_spec,
};

pub mod presets;

/// `AGYLINE_HOME` relocates the whole config home (config.toml, presets, palettes, state); unset
/// or blank falls back to `~/.gemini/agyline`. `lookup` is injected so tests and background
/// workers can resolve a home without touching process-wide env.
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

/// The `theme` key. `auto` defers to `COLORFGBG`; `dark` and `light` pin polarity and ignore it.
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

/// Resolved light/dark. Palettes are keyed by it: an RGB palette whose polarity disagrees falls
/// back to `terminal` rather than printing unreadable colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePolarity {
    Dark,
    Light,
}

/// Icon vocabulary: `nerd` needs a patched font, `unicode` and `ascii` are the portable fallbacks.
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

/// The `color_depth` key. `auto` probes NO_COLOR / TERM / COLORTERM; the others pin the ANSI level
/// every color is quantized to, and `none` prints plain text.
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

    /// Also accepts the aliases `24bit`, `ansi256`, `ansi16`, `plain` and `off`; `None` otherwise.
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

/// `fit` narrows a row to the frame's `terminal_width` (compact first, then drop by priority);
/// `none` prints full length. With no width in the payload, `fit` also prints full length.
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

/// Row-level `Container Style` framing around rendered items: separator-divided `minimal`,
/// padded surface `badge`, or Powerline-capped `pill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemStyle {
    #[default]
    Minimal,
    Badge,
    Pill,
}

impl ItemStyle {
    pub const ALL: [Self; 3] = [Self::Minimal, Self::Badge, Self::Pill];

    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Badge => "badge",
            Self::Pill => "pill",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Self::Minimal),
            "badge" => Some(Self::Badge),
            "pill" => Some(Self::Pill),
            _ => None,
        }
    }
}

/// Default `Compact Threshold` in terminal columns; `0` disables automatic single-row collapse.
pub const DEFAULT_COMPACT_AT: usize = 88;
const MAX_COMPACT_AT: usize = 500;

/// Resolves semantic separator presets (`pipe`, `dashed`, `dot`, `square`, `none`) and strips
/// control characters from arbitrary raw strings while preserving surrounding spacing.
pub fn resolve_separator(raw: &str) -> String {
    let clean: String = raw.chars().filter(|c| !c.is_control()).collect();
    match clean.trim().to_ascii_lowercase().as_str() {
        "pipe" => " \u{2502} ".to_string(),
        "dashed" => " \u{250a} ".to_string(),
        "dot" => " \u{2022} ".to_string(),
        "square" => " \u{25aa} ".to_string(),
        "none" => "  ".to_string(),
        _ => clean,
    }
}

/// What `presets::apply_preset` replaces: rows only, style only, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetScope {
    All,
    Layout,
    Style,
}

/// The 24-item catalog. `Quota` and `QuotaGroup` are repeatable (one slot per bucket/family).
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
    Status,
    Tokens,
    Duration,
    QuotaGroup,
}

impl ItemId {
    /// Picker order, grouped Workspace, Usage, Runtime, then Session/Identity.
    pub const ALL: [Self; 24] = [
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
        Self::Status,
        Self::Tokens,
        Self::Duration,
        Self::QuotaGroup,
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
            Self::Status => "status",
            Self::Tokens => "tokens",
            Self::Duration => "duration",
            Self::QuotaGroup => "quota_group",
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
            "status" => Some(Self::Status),
            "tokens" => Some(Self::Tokens),
            "duration" => Some(Self::Duration),
            "quota_group" | "quota-group" => Some(Self::QuotaGroup),
            _ => None,
        }
    }

    /// Narrowing rank when a slot sets no valid `priority`: 1 survives longest, 5 drops first.
    pub const fn default_priority(self) -> u8 {
        match self {
            Self::Status | Self::Model | Self::Context | Self::Mode => 1,
            Self::Git
            | Self::QuotaGroup
            | Self::Subagents
            | Self::Queue
            | Self::Vim
            | Self::Agent => 2,
            Self::Dir | Self::Budget | Self::Quota | Self::Tokens | Self::Cost | Self::Tasks => 3,
            Self::Cache | Self::Duration | Self::Sandbox | Self::Title | Self::Artifacts => 4,
            Self::Speed | Self::Version | Self::Plan => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelQuotaPeriod {
    FiveHours,
    Weekly,
}

impl ModelQuotaPeriod {
    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::FiveHours => "@model/5h",
            Self::Weekly => "@model/weekly",
        }
    }
}

/// One `[[line]].items` entry. Options are `None` when the file omits them, so the item's own
/// default applies; only the options an item understands have any effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSlot {
    pub id: ItemId,
    /// Replaces the glyph-set icon; `Some("")` renders no icon at all.
    pub icon: Option<String>,
    /// Color spec text (palette token, ANSI index or `#rrggbb`), resolved and quantized at render.
    pub fg: Option<String>,
    /// Icon color spec, kept separate so the icon can differ from the body.
    pub icon_color: Option<String>,
    /// Narrowing rank 1..=5; out of range falls back to `ItemId::default_priority`.
    pub priority: Option<u8>,
    /// Context: show remaining percent instead of used percent.
    pub show_remaining: Option<bool>,
    /// Quota/budget: exact then substring bucket filter; unset picks the lowest remaining.
    /// Quota also accepts `@model/5h` and `@model/weekly`.
    pub bucket: Option<String>,
    /// Quota: label after the percentage, default `quota`.
    pub label: Option<String>,
    /// Quota/budget: append the bucket's reset countdown.
    pub show_reset: Option<bool>,
    /// Quota/budget: show a count allowance when the bucket has one.
    pub show_amount: Option<bool>,
    /// Cost/budget: append the subagent share of the USD cost.
    pub show_subagent: Option<bool>,
    /// Mode: print the default ask pill when no special mode is active.
    pub show_default: Option<bool>,
    /// Mode: always use the compact pill, even at full width.
    pub compact: Option<bool>,
    /// Subagents: expand up to two running names and models.
    pub show_detail: Option<bool>,
    /// Visual variant (`hud`, `badge`, `bar`, `counts`, `hit_rate`) for HUD-styled items.
    pub style: Option<String>,
    /// Dir: max trailing path levels to show (`1..=8`) with home tildefying.
    pub max_levels: Option<u8>,
    /// Sandbox: render `sandbox off` even when disabled.
    pub show_when_off: Option<bool>,
    /// Quota / QuotaGroup: display used percentage instead of remaining percentage.
    pub show_used: Option<bool>,
    /// Context / Quota / QuotaGroup: display proportional `Usage Bar`.
    pub show_bar: Option<bool>,
    /// `Usage Bar` style (`line`, `solid`, `block`, `ascii`).
    pub bar_style: Option<String>,
    /// `Usage Bar` column width (`4..=20`, default `10`).
    pub bar_width: Option<u8>,
    /// QuotaGroup: model family (`gemini`, `3p`, `@model`).
    pub family: Option<String>,
    /// Cache (hit_rate): also append 5-minute TTL and relay badge when active.
    pub show_ttl: Option<bool>,
    /// Tokens: append `(in↑ out↓)` breakdown after total.
    pub show_breakdown: Option<bool>,
    /// Status: append `(+N)` pending input count when non-zero.
    pub show_pending: Option<bool>,
}

impl ItemSlot {
    pub(crate) fn model_quota_period(&self) -> Option<ModelQuotaPeriod> {
        if self.id != ItemId::Quota {
            return None;
        }
        let selector = self.bucket.as_deref()?.trim();
        [ModelQuotaPeriod::FiveHours, ModelQuotaPeriod::Weekly]
            .into_iter()
            .find(|period| selector.eq_ignore_ascii_case(period.selector()))
    }

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
            style: None,
            max_levels: None,
            show_when_off: None,
            show_used: None,
            show_bar: None,
            bar_style: None,
            bar_width: None,
            family: None,
            show_ttl: None,
            show_breakdown: None,
            show_pending: None,
        }
    }

    /// True when no option is set, so serialization can write a bare `"id"` instead of a table.
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
            && self.style.is_none()
            && self.max_levels.is_none()
            && self.show_when_off.is_none()
            && self.show_used.is_none()
            && self.show_bar.is_none()
            && self.bar_style.is_none()
            && self.bar_width.is_none()
            && self.family.is_none()
            && self.show_ttl.is_none()
            && self.show_breakdown.is_none()
            && self.show_pending.is_none()
    }

    /// The slot's priority when it is inside 1..=5, else the item default; bad values are ignored,
    /// not clamped.
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

/// Per-token colors layered over the chosen palette. `BTreeMap` keeps token order stable so
/// repeated saves do not reshuffle the file.
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

    /// A blank spec removes the token: clearing a field deletes the key rather than writing "".
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

/// Coloring thresholds. The triples run in opposite directions: context counts used percent up
/// (`warn < high < crit`), quota counts remaining percent down (`warn > high > crit`).
#[derive(Debug, Clone, PartialEq)]
pub struct ThresholdsConfig {
    /// Used percent, 0..=100, where context turns warn; must stay below `context_high`.
    pub context_warn: f64,
    /// Used percent, 0..=100, where context turns high; strictly between `context_warn` and `context_crit`.
    pub context_high: f64,
    /// Used percent, 0..=100, where context turns crit.
    pub context_crit: f64,
    /// Remaining percent, 0..=100, where quota turns warn; must stay above `quota_high`.
    pub quota_warn: i32,
    /// Remaining percent, 0..=100, where quota turns high; strictly between `quota_crit` and `quota_warn`.
    pub quota_high: i32,
    /// Remaining percent, 0..=100, where quota turns crit.
    pub quota_crit: i32,
    /// Cache countdown seconds at or below which the TTL turns warn; capped at one day.
    pub cache_warn_secs: u64,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            // statusline.ps1: used percent >= 80 is crit, >= 50 is warn; 75 splits the high tier.
            context_warn: 50.0,
            context_high: 75.0,
            context_crit: 80.0,
            // statusline.ps1: remaining percent < 20 is crit, < 45 is warn; 25 splits the high tier.
            quota_warn: 45,
            quota_high: 25,
            quota_crit: 20,
            // statusline.py turns the countdown warn inside the last 60 s of the 300 s window.
            cache_warn_secs: 60,
        }
    }
}

/// The TOML document as written: every field is a config.toml key and survives a parse/serialize
/// round trip. Nothing is resolved here and `preset` is already expanded into `lines`; see
/// `EffectiveConfig` for what actually renders.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDoc {
    /// Only 1 is understood; another value warns and is still read as v1.
    pub version: u32,
    /// Name recorded for the picker; it is not re-applied at render time.
    pub preset: String,
    pub palette: String,
    pub theme: ThemeSetting,
    pub glyphs: GlyphSet,
    pub color_depth: ColorDepth,
    pub item_style: ItemStyle,
    pub separator: String,
    pub indent: String,
    pub single_line: bool,
    /// `Compact Threshold` in columns (`0` disables automatic single-row collapse).
    pub compact_at: usize,
    pub overflow: OverflowMode,
    /// Freshness window for the cached git state, in ms. The render reads that cache and never
    /// waits on git; a detached `__refresh-git` worker refills it.
    pub git_cache_ms: u64,
    /// Empty or `off` disables the relay state read, the keepalive mark and the touch.
    pub cache_relay_url: String,
    pub thresholds: ThresholdsConfig,
    pub palette_overrides: PaletteOverrides,
    /// One entry per `[[line]]`, in print order; an empty row prints nothing.
    pub lines: Vec<Vec<ItemSlot>>,
}

impl ConfigDoc {
    /// The reference preset, reproducing `ref/agy-patch/statusline.ps1` (`cometix` preset,
    /// `terminal` palette, plain skin).
    pub fn cometix_base() -> Self {
        Self::base("cometix", presets::default_cometix_lines())
    }

    fn base(preset: &str, lines: Vec<Vec<ItemSlot>>) -> Self {
        Self {
            version: 1,
            preset: preset.to_string(),
            palette: "terminal".to_string(),
            theme: ThemeSetting::Auto,
            glyphs: GlyphSet::Nerd,
            color_depth: ColorDepth::Auto,
            item_style: ItemStyle::Minimal,
            // statusline.ps1 joins items with $SEP_PIPE = " | ".
            separator: " | ".to_string(),
            // Both reference scripts default the indent to two spaces.
            indent: "  ".to_string(),
            single_line: false,
            compact_at: 0,
            overflow: OverflowMode::Fit,
            // statusline.py reuses its vcs cache while it is under 2 s old.
            git_cache_ms: 2000,
            cache_relay_url: String::new(),
            thresholds: ThresholdsConfig::default(),
            palette_overrides: PaletteOverrides::default(),
            lines,
        }
    }
}

impl Default for ConfigDoc {
    fn default() -> Self {
        presets::hud_preset_doc("default")
    }
}

/// Why config.toml could not be used. `line`/`column` are 1-based and `None` when there is no span
/// (an unreadable file); the text feeds the CLI warning and the configurator's recovery UI.
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

/// Outcome of a load that never fails: `doc` is usable even when `syntax_error` is set.
/// `config_exists` separates "no file yet" from "broken file", which decides the save/backup path.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLoadResult {
    pub doc: ConfigDoc,
    pub config_exists: bool,
    pub syntax_error: Option<ConfigSyntaxError>,
    pub warnings: Vec<String>,
}

/// One legacy `AGY_*` variable that is actually in force, so the configurator can badge the field
/// it shadows and offer a File-Only preview without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEnvOverride {
    pub var_name: &'static str,
    pub raw_value: String,
    /// Badge key, not always a literal TOML path: `mode.show_default` is a per-slot option.
    pub target_key: &'static str,
}

/// Must list exactly what `EffectiveConfig::from_doc` applies, or a badge would promise an
/// override that is ignored: unparseable `AGY_THEME` / `AGY_SHOW_DEFAULT_MODE` and a
/// `AGY_STATUSLINE_LINES` other than 1, 2 or auto are skipped. Indent and relay always win.
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

/// Parses with no config home, so custom presets and palettes cannot be discovered; prefer
/// `parse_config_doc_with_home` whenever a home is known.
pub fn parse_config_doc(raw: &str) -> Result<(ConfigDoc, Vec<String>), ConfigSyntaxError> {
    parse_config_doc_with_home(raw, None)
}

/// Tolerant overlay onto the default preset: bad values become warnings and only TOML syntax errors
/// are `Err`, so a typo never costs the user their line.
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
    let mut doc = ConfigDoc::default();
    let mut warnings = Vec::new();
    parse_config_overlay_with_home(tbl, &mut doc, &mut warnings, true, home_dir);
    Ok((doc, warnings))
}

/// Reads `<home>/config.toml`. Missing, unreadable and unparsable all return the default preset
/// with diagnostics set, never an error.
pub fn load_config_doc(home_dir: &Path) -> ConfigLoadResult {
    let config_path = home_dir.join("config.toml");
    if !config_path.exists() {
        return ConfigLoadResult {
            doc: ConfigDoc::default(),
            config_exists: false,
            syntax_error: None,
            warnings: Vec::new(),
        };
    }

    let raw = match fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(err) => {
            return ConfigLoadResult {
                doc: ConfigDoc::default(),
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
            doc: ConfigDoc::default(),
            config_exists: true,
            syntax_error: Some(err),
            warnings: Vec::new(),
        },
    }
}

/// Regenerates the whole file in a fixed key order for the configurator's live TOML drawer and
/// for saving. The thresholds, git and cache_relay tables are always written; comments and
/// unknown keys in the user's original file are not preserved.
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
    let _ = writeln!(
        out,
        "item_style = {}",
        quote_toml_str(doc.item_style.as_key())
    );
    let _ = writeln!(out, "separator = {}", quote_toml_str(&doc.separator));
    let _ = writeln!(out, "indent = {}", quote_toml_str(&doc.indent));
    let _ = writeln!(out, "single_line = {}", doc.single_line);
    let _ = writeln!(out, "compact_at = {}", doc.compact_at);
    let _ = writeln!(out, "overflow = {}", quote_toml_str(doc.overflow.as_key()));

    out.push_str("\n[thresholds]\n");
    let _ = writeln!(
        out,
        "context_warn = {}",
        format_toml_num(doc.thresholds.context_warn)
    );
    let _ = writeln!(
        out,
        "context_high = {}",
        format_toml_num(doc.thresholds.context_high)
    );
    let _ = writeln!(
        out,
        "context_crit = {}",
        format_toml_num(doc.thresholds.context_crit)
    );
    let _ = writeln!(out, "quota_warn = {}", doc.thresholds.quota_warn);
    let _ = writeln!(out, "quota_high = {}", doc.thresholds.quota_high);
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
        // An empty row must still be written, or reloading would lose a [[line]] slot.
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

/// Fully resolved render input: the doc plus env overrides, polarity, palette and color depth.
/// The live line and the configurator preview both build this, so they share one renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveConfig {
    pub preset: String,
    pub palette: String,
    pub theme_setting: ThemeSetting,
    pub polarity: ThemePolarity,
    pub glyphs: GlyphSet,
    pub color_depth: ColorDepth,
    /// What `color_depth` resolved to; the renderer quantizes every color with this.
    pub effective_color_depth: EffectiveColorDepth,
    pub item_style: ItemStyle,
    pub separator: String,
    pub indent: String,
    pub single_line: bool,
    pub compact_at: usize,
    pub overflow: OverflowMode,
    /// True when any mode slot asks for the default pill; the env force below still wins.
    pub show_default_mode: bool,
    /// `Some` only when `AGY_SHOW_DEFAULT_MODE` parsed; `None` leaves the decision to the slots.
    pub force_show_default_mode: Option<bool>,
    pub git_cache_ms: u64,
    /// `None` when disabled by the file or `AGY_CACHE_RELAY`; the relay file is then not read.
    pub cache_relay_url: Option<String>,
    pub thresholds: ThresholdsConfig,
    pub palette_overrides: PaletteOverrides,
    pub resolved_palette: ResolvedPalette,
    pub lines: Vec<Vec<ItemSlot>>,
    /// config.toml failed to parse: the renderer appends ` ! config` to the first printed row
    /// instead of failing, so a broken file still shows a line.
    pub config_broken: bool,
}

impl Default for EffectiveConfig {
    fn default() -> Self {
        Self::from_doc(&ConfigDoc::default(), None, &BTreeMap::new(), false)
    }
}

impl EffectiveConfig {
    /// Render-path entry: load `<home>/config.toml` and apply env overrides, dropping diagnostics.
    pub fn resolve(home_dir: &Path, env_vars: &BTreeMap<String, String>) -> Self {
        let (cfg, _) = Self::resolve_with_diagnostics(home_dir, env_vars);
        cfg
    }

    /// Same resolution plus the load result, so the CLI can warn on stderr and still print the
    /// fallback line.
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

    /// Pure resolution of an already-parsed doc. `apply_env_overrides = false` is the
    /// configurator's File-Only preview; `home_dir` is needed only to discover custom palettes.
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
        // Empty or "off" disables the relay read, keepalive mark and touch (AGY_CACHE_RELAY=off).
        let mut cache_relay_url = if doc.cache_relay_url.trim().is_empty()
            || doc.cache_relay_url.trim().eq_ignore_ascii_case("off")
        {
            None
        } else {
            Some(doc.cache_relay_url.trim().to_string())
        };

        // Precedence: an explicit AGY_* var beats the file; COLORFGBG is consulted only for auto.
        if apply_env_overrides {
            if let Some(raw_theme) = env_vars.get("AGY_THEME")
                && let Some(parsed) = ThemeSetting::from_key(raw_theme)
            {
                theme_setting = parsed;
            }

            // The reference only tests == "1"; "2" and "auto" explicitly restore two lines.
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

        // One mode slot asking for the default enables it line-wide; the env force still wins.
        let any_slot_shows_default = doc
            .lines
            .iter()
            .flatten()
            .any(|slot| slot.id == ItemId::Mode && slot.show_default == Some(true));
        let show_default_mode = force_show_default_mode.unwrap_or(any_slot_shows_default);

        let resolved_sep = resolve_separator(&doc.separator);
        let separator = if doc.glyphs == GlyphSet::Ascii {
            match resolved_sep.as_str() {
                " \u{2502} " | " \u{250a} " => " | ".to_string(),
                " \u{2022} " => " . ".to_string(),
                " \u{25aa} " => " - ".to_string(),
                _ => resolved_sep,
            }
        } else {
            resolved_sep
        };

        Self {
            preset: doc.preset.clone(),
            palette: doc.palette.clone(),
            theme_setting,
            polarity,
            glyphs: doc.glyphs,
            color_depth: doc.color_depth,
            effective_color_depth,
            item_style: doc.item_style,
            separator,
            indent,
            single_line,
            compact_at: doc.compact_at,
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

    /// Palette lookup then quantization; `None` at depth `none`, meaning print no color at all.
    pub fn resolve_span_color(&self, spec: impl Into<ColorSpec>) -> Option<ResolvedColor> {
        let resolved = self.resolved_palette.resolve_spec(spec.into());
        resolved.quantize(self.effective_color_depth)
    }

    /// Resolves `Container Style` against terminal capabilities: `pill` degrades to `badge`
    /// without Nerd Font glyphs, and both degrade to `minimal` below 256 colors.
    pub fn effective_item_style(&self) -> ItemStyle {
        match self.effective_color_depth {
            EffectiveColorDepth::Ansi16 | EffectiveColorDepth::None => ItemStyle::Minimal,
            EffectiveColorDepth::TrueColor | EffectiveColorDepth::Ansi256 => {
                match self.item_style {
                    ItemStyle::Pill if self.glyphs != GlyphSet::Nerd => ItemStyle::Badge,
                    ItemStyle::Minimal => ItemStyle::Minimal,
                    ItemStyle::Badge => ItemStyle::Badge,
                    ItemStyle::Pill => ItemStyle::Pill,
                }
            }
        }
    }

    /// Row separator placed between adjacent items: single space in framed container modes,
    /// otherwise the resolved `separator` string.
    pub fn effective_row_separator(&self) -> &str {
        match self.effective_item_style() {
            ItemStyle::Minimal => &self.separator,
            ItemStyle::Badge | ItemStyle::Pill => " ",
        }
    }
}

/// Overlay without a config home, used for preset files; `allow_preset = false` blocks chaining.
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
    // Expand the preset first so the explicit keys below win, whatever order the file lists them.
    if allow_preset && let Some(preset_val) = tbl.get("preset") {
        if let Some(preset_name) = preset_val.as_str() {
            let trimmed = preset_name.trim();
            if !trimmed.is_empty() {
                if let Some(built) = presets::build_preset_doc(trimmed, home_dir) {
                    doc.thresholds = built.thresholds;
                    let _ = presets::apply_preset(doc, trimmed, PresetScope::All, home_dir);
                } else {
                    warnings.push(format!(
                        "unknown preset '{trimmed}', using the default layout"
                    ));
                }
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
            // Already applied above; applying it again here would wipe the keys parsed so far.
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
            "item_style" => match val.as_str().and_then(ItemStyle::from_key) {
                Some(style) => doc.item_style = style,
                None => warnings
                    .push("invalid 'item_style': expected minimal, badge, or pill".to_string()),
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
            "compact_at" => match val.as_integer() {
                Some(v) if (0..=MAX_COMPACT_AT as i64).contains(&v) => {
                    doc.compact_at = v as usize;
                }
                _ => warnings.push(format!(
                    "invalid 'compact_at': expected integer in 0..={MAX_COMPACT_AT}"
                )),
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
                    // [[line]] replaces the preset's rows wholesale rather than merging into them.
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

fn fallback_context_high(default_high: f64, warn: f64, crit: f64) -> f64 {
    if default_high > warn && default_high < crit {
        default_high
    } else {
        (warn + crit) * 0.5
    }
}

fn fallback_quota_high(default_high: i32, crit: i32, warn: i32) -> i32 {
    if default_high > crit && default_high < warn {
        default_high
    } else if warn - crit >= 2 {
        (crit + warn) / 2
    } else {
        crit
    }
}

fn parse_thresholds_table(
    tbl: &toml::map::Map<String, toml::Value>,
    thresholds: &mut ThresholdsConfig,
    warnings: &mut Vec<String>,
) {
    // Stage each group and commit only when ordered, so a half-valid edit cannot invert bounds.
    // Omitting optional `*_high` keys preserves legacy configs by keeping default or midpoint.
    let mut cand_ctx_warn = thresholds.context_warn;
    let mut cand_ctx_high = thresholds.context_high;
    let mut cand_ctx_crit = thresholds.context_crit;
    let mut ctx_touched = false;
    let mut ctx_bounds_valid = true;
    let mut ctx_high_touched = false;
    let mut ctx_high_valid = true;

    let mut cand_q_warn = thresholds.quota_warn;
    let mut cand_q_high = thresholds.quota_high;
    let mut cand_q_crit = thresholds.quota_crit;
    let mut q_touched = false;
    let mut q_bounds_valid = true;
    let mut q_high_touched = false;
    let mut q_high_valid = true;

    for (k, v) in tbl {
        match k.as_str() {
            "context_warn" => {
                ctx_touched = true;
                if let Some(num) = toml_as_f64(v).filter(|n| (0.0..=100.0).contains(n)) {
                    cand_ctx_warn = num;
                } else {
                    ctx_bounds_valid = false;
                    warnings.push("invalid 'thresholds.context_warn'".to_string());
                }
            }
            "context_high" => {
                ctx_touched = true;
                ctx_high_touched = true;
                if let Some(num) = toml_as_f64(v).filter(|n| (0.0..=100.0).contains(n)) {
                    cand_ctx_high = num;
                } else {
                    ctx_high_valid = false;
                    warnings.push("invalid 'thresholds.context_high'".to_string());
                }
            }
            "context_crit" => {
                ctx_touched = true;
                if let Some(num) = toml_as_f64(v).filter(|n| (0.0..=100.0).contains(n)) {
                    cand_ctx_crit = num;
                } else {
                    ctx_bounds_valid = false;
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
                    q_bounds_valid = false;
                    warnings.push("invalid 'thresholds.quota_warn'".to_string());
                }
            }
            "quota_high" => {
                q_touched = true;
                q_high_touched = true;
                if let Some(num) = v
                    .as_integer()
                    .and_then(|i| i32::try_from(i).ok())
                    .filter(|n| (0..=100).contains(n))
                {
                    cand_q_high = num;
                } else {
                    q_high_valid = false;
                    warnings.push("invalid 'thresholds.quota_high'".to_string());
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
                    q_bounds_valid = false;
                    warnings.push("invalid 'thresholds.quota_crit'".to_string());
                }
            }
            "cache_warn_secs" => {
                // Bounded to one day; the reference countdown never exceeds 300 s.
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

    // Context counts up: warn < high < crit; inverted outer bounds fall back to defaults.
    if ctx_touched {
        if ctx_bounds_valid && cand_ctx_warn < cand_ctx_crit {
            let default_high = thresholds.context_high;
            thresholds.context_warn = cand_ctx_warn;
            thresholds.context_crit = cand_ctx_crit;
            if ctx_high_touched {
                if ctx_high_valid && cand_ctx_warn < cand_ctx_high && cand_ctx_high < cand_ctx_crit
                {
                    thresholds.context_high = cand_ctx_high;
                } else {
                    if ctx_high_valid {
                        warnings.push(
                            "out-of-order 'thresholds.context_high' (require context_warn < context_high < context_crit); using fallback"
                                .to_string(),
                        );
                    }
                    thresholds.context_high =
                        fallback_context_high(default_high, cand_ctx_warn, cand_ctx_crit);
                }
            } else {
                thresholds.context_high =
                    fallback_context_high(default_high, cand_ctx_warn, cand_ctx_crit);
            }
        } else if ctx_bounds_valid {
            warnings.push(
                "inverted context thresholds (require context_warn < context_crit); using defaults"
                    .to_string(),
            );
        }
    }

    // Quota counts down: warn > high > crit; inverted outer bounds fall back to defaults.
    if q_touched {
        if q_bounds_valid && cand_q_warn > cand_q_crit {
            let default_high = thresholds.quota_high;
            thresholds.quota_warn = cand_q_warn;
            thresholds.quota_crit = cand_q_crit;
            if q_high_touched {
                if q_high_valid && cand_q_crit < cand_q_high && cand_q_high < cand_q_warn {
                    thresholds.quota_high = cand_q_high;
                } else {
                    if q_high_valid {
                        warnings.push(
                            "out-of-order 'thresholds.quota_high' (require quota_crit < quota_high < quota_warn); using fallback"
                                .to_string(),
                        );
                    }
                    thresholds.quota_high =
                        fallback_quota_high(default_high, cand_q_crit, cand_q_warn);
                }
            } else {
                thresholds.quota_high = fallback_quota_high(default_high, cand_q_crit, cand_q_warn);
            }
        } else if q_bounds_valid {
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
            // "item" is an accepted alias for "id"; serialization always writes "id".
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
                    "style" => match v.as_str() {
                        Some(s) if !s.trim().is_empty() => slot.style = Some(s.trim().to_string()),
                        _ => warnings.push(format!("invalid '{id_str}.style': expected string")),
                    },
                    "max_levels" => {
                        if let Some(n) = v
                            .as_integer()
                            .and_then(|i| u8::try_from(i).ok())
                            .filter(|n| (1..=8).contains(n))
                        {
                            slot.max_levels = Some(n);
                        } else {
                            warnings.push(format!(
                                "invalid '{id_str}.max_levels': expected integer 1..=8"
                            ));
                        }
                    }
                    "show_when_off" => match v.as_bool() {
                        Some(b) => slot.show_when_off = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_when_off'")),
                    },
                    "show_used" => match v.as_bool() {
                        Some(b) => slot.show_used = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_used'")),
                    },
                    "show_bar" => match v.as_bool() {
                        Some(b) => slot.show_bar = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_bar'")),
                    },
                    "bar_style" => {
                        if let Some(bs) = v.as_str().and_then(crate::render::BarStyle::from_key) {
                            slot.bar_style = Some(bs.as_key().to_string());
                        } else {
                            warnings.push(format!(
                                "invalid '{id_str}.bar_style': expected line, solid, block, or ascii"
                            ));
                        }
                    }
                    "bar_width" => {
                        if let Some(w) = v
                            .as_integer()
                            .and_then(|i| u8::try_from(i).ok())
                            .filter(|w| (4..=20).contains(w))
                        {
                            slot.bar_width = Some(w);
                        } else {
                            warnings.push(format!(
                                "invalid '{id_str}.bar_width': expected integer 4..=20"
                            ));
                        }
                    }
                    "family" => match v.as_str() {
                        Some(s) if !s.trim().is_empty() => {
                            slot.family = Some(s.trim().to_string());
                        }
                        _ => warnings.push(format!("invalid '{id_str}.family': expected string")),
                    },
                    "show_ttl" => match v.as_bool() {
                        Some(b) => slot.show_ttl = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_ttl'")),
                    },
                    "show_breakdown" => match v.as_bool() {
                        Some(b) => slot.show_breakdown = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_breakdown'")),
                    },
                    "show_pending" => match v.as_bool() {
                        Some(b) => slot.show_pending = Some(b),
                        None => warnings.push(format!("invalid '{id_str}.show_pending'")),
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
    // Validate as a color, then store text: an integer index becomes "31" so every saved color is
    // a string the drawer and the next parse both understand.
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
        // NaN or inf would poison threshold comparisons, so only finite floats are accepted.
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
    // The TOML error message is multi-line; the banner and the stderr warning need one line.
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
    // 1-based and counted in chars, not bytes, so columns match what an editor shows.
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
    // Only set options are written, in a fixed order: emitting defaults would turn a shorthand
    // row into tables and churn the drawer on every save.
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
    if let Some(ref st) = slot.style {
        fields.push(format!("style = {}", quote_toml_str(st)));
    }
    if let Some(ml) = slot.max_levels {
        fields.push(format!("max_levels = {ml}"));
    }
    if let Some(v) = slot.show_when_off {
        fields.push(format!("show_when_off = {v}"));
    }
    if let Some(v) = slot.show_used {
        fields.push(format!("show_used = {v}"));
    }
    if let Some(v) = slot.show_bar {
        fields.push(format!("show_bar = {v}"));
    }
    if let Some(ref bs) = slot.bar_style {
        fields.push(format!("bar_style = {}", quote_toml_str(bs)));
    }
    if let Some(bw) = slot.bar_width {
        fields.push(format!("bar_width = {bw}"));
    }
    if let Some(ref fam) = slot.family {
        fields.push(format!("family = {}", quote_toml_str(fam)));
    }
    if let Some(v) = slot.show_ttl {
        fields.push(format!("show_ttl = {v}"));
    }
    if let Some(v) = slot.show_breakdown {
        fields.push(format!("show_breakdown = {v}"));
    }
    if let Some(v) = slot.show_pending {
        fields.push(format!("show_pending = {v}"));
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
    // Control characters are escaped too: a raw one would make the saved file unparsable.
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
    // Integral thresholds stay integers in the file; toml_as_f64 reads both forms back.
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

fn parse_env_bool(raw: &str) -> Option<bool> {
    // A superset of the reference's == "1" test; None leaves the file's value in force.
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
    // Both reference scripts read the last ';'-separated COLORFGBG field: >= 7 means a light
    // background. Non-numeric or absent stays dark, matching their default palette.
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
