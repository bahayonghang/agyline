use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::text::Line;

use crate::config::{
    ActiveEnvOverride, ColorDepth, ConfigDoc, ConfigSyntaxError, EffectiveConfig, GlyphSet, ItemId,
    ItemSlot, OverflowMode, PresetScope, ThemeSetting, inspect_env_overrides, load_config_doc,
    presets::{apply_preset, discover_presets},
    serialize_config_doc,
};
use crate::install::{
    HookInspection, InstallEnv, InstallError, InstallPlan, UninstallOptions, UninstallPlan,
    apply_install, apply_uninstall, inspect_hook, plan_install, plan_uninstall,
};
use crate::items::{ItemCatalogEntry, ItemCategory, catalog};
use crate::platform;
use crate::render::{ColorToken, RenderedFrame, discover_palettes, parse_color_spec};
use crate::scenarios::{Scenario, builtin_scenarios};
use crate::state::maybe_sweep_stale_state;
use crate::statusline::render_lines_with_width;
use crate::tui::convert::rendered_frame_to_ratatui_lines;

pub const MAX_UNDO_STEPS: usize = 100;

static TUI_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ConfiguratorEnv {
    pub home_dir: PathBuf,
    pub env_vars: BTreeMap<String, String>,
    pub install_env: Result<InstallEnv, InstallError>,
    pub now_ms: u64,
}

impl ConfiguratorEnv {
    pub fn from_process() -> Self {
        let env_vars: BTreeMap<String, String> = std::env::vars().collect();
        let lookup = |k: &str| env_vars.get(k).cloned();
        let user_home = platform::user_home_dir(&lookup).unwrap_or_else(|| PathBuf::from("."));
        let home_dir = crate::config::resolve_home_dir(&lookup, &user_home);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let install_env = InstallEnv::from_process();
        Self {
            home_dir,
            env_vars,
            install_env,
            now_ms,
        }
    }

    pub fn for_sandbox(
        home_dir: PathBuf,
        env_vars: BTreeMap<String, String>,
        install_env: InstallEnv,
        now_ms: u64,
    ) -> Self {
        Self {
            home_dir,
            env_vars,
            install_env: Ok(install_env),
            now_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguratorExit {
    Saved,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiTab {
    Layout,
    StylePresets,
    ThresholdsRuntime,
    AgyHook,
}

impl TuiTab {
    pub const ALL: [Self; 4] = [
        Self::Layout,
        Self::StylePresets,
        Self::ThresholdsRuntime,
        Self::AgyHook,
    ];

    pub const fn index(self) -> usize {
        match self {
            Self::Layout => 0,
            Self::StylePresets => 1,
            Self::ThresholdsRuntime => 2,
            Self::AgyHook => 3,
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Layout => "1: Layout",
            Self::StylePresets => "2: Style & Presets",
            Self::ThresholdsRuntime => "3: Thresholds & Runtime",
            Self::AgyHook => "4: agy Hook",
        }
    }

    pub(crate) const fn title_for_width(self, width: u16) -> &'static str {
        if width >= 100 {
            return self.title();
        }
        match self {
            Self::Layout => "1: Layout",
            Self::StylePresets => "2: Style",
            Self::ThresholdsRuntime => "3: Runtime",
            Self::AgyHook => "4: agy Hook",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Layout => Self::StylePresets,
            Self::StylePresets => Self::ThresholdsRuntime,
            Self::ThresholdsRuntime => Self::AgyHook,
            Self::AgyHook => Self::Layout,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Layout => Self::AgyHook,
            Self::StylePresets => Self::Layout,
            Self::ThresholdsRuntime => Self::StylePresets,
            Self::AgyHook => Self::ThresholdsRuntime,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPane {
    ActiveRows,
    Catalog,
    Inspector,
}

impl LayoutPane {
    pub const fn next(self) -> Self {
        match self {
            Self::ActiveRows => Self::Catalog,
            Self::Catalog => Self::Inspector,
            Self::Inspector => Self::ActiveRows,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::ActiveRows => Self::Inspector,
            Self::Catalog => Self::ActiveRows,
            Self::Inspector => Self::Catalog,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StylePane {
    Presets,
    GlobalStyle,
    PaletteOverrides,
}

impl StylePane {
    pub const fn next(self) -> Self {
        match self {
            Self::Presets => Self::GlobalStyle,
            Self::GlobalStyle => Self::PaletteOverrides,
            Self::PaletteOverrides => Self::Presets,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Presets => Self::PaletteOverrides,
            Self::GlobalStyle => Self::Presets,
            Self::PaletteOverrides => Self::GlobalStyle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePane {
    Thresholds,
    Runtime,
}

impl RuntimePane {
    pub const fn toggle(self) -> Self {
        match self {
            Self::Thresholds => Self::Runtime,
            Self::Runtime => Self::Thresholds,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorField {
    Priority,
    Icon,
    Fg,
    IconColor,
    ShowRemaining,
    Bucket,
    Label,
    ShowReset,
    ShowAmount,
    ShowSubagent,
    ShowDefault,
    Compact,
    ShowDetail,
}

impl InspectorField {
    pub const fn key_label(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Icon => "icon",
            Self::Fg => "fg",
            Self::IconColor => "icon_color",
            Self::ShowRemaining => "show_remaining",
            Self::Bucket => "bucket",
            Self::Label => "label",
            Self::ShowReset => "show_reset",
            Self::ShowAmount => "show_amount",
            Self::ShowSubagent => "show_subagent",
            Self::ShowDefault => "show_default",
            Self::Compact => "compact",
            Self::ShowDetail => "show_detail",
        }
    }
}

const INSP_BASE: [InspectorField; 4] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
];
const INSP_CONTEXT: [InspectorField; 5] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::ShowRemaining,
];
const INSP_BUDGET: [InspectorField; 8] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Bucket,
    InspectorField::ShowReset,
    InspectorField::ShowAmount,
    InspectorField::ShowSubagent,
];
const INSP_MODE: [InspectorField; 6] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::ShowDefault,
    InspectorField::Compact,
];
const INSP_SUBAGENTS: [InspectorField; 5] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::ShowDetail,
];
const INSP_QUOTA: [InspectorField; 8] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Bucket,
    InspectorField::Label,
    InspectorField::ShowReset,
    InspectorField::ShowAmount,
];
const INSP_COST: [InspectorField; 5] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::ShowSubagent,
];

pub fn inspector_fields_for_item(id: ItemId) -> &'static [InspectorField] {
    match id {
        ItemId::Context => &INSP_CONTEXT,
        ItemId::Budget => &INSP_BUDGET,
        ItemId::Mode => &INSP_MODE,
        ItemId::Subagents => &INSP_SUBAGENTS,
        ItemId::Quota => &INSP_QUOTA,
        ItemId::Cost => &INSP_COST,
        ItemId::Model
        | ItemId::Dir
        | ItemId::Git
        | ItemId::Cache
        | ItemId::Speed
        | ItemId::Tasks
        | ItemId::Queue
        | ItemId::Sandbox
        | ItemId::Vim
        | ItemId::Title
        | ItemId::Agent
        | ItemId::Artifacts
        | ItemId::Version
        | ItemId::Plan => &INSP_BASE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleField {
    Palette,
    Theme,
    Glyphs,
    ColorDepth,
    Separator,
    Indent,
}

impl StyleField {
    pub const ALL: [Self; 6] = [
        Self::Palette,
        Self::Theme,
        Self::Glyphs,
        Self::ColorDepth,
        Self::Separator,
        Self::Indent,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdField {
    SingleLine,
    Overflow,
    ContextWarn,
    ContextCrit,
    QuotaWarn,
    QuotaCrit,
    CacheWarnSecs,
}

impl ThresholdField {
    pub const ALL: [Self; 7] = [
        Self::SingleLine,
        Self::Overflow,
        Self::ContextWarn,
        Self::ContextCrit,
        Self::QuotaWarn,
        Self::QuotaCrit,
        Self::CacheWarnSecs,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSubField {
    GitCacheMs,
    CacheRelayUrl,
}

impl RuntimeSubField {
    pub const ALL: [Self; 2] = [Self::GitCacheMs, Self::CacheRelayUrl];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEditTarget {
    InspectorIcon { row: usize, slot: usize },
    InspectorFg { row: usize, slot: usize },
    InspectorIconColor { row: usize, slot: usize },
    InspectorBucket { row: usize, slot: usize },
    InspectorLabel { row: usize, slot: usize },
    StyleSeparator,
    StyleIndent,
    PaletteTokenOverride(ColorToken),
    ThresholdContextWarn,
    ThresholdContextCrit,
    ThresholdQuotaWarn,
    ThresholdQuotaCrit,
    ThresholdCacheWarnSecs,
    RuntimeGitCacheMs,
    RuntimeCacheRelayUrl,
}

impl TextEditTarget {
    pub const fn prompt_label(self) -> &'static str {
        match self {
            Self::InspectorIcon { .. } => "icon",
            Self::InspectorFg { .. } => "fg",
            Self::InspectorIconColor { .. } => "icon_color",
            Self::InspectorBucket { .. } => "bucket",
            Self::InspectorLabel { .. } => "label",
            Self::StyleSeparator => "separator",
            Self::StyleIndent => "indent",
            Self::PaletteTokenOverride(tok) => tok.as_key(),
            Self::ThresholdContextWarn => "context_warn",
            Self::ThresholdContextCrit => "context_crit",
            Self::ThresholdQuotaWarn => "quota_warn",
            Self::ThresholdQuotaCrit => "quota_crit",
            Self::ThresholdCacheWarnSecs => "cache_warn_secs",
            Self::RuntimeGitCacheMs => "git.cache_ms",
            Self::RuntimeCacheRelayUrl => "cache_relay.url",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEditState {
    pub target: TextEditTarget,
    pub buffer: String,
    pub cursor_chars: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    Connect,
    Uninstall,
    Reset,
}

impl HookAction {
    pub const ALL: [Self; 3] = [Self::Connect, Self::Uninstall, Self::Reset];

    pub const fn next(self) -> Self {
        match self {
            Self::Connect => Self::Uninstall,
            Self::Uninstall => Self::Reset,
            Self::Reset => Self::Connect,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Connect => Self::Reset,
            Self::Uninstall => Self::Connect,
            Self::Reset => Self::Uninstall,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookModalPlan {
    Install(InstallPlan),
    Uninstall(UninstallPlan),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookModalState {
    pub action: HookAction,
    pub plan: HookModalPlan,
    pub scroll: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    DirtyExit {
        selected: usize,
    },
    BrokenTomlConfirm {
        exit_after_save: bool,
        selected: usize,
    },
    ExternalConflict {
        exit_after_save: bool,
        selected: usize,
    },
    PresetApply {
        preset_idx: usize,
        scope: PresetScope,
    },
    HookDiff(HookModalState),
    HelpOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiskStamp {
    pub exists: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub content_hash: u64,
}

impl DiskStamp {
    pub fn probe(path: &Path) -> Self {
        match fs::read(path) {
            Ok(bytes) => {
                let modified = fs::metadata(path).ok().and_then(|m| m.modified().ok());
                Self {
                    exists: true,
                    len: bytes.len() as u64,
                    modified,
                    content_hash: fnv1a64(&bytes),
                }
            }
            Err(_) => Self {
                exists: false,
                len: 0,
                modified: None,
                content_hash: 0,
            },
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn atomic_write_in_home(home_dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    platform::ensure_private_dir(home_dir)?;
    let dst_path = home_dir.join(file_name);
    let seq = TUI_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = home_dir.join(format!(
        ".{file_name}.{}.{nanos}.{seq}.tmp",
        std::process::id()
    ));

    let write_res = (|| -> io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        f.write_all(bytes)?;
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

pub fn ordered_catalog_entries() -> Vec<&'static ItemCatalogEntry> {
    let mut out = Vec::with_capacity(20);
    for cat in [
        ItemCategory::Workspace,
        ItemCategory::Usage,
        ItemCategory::Runtime,
        ItemCategory::Session,
    ] {
        for entry in catalog().iter().filter(|e| e.category == cat) {
            out.push(entry);
        }
    }
    out
}

pub fn preset_scope_next(scope: PresetScope) -> PresetScope {
    match scope {
        PresetScope::All => PresetScope::Layout,
        PresetScope::Layout => PresetScope::Style,
        PresetScope::Style => PresetScope::All,
    }
}

pub fn preset_scope_prev(scope: PresetScope) -> PresetScope {
    match scope {
        PresetScope::All => PresetScope::Style,
        PresetScope::Layout => PresetScope::All,
        PresetScope::Style => PresetScope::Layout,
    }
}

pub fn preset_scope_label(scope: PresetScope) -> &'static str {
    match scope {
        PresetScope::All => "All (Layout + Style)",
        PresetScope::Layout => "Layout Only",
        PresetScope::Style => "Style Only",
    }
}

pub struct ConfiguratorApp {
    pub(crate) env: ConfiguratorEnv,
    pub(crate) doc: ConfigDoc,
    pub(crate) baseline_doc: ConfigDoc,
    pub(crate) undo_stack: VecDeque<ConfigDoc>,
    pub(crate) redo_stack: Vec<ConfigDoc>,
    pub(crate) file_existed: bool,
    pub(crate) loaded_from_broken_toml: bool,
    pub(crate) syntax_error: Option<ConfigSyntaxError>,
    pub(crate) loaded_disk_stamp: DiskStamp,
    pub(crate) env_overrides: Vec<ActiveEnvOverride>,
    pub(crate) hook_inspection: Option<HookInspection>,
    pub(crate) available_presets: Vec<String>,
    pub(crate) available_palettes: Vec<String>,

    pub(crate) active_tab: TuiTab,
    pub(crate) layout_pane: LayoutPane,
    pub(crate) selected_row: usize,
    pub(crate) selected_slot: usize,
    pub(crate) catalog_idx: usize,
    pub(crate) inspector_idx: usize,

    pub(crate) style_pane: StylePane,
    pub(crate) style_preset_idx: usize,
    pub(crate) style_preset_scope: PresetScope,
    pub(crate) style_field_idx: usize,
    pub(crate) palette_override_idx: usize,

    pub(crate) runtime_pane: RuntimePane,
    pub(crate) threshold_field_idx: usize,
    pub(crate) runtime_field_idx: usize,

    pub(crate) hook_action: HookAction,

    pub(crate) active_modal: Option<ActiveModal>,
    pub(crate) text_edit: Option<TextEditState>,

    pub(crate) scenario_idx: usize,
    pub(crate) sim_width: Option<usize>,
    pub(crate) last_finite_width: usize,
    pub(crate) width_pinned_by_user: bool,
    pub(crate) effective_preview_mode: bool,
    pub(crate) toml_drawer_open: bool,
    pub(crate) term_size: (u16, u16),
    pub(crate) status_toast: Option<String>,
}

impl ConfiguratorApp {
    pub(crate) fn layout_list_offsets(&self, visible_rows: usize) -> (usize, usize) {
        let active_index = self
            .doc
            .lines
            .iter()
            .take(self.selected_row)
            .map(|row| row.len().max(1) + 1)
            .sum::<usize>()
            + self.selected_slot
            + 1;
        let entries = ordered_catalog_entries();
        let mut catalog_index = self.catalog_idx;
        let mut last_category = None;
        for entry in entries.iter().take(self.catalog_idx + 1) {
            if last_category != Some(entry.category) {
                catalog_index += 1;
                last_category = Some(entry.category);
            }
        }
        (
            (active_index + 1).saturating_sub(visible_rows),
            (catalog_index + 1).saturating_sub(visible_rows),
        )
    }

    pub fn new(env: ConfiguratorEnv) -> Self {
        maybe_sweep_stale_state(&env.home_dir, env.now_ms);

        let loaded = load_config_doc(&env.home_dir);
        let loaded_disk_stamp = DiskStamp::probe(&env.home_dir.join("config.toml"));
        let env_overrides = inspect_env_overrides(&env.env_vars);
        let hook_inspection = env.install_env.as_ref().ok().map(inspect_hook);
        let available_presets = discover_presets(&env.home_dir);
        let available_palettes = discover_palettes(&env.home_dir);

        let scenarios = builtin_scenarios();
        let default_width = scenarios.first().map(|s| s.default_width).unwrap_or(112);

        Self {
            env,
            baseline_doc: loaded.doc.clone(),
            doc: loaded.doc,
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            file_existed: loaded.config_exists,
            loaded_from_broken_toml: loaded.syntax_error.is_some(),
            syntax_error: loaded.syntax_error,
            loaded_disk_stamp,
            env_overrides,
            hook_inspection,
            available_presets,
            available_palettes,
            active_tab: TuiTab::Layout,
            layout_pane: LayoutPane::ActiveRows,
            selected_row: 0,
            selected_slot: 0,
            catalog_idx: 0,
            inspector_idx: 0,
            style_pane: StylePane::Presets,
            style_preset_idx: 0,
            style_preset_scope: PresetScope::All,
            style_field_idx: 0,
            palette_override_idx: 0,
            runtime_pane: RuntimePane::Thresholds,
            threshold_field_idx: 0,
            runtime_field_idx: 0,
            hook_action: HookAction::Connect,
            active_modal: None,
            text_edit: None,
            scenario_idx: 0,
            sim_width: Some(default_width),
            last_finite_width: default_width,
            width_pinned_by_user: false,
            effective_preview_mode: true,
            toml_drawer_open: false,
            term_size: (100, 28),
            status_toast: None,
        }
    }

    pub fn doc(&self) -> &ConfigDoc {
        &self.doc
    }

    pub fn is_dirty(&self) -> bool {
        self.doc != self.baseline_doc
    }

    pub fn has_active_modal(&self) -> bool {
        self.active_modal.is_some()
    }

    pub fn active_tab(&self) -> TuiTab {
        self.active_tab
    }

    pub fn active_scenario(&self) -> &'static Scenario {
        let list = builtin_scenarios();
        &list[self.scenario_idx % list.len()]
    }

    pub fn simulated_width(&self) -> Option<usize> {
        self.sim_width
    }

    pub fn is_effective_preview_mode(&self) -> bool {
        self.effective_preview_mode
    }

    pub fn is_toml_drawer_open(&self) -> bool {
        self.toml_drawer_open
    }

    pub fn is_editing_text(&self) -> bool {
        self.text_edit.is_some()
    }

    pub fn loaded_from_broken_toml(&self) -> bool {
        self.loaded_from_broken_toml
    }

    pub fn syntax_error(&self) -> Option<&ConfigSyntaxError> {
        self.syntax_error.as_ref()
    }

    pub fn effective_config_for_preview(&self) -> EffectiveConfig {
        let mut cfg = EffectiveConfig::from_doc(
            &self.doc,
            Some(&self.env.home_dir),
            &self.env.env_vars,
            self.effective_preview_mode,
        );
        if self.loaded_from_broken_toml {
            cfg.config_broken = true;
        }
        cfg
    }

    pub fn preview_rendered_frame(&self) -> (RenderedFrame, EffectiveConfig) {
        let cfg = self.effective_config_for_preview();
        let sc = self.active_scenario();
        let frame = if self.effective_preview_mode {
            sc.frame_for_env(&self.env.env_vars)
        } else {
            sc.frame.clone()
        };
        let rendered = render_lines_with_width(&frame, &cfg, self.sim_width);
        (rendered, cfg)
    }

    pub fn preview_lines(&self) -> Vec<Line<'static>> {
        let (rendered, cfg) = self.preview_rendered_frame();
        rendered_frame_to_ratatui_lines(&rendered, &cfg)
    }

    pub(crate) fn mutate_doc<F: FnOnce(&mut ConfigDoc)>(&mut self, f: F) {
        let snapshot = self.doc.clone();
        f(&mut self.doc);
        if self.doc != snapshot {
            if self.undo_stack.len() >= MAX_UNDO_STEPS {
                let _ = self.undo_stack.pop_front();
            }
            self.undo_stack.push_back(snapshot);
            self.redo_stack.clear();
            self.status_toast = None;
        }
        self.clamp_layout_selection();
    }

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop_back() {
            let current = std::mem::replace(&mut self.doc, prev);
            self.redo_stack.push(current);
            self.clamp_layout_selection();
            self.status_toast = Some("Undid last change".to_string());
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            let current = std::mem::replace(&mut self.doc, next);
            if self.undo_stack.len() >= MAX_UNDO_STEPS {
                let _ = self.undo_stack.pop_front();
            }
            self.undo_stack.push_back(current);
            self.clamp_layout_selection();
            self.status_toast = Some("Redid change".to_string());
        }
    }

    pub(crate) fn ensure_two_rows(doc: &mut ConfigDoc) {
        while doc.lines.len() < 2 {
            doc.lines.push(Vec::new());
        }
    }

    pub(crate) fn clamp_layout_selection(&mut self) {
        self.selected_row = self.selected_row.min(1);
        let row_len = self.doc.lines.get(self.selected_row).map_or(0, Vec::len);
        if row_len == 0 {
            self.selected_slot = 0;
            self.inspector_idx = 0;
        } else {
            self.selected_slot = self.selected_slot.min(row_len - 1);
            let slot_id = self.doc.lines[self.selected_row][self.selected_slot].id;
            let fields = inspector_fields_for_item(slot_id);
            self.inspector_idx = self.inspector_idx.min(fields.len().saturating_sub(1));
        }
    }

    pub(crate) fn selected_item_slot(&self) -> Option<&ItemSlot> {
        self.doc
            .lines
            .get(self.selected_row)
            .and_then(|r| r.get(self.selected_slot))
    }

    pub(crate) fn start_text_edit(&mut self, target: TextEditTarget, initial: String) {
        let cursor_chars = initial.chars().count();
        self.text_edit = Some(TextEditState {
            target,
            buffer: initial,
            cursor_chars,
            error: None,
        });
    }

    pub(crate) fn commit_text_edit(&mut self) {
        let Some(edit) = self.text_edit.clone() else {
            return;
        };
        let raw = edit.buffer;
        let trimmed = raw.trim().to_string();

        match edit.target {
            TextEditTarget::InspectorIcon { row, slot } => {
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.icon = Some(raw);
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorFg { row, slot } => {
                if !trimmed.is_empty() && parse_color_spec(&trimmed).is_none() {
                    if let Some(ref mut st) = self.text_edit {
                        st.error =
                            Some("Invalid color (#RRGGBB, 0..255, or token/ANSI name)".to_string());
                    }
                    return;
                }
                let next = (!trimmed.is_empty()).then_some(trimmed);
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.fg = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorIconColor { row, slot } => {
                if !trimmed.is_empty() && parse_color_spec(&trimmed).is_none() {
                    if let Some(ref mut st) = self.text_edit {
                        st.error =
                            Some("Invalid color (#RRGGBB, 0..255, or token/ANSI name)".to_string());
                    }
                    return;
                }
                let next = (!trimmed.is_empty()).then_some(trimmed);
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.icon_color = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorBucket { row, slot } => {
                let next = (!trimmed.is_empty()).then_some(trimmed);
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.bucket = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorLabel { row, slot } => {
                let next = (!trimmed.is_empty()).then_some(trimmed);
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.label = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::StyleSeparator => {
                self.mutate_doc(|doc| {
                    doc.separator = raw;
                });
                self.text_edit = None;
            }
            TextEditTarget::StyleIndent => {
                self.mutate_doc(|doc| {
                    doc.indent = raw;
                });
                self.text_edit = None;
            }
            TextEditTarget::PaletteTokenOverride(tok) => {
                if !trimmed.is_empty() && parse_color_spec(&trimmed).is_none() {
                    if let Some(ref mut st) = self.text_edit {
                        st.error =
                            Some("Invalid color (#RRGGBB, 0..255, or token/ANSI name)".to_string());
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    if trimmed.is_empty() {
                        doc.palette_overrides.remove(tok);
                    } else {
                        doc.palette_overrides.set(tok, trimmed);
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdContextWarn => {
                let max_allowed = (self.doc.thresholds.context_crit - 1.0).max(0.0);
                let Ok(v) = trimmed.parse::<f64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected number 0..99 (< context_crit)".to_string());
                    }
                    return;
                };
                if !v.is_finite() || v < 0.0 || v >= self.doc.thresholds.context_crit {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!(
                            "Require 0 <= context_warn < {} (max {max_allowed})",
                            self.doc.thresholds.context_crit
                        ));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.context_warn = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdContextCrit => {
                let Ok(v) = trimmed.parse::<f64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected number 1..100 (> context_warn)".to_string());
                    }
                    return;
                };
                if !v.is_finite() || v > 100.0 || v <= self.doc.thresholds.context_warn {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!(
                            "Require {} < context_crit <= 100",
                            self.doc.thresholds.context_warn
                        ));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.context_crit = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdQuotaWarn => {
                let Ok(v) = trimmed.parse::<i32>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected integer 1..100 (> quota_crit)".to_string());
                    }
                    return;
                };
                if v > 100 || v <= self.doc.thresholds.quota_crit {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!(
                            "Require {} < quota_warn <= 100",
                            self.doc.thresholds.quota_crit
                        ));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.quota_warn = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdQuotaCrit => {
                let Ok(v) = trimmed.parse::<i32>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected integer 0..99 (< quota_warn)".to_string());
                    }
                    return;
                };
                if v < 0 || v >= self.doc.thresholds.quota_warn {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!(
                            "Require 0 <= quota_crit < {}",
                            self.doc.thresholds.quota_warn
                        ));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.quota_crit = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdCacheWarnSecs => {
                let Ok(v) = trimmed.parse::<u64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected integer 0..=86400".to_string());
                    }
                    return;
                };
                if v > 86_400 {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected integer 0..=86400".to_string());
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.cache_warn_secs = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::RuntimeGitCacheMs => {
                let Ok(v) = trimmed.parse::<u64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some("Expected non-negative integer milliseconds".to_string());
                    }
                    return;
                };
                self.mutate_doc(|doc| {
                    doc.git_cache_ms = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::RuntimeCacheRelayUrl => {
                self.mutate_doc(|doc| {
                    doc.cache_relay_url = trimmed;
                });
                self.text_edit = None;
            }
        }
    }

    pub(crate) fn apply_preset_with_undo(&mut self, name: &str, scope: PresetScope) {
        let home = self.env.home_dir.clone();
        self.mutate_doc(|doc| {
            let _ = apply_preset(doc, name, scope, Some(&home));
        });
    }

    pub(crate) fn cycle_palette(&mut self, forward: bool) {
        if self.available_palettes.is_empty() {
            return;
        }
        let cur_idx = self
            .available_palettes
            .iter()
            .position(|p| p.eq_ignore_ascii_case(&self.doc.palette))
            .unwrap_or(0);
        let len = self.available_palettes.len();
        let next_idx = if forward {
            (cur_idx + 1) % len
        } else if cur_idx == 0 {
            len - 1
        } else {
            cur_idx - 1
        };
        let next_pal = self.available_palettes[next_idx].clone();
        self.mutate_doc(|doc| {
            doc.palette = next_pal;
        });
    }

    pub(crate) fn cycle_theme(&mut self, forward: bool) {
        let next = match (self.doc.theme, forward) {
            (ThemeSetting::Auto, true) => ThemeSetting::Dark,
            (ThemeSetting::Dark, true) => ThemeSetting::Light,
            (ThemeSetting::Light, true) => ThemeSetting::Auto,
            (ThemeSetting::Auto, false) => ThemeSetting::Light,
            (ThemeSetting::Dark, false) => ThemeSetting::Auto,
            (ThemeSetting::Light, false) => ThemeSetting::Dark,
        };
        self.mutate_doc(|doc| {
            doc.theme = next;
        });
    }

    pub(crate) fn cycle_glyphs(&mut self, forward: bool) {
        let next = match (self.doc.glyphs, forward) {
            (GlyphSet::Nerd, true) => GlyphSet::Unicode,
            (GlyphSet::Unicode, true) => GlyphSet::Ascii,
            (GlyphSet::Ascii, true) => GlyphSet::Nerd,
            (GlyphSet::Nerd, false) => GlyphSet::Ascii,
            (GlyphSet::Unicode, false) => GlyphSet::Nerd,
            (GlyphSet::Ascii, false) => GlyphSet::Unicode,
        };
        self.mutate_doc(|doc| {
            doc.glyphs = next;
        });
    }

    pub(crate) fn cycle_color_depth(&mut self, forward: bool) {
        let next = match (self.doc.color_depth, forward) {
            (ColorDepth::Auto, true) => ColorDepth::TrueColor,
            (ColorDepth::TrueColor, true) => ColorDepth::Ansi256,
            (ColorDepth::Ansi256, true) => ColorDepth::Ansi16,
            (ColorDepth::Ansi16, true) => ColorDepth::None,
            (ColorDepth::None, true) => ColorDepth::Auto,
            (ColorDepth::Auto, false) => ColorDepth::None,
            (ColorDepth::TrueColor, false) => ColorDepth::Auto,
            (ColorDepth::Ansi256, false) => ColorDepth::TrueColor,
            (ColorDepth::Ansi16, false) => ColorDepth::Ansi256,
            (ColorDepth::None, false) => ColorDepth::Ansi16,
        };
        self.mutate_doc(|doc| {
            doc.color_depth = next;
        });
    }

    pub(crate) fn toggle_overflow(&mut self) {
        let next = match self.doc.overflow {
            OverflowMode::Fit => OverflowMode::None,
            OverflowMode::None => OverflowMode::Fit,
        };
        self.mutate_doc(|doc| {
            doc.overflow = next;
        });
    }

    pub(crate) fn cycle_scenario(&mut self, forward: bool) {
        let list = builtin_scenarios();
        if list.is_empty() {
            return;
        }
        if forward {
            self.scenario_idx = (self.scenario_idx + 1) % list.len();
        } else if self.scenario_idx == 0 {
            self.scenario_idx = list.len() - 1;
        } else {
            self.scenario_idx -= 1;
        }
        if !self.width_pinned_by_user {
            let w = list[self.scenario_idx].default_width;
            self.sim_width = Some(w);
            self.last_finite_width = w;
        }
    }

    pub(crate) fn adjust_sim_width(&mut self, delta: isize) {
        self.width_pinned_by_user = true;
        let base = self.sim_width.unwrap_or(self.last_finite_width) as isize;
        let next = (base + delta).clamp(40, 200) as usize;
        self.sim_width = Some(next);
        self.last_finite_width = next;
    }

    pub(crate) fn toggle_unbounded_width(&mut self) {
        self.width_pinned_by_user = true;
        match self.sim_width {
            Some(w) => {
                self.last_finite_width = w;
                self.sim_width = None;
            }
            None => {
                self.sim_width = Some(self.last_finite_width.clamp(40, 200));
            }
        }
    }

    pub(crate) fn config_path(&self) -> PathBuf {
        self.env.home_dir.join("config.toml")
    }

    pub(crate) fn request_save(&mut self, exit_after_save: bool) -> Option<ConfiguratorExit> {
        let cfg_path = self.config_path();
        let current_stamp = DiskStamp::probe(&cfg_path);
        if current_stamp != self.loaded_disk_stamp {
            self.active_modal = Some(ActiveModal::ExternalConflict {
                exit_after_save,
                selected: 0,
            });
            return None;
        }

        if self.loaded_from_broken_toml {
            self.active_modal = Some(ActiveModal::BrokenTomlConfirm {
                exit_after_save,
                selected: 0,
            });
            return None;
        }

        if !self.file_existed && !self.is_dirty() {
            self.active_modal = None;
            self.status_toast =
                Some("Default configuration active (no changes to write)".to_string());
            if exit_after_save {
                return Some(ConfiguratorExit::Cancelled);
            }
            return None;
        }

        self.perform_write_config(false, exit_after_save)
    }

    pub(crate) fn perform_write_config(
        &mut self,
        write_backup: bool,
        exit_after_save: bool,
    ) -> Option<ConfiguratorExit> {
        let cfg_path = self.config_path();
        if write_backup && cfg_path.exists() {
            match fs::read(&cfg_path) {
                Ok(raw_bytes) => {
                    if let Err(err) =
                        atomic_write_in_home(&self.env.home_dir, "config.toml.bak", &raw_bytes)
                    {
                        self.active_modal = None;
                        self.status_toast = Some(format!("Backup error: {err}"));
                        return None;
                    }
                }
                Err(err) => {
                    self.active_modal = None;
                    self.status_toast = Some(format!("Backup read error: {err}"));
                    return None;
                }
            }
        }

        let serialized = serialize_config_doc(&self.doc);
        match atomic_write_in_home(&self.env.home_dir, "config.toml", serialized.as_bytes()) {
            Ok(saved_path) => {
                self.baseline_doc = self.doc.clone();
                self.file_existed = true;
                self.loaded_from_broken_toml = false;
                self.syntax_error = None;
                self.loaded_disk_stamp = DiskStamp::probe(&saved_path);
                self.active_modal = None;
                self.status_toast = Some(if write_backup {
                    "Backed up config.toml.bak and saved config.toml".to_string()
                } else {
                    "Saved config.toml".to_string()
                });
                if exit_after_save {
                    Some(ConfiguratorExit::Saved)
                } else {
                    None
                }
            }
            Err(err) => {
                self.active_modal = None;
                self.status_toast = Some(format!("Save error: {err}"));
                None
            }
        }
    }

    pub(crate) fn reload_from_disk(&mut self) {
        let prev = self.doc.clone();
        if self.undo_stack.len() >= MAX_UNDO_STEPS {
            let _ = self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(prev);
        self.redo_stack.clear();

        let loaded = load_config_doc(&self.env.home_dir);
        self.baseline_doc = loaded.doc.clone();
        self.doc = loaded.doc;
        self.file_existed = loaded.config_exists;
        self.loaded_from_broken_toml = loaded.syntax_error.is_some();
        self.syntax_error = loaded.syntax_error;
        self.loaded_disk_stamp = DiskStamp::probe(&self.config_path());
        self.active_modal = None;
        self.clamp_layout_selection();
        self.status_toast = Some("Reloaded config.toml from disk".to_string());
    }

    pub(crate) fn open_hook_modal(&mut self, action: HookAction) {
        let plan = match &self.env.install_env {
            Err(err) => HookModalPlan::Error(err.to_string()),
            Ok(ienv) => match action {
                HookAction::Connect => match plan_install(ienv) {
                    Ok(p) => HookModalPlan::Install(p),
                    Err(err) => HookModalPlan::Error(err.to_string()),
                },
                HookAction::Uninstall => match plan_uninstall(ienv, UninstallOptions::default()) {
                    Ok(p) => HookModalPlan::Uninstall(p),
                    Err(err) => HookModalPlan::Error(err.to_string()),
                },
                HookAction::Reset => {
                    let opts = UninstallOptions {
                        reset: true,
                        ..UninstallOptions::default()
                    };
                    match plan_uninstall(ienv, opts) {
                        Ok(p) => HookModalPlan::Uninstall(p),
                        Err(err) => HookModalPlan::Error(err.to_string()),
                    }
                }
            },
        };
        self.active_modal = Some(ActiveModal::HookDiff(HookModalState {
            action,
            plan,
            scroll: 0,
        }));
    }

    pub(crate) fn apply_active_hook_modal(&mut self) {
        let Some(ActiveModal::HookDiff(modal_state)) = self.active_modal.take() else {
            return;
        };
        let Ok(ref ienv) = self.env.install_env else {
            return;
        };

        let toast = match &modal_state.plan {
            HookModalPlan::Install(plan) => match plan {
                InstallPlan::AlreadyInstalled { .. } => {
                    "agyline is already connected in settings.json".to_string()
                }
                InstallPlan::WouldChange { .. } => match apply_install(ienv, plan) {
                    Ok(_) => "Updated settings.json statusLine hook".to_string(),
                    Err(err) => format!("Hook error: {err}"),
                },
            },
            HookModalPlan::Uninstall(plan) => match plan {
                UninstallPlan::NotInstalled { .. } => {
                    "agyline is not currently configured in settings.json".to_string()
                }
                UninstallPlan::ForeignStatusLine { .. } => {
                    "settings.json uses an external statusLine; left untouched".to_string()
                }
                UninstallPlan::WouldChange { .. } => match apply_uninstall(ienv, plan) {
                    Ok(_) => "Updated settings.json statusLine hook".to_string(),
                    Err(err) => format!("Hook error: {err}"),
                },
            },
            HookModalPlan::Error(_) => return,
        };

        self.hook_inspection = self.env.install_env.as_ref().ok().map(inspect_hook);
        self.status_toast = Some(toast);
    }
}
