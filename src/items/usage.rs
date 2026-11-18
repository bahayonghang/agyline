//! Usage Segments 4-7: context window, prompt-cache TTL and hit rate, latched output speed, quota
//! buckets and USD cost. The 300s cache window, the 60s warn boundary, the 32K keepalive floor and
//! the 50/80% context and 20/45% quota thresholds reproduce `ref/agy-patch/statusline.ps1` and
//! `statusline.py`. Inputs are already in `FrameData`; nothing here reads state or the network.

use crate::collect::{FrameData, RelayStatus};
use crate::config::{EffectiveConfig, ItemId, ItemSlot, ModelQuotaPeriod};
use crate::input::QuotaBucketData;
use crate::items::{
    EvaluatedItem, coalesced_icon_body_spans, slot_custom_icon_color, slot_fg_color,
    truncate_chars_with_ellipsis,
};
use crate::render::{ColorSpec, ColorToken, StyledSpan, glyphs};

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

    // `show_remaining` is an agyline option; the reference only prints used percent.
    let show_remaining = slot.show_remaining.unwrap_or(false);
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
    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::cache_icon(config.glyphs));
    let base_col = slot_fg_color(slot, ColorToken::Cache);
    let icon_col = slot_custom_icon_color(slot).unwrap_or(base_col);

    let payload = &frame.payload;
    let cur_in = payload.input_tokens.max(0);
    let cache_read = payload.cache_read_input_tokens.max(0);
    let tot_prompt = cur_in.saturating_add(cache_read);
    // Hit rate truncates instead of rounding, like the reference's integer division.
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
        // Ties-to-even percent, matching `[Math]::Round` in the reference.
        let quota_pct = (rf * 100.0).round_ties_even() as i32;
        let icon = slot
            .icon
            .as_deref()
            .unwrap_or_else(|| glyphs::quota_icon(config.glyphs, quota_pct));
        let base_col = slot_fg_color(slot, ColorToken::Ok);
        let q_col: ColorSpec = if quota_pct < config.thresholds.quota_crit {
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
        let full_body = format!("{quota_pct}%{lbl_part}{amt_note}{reset_suffix}");

        // Compact drops the default `quota` label when an icon already carries it.
        let compact_lbl = if label.is_empty() || (label == "quota" && !icon.is_empty()) {
            String::new()
        } else {
            format!(" {}", truncate_chars_with_ellipsis(label, 8, config.glyphs))
        };
        let compact_body = format!("{quota_pct}%{compact_lbl}");

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
fn parse_rfc3339_epoch_secs(raw: &str) -> Option<i64> {
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

/// Maps -0.0 to 0.0: rounding a small negative remainder to zero would otherwise print `-0%`.
fn normalize_zero(v: f64) -> f64 {
    if v == 0.0 { 0.0 } else { v }
}
