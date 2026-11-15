use crate::config::ItemId;

pub use ansi::{emit_ansi, emit_plain};
pub use palette::ColorToken;

pub mod ansi;
pub mod glyphs;
pub mod layout;
pub mod palette;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    pub color: ColorToken,
    pub text: String,
}

impl StyledSpan {
    pub fn new(color: ColorToken, text: impl Into<String>) -> Self {
        Self {
            color,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFrame {
    pub lines: Vec<StyledLine>,
    pub config_broken: bool,
}
