//! Workspace segments: model plus effort, directory leaf, and git state. Effort labels, model-slug
//! cleanup and the dirty/conflict marks reproduce Segments 1-3 of `ref/agy-patch/statusline.ps1`
//! and `statusline.py`. Compact widths are agyline's own; the reference prints a single width. Git
//! state is already resolved in `FrameData`: nothing here runs `git` or reads the filesystem.

use std::path::Path;

use crate::collect::FrameData;
use crate::config::{EffectiveConfig, GlyphSet, ItemId, ItemSlot};
use crate::items::{
    EvaluatedItem, coalesced_icon_body_spans, slot_custom_icon_color, slot_fg_color,
    truncate_chars_with_ellipsis,
};
use crate::render::{ColorSpec, ColorToken, StyledSpan, TextStyle, glyphs};

/// Model segment: picker display name, else `model.id`, else the literal `Model`, plus effort
/// joined by the reference's middle dot. Never `None`; the compact form shortens the name only.
pub fn eval_model(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let payload = &frame.payload;
    // Reference `$modelKeySource`: display_name, then model.id, then the literal `Model`.
    let raw_source = if !payload.model_display_name.trim().is_empty() {
        payload.model_display_name.as_str()
    } else if !payload.model_id.trim().is_empty() {
        payload.model_id.as_str()
    } else {
        "Model"
    };

    let (base_model, extracted_effort) = split_model_and_effort(raw_source);
    // `model.effort` (already merged with `thinkingLevel`) outranks a suffix parsed from the name.
    let raw_effort = if !payload.model_effort.trim().is_empty() {
        payload.model_effort.as_str()
    } else {
        extracted_effort
    };
    let effort_label = format_effort_label(raw_effort);
    let mut pretty_base = format_pretty_model_slug(base_model);

    // The DB-resolved upstream model wins only when it differs and is not already contained in the
    // picker name, so a shorter upstream id cannot cut down the displayed model.
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

    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::model_icon(config.glyphs));
    let body_col = slot_fg_color(slot, ColorToken::Model);
    let custom_ic = slot_custom_icon_color(slot);
    let compact_base = truncate_chars_with_ellipsis(&pretty_base, 20, config.glyphs);

    if slot
        .style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("hud"))
    {
        let hud_effort = if effort_label == "xHigh" {
            "XHigh".to_string()
        } else {
            effort_label
        };
        let full_body = if hud_effort.is_empty() {
            pretty_base
        } else {
            format!("{pretty_base} ({hud_effort})")
        };
        let italic = TextStyle::PLAIN.italic();
        let build_hud = |body: String| -> Vec<StyledSpan> {
            if icon.is_empty() {
                vec![StyledSpan::styled(body_col, italic, body)]
            } else if let Some(ic) = custom_ic
                && ic != body_col
            {
                vec![
                    StyledSpan::new(ic, icon),
                    StyledSpan::styled(body_col, italic, format!(" {body}")),
                ]
            } else {
                vec![
                    StyledSpan::new(body_col, format!("{icon} ")),
                    StyledSpan::styled(body_col, italic, body),
                ]
            }
        };
        return Some(EvaluatedItem {
            id: ItemId::Model,
            priority: slot.effective_priority(),
            full_spans: build_hud(full_body),
            compact_spans: build_hud(compact_base),
        });
    }

    let build = |name: String| -> Vec<StyledSpan> {
        let mut spans = coalesced_icon_body_spans(icon, custom_ic, body_col, name);
        // Effort joins through the separator-colored dot, both parts in the model color.
        if !effort_label.is_empty() {
            spans.push(StyledSpan::new(
                ColorToken::Separator,
                glyphs::dot_sep(config.glyphs),
            ));
            spans.push(StyledSpan::new(body_col, effort_label.clone()));
        }
        spans
    };

    Some(EvaluatedItem {
        id: ItemId::Model,
        priority: slot.effective_priority(),
        full_spans: build(pretty_base),
        compact_spans: build(compact_base),
    })
}

/// Directory leaf of `cwd`; `None` when cwd is blank or yields no leaf. The icon keeps the
/// reference's separate folder color while the text uses the dir color.
pub fn eval_dir(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    let cwd = &frame.payload.cwd;
    if cwd.trim().is_empty() {
        return None;
    }
    // Trailing separators go first; a root path trims to empty and keeps the original text.
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

    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::folder_icon(config.glyphs));
    let icon_col = slot_custom_icon_color(slot).unwrap_or(ColorSpec::Token(ColorToken::DirIcon));
    let body_col = slot_fg_color(slot, ColorToken::Dir);
    let compact_leaf = truncate_chars_with_ellipsis(leaf, 12, config.glyphs);

    let full_dir = if let Some(max_levels) = slot.max_levels {
        format_multi_level_dir(
            cwd,
            frame.user_home.as_deref(),
            usize::from(max_levels.clamp(1, 8)),
            config.glyphs,
        )
    } else {
        leaf.to_string()
    };

    let build = |dir_txt: &str| -> Vec<StyledSpan> {
        if icon.is_empty() {
            vec![StyledSpan::new(body_col, dir_txt)]
        } else {
            vec![
                StyledSpan::new(icon_col, icon),
                StyledSpan::new(body_col, format!(" {dir_txt}")),
            ]
        }
    };

    Some(EvaluatedItem {
        id: ItemId::Dir,
        priority: slot.effective_priority(),
        full_spans: build(&full_dir),
        compact_spans: build(&compact_leaf),
    })
}

fn format_multi_level_dir(
    cwd: &str,
    user_home: Option<&Path>,
    max_levels: usize,
    glyphs: GlyphSet,
) -> String {
    let norm_cwd = cwd.replace('\\', "/");
    let trimmed_cwd = if norm_cwd.len() > 1 {
        norm_cwd.trim_end_matches('/')
    } else {
        norm_cwd.as_str()
    };
    let norm_home = user_home.map(|h| {
        let s = h.to_string_lossy().replace('\\', "/");
        if s.len() > 1 {
            s.trim_end_matches('/').to_string()
        } else {
            s
        }
    });

    let (prefix, rel) = if let Some(ref home) = norm_home
        && !home.is_empty()
        && path_starts_with_component(trimmed_cwd, home)
    {
        ("~/", &trimmed_cwd[home.len()..])
    } else if let Some(rest) = trimmed_cwd.strip_prefix('/') {
        ("/", rest)
    } else {
        ("", trimmed_cwd)
    };

    let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        if prefix == "/" {
            "/".to_string()
        } else if prefix == "~/" {
            "~".to_string()
        } else {
            trimmed_cwd.to_string()
        }
    } else if segs.len() > max_levels {
        format!(
            "{}/{}",
            glyphs::ellipsis(glyphs),
            segs[segs.len() - max_levels..].join("/")
        )
    } else {
        format!("{prefix}{}", segs.join("/"))
    }
}

fn path_starts_with_component(path: &str, home: &str) -> bool {
    if path.len() < home.len() {
        return false;
    }
    let Some(head) = path.get(..home.len()) else {
        return false;
    };
    let matches_head = if cfg!(windows) || path.as_bytes().get(1) == Some(&b':') {
        head.eq_ignore_ascii_case(home)
    } else {
        head == home
    };
    if !matches_head {
        return false;
    }
    path.len() == home.len() || path.as_bytes()[home.len()] == b'/'
}

/// Git segment from the cached frame state; `None` when no branch is known. A conflict paints the
/// whole body crit, otherwise the reference's dirty/clean mark follows the branch.
pub fn eval_git(
    slot: &ItemSlot,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<EvaluatedItem> {
    // Branch and status come from the async git cache, so the render never waits on `git`.
    let git = frame.git.as_ref().filter(|g| !g.branch.trim().is_empty())?;

    // Reference order: ahead then behind, both appended after the status mark.
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

    let icon = slot
        .icon
        .as_deref()
        .unwrap_or_else(|| glyphs::git_icon(config.glyphs));
    let base_col = slot_fg_color(slot, ColorToken::Git);
    let custom_ic = slot_custom_icon_color(slot);

    if slot
        .style
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("counts"))
    {
        let branch_col: ColorSpec = if git.dirty || git.has_conflict {
            ColorToken::Crit.into()
        } else {
            base_col
        };
        let build_branch_only = |limit: usize| -> Vec<StyledSpan> {
            let short = truncate_chars_with_ellipsis(&git.branch, limit, config.glyphs);
            let label = if git.dirty {
                format!("{short}*")
            } else {
                short
            };
            coalesced_icon_body_spans(icon, custom_ic, branch_col, label)
        };
        let mut full_spans = build_branch_only(24);
        if git.conflicted > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Crit,
                format!(" ={}", git.conflicted),
            ));
        }
        if git.staged > 0 {
            full_spans.push(StyledSpan::new(ColorToken::Ok, format!(" +{}", git.staged)));
        }
        if git.modified > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Warn,
                format!(" !{}", git.modified),
            ));
        }
        if git.deleted > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Crit,
                format!(
                    " {}{}",
                    glyphs::git_deleted_mark(config.glyphs),
                    git.deleted
                ),
            ));
        }
        if git.renamed > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Context,
                format!(
                    " {}{}",
                    glyphs::git_renamed_mark(config.glyphs),
                    git.renamed
                ),
            ));
        }
        if git.untracked > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Muted,
                format!(" ?{}", git.untracked),
            ));
        }
        if git.stashed > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Model,
                format!(" ${}", git.stashed),
            ));
        }
        if git.ahead > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Model,
                format!(" {}{}", glyphs::git_ahead_prefix(config.glyphs), git.ahead),
            ));
        }
        if git.behind > 0 {
            full_spans.push(StyledSpan::new(
                ColorToken::Model,
                format!(
                    " {}{}",
                    glyphs::git_behind_prefix(config.glyphs),
                    git.behind
                ),
            ));
        }
        return Some(EvaluatedItem {
            id: ItemId::Git,
            priority: slot.effective_priority(),
            full_spans,
            compact_spans: build_branch_only(12),
        });
    }

    let build = |branch_limit: usize| -> Vec<StyledSpan> {
        let br_short = truncate_chars_with_ellipsis(&git.branch, branch_limit, config.glyphs);
        // Conflict: warn mark and crit body, while the icon keeps the git color.
        if git.has_conflict {
            let body = format!("{} {}{ab_txt}", br_short, glyphs::warn_mark(config.glyphs));
            if icon.is_empty() {
                vec![StyledSpan::new(ColorToken::Crit, body)]
            } else {
                let ic = custom_ic.unwrap_or(base_col);
                vec![
                    StyledSpan::new(ic, icon),
                    StyledSpan::new(ColorToken::Crit, format!(" {body}")),
                ]
            }
        } else {
            // Reference marks: filled circle when dirty, check when clean.
            let mark = if git.dirty {
                glyphs::git_dirty_mark(config.glyphs)
            } else {
                glyphs::git_clean_mark(config.glyphs)
            };
            let body = format!("{br_short} {mark}{ab_txt}");
            coalesced_icon_body_spans(icon, custom_ic, base_col, body)
        }
    };

    Some(EvaluatedItem {
        id: ItemId::Git,
        priority: slot.effective_priority(),
        // 24 chars is the reference's 23 plus ellipsis; 12 is agyline's compact width.
        full_spans: build(24),
        compact_spans: build(12),
    })
}

/// Splits a trailing `(High)` or middle-dot effort suffix off the display name, as the reference
/// does before formatting. Returns the input unchanged with no effort when neither shape matches.
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
    // No spaces required around the dot, following `statusline.ps1`; `statusline.py` wants them.
    if let Some((left, right)) = raw.split_once('\u{00b7}') {
        let right_trimmed = right.trim();
        if !right_trimmed.is_empty() {
            return (left.trim(), right_trimmed);
        }
    }
    (raw, "")
}

/// Reference effort labels: `Low`, `Med`, `High`, `xHigh`, `Max`. An unknown value passes through
/// trimmed; blank yields an empty string so the model segment prints no effort part.
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

/// Reference slug cleanup: drop `models/`, a `-20YYYYMMDD` stamp, `-preview[-MM-DD]`, `-latest`,
/// then apply the Claude/GPT short forms or generic kebab title case. Blank input yields `Model`.
pub fn format_pretty_model_slug(slug: &str) -> String {
    let mut s = slug.trim();
    if s.is_empty() {
        return "Model".to_string();
    }

    // Affixes match case-insensitively like PowerShell `-replace`, and each is verified ASCII
    // before the byte slice, so slicing stays on char boundaries.
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
        // Number-unit parts (`16k`, `4.5`) stay verbatim; `v2` uppercases; others capitalize once.
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

/// Both reference Claude shapes, `claude-opus-4-6` and `claude-4-6-opus`, with an optional `1m`
/// rendered as a ` 1M` suffix. `None` when the parts fit neither shape.
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

/// `gpt-5.1-codex-max` becomes `GPT-5.1 Codex Max`: the version keeps its dot and the remaining
/// words are title-cased. `None` unless the slug starts with `gpt-` and a digit.
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

/// The PowerShell reference's .NET `ToTitleCase` word rule: lowercase, then capitalize the first
/// letter. `statusline.py` uses `str.title()`, which also capitalizes after an apostrophe.
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

/// Comparison key for the upstream-model check: spaces and punctuation are ignored, so
/// `Gemini 3 Pro` and `gemini-3-pro` compare equal.
fn normalize_alnum_lower(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
