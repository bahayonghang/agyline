use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId};
use crate::render::{ColorToken, StyledItem, StyledSpan, glyphs};

pub fn eval_model(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let payload = &frame.payload;
    let raw_source = if !payload.model_display_name.trim().is_empty() {
        payload.model_display_name.as_str()
    } else if !payload.model_id.trim().is_empty() {
        payload.model_id.as_str()
    } else {
        "Model"
    };

    let (base_model, extracted_effort) = split_model_and_effort(raw_source);
    let raw_effort = if !payload.model_effort.trim().is_empty() {
        payload.model_effort.as_str()
    } else {
        extracted_effort
    };
    let effort_label = format_effort_label(raw_effort);
    let mut pretty_base = format_pretty_model_slug(base_model);

    if let Some(resp_model) = &frame.response_model {
        let trimmed_resp = resp_model.trim();
        if !trimmed_resp.is_empty() {
            let pretty_resp = format_pretty_model_slug(trimmed_resp);
            let norm_base = normalize_alnum_lower(&pretty_base);
            let norm_resp = normalize_alnum_lower(&pretty_resp);
            if !norm_resp.is_empty() && norm_resp != norm_base && !norm_base.contains(&norm_resp) {
                pretty_base = pretty_resp;
            }
        }
    }

    let icon = glyphs::model_icon(config.glyphs);
    let spans = if effort_label.is_empty() {
        vec![StyledSpan::new(
            ColorToken::Model,
            format!("{icon} {pretty_base}"),
        )]
    } else {
        vec![
            StyledSpan::new(ColorToken::Model, format!("{icon} {pretty_base}")),
            StyledSpan::new(ColorToken::Separator, glyphs::dot_sep(config.glyphs)),
            StyledSpan::new(ColorToken::Model, effort_label),
        ]
    };

    Some(StyledItem::new(ItemId::Model, spans))
}

pub fn eval_dir(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let cwd = &frame.payload.cwd;
    if cwd.trim().is_empty() {
        return None;
    }
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    let leaf = if trimmed.is_empty() {
        cwd.as_str()
    } else {
        match trimmed.rsplit(['\\', '/']).next() {
            Some(seg) if !seg.is_empty() => seg,
            Some(_) | None => trimmed,
        }
    };
    if leaf.is_empty() {
        return None;
    }
    Some(StyledItem::new(
        ItemId::Dir,
        vec![
            StyledSpan::new(ColorToken::DirIcon, glyphs::folder_icon(config.glyphs)),
            StyledSpan::new(ColorToken::Dir, format!(" {leaf}")),
        ],
    ))
}

pub fn eval_git(frame: &FrameData, config: &EffectiveConfig) -> Option<StyledItem> {
    let git = frame.git.as_ref().filter(|g| !g.branch.trim().is_empty())?;
    let br_short = if git.branch.chars().count() > 24 {
        let mut s: String = git.branch.chars().take(23).collect();
        s.push_str(glyphs::ellipsis(config.glyphs));
        s
    } else {
        git.branch.clone()
    };

    let mut ab_txt = String::new();
    if git.ahead > 0 {
        ab_txt.push_str(&format!(
            " {}{}",
            glyphs::git_ahead_prefix(config.glyphs),
            git.ahead
        ));
    }
    if git.behind > 0 {
        ab_txt.push_str(&format!(
            " {}{}",
            glyphs::git_behind_prefix(config.glyphs),
            git.behind
        ));
    }

    let icon = glyphs::git_icon(config.glyphs);
    let spans = if git.has_conflict {
        vec![
            StyledSpan::new(ColorToken::Git, icon),
            StyledSpan::new(
                ColorToken::Crit,
                format!(" {br_short} {}{ab_txt}", glyphs::warn_mark(config.glyphs)),
            ),
        ]
    } else if git.dirty {
        vec![StyledSpan::new(
            ColorToken::Git,
            format!(
                "{icon} {br_short} {}{ab_txt}",
                glyphs::git_dirty_mark(config.glyphs)
            ),
        )]
    } else {
        vec![StyledSpan::new(
            ColorToken::Git,
            format!(
                "{icon} {br_short} {}{ab_txt}",
                glyphs::git_clean_mark(config.glyphs)
            ),
        )]
    };

    Some(StyledItem::new(ItemId::Git, spans))
}

fn split_model_and_effort(raw: &str) -> (&str, &str) {
    let trimmed = raw.trim();
    if let Some(without_close) = trimmed.strip_suffix(')')
        && let Some(open_idx) = without_close.rfind('(')
    {
        let inner = &without_close[open_idx + 1..];
        if !inner.is_empty() && !inner.contains(')') {
            return (without_close[..open_idx].trim(), inner.trim());
        }
    }
    if let Some((left, right)) = raw.split_once('\u{00b7}') {
        let right_trimmed = right.trim();
        if !right_trimmed.is_empty() {
            return (left.trim(), right_trimmed);
        }
    }
    (raw, "")
}

pub fn format_effort_label(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    match s.to_ascii_lowercase().as_str() {
        "minimal" | "low" => "Low".to_string(),
        "medium" | "med" => "Med".to_string(),
        "high" => "High".to_string(),
        "xhigh" | "very_high" | "very high" => "xHigh".to_string(),
        "max" => "Max".to_string(),
        _ => s.to_string(),
    }
}

pub fn format_pretty_model_slug(slug: &str) -> String {
    let mut s = slug.trim();
    if s.is_empty() {
        return "Model".to_string();
    }

    if s.len() >= 7 && s.as_bytes()[..7].eq_ignore_ascii_case(b"models/") {
        s = &s[7..];
    }
    if s.len() >= 9 {
        let tail = &s.as_bytes()[s.len() - 9..];
        if tail[0] == b'-'
            && tail[1] == b'2'
            && tail[2] == b'0'
            && tail[3..].iter().all(u8::is_ascii_digit)
        {
            s = &s[..s.len() - 9];
        }
    }
    if s.len() >= 14 {
        let tail = &s.as_bytes()[s.len() - 14..];
        if tail[..9].eq_ignore_ascii_case(b"-preview-")
            && tail[9].is_ascii_digit()
            && tail[10].is_ascii_digit()
            && tail[11] == b'-'
            && tail[12].is_ascii_digit()
            && tail[13].is_ascii_digit()
        {
            s = &s[..s.len() - 14];
        } else if s.len() >= 8 && s.as_bytes()[s.len() - 8..].eq_ignore_ascii_case(b"-preview") {
            s = &s[..s.len() - 8];
        }
    } else if s.len() >= 8 && s.as_bytes()[s.len() - 8..].eq_ignore_ascii_case(b"-preview") {
        s = &s[..s.len() - 8];
    }
    if s.len() >= 7 && s.as_bytes()[s.len() - 7..].eq_ignore_ascii_case(b"-latest") {
        s = &s[..s.len() - 7];
    }

    if let Some(claude) = try_format_claude(s) {
        return claude;
    }
    if let Some(gpt) = try_format_gpt(s) {
        return gpt;
    }
    if s.contains('-')
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
        })
    {
        let mut words = Vec::new();
        for part in s.split('-') {
            if part.is_empty() {
                continue;
            }
            if is_number_with_unit(part) {
                words.push(part.to_string());
            } else if part.len() <= 2 && is_lower_letters_then_digits(part) {
                words.push(part.to_ascii_uppercase());
            } else {
                let mut chars = part.chars();
                if let Some(first) = chars.next() {
                    let mut w = String::with_capacity(part.len());
                    w.push(first.to_ascii_uppercase());
                    w.push_str(chars.as_str());
                    words.push(w);
                }
            }
        }
        if !words.is_empty() {
            return words.join(" ");
        }
    }

    s.to_string()
}

fn try_format_claude(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() < 3 || !parts[0].eq_ignore_ascii_case("claude") {
        return None;
    }

    if let Some(fam) = claude_family_title(parts[1])
        && is_non_empty_digits(parts[2])
    {
        let major = parts[2];
        let (minor, has_1m) = match &parts[3..] {
            [] => (None, false),
            [p3] if p3.eq_ignore_ascii_case("1m") => (None, true),
            [p3] if is_non_empty_digits(p3) => (Some(*p3), false),
            [p3, p4] if is_non_empty_digits(p3) && p4.eq_ignore_ascii_case("1m") => {
                (Some(*p3), true)
            }
            _ => return None,
        };
        let ver = match minor {
            Some(m) => format!("{major}.{m}"),
            None => major.to_string(),
        };
        let suf = if has_1m { " 1M" } else { "" };
        return Some(format!("{fam} {ver}{suf}"));
    }

    if is_non_empty_digits(parts[1]) {
        let major = parts[1];
        if let Some(fam) = claude_family_title(parts[2]) {
            let has_1m = match &parts[3..] {
                [] => false,
                [p3] if p3.eq_ignore_ascii_case("1m") => true,
                _ => return None,
            };
            let suf = if has_1m { " 1M" } else { "" };
            return Some(format!("{fam} {major}{suf}"));
        }
        if parts.len() >= 4
            && is_non_empty_digits(parts[2])
            && let Some(fam) = claude_family_title(parts[3])
        {
            let minor = parts[2];
            let has_1m = match &parts[4..] {
                [] => false,
                [p4] if p4.eq_ignore_ascii_case("1m") => true,
                _ => return None,
            };
            let suf = if has_1m { " 1M" } else { "" };
            return Some(format!("{fam} {major}.{minor}{suf}"));
        }
    }

    None
}

fn claude_family_title(part: &str) -> Option<&'static str> {
    if part.eq_ignore_ascii_case("fable") {
        Some("Fable")
    } else if part.eq_ignore_ascii_case("opus") {
        Some("Opus")
    } else if part.eq_ignore_ascii_case("sonnet") {
        Some("Sonnet")
    } else if part.eq_ignore_ascii_case("haiku") {
        Some("Haiku")
    } else {
        None
    }
}

fn try_format_gpt(s: &str) -> Option<String> {
    if s.len() < 5 || !s.as_bytes()[..4].eq_ignore_ascii_case(b"gpt-") {
        return None;
    }
    let after_prefix = &s[4..];
    let major_len = after_prefix.bytes().take_while(u8::is_ascii_digit).count();
    if major_len == 0 {
        return None;
    }
    let major = &after_prefix[..major_len];
    let after_major = &after_prefix[major_len..];

    let (ver, rest_raw) = if let Some(after_dash) = after_major.strip_prefix('-') {
        let minor_len = after_dash.bytes().take_while(u8::is_ascii_digit).count();
        if minor_len > 0 {
            (
                format!("{major}.{}", &after_dash[..minor_len]),
                &after_dash[minor_len..],
            )
        } else {
            (major.to_string(), after_major)
        }
    } else {
        (major.to_string(), after_major)
    };

    let cleaned = rest_raw.replace('-', " ");
    let words: Vec<String> = cleaned
        .split_whitespace()
        .map(dotnet_titlecase_word)
        .collect();
    if words.is_empty() {
        Some(format!("GPT-{ver}"))
    } else {
        Some(format!("GPT-{ver} {}", words.join(" ")))
    }
}

fn dotnet_titlecase_word(word: &str) -> String {
    let lower = word.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut capitalized_first_letter = false;
    for ch in lower.chars() {
        if !capitalized_first_letter && ch.is_alphabetic() {
            for up in ch.to_uppercase() {
                out.push(up);
            }
            capitalized_first_letter = true;
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_non_empty_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_number_with_unit(part: &str) -> bool {
    let bytes = part.as_bytes();
    let d1 = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if d1 == 0 {
        return false;
    }
    let mut idx = d1;
    if idx < bytes.len() && bytes[idx] == b'.' {
        let d2 = bytes[idx + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if d2 == 0 {
            return false;
        }
        idx += 1 + d2;
    }
    bytes[idx..].iter().all(u8::is_ascii_alphabetic)
}

fn is_lower_letters_then_digits(part: &str) -> bool {
    let bytes = part.as_bytes();
    let letters = bytes.iter().take_while(|b| b.is_ascii_lowercase()).count();
    letters > 0 && letters < bytes.len() && bytes[letters..].iter().all(u8::is_ascii_digit)
}

fn normalize_alnum_lower(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
