use crate::collect::{FrameData, RelayStatus};
use crate::config::{EffectiveConfig, ItemId};
use crate::render::{ColorToken, StyledItem, StyledSpan, glyphs};

pub fn eval_context(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
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

    let ctx_col = if used_pct >= config.thresholds.context_crit {
        ColorToken::Crit
    } else if used_pct >= config.thresholds.context_warn {
        ColorToken::Warn
    } else {
        ColorToken::Context
    };

    let pct_fmt = if used_pct >= 10.0 || used_pct == 0.0 {
        format!("{:.0}%", normalize_zero(used_pct.round_ties_even()))
    } else {
        format!("{used_pct:.1}%")
    };

    let icon = glyphs::context_icon(config.glyphs);
    let spans = if used_tokens > 0 {
        let tok_fmt = format_compact_tokens(used_tokens);
        vec![
            StyledSpan::new(ColorToken::Context, icon),
            StyledSpan::new(ctx_col, format!(" {pct_fmt}")),
            StyledSpan::new(ColorToken::Separator, glyphs::dot_sep(config.glyphs)),
            StyledSpan::new(ctx_col, format!("{tok_fmt} tokens")),
        ]
    } else {
        vec![StyledSpan::new(
            ColorToken::Context,
            format!("{icon} {pct_fmt}"),
        )]
    };

    Some(StyledItem::new(ItemId::Context, spans))
}

pub fn eval_cache(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let icon = glyphs::cache_icon(config.glyphs);
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
        return Some(StyledItem::new(
            ItemId::Cache,
            vec![
                StyledSpan::new(ColorToken::Cache, icon),
                StyledSpan::new(ColorToken::Muted, " cold"),
            ],
        ));
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

    let mut spans = Vec::with_capacity(5);
    spans.push(StyledSpan::new(ColorToken::Cache, icon));

    if cache_rem > 0 {
        let mm = cache_rem / 60;
        let ss = cache_rem % 60;
        let ttl_col = if (cache_rem as u64) > config.thresholds.cache_warn_secs {
            ColorToken::Cache
        } else {
            ColorToken::Warn
        };
        spans.push(StyledSpan::new(ttl_col, format!(" {mm}:{ss:02}")));
        if let Some(badge) = keep_badge {
            spans.push(badge);
        }
        if tot_prompt > 0 {
            spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
            spans.push(StyledSpan::new(ttl_col, format!("{cache_pct}% hit")));
        }
    } else {
        spans.push(StyledSpan::new(ColorToken::Warn, " cold"));
        if let Some(badge) = keep_badge {
            spans.push(badge);
        }
        if tot_prompt > 0 {
            spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
            spans.push(StyledSpan::new(
                ColorToken::Warn,
                format!("{cache_pct}% hit"),
            ));
        }
    }

    Some(StyledItem::new(ItemId::Cache, spans))
}

pub fn eval_speed(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let tps = frame
        .last_tps
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "--")?;
    let icon = glyphs::bolt_icon(config.glyphs);
    Some(StyledItem::new(
        ItemId::Speed,
        vec![StyledSpan::new(
            ColorToken::Ok,
            format!("{icon} {tps} tok/s"),
        )],
    ))
}

pub fn eval_budget(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let payload = &frame.payload;
    let mut min_frac: Option<f64> = None;
    for bucket in &payload.quota_buckets {
        if bucket.disabled {
            continue;
        }
        if let Some(rf) = bucket.remaining_fraction
            && (0.0..=1.0).contains(&rf)
        {
            min_frac = Some(match min_frac {
                Some(cur) if cur <= rf => cur,
                Some(_) | None => rf,
            });
        }
    }

    let mut spans = Vec::with_capacity(3);
    if let Some(rf) = min_frac {
        let quota_pct = (rf * 100.0).round_ties_even() as i32;
        let q_icon = glyphs::quota_icon(config.glyphs, quota_pct);
        let q_col = if quota_pct < config.thresholds.quota_crit {
            ColorToken::Crit
        } else if quota_pct < config.thresholds.quota_warn {
            ColorToken::Warn
        } else {
            ColorToken::Ok
        };
        spans.push(StyledSpan::new(
            q_col,
            format!("{q_icon} {quota_pct}% quota"),
        ));
    }

    if payload.cost_usd > 0.0 {
        if !spans.is_empty() {
            spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
        }
        let cost_fmt = if payload.cost_usd >= 1.0 {
            format!("${:.2}", payload.cost_usd)
        } else {
            format!("${:.3}", payload.cost_usd)
        };
        spans.push(StyledSpan::new(ColorToken::Warn, cost_fmt));
    }

    if spans.is_empty() {
        None
    } else {
        Some(StyledItem::new(ItemId::Budget, spans))
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
