use crate::collect::FrameData;
use crate::config::{EffectiveConfig, GlyphSet, ItemId, ItemSlot};
use crate::render::{ColorSpec, ColorToken, StyledItem, StyledSpan, glyphs, parse_color_spec};

pub mod runtime;
pub mod usage;
pub mod workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemCategory {
    Workspace,
    Usage,
    Runtime,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOptionKind {
    Bool,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemOptionSpec {
    pub key: &'static str,
    pub kind: ItemOptionKind,
    pub summary: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemCatalogEntry {
    pub id: ItemId,
    pub key: &'static str,
    pub category: ItemCategory,
    pub default_priority: u8,
    pub default_color: ColorToken,
    pub allow_multiple: bool,
    pub summary: &'static str,
    pub options: &'static [ItemOptionSpec],
}

const CONTEXT_OPTS: [ItemOptionSpec; 1] = [ItemOptionSpec {
    key: "show_remaining",
    kind: ItemOptionKind::Bool,
    summary: "Show remaining % instead of used %",
}];

const BUDGET_OPTS: [ItemOptionSpec; 4] = [
    ItemOptionSpec {
        key: "bucket",
        kind: ItemOptionKind::Text,
        summary: "Quota bucket filter (empty = minimum remaining)",
    },
    ItemOptionSpec {
        key: "show_reset",
        kind: ItemOptionKind::Bool,
        summary: "Append bucket reset countdown",
    },
    ItemOptionSpec {
        key: "show_amount",
        kind: ItemOptionKind::Bool,
        summary: "Show count allowance when present",
    },
    ItemOptionSpec {
        key: "show_subagent",
        kind: ItemOptionKind::Bool,
        summary: "Append subagent USD cost share",
    },
];

const MODE_OPTS: [ItemOptionSpec; 2] = [
    ItemOptionSpec {
        key: "show_default",
        kind: ItemOptionKind::Bool,
        summary: "Show default ask mode when no special mode is active",
    },
    ItemOptionSpec {
        key: "compact",
        kind: ItemOptionKind::Bool,
        summary: "Always render compact mode pill",
    },
];

const SUBAGENTS_OPTS: [ItemOptionSpec; 1] = [ItemOptionSpec {
    key: "show_detail",
    kind: ItemOptionKind::Bool,
    summary: "Expand up to two active subagent names and models",
}];

const QUOTA_OPTS: [ItemOptionSpec; 4] = [
    ItemOptionSpec {
        key: "bucket",
        kind: ItemOptionKind::Text,
        summary: "Quota bucket filter (empty = minimum remaining)",
    },
    ItemOptionSpec {
        key: "label",
        kind: ItemOptionKind::Text,
        summary: "Display label after percentage (default: quota)",
    },
    ItemOptionSpec {
        key: "show_reset",
        kind: ItemOptionKind::Bool,
        summary: "Append bucket reset countdown",
    },
    ItemOptionSpec {
        key: "show_amount",
        kind: ItemOptionKind::Bool,
        summary: "Show count allowance fallback or note",
    },
];

const COST_OPTS: [ItemOptionSpec; 1] = [ItemOptionSpec {
    key: "show_subagent",
    kind: ItemOptionKind::Bool,
    summary: "Append subagent USD cost share",
}];

const ITEM_CATALOG: [ItemCatalogEntry; 20] = [
    ItemCatalogEntry {
        id: ItemId::Model,
        key: "model",
        category: ItemCategory::Workspace,
        default_priority: 1,
        default_color: ColorToken::Model,
        allow_multiple: false,
        summary: "Active response/picker model and thinking effort",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Dir,
        key: "dir",
        category: ItemCategory::Workspace,
        default_priority: 3,
        default_color: ColorToken::Dir,
        allow_multiple: false,
        summary: "Current workspace leaf directory",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Git,
        key: "git",
        category: ItemCategory::Workspace,
        default_priority: 2,
        default_color: ColorToken::Git,
        allow_multiple: false,
        summary: "Git branch, clean/dirty/conflict mark, and ahead/behind",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Context,
        key: "context",
        category: ItemCategory::Usage,
        default_priority: 1,
        default_color: ColorToken::Context,
        allow_multiple: false,
        summary: "Context window usage percentage and compact token count",
        options: &CONTEXT_OPTS,
    },
    ItemCatalogEntry {
        id: ItemId::Cache,
        key: "cache",
        category: ItemCategory::Usage,
        default_priority: 4,
        default_color: ColorToken::Cache,
        allow_multiple: false,
        summary: "5-minute prompt cache TTL, relay keepalive badge, and hit rate",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Speed,
        key: "speed",
        category: ItemCategory::Usage,
        default_priority: 5,
        default_color: ColorToken::Ok,
        allow_multiple: false,
        summary: "Output token generation speed (tok/s)",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Budget,
        key: "budget",
        category: ItemCategory::Usage,
        default_priority: 3,
        default_color: ColorToken::Ok,
        allow_multiple: false,
        summary: "Combined quota bucket percentage and session USD cost",
        options: &BUDGET_OPTS,
    },
    ItemCatalogEntry {
        id: ItemId::Mode,
        key: "mode",
        category: ItemCategory::Runtime,
        default_priority: 1,
        default_color: ColorToken::Auto,
        allow_multiple: false,
        summary: "Execution mode and pending tool confirmation banner",
        options: &MODE_OPTS,
    },
    ItemCatalogEntry {
        id: ItemId::Subagents,
        key: "subagents",
        category: ItemCategory::Runtime,
        default_priority: 2,
        default_color: ColorToken::Model,
        allow_multiple: false,
        summary: "Running and total subagent counts with optional active details",
        options: &SUBAGENTS_OPTS,
    },
    ItemCatalogEntry {
        id: ItemId::Tasks,
        key: "tasks",
        category: ItemCategory::Runtime,
        default_priority: 3,
        default_color: ColorToken::Warn,
        allow_multiple: false,
        summary: "Active background task count",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Queue,
        key: "queue",
        category: ItemCategory::Runtime,
        default_priority: 2,
        default_color: ColorToken::Context,
        allow_multiple: false,
        summary: "Queued user input count",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Sandbox,
        key: "sandbox",
        category: ItemCategory::Runtime,
        default_priority: 4,
        default_color: ColorToken::Model,
        allow_multiple: false,
        summary: "Sandbox isolation and network allowance state",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Quota,
        key: "quota",
        category: ItemCategory::Usage,
        default_priority: 3,
        default_color: ColorToken::Ok,
        allow_multiple: true,
        summary: "Atomic single quota bucket indicator (repeatable per row)",
        options: &QUOTA_OPTS,
    },
    ItemCatalogEntry {
        id: ItemId::Cost,
        key: "cost",
        category: ItemCategory::Usage,
        default_priority: 3,
        default_color: ColorToken::Warn,
        allow_multiple: false,
        summary: "Atomic session USD cost indicator",
        options: &COST_OPTS,
    },
    ItemCatalogEntry {
        id: ItemId::Vim,
        key: "vim",
        category: ItemCategory::Session,
        default_priority: 2,
        default_color: ColorToken::Ok,
        allow_multiple: false,
        summary: "Current Vim input mode when Vim keybindings are active",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Title,
        key: "title",
        category: ItemCategory::Session,
        default_priority: 4,
        default_color: ColorToken::Muted,
        allow_multiple: false,
        summary: "Single-line sanitized conversation title",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Agent,
        key: "agent",
        category: ItemCategory::Session,
        default_priority: 2,
        default_color: ColorToken::Model,
        allow_multiple: false,
        summary: "Active non-default custom agent name",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Artifacts,
        key: "artifacts",
        category: ItemCategory::Session,
        default_priority: 4,
        default_color: ColorToken::Git,
        allow_multiple: false,
        summary: "Session artifact count",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Version,
        key: "version",
        category: ItemCategory::Session,
        default_priority: 5,
        default_color: ColorToken::Muted,
        allow_multiple: false,
        summary: "Host agy CLI version",
        options: &[],
    },
    ItemCatalogEntry {
        id: ItemId::Plan,
        key: "plan",
        category: ItemCategory::Session,
        default_priority: 5,
        default_color: ColorToken::Context,
        allow_multiple: false,
        summary: "In-memory account plan tier",
        options: &[],
    },
];

pub fn catalog() -> &'static [ItemCatalogEntry] {
    &ITEM_CATALOG
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedItem {
    pub id: ItemId,
    pub priority: u8,
    pub full_spans: Vec<StyledSpan>,
    pub compact_spans: Vec<StyledSpan>,
}

impl EvaluatedItem {
    pub fn has_compact_diff(&self) -> bool {
        self.full_spans != self.compact_spans
    }
}

pub fn evaluate_slot_dual(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    match slot.id {
        ItemId::Model => workspace::eval_model(slot, frame, config),
        ItemId::Dir => workspace::eval_dir(slot, frame, config),
        ItemId::Git => workspace::eval_git(slot, frame, config),
        ItemId::Context => usage::eval_context(slot, frame, config),
        ItemId::Cache => usage::eval_cache(slot, frame, config),
        ItemId::Speed => usage::eval_speed(slot, frame, config),
        ItemId::Budget => usage::eval_budget(slot, frame, config),
        ItemId::Quota => usage::eval_quota(slot, frame, config),
        ItemId::Cost => usage::eval_cost(slot, frame, config),
        ItemId::Mode => runtime::eval_mode(slot, frame, config),
        ItemId::Subagents => runtime::eval_subagents(slot, frame, config),
        ItemId::Tasks => runtime::eval_tasks(slot, frame, config),
        ItemId::Queue => runtime::eval_queue(slot, frame, config),
        ItemId::Sandbox => runtime::eval_sandbox(slot, frame, config),
        ItemId::Vim => runtime::eval_vim(slot, frame, config),
        ItemId::Title => runtime::eval_title(slot, frame, config),
        ItemId::Agent => runtime::eval_agent(slot, frame, config),
        ItemId::Artifacts => runtime::eval_artifacts(slot, frame, config),
        ItemId::Version => runtime::eval_version(slot, frame, config),
        ItemId::Plan => runtime::eval_plan(slot, frame, config),
    }
}

pub fn evaluate_slot(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<StyledItem> {
    let eval = evaluate_slot_dual(slot, frame, config)?;
    Some(StyledItem::new(eval.id, eval.full_spans))
}

pub(crate) fn slot_fg_color(slot: &ItemSlot, default_token: ColorToken) -> ColorSpec {
    slot.fg
        .as_deref()
        .and_then(parse_color_spec)
        .unwrap_or(ColorSpec::Token(default_token))
}

pub(crate) fn slot_custom_icon_color(slot: &ItemSlot) -> Option<ColorSpec> {
    slot.icon_color.as_deref().and_then(parse_color_spec)
}

pub(crate) fn coalesced_icon_body_spans(
    icon: &str,
    custom_icon_col: Option<ColorSpec>,
    body_col: ColorSpec,
    body: String,
) -> Vec<StyledSpan> {
    if icon.is_empty() {
        vec![StyledSpan::new(body_col, body)]
    } else if let Some(ic) = custom_icon_col
        && ic != body_col
    {
        vec![
            StyledSpan::new(ic, icon),
            StyledSpan::new(body_col, format!(" {body}")),
        ]
    } else {
        vec![StyledSpan::new(body_col, format!("{icon} {body}"))]
    }
}

pub(crate) fn truncate_chars_with_ellipsis(
    text: &str,
    max_chars: usize,
    glyphs: GlyphSet,
) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1).max(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(glyphs::ellipsis(glyphs));
    out
}
