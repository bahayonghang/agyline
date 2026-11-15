use crate::config::ItemId;

pub use ansi::{emit_ansi, emit_plain};
pub use layout::{SlotOutcome, str_display_width};
pub use palette::{
    Ansi16Color, ColorSpec, ColorToken, EffectiveColorDepth, PaletteInfo, PalettePolarity,
    ResolvedColor, ResolvedPalette, builtin_palettes, discover_palettes, parse_color_spec,
    resolve_effective_color_depth, resolve_palette,
};

pub mod ansi;
pub mod glyphs;
pub mod layout;
pub mod palette;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub color: ColorSpec,
    pub text: String,
}

impl StyledSpan {
    pub fn new(color: impl Into<ColorSpec>, text: impl Into<String>) -> Self {
        Self {
            color: color.into(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledItem {
    pub id: ItemId,
    pub spans: Vec<StyledSpan>,
}

impl StyledItem {
    pub fn new(id: ItemId, spans: Vec<StyledSpan>) -> Self {
        Self { id, spans }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    pub items: Vec<StyledItem>,
    pub indent_override: Option<String>,
    pub broken_suffix: Option<String>,
}

impl StyledLine {
    pub fn new(items: Vec<StyledItem>) -> Self {
        Self {
            items,
            indent_override: None,
            broken_suffix: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFrame {
    pub lines: Vec<StyledLine>,
    pub config_broken: bool,
    pub slot_outcomes: Vec<Vec<SlotOutcome>>,
}

impl RenderedFrame {
    pub fn slot_outcome(&self, row: usize, slot: usize) -> Option<SlotOutcome> {
        self.slot_outcomes
            .get(row)
            .and_then(|r| r.get(slot))
            .copied()
    }
}
