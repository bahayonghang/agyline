//! Display-width measurement and priority-based narrowing. `overflow = "fit"` shrinks a row to
//! the terminal budget in three stages (compact, drop, grapheme-safe truncate); `"none"` prints
//! it whole. Widths are terminal columns, never bytes or chars, and every decision is recorded
//! per config slot in `SlotOutcome` for the configurator and the narrowing tests.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId, ItemSlot, ItemStyle, OverflowMode};
use crate::items::{EvaluatedItem, runtime::eval_single_line_mode_pill};
use crate::render::{ColorSpec, ColorToken, StyledItem, StyledLine, StyledSpan, glyphs};

const PILL_LEFT_CAP: &str = "\u{e0b6}";
const PILL_RIGHT_CAP: &str = "\u{e0b4}";

/// What narrowing did to one configured slot: `Empty` when the item produced nothing this frame,
/// otherwise `Full` or the stage that changed it (`Compact`, `Dropped`, `Truncated`). Indexed by
/// config row/slot, so the configurator can badge a slot that no longer appears in the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOutcome {
    Empty,
    Full,
    Compact,
    Dropped,
    Truncated,
}

#[derive(Debug, Clone)]
struct ActiveSlot {
    // None for the synthesized single-line mode pill, which has no config slot to report on.
    origin: Option<(usize, usize)>,
    eval: EvaluatedItem,
    use_compact: bool,
    dropped: bool,
    full_w: usize,
    compact_w: usize,
    has_compact_diff: bool,
}

impl ActiveSlot {
    fn from_eval(origin: Option<(usize, usize)>, eval: EvaluatedItem, use_compact: bool) -> Self {
        let has_compact_diff = eval.has_compact_diff();
        Self {
            origin,
            eval,
            use_compact,
            dropped: false,
            full_w: 0,
            compact_w: 0,
            has_compact_diff,
        }
    }

    fn current_spans(&self) -> &[StyledSpan] {
        if self.use_compact {
            &self.eval.compact_spans
        } else {
            &self.eval.full_spans
        }
    }

    fn current_width(&self) -> usize {
        if self.use_compact {
            self.compact_w
        } else {
            self.full_w
        }
    }
}

/// Terminal columns occupied by `text`, summed per grapheme cluster so a ZWJ emoji or a flag
/// pair counts once. Bytes or `char` counts would mis-size CJK, combining marks, and PUA icons.
pub fn str_display_width(text: &str) -> usize {
    text.graphemes(true).map(grapheme_display_width).sum()
}

/// Columns for one grapheme cluster: Nerd PUA icons are pinned to 1, emoji-presentation
/// sequences and regional-indicator flags to 2, everything else to the unicode-width tables.
pub fn grapheme_display_width(g: &str) -> usize {
    if g.is_empty() {
        return 0;
    }
    let mut chars = g.chars();
    // A patched font draws a Nerd icon in one column; pin that instead of trusting PUA tables.
    if let Some(first) = chars.next()
        && chars.next().is_none()
        && is_nerd_pua_codepoint(first)
    {
        return 1;
    }
    // ZWJ/VS16 mean emoji presentation, which terminals draw 2 columns wide; a cluster that
    // measures 0 (a lone joiner or combining mark) must stay 0.
    if (g.contains('\u{200d}') || g.contains('\u{fe0f}')) && UnicodeWidthStr::width(g) > 0 {
        return 2;
    }
    // A flag is two regional indicators in one cluster and still occupies 2 columns.
    if g.chars().all(is_regional_indicator) {
        return 2;
    }
    UnicodeWidthStr::width(g)
}

// BMP PUA plus the supplementary PUA planes where Nerd Font keeps its material-design icons.
fn is_nerd_pua_codepoint(ch: char) -> bool {
    ('\u{e000}'..='\u{f8ff}').contains(&ch)
        || ('\u{f0000}'..='\u{ffffd}').contains(&ch)
        || ('\u{100000}'..='\u{10fffd}').contains(&ch)
}

fn is_regional_indicator(ch: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch)
}

/// Lays out evaluated rows into styled lines plus the per-slot outcome grid. `max_cols` is the
/// terminal budget; `None` (or `overflow = "none"`) prints full width. The grid keeps the input
/// shape, including empty slots and rows that `single_line` drops.
pub fn layout_evaluated_rows(
    evaluated_rows: Vec<Vec<Option<EvaluatedItem>>>,
    frame: &FrameData,
    config: &EffectiveConfig,
    max_cols: Option<usize>,
) -> (Vec<StyledLine>, Vec<Vec<SlotOutcome>>) {
    let mut slot_outcomes: Vec<Vec<SlotOutcome>> = evaluated_rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|opt| {
                    if opt.is_some() {
                        SlotOutcome::Full
                    } else {
                        SlotOutcome::Empty
                    }
                })
                .collect()
        })
        .collect();

    let single_line = config.single_line && evaluated_rows.len() > 1;
    let compact_collapse = !single_line
        && evaluated_rows.len() > 1
        && config.compact_at > 0
        && max_cols.is_some_and(|w| w > 0 && w < config.compact_at);
    let row_count = if single_line || compact_collapse {
        1
    } else {
        evaluated_rows.len()
    };
    let eff_item_style = config.effective_item_style();
    let mut working_rows = vec![Vec::new(); row_count];
    let mut secondary_has_items = false;

    for (row_idx, row) in evaluated_rows.into_iter().enumerate() {
        for (slot_idx, eval) in row.into_iter().enumerate() {
            let Some(mut eval) = eval else { continue };
            let slot = config
                .lines
                .get(row_idx)
                .and_then(|line| line.get(slot_idx));
            let promote = single_line && row_idx > 0;
            if promote {
                secondary_has_items = true;
                let model_quota = slot.and_then(|s| s.model_quota_period()).is_some();
                if !model_quota {
                    slot_outcomes[row_idx][slot_idx] = SlotOutcome::Dropped;
                    continue;
                }
                if eval.has_compact_diff() {
                    slot_outcomes[row_idx][slot_idx] = SlotOutcome::Compact;
                }
            } else if compact_collapse && eval.has_compact_diff() {
                slot_outcomes[row_idx][slot_idx] = SlotOutcome::Compact;
            }
            wrap_eval_for_container(&mut eval, slot, eff_item_style);
            let target_row = if promote || compact_collapse {
                0
            } else {
                row_idx
            };
            working_rows[target_row].push(ActiveSlot::from_eval(
                Some((row_idx, slot_idx)),
                eval,
                promote || compact_collapse,
            ));
        }
    }

    // Keep the mode cue when its configured row was hidden.
    if secondary_has_items
        && !working_rows[0].iter().any(|s| s.eval.id == ItemId::Mode)
        && let Some(mut pill) = eval_single_line_mode_pill(frame, config)
    {
        wrap_eval_for_container(&mut pill, None, eff_item_style);
        working_rows[0].push(ActiveSlot::from_eval(None, pill, false));
    }

    // A zero budget counts as no budget: narrowing to 0 columns would print nothing useful.
    let width_budget = match config.overflow {
        OverflowMode::None => None,
        OverflowMode::Fit => max_cols.filter(|&w| w > 0),
    };

    let mut out_lines = Vec::with_capacity(working_rows.len());
    for mut active in working_rows.into_iter().filter(|row| !row.is_empty()) {
        let append_broken = config.config_broken && out_lines.is_empty();
        let line = narrow_single_row(
            &mut active,
            &mut slot_outcomes,
            config,
            width_budget,
            append_broken,
        );
        out_lines.push(line);
    }

    (out_lines, slot_outcomes)
}

// The ladder: compact what can shrink, drop whole items, then truncate the last survivor, and
// stop at the first stage that fits. `order` visits P5 first and P1 (model, context, mode) last.
fn narrow_single_row(
    active: &mut [ActiveSlot],
    slot_outcomes: &mut [Vec<SlotOutcome>],
    config: &EffectiveConfig,
    width_budget: Option<usize>,
    append_broken: bool,
) -> StyledLine {
    let default_broken = append_broken.then(|| " ! config".to_string());
    let Some(budget) = width_budget else {
        return StyledLine {
            items: collect_surviving_items(active),
            indent_override: None,
            broken_suffix: default_broken,
        };
    };

    let indent_w = str_display_width(&config.indent);
    let sep_w = str_display_width(config.effective_row_separator());
    // 9 is the display width of the " ! config" badge reserved on the first emitted row.
    let broken_w = if append_broken { 9 } else { 0 };

    let mut surviving_count = active.len();
    let mut total_w = indent_w + broken_w + sep_w * surviving_count.saturating_sub(1);
    for slot in active.iter_mut() {
        slot.full_w = spans_display_width(&slot.eval.full_spans);
        slot.compact_w = if slot.has_compact_diff {
            spans_display_width(&slot.eval.compact_spans)
        } else {
            slot.full_w
        };
        total_w += slot.current_width();
    }

    if total_w <= budget {
        return StyledLine {
            items: collect_surviving_items(active),
            indent_override: None,
            broken_suffix: default_broken,
        };
    }

    // Order: priority 5 down to 1, right-to-left within equal priority.
    let mut order: Vec<usize> = (0..active.len()).collect();
    order.sort_by(|&a, &b| {
        active[b]
            .eval
            .priority
            .cmp(&active[a].eval.priority)
            .then_with(|| b.cmp(&a))
    });

    // Stage 1: Downgrade eligible items from Full -> Compact in O(1) width updates.
    for &idx in &order {
        // Skip items whose compact form is identical: downgrading them would save no columns.
        if !active[idx].use_compact && active[idx].has_compact_diff {
            let prev_w = active[idx].full_w;
            let next_w = active[idx].compact_w;
            active[idx].use_compact = true;
            total_w = total_w.saturating_add(next_w).saturating_sub(prev_w);
            if let Some((row, slot)) = active[idx].origin {
                slot_outcomes[row][slot] = SlotOutcome::Compact;
            }
            if total_w <= budget {
                return StyledLine {
                    items: collect_surviving_items(active),
                    indent_override: None,
                    broken_suffix: default_broken,
                };
            }
        }
    }

    // Stage 2: Drop whole items right-to-left (5 -> 1), keeping at least 1 surviving item.
    for &idx in &order {
        // Keep one survivor for Stage 3 to truncate; dropping it would leave an empty row.
        if surviving_count <= 1 {
            break;
        }
        let item_w = active[idx].current_width();
        active[idx].dropped = true;
        surviving_count -= 1;
        total_w = total_w.saturating_sub(item_w + sep_w);
        if let Some((row, slot)) = active[idx].origin {
            slot_outcomes[row][slot] = SlotOutcome::Dropped;
        }
        if total_w <= budget {
            return StyledLine {
                items: collect_surviving_items(active),
                indent_override: None,
                broken_suffix: default_broken,
            };
        }
    }

    // Stage 3: Grapheme-safe right truncation of the last surviving item.
    let Some(surviving) = active.iter().find(|s| !s.dropped) else {
        return StyledLine {
            items: Vec::new(),
            indent_override: None,
            broken_suffix: default_broken,
        };
    };
    if let Some((row, slot)) = surviving.origin {
        slot_outcomes[row][slot] = SlotOutcome::Truncated;
    }

    let ell = glyphs::ellipsis(config.glyphs);
    let ell_w = str_display_width(ell);

    // Normal narrow budget: indent + ellipsis + optional " ! config" fits inside `budget`.
    if indent_w + ell_w + broken_w <= budget {
        let item_budget = budget - indent_w - broken_w;
        let clipped_spans =
            clip_spans_with_ellipsis(surviving.current_spans(), item_budget, ell, ell_w);
        return StyledLine {
            items: vec![StyledItem::new(surviving.eval.id, clipped_spans)],
            indent_override: None,
            broken_suffix: default_broken,
        };
    }

    // Ultra-tiny width fallback: preserve grapheme boundaries across indent, item, and broken badge.
    // The indent is all-or-nothing: drop it whole so the item keeps the remaining columns.
    let effective_indent = if indent_w < budget {
        config.indent.clone()
    } else {
        String::new()
    };
    let eff_indent_w = str_display_width(&effective_indent);
    let rem_after_indent = budget.saturating_sub(eff_indent_w);

    // No columns left: suppress indent and badge rather than overflow the budget.
    if rem_after_indent == 0 {
        return StyledLine {
            items: vec![StyledItem::new(surviving.eval.id, Vec::new())],
            indent_override: Some(String::new()),
            broken_suffix: Some(String::new()),
        };
    }

    if append_broken && rem_after_indent > ell_w {
        // Keep a single ellipsis for the surviving item and grapheme-clip " ! config" into remaining cols.
        let item_spans = clip_spans_with_ellipsis(surviving.current_spans(), ell_w, ell, ell_w);
        let item_w = spans_display_width(&item_spans);
        let badge_budget = rem_after_indent.saturating_sub(item_w);
        let clipped_badge = clip_plain_str(" ! config", badge_budget);
        return StyledLine {
            items: vec![StyledItem::new(surviving.eval.id, item_spans)],
            indent_override: Some(effective_indent),
            broken_suffix: Some(clipped_badge),
        };
    }

    let item_spans =
        clip_spans_with_ellipsis(surviving.current_spans(), rem_after_indent, ell, ell_w);
    StyledLine {
        items: vec![StyledItem::new(surviving.eval.id, item_spans)],
        indent_override: Some(effective_indent),
        broken_suffix: append_broken.then(String::new),
    }
}

fn collect_surviving_items(active: &[ActiveSlot]) -> Vec<StyledItem> {
    active
        .iter()
        .filter(|s| !s.dropped)
        .map(|s| StyledItem::new(s.eval.id, s.current_spans().to_vec()))
        .collect()
}

fn spans_display_width(spans: &[StyledSpan]) -> usize {
    spans.iter().map(|sp| str_display_width(&sp.text)).sum()
}

fn clip_spans_with_ellipsis(
    spans: &[StyledSpan],
    budget: usize,
    ell: &str,
    ell_w: usize,
) -> Vec<StyledSpan> {
    if budget == 0 {
        return Vec::new();
    }
    // Even the ellipsis does not fit (ascii "..." is 3 columns): fill with plain dots instead.
    if budget < ell_w {
        let dots = ".".repeat(budget);
        return vec![StyledSpan::new(ColorToken::Muted, dots)];
    }
    let text_limit = budget - ell_w;
    let mut clipped = Vec::new();
    let mut used = 0usize;

    for sp in spans {
        let mut acc = String::new();
        for g in sp.text.graphemes(true) {
            let gw = grapheme_display_width(g);
            if used + gw > text_limit {
                if !acc.is_empty() {
                    clipped.push(StyledSpan::styled(sp.color, sp.style, acc));
                }
                clipped.push(StyledSpan::new(ColorToken::Muted, ell));
                return clipped;
            }
            acc.push_str(g);
            used += gw;
        }
        if !acc.is_empty() {
            clipped.push(StyledSpan::styled(sp.color, sp.style, acc));
        }
    }

    clipped.push(StyledSpan::new(ColorToken::Muted, ell));
    clipped
}

fn clip_plain_str(text: &str, budget: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for g in text.graphemes(true) {
        let gw = grapheme_display_width(g);
        if used + gw > budget {
            break;
        }
        out.push_str(g);
        used += gw;
    }
    out
}

fn is_container_boxed(id: ItemId, slot: Option<&ItemSlot>) -> bool {
    match id {
        // Graphic or lightweight items stay un-boxed in badge/pill modes to keep visual rhythm.
        ItemId::QuotaGroup | ItemId::Status | ItemId::Dir => false,
        ItemId::Context => !slot.is_some_and(|s| {
            s.style
                .as_deref()
                .is_some_and(|st| st.eq_ignore_ascii_case("bar"))
        }),
        ItemId::Model
        | ItemId::Git
        | ItemId::Cache
        | ItemId::Speed
        | ItemId::Budget
        | ItemId::Quota
        | ItemId::Cost
        | ItemId::Mode
        | ItemId::Subagents
        | ItemId::Tasks
        | ItemId::Queue
        | ItemId::Sandbox
        | ItemId::Vim
        | ItemId::Title
        | ItemId::Agent
        | ItemId::Artifacts
        | ItemId::Version
        | ItemId::Plan
        | ItemId::Tokens
        | ItemId::Duration => true,
    }
}

fn container_bg_token(id: ItemId) -> ColorToken {
    match id {
        ItemId::Model => ColorToken::SurfaceHi,
        ItemId::Dir
        | ItemId::Git
        | ItemId::Context
        | ItemId::Cache
        | ItemId::Speed
        | ItemId::Budget
        | ItemId::Quota
        | ItemId::Cost
        | ItemId::Mode
        | ItemId::Subagents
        | ItemId::Tasks
        | ItemId::Queue
        | ItemId::Sandbox
        | ItemId::Vim
        | ItemId::Title
        | ItemId::Agent
        | ItemId::Artifacts
        | ItemId::Version
        | ItemId::Plan
        | ItemId::Status
        | ItemId::Tokens
        | ItemId::Duration
        | ItemId::QuotaGroup => ColorToken::Surface,
    }
}

fn wrap_eval_for_container(eval: &mut EvaluatedItem, slot: Option<&ItemSlot>, style: ItemStyle) {
    if style == ItemStyle::Minimal || !is_container_boxed(eval.id, slot) {
        return;
    }
    let full = std::mem::take(&mut eval.full_spans);
    let compact = std::mem::take(&mut eval.compact_spans);
    eval.full_spans = wrap_spans_for_container(eval.id, full, style);
    eval.compact_spans = wrap_spans_for_container(eval.id, compact, style);
}

fn wrap_spans_for_container(
    id: ItemId,
    spans: Vec<StyledSpan>,
    style: ItemStyle,
) -> Vec<StyledSpan> {
    if spans.is_empty() {
        return spans;
    }
    match style {
        ItemStyle::Minimal => spans,
        ItemStyle::Badge | ItemStyle::Pill => {
            let bg_spec = ColorSpec::Token(container_bg_token(id));
            let extra = match style {
                ItemStyle::Minimal => 0,
                ItemStyle::Badge => 2,
                ItemStyle::Pill => 4,
            };
            let mut out = Vec::with_capacity(spans.len() + extra);
            if style == ItemStyle::Pill {
                out.push(StyledSpan::new(bg_spec, PILL_LEFT_CAP));
            }
            out.push(StyledSpan::new(bg_spec, " ").with_bg(bg_spec));
            for mut sp in spans {
                if sp.style.bg.is_none() {
                    sp.style.bg = Some(bg_spec);
                }
                out.push(sp);
            }
            out.push(StyledSpan::new(bg_spec, " ").with_bg(bg_spec));
            if style == ItemStyle::Pill {
                out.push(StyledSpan::new(bg_spec, PILL_RIGHT_CAP));
            }
            out
        }
    }
}
