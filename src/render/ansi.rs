use crate::config::EffectiveConfig;
use crate::render::{ColorToken, EffectiveColorDepth, RenderedFrame};

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
            let mut out = String::with_capacity(256);
            let mut emitted_rows = 0usize;
            for line in &frame.lines {
                if line.items.is_empty() && line.broken_suffix.is_none() {
                    continue;
                }
                let indent = line.indent_override.as_deref().unwrap_or(&config.indent);
                out.push_str(indent);
                out.push_str("\x1b[0m");
                for (idx, item) in line.items.iter().enumerate() {
                    if idx > 0 {
                        if let Some(c) = sep_col {
                            c.write_sgr(&mut out);
                        }
                        out.push_str(&config.separator);
                    }
                    for span in &item.spans {
                        if let Some(c) = config.resolve_span_color(span.color) {
                            c.write_sgr(&mut out);
                        }
                        out.push_str(&span.text);
                    }
                }
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

pub fn emit_plain(frame: &RenderedFrame, config: &EffectiveConfig) -> String {
    if frame.lines.is_empty() {
        return String::new();
    }
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
                out.push_str(&config.separator);
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
