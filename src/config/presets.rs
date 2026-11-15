use std::fs;
use std::path::Path;

use crate::config::{ConfigDoc, ItemId, ItemSlot, PresetScope, parse_config_overlay};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetInfo {
    pub name: &'static str,
    pub summary: &'static str,
}

const BUILTIN_PRESETS: [PresetInfo; 4] = [
    PresetInfo {
        name: "cometix",
        summary: "Reference Line 1:1 default (2 lines, terminal plain, 7 + 5 items)",
    },
    PresetInfo {
        name: "minimal",
        summary: "Single-line essentials: model, dir, git, context",
    },
    PresetInfo {
        name: "dashboard",
        summary: "Full telemetry with split quota buckets, vim, agent, artifacts & subagent details",
    },
    PresetInfo {
        name: "cache-focus",
        summary: "Prompt-cache & budget oriented layout with Catppuccin Mocha palette",
    },
];

pub fn builtin_presets() -> &'static [PresetInfo] {
    &BUILTIN_PRESETS
}

pub fn discover_presets(home_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_PRESETS.iter().map(|p| p.name.to_string()).collect();
    let dir = home_dir.join("presets");
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

pub fn default_cometix_lines() -> Vec<Vec<ItemSlot>> {
    vec![
        vec![
            ItemSlot::new(ItemId::Model),
            ItemSlot::new(ItemId::Dir),
            ItemSlot::new(ItemId::Git),
            ItemSlot::new(ItemId::Context),
            ItemSlot::new(ItemId::Cache),
            ItemSlot::new(ItemId::Speed),
            ItemSlot::new(ItemId::Budget),
        ],
        vec![
            ItemSlot::new(ItemId::Mode),
            ItemSlot::new(ItemId::Subagents),
            ItemSlot::new(ItemId::Tasks),
            ItemSlot::new(ItemId::Queue),
            ItemSlot::new(ItemId::Sandbox),
        ],
    ]
}

pub fn build_preset_doc(name: &str, home_dir: Option<&Path>) -> Option<ConfigDoc> {
    let key = name.trim().to_ascii_lowercase();
    match key.as_str() {
        "cometix" => Some(ConfigDoc::cometix_base()),
        "minimal" => {
            let mut doc = ConfigDoc::cometix_base();
            doc.preset = "minimal".to_string();
            doc.single_line = true;
            doc.lines = vec![
                vec![
                    ItemSlot::new(ItemId::Model),
                    ItemSlot::new(ItemId::Dir),
                    ItemSlot::new(ItemId::Git),
                    ItemSlot::new(ItemId::Context),
                ],
                Vec::new(),
            ];
            Some(doc)
        }
        "dashboard" => {
            let mut doc = ConfigDoc::cometix_base();
            doc.preset = "dashboard".to_string();
            doc.palette = "tokyo-night".to_string();

            let mut q_gem = ItemSlot::new(ItemId::Quota);
            q_gem.bucket = Some("gemini-5h".to_string());
            q_gem.label = Some("gem".to_string());
            q_gem.show_reset = Some(true);

            let mut q_3p = ItemSlot::new(ItemId::Quota);
            q_3p.bucket = Some("3p-5h".to_string());
            q_3p.label = Some("3p".to_string());
            q_3p.show_reset = Some(true);

            let mut cost = ItemSlot::new(ItemId::Cost);
            cost.show_subagent = Some(true);

            let mut mode = ItemSlot::new(ItemId::Mode);
            mode.show_default = Some(true);

            let mut subagents = ItemSlot::new(ItemId::Subagents);
            subagents.show_detail = Some(true);

            doc.lines = vec![
                vec![
                    ItemSlot::new(ItemId::Model),
                    ItemSlot::new(ItemId::Plan),
                    ItemSlot::new(ItemId::Dir),
                    ItemSlot::new(ItemId::Git),
                    ItemSlot::new(ItemId::Context),
                    ItemSlot::new(ItemId::Cache),
                    ItemSlot::new(ItemId::Speed),
                    q_gem,
                    q_3p,
                    cost,
                ],
                vec![
                    mode,
                    ItemSlot::new(ItemId::Vim),
                    ItemSlot::new(ItemId::Agent),
                    subagents,
                    ItemSlot::new(ItemId::Tasks),
                    ItemSlot::new(ItemId::Queue),
                    ItemSlot::new(ItemId::Artifacts),
                    ItemSlot::new(ItemId::Sandbox),
                    ItemSlot::new(ItemId::Version),
                ],
            ];
            Some(doc)
        }
        "cache-focus" => {
            let mut doc = ConfigDoc::cometix_base();
            doc.preset = "cache-focus".to_string();
            doc.palette = "catppuccin-mocha".to_string();

            let mut ctx = ItemSlot::new(ItemId::Context);
            ctx.show_remaining = Some(true);

            let mut cache = ItemSlot::new(ItemId::Cache);
            cache.priority = Some(1);

            let mut budget = ItemSlot::new(ItemId::Budget);
            budget.show_reset = Some(true);
            budget.priority = Some(2);

            let mut git = ItemSlot::new(ItemId::Git);
            git.priority = Some(4);

            doc.lines = vec![
                vec![ItemSlot::new(ItemId::Model), ctx, cache, budget, git],
                vec![
                    ItemSlot::new(ItemId::Mode),
                    ItemSlot::new(ItemId::Subagents),
                    ItemSlot::new(ItemId::Queue),
                ],
            ];
            Some(doc)
        }
        _ => {
            let dir = home_dir?;
            load_custom_preset_doc(dir, name)
        }
    }
}

pub fn apply_preset(
    doc: &mut ConfigDoc,
    preset_name: &str,
    scope: PresetScope,
    home_dir: Option<&Path>,
) -> bool {
    let Some(built) = build_preset_doc(preset_name, home_dir) else {
        return false;
    };
    doc.preset = built.preset;
    match scope {
        PresetScope::All => {
            doc.lines = built.lines;
            doc.single_line = built.single_line;
            doc.palette = built.palette;
            doc.theme = built.theme;
            doc.glyphs = built.glyphs;
            doc.color_depth = built.color_depth;
            doc.separator = built.separator;
            doc.indent = built.indent;
            doc.palette_overrides = built.palette_overrides;
        }
        PresetScope::Layout => {
            doc.lines = built.lines;
            doc.single_line = built.single_line;
        }
        PresetScope::Style => {
            doc.palette = built.palette;
            doc.theme = built.theme;
            doc.glyphs = built.glyphs;
            doc.color_depth = built.color_depth;
            doc.separator = built.separator;
            doc.indent = built.indent;
            doc.palette_overrides = built.palette_overrides;
        }
    }
    true
}

fn load_custom_preset_doc(home_dir: &Path, name: &str) -> Option<ConfigDoc> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains(['/', '\\', ':', '\0'])
        || trimmed.contains("..")
    {
        return None;
    }
    let path = home_dir.join("presets").join(format!("{trimmed}.toml"));
    let raw = fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&raw).ok()?;
    let tbl = val.as_table()?;
    let mut doc = ConfigDoc::cometix_base();
    doc.preset = trimmed.to_string();
    let mut ignored_warnings = Vec::new();
    parse_config_overlay(tbl, &mut doc, &mut ignored_warnings, false);
    Some(doc)
}
