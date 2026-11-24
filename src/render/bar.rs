//! Proportional `Usage Bar` renderer for context and quota items. Supports cap-height segmented
//! `cells` (`❚`, alias `segments`), half-cell `line` (`━╸─`), slanted `parallelogram` (`▰▱`, alias
//! `blocks`), half-cell `braille` (`⣿⣤⣀`), `solid` (`█` over a polarity-aware track background),
//! shaded `block` (`█░`), and `ascii` (`#-`), with optional 4-tier RGB linear gradients.

use crate::config::{EffectiveConfig, GlyphSet};
use crate::render::{
    Ansi16Color, ColorSpec, ColorToken, EffectiveColorDepth, ResolvedColor, StyledSpan, TextStyle,
    bar_track_color,
};

const PARTIAL_BLOCKS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Visual rendering mode for a `Usage Bar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarStyle {
    Cells,
    #[default]
    Line,
    Solid,
    Block,
    Parallelogram,
    Braille,
    Ascii,
}

impl BarStyle {
    pub const ALL: [Self; 7] = [
        Self::Cells,
        Self::Line,
        Self::Solid,
        Self::Block,
        Self::Parallelogram,
        Self::Braille,
        Self::Ascii,
    ];

    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Cells => "cells",
            Self::Line => "line",
            Self::Solid => "solid",
            Self::Block => "block",
            Self::Parallelogram => "parallelogram",
            Self::Braille => "braille",
            Self::Ascii => "ascii",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cells" | "segments" => Some(Self::Cells),
            "line" => Some(Self::Line),
            "solid" => Some(Self::Solid),
            "block" => Some(Self::Block),
            "parallelogram" | "blocks" => Some(Self::Parallelogram),
            "braille" => Some(Self::Braille),
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
        BarStyle::Cells => {
            let empty_col = match config.effective_color_depth {
                EffectiveColorDepth::TrueColor | EffectiveColorDepth::Ansi256 => {
                    match config
                        .resolved_palette
                        .resolve_spec(ColorSpec::Token(ColorToken::SurfaceHi))
                    {
                        ResolvedColor::Ansi16(Ansi16Color::White | Ansi16Color::BrightWhite) => {
                            ColorToken::Muted
                        }
                        _ => ColorToken::SurfaceHi,
                    }
                }
                EffectiveColorDepth::Ansi16 | EffectiveColorDepth::None => ColorToken::Muted,
            };
            render_whole_cell_bar(pct, width, "❚", "❚", empty_col, fill_col, config)
        }
        BarStyle::Line => render_half_cell_bar(pct, width, "━", "╸", "─", fill_col, config),
        BarStyle::Parallelogram => {
            render_whole_cell_bar(pct, width, "▰", "▱", ColorToken::Muted, fill_col, config)
        }
        BarStyle::Braille => render_half_cell_bar(pct, width, "⣿", "⣤", "⣀", fill_col, config),
        BarStyle::Solid => {
            if matches!(
                config.effective_color_depth,
                EffectiveColorDepth::TrueColor | EffectiveColorDepth::Ansi256
            ) {
                render_solid_bar(pct, width, fill_col, config)
            } else {
                render_whole_cell_bar(pct, width, "█", "░", ColorToken::Muted, fill_col, config)
            }
        }
        BarStyle::Block => {
            render_whole_cell_bar(pct, width, "█", "░", ColorToken::Muted, fill_col, config)
        }
        BarStyle::Ascii => render_ascii_bar(pct, width, fill_col),
    }
}

fn render_half_cell_bar(
    pct: f64,
    width: usize,
    full_glyph: &str,
    half_glyph: &str,
    empty_glyph: &str,
    fill_col: ColorSpec,
    config: &EffectiveConfig,
) -> Vec<StyledSpan> {
    let total_halves = width * 2;
    let mut halves = (((pct / 100.0) * (total_halves as f64)).round() as usize).min(total_halves);
    if pct > 0.0 && halves == 0 {
        halves = 1;
    }
    let full = halves / 2;
    let half = halves % 2;
    let empty = width.saturating_sub(full + half);
    let partial = (half > 0).then_some(half_glyph);

    let mut spans = Vec::with_capacity(if config.bar_gradient { width + 1 } else { 2 });
    push_filled_cells(
        &mut spans,
        full,
        full_glyph,
        partial,
        width,
        fill_col,
        TextStyle::PLAIN,
        config,
    );
    if empty > 0 {
        spans.push(StyledSpan::new(
            ColorToken::Muted,
            empty_glyph.repeat(empty),
        ));
    }
    spans
}

fn render_whole_cell_bar(
    pct: f64,
    width: usize,
    full_glyph: &str,
    empty_glyph: &str,
    empty_col: ColorToken,
    fill_col: ColorSpec,
    config: &EffectiveConfig,
) -> Vec<StyledSpan> {
    let mut filled = (((pct / 100.0) * (width as f64)).round() as usize).min(width);
    if pct > 0.0 && filled == 0 {
        filled = 1;
    }
    let empty = width - filled;
    let mut spans = Vec::with_capacity(if config.bar_gradient { width + 1 } else { 2 });
    push_filled_cells(
        &mut spans,
        filled,
        full_glyph,
        None,
        width,
        fill_col,
        TextStyle::PLAIN,
        config,
    );
    if empty > 0 {
        spans.push(StyledSpan::new(empty_col, empty_glyph.repeat(empty)));
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
    let mut eighths =
        (((pct / 100.0) * (total_eighths as f64)).round() as usize).min(total_eighths);
    if pct > 0.0 && eighths == 0 {
        eighths = 1;
    }
    let full = eighths / 8;
    let rem = eighths % 8;
    let partial_cols = usize::from(rem > 0);
    let empty = width.saturating_sub(full + partial_cols);
    let partial = (rem > 0).then_some(PARTIAL_BLOCKS[rem]);

    let bar_style = TextStyle::PLAIN.with_bg(track_bg);
    let mut spans = Vec::with_capacity(if config.bar_gradient { width + 1 } else { 2 });
    push_filled_cells(
        &mut spans, full, "█", partial, width, fill_col, bar_style, config,
    );
    if empty > 0 {
        spans.push(StyledSpan::styled(
            ColorToken::Muted,
            bar_style,
            " ".repeat(empty),
        ));
    }
    spans
}

fn render_ascii_bar(pct: f64, width: usize, fill_col: ColorSpec) -> Vec<StyledSpan> {
    let mut filled = (((pct / 100.0) * (width as f64)).round() as usize).min(width);
    if pct > 0.0 && filled == 0 {
        filled = 1;
    }
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

// Interpolates filled bar cells smoothly in RGB under TrueColor/Ansi256, coalescing adjacent
// cells with identical quantized colors. Semantic urgency tokens sample the full-width 4-stop
// Ok -> Warn -> High -> Crit ramp so mid-range usage (e.g. 48%) already gradients cleanly; custom
// literal fill colors interpolate linearly from Ok to `fill_col`.
#[allow(clippy::too_many_arguments)]
fn push_filled_cells(
    spans: &mut Vec<StyledSpan>,
    full_count: usize,
    full_glyph: &str,
    partial_glyph: Option<&str>,
    bar_width: usize,
    fill_col: ColorSpec,
    text_style: TextStyle,
    config: &EffectiveConfig,
) {
    let total_cells = full_count + usize::from(partial_glyph.is_some());
    if total_cells == 0 {
        return;
    }

    let use_gradient = config.bar_gradient
        && total_cells > 1
        && matches!(
            config.effective_color_depth,
            EffectiveColorDepth::TrueColor | EffectiveColorDepth::Ansi256
        );
    if use_gradient {
        let is_semantic_tier = matches!(
            fill_col,
            ColorSpec::Token(
                ColorToken::Ok | ColorToken::Warn | ColorToken::High | ColorToken::Crit
            )
        );
        let ok_rgb = config
            .resolved_palette
            .resolve_spec(ColorSpec::Token(ColorToken::Ok))
            .to_rgb();
        let warn_rgb = config
            .resolved_palette
            .resolve_spec(ColorSpec::Token(ColorToken::Warn))
            .to_rgb();
        let high_rgb = config
            .resolved_palette
            .resolve_spec(ColorSpec::Token(ColorToken::High))
            .to_rgb();
        let crit_rgb = config
            .resolved_palette
            .resolve_spec(ColorSpec::Token(ColorToken::Crit))
            .to_rgb();
        let custom_end_rgb = config.resolved_palette.resolve_spec(fill_col).to_rgb();
        let has_variation = if is_semantic_tier {
            ok_rgb != warn_rgb || warn_rgb != high_rgb || high_rgb != crit_rgb
        } else {
            ok_rgb != custom_end_rgb
        };
        if has_variation {
            let width_den = bar_width.saturating_sub(1).max(1);
            let filled_den = total_cells - 1;
            let mut cur_spec: Option<ColorSpec> = None;
            let mut cur_text = String::with_capacity(full_glyph.len());
            for idx in 0..total_cells {
                let (r, g, b) = if is_semantic_tier {
                    let pos = if fill_col == ColorSpec::Token(ColorToken::Crit)
                        && idx + 1 == total_cells
                    {
                        1.0
                    } else {
                        (idx as f64 / width_den as f64).clamp(0.0, 1.0)
                    };
                    sample_four_tier_rgb(pos, ok_rgb, warn_rgb, high_rgb, crit_rgb)
                } else {
                    (
                        lerp_u8(ok_rgb.0, custom_end_rgb.0, idx, filled_den),
                        lerp_u8(ok_rgb.1, custom_end_rgb.1, idx, filled_den),
                        lerp_u8(ok_rgb.2, custom_end_rgb.2, idx, filled_den),
                    )
                };
                let spec = quantized_rgb_spec(r, g, b, config.effective_color_depth);
                let glyph = if idx < full_count {
                    full_glyph
                } else {
                    partial_glyph.unwrap_or(full_glyph)
                };
                if cur_spec == Some(spec) {
                    cur_text.push_str(glyph);
                } else {
                    if let Some(prev) = cur_spec.replace(spec) {
                        let flushed =
                            std::mem::replace(&mut cur_text, String::with_capacity(glyph.len()));
                        spans.push(StyledSpan::styled(prev, text_style, flushed));
                    }
                    cur_text.push_str(glyph);
                }
            }
            if let Some(last) = cur_spec
                && !cur_text.is_empty()
            {
                spans.push(StyledSpan::styled(last, text_style, cur_text));
            }
            return;
        }
    }

    let mut fill_txt =
        String::with_capacity(full_count * full_glyph.len() + partial_glyph.map_or(0, str::len));
    for _ in 0..full_count {
        fill_txt.push_str(full_glyph);
    }
    if let Some(pg) = partial_glyph {
        fill_txt.push_str(pg);
    }
    spans.push(StyledSpan::styled(fill_col, text_style, fill_txt));
}

fn sample_four_tier_rgb(
    pos: f64,
    ok: (u8, u8, u8),
    warn: (u8, u8, u8),
    high: (u8, u8, u8),
    crit: (u8, u8, u8),
) -> (u8, u8, u8) {
    let p = pos.clamp(0.0, 1.0);
    if p <= 0.50 {
        lerp_rgb_f64(ok, warn, p / 0.50)
    } else if p <= 0.75 {
        lerp_rgb_f64(warn, high, (p - 0.50) / 0.25)
    } else {
        lerp_rgb_f64(high, crit, (p - 0.75) / 0.25)
    }
}

fn lerp_rgb_f64(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        let v = f64::from(x) + (f64::from(y) - f64::from(x)) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn lerp_u8(a: u8, b: u8, num: usize, den: usize) -> u8 {
    let a = i32::from(a);
    let b = i32::from(b);
    let diff = b - a;
    let num = num as i32;
    let den = den as i32;
    let scaled = if diff >= 0 {
        (diff * num + den / 2) / den
    } else {
        (diff * num - den / 2) / den
    };
    (a + scaled).clamp(0, 255) as u8
}

fn quantized_rgb_spec(r: u8, g: u8, b: u8, depth: EffectiveColorDepth) -> ColorSpec {
    match ResolvedColor::Rgb(r, g, b).quantize(depth) {
        Some(ResolvedColor::Rgb(qr, qg, qb)) => ColorSpec::Rgb(qr, qg, qb),
        Some(ResolvedColor::Ansi256(idx)) => ColorSpec::Ansi256(idx),
        Some(ResolvedColor::Ansi16(c)) => ColorSpec::Ansi16(c),
        None => ColorSpec::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigDoc;
    use crate::render::{Ansi16Color, ResolvedColor, emit_ansi, emit_plain, str_display_width};
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

    #[test]
    fn parallelogram_and_braille_styles_preserve_width_and_degrade_cleanly() {
        assert_eq!(BarStyle::ALL.len(), 7);
        assert_eq!(BarStyle::from_key("cells"), Some(BarStyle::Cells));
        assert_eq!(BarStyle::from_key("segments"), Some(BarStyle::Cells));
        assert_eq!(BarStyle::Cells.as_key(), "cells");
        assert_eq!(
            BarStyle::from_key("parallelogram"),
            Some(BarStyle::Parallelogram)
        );
        assert_eq!(BarStyle::from_key("blocks"), Some(BarStyle::Parallelogram));
        assert_eq!(BarStyle::Parallelogram.as_key(), "parallelogram");
        assert_eq!(BarStyle::from_key("braille"), Some(BarStyle::Braille));
        assert_eq!(BarStyle::Braille.as_key(), "braille");

        let mut base_doc = ConfigDoc::cometix_base();
        base_doc.color_depth = crate::config::ColorDepth::TrueColor;
        let tc_cfg = EffectiveConfig::from_doc(&base_doc, None, &BTreeMap::new(), false);

        // Cap-height segmented `cells` (`❚`) lights filled cells in active color and empty cells
        // in `SurfaceHi` (falling back to `Muted` on light `terminal` so empty cells stay visible).
        let cat_doc = ConfigDoc {
            color_depth: crate::config::ColorDepth::TrueColor,
            bar_gradient: false,
            ..ConfigDoc::default()
        };
        let cat_cfg = EffectiveConfig::from_doc(&cat_doc, None, &BTreeMap::new(), false);
        let cell_spans =
            render_usage_bar(50.0, 10, ColorToken::Ok.into(), &cat_cfg, BarStyle::Cells);
        assert_eq!(cell_spans.len(), 2);
        assert_eq!(cell_spans[0].text, "❚❚❚❚❚");
        assert_eq!(cell_spans[0].color, ColorSpec::Token(ColorToken::Ok));
        assert_eq!(cell_spans[1].text, "❚❚❚❚❚");
        assert_eq!(cell_spans[1].color, ColorSpec::Token(ColorToken::SurfaceHi));
        assert_eq!(str_display_width("❚❚❚❚❚❚❚❚❚❚"), 10);

        let mut light_term_doc = ConfigDoc::cometix_base();
        light_term_doc.theme = crate::config::ThemeSetting::Light;
        light_term_doc.color_depth = crate::config::ColorDepth::TrueColor;
        let light_term_cfg =
            EffectiveConfig::from_doc(&light_term_doc, None, &BTreeMap::new(), false);
        let light_cells = render_usage_bar(
            50.0,
            10,
            ColorToken::Ok.into(),
            &light_term_cfg,
            BarStyle::Cells,
        );
        assert_eq!(light_cells[1].color, ColorSpec::Token(ColorToken::Muted));

        // Worked examples for parallelogram (whole-cell ▰/▱) and braille (half-cell ⣿/⣤/⣀).
        // Positive usage (<5%) lights at least 1 cell so low-percent 1M contexts stay visible.
        let para_expected = [
            (0.0, "▱▱▱▱▱▱▱▱▱▱"),
            (2.9, "▰▱▱▱▱▱▱▱▱▱"),
            (50.0, "▰▰▰▰▰▱▱▱▱▱"),
            (100.0, "▰▰▰▰▰▰▰▰▰▰"),
        ];
        for (pct, exp) in para_expected {
            let spans = render_usage_bar(
                pct,
                10,
                ColorToken::Ok.into(),
                &tc_cfg,
                BarStyle::Parallelogram,
            );
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, exp, "parallelogram mismatch at {pct}%");
            assert_eq!(str_display_width(&joined), 10);
        }

        let braille_expected = [
            (0.0, "⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀"),
            (1.0, "⣤⣀⣀⣀⣀⣀⣀⣀⣀⣀"),
            (5.0, "⣤⣀⣀⣀⣀⣀⣀⣀⣀⣀"),
            (35.3, "⣿⣿⣿⣤⣀⣀⣀⣀⣀⣀"),
            (100.0, "⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿"),
        ];
        for (pct, exp) in braille_expected {
            let spans =
                render_usage_bar(pct, 10, ColorToken::Ok.into(), &tc_cfg, BarStyle::Braille);
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, exp, "braille mismatch at {pct}%");
            assert_eq!(str_display_width(&joined), 10);
        }

        // Exact display column width holds across all 6 styles, widths, depths, glyphs, and edge floats.
        for depth in [
            crate::config::ColorDepth::TrueColor,
            crate::config::ColorDepth::Ansi256,
            crate::config::ColorDepth::Ansi16,
            crate::config::ColorDepth::None,
        ] {
            for glyphs in [GlyphSet::Nerd, GlyphSet::Unicode, GlyphSet::Ascii] {
                for bar_gradient in [false, true] {
                    let mut doc = ConfigDoc::cometix_base();
                    doc.palette = "catppuccin".to_string();
                    doc.color_depth = depth;
                    doc.glyphs = glyphs;
                    doc.bar_gradient = bar_gradient;
                    let cfg = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);
                    for style in BarStyle::ALL {
                        for raw_w in [2usize, 4, 8, 10, 20, 28] {
                            let expected_w = raw_w.clamp(4, 20);
                            for pct in [
                                -10.0,
                                0.0,
                                7.3,
                                50.0,
                                92.0,
                                100.0,
                                150.0,
                                f64::NAN,
                                f64::INFINITY,
                            ] {
                                let spans = render_usage_bar(
                                    pct,
                                    raw_w,
                                    ColorToken::Crit.into(),
                                    &cfg,
                                    style,
                                );
                                let w: usize =
                                    spans.iter().map(|s| str_display_width(&s.text)).sum();
                                assert_eq!(
                                    w, expected_w,
                                    "width mismatch style={style:?} depth={depth:?} glyphs={glyphs:?} grad={bar_gradient} w={raw_w} pct={pct}"
                                );
                            }
                        }
                    }
                }
            }
        }

        // Ascii glyphs and None color depth degrade parallelogram/braille to #-
        for (depth, glyphs) in [
            (crate::config::ColorDepth::TrueColor, GlyphSet::Ascii),
            (crate::config::ColorDepth::None, GlyphSet::Nerd),
        ] {
            let mut doc = ConfigDoc::cometix_base();
            doc.color_depth = depth;
            doc.glyphs = glyphs;
            let cfg = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);
            for style in [BarStyle::Parallelogram, BarStyle::Braille] {
                let spans = render_usage_bar(30.0, 10, ColorToken::Crit.into(), &cfg, style);
                let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
                assert_eq!(joined, "###-------");
            }
        }
    }

    #[test]
    fn bar_gradient_interpolates_rgb_quantizes_256_degrades_16_and_matches_tui_spans() {
        assert_eq!(ResolvedColor::Rgb(1, 2, 3).to_rgb(), (1, 2, 3));
        assert_eq!(
            ResolvedColor::Ansi16(Ansi16Color::Red).to_rgb(),
            Ansi16Color::Red.to_rgb()
        );
        assert_eq!(ResolvedColor::Ansi256(196).to_rgb(), (255, 0, 0));

        let mut doc = ConfigDoc::cometix_base();
        doc.palette = "catppuccin".to_string();
        doc.theme = crate::config::ThemeSetting::Dark;
        doc.color_depth = crate::config::ColorDepth::TrueColor;
        doc.bar_gradient = true;
        let tc_cfg = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);

        // Catppuccin Mocha: Ok = (166, 227, 161), Crit = (243, 139, 168).
        let spans = render_usage_bar(
            90.0,
            10,
            ColorToken::Crit.into(),
            &tc_cfg,
            BarStyle::Parallelogram,
        );
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "▰▰▰▰▰▰▰▰▰▱");
        assert_eq!(spans[0].color, ColorSpec::Rgb(166, 227, 161));
        assert_eq!(spans[spans.len() - 2].color, ColorSpec::Rgb(243, 139, 168));
        assert_eq!(
            spans.last().unwrap().color,
            ColorSpec::Token(ColorToken::Muted)
        );
        assert_eq!(spans.last().unwrap().text, "▱");
        assert!(
            spans.len() > 3,
            "TrueColor gradient should emit interpolated steps"
        );

        // Multi-stop gradient across Ok (Green) -> Warn (Yellow) -> High (Peach) -> Crit (Red):
        // green channel falls monotonically from 227 -> 139 and mid-bar hits warm yellow/peach.
        let filled_rgbs: Vec<(u8, u8, u8)> = spans[..spans.len() - 1]
            .iter()
            .map(|s| match s.color {
                ColorSpec::Rgb(r, g, b) => (r, g, b),
                other => panic!("expected RGB gradient span, got {other:?}"),
            })
            .collect();
        for pair in filled_rgbs.windows(2) {
            assert!(
                pair[0].1 >= pair[1].1,
                "green must fall monotonically across Ok->Warn->High->Crit: {pair:?}"
            );
        }
        assert!(
            filled_rgbs.iter().any(|&(r, g, _)| r >= 240 && g >= 210),
            "mid-bar must pass through warm yellow rather than muddy brown: {filled_rgbs:?}"
        );

        // Mid-range Ok usage (48%) already gradients from Green toward Yellow across the bar width.
        let mid_spans = render_usage_bar(
            48.0,
            10,
            ColorToken::Ok.into(),
            &tc_cfg,
            BarStyle::Parallelogram,
        );
        assert!(
            mid_spans.len() > 3,
            "48% Ok bar must emit multi-stop gradient spans: {mid_spans:?}"
        );
        assert_eq!(mid_spans[0].color, ColorSpec::Rgb(166, 227, 161));
        assert_ne!(
            mid_spans[mid_spans.len() - 2].color,
            ColorSpec::Rgb(166, 227, 161)
        );

        // Live ANSI and shared Ratatui preview agree on exact RGB start/end colors.
        let frame = RenderedFrame {
            lines: vec![StyledLine::new(vec![StyledItem::new(
                crate::config::ItemId::Context,
                spans,
            )])],
            config_broken: false,
            slot_outcomes: vec![],
        };
        let ansi = emit_ansi(&frame, &tc_cfg);
        assert!(ansi.contains("\x1b[0;38;2;166;227;161m▰"));
        assert!(ansi.contains("\x1b[0;38;2;243;139;168m▰"));
        let tui_lines = rendered_frame_to_ratatui_lines(&frame, &tc_cfg);
        assert_eq!(
            tui_lines[0].spans[1].style.fg,
            Some(ratatui::style::Color::Rgb(166, 227, 161))
        );
        let crit_cell_span = tui_lines[0]
            .spans
            .iter()
            .find(|s| s.style.fg == Some(ratatui::style::Color::Rgb(243, 139, 168)));
        assert!(crit_cell_span.is_some());

        // Ansi256 quantizes each cell and coalesces identical adjacent indices.
        let mut doc256 = doc.clone();
        doc256.color_depth = crate::config::ColorDepth::Ansi256;
        let cfg256 = EffectiveConfig::from_doc(&doc256, None, &BTreeMap::new(), false);
        let spans256 = render_usage_bar(
            90.0,
            10,
            ColorToken::Crit.into(),
            &cfg256,
            BarStyle::Parallelogram,
        );
        let joined256: String = spans256.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined256, "▰▰▰▰▰▰▰▰▰▱");
        assert!(
            spans256[..spans256.len() - 1]
                .iter()
                .all(|s| matches!(s.color, ColorSpec::Ansi256(_)))
        );
        let frame256 = RenderedFrame {
            lines: vec![StyledLine::new(vec![StyledItem::new(
                crate::config::ItemId::Context,
                spans256,
            )])],
            config_broken: false,
            slot_outcomes: vec![],
        };
        let ansi256 = emit_ansi(&frame256, &cfg256);
        assert!(ansi256.contains(";38;5;"));
        assert!(!ansi256.contains(";38;2;"));

        // Ansi16 degrades gradient to flat single-color fill_col (2 spans: filled + empty).
        let mut doc16 = doc.clone();
        doc16.color_depth = crate::config::ColorDepth::Ansi16;
        let cfg16 = EffectiveConfig::from_doc(&doc16, None, &BTreeMap::new(), false);
        let spans16 = render_usage_bar(
            90.0,
            10,
            ColorToken::Crit.into(),
            &cfg16,
            BarStyle::Parallelogram,
        );
        assert_eq!(spans16.len(), 2);
        assert_eq!(spans16[0].color, ColorSpec::Token(ColorToken::Crit));
        assert_eq!(spans16[0].text, "▰▰▰▰▰▰▰▰▰");
        assert_eq!(spans16[1].text, "▱");

        // Solid style preserves track background across gradient cells.
        let solid_grad =
            render_usage_bar(80.0, 10, ColorToken::Crit.into(), &tc_cfg, BarStyle::Solid);
        assert!(solid_grad.len() > 2);
        assert!(solid_grad.iter().all(|s| s.style.bg.is_some()));
    }
}
