use crate::collect::{FrameData, RelayStatus};
use crate::config::{EffectiveConfig, ItemId, ItemSlot};
use crate::input::QuotaBucketData;
use crate::items::{
    EvaluatedItem, coalesced_icon_body_spans, slot_custom_icon_color, slot_fg_color,
    truncate_chars_with_ellipsis,
};
use crate::render::{ColorSpec, ColorToken, StyledSpan, glyphs};

pub fn eval_context(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    let cur_in = payload.input_tokens;
    let cache_read = payload.cache_read_input_tokens;
    let real_in = cur_in.saturating_add(cache_read);
    let win_sz = payload.context_window_size;

    let raw_pct = if real_in > 0 && win_sz > 0.0 {
        (real_in as f64) * 100.0 / win_sz
    } else {
        payload.used_percentage
    };
    let used_pct = normalize_zero(raw_pct);

    let used_tokens = if real_in > 0 {
        real_in
    } else if used_pct > 0.0 && win_sz > 0.0 {
        ((used_pct / 100.0) * win_sz).round_ties_even() as i64
    } else {
        0
    };

    let base_col = slot_fg_color(slot, ColorToken::Context);
    let ctx_col: ColorSpec = if used_pct >= config.thresholds.context_crit {
        ColorToken::Crit.into()
    } else if used_pct >= config.thresholds.context_warn {
        ColorToken::Warn.into()
    } else {
        base_col
    };

    let show_remaining = slot.show_remaining.unwrap_or(false);
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
    let icon_col = custom_ic.unwrap_or(base_col);

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
    let cache_pct = if tot_prompt > 0 {
        (cache_read.saturating_mul(100)) / tot_prompt
    } else {
        0
    };

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

    let cache_rem = 300_i64.saturating_sub(frame.now_sec.saturating_sub(cache_ts));
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

pub fn eval_quota(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let show_reset = slot.show_reset.unwrap_or(false);
    let show_amount = slot.show_amount.unwrap_or(false);
    let label = slot.label.as_deref().unwrap_or("quota");
    let bucket = select_quota_bucket(
        &frame.payload.quota_buckets,
        slot.bucket.as_deref(),
        show_amount,
    )?;
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

fn select_quota_bucket<'a>(
    buckets: &'a [QuotaBucketData],
    filter: Option<&str>,
    allow_amount_only: bool,
) -> Option<&'a QuotaBucketData> {
    let filter_trimmed = filter.map(str::trim).filter(|s| !s.is_empty());
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

fn compare_bucket_urgency(a: &&QuotaBucketData, b: &&QuotaBucketData) -> std::cmp::Ordering {
    match (a.remaining_fraction, b.remaining_fraction) {
        (Some(af), Some(bf)) => af.total_cmp(&bf),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.remaining_amount.cmp(&b.remaining_amount),
    }
}

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

fn parse_rfc3339_epoch_secs(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.len() < 19 {
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

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn format_usd(usd: f64) -> String {
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.3}")
    }
}

fn format_subagent_usd(usd: f64) -> String {
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else {
        let s = format!("${usd:.3}");
        s.strip_suffix('0').map(str::to_string).unwrap_or(s)
    }
}

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

fn normalize_zero(v: f64) -> f64 {
    if v == 0.0 { 0.0 } else { v }
}
