//! Configurator state machine: the editable `ConfigDoc`, focus, bounded undo history and all disk
//! side effects. The preview uses the live renderer (`render_lines_with_width`) and is only then
//! converted to ratatui, so nothing here formats or re-parses ANSI. Saves are guarded by a disk
//! stamp, and an unparseable `config.toml` is never overwritten unconfirmed.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::text::Line;

use crate::config::{
    ActiveEnvOverride, ColorDepth, ConfigDoc, ConfigSyntaxError, DEFAULT_COMPACT_AT,
    EffectiveConfig, GlyphSet, ItemId, ItemSlot, ItemStyle, MAX_COMPACT_AT, OverflowMode,
    PresetScope, SEMANTIC_SEPARATORS, ThemeSetting, inspect_env_overrides, load_config_doc,
    presets::{apply_preset, discover_presets},
    resolve_separator, serialize_config_doc,
};
use crate::install::{
    HookInspection, InstallEnv, InstallError, InstallPlan, UninstallOptions, UninstallPlan,
    apply_install, apply_uninstall, inspect_hook, plan_install, plan_uninstall,
};
use crate::items::{ItemCatalogEntry, ItemCategory, catalog};
use crate::platform;
use crate::render::{BarStyle, ColorToken, RenderedFrame, discover_palettes, parse_color_spec};
use crate::scenarios::{Scenario, builtin_scenarios};
use crate::state::maybe_sweep_stale_state;
use crate::statusline::render_lines_with_width;
use crate::tui::convert::rendered_frame_to_ratatui_lines;

// History holds whole-doc snapshots, so it is capped and the oldest is evicted when full. Undo and
// redo only move snapshots between the two stacks, so this bounds their combined memory too.
pub const MAX_UNDO_STEPS: usize = 100;
/// Maximum configurable statusline rows supported by the Layout editor.
pub const MAX_LAYOUT_ROWS: usize = 5;

/// Dynamic outer height of the Live Preview box: 2 borders + 1 width ruler + 2..=4 status rows.
pub fn preview_box_height(rendered_line_count: usize) -> u16 {
    3 + rendered_line_count.clamp(2, 4) as u16
}

fn step_wrapped_index(len: usize, cur: usize, forward: bool) -> usize {
    if forward {
        (cur + 1) % len
    } else if cur == 0 {
        len - 1
    } else {
        cur - 1
    }
}

// Keeps temp names unique when two writes share a nanosecond timestamp or the clock is coarse.
static TUI_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Everything the configurator reads from the host, captured once at startup. Injected rather than
/// read live so tests can use a sandbox home and a fixed clock without touching process env.
#[derive(Debug, Clone)]
pub struct ConfiguratorEnv {
    // AGYLINE_HOME, else ~/.gemini/agyline; config, presets and state all hang off this directory.
    pub home_dir: PathBuf,
    pub env_vars: BTreeMap<String, String>,
    // Kept as a Result: a host with no usable settings path must still open the workbench.
    pub install_env: Result<InstallEnv, InstallError>,
    // Frozen at construction so the stale-state sweep is reproducible in tests.
    pub now_ms: u64,
}

impl ConfiguratorEnv {
    /// The only constructor that reads the real process: one env snapshot feeds home resolution,
    /// the AGY_* badges and the Effective preview so they cannot disagree.
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

    /// Test constructor: explicit home, env and clock, no process reads, install always available.
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

/// Exit path, not a write log: `Saved` is returned only by a save-and-quit, while `Cancelled`
/// covers quit and discard and may still follow an earlier Ctrl+S write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguratorExit {
    Saved,
    Cancelled,
}

/// The four workbench tabs in display order. `ALL`, `index()` and the tab bar must agree, since
/// mouse hit testing converts a column into a tab through that order.
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

    /// Position in `ALL`, used to map a tab-bar column back to a tab.
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

    /// Short labels below 100 columns so all four tabs still fit the minimum 80-column terminal.
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

/// Layout-tab focus; each pane keeps its own cursor, so switching back restores the position.
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

/// Editable keys of the selected slot. The visible set depends on the item id, so `inspector_idx`
/// is per-item and must be re-clamped whenever the selection changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorField {
    Priority,
    Icon,
    Fg,
    IconColor,
    Style,
    MaxLevels,
    ShowRemaining,
    Bucket,
    Family,
    Label,
    ShowUsed,
    ShowBar,
    BarStyle,
    BarWidth,
    ShowReset,
    ShowAmount,
    ShowSubagent,
    ShowDefault,
    Compact,
    ShowDetail,
    ShowWhenOff,
    ShowTtl,
    ShowBreakdown,
    ShowPending,
    ShowTokens,
}

impl InspectorField {
    /// TOML key name, matching what the drawer serializes for the same slot field.
    pub const fn key_label(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Icon => "icon",
            Self::Fg => "fg",
            Self::IconColor => "icon_color",
            Self::Style => "style",
            Self::MaxLevels => "max_levels",
            Self::ShowRemaining => "show_remaining",
            Self::Bucket => "bucket",
            Self::Family => "family",
            Self::Label => "label",
            Self::ShowUsed => "show_used",
            Self::ShowBar => "show_bar",
            Self::BarStyle => "bar_style",
            Self::BarWidth => "bar_width",
            Self::ShowReset => "show_reset",
            Self::ShowAmount => "show_amount",
            Self::ShowSubagent => "show_subagent",
            Self::ShowDefault => "show_default",
            Self::Compact => "compact",
            Self::ShowDetail => "show_detail",
            Self::ShowWhenOff => "show_when_off",
            Self::ShowTtl => "show_ttl",
            Self::ShowBreakdown => "show_breakdown",
            Self::ShowPending => "show_pending",
            Self::ShowTokens => "show_tokens",
        }
    }
}

/// Default boolean value when a per-slot toggle is unset (`None`), so the Inspector and toggle
/// actions stay synchronized for items whose options default to `true` (e.g. HUD `QuotaGroup`).
pub const fn inspector_bool_default(id: ItemId, field: InspectorField) -> bool {
    matches!(
        (id, field),
        (ItemId::Status, InspectorField::ShowPending)
            | (ItemId::Tokens, InspectorField::ShowBreakdown)
            | (ItemId::Context, InspectorField::ShowBar)
            | (
                ItemId::QuotaGroup,
                InspectorField::ShowUsed
                    | InspectorField::ShowBar
                    | InspectorField::ShowReset
                    | InspectorField::ShowAmount,
            )
    )
}

// Per-item field sets: the four base keys plus only the options that item actually reads.
const INSP_BASE: [InspectorField; 4] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
];
const INSP_STATUS: [InspectorField; 6] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
    InspectorField::ShowPending,
];
const INSP_STYLE: [InspectorField; 5] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
];
const INSP_DIR: [InspectorField; 5] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::MaxLevels,
];
const INSP_CONTEXT: [InspectorField; 11] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
    InspectorField::ShowRemaining,
    InspectorField::ShowBar,
    InspectorField::BarStyle,
    InspectorField::BarWidth,
    InspectorField::Label,
    InspectorField::ShowTokens,
];
const INSP_CACHE: [InspectorField; 6] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
    InspectorField::ShowTtl,
];
const INSP_TOKENS: [InspectorField; 6] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
    InspectorField::ShowBreakdown,
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
const INSP_MODE: [InspectorField; 7] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
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
const INSP_SANDBOX: [InspectorField; 5] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::ShowWhenOff,
];
const INSP_QUOTA: [InspectorField; 12] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Bucket,
    InspectorField::Label,
    InspectorField::ShowReset,
    InspectorField::ShowAmount,
    InspectorField::ShowUsed,
    InspectorField::ShowBar,
    InspectorField::BarStyle,
    InspectorField::BarWidth,
];
const INSP_QUOTA_GROUP: [InspectorField; 13] = [
    InspectorField::Priority,
    InspectorField::Icon,
    InspectorField::Fg,
    InspectorField::IconColor,
    InspectorField::Style,
    InspectorField::Family,
    InspectorField::Label,
    InspectorField::ShowUsed,
    InspectorField::ShowBar,
    InspectorField::BarStyle,
    InspectorField::BarWidth,
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

/// Field list for `id`. Lists differ in length, so `inspector_idx` must be clamped after any
/// change of selection or item kind.
pub fn inspector_fields_for_item(id: ItemId) -> &'static [InspectorField] {
    match id {
        ItemId::Status => &INSP_STATUS,
        ItemId::Model | ItemId::Git => &INSP_STYLE,
        ItemId::Dir => &INSP_DIR,
        ItemId::Context => &INSP_CONTEXT,
        ItemId::Cache => &INSP_CACHE,
        ItemId::Tokens => &INSP_TOKENS,
        ItemId::Budget => &INSP_BUDGET,
        ItemId::Mode => &INSP_MODE,
        ItemId::Subagents => &INSP_SUBAGENTS,
        ItemId::Sandbox => &INSP_SANDBOX,
        ItemId::Quota => &INSP_QUOTA,
        ItemId::QuotaGroup => &INSP_QUOTA_GROUP,
        ItemId::Cost => &INSP_COST,
        ItemId::Speed
        | ItemId::Tasks
        | ItemId::Queue
        | ItemId::Duration
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
    ItemStyle,
    BarStyle,
    BarGradient,
    Separator,
    Indent,
}

impl StyleField {
    pub const ALL: [Self; 9] = [
        Self::Palette,
        Self::Theme,
        Self::Glyphs,
        Self::ColorDepth,
        Self::ItemStyle,
        Self::BarStyle,
        Self::BarGradient,
        Self::Separator,
        Self::Indent,
    ];
}

/// Threshold keys in navigation order. The context triple (`warn < high < crit`, used %) and the
/// quota triple (`warn > high > crit`, remaining %) are strictly cross-validated on step & commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdField {
    SingleLine,
    CompactAt,
    Overflow,
    ContextWarn,
    ContextHigh,
    ContextCrit,
    QuotaWarn,
    QuotaHigh,
    QuotaCrit,
    CacheWarnSecs,
}

impl ThresholdField {
    pub const ALL: [Self; 10] = [
        Self::SingleLine,
        Self::CompactAt,
        Self::Overflow,
        Self::ContextWarn,
        Self::ContextHigh,
        Self::ContextCrit,
        Self::QuotaWarn,
        Self::QuotaHigh,
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

/// Inline-edit destination. Inspector targets carry row/slot so the commit addresses the slot the
/// edit was opened on, not whatever happens to be selected when it closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEditTarget {
    InspectorIcon { row: usize, slot: usize },
    InspectorFg { row: usize, slot: usize },
    InspectorIconColor { row: usize, slot: usize },
    InspectorStyle { row: usize, slot: usize },
    InspectorMaxLevels { row: usize, slot: usize },
    InspectorBucket { row: usize, slot: usize },
    InspectorFamily { row: usize, slot: usize },
    InspectorLabel { row: usize, slot: usize },
    InspectorBarStyle { row: usize, slot: usize },
    InspectorBarWidth { row: usize, slot: usize },
    StyleSeparator,
    StyleIndent,
    PaletteTokenOverride(ColorToken),
    ThresholdCompactAt,
    ThresholdContextWarn,
    ThresholdContextHigh,
    ThresholdContextCrit,
    ThresholdQuotaWarn,
    ThresholdQuotaHigh,
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
            Self::InspectorStyle { .. } => "style",
            Self::InspectorMaxLevels { .. } => "max_levels",
            Self::InspectorBucket { .. } => "bucket",
            Self::InspectorFamily { .. } => "family",
            Self::InspectorLabel { .. } => "label",
            Self::InspectorBarStyle { .. } => "bar_style",
            Self::InspectorBarWidth { .. } => "bar_width",
            Self::StyleSeparator => "separator",
            Self::StyleIndent => "indent",
            Self::PaletteTokenOverride(tok) => tok.as_key(),
            Self::ThresholdCompactAt => "compact_at",
            Self::ThresholdContextWarn => "context_warn",
            Self::ThresholdContextHigh => "context_high",
            Self::ThresholdContextCrit => "context_crit",
            Self::ThresholdQuotaWarn => "quota_warn",
            Self::ThresholdQuotaHigh => "quota_high",
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
    // A char index, not a byte offset: icons and labels may be non-ASCII.
    pub cursor_chars: usize,
    // Set when a commit is rejected; the edit stays open so the value can be corrected.
    pub error: Option<String>,
}

/// The three settings.json operations. Each opens a diff preview first; nothing is written until
/// the user confirms it.
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

/// A planned settings.json change, or the reason it could not be planned. `Error` is display-only
/// and is never applied.
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

/// One modal at a time; while open it intercepts every key before panes and global hotkeys.
/// `selected` is the focused button, and `exit_after_save` carries a quit intent through a save
/// confirmation so the user still leaves once the write succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    DirtyExit {
        selected: usize,
    },
    // The on-disk TOML did not parse; replacing it needs an explicit back-up-or-not choice.
    BrokenTomlConfirm {
        exit_after_save: bool,
        selected: usize,
    },
    // Disk changed since load: writing now would silently drop another editor's work.
    ExternalConflict {
        exit_after_save: bool,
        selected: usize,
    },
    // Scope is picked before applying so a Style preset cannot clobber a customized layout.
    PresetApply {
        preset_idx: usize,
        scope: PresetScope,
    },
    HookDiff(HookModalState),
    HelpOverlay,
}

/// Fingerprint of config.toml used to detect external edits before a save. Length and content hash
/// back up `modified`, which coarse filesystem timestamps can leave unchanged after a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiskStamp {
    pub exists: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub content_hash: u64,
}

impl DiskStamp {
    /// Reads the file now. A missing or unreadable file yields `exists: false` and a zero stamp,
    /// which compares unequal to any stamp taken from a file that was present.
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

// Cheap non-cryptographic hash: this only has to notice a changed file, not resist collisions.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Writes `file_name` under `home_dir` via a private temp file and an atomic rename, so a crash
/// or a concurrent reader never sees a half-written config. Returns the destination path.
fn atomic_write_in_home(home_dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    platform::ensure_private_dir(home_dir)?;
    let dst_path = home_dir.join(file_name);
    let seq = TUI_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // `create_new` below fails on an existing name, so pid + nanos + counter must stay unique.
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
        // Flush and sync before the rename so the destination never exposes a partial file.
        f.flush()?;
        let _ = f.sync_all();
        Ok(())
    })();

    // One cleanup path for any write failure: a partial temp file must not be left behind.
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

/// Catalog grouped in category order (Workspace, Usage, Runtime, Session). The Layout pane draws
/// and the mouse hit-tests by index into this list, so the order is part of the UI contract.
pub fn ordered_catalog_entries() -> Vec<&'static ItemCatalogEntry> {
    let mut out = Vec::with_capacity(24);
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

/// Cycles All -> Layout -> Style; the scope decides which half of the doc a preset may replace.
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

/// The whole workbench state. `doc` is the draft and `baseline_doc` the last loaded or saved
/// version; dirty is their difference, so undoing back to the baseline clears the dirty flag.
pub struct ConfiguratorApp {
    pub(crate) env: ConfiguratorEnv,
    pub(crate) doc: ConfigDoc,
    // Last loaded or saved doc; `is_dirty` compares against it, not against the defaults.
    pub(crate) baseline_doc: ConfigDoc,
    // Oldest-first whole-doc snapshots, capped at MAX_UNDO_STEPS.
    pub(crate) undo_stack: VecDeque<ConfigDoc>,
    // Cleared by any new edit so redo cannot replay an abandoned branch.
    pub(crate) redo_stack: Vec<ConfigDoc>,
    // False when no config.toml was found: an unmodified draft must not create one.
    pub(crate) file_existed: bool,
    // The file failed to parse, so the draft is defaults and saving needs confirmation.
    pub(crate) loaded_from_broken_toml: bool,
    pub(crate) syntax_error: Option<ConfigSyntaxError>,
    // Taken at load and re-probed before each save to catch an external editor.
    pub(crate) loaded_disk_stamp: DiskStamp,
    // AGY_* values that currently win over the doc; shown as badges, never written to disk.
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
    // None means unbounded preview width; `last_finite_width` is what toggling back restores.
    pub(crate) sim_width: Option<usize>,
    pub(crate) last_finite_width: usize,
    // Once the user sets a width, switching scenarios must not override it.
    pub(crate) width_pinned_by_user: bool,
    // Applies AGY_* overrides to the preview only; toggling it never mutates the doc.
    pub(crate) effective_preview_mode: bool,
    pub(crate) toml_drawer_open: bool,
    // Placeholder until the first Resize event; `run_configurator` seeds the real size.
    pub(crate) term_size: (u16, u16),
    // Cleared by the next edit so a stale "Saved" toast cannot outlive the change it described.
    pub(crate) status_toast: Option<String>,
}

impl ConfiguratorApp {
    /// Scroll offsets that keep the selected Active Rows / Catalog entry visible. The view and the
    /// mouse hit test must use the same visible height or clicks land on the wrong item.
    pub(crate) fn layout_list_offsets(&self, visible_rows: usize) -> (usize, usize) {
        // Each row contributes a header line plus its slots; an empty row still shows one line.
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
        // Category headers are list items too, so the catalog index shifts once per new category.
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

    /// Scroll offset for Inspector fields beneath the pinned item header line.
    pub(crate) fn inspector_list_offset(&self, visible_rows: usize) -> usize {
        (self.inspector_idx + 1).saturating_sub(visible_rows.saturating_sub(1))
    }

    /// Scroll offset for the Presets list beneath the pinned Scope header line.
    pub(crate) fn style_preset_offset(&self, visible_rows: usize) -> usize {
        (self.style_preset_idx + 1).saturating_sub(visible_rows.saturating_sub(1))
    }

    /// Maps each visual line drawn in Global Style back to its `StyleField` index so mouse clicks
    /// and scroll offsets stay aligned when `AGY_*` override badges insert extra lines.
    pub(crate) fn style_global_row_map(&self) -> Vec<usize> {
        let mut map = Vec::with_capacity(StyleField::ALL.len() + 2);
        for (idx, field) in StyleField::ALL.iter().enumerate() {
            map.push(idx);
            let env_key = match field {
                StyleField::Theme => Some("theme"),
                StyleField::Indent => Some("indent"),
                StyleField::Palette
                | StyleField::Glyphs
                | StyleField::ColorDepth
                | StyleField::ItemStyle
                | StyleField::BarStyle
                | StyleField::BarGradient
                | StyleField::Separator => None,
            };
            if let Some(key) = env_key
                && self.env_overrides.iter().any(|ov| ov.target_key == key)
            {
                map.push(idx);
            }
        }
        map
    }

    /// Scroll offset for Global Style that keeps the selected field (and its env badge) in view.
    pub(crate) fn style_global_offset(&self, visible_rows: usize) -> usize {
        let row_map = self.style_global_row_map();
        let selected_line = row_map
            .iter()
            .rposition(|&f| f == self.style_field_idx)
            .unwrap_or(self.style_field_idx);
        (selected_line + 1).saturating_sub(visible_rows)
    }

    /// Scroll offset for `[palette_overrides]` across all `ColorToken::ALL` entries.
    pub(crate) fn palette_override_offset(&self, visible_rows: usize) -> usize {
        (self.palette_override_idx + 1).saturating_sub(visible_rows)
    }

    /// Scroll offset for the `Layout & [thresholds]` pane.
    pub(crate) fn threshold_list_offset(&self, visible_rows: usize) -> usize {
        (self.threshold_field_idx + 1).saturating_sub(visible_rows)
    }

    /// Loads the draft and captures every host fact once. A broken config.toml loads as defaults
    /// with `loaded_from_broken_toml` set, so quitting cannot overwrite the unreadable file.
    pub fn new(env: ConfiguratorEnv) -> Self {
        // Reuse the render path's rate-limited sweep rather than adding a second cleanup policy.
        maybe_sweep_stale_state(&env.home_dir, env.now_ms);

        let loaded = load_config_doc(&env.home_dir);
        let loaded_disk_stamp = DiskStamp::probe(&env.home_dir.join("config.toml"));
        let env_overrides = inspect_env_overrides(&env.env_vars);
        let hook_inspection = env.install_env.as_ref().ok().map(inspect_hook);
        let available_presets = discover_presets(&env.home_dir);
        let available_palettes = discover_palettes(&env.home_dir);

        let scenarios = builtin_scenarios();
        // Start at the first scenario's width so the preview matches the scenario on screen.
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

    /// True when the draft differs from the last loaded or saved doc; undoing back clears it.
    pub fn is_dirty(&self) -> bool {
        self.doc != self.baseline_doc
    }

    pub fn has_active_modal(&self) -> bool {
        self.active_modal.is_some()
    }

    pub fn active_tab(&self) -> TuiTab {
        self.active_tab
    }

    /// Wraps the index so a stale selection cannot panic if the scenario list ever shrinks.
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

    /// The same `EffectiveConfig` the live render uses. `effective_preview_mode` decides only
    /// whether AGY_* overrides apply; it never changes the doc.
    pub fn effective_config_for_preview(&self) -> EffectiveConfig {
        let mut cfg = EffectiveConfig::from_doc(
            &self.doc,
            Some(&self.env.home_dir),
            &self.env.env_vars,
            self.effective_preview_mode,
        );
        if self.loaded_from_broken_toml {
            // Keep the broken-config badge even though the draft itself is valid defaults.
            cfg.config_broken = true;
        }
        cfg
    }

    /// The single preview path: live renderer first, ratatui conversion afterwards. Returns the
    /// frame together with the config it was rendered with so the view labels the same values.
    pub fn preview_rendered_frame(&self) -> (RenderedFrame, EffectiveConfig) {
        let cfg = self.effective_config_for_preview();
        let sc = self.active_scenario();
        // File-Only also drops AGY_CACHE_RELAY's effect on the frame, not just on the config.
        let frame = if self.effective_preview_mode {
            sc.frame_for_env(&self.env.env_vars)
        } else {
            sc.frame.clone()
        };
        let rendered = render_lines_with_width(&frame, &cfg, self.sim_width);
        (rendered, cfg)
    }

    /// Dynamic Live Preview box height for the currently rendered output. Shared by `view.rs`
    /// and `input.rs` mouse hit-testing so clicks on tabs and panes stay aligned.
    pub fn preview_box_height(&self) -> u16 {
        let (rendered, _) = self.preview_rendered_frame();
        preview_box_height(rendered.lines.len())
    }

    /// Preview as ratatui lines; the text must equal `emit_plain` of the same frame.
    pub fn preview_lines(&self) -> Vec<Line<'static>> {
        let (rendered, cfg) = self.preview_rendered_frame();
        rendered_frame_to_ratatui_lines(&rendered, &cfg)
    }

    /// The only way to change the draft: snapshot, apply, then record one undo step if anything
    /// actually changed. A no-op edit must not push a snapshot or clear redo.
    pub(crate) fn mutate_doc<F: FnOnce(&mut ConfigDoc)>(&mut self, f: F) {
        let snapshot = self.doc.clone();
        f(&mut self.doc);
        if self.doc != snapshot {
            // Full history: evict oldest-first so the newest steps survive the cap.
            if self.undo_stack.len() >= MAX_UNDO_STEPS {
                let _ = self.undo_stack.pop_front();
            }
            self.undo_stack.push_back(snapshot);
            // A new edit forks history; redo must not replay the abandoned branch.
            self.redo_stack.clear();
            self.status_toast = None;
        }
        // Outside the change check: indices are only valid against the post-edit doc.
        self.clamp_layout_selection();
    }

    /// Restores the previous snapshot and pushes the current doc onto redo; no-op when empty.
    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop_back() {
            let current = std::mem::replace(&mut self.doc, prev);
            self.redo_stack.push(current);
            self.clamp_layout_selection();
            self.status_toast = Some("Undid last change".to_string());
        }
    }

    /// Replays the next snapshot. Re-applies the history cap because it pushes back onto undo.
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

    /// The layout editor always addresses two rows, so a shorter doc is padded before indexing.
    pub(crate) fn ensure_two_rows(doc: &mut ConfigDoc) {
        while doc.lines.len() < 2 {
            doc.lines.push(Vec::new());
        }
    }

    /// Keeps row, slot and inspector indices inside the doc. Must run after every mutation,
    /// undo, redo and reload, or the next draw and mouse hit test index out of bounds.
    pub(crate) fn clamp_layout_selection(&mut self) {
        let max_row = self.doc.lines.len().max(2) - 1;
        self.selected_row = self.selected_row.min(max_row);
        let row_len = self.doc.lines.get(self.selected_row).map_or(0, Vec::len);
        if row_len == 0 {
            self.selected_slot = 0;
            self.inspector_idx = 0;
        } else {
            self.selected_slot = self.selected_slot.min(row_len - 1);
            let slot_id = self.doc.lines[self.selected_row][self.selected_slot].id;
            // Field count is per item id, so switching slots can shrink the inspector list.
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

    /// Opens an inline edit with the cursor at the end; the buffer stays a draft until commit.
    pub(crate) fn start_text_edit(&mut self, target: TextEditTarget, initial: String) {
        let cursor_chars = initial.chars().count();
        self.text_edit = Some(TextEditState {
            target,
            buffer: initial,
            cursor_chars,
            error: None,
        });
    }

    /// Validates and applies the inline buffer. On rejection the edit stays open with `error` set;
    /// on success it closes. Empty clears optional keys, but separator/indent/icon keep raw text.
    pub(crate) fn commit_text_edit(&mut self) {
        let Some(edit) = self.text_edit.clone() else {
            return;
        };
        // Icon, separator and indent keep raw whitespace; every other field commits trimmed.
        let raw = edit.buffer;
        let trimmed = raw.trim().to_string();

        match edit.target {
            TextEditTarget::InspectorIcon { row, slot } => {
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        // An empty icon is a deliberate blank, so it is stored, not cleared.
                        s.icon = Some(raw);
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorFg { row, slot } => {
                // Reject before mutating: an unparseable color must not reach the doc or history.
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
            TextEditTarget::InspectorStyle { row, slot } => {
                let next = (!trimmed.is_empty()).then_some(trimmed);
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.style = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorMaxLevels { row, slot } => {
                let next = if trimmed.is_empty() {
                    None
                } else {
                    let Ok(v) = trimmed.parse::<u8>() else {
                        if let Some(ref mut st) = self.text_edit {
                            st.error = Some("Expected integer 1..=8 (empty clears)".to_string());
                        }
                        return;
                    };
                    if !(1..=8).contains(&v) {
                        if let Some(ref mut st) = self.text_edit {
                            st.error = Some("Require 1 <= max_levels <= 8".to_string());
                        }
                        return;
                    }
                    Some(v)
                };
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.max_levels = next;
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
            TextEditTarget::InspectorFamily { row, slot } => {
                let next = (!trimmed.is_empty()).then_some(trimmed);
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.family = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorLabel { row, slot } => {
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        // Context distinguishes Some("") (hide "ctx ") from None (default "ctx").
                        s.label = if s.id == ItemId::Context {
                            Some(trimmed)
                        } else {
                            (!trimmed.is_empty()).then_some(trimmed)
                        };
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorBarStyle { row, slot } => {
                let next = if trimmed.is_empty() {
                    None
                } else if let Some(parsed) = BarStyle::from_key(&trimmed) {
                    Some(parsed.as_key().to_string())
                } else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(
                            "Expected bar_style: line, parallelogram, braille, solid, block, or ascii"
                                .to_string(),
                        );
                    }
                    return;
                };
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.bar_style = next;
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::InspectorBarWidth { row, slot } => {
                let next = if trimmed.is_empty() {
                    None
                } else {
                    let Ok(v) = trimmed.parse::<u8>() else {
                        if let Some(ref mut st) = self.text_edit {
                            st.error = Some("Expected integer 4..=20 (empty clears)".to_string());
                        }
                        return;
                    };
                    if !(4..=20).contains(&v) {
                        if let Some(ref mut st) = self.text_edit {
                            st.error = Some("Require 4 <= bar_width <= 20".to_string());
                        }
                        return;
                    }
                    Some(v)
                };
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot)) {
                        s.bar_width = next;
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
                        // Clearing falls back to the palette rather than storing an empty color.
                        doc.palette_overrides.remove(tok);
                    } else {
                        doc.palette_overrides.set(tok, trimmed);
                    }
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdCompactAt => {
                let Ok(v) = trimmed.parse::<usize>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!(
                            "Expected integer 0..={MAX_COMPACT_AT} (0 disables; default {DEFAULT_COMPACT_AT})"
                        ));
                    }
                    return;
                };
                if v > MAX_COMPACT_AT {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require 0 <= compact_at <= {MAX_COMPACT_AT}"));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.compact_at = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdContextWarn => {
                let high = self.doc.thresholds.context_high;
                let Ok(v) = trimmed.parse::<f64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Expected number 0..<{high} (< context_high)"));
                    }
                    return;
                };
                if !v.is_finite() || v < 0.0 || v >= high {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require 0 <= context_warn < {high}"));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.context_warn = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdContextHigh => {
                let warn = self.doc.thresholds.context_warn;
                let crit = self.doc.thresholds.context_crit;
                let Ok(v) = trimmed.parse::<f64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Expected number between {warn} and {crit}"));
                    }
                    return;
                };
                if !v.is_finite() || v <= warn || v >= crit {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require {warn} < context_high < {crit}"));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.context_high = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdContextCrit => {
                let high = self.doc.thresholds.context_high;
                let Ok(v) = trimmed.parse::<f64>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Expected number >{high}..=100 (> context_high)"));
                    }
                    return;
                };
                if !v.is_finite() || v > 100.0 || v <= high {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require {high} < context_crit <= 100"));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.context_crit = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdQuotaWarn => {
                // Inverted vs context: quota is remaining percent, so warn > high > crit.
                let high = self.doc.thresholds.quota_high;
                let Ok(v) = trimmed.parse::<i32>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Expected integer >{high}..=100 (> quota_high)"));
                    }
                    return;
                };
                if v > 100 || v <= high {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require {high} < quota_warn <= 100"));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.quota_warn = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdQuotaHigh => {
                let crit = self.doc.thresholds.quota_crit;
                let warn = self.doc.thresholds.quota_warn;
                let Ok(v) = trimmed.parse::<i32>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Expected integer between {crit} and {warn}"));
                    }
                    return;
                };
                if v <= crit || v >= warn {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require {crit} < quota_high < {warn}"));
                    }
                    return;
                }
                self.mutate_doc(|doc| {
                    doc.thresholds.quota_high = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::ThresholdQuotaCrit => {
                let high = self.doc.thresholds.quota_high;
                let Ok(v) = trimmed.parse::<i32>() else {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Expected integer 0..<{high} (< quota_high)"));
                    }
                    return;
                };
                if v < 0 || v >= high {
                    if let Some(ref mut st) = self.text_edit {
                        st.error = Some(format!("Require 0 <= quota_crit < {high}"));
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
                    // Git-cache freshness TTL in ms; 0 asks for a refresh on every render.
                    doc.git_cache_ms = v;
                });
                self.text_edit = None;
            }
            TextEditTarget::RuntimeCacheRelayUrl => {
                self.mutate_doc(|doc| {
                    // Stored as typed; the loopback warning is advisory, not a validation gate.
                    doc.cache_relay_url = trimmed;
                });
                self.text_edit = None;
            }
        }
    }

    /// Applies a preset as one undo step. An unknown name changes nothing and takes no snapshot.
    pub(crate) fn apply_preset_with_undo(&mut self, name: &str, scope: PresetScope) {
        // `mutate_doc` borrows all of self, so the home path must be cloned out first.
        let home = self.env.home_dir.clone();
        self.mutate_doc(|doc| {
            // False means the preset could not be resolved; the doc is left untouched.
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
        let next_idx = step_wrapped_index(self.available_palettes.len(), cur_idx, forward);
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

    /// Cycles the depth *setting*, not the resolved depth: Auto still follows the environment.
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

    pub(crate) fn cycle_item_style(&mut self, forward: bool) {
        let styles = ItemStyle::ALL;
        let idx = styles
            .iter()
            .position(|&s| s == self.doc.item_style)
            .unwrap_or(0);
        let next = styles[step_wrapped_index(styles.len(), idx, forward)];
        self.mutate_doc(|doc| {
            doc.item_style = next;
        });
    }

    pub(crate) fn cycle_bar_style(&mut self, forward: bool) {
        let styles = BarStyle::ALL;
        let idx = styles
            .iter()
            .position(|&s| s == self.doc.bar_style)
            .unwrap_or(0);
        let next = styles[step_wrapped_index(styles.len(), idx, forward)];
        self.mutate_doc(|doc| {
            doc.bar_style = next;
        });
    }

    pub(crate) fn toggle_bar_gradient(&mut self) {
        let next = !self.doc.bar_gradient;
        self.mutate_doc(|doc| {
            doc.bar_gradient = next;
        });
    }

    /// Cycles semantic separator presets (`pipe`, `dashed`, `dot`, `square`, `none`). Matches
    /// both raw aliases and their resolved glyph form so presets like `" │ "` step immediately.
    pub(crate) fn cycle_separator_preset(&mut self, forward: bool) {
        let cur_trimmed = self.doc.separator.trim();
        let cur_resolved = resolve_separator(&self.doc.separator);
        let cur_idx = SEMANTIC_SEPARATORS.iter().position(|&(alias, glyph)| {
            alias.eq_ignore_ascii_case(cur_trimmed) || glyph == cur_resolved
        });
        let len = SEMANTIC_SEPARATORS.len();
        let next_idx = match (cur_idx, forward) {
            (Some(i), _) => step_wrapped_index(len, i, forward),
            (None, true) => 0,
            (None, false) => len - 1,
        };
        let next = SEMANTIC_SEPARATORS[next_idx].0.to_string();
        self.mutate_doc(|doc| {
            doc.separator = next;
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

    /// Switches the preview scenario; the width follows it unless the user pinned a width.
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

    /// Steps the simulated width by `delta` columns, clamped to 40..=200, and pins it.
    pub(crate) fn adjust_sim_width(&mut self, delta: isize) {
        self.width_pinned_by_user = true;
        // Stepping out of unbounded mode resumes from the last finite width.
        let base = self.sim_width.unwrap_or(self.last_finite_width) as isize;
        let next = (base + delta).clamp(40, 200) as usize;
        self.sim_width = Some(next);
        self.last_finite_width = next;
    }

    /// Toggles unbounded (`None`) against the remembered finite width; both directions pin it.
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

    /// Save entry point. Runs the external-edit and broken-TOML guards first and may open a modal
    /// instead of writing; returns an exit only when the caller asked to quit after saving.
    pub(crate) fn request_save(&mut self, exit_after_save: bool) -> Option<ConfiguratorExit> {
        let cfg_path = self.config_path();
        let current_stamp = DiskStamp::probe(&cfg_path);
        // Another editor changed the file since load; writing now would silently drop their work.
        if current_stamp != self.loaded_disk_stamp {
            self.active_modal = Some(ActiveModal::ExternalConflict {
                exit_after_save,
                selected: 0,
            });
            return None;
        }

        // The draft is defaults, not the user's file: overwriting needs an explicit .bak choice.
        if self.loaded_from_broken_toml {
            self.active_modal = Some(ActiveModal::BrokenTomlConfirm {
                exit_after_save,
                selected: 0,
            });
            return None;
        }

        // Never materialize a config.toml just to restate the defaults.
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

    /// The only config.toml writer. With `write_backup` the current bytes go to config.toml.bak
    /// first, and a failed backup aborts the save so the original is never left unprotected.
    pub(crate) fn perform_write_config(
        &mut self,
        write_backup: bool,
        exit_after_save: bool,
    ) -> Option<ConfiguratorExit> {
        let cfg_path = self.config_path();
        if write_backup && cfg_path.exists() {
            // Back up before overwriting; any failure here aborts the save entirely.
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
                // Re-baseline and re-stamp: the doc is clean and this write is the new baseline.
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
                // Keep baseline and stamp: a failed save must stay dirty and re-check the disk.
                self.active_modal = None;
                self.status_toast = Some(format!("Save error: {err}"));
                None
            }
        }
    }

    /// Discards the draft for the on-disk version, but keeps the old draft as one undo step so the
    /// reload itself is reversible.
    pub(crate) fn reload_from_disk(&mut self) {
        // Push the current draft first: choosing Reload must not silently destroy local edits.
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

    /// Plans a settings.json change and shows it as a diff; nothing is written here.
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
                    // Reset drops statusLine and discards the snapshot instead of restoring it.
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

    /// Confirms the hook diff and writes settings.json only. It never saves the config draft, so
    /// an unsaved layout stays unsaved.
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
                // A statusLine we do not own is never replaced.
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

        // Re-inspect so the tab shows the post-write state, not the pre-write plan.
        self.hook_inspection = self.env.install_env.as_ref().ok().map(inspect_hook);
        self.status_toast = Some(toast);
    }
}
