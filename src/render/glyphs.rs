//! Icon and mark vocabulary for each `GlyphSet`. `nerd` reproduces the reference's Nerd Font
//! codepoints; `unicode` and `ascii` exist because an unpatched font draws PUA icons as tofu.
//! The sets are not width-equivalent, so layout must measure whichever set is configured.

use crate::config::GlyphSet;

pub fn model_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{ef23}",
        GlyphSet::Unicode => "◆",
        GlyphSet::Ascii => "*",
    }
}

pub fn folder_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f024b}",
        GlyphSet::Unicode => "▸",
        GlyphSet::Ascii => "dir:",
    }
}

pub fn git_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f02a2}",
        GlyphSet::Unicode => "⎇",
        GlyphSet::Ascii => "git:",
    }
}

pub fn context_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f49b}",
        GlyphSet::Unicode => "◔",
        GlyphSet::Ascii => "ctx:",
    }
}

pub fn cache_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f1c0}",
        GlyphSet::Unicode => "⛁",
        GlyphSet::Ascii => "cache:",
    }
}

pub fn bolt_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f0e7}",
        GlyphSet::Unicode => "⚡",
        GlyphSet::Ascii => "spd:",
    }
}

pub fn subagents_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f0e8}",
        GlyphSet::Unicode => "☍",
        GlyphSet::Ascii => "sub:",
    }
}

pub fn tasks_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f013}",
        GlyphSet::Unicode => "⚙",
        GlyphSet::Ascii => "tsk:",
    }
}

pub fn queue_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f01c}",
        GlyphSet::Unicode => "📥",
        GlyphSet::Ascii => "q:",
    }
}

pub fn sandbox_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f132}",
        GlyphSet::Unicode => "⛨",
        GlyphSet::Ascii => "sbx:",
    }
}

pub fn vim_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{e62b}",
        GlyphSet::Unicode => "V",
        GlyphSet::Ascii => "VIM",
    }
}

pub fn title_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f0b79}",
        GlyphSet::Unicode => "💬",
        GlyphSet::Ascii => "ttl:",
    }
}

pub fn agent_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f06a9}",
        GlyphSet::Unicode => "@",
        GlyphSet::Ascii => "@",
    }
}

pub fn artifacts_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f1c9}",
        GlyphSet::Unicode => "▤",
        GlyphSet::Ascii => "art:",
    }
}

pub fn plan_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => "\u{f01a5}",
        GlyphSet::Unicode => "★",
        GlyphSet::Ascii => "plan:",
    }
}

/// Battery glyph for remaining quota percent. The 90/70/45/20 steps and their Nerd codepoints
/// reproduce `statusline.py`; only the `nerd` set varies by level.
pub fn quota_icon(glyphs: GlyphSet, quota_pct: i32) -> &'static str {
    match glyphs {
        GlyphSet::Nerd => {
            if quota_pct >= 90 {
                "\u{f0079}"
            } else if quota_pct >= 70 {
                "\u{f0081}"
            } else if quota_pct >= 45 {
                "\u{f007e}"
            } else if quota_pct >= 20 {
                "\u{f007c}"
            } else {
                "\u{f007a}"
            }
        }
        GlyphSet::Unicode => "▮",
        GlyphSet::Ascii => "qta:",
    }
}

/// The reference's ` · ` joiner for model/effort and quota/cost; ascii falls back to ` . `.
pub fn dot_sep(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => " \u{00b7} ",
        GlyphSet::Ascii => " . ",
    }
}

// The marks below are ordinary Unicode, not PUA, so nerd and unicode share them; only the icon
// functions above need a patched font.
pub fn relay_live_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{21bb}",
        GlyphSet::Ascii => "R",
    }
}

pub fn relay_fail_icon(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{2717}",
        GlyphSet::Ascii => "X",
    }
}

pub fn git_clean_mark(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{2713}",
        GlyphSet::Ascii => "ok",
    }
}

pub fn git_dirty_mark(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{25cf}",
        GlyphSet::Ascii => "*",
    }
}

pub fn warn_mark(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{26a0}",
        GlyphSet::Ascii => "!",
    }
}

pub fn git_ahead_prefix(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{2191}",
        GlyphSet::Ascii => "+",
    }
}

pub fn git_behind_prefix(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{2193}",
        GlyphSet::Ascii => "-",
    }
}

pub fn mode_chevrons(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{25b8}\u{25b8}",
        GlyphSet::Ascii => ">>",
    }
}

/// Truncation marker for layout's last stage; ascii `...` costs 3 columns where `…` costs 1.
pub fn ellipsis(glyphs: GlyphSet) -> &'static str {
    match glyphs {
        GlyphSet::Nerd | GlyphSet::Unicode => "\u{2026}",
        GlyphSet::Ascii => "...",
    }
}
