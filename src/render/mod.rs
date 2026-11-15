//! Styled intermediate representation of one render. The live line, `agyline preview`, and the
//! configurator all consume `RenderedFrame`: colors stay symbolic and text stays unescaped, so
//! the TUI builds ratatui spans from it without re-parsing ANSI. Only `ansi` serializes a frame.

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

/// One run of text in one color. `color` is still symbolic here: the palette and the terminal's
/// color depth are applied at emit time, never while building spans.
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

/// One catalog item's spans after narrowing. `id` is the item, not a slot index: dropped and
/// compacted slots change the row, but the surviving items keep their identity.
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

/// One output row. `indent_override` is set only by ultra-tiny narrowing. `broken_suffix` holds
/// this row's `! config` badge when one is reserved: `None` defers to the frame-level badge, and
/// `Some("")` suppresses it because it could not fit.
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

/// The whole render, shared by the live line, preview, and configurator. `slot_outcomes` keeps
/// the original config grid (rows x slots, empty and dropped ones included), so it is indexed by
/// config coordinates rather than by `lines`/`items`, which skip empty rows and dropped items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFrame {
    pub lines: Vec<StyledLine>,
    /// Config TOML failed to parse and defaults are in use; the emitters append `! config`.
    pub config_broken: bool,
    /// Per config row and slot, including slots that rendered nothing or were dropped.
    pub slot_outcomes: Vec<Vec<SlotOutcome>>,
}

impl RenderedFrame {
    /// Outcome of one config slot, or `None` when `row`/`slot` is outside the configured grid.
    pub fn slot_outcome(&self, row: usize, slot: usize) -> Option<SlotOutcome> {
        self.slot_outcomes
            .get(row)
            .and_then(|r| r.get(slot))
            .copied()
    }
}
