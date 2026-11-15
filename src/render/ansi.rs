use crate::config::{ColorDepth, EffectiveConfig};
use crate::render::{ColorToken, RenderedFrame, palette::terminal_sgr};

pub fn emit_ansi(frame: &RenderedFrame, config: &EffectiveConfig) -> String {
    match config.color_depth {
        ColorDepth::None => emit_plain(frame, config),
        ColorDepth::Auto | ColorDepth::TrueColor | ColorDepth::Ansi256 | ColorDepth::Ansi16 => {
            if frame.lines.is_empty() {
                return String::new();
            }
            let sep_sgr = terminal_sgr(ColorToken::Separator, config.polarity);
            let mut out = String::with_capacity(256);
            let mut emitted_rows = 0usize;
            for line in &frame.lines {
                if line.items.is_empty() {
                    continue;
                }
                out.push_str(&config.indent);
                out.push_str("\x1b[0m");
                for (idx, item) in line.items.iter().enumerate() {
                    if idx > 0 {
                        out.push_str(sep_sgr);
                        out.push_str(&config.separator);
                    }
                    for span in &item.spans {
                        out.push_str(terminal_sgr(span.color, config.polarity));
                        out.push_str(&span.text);
                    }
                }
                if frame.config_broken && emitted_rows == 0 {
                    out.push_str(terminal_sgr(ColorToken::Crit, config.polarity));
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
        if line.items.is_empty() {
            continue;
        }
        out.push_str(&config.indent);
        for (idx, item) in line.items.iter().enumerate() {
            if idx > 0 {
                out.push_str(&config.separator);
            }
            for span in &item.spans {
                out.push_str(&span.text);
            }
        }
        if frame.config_broken && emitted_rows == 0 {
            out.push_str(" ! config");
        }
        out.push('\n');
        emitted_rows += 1;
    }
    out
}
