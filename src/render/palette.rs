use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::config::{ColorDepth, PaletteOverrides, ThemePolarity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ColorToken {
    Separator,
    Muted,
    Model,
    DirIcon,
    Dir,
    Git,
    Context,
    Cache,
    Ok,
    Warn,
    Crit,
    Auto,
}

impl ColorToken {
    pub const ALL: [Self; 12] = [
        Self::Separator,
        Self::Muted,
        Self::Model,
        Self::DirIcon,
        Self::Dir,
        Self::Git,
        Self::Context,
        Self::Cache,
        Self::Ok,
        Self::Warn,
        Self::Crit,
        Self::Auto,
    ];

    pub const fn index(self) -> usize {
        match self {
            Self::Separator => 0,
            Self::Muted => 1,
            Self::Model => 2,
            Self::DirIcon => 3,
            Self::Dir => 4,
            Self::Git => 5,
            Self::Context => 6,
            Self::Cache => 7,
            Self::Ok => 8,
            Self::Warn => 9,
            Self::Crit => 10,
            Self::Auto => 11,
        }
    }

    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Separator => "separator",
            Self::Muted => "muted",
            Self::Model => "model",
            Self::DirIcon => "dir_icon",
            Self::Dir => "dir",
            Self::Git => "git",
            Self::Context => "context",
            Self::Cache => "cache",
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Crit => "crit",
            Self::Auto => "auto",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "separator" => Some(Self::Separator),
            "muted" => Some(Self::Muted),
            "model" => Some(Self::Model),
            "dir_icon" => Some(Self::DirIcon),
            "dir" => Some(Self::Dir),
            "git" => Some(Self::Git),
            "context" => Some(Self::Context),
            "cache" => Some(Self::Cache),
            "ok" => Some(Self::Ok),
            "warn" => Some(Self::Warn),
            "crit" => Some(Self::Crit),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ansi16Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl Ansi16Color {
    pub const ALL: [Self; 16] = [
        Self::Black,
        Self::Red,
        Self::Green,
        Self::Yellow,
        Self::Blue,
        Self::Magenta,
        Self::Cyan,
        Self::White,
        Self::BrightBlack,
        Self::BrightRed,
        Self::BrightGreen,
        Self::BrightYellow,
        Self::BrightBlue,
        Self::BrightMagenta,
        Self::BrightCyan,
        Self::BrightWhite,
    ];

    pub const fn sgr_code(self) -> u8 {
        match self {
            Self::Black => 30,
            Self::Red => 31,
            Self::Green => 32,
            Self::Yellow => 33,
            Self::Blue => 34,
            Self::Magenta => 35,
            Self::Cyan => 36,
            Self::White => 37,
            Self::BrightBlack => 90,
            Self::BrightRed => 91,
            Self::BrightGreen => 92,
            Self::BrightYellow => 93,
            Self::BrightBlue => 94,
            Self::BrightMagenta => 95,
            Self::BrightCyan => 96,
            Self::BrightWhite => 97,
        }
    }

    pub const fn to_rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Black => (0, 0, 0),
            Self::Red => (205, 49, 49),
            Self::Green => (13, 188, 121),
            Self::Yellow => (229, 229, 16),
            Self::Blue => (36, 114, 200),
            Self::Magenta => (188, 63, 188),
            Self::Cyan => (17, 168, 205),
            Self::White => (229, 229, 229),
            Self::BrightBlack => (102, 102, 102),
            Self::BrightRed => (241, 76, 76),
            Self::BrightGreen => (35, 209, 139),
            Self::BrightYellow => (245, 245, 67),
            Self::BrightBlue => (59, 142, 234),
            Self::BrightMagenta => (214, 112, 214),
            Self::BrightCyan => (41, 184, 219),
            Self::BrightWhite => (255, 255, 255),
        }
    }

    pub fn from_name(raw: &str) -> Option<Self> {
        let norm = raw.trim().to_ascii_lowercase().replace('-', "_");
        match norm.as_str() {
            "black" => Some(Self::Black),
            "red" => Some(Self::Red),
            "green" => Some(Self::Green),
            "yellow" => Some(Self::Yellow),
            "blue" => Some(Self::Blue),
            "magenta" => Some(Self::Magenta),
            "cyan" => Some(Self::Cyan),
            "white" => Some(Self::White),
            "gray" | "grey" | "dark_gray" | "dark_grey" | "bright_black" => Some(Self::BrightBlack),
            "bright_red" => Some(Self::BrightRed),
            "bright_green" => Some(Self::BrightGreen),
            "bright_yellow" => Some(Self::BrightYellow),
            "bright_blue" => Some(Self::BrightBlue),
            "bright_magenta" => Some(Self::BrightMagenta),
            "bright_cyan" => Some(Self::BrightCyan),
            "bright_white" => Some(Self::BrightWhite),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpec {
    Token(ColorToken),
    Ansi16(Ansi16Color),
    Ansi256(u8),
    Rgb(u8, u8, u8),
}

impl From<ColorToken> for ColorSpec {
    fn from(tok: ColorToken) -> Self {
        Self::Token(tok)
    }
}

pub fn parse_color_spec(raw: &str) -> Option<ColorSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(ColorSpec::Rgb(r, g, b));
        }
        if hex.len() == 3 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            return Some(ColorSpec::Rgb(r, g, b));
        }
        return None;
    }
    if let Some(idx_str) = trimmed
        .strip_prefix("256:")
        .or_else(|| trimmed.strip_prefix("ansi256:"))
    {
        let idx = idx_str.trim().parse::<u8>().ok()?;
        return Some(ColorSpec::Ansi256(idx));
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit())
        && let Ok(idx) = trimmed.parse::<u8>()
    {
        return Some(ColorSpec::Ansi256(idx));
    }
    if let Some(tok) = ColorToken::from_key(trimmed) {
        return Some(ColorSpec::Token(tok));
    }
    if let Some(c16) = Ansi16Color::from_name(trimmed) {
        return Some(ColorSpec::Ansi16(c16));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

pub fn resolve_effective_color_depth(
    setting: ColorDepth,
    env_vars: &BTreeMap<String, String>,
) -> EffectiveColorDepth {
    match setting {
        ColorDepth::TrueColor => EffectiveColorDepth::TrueColor,
        ColorDepth::Ansi256 => EffectiveColorDepth::Ansi256,
        ColorDepth::Ansi16 => EffectiveColorDepth::Ansi16,
        ColorDepth::None => EffectiveColorDepth::None,
        ColorDepth::Auto => {
            if env_vars.get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                return EffectiveColorDepth::None;
            }
            if let Some(term) = env_vars.get("TERM").map(|s| s.trim())
                && term.eq_ignore_ascii_case("dumb")
            {
                return EffectiveColorDepth::None;
            }
            if let Some(ct) = env_vars.get("COLORTERM").map(|s| s.trim())
                && !ct.is_empty()
            {
                let low = ct.to_ascii_lowercase();
                if low.contains("truecolor") || low.contains("24bit") {
                    return EffectiveColorDepth::TrueColor;
                }
                if low.contains("256") || low.contains("8bit") {
                    return EffectiveColorDepth::Ansi256;
                }
            }
            if let Some(term) = env_vars.get("TERM").map(|s| s.trim())
                && !term.is_empty()
            {
                let low = term.to_ascii_lowercase();
                if low.contains("truecolor") || low.contains("24bit") || low.contains("direct") {
                    return EffectiveColorDepth::TrueColor;
                }
                if low.contains("256color") {
                    return EffectiveColorDepth::Ansi256;
                }
                if matches!(low.as_str(), "xterm" | "vt100" | "linux" | "ansi") {
                    return EffectiveColorDepth::Ansi16;
                }
            }
            EffectiveColorDepth::TrueColor
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedColor {
    Ansi16(Ansi16Color),
    Ansi256(u8),
    Rgb(u8, u8, u8),
}

impl ResolvedColor {
    pub fn quantize(self, depth: EffectiveColorDepth) -> Option<Self> {
        match depth {
            EffectiveColorDepth::None => None,
            EffectiveColorDepth::TrueColor => Some(self),
            EffectiveColorDepth::Ansi256 => match self {
                Self::Ansi16(c) => Some(Self::Ansi16(c)),
                Self::Ansi256(n) => Some(Self::Ansi256(n)),
                Self::Rgb(r, g, b) => Some(Self::Ansi256(rgb_to_ansi256(r, g, b))),
            },
            EffectiveColorDepth::Ansi16 => match self {
                Self::Ansi16(c) => Some(Self::Ansi16(c)),
                Self::Ansi256(n) => {
                    let (r, g, b) = ansi256_to_rgb(n);
                    Some(Self::Ansi16(rgb_to_ansi16(r, g, b)))
                }
                Self::Rgb(r, g, b) => Some(Self::Ansi16(rgb_to_ansi16(r, g, b))),
            },
        }
    }

    pub fn write_sgr(self, out: &mut String) {
        match self {
            Self::Ansi16(c) => {
                let _ = write!(out, "\x1b[0;{}m", c.sgr_code());
            }
            Self::Ansi256(idx) => {
                let _ = write!(out, "\x1b[0;38;5;{idx}m");
            }
            Self::Rgb(r, g, b) => {
                let _ = write!(out, "\x1b[0;38;2;{r};{g};{b}m");
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalettePolarity {
    Dual,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteInfo {
    pub name: &'static str,
    pub polarity: PalettePolarity,
}

const BUILTIN_PALETTES: [PaletteInfo; 8] = [
    PaletteInfo {
        name: "terminal",
        polarity: PalettePolarity::Dual,
    },
    PaletteInfo {
        name: "catppuccin-mocha",
        polarity: PalettePolarity::Dark,
    },
    PaletteInfo {
        name: "catppuccin-latte",
        polarity: PalettePolarity::Light,
    },
    PaletteInfo {
        name: "tokyo-night",
        polarity: PalettePolarity::Dark,
    },
    PaletteInfo {
        name: "nord",
        polarity: PalettePolarity::Dark,
    },
    PaletteInfo {
        name: "gruvbox",
        polarity: PalettePolarity::Dark,
    },
    PaletteInfo {
        name: "rose-pine",
        polarity: PalettePolarity::Dark,
    },
    PaletteInfo {
        name: "dracula",
        polarity: PalettePolarity::Dark,
    },
];

pub fn builtin_palettes() -> &'static [PaletteInfo] {
    &BUILTIN_PALETTES
}

pub fn discover_palettes(home_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_PALETTES
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    let dir = home_dir.join("palettes");
    if let Ok(entries) = fs::read_dir(dir) {
        let mut custom = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && !stem.is_empty()
                && !names.iter().any(|n| n.eq_ignore_ascii_case(stem))
            {
                custom.push(stem.to_string());
            }
        }
        custom.sort();
        custom.dedup();
        names.extend(custom);
    }
    names
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPalette {
    colors: [ResolvedColor; 12],
    pub polarity_fell_back: bool,
}

impl ResolvedPalette {
    pub const fn terminal(polarity: ThemePolarity) -> Self {
        Self {
            colors: terminal_colors(polarity),
            polarity_fell_back: false,
        }
    }

    pub const fn get(&self, token: ColorToken) -> ResolvedColor {
        self.colors[token.index()]
    }

    pub fn resolve_spec(&self, spec: ColorSpec) -> ResolvedColor {
        match spec {
            ColorSpec::Token(tok) => self.get(tok),
            ColorSpec::Ansi16(c) => ResolvedColor::Ansi16(c),
            ColorSpec::Ansi256(n) => ResolvedColor::Ansi256(n),
            ColorSpec::Rgb(r, g, b) => ResolvedColor::Rgb(r, g, b),
        }
    }
}

pub fn resolve_palette(
    palette_name: &str,
    polarity: ThemePolarity,
    overrides: &PaletteOverrides,
    home_dir: Option<&Path>,
) -> ResolvedPalette {
    let key = palette_name.trim().to_ascii_lowercase();
    let (mut base, fell_back) = if key.is_empty() || key == "terminal" {
        (terminal_colors(polarity), false)
    } else if let Some((pal_pol, pal_colors)) = builtin_rgb_palette(&key) {
        if polarity_matches(pal_pol, polarity) {
            (pal_colors, false)
        } else {
            (terminal_colors(polarity), true)
        }
    } else if let Some(dir) = home_dir
        && let Some((custom_pol, dark_cols, light_cols)) = load_custom_palette(dir, palette_name)
    {
        if polarity_matches(custom_pol, polarity) {
            let chosen = match polarity {
                ThemePolarity::Dark => dark_cols,
                ThemePolarity::Light => light_cols,
            };
            (chosen, false)
        } else {
            (terminal_colors(polarity), true)
        }
    } else {
        (terminal_colors(polarity), false)
    };

    let snapshot = base;
    for tok in ColorToken::ALL {
        if let Some(raw_ov) = overrides.get(tok)
            && let Some(spec) = parse_color_spec(raw_ov)
        {
            base[tok.index()] = match spec {
                ColorSpec::Token(ref_tok) => snapshot[ref_tok.index()],
                ColorSpec::Ansi16(c) => ResolvedColor::Ansi16(c),
                ColorSpec::Ansi256(n) => ResolvedColor::Ansi256(n),
                ColorSpec::Rgb(r, g, b) => ResolvedColor::Rgb(r, g, b),
            };
        }
    }

    ResolvedPalette {
        colors: base,
        polarity_fell_back: fell_back,
    }
}

const fn polarity_matches(pal_pol: PalettePolarity, target: ThemePolarity) -> bool {
    match pal_pol {
        PalettePolarity::Dual => true,
        PalettePolarity::Dark => matches!(target, ThemePolarity::Dark),
        PalettePolarity::Light => matches!(target, ThemePolarity::Light),
    }
}

const fn terminal_colors(polarity: ThemePolarity) -> [ResolvedColor; 12] {
    match polarity {
        ThemePolarity::Dark => [
            ResolvedColor::Ansi16(Ansi16Color::White),
            ResolvedColor::Ansi16(Ansi16Color::BrightBlack),
            ResolvedColor::Ansi16(Ansi16Color::BrightCyan),
            ResolvedColor::Ansi16(Ansi16Color::BrightYellow),
            ResolvedColor::Ansi16(Ansi16Color::BrightGreen),
            ResolvedColor::Ansi16(Ansi16Color::BrightBlue),
            ResolvedColor::Ansi16(Ansi16Color::BrightMagenta),
            ResolvedColor::Ansi16(Ansi16Color::Blue),
            ResolvedColor::Ansi16(Ansi16Color::BrightGreen),
            ResolvedColor::Ansi16(Ansi16Color::Yellow),
            ResolvedColor::Ansi16(Ansi16Color::Red),
            ResolvedColor::Ansi16(Ansi16Color::BrightRed),
        ],
        ThemePolarity::Light => [
            ResolvedColor::Ansi16(Ansi16Color::BrightBlack),
            ResolvedColor::Ansi16(Ansi16Color::BrightBlack),
            ResolvedColor::Ansi16(Ansi16Color::Cyan),
            ResolvedColor::Ansi16(Ansi16Color::Yellow),
            ResolvedColor::Ansi16(Ansi16Color::Green),
            ResolvedColor::Ansi16(Ansi16Color::Blue),
            ResolvedColor::Ansi16(Ansi16Color::Magenta),
            ResolvedColor::Ansi16(Ansi16Color::Blue),
            ResolvedColor::Ansi16(Ansi16Color::Green),
            ResolvedColor::Ansi16(Ansi16Color::Yellow),
            ResolvedColor::Ansi16(Ansi16Color::Red),
            ResolvedColor::Ansi16(Ansi16Color::Red),
        ],
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> ResolvedColor {
    ResolvedColor::Rgb(r, g, b)
}

fn builtin_rgb_palette(key: &str) -> Option<(PalettePolarity, [ResolvedColor; 12])> {
    match key {
        "catppuccin-mocha" => Some((
            PalettePolarity::Dark,
            [
                rgb(0x6c, 0x70, 0x86),
                rgb(0x7f, 0x84, 0x9c),
                rgb(0x89, 0xdc, 0xeb),
                rgb(0xf9, 0xe2, 0xaf),
                rgb(0xa6, 0xe3, 0xa1),
                rgb(0x89, 0xb4, 0xfa),
                rgb(0xcb, 0xa6, 0xf7),
                rgb(0x74, 0xc7, 0xec),
                rgb(0xa6, 0xe3, 0xa1),
                rgb(0xf9, 0xe2, 0xaf),
                rgb(0xf3, 0x8b, 0xa8),
                rgb(0xeb, 0xa0, 0xac),
            ],
        )),
        "catppuccin-latte" => Some((
            PalettePolarity::Light,
            [
                rgb(0x8c, 0x8f, 0xa1),
                rgb(0x7c, 0x7f, 0x93),
                rgb(0x04, 0xa5, 0xe5),
                rgb(0xdf, 0x8e, 0x1d),
                rgb(0x40, 0xa0, 0x2b),
                rgb(0x1e, 0x66, 0xf5),
                rgb(0x88, 0x39, 0xef),
                rgb(0x20, 0x9f, 0xb5),
                rgb(0x40, 0xa0, 0x2b),
                rgb(0xdf, 0x8e, 0x1d),
                rgb(0xd2, 0x0f, 0x39),
                rgb(0xe6, 0x45, 0x53),
            ],
        )),
        "tokyo-night" => Some((
            PalettePolarity::Dark,
            [
                rgb(0x56, 0x5f, 0x89),
                rgb(0x56, 0x5f, 0x89),
                rgb(0x7d, 0xcf, 0xff),
                rgb(0xe0, 0xaf, 0x68),
                rgb(0x9e, 0xce, 0x6a),
                rgb(0x7a, 0xa2, 0xf7),
                rgb(0xbb, 0x9a, 0xf7),
                rgb(0x2a, 0xc3, 0xde),
                rgb(0x9e, 0xce, 0x6a),
                rgb(0xe0, 0xaf, 0x68),
                rgb(0xf7, 0x76, 0x8e),
                rgb(0xff, 0x9e, 0x64),
            ],
        )),
        "nord" => Some((
            PalettePolarity::Dark,
            [
                rgb(0x61, 0x6e, 0x88),
                rgb(0x61, 0x6e, 0x88),
                rgb(0x88, 0xc0, 0xd0),
                rgb(0xeb, 0xcb, 0x8b),
                rgb(0xa3, 0xbe, 0x8c),
                rgb(0x81, 0xa1, 0xc1),
                rgb(0xb4, 0x8e, 0xad),
                rgb(0x5e, 0x81, 0xac),
                rgb(0xa3, 0xbe, 0x8c),
                rgb(0xeb, 0xcb, 0x8b),
                rgb(0xbf, 0x61, 0x6a),
                rgb(0xd0, 0x87, 0x70),
            ],
        )),
        "gruvbox" => Some((
            PalettePolarity::Dark,
            [
                rgb(0x92, 0x83, 0x74),
                rgb(0x92, 0x83, 0x74),
                rgb(0x8e, 0xc0, 0x7c),
                rgb(0xfa, 0xbd, 0x2f),
                rgb(0xb8, 0xbb, 0x26),
                rgb(0x83, 0xa5, 0x98),
                rgb(0xd3, 0x86, 0x9b),
                rgb(0x45, 0x85, 0x88),
                rgb(0xb8, 0xbb, 0x26),
                rgb(0xfa, 0xbd, 0x2f),
                rgb(0xfb, 0x49, 0x34),
                rgb(0xfe, 0x80, 0x19),
            ],
        )),
        "rose-pine" => Some((
            PalettePolarity::Dark,
            [
                rgb(0x6e, 0x6a, 0x86),
                rgb(0x6e, 0x6a, 0x86),
                rgb(0x9c, 0xcf, 0xd8),
                rgb(0xf6, 0xc1, 0x77),
                rgb(0x31, 0x74, 0x8f),
                rgb(0xc4, 0xa7, 0xe7),
                rgb(0xeb, 0xbc, 0xba),
                rgb(0x31, 0x74, 0x8f),
                rgb(0x9c, 0xcf, 0xd8),
                rgb(0xf6, 0xc1, 0x77),
                rgb(0xeb, 0x6f, 0x92),
                rgb(0xeb, 0x6f, 0x92),
            ],
        )),
        "dracula" => Some((
            PalettePolarity::Dark,
            [
                rgb(0x62, 0x72, 0xa4),
                rgb(0x62, 0x72, 0xa4),
                rgb(0x8b, 0xe9, 0xfd),
                rgb(0xf1, 0xfa, 0x8c),
                rgb(0x50, 0xfa, 0x7b),
                rgb(0xbd, 0x93, 0xf9),
                rgb(0xff, 0x79, 0xc6),
                rgb(0x8b, 0xe9, 0xfd),
                rgb(0x50, 0xfa, 0x7b),
                rgb(0xf1, 0xfa, 0x8c),
                rgb(0xff, 0x55, 0x55),
                rgb(0xff, 0xb8, 0x6c),
            ],
        )),
        _ => None,
    }
}

fn is_safe_asset_name(name: &str) -> bool {
    let t = name.trim();
    !t.is_empty()
        && t != "."
        && t != ".."
        && !t.contains(['/', '\\', ':', '\0'])
        && !t.contains("..")
}

fn load_custom_palette(
    home_dir: &Path,
    name: &str,
) -> Option<(PalettePolarity, [ResolvedColor; 12], [ResolvedColor; 12])> {
    if !is_safe_asset_name(name) {
        return None;
    }
    let path = home_dir
        .join("palettes")
        .join(format!("{}.toml", name.trim()));
    let raw = fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&raw).ok()?;
    let tbl = val.as_table()?;

    let has_dark_tbl = tbl.get("dark").and_then(toml::Value::as_table).is_some();
    let has_light_tbl = tbl.get("light").and_then(toml::Value::as_table).is_some();
    let default_pol = if has_dark_tbl && has_light_tbl {
        PalettePolarity::Dual
    } else if has_light_tbl && !has_dark_tbl {
        PalettePolarity::Light
    } else {
        PalettePolarity::Dark
    };

    let pol = match tbl
        .get("polarity")
        .and_then(toml::Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("dual") => PalettePolarity::Dual,
        Some("light") => PalettePolarity::Light,
        Some("dark") => PalettePolarity::Dark,
        Some(_) | None => default_pol,
    };

    let mut dark = terminal_colors(ThemePolarity::Dark);
    let mut light = terminal_colors(ThemePolarity::Light);

    apply_toml_color_map(tbl, &mut dark);
    apply_toml_color_map(tbl, &mut light);

    if let Some(colors_tbl) = tbl.get("colors").and_then(toml::Value::as_table) {
        apply_toml_color_map(colors_tbl, &mut dark);
        apply_toml_color_map(colors_tbl, &mut light);
    }
    if let Some(dark_tbl) = tbl.get("dark").and_then(toml::Value::as_table) {
        apply_toml_color_map(dark_tbl, &mut dark);
    }
    if let Some(light_tbl) = tbl.get("light").and_then(toml::Value::as_table) {
        apply_toml_color_map(light_tbl, &mut light);
    }

    Some((pol, dark, light))
}

fn apply_toml_color_map(tbl: &toml::map::Map<String, toml::Value>, dst: &mut [ResolvedColor; 12]) {
    let snap = *dst;
    for tok in ColorToken::ALL {
        if let Some(v) = tbl.get(tok.as_key())
            && let Some(spec) = toml_value_to_color_spec(v)
        {
            dst[tok.index()] = match spec {
                ColorSpec::Token(other) => snap[other.index()],
                ColorSpec::Ansi16(c) => ResolvedColor::Ansi16(c),
                ColorSpec::Ansi256(n) => ResolvedColor::Ansi256(n),
                ColorSpec::Rgb(r, g, b) => ResolvedColor::Rgb(r, g, b),
            };
        }
    }
}

pub fn toml_value_to_color_spec(val: &toml::Value) -> Option<ColorSpec> {
    match val {
        toml::Value::String(s) => parse_color_spec(s),
        toml::Value::Integer(i) => u8::try_from(*i).ok().map(ColorSpec::Ansi256),
        toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_)
        | toml::Value::Array(_)
        | toml::Value::Table(_) => None,
    }
}

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let ri = nearest_cube_idx(r);
    let gi = nearest_cube_idx(g);
    let bi = nearest_cube_idx(b);
    let cr = CUBE_LEVELS[ri];
    let cg = CUBE_LEVELS[gi];
    let cb = CUBE_LEVELS[bi];
    let cube_dist = color_dist_sq(r, g, b, cr, cg, cb);
    let cube_code = 16 + (36 * ri as u8) + (6 * gi as u8) + (bi as u8);

    let avg = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
    let gray_step = if avg < 8 {
        0
    } else if avg > 238 {
        23
    } else {
        ((u16::from(avg) - 8 + 5) / 10).min(23) as u8
    };
    let gv = 8 + gray_step * 10;
    let gray_dist = color_dist_sq(r, g, b, gv, gv, gv);
    if gray_dist < cube_dist {
        232 + gray_step
    } else {
        cube_code
    }
}

fn nearest_cube_idx(v: u8) -> usize {
    let mut best_i = 0;
    let mut best_d = u16::MAX;
    for (idx, &lvl) in CUBE_LEVELS.iter().enumerate() {
        let d = v.abs_diff(lvl) as u16;
        if d < best_d {
            best_d = d;
            best_i = idx;
        }
    }
    best_i
}

fn ansi256_to_rgb(idx: u8) -> (u8, u8, u8) {
    if idx < 16 {
        Ansi16Color::ALL[idx as usize].to_rgb()
    } else if idx < 232 {
        let c = idx - 16;
        let r = CUBE_LEVELS[(c / 36) as usize];
        let g = CUBE_LEVELS[((c % 36) / 6) as usize];
        let b = CUBE_LEVELS[(c % 6) as usize];
        (r, g, b)
    } else {
        let v = 8 + (idx - 232) * 10;
        (v, v, v)
    }
}

fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Ansi16Color {
    let mut best = Ansi16Color::White;
    let mut best_dist = u32::MAX;
    for candidate in Ansi16Color::ALL {
        let (cr, cg, cb) = candidate.to_rgb();
        let d = color_dist_sq(r, g, b, cr, cg, cb);
        if d < best_dist {
            best_dist = d;
            best = candidate;
        }
    }
    best
}

fn color_dist_sq(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = i32::from(r1) - i32::from(r2);
    let dg = i32::from(g1) - i32::from(g2);
    let db = i32::from(b1) - i32::from(b2);
    (dr * dr + dg * dg + db * db) as u32
}
