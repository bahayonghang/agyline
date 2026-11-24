//! Proportional `Usage Bar` renderer for context and quota items. Defaults to pure-foreground
//! half-cell `line` tracks (`━╸─`), and also supports `solid` (`█` over a polarity-aware track
//! background), shaded `block` (`█░`), and `ascii` (`#-`) degradation.

use crate::config::{EffectiveConfig, GlyphSet};
use crate::render::{
    ColorSpec, ColorToken, EffectiveColorDepth, StyledSpan, TextStyle, bar_track_color,
};

const PARTIAL_BLOCKS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Visual rendering mode for a `Usage Bar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarStyle {
    #[default]
    Line,
    Solid,
    Block,
    Ascii,
}

impl BarStyle {
    pub const ALL: [Self; 4] = [Self::Line, Self::Solid, Self::Block, Self::Ascii];

    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Solid => "solid",
            Self::Block => "block",
            Self::Ascii => "ascii",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "line" => Some(Self::Line),
            "solid" => Some(Self::Solid),
            "block" => Some(Self::Block),
            "ascii" => Some(Self::Ascii),
            _ => None,
        }
    }
}

/// Renders a fixed-column progress bar for `used_pct` (`0.0..=100.0`). Total display width of the
/// emitted spans always equals `width.clamp(4, 20)` across all color depths and glyph sets.
pub fn render_usage_bar(
    used_pct: f64,
    width: usize,
    fill_col: ColorSpec,
    config: &EffectiveConfig,
    style: BarStyle,
) -> Vec<StyledSpan> {
    let width = width.clamp(4, 20);
    let pct = if used_pct.is_finite() {
        used_pct.clamp(0.0, 100.0)
    } else {
        0.0
    };

    if config.effective_color_depth == EffectiveColorDepth::None || config.glyphs == GlyphSet::Ascii
    {
        return render_ascii_bar(pct, width, fill_col);
    }

    match style {
        BarStyle::Line => render_line_bar(pct, width, fill_col),
        BarStyle::Solid => {
            if matches!(
                config.effective_color_depth,
                EffectiveColorDepth::TrueColor | EffectiveColorDepth::Ansi256
            ) {
                render_solid_bar(pct, width, fill_col, config)
            } else {
                render_block_bar(pct, width, fill_col)
            }
        }
        BarStyle::Block => render_block_bar(pct, width, fill_col),
        BarStyle::Ascii => render_ascii_bar(pct, width, fill_col),
    }
}

fn render_line_bar(pct: f64, width: usize, fill_col: ColorSpec) -> Vec<StyledSpan> {
    let total_halves = width * 2;
    let halves = (((pct / 100.0) * (total_halves as f64)).round() as usize).min(total_halves);
    let full = halves / 2;
    let half = halves % 2;
    let empty = width.saturating_sub(full + half);

    let mut spans = Vec::with_capacity(2);
    if full > 0 || half > 0 {
        let mut fill_txt = "━".repeat(full);
        if half > 0 {
            fill_txt.push('╸');
        }
        spans.push(StyledSpan::new(fill_col, fill_txt));
    }
    if empty > 0 {
        spans.push(StyledSpan::new(ColorToken::Muted, "─".repeat(empty)));
    }
    spans
}

fn render_solid_bar(
    pct: f64,
    width: usize,
    fill_col: ColorSpec,
    config: &EffectiveConfig,
) -> Vec<StyledSpan> {
    let track_bg = bar_track_color(config.polarity);
    let total_eighths = width * 8;
    let eighths = (((pct / 100.0) * (total_eighths as f64)).round() as usize).min(total_eighths);
    let full = eighths / 8;
    let rem = eighths % 8;
    let partial_cols = usize::from(rem > 0);
    let empty = width.saturating_sub(full + partial_cols);

    let bar_style = TextStyle::PLAIN.with_bg(track_bg);
    let mut spans = Vec::with_capacity(2);
    if full > 0 || rem > 0 {
        let mut fill_txt = "█".repeat(full);
        fill_txt.push_str(PARTIAL_BLOCKS[rem]);
        spans.push(StyledSpan::styled(fill_col, bar_style, fill_txt));
    }
    if empty > 0 {
        spans.push(StyledSpan::styled(
            ColorToken::Muted,
            bar_style,
            " ".repeat(empty),
        ));
    }
    spans
}

fn render_block_bar(pct: f64, width: usize, fill_col: ColorSpec) -> Vec<StyledSpan> {
    let filled = (((pct / 100.0) * (width as f64)).round() as usize).min(width);
    let empty = width - filled;
    let mut spans = Vec::with_capacity(2);
    if filled > 0 {
        spans.push(StyledSpan::new(fill_col, "█".repeat(filled)));
    }
    if empty > 0 {
        spans.push(StyledSpan::new(ColorToken::Muted, "░".repeat(empty)));
    }
    spans
}

fn render_ascii_bar(pct: f64, width: usize, fill_col: ColorSpec) -> Vec<StyledSpan> {
    let filled = (((pct / 100.0) * (width as f64)).round() as usize).min(width);
    let empty = width - filled;
    let mut spans = Vec::with_capacity(2);
    if filled > 0 {
        spans.push(StyledSpan::new(fill_col, "#".repeat(filled)));
    }
    if empty > 0 {
        spans.push(StyledSpan::new(ColorToken::Muted, "-".repeat(empty)));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigDoc;
    use crate::render::{emit_ansi, emit_plain, str_display_width};
    use crate::tui::rendered_frame_to_ratatui_lines;
    use crate::{RenderedFrame, StyledItem, StyledLine};
    use ratatui::style::Modifier;
    use std::collections::BTreeMap;

    #[test]
    fn usage_bar_preserves_exact_width_and_degrades_across_depths_and_glyphs() {
        let mut tc_doc = ConfigDoc::cometix_base();
        tc_doc.color_depth = crate::config::ColorDepth::TrueColor;
        let tc_cfg = EffectiveConfig::from_doc(&tc_doc, None, &BTreeMap::new(), false);

        for pct in [0.0, 7.3, 19.7, 52.0, 100.0] {
            let spans = render_usage_bar(pct, 10, ColorToken::Ok.into(), &tc_cfg, BarStyle::Solid);
            let total_w: usize = spans.iter().map(|s| str_display_width(&s.text)).sum();
            assert_eq!(total_w, 10, "width mismatch at {pct}%");
            if pct > 0.0 {
                assert!(spans[0].style.bg.is_some());
            }
        }

        // 7.3% at width 10 -> 6 eighths -> partial block ▊ + 9 spaces
        let s73 = render_usage_bar(7.3, 10, ColorToken::Ok.into(), &tc_cfg, BarStyle::Solid);
        assert_eq!(s73.len(), 2);
        assert_eq!(s73[0].text, "▊");
        assert_eq!(s73[1].text.len(), 9);

        // 16-color falls back to █░
        let mut c16_doc = tc_doc.clone();
        c16_doc.color_depth = crate::config::ColorDepth::Ansi16;
        let c16_cfg = EffectiveConfig::from_doc(&c16_doc, None, &BTreeMap::new(), false);
        let b16 = render_usage_bar(50.0, 10, ColorToken::Ok.into(), &c16_cfg, BarStyle::Solid);
        assert_eq!(b16[0].text, "█████");
        assert_eq!(b16[1].text, "░░░░░");

        // None depth or Ascii glyphs fall back to #-
        let mut asc_doc = tc_doc.clone();
        asc_doc.glyphs = GlyphSet::Ascii;
        let asc_cfg = EffectiveConfig::from_doc(&asc_doc, None, &BTreeMap::new(), false);
        let basc = render_usage_bar(30.0, 10, ColorToken::Ok.into(), &asc_cfg, BarStyle::Solid);
        assert_eq!(basc[0].text, "###");
        assert_eq!(basc[1].text, "-------");
    }

    #[test]
    fn line_style_is_default_uses_half_cells_with_no_bg_and_light_solid_uses_soft_track() {
        assert_eq!(BarStyle::default(), BarStyle::Line);
        assert_eq!(BarStyle::from_key("line"), Some(BarStyle::Line));

        let mut tc_doc = ConfigDoc::cometix_base();
        tc_doc.color_depth = crate::config::ColorDepth::TrueColor;
        let tc_cfg = EffectiveConfig::from_doc(&tc_doc, None, &BTreeMap::new(), false);

        let expected = [
            (0.0, "──────────"),
            (5.0, "╸─────────"),
            (7.3, "╸─────────"),
            (14.2, "━╸────────"),
            (19.7, "━━────────"),
            (35.3, "━━━╸──────"),
            (52.0, "━━━━━─────"),
            (100.0, "━━━━━━━━━━"),
        ];
        for (pct, exp_str) in expected {
            let spans = render_usage_bar(pct, 10, ColorToken::Ok.into(), &tc_cfg, BarStyle::Line);
            let total_w: usize = spans.iter().map(|s| str_display_width(&s.text)).sum();
            assert_eq!(total_w, 10, "width mismatch at {pct}%");
            for s in &spans {
                assert!(s.style.bg.is_none(), "line bar must have no bg at {pct}%");
                assert_eq!(s.style, TextStyle::PLAIN);
            }
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, exp_str, "half-cell mismatch at {pct}%");
        }

        // Line bar stays ━╸─ across TrueColor, Ansi256, and Ansi16
        for depth in [
            crate::config::ColorDepth::TrueColor,
            crate::config::ColorDepth::Ansi256,
            crate::config::ColorDepth::Ansi16,
        ] {
            let mut d = tc_doc.clone();
            d.color_depth = depth;
            let cfg = EffectiveConfig::from_doc(&d, None, &BTreeMap::new(), false);
            let spans = render_usage_bar(35.3, 10, ColorToken::Ok.into(), &cfg, BarStyle::Line);
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, "━━━╸──────", "depth {depth:?} changed line bar");
        }

        // Ascii glyphs or None depth degrade Line to #-
        let mut asc_doc = tc_doc.clone();
        asc_doc.glyphs = GlyphSet::Ascii;
        let asc_cfg = EffectiveConfig::from_doc(&asc_doc, None, &BTreeMap::new(), false);
        let l_asc = render_usage_bar(30.0, 10, ColorToken::Ok.into(), &asc_cfg, BarStyle::Line);
        assert_eq!(l_asc[0].text, "###");
        assert_eq!(l_asc[1].text, "-------");

        let mut none_doc = tc_doc.clone();
        none_doc.color_depth = crate::config::ColorDepth::None;
        let none_cfg = EffectiveConfig::from_doc(&none_doc, None, &BTreeMap::new(), false);
        let l_none = render_usage_bar(30.0, 10, ColorToken::Ok.into(), &none_cfg, BarStyle::Line);
        assert_eq!(l_none[0].text, "###");
        assert_eq!(l_none[1].text, "-------");

        // Solid style on Light polarity emits #e2e5ec track and degrades to █░ on Ansi16
        let mut light_doc = tc_doc.clone();
        light_doc.theme = crate::config::ThemeSetting::Light;
        let light_cfg = EffectiveConfig::from_doc(&light_doc, None, &BTreeMap::new(), false);
        let s_light =
            render_usage_bar(20.0, 10, ColorToken::Ok.into(), &light_cfg, BarStyle::Solid);
        assert_eq!(s_light[0].style.bg, Some(ColorSpec::Rgb(226, 229, 236)));
        assert_eq!(s_light[1].style.bg, Some(ColorSpec::Rgb(226, 229, 236)));

        let mut l16_doc = light_doc;
        l16_doc.color_depth = crate::config::ColorDepth::Ansi16;
        let l16_cfg = EffectiveConfig::from_doc(&l16_doc, None, &BTreeMap::new(), false);
        let s16 = render_usage_bar(50.0, 10, ColorToken::Ok.into(), &l16_cfg, BarStyle::Solid);
        assert_eq!(s16[0].text, "█████");
        assert_eq!(s16[1].text, "░░░░░");
    }

    #[test]
    fn text_style_coalesces_adjacent_sgr_and_matches_ratatui_modifiers() {
        let mut doc = ConfigDoc::cometix_base();
        doc.color_depth = crate::config::ColorDepth::TrueColor;
        doc.palette = "tokyo-night".to_string();
        let cfg = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);

        let bold_style = TextStyle::PLAIN.bold();
        let italic_style = TextStyle::PLAIN.italic();
        let frame = RenderedFrame {
            lines: vec![StyledLine::new(vec![StyledItem::new(
                crate::config::ItemId::Model,
                vec![
                    StyledSpan::styled(ColorToken::Ok, bold_style, "A"),
                    StyledSpan::styled(ColorToken::Ok, bold_style, "B"),
                    StyledSpan::styled(ColorToken::Model, italic_style, "C"),
                ],
            )])],
            config_broken: false,
            slot_outcomes: vec![],
        };

        let ansi = emit_ansi(&frame, &cfg);
        // "A" and "B" share exact (Ok, bold, None), so only one styled SGR is emitted for "AB".
        assert!(ansi.contains(";1;38;2;158;206;106mAB\x1b[0;3;38;2;125;207;255mC\x1b[0m"));
        assert_eq!(emit_plain(&frame, &cfg), "  ABC\n");

        let tui_lines = rendered_frame_to_ratatui_lines(&frame, &cfg);
        assert_eq!(tui_lines.len(), 1);
        let spans = &tui_lines[0].spans;
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[3].style.add_modifier.contains(Modifier::ITALIC));
    }
}
