//! Usage Segments 4-7: context window, prompt-cache TTL and hit rate, latched output speed, quota
//! buckets and USD cost. The 300s cache window, the 60s warn boundary, the 32K keepalive floor and
//! the 50/80% context and 20/45% quota thresholds reproduce `ref/agy-patch/statusline.ps1` and
//! `statusline.py`. Inputs are already in `FrameData`; nothing here reads state or the network.

use crate::collect::{FrameData, RelayStatus};
use crate::config::{EffectiveConfig, ItemId, ItemSlot, ModelQuotaPeriod, ThresholdsConfig};
use crate::input::QuotaBucketData;
use crate::items::{
    EvaluatedItem, coalesced_icon_body_spans, labeled_slot_icon, slot_custom_icon_color,
    slot_fg_color, truncate_chars_with_ellipsis,
};
use crate::render::{
    BarStyle, ColorSpec, ColorToken, StyledSpan, TextStyle, glyphs, render_usage_bar,
};

/// Context percent and token count. Percent is `(input + cache_read) / window`, falling back to
/// the frame's `used_percentage`; warn/crit come from `[thresholds]`, default 50/80 as reference.
pub fn eval_context(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let cur_in = payload.input_tokens;
    let cache_read = payload.cache_read_input_tokens;
    // Cached prompt tokens count toward the window, as in the reference's `$realIn`.
    let real_in = cur_in.saturating_add(cache_read);
    let win_sz = payload.context_window_size;

    let raw_pct = if real_in > 0 && win_sz > 0.0 {
        (real_in as f64) * 100.0 / win_sz
    } else {
        payload.used_percentage
    };
    let used_pct = normalize_zero(raw_pct);

    // Ties-to-even rounding matches `[Math]::Round` and Python `round` in the reference.
    let used_tokens = if real_in > 0 {
        real_in
    } else if used_pct > 0.0 && win_sz > 0.0 {
        ((used_pct / 100.0) * win_sz).round_ties_even() as i64
    } else {
        0
    };

    let base_col = slot_fg_color(slot, ColorToken::Context);
    // Reference: >=80 crit, >=50 warn, else the context magenta.
    let ctx_col: ColorSpec = if used_pct >= config.thresholds.context_crit {
        ColorToken::Crit.into()
    } else if used_pct >= config.thresholds.context_warn {
        ColorToken::Warn.into()
    } else {
        base_col
    };

    let show_remaining = slot.show_remaining.unwrap_or(false);
    if slot
        .style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("bar"))
    {
        let bar_col: ColorSpec = if used_pct >= config.thresholds.context_crit {
            ColorToken::Crit.into()
        } else if used_pct >= config.thresholds.context_warn {
            ColorToken::Warn.into()
        } else {
            slot_fg_color(slot, ColorToken::Ok)
        };
        let disp_pct = if show_remaining {
            let rem = if real_in > 0 && win_sz > 0.0 {
                (100.0 - used_pct).clamp(0.0, 100.0)
            } else if let Some(rp) = payload.remaining_percentage {
                rp.clamp(0.0, 100.0)
            } else {
                (100.0 - used_pct).clamp(0.0, 100.0)
            };
            normalize_zero(rem)
        } else {
            used_pct.clamp(0.0, 100.0)
        };
        let pct_label = if show_remaining {
            format!("{disp_pct:.1}% left")
        } else {
            format!("{disp_pct:.1}%")
        };
        let bar_w = usize::from(slot.bar_width.unwrap_or(10).clamp(4, 20));
        let bar_style = slot
            .bar_style
            .as_deref()
            .and_then(BarStyle::from_key)
            .unwrap_or_default();
        let icon = labeled_slot_icon(slot, config.glyphs, glyphs::context_icon);
        let custom_ic = slot_custom_icon_color(slot);

        let mut prefix_spans = Vec::with_capacity(2);
        if !icon.is_empty() {
            prefix_spans.push(StyledSpan::new(
                custom_ic.unwrap_or(ColorSpec::Token(ColorToken::Context)),
                format!("{icon} "),
            ));
        }
        prefix_spans.push(StyledSpan::new(ColorToken::Muted, "ctx "));

        let mut full_spans = prefix_spans.clone();
        if slot.show_bar.unwrap_or(true) {
            full_spans.extend(render_usage_bar(
                disp_pct, bar_w, bar_col, config, bar_style,
            ));
            full_spans.push(StyledSpan::new(bar_col, format!(" {pct_label}")));
        } else {
            full_spans.push(StyledSpan::new(bar_col, pct_label.clone()));
        }

        let mut compact_spans = prefix_spans;
        compact_spans.push(StyledSpan::new(bar_col, pct_label));

        return Some(EvaluatedItem {
            id: ItemId::Context,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans,
        });
    }

    // Reference: integer percent at >=10 or exactly 0, one decimal below 10 (both branches).
    let pct_fmt = if show_remaining {
        let rem_raw = if real_in > 0 && win_sz > 0.0 {
            (100.0 - used_pct).clamp(0.0, 100.0)
        } else if let Some(rp) = payload.remaining_percentage {
            rp.clamp(0.0, 100.0)
        } else {
            (100.0 - used_pct).clamp(0.0, 100.0)
        };
        let rem = normalize_zero(rem_raw);
        if rem >= 10.0 || rem == 0.0 {
            format!("{:.0}% left", normalize_zero(rem.round_ties_even()))
        } else {
            format!("{rem:.1}% left")
        }
    } else if used_pct >= 10.0 || used_pct == 0.0 {
        format!("{:.0}%", normalize_zero(used_pct.round_ties_even()))
    } else {
        format!("{used_pct:.1}%")
    };

    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::context_icon(config.glyphs));
    let custom_ic = slot_custom_icon_color(slot);
    // The icon stays at the base color even when the text turns warn/crit (reference).
    let icon_col = custom_ic.unwrap_or(base_col);

    // Full keeps the reference's ` tokens` suffix; compact drops the word, keeping the count.
    let (full_spans, compact_spans) = if used_tokens > 0 {
        let tok_fmt = format_compact_tokens(used_tokens);
        let build_with_tok = |tok_label: String| -> Vec<StyledSpan> {
            let mut spans = Vec::with_capacity(4);
            if icon.is_empty() {
                spans.push(StyledSpan::new(ctx_col, pct_fmt.clone()));
            } else {
                spans.push(StyledSpan::new(icon_col, icon));
                spans.push(StyledSpan::new(ctx_col, format!(" {pct_fmt}")));
            }
            spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
            spans.push(StyledSpan::new(ctx_col, tok_label));
            spans
        };
        (
            build_with_tok(format!("{tok_fmt} tokens")),
            build_with_tok(tok_fmt),
        )
    } else {
        let spans = coalesced_icon_body_spans(icon, custom_ic, ctx_col, pct_fmt);
        (spans.clone(), spans)
    };

    Some(EvaluatedItem {
        id: ItemId::Context,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

/// Prompt-cache TTL from the last accepted turn, plus keepalive badge and hit rate. `cold` is
/// muted when no timestamp exists and warn once the 300s window expires. Never `None`.
pub fn eval_cache(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let base_col = slot_fg_color(slot, ColorToken::Cache);
    let icon_col = slot_custom_icon_color(slot).unwrap_or(base_col);

    let payload = &frame.payload;
    let cur_in = payload.input_tokens.max(0);
    let cache_read = payload.cache_read_input_tokens.max(0);
    let cache_create = payload.cache_creation_input_tokens.max(0);
    let tot_prompt = cur_in.saturating_add(cache_read);

    if slot
        .style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("hit_rate"))
    {
        let hit_pct: Option<f64> = if let Some(tt) = frame.transcript_totals
            && tt.cache_valid
            && tt.total_in > 0
        {
            Some(((tt.cache_read.max(0) as f64) * 100.0 / (tt.total_in as f64)).clamp(0.0, 100.0))
        } else {
            let denom = tot_prompt.saturating_add(cache_create);
            if denom > 0 && (cache_read > 0 || frame.cache.cache_ts.is_some()) {
                Some(((cache_read as f64) * 100.0 / (denom as f64)).clamp(0.0, 100.0))
            } else {
                None
            }
        };
        let show_ttl = slot.show_ttl.unwrap_or(false);
        if hit_pct.is_none() && !show_ttl {
            return None;
        }
        let val_col = slot_fg_color(slot, ColorToken::Separator);
        let bold = TextStyle::PLAIN.bold();
        let hit_icon = labeled_slot_icon(slot, config.glyphs, glyphs::cache_icon);
        let mut base_spans = Vec::with_capacity(4);
        if !hit_icon.is_empty() {
            base_spans.push(StyledSpan::new(icon_col, format!("{hit_icon} ")));
        }
        base_spans.push(StyledSpan::new(ColorToken::Muted, "cache "));
        if let Some(pct) = hit_pct {
            base_spans.push(StyledSpan::styled(val_col, bold, format!("{pct:.1}%")));
        } else {
            base_spans.push(StyledSpan::new(ColorToken::Muted, "cold"));
        }

        let mut full_spans = base_spans.clone();
        if show_ttl && let Some(cache_ts) = frame.cache.cache_ts.filter(|&ts| ts > 0) {
            let cache_rem = 300_i64.saturating_sub(frame.now_sec.saturating_sub(cache_ts));
            full_spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
            if cache_rem > 0 {
                let ttl_col: ColorSpec = if (cache_rem as u64) > config.thresholds.cache_warn_secs {
                    base_col
                } else {
                    ColorToken::Warn.into()
                };
                full_spans.push(StyledSpan::new(
                    ttl_col,
                    format!("{}:{:02}", cache_rem / 60, cache_rem % 60),
                ));
            } else {
                full_spans.push(StyledSpan::new(ColorToken::Warn, "cold"));
            }
            match frame.cache.relay_status {
                Some(RelayStatus::Fail) => full_spans.push(StyledSpan::new(
                    ColorToken::Crit,
                    format!(" {}", glyphs::relay_fail_icon(config.glyphs)),
                )),
                Some(RelayStatus::Live)
                    if frame.cache.relay_touches > 0 || tot_prompt >= 32_000 =>
                {
                    let live_icon = glyphs::relay_live_icon(config.glyphs);
                    let badge_txt = if frame.cache.relay_touches > 0 {
                        format!(" {live_icon}{}", frame.cache.relay_touches)
                    } else {
                        format!(" {live_icon}")
                    };
                    full_spans.push(StyledSpan::new(ColorToken::Ok, badge_txt));
                }
                Some(RelayStatus::Live) | None => {}
            }
        }

        return Some(EvaluatedItem {
            id: ItemId::Cache,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans: base_spans,
        });
    }
    // Hit rate truncates instead of rounding, like the reference's integer division.
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::cache_icon(config.glyphs));
    let cache_pct = if tot_prompt > 0 {
        (cache_read.saturating_mul(100)) / tot_prompt
    } else {
        0
    };

    // No timestamp at all: the reference prints a dim `cold`, not the warn one.
    let Some(cache_ts) = frame.cache.cache_ts.filter(|&ts| ts > 0) else {
        let spans = if icon.is_empty() {
            vec![StyledSpan::new(ColorToken::Muted, "cold")]
        } else {
            vec![
                StyledSpan::new(icon_col, icon),
                StyledSpan::new(ColorToken::Muted, " cold"),
            ]
        };
        return Some(EvaluatedItem {
            id: ItemId::Cache,
            priority: slot.effective_priority(),
            full_spans: spans.clone(),
            compact_spans: spans,
        });
    };

    // 300s implicit-cache window from the last accepted turn; the relay anchor can move it.
    let cache_rem = 300_i64.saturating_sub(frame.now_sec.saturating_sub(cache_ts));
    // `fail` outranks `live`. The live badge needs a touch or the reference's 32K prompt floor,
    // the same floor that gates a keepalive touch.
    let keep_badge = match frame.cache.relay_status {
        Some(RelayStatus::Fail) => Some(StyledSpan::new(
            ColorToken::Crit,
            format!(" {}", glyphs::relay_fail_icon(config.glyphs)),
        )),
        Some(RelayStatus::Live) if frame.cache.relay_touches > 0 || tot_prompt >= 32_000 => {
            let live_icon = glyphs::relay_live_icon(config.glyphs);
            let badge_txt = if frame.cache.relay_touches > 0 {
                format!(" {live_icon}{}", frame.cache.relay_touches)
            } else {
                format!(" {live_icon}")
            };
            Some(StyledSpan::new(ColorToken::Ok, badge_txt))
        }
        Some(RelayStatus::Live) | None => None,
    };

    let mut core_spans = Vec::with_capacity(3);
    if !icon.is_empty() {
        core_spans.push(StyledSpan::new(icon_col, icon));
    }

    let val_col: ColorSpec = if cache_rem > 0 {
        let mm = cache_rem / 60;
        let ss = cache_rem % 60;
        // Reference: cache blue above 60s left, warn in the last minute (`cache_warn_secs`).
        let ttl_col: ColorSpec = if (cache_rem as u64) > config.thresholds.cache_warn_secs {
            base_col
        } else {
            ColorToken::Warn.into()
        };
        let prefix = if icon.is_empty() { "" } else { " " };
        core_spans.push(StyledSpan::new(ttl_col, format!("{prefix}{mm}:{ss:02}")));
        ttl_col
    } else {
        let prefix = if icon.is_empty() { "" } else { " " };
        core_spans.push(StyledSpan::new(ColorToken::Warn, format!("{prefix}cold")));
        ColorToken::Warn.into()
    };

    if let Some(badge) = keep_badge {
        core_spans.push(badge);
    }

    let mut full_spans = core_spans.clone();
    // Hit rate takes the TTL color, as in the reference; the compact form drops it (agyline).
    if tot_prompt > 0 {
        full_spans.push(StyledSpan::new(
            ColorToken::Separator,
            glyphs::dot_sep(config.glyphs),
        ));
        full_spans.push(StyledSpan::new(val_col, format!("{cache_pct}% hit")));
    }

    Some(EvaluatedItem {
        id: ItemId::Cache,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans: core_spans,
    })
}

/// Output rate in tok/s. `None` until the first measurement (`--`); the value then latches in
/// session state, so the segment stays visible between turns instead of blinking out.
pub fn eval_speed(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let tps = frame
        .last_tps
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "--")?;
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::bolt_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Ok);
    let custom_ic = slot_custom_icon_color(slot);

    Some(EvaluatedItem {
        id: ItemId::Speed,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, format!("{tps} tok/s")),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, format!("{tps}/s")),
    })
}

/// Reference segment 7: the most urgent quota bucket and the session cost joined by a middle dot.
/// `None` when neither is present.
pub fn eval_budget(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let show_reset = slot.show_reset.unwrap_or(false);
    let show_amount = slot.show_amount.unwrap_or(false);
    let show_subagent = slot.show_subagent.unwrap_or(false);

    let mut full_spans = Vec::with_capacity(4);
    let mut compact_spans = Vec::with_capacity(4);

    if let Some(bucket) =
        select_quota_bucket(&payload.quota_buckets, slot.bucket.as_deref(), show_amount)
        && let Some((q_full, q_comp)) = render_quota_spans(
            slot,
            bucket,
            "quota",
            show_reset,
            show_amount,
            frame.now_sec,
            config,
        )
    {
        full_spans.extend(q_full);
        compact_spans.extend(q_comp);
    }

    // Cost is warn-colored in the reference even when the quota part is green.
    if payload.cost_usd > 0.0 {
        if !full_spans.is_empty() {
            full_spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
            compact_spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
        }
        let cost_base = format_usd(payload.cost_usd);
        // The subagent share is an agyline addition; the reference prints only the session total.
        let cost_full = if show_subagent && payload.subagent_cost_usd > 0.0 {
            format!(
                "{cost_base} (sub {})",
                format_subagent_usd(payload.subagent_cost_usd)
            )
        } else {
            cost_base.clone()
        };
        full_spans.push(StyledSpan::new(ColorToken::Warn, cost_full));
        compact_spans.push(StyledSpan::new(ColorToken::Warn, cost_base));
    }

    if full_spans.is_empty() {
        None
    } else {
        Some(EvaluatedItem {
            id: ItemId::Budget,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans,
        })
    }
}

/// One bucket per slot, repeatable within a row. `None` when no bucket has a usable value; `label`
/// defaults to `quota`.
pub fn eval_quota(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let show_reset = slot.show_reset.unwrap_or(false);
    let show_amount = slot.show_amount.unwrap_or(false);
    let label = slot.label.as_deref().unwrap_or("quota");
    let payload = &frame.payload;
    let bucket = if let Some(period) = slot.model_quota_period() {
        let key = model_quota_bucket(&payload.model_id, period)
            .or_else(|| model_quota_bucket(&payload.model_display_name, period))?;
        payload.quota_buckets.iter().find(|bucket| {
            bucket.name.eq_ignore_ascii_case(key)
                && !bucket.disabled
                && bucket
                    .remaining_fraction
                    .is_some_and(|rf| (0.0..=1.0).contains(&rf))
        })
    } else {
        select_quota_bucket(&payload.quota_buckets, slot.bucket.as_deref(), show_amount)
    }?;
    let (full_spans, compact_spans) = render_quota_spans(
        slot,
        bucket,
        label,
        show_reset,
        show_amount,
        frame.now_sec,
        config,
    )?;

    Some(EvaluatedItem {
        id: ItemId::Quota,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

fn model_quota_bucket(model: &str, period: ModelQuotaPeriod) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let model = normalized.strip_prefix("models/").unwrap_or(&normalized);
    let mut parts = model
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty());
    let gemini = match parts.next()? {
        "gemini" => true,
        "orchestrator" if matches!(parts.next(), Some("carbon" | "argon")) => true,
        "claude" | "gpt" => false,
        _ => return None,
    };
    match (gemini, period) {
        (true, ModelQuotaPeriod::FiveHours) => Some("gemini-5h"),
        (true, ModelQuotaPeriod::Weekly) => Some("gemini-weekly"),
        (false, ModelQuotaPeriod::FiveHours) => Some("3p-5h"),
        (false, ModelQuotaPeriod::Weekly) => Some("3p-weekly"),
    }
}

/// Session USD cost; `None` at zero. The warn color and the `$` formatting are the reference's.
pub fn eval_cost(
    slot: &ItemSlot,
    frame: &FrameData,
    _config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    if payload.cost_usd <= 0.0 {
        return None;
    }
    let icon = slot.icon.as_deref().unwrap_or("");
    let body_col = slot_fg_color(slot, ColorToken::Warn);
    let custom_ic = slot_custom_icon_color(slot);
    let show_subagent = slot.show_subagent.unwrap_or(false);

    let base = format_usd(payload.cost_usd);
    let full = if show_subagent && payload.subagent_cost_usd > 0.0 {
        format!(
            "{base} (sub {})",
            format_subagent_usd(payload.subagent_cost_usd)
        )
    } else {
        base.clone()
    };

    Some(EvaluatedItem {
        id: ItemId::Cost,
        priority: slot.effective_priority(),
        full_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, full),
        compact_spans: coalesced_icon_body_spans(icon, custom_ic, body_col, base),
    })
}

/// No filter: the lowest remaining fraction among enabled buckets, as the reference's min. A
/// filter prefers an exact case-insensitive name, then the most urgent substring match.
/// `allow_amount_only` admits count-only buckets, which the reference does not read.
fn select_quota_bucket<'a>(
    buckets: &'a [QuotaBucketData],
    filter: Option<&str>,
    allow_amount_only: bool,
) -> Option<&'a QuotaBucketData> {
    let filter_trimmed = filter.map(str::trim).filter(|s| !s.is_empty());
    // Disabled buckets and fractions outside 0..=1 are skipped, as in the reference.
    let has_usable_value = |b: &QuotaBucketData| -> bool {
        !b.disabled
            && (b
                .remaining_fraction
                .is_some_and(|rf| (0.0..=1.0).contains(&rf))
                || (allow_amount_only && b.remaining_amount.is_some()))
    };

    if let Some(f) = filter_trimmed {
        let f_low = f.to_ascii_lowercase();
        if let Some(exact) = buckets
            .iter()
            .find(|b| has_usable_value(b) && b.name.eq_ignore_ascii_case(&f_low))
        {
            return Some(exact);
        }
        return buckets
            .iter()
            .filter(|b| has_usable_value(b) && b.name.to_ascii_lowercase().contains(f_low.as_str()))
            .min_by(compare_bucket_urgency);
    }

    buckets
        .iter()
        .filter(|b| has_usable_value(b))
        .min_by(compare_bucket_urgency)
}

/// A fraction bucket outranks a count-only one, so a credits count cannot hide a percentage.
fn compare_bucket_urgency(a: &&QuotaBucketData, b: &&QuotaBucketData) -> std::cmp::Ordering {
    match (a.remaining_fraction, b.remaining_fraction) {
        (Some(af), Some(bf)) => af.total_cmp(&bf),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.remaining_amount.cmp(&b.remaining_amount),
    }
}

/// Quota body in both widths. Below `quota_crit` (default 20) crit, below `quota_warn` (45) warn,
/// else ok; the battery icon bands are the reference's 90/70/45/20. `None` when nothing is usable.
fn render_quota_spans(
    slot: &ItemSlot,
    bucket: &QuotaBucketData,
    label: &str,
    show_reset: bool,
    show_amount: bool,
    now_sec: i64,
    config: &EffectiveConfig,
) -> Option<(Vec<StyledSpan>, Vec<StyledSpan>)> {
    let reset_suffix = if show_reset {
        format_reset_suffix(bucket, now_sec).unwrap_or_default()
    } else {
        String::new()
    };
    let custom_ic = slot_custom_icon_color(slot);

    if let Some(rf) = bucket
        .remaining_fraction
        .filter(|v| (0.0..=1.0).contains(v))
    {
        let show_used = slot.show_used.unwrap_or(false);
        let show_bar = slot.show_bar.unwrap_or(false);
        // Ties-to-even percent, matching `[Math]::Round` in the reference.
        let quota_pct = (rf * 100.0).round_ties_even() as i32;
        let icon = slot
            .icon
            .as_deref()
            .unwrap_or_else(|| glyphs::quota_icon(config.glyphs, quota_pct));
        let base_col = slot_fg_color(slot, ColorToken::Ok);
        let used_tenths = (((1.0 - rf) * 1000.0).round() as i32).clamp(0, 1000);
        let q_col: ColorSpec = if show_used {
            let used_crit_tenths = (100 - config.thresholds.quota_crit) * 10;
            let used_warn_tenths = (100 - config.thresholds.quota_warn) * 10;
            if used_tenths >= used_crit_tenths {
                ColorToken::Crit.into()
            } else if used_tenths >= used_warn_tenths {
                ColorToken::Warn.into()
            } else {
                base_col
            }
        } else if quota_pct < config.thresholds.quota_crit {
            ColorToken::Crit.into()
        } else if quota_pct < config.thresholds.quota_warn {
            ColorToken::Warn.into()
        } else {
            base_col
        };

        let amt_note = if show_amount && let Some(amt) = bucket.remaining_amount {
            format!(" [{amt}]")
        } else {
            String::new()
        };
        let lbl_part = if label.is_empty() {
            String::new()
        } else {
            format!(" {label}")
        };
        let pct_txt = if show_used {
            format!("{}.{}%", used_tenths / 10, used_tenths % 10)
        } else {
            format!("{quota_pct}%")
        };
        if show_used || show_bar {
            let mut full_spans = Vec::with_capacity(6);
            if !icon.is_empty() {
                full_spans.push(StyledSpan::new(
                    custom_ic.unwrap_or(q_col),
                    format!("{icon} "),
                ));
            }
            if show_bar {
                let bar_pct = if show_used {
                    (1.0 - rf) * 100.0
                } else {
                    rf * 100.0
                };
                let bar_w = usize::from(slot.bar_width.unwrap_or(10).clamp(4, 20));
                let bar_style = slot
                    .bar_style
                    .as_deref()
                    .and_then(BarStyle::from_key)
                    .unwrap_or_default();
                full_spans.extend(render_usage_bar(bar_pct, bar_w, q_col, config, bar_style));
                full_spans.push(StyledSpan::new(
                    q_col,
                    format!(" {pct_txt}{lbl_part}{amt_note}{reset_suffix}"),
                ));
            } else {
                full_spans.push(StyledSpan::new(
                    q_col,
                    format!("{pct_txt}{lbl_part}{amt_note}{reset_suffix}"),
                ));
            }
            let compact_lbl = if label.is_empty() || (label == "quota" && !icon.is_empty()) {
                String::new()
            } else {
                format!(" {}", truncate_chars_with_ellipsis(label, 8, config.glyphs))
            };
            let compact_spans = coalesced_icon_body_spans(
                icon,
                custom_ic,
                q_col,
                format!("{pct_txt}{compact_lbl}"),
            );
            return Some((full_spans, compact_spans));
        }
        let full_body = format!("{pct_txt}{lbl_part}{amt_note}{reset_suffix}");

        // Compact drops the default `quota` label when an icon already carries it.
        let compact_lbl = if label.is_empty() || (label == "quota" && !icon.is_empty()) {
            String::new()
        } else {
            format!(" {}", truncate_chars_with_ellipsis(label, 8, config.glyphs))
        };
        let compact_body = format!("{pct_txt}{compact_lbl}");

        return Some((
            coalesced_icon_body_spans(icon, custom_ic, q_col, full_body),
            coalesced_icon_body_spans(icon, custom_ic, q_col, compact_body),
        ));
    }

    // Count-only fallback: crit at zero, since there is no percentage to color.
    if show_amount && let Some(amt) = bucket.remaining_amount {
        let icon = slot
            .icon
            .as_deref()
            .unwrap_or_else(|| glyphs::quota_icon(config.glyphs, 100));
        let base_col = slot_fg_color(slot, ColorToken::Ok);
        let q_col: ColorSpec = if amt <= 0 {
            ColorToken::Crit.into()
        } else {
            base_col
        };
        let lbl_part = if label.is_empty() {
            String::new()
        } else {
            format!(" {label}")
        };
        let full_body = format!("{amt}{lbl_part}{reset_suffix}");
        let compact_lbl = if label.is_empty() || (label == "quota" && !icon.is_empty()) {
            String::new()
        } else {
            format!(" {}", truncate_chars_with_ellipsis(label, 8, config.glyphs))
        };
        let compact_body = format!("{amt}{compact_lbl}");
        return Some((
            coalesced_icon_body_spans(icon, custom_ic, q_col, full_body),
            coalesced_icon_body_spans(icon, custom_ic, q_col, compact_body),
        ));
    }

    None
}

/// Reset countdown suffix such as ` (2d3h)`; `None` when no positive remainder. Prefers
/// `reset_in_seconds`, then an RFC3339 `reset_time`. No reference counterpart.
fn format_reset_suffix(bucket: &QuotaBucketData, now_sec: i64) -> Option<String> {
    let rem_secs = if let Some(s) = bucket.reset_in_seconds.filter(|&v| v > 0) {
        s
    } else if let Some(ref ts_str) = bucket.reset_time
        && let Some(epoch) = parse_rfc3339_epoch_secs(ts_str)
    {
        epoch.saturating_sub(now_sec)
    } else {
        0
    };
    if rem_secs <= 0 {
        return None;
    }
    let formatted = if rem_secs >= 86_400 {
        let days = rem_secs / 86_400;
        let hours = (rem_secs % 86_400) / 3600;
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if rem_secs >= 3600 {
        let hours = rem_secs / 3600;
        let mins = (rem_secs % 3600) / 60;
        if mins > 0 {
            format!("{hours}h{mins:02}m")
        } else {
            format!("{hours}h")
        }
    } else if rem_secs >= 60 {
        let mins = rem_secs / 60;
        format!("{mins}m")
    } else {
        format!("{rem_secs}s")
    };
    Some(format!(" ({formatted})"))
}

/// Fixed-shape RFC3339 parser, so the printing path needs no date library. Accepts `Z`, `+HH:MM`,
/// `+HHMM` or no offset, and leap second 60; `None` on any malformed field.
pub(crate) fn parse_rfc3339_epoch_secs(raw: &str) -> Option<i64> {
    let s = raw.trim();
    // Length and ASCII checks first, so the byte slicing below cannot panic.
    if s.len() < 19 || !s.is_ascii() {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year: i32 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let min: u32 = s[14..16].parse().ok()?;
    let sec: u32 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let mut rest = &s[19..];
    if let Some(after_dot) = rest.strip_prefix('.') {
        let digits = after_dot.bytes().take_while(u8::is_ascii_digit).count();
        rest = &after_dot[digits..];
    }

    let offset_secs: i64 = if rest.eq_ignore_ascii_case("Z") || rest.is_empty() {
        0
    } else {
        let sign = rest.bytes().next().filter(|c| *c == b'+' || *c == b'-')?;
        let tz = &rest[1..];
        let (tz_h, tz_m) = if tz.len() == 5 && tz.as_bytes()[2] == b':' {
            (tz[0..2].parse::<i64>().ok()?, tz[3..5].parse::<i64>().ok()?)
        } else if tz.len() == 4 {
            (tz[0..2].parse::<i64>().ok()?, tz[2..4].parse::<i64>().ok()?)
        } else {
            return None;
        };
        let total = tz_h * 3600 + tz_m * 60;
        if sign == b'+' { total } else { -total }
    };

    let days = days_from_civil(year, month, day);
    let utc_secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec);
    Some(utc_secs - offset_secs)
}

/// Hinnant's `days_from_civil`: a March-based year puts the leap day last, 146097 is the days in
/// a 400-year era, and 719468 shifts the result to the Unix epoch.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reference cost format: two decimals at $1 and above, three below.
fn format_usd(usd: f64) -> String {
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.3}")
    }
}

/// Subagent share trims one trailing zero below $1 (`$0.10`, not `$0.100`). Agyline-only: the
/// reference's agents segment shows counts, not cost.
fn format_subagent_usd(usd: f64) -> String {
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else {
        let s = format!("${usd:.3}");
        s.strip_suffix('0').map(str::to_string).unwrap_or(s)
    }
}

/// Reference token format: `1.2M` at a million, rounded `210k` at a thousand, raw below.
/// The k branch rounds ties to even, matching `[Math]::Round` and Python `round`.
pub fn format_compact_tokens(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", (tokens as f64) / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!(
            "{:.0}k",
            normalize_zero(((tokens as f64) / 1_000.0).round_ties_even())
        )
    } else {
        tokens.to_string()
    }
}

/// Cumulative session token counter: `token 4.6M (4.5M↑ 122k↓)`.
pub fn eval_tokens(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let (tok_in, tok_out) = if let Some(tt) = frame.transcript_totals
        && payload.total_input_tokens == 0
        && (tt.total_in > 0 || tt.total_out > 0)
    {
        (tt.total_in.max(0), tt.total_out.max(0))
    } else if payload.total_input_tokens > 0 || payload.total_output_tokens > 0 {
        (
            payload.total_input_tokens.max(0),
            payload.total_output_tokens.max(0),
        )
    } else {
        (
            payload
                .input_tokens
                .max(0)
                .saturating_add(payload.cache_read_input_tokens.max(0))
                .saturating_add(payload.cache_creation_input_tokens.max(0)),
            payload.output_tokens.max(0),
        )
    };
    let total = tok_in.saturating_add(tok_out);
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::tokens_icon(config.glyphs));
    let custom_ic = slot_custom_icon_color(slot);
    let num_col = slot_fg_color(slot, ColorToken::Separator);
    let bold = TextStyle::PLAIN.bold();

    let mut compact_spans = Vec::with_capacity(3);
    if !icon.is_empty() {
        compact_spans.push(StyledSpan::new(
            custom_ic.unwrap_or(ColorSpec::Token(ColorToken::Muted)),
            format!("{icon} "),
        ));
    }
    compact_spans.push(StyledSpan::new(ColorToken::Muted, "token "));
    compact_spans.push(StyledSpan::styled(
        num_col,
        bold,
        format_compact_tokens(total),
    ));

    let mut full_spans = compact_spans.clone();
    if slot.show_breakdown.unwrap_or(true) {
        full_spans.push(StyledSpan::new(
            ColorToken::Muted,
            format!(
                " ({}{} {}{})",
                format_compact_tokens(tok_in),
                glyphs::token_in_arrow(config.glyphs),
                format_compact_tokens(tok_out),
                glyphs::token_out_arrow(config.glyphs),
            ),
        ));
    }

    Some(EvaluatedItem {
        id: ItemId::Tokens,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

/// Grouped 5-hour + weekly `Quota Group` pair for one model family (`gemini`, `3p`, or `@model`).
pub fn eval_quota_group(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let fam_raw = slot
        .family
        .as_deref()
        .or(slot.bucket.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("gemini");

    let (key_5h, key_wk, default_label): (String, String, String) =
        if fam_raw.eq_ignore_ascii_case("gemini") {
            (
                "gemini-5h".to_string(),
                "gemini-weekly".to_string(),
                "Gemini".to_string(),
            )
        } else if fam_raw.eq_ignore_ascii_case("3p") {
            (
                "3p-5h".to_string(),
                "3p-weekly".to_string(),
                "3P".to_string(),
            )
        } else if fam_raw.eq_ignore_ascii_case("@model") {
            let k5 = model_quota_bucket(&payload.model_id, ModelQuotaPeriod::FiveHours).or_else(
                || model_quota_bucket(&payload.model_display_name, ModelQuotaPeriod::FiveHours),
            )?;
            let kw =
                model_quota_bucket(&payload.model_id, ModelQuotaPeriod::Weekly).or_else(|| {
                    model_quota_bucket(&payload.model_display_name, ModelQuotaPeriod::Weekly)
                })?;
            let lbl = if k5.starts_with("gemini") {
                "Gemini"
            } else {
                "3P"
            };
            (k5.to_string(), kw.to_string(), lbl.to_string())
        } else {
            (
                format!("{fam_raw}-5h"),
                format!("{fam_raw}-weekly"),
                fam_raw.to_string(),
            )
        };

    let show_amount = slot.show_amount.unwrap_or(true);
    let is_usable = |b: &QuotaBucketData| -> bool {
        !b.disabled
            && (b
                .remaining_fraction
                .is_some_and(|rf| (0.0..=1.0).contains(&rf))
                || (show_amount && b.remaining_amount.is_some()))
    };
    let find_bucket = |key: &str| -> Option<&QuotaBucketData> {
        payload
            .quota_buckets
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(key) && is_usable(b))
    };

    let b_5h = find_bucket(&key_5h);
    let b_wk = find_bucket(&key_wk);
    if b_5h.is_none() && b_wk.is_none() {
        return None;
    }

    let label = slot
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_label.as_str());
    let show_used = slot.show_used.unwrap_or(true);
    let show_bar = slot.show_bar.unwrap_or(true);
    let show_reset = slot.show_reset.unwrap_or(true);
    let bar_w = usize::from(slot.bar_width.unwrap_or(10).clamp(4, 20));
    let bar_style = slot
        .bar_style
        .as_deref()
        .and_then(BarStyle::from_key)
        .unwrap_or_default();
    let ev_5h = b_5h.map(|b| (b, eval_quota_group_bucket(b, show_used, &config.thresholds)));
    let ev_wk = b_wk.map(|b| (b, eval_quota_group_bucket(b, show_used, &config.thresholds)));
    let (worst_urgency, worst_rem_pct) = [ev_5h.as_ref(), ev_wk.as_ref()]
        .into_iter()
        .flatten()
        .fold((QuotaUrgency::Ok, 100_i32), |(acc_u, acc_pct), (_, ev)| {
            (acc_u.max(ev.urgency), acc_pct.min(ev.rem_pct))
        });
    let icon = labeled_slot_icon(slot, config.glyphs, |g| {
        glyphs::quota_icon(g, worst_rem_pct)
    });
    let custom_ic = slot_custom_icon_color(slot);
    let base_ok = slot_fg_color(slot, ColorToken::Ok);
    let default_icon_col = worst_urgency.color(base_ok);

    let mut full_spans = Vec::with_capacity(12);
    let mut compact_spans = Vec::with_capacity(8);
    if !icon.is_empty() {
        let ic_col = custom_ic.unwrap_or(default_icon_col);
        full_spans.push(StyledSpan::new(ic_col, format!("{icon} ")));
        compact_spans.push(StyledSpan::new(ic_col, format!("{icon} ")));
    }
    full_spans.push(StyledSpan::new(ColorToken::Muted, format!("{label} ")));
    compact_spans.push(StyledSpan::new(ColorToken::Muted, format!("{label} ")));

    let append_entry = |dst: &mut Vec<StyledSpan>,
                        period: &str,
                        b: &QuotaBucketData,
                        ev: QuotaGroupBucketEval,
                        with_bar: bool,
                        with_reset: bool| {
        dst.push(StyledSpan::new(ColorToken::Muted, format!("{period} ")));
        if let Some(disp_tenths) = ev.disp_tenths {
            let col = ev.urgency.color(base_ok);
            let bar_pct = (disp_tenths as f64) / 10.0;
            if with_bar {
                dst.extend(render_usage_bar(bar_pct, bar_w, col, config, bar_style));
                dst.push(StyledSpan::new(
                    col,
                    format!(" {}.{}%", disp_tenths / 10, disp_tenths % 10),
                ));
            } else {
                dst.push(StyledSpan::new(
                    col,
                    format!("{}.{}%", disp_tenths / 10, disp_tenths % 10),
                ));
            }
        } else if let Some(amt) = b.remaining_amount {
            let col: ColorSpec = if amt <= 0 {
                ColorToken::Crit.into()
            } else {
                ColorToken::Separator.into()
            };
            dst.push(StyledSpan::styled(
                col,
                TextStyle::PLAIN.bold(),
                format!("{amt} left"),
            ));
        }

        if with_reset && let Some(reset_str) = format_hud_reset(b, frame.now_sec) {
            dst.push(StyledSpan::new(ColorToken::Muted, format!(" {reset_str}")));
        }
    };

    if let Some((b, ev)) = ev_5h {
        append_entry(&mut full_spans, "5h", b, ev, show_bar, show_reset);
        append_entry(&mut compact_spans, "5h", b, ev, false, false);
    }
    if ev_5h.is_some() && ev_wk.is_some() {
        full_spans.push(StyledSpan::new(ColorToken::Muted, " / "));
        compact_spans.push(StyledSpan::new(ColorToken::Muted, " / "));
    }
    if let Some((b, ev)) = ev_wk {
        append_entry(&mut full_spans, "week", b, ev, show_bar, show_reset);
        append_entry(&mut compact_spans, "wk", b, ev, false, false);
    }

    Some(EvaluatedItem {
        id: ItemId::QuotaGroup,
        priority: slot.effective_priority(),
        full_spans,
        compact_spans,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum QuotaUrgency {
    Ok,
    Warn,
    Crit,
}

impl QuotaUrgency {
    fn color(self, base_ok: ColorSpec) -> ColorSpec {
        match self {
            Self::Ok => base_ok,
            Self::Warn => ColorToken::Warn.into(),
            Self::Crit => ColorToken::Crit.into(),
        }
    }
}

#[derive(Clone, Copy)]
struct QuotaGroupBucketEval {
    urgency: QuotaUrgency,
    rem_pct: i32,
    disp_tenths: Option<i32>,
}

fn eval_quota_group_bucket(
    b: &QuotaBucketData,
    show_used: bool,
    thresholds: &ThresholdsConfig,
) -> QuotaGroupBucketEval {
    if let Some(rf) = b.remaining_fraction.filter(|v| (0.0..=1.0).contains(v)) {
        let used_tenths = (((1.0 - rf) * 1000.0).round() as i32).clamp(0, 1000);
        let rem_tenths = ((rf * 1000.0).round() as i32).clamp(0, 1000);
        let rem_pct = ((rf * 100.0).round() as i32).clamp(0, 100);
        let (disp_tenths, urgency) = if show_used {
            let crit_t = (100 - thresholds.quota_crit) * 10;
            let warn_t = (100 - thresholds.quota_warn) * 10;
            let u = if used_tenths >= crit_t {
                QuotaUrgency::Crit
            } else if used_tenths >= warn_t {
                QuotaUrgency::Warn
            } else {
                QuotaUrgency::Ok
            };
            (used_tenths, u)
        } else {
            let u = if rem_tenths < thresholds.quota_crit * 10 {
                QuotaUrgency::Crit
            } else if rem_tenths < thresholds.quota_warn * 10 {
                QuotaUrgency::Warn
            } else {
                QuotaUrgency::Ok
            };
            (rem_tenths, u)
        };
        QuotaGroupBucketEval {
            urgency,
            rem_pct,
            disp_tenths: Some(disp_tenths),
        }
    } else if let Some(amt) = b.remaining_amount
        && amt <= 0
    {
        QuotaGroupBucketEval {
            urgency: QuotaUrgency::Crit,
            rem_pct: 0,
            disp_tenths: None,
        }
    } else {
        QuotaGroupBucketEval {
            urgency: QuotaUrgency::Ok,
            rem_pct: 100,
            disp_tenths: None,
        }
    }
}

fn format_hud_reset(bucket: &QuotaBucketData, now_sec: i64) -> Option<String> {
    let rem_secs = if let Some(s) = bucket.reset_in_seconds.filter(|&v| v > 0) {
        s
    } else if let Some(ref ts_str) = bucket.reset_time
        && let Some(epoch) = parse_rfc3339_epoch_secs(ts_str)
    {
        epoch.saturating_sub(now_sec)
    } else {
        return None;
    };
    if rem_secs <= 0 {
        return None;
    }
    let mins = (rem_secs + 59) / 60;
    if mins >= 1440 {
        Some(format!("{}d{}h", mins / 1440, (mins % 1440) / 60))
    } else if mins >= 60 {
        Some(format!("{}h{}m", mins / 60, mins % 60))
    } else {
        Some(format!("{mins}m"))
    }
}

/// Maps -0.0 to 0.0: rounding a small negative remainder to zero would otherwise print `-0%`.
fn normalize_zero(v: f64) -> f64 {
    if v == 0.0 { 0.0 } else { v }
}
