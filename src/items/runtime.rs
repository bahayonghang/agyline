//! Runtime and session segments. Mode labels, the shift+tab hint and the subagent/task/queue/
//! sandbox wording reproduce `ref/agy-patch/statusline.ps1` line 2; an empty item returns `None`
//! so it disappears with its separator. Vim, title, agent, artifacts, version and plan have no
//! reference counterpart and are agyline additions.

use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId, ItemSlot};
use crate::items::{
    EvaluatedItem, coalesced_icon_body_spans, labeled_slot_icon, parse_color_spec,
    slot_custom_icon_color, slot_fg_color, truncate_chars_with_ellipsis,
    workspace::format_pretty_model_slug,
};
use crate::render::{ColorSpec, ColorToken, StyledSpan, TextStyle, glyphs};

/// Execution-mode pill. A pending tool confirmation outranks every mode, and an unrecognized mode
/// is suppressed unless `AGY_SHOW_DEFAULT_MODE` or the slot's `show_default` asks for `ask`.
pub fn eval_mode(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    if slot
        .style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("badge"))
    {
        return eval_mode_badge(slot, frame, config);
    }
    let payload = &frame.payload;
    let force_compact = slot.compact.unwrap_or(false);
    let custom_fg = slot.fg.as_deref().and_then(parse_color_spec);

    // The reference replaces the mode with the approval banner while a confirmation is pending.
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
    // First match wins, in the reference's order: yolo, auto, accept-edits, plan, best-of-n.
    let matched: Option<(&str, &str, ColorToken)> = if !low_mode.is_empty() {
        if low_mode.contains("yolo") {
            Some(("bypass permissions on", "bypass", ColorToken::Auto))
        } else if low_mode.contains("auto") {
            Some(("auto mode on", "auto", ColorToken::Auto))
        } else if low_mode.contains("accept-edits") {
            Some(("accept edits on", "accept-edits", ColorToken::Context))
        } else if low_mode.contains("plan") {
            Some(("plan mode on", "plan", ColorToken::Model))
        // `bestn` is an agyline alias; both references match only `best-of-n`.
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
        // Full wording and the dim hint are the reference's; the compact pill drops the hint.
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

    // Env `AGY_SHOW_DEFAULT_MODE` outranks the slot option, so `=1` cannot be disabled per slot.
    let show_default = config
        .force_show_default_mode
        .unwrap_or_else(|| slot.show_default.unwrap_or(config.show_default_mode));
    // Deliberate divergence: both scripts gate `ask` behind a non-empty mode, but agy 1.5.9 has
    // no `execution_mode` and omits `cycle_mode` until the first shift+tab, which would make
    // `AGY_SHOW_DEFAULT_MODE=1` a no-op. Pinned by golden fixture I03_empty_mode_show_default.
    if show_default {
        let ask_col: ColorSpec = custom_fg.unwrap_or(ColorSpec::Token(ColorToken::Muted));
        // Reference default-mode wording: `ask mode (shift+tab to cycle)`, compact just `ask`.
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

fn eval_mode_badge(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let custom_fg = slot.fg.as_deref().and_then(parse_color_spec);
    let low_mode = payload.exec_mode.trim().to_ascii_lowercase();
    let show_default = config
        .force_show_default_mode
        .unwrap_or_else(|| slot.show_default.unwrap_or(config.show_default_mode));

    let badge: Option<(String, ColorToken)> = if low_mode.is_empty() {
        show_default.then(|| ("ASK".to_string(), ColorToken::Muted))
    } else if low_mode.contains("yolo") {
        Some(("BYPASS".to_string(), ColorToken::Crit))
    } else if low_mode.contains("auto") {
        Some(("AUTO".to_string(), ColorToken::Crit))
    } else if low_mode.contains("accept-edits") {
        Some(("EDITS".to_string(), ColorToken::Warn))
    } else if low_mode.contains("plan") {
        Some(("PLAN".to_string(), ColorToken::Git))
    } else if low_mode.contains("best-of-n") || low_mode.contains("bestn") {
        Some(("BEST-OF-N".to_string(), ColorToken::Git))
    } else if low_mode == "default" || low_mode == "ask" || low_mode == "request-review" {
        show_default.then(|| ("ASK".to_string(), ColorToken::Muted))
    } else {
        Some((low_mode.to_ascii_uppercase(), ColorToken::Separator))
    };

    let vim_tag = normalize_vim_tag(&payload.vim_mode);
    if badge.is_none() && vim_tag.is_none() {
        return None;
    }

    let bold = TextStyle::PLAIN.bold();
    let icon = slot.icon.as_deref().unwrap_or("");
    let custom_ic = slot_custom_icon_color(slot);

    let mut full_spans = Vec::with_capacity(3);
    let mut compact_spans = Vec::with_capacity(2);
    if let Some((label, tok)) = badge {
        let col = custom_fg.unwrap_or(ColorSpec::Token(tok));
        let prefix = if icon.is_empty() {
            String::new()
        } else {
            format!("{icon} ")
        };
        if let Some(ic) = custom_ic
            && !icon.is_empty()
            && ic != col
        {
            full_spans.push(StyledSpan::styled(ic, bold, icon));
            full_spans.push(StyledSpan::styled(col, bold, format!(" {label}")));
            compact_spans.push(StyledSpan::styled(ic, bold, icon));
            compact_spans.push(StyledSpan::styled(col, bold, format!(" {label}")));
        } else {
            let txt = format!("{prefix}{label}");
            full_spans.push(StyledSpan::styled(col, bold, txt.clone()));
            compact_spans.push(StyledSpan::styled(col, bold, txt));
        }
        if let Some(tag) = vim_tag {
            full_spans.push(StyledSpan::new(ColorToken::Model, format!(" [{tag}]")));
        }
    } else if let Some(tag) = vim_tag {
        let s = StyledSpan::new(ColorToken::Model, format!("[{tag}]"));
        full_spans.push(s.clone());
        compact_spans.push(s);
    }

    Some(EvaluatedItem {
        id: ItemId::Mode,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

fn normalize_vim_tag(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    match upper.as_str() {
        "OFF" | "NONE" | "FALSE" | "DISABLED" => None,
        "NORMAL" => Some("N"),
        "INSERT" => Some("I"),
        "VISUAL" => Some("V"),
        "VISUAL LINE" | "V-LINE" => Some("VL"),
        "VISUAL BLOCK" | "V-BLOCK" => Some("VB"),
        "REPLACE" => Some("R"),
        _ => Some("VIM"),
    }
}

/// Mode pill for `AGY_STATUSLINE_LINES=1`, where line 2 collapses into line 1. Layout adds it only
/// when the dropped row had items and row 1 has no mode slot. `None` for the default/ask modes;
/// priority 1 makes it the last item that width narrowing drops.
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
    // The reference suppresses `default` and `request-review`; `ask` is suppressed here too.
    if low_mode == "default" || low_mode == "request-review" || low_mode == "ask" {
        return None;
    }

    let chev = glyphs::mode_chevrons(config.glyphs);
    // The reference prints the raw lowercased mode here; only `best-of-n` is normalized.
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

/// Subagent counts; `None` when the frame lists none. Running uses the reference's three active
/// statuses, and the compact form drops the word `agents`.
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
    // Reference colors running subagents as model cyan and idle ones dim.
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
            // Same active statuses as the count in `input.rs`. `-in` is case-insensitive in
            // `statusline.ps1`, while `statusline.py` compares case-sensitively.
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
            // Detail names are an agyline addition: two at most, then `+N` for the rest.
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

/// Active background tasks; `None` at zero. Singular `task` versus `tasks` is reference wording.
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

/// Queued user input; `None` at zero, so an idle queue leaves no segment behind.
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

/// Sandbox state; `None` unless the sandbox is enabled or `show_when_off = true`.
pub fn eval_sandbox(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let show_when_off = slot.show_when_off.unwrap_or(false);
    if !frame.payload.sandbox_enabled && !show_when_off {
        return None;
    }
    let custom_ic = slot_custom_icon_color(slot);

    if show_when_off {
        let icon = labeled_slot_icon(slot, config.glyphs, glyphs::sandbox_icon);
        if frame.payload.sandbox_enabled {
            let val_col = slot_fg_color(slot, ColorToken::Ok);
            let on_txt = if frame.payload.sandbox_allow_network {
                "ON+net"
            } else {
                "ON"
            };
            let bold = TextStyle::PLAIN.bold();
            let mut full_spans = Vec::with_capacity(3);
            let mut compact_spans = Vec::with_capacity(3);
            if !icon.is_empty() {
                let ic_col = custom_ic.unwrap_or(val_col);
                full_spans.push(StyledSpan::new(ic_col, format!("{icon} ")));
                compact_spans.push(StyledSpan::new(ic_col, format!("{icon} ")));
            }
            full_spans.push(StyledSpan::new(ColorToken::Muted, "sandbox "));
            compact_spans.push(StyledSpan::new(ColorToken::Muted, "sbx "));
            full_spans.push(StyledSpan::styled(val_col, bold, on_txt));
            compact_spans.push(StyledSpan::styled(val_col, bold, on_txt));
            return Some(EvaluatedItem {
                id: ItemId::Sandbox,
                priority: slot.effective_priority(),
                full_spans,
                compact_spans,
            });
        }
        return Some(EvaluatedItem {
            id: ItemId::Sandbox,
            priority: slot.effective_priority(),
            full_spans: coalesced_icon_body_spans(
                icon,
                custom_ic,
                ColorToken::Muted.into(),
                "sandbox off".to_string(),
            ),
            compact_spans: coalesced_icon_body_spans(
                icon,
                custom_ic,
                ColorToken::Muted.into(),
                "sbx off".to_string(),
            ),
        });
    }

    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::sandbox_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Model);
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

/// Vim input mode, uppercased; `None` when blank or an off/none/false/disabled value. No reference
/// counterpart: the colors and the compact single letter are agyline's.
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

/// Conversation title flattened to one line; `None` when empty. The 32/16-char widths are
/// agyline's, not the reference's.
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

/// Custom agent name; `None` when blank or `default`, which carries no information on the line.
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

/// Session artifact count; `None` at zero. Compact drops the word when an icon already marks it.
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

/// Host CLI version, always re-prefixed with `v`; `None` when blank. No default icon and no
/// reference counterpart.
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

/// Account plan tier; `None` for blank, `none` or `unknown`, which are not worth a segment.
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

/// Compact agent `Status Badge` (`READY`, `WORKING`, `TOOL`, `CONFIRM`, plus pending input `(+N)`).
pub fn eval_status(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let bold = TextStyle::PLAIN.bold();
    let custom_ic = slot_custom_icon_color(slot);

    if slot
        .style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("dot"))
    {
        if payload.tool_confirmation_pending {
            let ic = slot
                .icon
                .as_deref()
                .unwrap_or_else(|| glyphs::warn_mark(config.glyphs));
            let text = if ic.is_empty() {
                "CONFIRM".to_string()
            } else {
                format!("{ic} CONFIRM")
            };
            let spans = vec![StyledSpan::styled(ColorToken::Warn, bold, text)];
            return Some(EvaluatedItem {
                id: ItemId::Status,
                priority: slot.effective_priority(),
                full_spans: spans.clone(),
                compact_spans: spans,
            });
        }
        let state_low = payload.agent_state.trim().to_ascii_lowercase();
        let (lbl, tok) = match state_low.as_str() {
            "" | "idle" => ("idle".to_string(), ColorToken::Muted),
            "thinking" => ("thinking".to_string(), ColorToken::Model),
            "working" => ("working".to_string(), ColorToken::Ok),
            "tool_use" => ("tool_use".to_string(), ColorToken::Cache),
            "error" => ("error".to_string(), ColorToken::Crit),
            "reviewing" => ("reviewing".to_string(), ColorToken::Warn),
            "authenticating" => ("auth".to_string(), ColorToken::Muted),
            "initializing" => ("init".to_string(), ColorToken::Muted),
            other => {
                let clean = sanitize_single_line(other);
                if clean.is_empty() {
                    ("idle".to_string(), ColorToken::Muted)
                } else {
                    (clean, ColorToken::Muted)
                }
            }
        };
        let default_ic = glyphs::status_dot_icon(config.glyphs, state_low.as_str());
        let icon = slot.icon.as_deref().unwrap_or(default_ic);
        let body_col = slot_fg_color(slot, tok);
        let mut full_spans = if icon.is_empty() {
            vec![StyledSpan::new(body_col, lbl.clone())]
        } else if let Some(ic) = custom_ic
            && ic != body_col
        {
            vec![
                StyledSpan::new(ic, icon),
                StyledSpan::new(body_col, format!(" {lbl}")),
            ]
        } else {
            vec![StyledSpan::new(body_col, format!("{icon} {lbl}"))]
        };
        if slot.show_pending.unwrap_or(true) && payload.pending_input_count > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Warn,
                format!(" (+{})", payload.pending_input_count),
            ));
        }
        let compact_spans = if icon.is_empty() {
            vec![StyledSpan::new(body_col, lbl)]
        } else {
            vec![StyledSpan::new(custom_ic.unwrap_or(body_col), icon)]
        };
        return Some(EvaluatedItem {
            id: ItemId::Status,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans,
        });
    }

    let (default_icon, full_lbl, comp_lbl, body_col): (&str, String, String, ColorSpec) =
        if payload.tool_confirmation_pending {
            (
                glyphs::warn_mark(config.glyphs),
                "CONFIRM".to_string(),
                "CONF".to_string(),
                ColorToken::Warn.into(),
            )
        } else {
            let state_low = payload.agent_state.trim().to_ascii_lowercase();
            let (ic, full, comp, tok) = match state_low.as_str() {
                "" | "idle" => (
                    glyphs::status_ready_icon(config.glyphs),
                    "READY".to_string(),
                    "READY".to_string(),
                    ColorToken::Ok,
                ),
                "working" => (
                    glyphs::status_working_icon(config.glyphs),
                    "WORKING".to_string(),
                    "WORK".to_string(),
                    ColorToken::Model,
                ),
                "tool_use" => (
                    glyphs::status_tool_icon(config.glyphs),
                    "TOOL".to_string(),
                    "TOOL".to_string(),
                    ColorToken::Context,
                ),
                "reviewing" => (
                    glyphs::status_review_icon(config.glyphs),
                    "REVIEW".to_string(),
                    "REVIEW".to_string(),
                    ColorToken::Warn,
                ),
                "error" => (
                    glyphs::status_error_icon(config.glyphs),
                    "ERROR".to_string(),
                    "ERROR".to_string(),
                    ColorToken::Crit,
                ),
                "thinking" => (
                    glyphs::status_thinking_icon(config.glyphs),
                    "THINKING".to_string(),
                    "THINK".to_string(),
                    ColorToken::Warn,
                ),
                "authenticating" => (
                    glyphs::status_wait_icon(config.glyphs),
                    "AUTH".to_string(),
                    "AUTH".to_string(),
                    ColorToken::Separator,
                ),
                "initializing" => (
                    glyphs::status_wait_icon(config.glyphs),
                    "INIT".to_string(),
                    "INIT".to_string(),
                    ColorToken::Separator,
                ),
                other => {
                    let up = sanitize_single_line(other).to_ascii_uppercase();
                    let label = if up.is_empty() {
                        "READY".to_string()
                    } else {
                        up
                    };
                    (
                        glyphs::status_wait_icon(config.glyphs),
                        label.clone(),
                        label,
                        ColorToken::Separator,
                    )
                }
            };
            (ic, full, comp, slot_fg_color(slot, tok))
        };

    let icon = slot.icon.as_deref().unwrap_or(default_icon);
    let build_badge = |lbl: String| -> Vec<StyledSpan> {
        if icon.is_empty() {
            vec![StyledSpan::styled(body_col, bold, lbl)]
        } else if let Some(ic) = custom_ic
            && ic != body_col
        {
            vec![
                StyledSpan::styled(ic, bold, icon),
                StyledSpan::styled(body_col, bold, format!(" {lbl}")),
            ]
        } else {
            vec![StyledSpan::styled(body_col, bold, format!("{icon} {lbl}"))]
        }
    };

    let mut full_spans = build_badge(full_lbl);
    let compact_spans = build_badge(comp_lbl);

    if slot.show_pending.unwrap_or(true) && payload.pending_input_count > 0 {
        full_spans.push(StyledSpan::new(
            ColorToken::Warn,
            format!(" (+{})", payload.pending_input_count),
        ));
    }

    Some(EvaluatedItem {
        id: ItemId::Status,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

/// Elapsed session duration from bounded transcript start time or `first_seen_sec`.
pub fn eval_duration(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let elapsed_secs = frame
        .session_start_sec
        .filter(|&s| s > 0 && frame.now_sec >= s)
        .map_or(0, |s| frame.now_sec - s);
    let mins = elapsed_secs / 60;
    let body = if mins >= 1440 {
        format!("{}d{}h", mins / 1440, (mins % 1440) / 60)
    } else if mins >= 60 {
        format!("{}h{}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    };
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::duration_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Muted);
    let custom_ic = slot_custom_icon_color(slot);
    let spans = coalesced_icon_body_spans(icon, custom_ic, body_col, body);
    Some(EvaluatedItem {
        id: ItemId::Duration,
        priority: slot.effective_priority(),
        full_spans: spans.clone(),
        compact_spans: spans,
    })
}

/// Collapses control characters and whitespace runs into single spaces so a title or agent name
/// cannot break the row or inject ANSI escapes. Leading and trailing spaces are dropped.
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
