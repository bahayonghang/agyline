use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId, ItemSlot};
use crate::items::{
    EvaluatedItem, coalesced_icon_body_spans, parse_color_spec, slot_custom_icon_color,
    slot_fg_color, truncate_chars_with_ellipsis, workspace::format_pretty_model_slug,
};
use crate::render::{ColorSpec, ColorToken, StyledSpan, glyphs};

pub fn eval_mode(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let force_compact = slot.compact.unwrap_or(false);
    let custom_fg = slot.fg.as_deref().and_then(parse_color_spec);

    if payload.tool_confirmation_pending {
        let warn_pfx = slot
            .icon
            .as_deref()
            .unwrap_or_else(|| glyphs::warn_mark(config.glyphs));
        let full_head = if warn_pfx.is_empty() {
            "approval required".to_string()
        } else {
            format!("{warn_pfx} approval required")
        };
        let comp_head = if warn_pfx.is_empty() {
            "confirm".to_string()
        } else {
            format!("{warn_pfx} confirm")
        };
        let compact_spans = vec![StyledSpan::new(ColorToken::Warn, comp_head)];
        let full_spans = if force_compact {
            compact_spans.clone()
        } else {
            vec![
                StyledSpan::new(ColorToken::Warn, full_head),
                StyledSpan::new(ColorToken::Muted, " (waiting for tool confirmation)"),
            ]
        };
        return Some(EvaluatedItem {
            id: ItemId::Mode,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans,
        });
    }

    let chev = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::mode_chevrons(config.glyphs));
    let with_chev = |txt: &str| -> String {
        if chev.is_empty() {
            txt.to_string()
        } else {
            format!("{chev} {txt}")
        }
    };

    let low_mode = payload.exec_mode.to_ascii_lowercase();
    let matched: Option<(&str, &str, ColorToken)> = if !low_mode.is_empty() {
        if low_mode.contains("yolo") {
            Some(("bypass permissions on", "bypass", ColorToken::Auto))
        } else if low_mode.contains("auto") {
            Some(("auto mode on", "auto", ColorToken::Auto))
        } else if low_mode.contains("accept-edits") {
            Some(("accept edits on", "accept-edits", ColorToken::Context))
        } else if low_mode.contains("plan") {
            Some(("plan mode on", "plan", ColorToken::Model))
        } else if low_mode.contains("best-of-n") || low_mode.contains("bestn") {
            Some(("best-of-n mode on", "best-of-n", ColorToken::Git))
        } else {
            None
        }
    } else {
        None
    };

    if let Some((full_txt, comp_txt, default_tok)) = matched {
        let mode_col: ColorSpec = custom_fg.unwrap_or(ColorSpec::Token(default_tok));
        let compact_spans = vec![StyledSpan::new(mode_col, with_chev(comp_txt))];
        let full_spans = if force_compact {
            compact_spans.clone()
        } else {
            vec![
                StyledSpan::new(mode_col, with_chev(full_txt)),
                StyledSpan::new(ColorToken::Muted, " (shift+tab to cycle)"),
            ]
        };
        return Some(EvaluatedItem {
            id: ItemId::Mode,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans,
        });
    }

    let show_default = config
        .force_show_default_mode
        .unwrap_or_else(|| slot.show_default.unwrap_or(config.show_default_mode));
    if show_default {
        let ask_col: ColorSpec = custom_fg.unwrap_or(ColorSpec::Token(ColorToken::Muted));
        let compact_spans = vec![StyledSpan::new(ask_col, with_chev("ask"))];
        let full_spans = if force_compact {
            compact_spans.clone()
        } else {
            vec![StyledSpan::new(
                ask_col,
                with_chev("ask mode (shift+tab to cycle)"),
            )]
        };
        return Some(EvaluatedItem {
            id: ItemId::Mode,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans,
        });
    }

    None
}

pub fn eval_single_line_mode_pill(
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    if payload.tool_confirmation_pending {
        let warn = glyphs::warn_mark(config.glyphs);
        let spans = vec![StyledSpan::new(ColorToken::Warn, format!("{warn} confirm"))];
        return Some(EvaluatedItem {
            id: ItemId::Mode,
            priority: 1,
            full_spans: spans.clone(),
            compact_spans: spans,
        });
    }

    if payload.exec_mode.is_empty() {
        return None;
    }

    let low_mode = payload.exec_mode.to_ascii_lowercase();
    if low_mode == "default" || low_mode == "request-review" || low_mode == "ask" {
        return None;
    }

    let chev = glyphs::mode_chevrons(config.glyphs);
    let label = if low_mode.contains("best-of-n") || low_mode.contains("bestn") {
        "best-of-n"
    } else {
        low_mode.as_str()
    };
    let spans = vec![StyledSpan::new(ColorToken::Auto, format!("{chev} {label}"))];
    Some(EvaluatedItem {
        id: ItemId::Mode,
        priority: 1,
        full_spans: spans.clone(),
        compact_spans: spans,
    })
}

pub fn eval_subagents(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    if payload.subagents_total == 0 {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::subagents_icon(config.glyphs));
    let default_tok = if payload.subagents_running > 0 {
        ColorToken::Model
    } else {
        ColorToken::Muted
    };
    let body_col = slot_fg_color(slot, default_tok);
    let custom_ic = slot_custom_icon_color(slot);

    let (full_core, comp_core) = if payload.subagents_running > 0 {
        (
            format!(
                "{}/{} agents running",
                payload.subagents_running, payload.subagents_total
            ),
            format!(
                "{}/{} running",
                payload.subagents_running, payload.subagents_total
            ),
        )
    } else {
        let s = format!("{} agents", payload.subagents_total);
        (s.clone(), s)
    };

    let mut full_spans = coalesced_icon_body_spans(icon, custom_ic, body_col, full_core);
    let compact_spans = coalesced_icon_body_spans(icon, custom_ic, body_col, comp_core);

    if slot.show_detail.unwrap_or(false) && payload.subagents_running > 0 {
        let dot = glyphs::dot_sep(config.glyphs);
        let mut active_labels = Vec::new();
        for sub in &payload.subagents {
            let is_running = sub.status.eq_ignore_ascii_case("running")
                || sub.status.eq_ignore_ascii_case("working")
                || sub.status.eq_ignore_ascii_case("thinking");
            if !is_running || sub.name.trim().is_empty() {
                continue;
            }
            let label = if sub.model.trim().is_empty() {
                sub.name.trim().to_string()
            } else {
                format!(
                    "{}{dot}{}",
                    sub.name.trim(),
                    format_pretty_model_slug(&sub.model)
                )
            };
            active_labels.push(label);
        }
        if !active_labels.is_empty() {
            let shown: Vec<&str> = active_labels.iter().take(2).map(String::as_str).collect();
            let extra = if active_labels.len() > 2 {
                format!(", +{}", active_labels.len() - 2)
            } else {
                String::new()
            };
            full_spans.push(StyledSpan::new(
                ColorToken::Muted,
                format!(" ({}{extra})", shown.join(", ")),
            ));
        }
    }

    Some(EvaluatedItem {
        id: ItemId::Subagents,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

pub fn eval_tasks(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let tasks = frame.payload.task_count;
    if tasks <= 0 {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::tasks_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Warn);
    let custom_ic = slot_custom_icon_color(slot);
    let word = if tasks == 1 { "task" } else { "tasks" };
    let spans = coalesced_icon_body_spans(icon, custom_ic, body_col, format!("{tasks} {word}"));
    Some(EvaluatedItem {
        id: ItemId::Tasks,
        priority: slot.effective_priority(),
        full_spans: spans.clone(),
        compact_spans: spans,
    })
}

pub fn eval_queue(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let pending = frame.payload.pending_input_count;
    if pending <= 0 {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::queue_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Context);
    let custom_ic = slot_custom_icon_color(slot);
    let spans = coalesced_icon_body_spans(icon, custom_ic, body_col, format!("{pending} queued"));
    Some(EvaluatedItem {
        id: ItemId::Queue,
        priority: slot.effective_priority(),
        full_spans: spans.clone(),
        compact_spans: spans,
    })
}

pub fn eval_sandbox(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    if !frame.payload.sandbox_enabled {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::sandbox_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Model);
    let custom_ic = slot_custom_icon_color(slot);
    let tag = if frame.payload.sandbox_allow_network {
        "sandbox+net"
    } else {
        "sandbox"
    };
    let spans = coalesced_icon_body_spans(icon, custom_ic, body_col, tag.to_string());
    Some(EvaluatedItem {
        id: ItemId::Sandbox,
        priority: slot.effective_priority(),
        full_spans: spans.clone(),
        compact_spans: spans,
    })
}

pub fn eval_vim(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let raw = frame.payload.vim_mode.trim();
    if raw.is_empty() {
        return None;
    }
    let low = raw.to_ascii_lowercase();
    if matches!(low.as_str(), "off" | "none" | "false" | "disabled") {
        return None;
    }
    let upper = raw.to_ascii_uppercase();
    let default_tok = if upper.starts_with("INSERT") {
        ColorToken::Model
    } else if upper.starts_with("VISUAL") || upper.starts_with("V-") || upper.starts_with("SELECT")
    {
        ColorToken::Context
    } else if upper.starts_with("REPLACE") {
        ColorToken::Warn
    } else {
        ColorToken::Ok
    };
    let body_col = slot_fg_color(slot, default_tok);
    let custom_ic = slot_custom_icon_color(slot);
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::vim_icon(config.glyphs));
    let compact_body = if !icon.is_empty()
        && let Some(first_ch) = upper.chars().next()
    {
        first_ch.to_string()
    } else {
        upper.clone()
    };

    Some(EvaluatedItem {
        id: ItemId::Vim,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, upper),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, compact_body),
    })
}

pub fn eval_title(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let cleaned = sanitize_single_line(&frame.payload.conversation_title);
    if cleaned.is_empty() {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::title_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Muted);
    let custom_ic = slot_custom_icon_color(slot);
    let full_txt = truncate_chars_with_ellipsis(&cleaned, 32, config.glyphs);
    let comp_txt = truncate_chars_with_ellipsis(&cleaned, 16, config.glyphs);

    Some(EvaluatedItem {
        id: ItemId::Title,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, full_txt),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, comp_txt),
    })
}

pub fn eval_agent(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let cleaned = sanitize_single_line(&frame.payload.agent_name);
    if cleaned.is_empty() || cleaned.eq_ignore_ascii_case("default") {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::agent_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Model);
    let custom_ic = slot_custom_icon_color(slot);
    let comp_txt = truncate_chars_with_ellipsis(&cleaned, 12, config.glyphs);

    Some(EvaluatedItem {
        id: ItemId::Agent,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, cleaned),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, comp_txt),
    })
}

pub fn eval_artifacts(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let count = frame.payload.artifact_count;
    if count <= 0 {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::artifacts_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Git);
    let custom_ic = slot_custom_icon_color(slot);
    let word = if count == 1 { "artifact" } else { "artifacts" };
    let full_body = format!("{count} {word}");
    let comp_body = if icon.is_empty() {
        format!("{count} art")
    } else {
        count.to_string()
    };

    Some(EvaluatedItem {
        id: ItemId::Artifacts,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, full_body),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, comp_body),
    })
}

pub fn eval_version(
    slot: &ItemSlot,
    frame: &FrameData,
    _config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let cleaned = sanitize_single_line(&frame.payload.version);
    let stripped = cleaned
        .strip_prefix('v')
        .or_else(|| cleaned.strip_prefix('V'))
        .unwrap_or(&cleaned)
        .trim();
    if stripped.is_empty() {
        return None;
    }
    let icon = slot.icon.as_deref().unwrap_or("");
    let body_col = slot_fg_color(slot, ColorToken::Muted);
    let custom_ic = slot_custom_icon_color(slot);
    let body = format!("v{stripped}");
    let spans = coalesced_icon_body_spans(icon, custom_ic, body_col, body);

    Some(EvaluatedItem {
        id: ItemId::Version,
        priority: slot.effective_priority(),
        full_spans: spans.clone(),
        compact_spans: spans,
    })
}

pub fn eval_plan(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let cleaned = sanitize_single_line(&frame.payload.plan_tier);
    if cleaned.is_empty()
        || cleaned.eq_ignore_ascii_case("none")
        || cleaned.eq_ignore_ascii_case("unknown")
    {
        return None;
    }
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::plan_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Context);
    let custom_ic = slot_custom_icon_color(slot);
    let comp_txt = truncate_chars_with_ellipsis(&cleaned, 10, config.glyphs);

    Some(EvaluatedItem {
        id: ItemId::Plan,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, cleaned),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, comp_txt),
    })
}

fn sanitize_single_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_space = true;
    for ch in raw.chars() {
        if ch.is_control() || ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}
