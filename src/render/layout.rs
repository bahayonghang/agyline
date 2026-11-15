//! Display-width measurement and priority-based narrowing. `overflow = "fit"` shrinks a row to
//! the terminal budget in three stages (compact, drop, grapheme-safe truncate); `"none"` prints
//! it whole. Widths are terminal columns, never bytes or chars, and every decision is recorded
//! per config slot in `SlotOutcome` for the configurator and the narrowing tests.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId, OverflowMode};
use crate::items::{EvaluatedItem, runtime::eval_single_line_mode_pill};
use crate::render::{ColorToken, StyledItem, StyledLine, StyledSpan, glyphs};

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
    orig_slot_idx: Option<usize>,
    eval: EvaluatedItem,
    use_compact: bool,
    dropped: bool,
}

impl ActiveSlot {
    fn current_spans(&self) -> &[StyledSpan] {
        if self.use_compact {
            &self.eval.compact_spans
        } else {
            &self.eval.full_spans
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

    let mut working_rows: Vec<(usize, Vec<ActiveSlot>)> = Vec::with_capacity(evaluated_rows.len());

    if config.single_line && evaluated_rows.len() > 1 {
        let mut secondary_has_items = false;
        for (r_idx, row) in evaluated_rows.iter().enumerate().skip(1) {
            for (s_idx, opt) in row.iter().enumerate() {
                if opt.is_some() {
                    secondary_has_items = true;
                    slot_outcomes[r_idx][s_idx] = SlotOutcome::Dropped;
                }
            }
        }
        let mut row0_active: Vec<ActiveSlot> = evaluated_rows
            .into_iter()
            .next()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                opt.map(|eval| ActiveSlot {
                    orig_slot_idx: Some(idx),
                    eval,
                    use_compact: false,
                    dropped: false,
                })
            })
            .collect();

        // Collapsing rows would hide runtime state, so synthesize a mode pill when row 0 has no
        // Mode slot of its own. It has no config slot, so it records no outcome.
        if secondary_has_items
            && !row0_active.iter().any(|s| s.eval.id == ItemId::Mode)
            && let Some(pill) = eval_single_line_mode_pill(frame, config)
        {
            row0_active.push(ActiveSlot {
                orig_slot_idx: None,
                eval: pill,
                use_compact: false,
                dropped: false,
            });
        }
        if !row0_active.is_empty() {
            working_rows.push((0, row0_active));
        }
    } else {
        for (r_idx, row) in evaluated_rows.into_iter().enumerate() {
            let active: Vec<ActiveSlot> = row
                .into_iter()
                .enumerate()
                .filter_map(|(idx, opt)| {
                    opt.map(|eval| ActiveSlot {
                        orig_slot_idx: Some(idx),
                        eval,
                        use_compact: false,
                        dropped: false,
                    })
                })
                .collect();
            if !active.is_empty() {
                working_rows.push((r_idx, active));
            }
        }
    }

    // A zero budget counts as no budget: narrowing to 0 columns would print nothing useful.
    let width_budget = match config.overflow {
        OverflowMode::None => None,
        OverflowMode::Fit => max_cols.filter(|&w| w > 0),
    };

    let mut out_lines = Vec::with_capacity(working_rows.len());
    for (emitted_idx, (orig_row_idx, mut active)) in working_rows.into_iter().enumerate() {
        let append_broken = config.config_broken && emitted_idx == 0;
        let line = narrow_single_row(
            &mut active,
            &mut slot_outcomes[orig_row_idx],
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
    row_outcomes: &mut [SlotOutcome],
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
    let sep_w = str_display_width(&config.separator);
    // 9 is the display width of the " ! config" badge reserved on the first emitted row.
    let broken_w = if append_broken { 9 } else { 0 };

    if compute_row_width(active, indent_w, sep_w, broken_w) <= budget {
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

    // Stage 1: Downgrade eligible items from Full -> Compact.
    for &idx in &order {
        // Skip items whose compact form is identical: downgrading them would save no columns.
        if active[idx].eval.has_compact_diff() {
            active[idx].use_compact = true;
            if let Some(orig) = active[idx].orig_slot_idx {
                row_outcomes[orig] = SlotOutcome::Compact;
            }
            if compute_row_width(active, indent_w, sep_w, broken_w) <= budget {
                return StyledLine {
                    items: collect_surviving_items(active),
                    indent_override: None,
                    broken_suffix: default_broken,
                };
            }
        }
    }

    // Stage 2: Drop whole items right-to-left (5 -> 1), keeping at least 1 surviving item.
    let mut surviving_count = active.len();
    for &idx in &order {
        // Keep one survivor for Stage 3 to truncate; dropping it would leave an empty row.
        if surviving_count <= 1 {
            break;
        }
        active[idx].dropped = true;
        surviving_count -= 1;
        if let Some(orig) = active[idx].orig_slot_idx {
            row_outcomes[orig] = SlotOutcome::Dropped;
        }
        if compute_row_width(active, indent_w, sep_w, broken_w) <= budget {
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
    if let Some(orig) = surviving.orig_slot_idx {
        row_outcomes[orig] = SlotOutcome::Truncated;
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

fn compute_row_width(
    active: &[ActiveSlot],
    indent_w: usize,
    sep_w: usize,
    broken_w: usize,
) -> usize {
    let mut total = indent_w + broken_w;
    // Separators only between survivors, so a dropped item leaves no phantom separator behind.
    for (seen, slot) in active.iter().filter(|s| !s.dropped).enumerate() {
        if seen > 0 {
            total += sep_w;
        }
        total += spans_display_width(slot.current_spans());
    }
    total
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
                    clipped.push(StyledSpan::new(sp.color, acc));
                }
                clipped.push(StyledSpan::new(ColorToken::Muted, ell));
                return clipped;
            }
            acc.push_str(g);
            used += gw;
        }
        if !acc.is_empty() {
            clipped.push(StyledSpan::new(sp.color, acc));
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
