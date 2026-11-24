//! Serializes a `RenderedFrame` into the bytes `agy` prints. This is the only frame-to-escape
//! path: the configurator reads the same frame as styled spans instead of parsing these.
//! Both emitters skip rows with nothing to show and badge the first row they actually emit.

use crate::config::{EffectiveConfig, ItemStyle};
use crate::render::{ColorToken, EffectiveColorDepth, RenderedFrame, ResolvedColor, TextStyle};

type SgrKey = (ResolvedColor, TextStyle, Option<ResolvedColor>);

/// Colored output quantized to `config.effective_color_depth`. Depth `None` delegates to
/// `emit_plain`, so a NO_COLOR terminal receives no escapes at all. Every row ends reset + `\n`.
pub fn emit_ansi(frame: &RenderedFrame, config: &EffectiveConfig) -> String {
    match config.effective_color_depth {
        EffectiveColorDepth::None => emit_plain(frame, config),
        EffectiveColorDepth::TrueColor
        | EffectiveColorDepth::Ansi256
        | EffectiveColorDepth::Ansi16 => {
            if frame.lines.is_empty() {
                return String::new();
            }
            let sep_col = config.resolve_span_color(ColorToken::Separator);
            let crit_col = config.resolve_span_color(ColorToken::Crit);
            let row_sep = config.effective_row_separator();
            let framed_mode = config.effective_item_style() != ItemStyle::Minimal;
            let mut out = String::with_capacity(256);
            let mut emitted_rows = 0usize;
            for line in &frame.lines {
                if line.items.is_empty() && line.broken_suffix.is_none() {
                    continue;
                }
                let indent = line.indent_override.as_deref().unwrap_or(&config.indent);
                // Reference row framing: indent, reset, items, reset (statusline.py `r_esc`).
                out.push_str(indent);
                out.push_str("\x1b[0m");
                let mut last_sgr: Option<SgrKey> = None;
                for (idx, item) in line.items.iter().enumerate() {
                    if idx > 0 {
                        if framed_mode {
                            // Reset before the inter-container space so badge backgrounds cannot bleed.
                            out.push_str("\x1b[0m");
                        } else if let Some(c) = sep_col {
                            c.write_sgr(&mut out);
                        }
                        last_sgr = None;
                        out.push_str(row_sep);
                    }
                    for span in &item.spans {
                        let fg = config.resolve_span_color(span.color);
                        let bg = span.style.bg.and_then(|b| config.resolve_span_color(b));
                        let is_plain = span.style.is_plain() && bg.is_none();
                        if let Some(fg_col) = fg {
                            let key = (fg_col, span.style, bg);
                            if is_plain {
                                // Emit plain spans verbatim so `cometix` adjacent plain spans remain byte-exact.
                                fg_col.write_sgr(&mut out);
                                last_sgr = None;
                            } else if last_sgr != Some(key) {
                                fg_col.write_sgr_styled(span.style, bg, &mut out);
                                last_sgr = Some(key);
                            }
                        }
                        out.push_str(&span.text);
                    }
                }
                // The badge lands on the first *emitted* row: earlier rows may have been skipped.
                if let Some(ref badge) = line.broken_suffix {
                    if !badge.is_empty() {
                        if let Some(c) = crit_col {
                            c.write_sgr(&mut out);
                        }
                        out.push_str(badge);
                    }
                } else if frame.config_broken && emitted_rows == 0 {
                    if let Some(c) = crit_col {
                        c.write_sgr(&mut out);
                    }
                    out.push_str(" ! config");
                }
                out.push_str("\x1b[0m\n");
                emitted_rows += 1;
            }
            out
        }
    }
}

/// Escape-free text of the same frame: `preview --plain`, depth `None` via `emit_ansi`, and the
/// plain copy of a live render. Row skipping and badge placement match `emit_ansi` exactly.
pub fn emit_plain(frame: &RenderedFrame, config: &EffectiveConfig) -> String {
    if frame.lines.is_empty() {
        return String::new();
    }
    let row_sep = config.effective_row_separator();
    let mut out = String::with_capacity(192);
    let mut emitted_rows = 0usize;
    for line in &frame.lines {
        if line.items.is_empty() && line.broken_suffix.is_none() {
            continue;
        }
        let indent = line.indent_override.as_deref().unwrap_or(&config.indent);
        out.push_str(indent);
        for (idx, item) in line.items.iter().enumerate() {
            if idx > 0 {
                out.push_str(row_sep);
            }
            for span in &item.spans {
                out.push_str(&span.text);
            }
        }
        if let Some(ref badge) = line.broken_suffix {
            out.push_str(badge);
        } else if frame.config_broken && emitted_rows == 0 {
            out.push_str(" ! config");
        }
        out.push('\n');
        emitted_rows += 1;
    }
    out
}
