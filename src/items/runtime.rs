use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId};
use crate::render::{ColorToken, StyledItem, StyledSpan, glyphs};

pub fn eval_mode(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let payload = &frame.payload;
    let chev = glyphs::mode_chevrons(config.glyphs);

    if payload.tool_confirmation_pending {
        let warn = glyphs::warn_mark(config.glyphs);
        return Some(StyledItem::new(
            ItemId::Mode,
            vec![
                StyledSpan::new(ColorToken::Warn, format!("{warn} approval required")),
                StyledSpan::new(ColorToken::Muted, " (waiting for tool confirmation)"),
            ],
        ));
    }

    let low_mode = payload.exec_mode.to_ascii_lowercase();
    if !low_mode.is_empty() {
        if low_mode.contains("yolo") {
            return Some(StyledItem::new(
                ItemId::Mode,
                vec![
                    StyledSpan::new(ColorToken::Auto, format!("{chev} bypass permissions on")),
                    StyledSpan::new(ColorToken::Muted, " (shift+tab to cycle)"),
                ],
            ));
        }
        if low_mode.contains("auto") {
            return Some(StyledItem::new(
                ItemId::Mode,
                vec![
                    StyledSpan::new(ColorToken::Auto, format!("{chev} auto mode on")),
                    StyledSpan::new(ColorToken::Muted, " (shift+tab to cycle)"),
                ],
            ));
        }
        if low_mode.contains("accept-edits") {
            return Some(StyledItem::new(
                ItemId::Mode,
                vec![
                    StyledSpan::new(ColorToken::Context, format!("{chev} accept edits on")),
                    StyledSpan::new(ColorToken::Muted, " (shift+tab to cycle)"),
                ],
            ));
        }
        if low_mode.contains("plan") {
            return Some(StyledItem::new(
                ItemId::Mode,
                vec![
                    StyledSpan::new(ColorToken::Model, format!("{chev} plan mode on")),
                    StyledSpan::new(ColorToken::Muted, " (shift+tab to cycle)"),
                ],
            ));
        }
        if low_mode.contains("best-of-n") || low_mode.contains("bestn") {
            return Some(StyledItem::new(
                ItemId::Mode,
                vec![
                    StyledSpan::new(ColorToken::Git, format!("{chev} best-of-n mode on")),
                    StyledSpan::new(ColorToken::Muted, " (shift+tab to cycle)"),
                ],
            ));
        }
    }

    if config.show_default_mode {
        return Some(StyledItem::new(
            ItemId::Mode,
            vec![StyledSpan::new(
                ColorToken::Muted,
                format!("{chev} ask mode (shift+tab to cycle)"),
            )],
        ));
    }

    None
}

pub fn eval_single_line_mode_pill(
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<StyledItem> {
    let payload = &frame.payload;
    if payload.tool_confirmation_pending {
        let warn = glyphs::warn_mark(config.glyphs);
        return Some(StyledItem::new(
            ItemId::Mode,
            vec![StyledSpan::new(ColorToken::Warn, format!("{warn} confirm"))],
        ));
    }

    if payload.exec_mode.is_empty() {
        return None;
    }

    let low_mode = payload.exec_mode.to_ascii_lowercase();
    if low_mode == "default" || low_mode == "request-review" {
        return None;
    }

    let chev = glyphs::mode_chevrons(config.glyphs);
    let label = if low_mode.contains("best-of-n") || low_mode.contains("bestn") {
        "best-of-n"
    } else {
        low_mode.as_str()
    };
    Some(StyledItem::new(
        ItemId::Mode,
        vec![StyledSpan::new(ColorToken::Auto, format!("{chev} {label}"))],
    ))
}

pub fn eval_subagents(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let payload = &frame.payload;
    if payload.subagents_total == 0 {
        return None;
    }
    let icon = glyphs::subagents_icon(config.glyphs);
    if payload.subagents_running > 0 {
        Some(StyledItem::new(
            ItemId::Subagents,
            vec![StyledSpan::new(
                ColorToken::Model,
                format!(
                    "{icon} {}/{} agents running",
                    payload.subagents_running, payload.subagents_total
                ),
            )],
        ))
    } else {
        Some(StyledItem::new(
            ItemId::Subagents,
            vec![StyledSpan::new(
                ColorToken::Muted,
                format!("{icon} {} agents", payload.subagents_total),
            )],
        ))
    }
}

pub fn eval_tasks(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let tasks = frame.payload.task_count;
    if tasks <= 0 {
        return None;
    }
    let icon = glyphs::tasks_icon(config.glyphs);
    let word = if tasks == 1 { "task" } else { "tasks" };
    Some(StyledItem::new(
        ItemId::Tasks,
        vec![StyledSpan::new(
            ColorToken::Warn,
            format!("{icon} {tasks} {word}"),
        )],
    ))
}

pub fn eval_queue(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let pending = frame.payload.pending_input_count;
    if pending <= 0 {
        return None;
    }
    let icon = glyphs::queue_icon(config.glyphs);
    Some(StyledItem::new(
        ItemId::Queue,
        vec![StyledSpan::new(
            ColorToken::Context,
            format!("{icon} {pending} queued"),
        )],
    ))
}

pub fn eval_sandbox(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    if !frame.payload.sandbox_enabled {
        return None;
    }
    let icon = glyphs::sandbox_icon(config.glyphs);
    let tag = if frame.payload.sandbox_allow_network {
        "sandbox+net"
    } else {
        "sandbox"
    };
    Some(StyledItem::new(
        ItemId::Sandbox,
        vec![StyledSpan::new(ColorToken::Model, format!("{icon} {tag}"))],
    ))
}
