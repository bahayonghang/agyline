//! Seam between the shared renderer and ratatui: a `RenderedFrame` plus its `EffectiveConfig`
//! become styled lines directly, never by re-parsing the ANSI that `emit_ansi` produces. Colors
//! arrive already quantized by `resolve_span_color`, so preview and live output agree by
//! construction; `tests/tui_smoke.rs` pins the text against `emit_plain`.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::config::EffectiveConfig;
use crate::render::{Ansi16Color, ColorToken, EffectiveColorDepth, RenderedFrame, ResolvedColor};

/// Maps an already-quantized color to ratatui. `None` means depth `None` and must stay unstyled
/// rather than fall back to a default foreground.
pub fn resolved_color_to_ratatui(color: Option<ResolvedColor>) -> Option<Color> {
    let resolved = color?;
    let mapped = match resolved {
        // ratatui's names are one step off ANSI's: `Gray` is SGR 37 and `White` is bright 97, so
        // this table cannot be collapsed into matching variant names.
        ResolvedColor::Ansi16(c16) => match c16 {
            Ansi16Color::Black => Color::Black,
            Ansi16Color::Red => Color::Red,
            Ansi16Color::Green => Color::Green,
            Ansi16Color::Yellow => Color::Yellow,
            Ansi16Color::Blue => Color::Blue,
            Ansi16Color::Magenta => Color::Magenta,
            Ansi16Color::Cyan => Color::Cyan,
            Ansi16Color::White => Color::Gray,
            Ansi16Color::BrightBlack => Color::DarkGray,
            Ansi16Color::BrightRed => Color::LightRed,
            Ansi16Color::BrightGreen => Color::LightGreen,
            Ansi16Color::BrightYellow => Color::LightYellow,
            Ansi16Color::BrightBlue => Color::LightBlue,
            Ansi16Color::BrightMagenta => Color::LightMagenta,
            Ansi16Color::BrightCyan => Color::LightCyan,
            Ansi16Color::BrightWhite => Color::White,
        },
        ResolvedColor::Ansi256(idx) => Color::Indexed(idx),
        ResolvedColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    };
    Some(mapped)
}

/// Foreground-only style. `None` yields the terminal default, matching `emit_ansi` writing no SGR.
pub fn style_for_resolved(color: Option<ResolvedColor>) -> Style {
    match resolved_color_to_ratatui(color) {
        Some(fg) => Style::default().fg(fg),
        None => Style::default(),
    }
}

/// The preview/live parity seam: converts the very frame the printer emits, span by span, with no
/// ANSI round-trip. Row skipping, indent, separators and the broken-config badge must mirror
/// `emit_ansi` / `emit_plain` exactly, including their empty-row and first-emitted-row rules.
pub fn rendered_frame_to_ratatui_lines(
    frame: &RenderedFrame,
    config: &EffectiveConfig,
) -> Vec<Line<'static>> {
    if frame.lines.is_empty() {
        return Vec::new();
    }

    // Depth None prints no SGR at all, so every span here must stay unstyled too.
    let plain_mode = match config.effective_color_depth {
        EffectiveColorDepth::None => true,
        EffectiveColorDepth::TrueColor
        | EffectiveColorDepth::Ansi256
        | EffectiveColorDepth::Ansi16 => false,
    };

    let sep_style = if plain_mode {
        Style::default()
    } else {
        style_for_resolved(config.resolve_span_color(ColorToken::Separator))
    };
    let crit_style = if plain_mode {
        Style::default()
    } else {
        style_for_resolved(config.resolve_span_color(ColorToken::Crit))
    };

    let mut out = Vec::with_capacity(frame.lines.len());
    let mut emitted_rows = 0usize;

    for line in &frame.lines {
        if line.items.is_empty() && line.broken_suffix.is_none() {
            continue;
        }
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.items.len() * 2 + 2);
        let indent = line.indent_override.as_deref().unwrap_or(&config.indent);
        if !indent.is_empty() {
            spans.push(Span::raw(indent.to_string()));
        }

        for (idx, item) in line.items.iter().enumerate() {
            if idx > 0 && !config.separator.is_empty() {
                spans.push(Span::styled(config.separator.clone(), sep_style));
            }
            for sp in &item.spans {
                if sp.text.is_empty() {
                    continue;
                }
                let sp_style = if plain_mode {
                    Style::default()
                } else {
                    style_for_resolved(config.resolve_span_color(sp.color))
                };
                spans.push(Span::styled(sp.text.clone(), sp_style));
            }
        }

        if let Some(ref badge) = line.broken_suffix {
            if !badge.is_empty() {
                spans.push(Span::styled(badge.clone(), crit_style));
            }
        } else if frame.config_broken && emitted_rows == 0 {
            // First *emitted* row, not row index 0: skipped empty rows must not steal the badge.
            spans.push(Span::styled(" ! config".to_string(), crit_style));
        }

        out.push(Line::from(spans));
        emitted_rows += 1;
    }

    out
}

/// Column ruler under the preview. `sim_width` of `None` means unbounded and gets a fixed legend;
/// a finite width is marked at that column. Output is always clipped to `viewport_cols` chars.
pub fn build_width_ruler(sim_width: Option<usize>, viewport_cols: usize) -> String {
    if viewport_cols == 0 {
        return String::new();
    }
    let Some(cols) = sim_width else {
        let base = "1·······10········20········30········40········50········60········70········80 (∞ unbounded)";
        return base.chars().take(viewport_cols).collect();
    };
    let target = cols.min(viewport_cols);
    if target == 0 {
        return String::new();
    }
    let mut chars: Vec<char> = Vec::with_capacity(target);
    for col in 1..=target {
        if col == cols {
            chars.push('┤');
        } else if col % 10 == 0 {
            let label = col.to_string();
            let label_chars: Vec<char> = label.chars().collect();
            // A multi-digit label overwrites the ticks before it so the ruler stays exactly
            // `target` chars wide instead of growing past the viewport.
            let replace_prev = label_chars.len().saturating_sub(1);
            for _ in 0..replace_prev {
                let _ = chars.pop();
            }
            for ch in label_chars {
                if chars.len() < target {
                    chars.push(ch);
                }
            }
        } else if col % 5 == 0 {
            chars.push('┼');
        } else {
            chars.push('─');
        }
    }
    chars.into_iter().collect()
}
