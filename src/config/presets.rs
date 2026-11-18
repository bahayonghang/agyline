//! Preset catalog: five built-ins plus custom TOML files under `<home>/presets`. A preset is a
//! whole `ConfigDoc` applied by `PresetScope`, so the configurator can swap layout without
//! restyling. `cometix` keeps the layout from statusline.ps1.

use std::fs;
use std::path::Path;

use crate::config::{ConfigDoc, ItemId, ItemSlot, PresetScope, parse_config_overlay};

/// Picker entry: `name` is the value written to the `preset` key, `summary` is display text only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetInfo {
    pub name: &'static str,
    pub summary: &'static str,
}

const BUILTIN_PRESETS: [PresetInfo; 5] = [
    PresetInfo {
        name: "cometix",
        summary: "Reference layout with a combined quota and cost item",
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
    PresetInfo {
        name: "default",
        summary: "Workspace and usage with 5-hour and weekly limits for the current model",
    },
];

pub fn builtin_presets() -> &'static [PresetInfo] {
    &BUILTIN_PRESETS
}

/// Built-in names first (in catalog order), then custom preset file stems sorted. A custom
/// file whose stem matches a built-in case-insensitively is shadowed, so a built-in cannot be
/// overridden by file. A missing directory yields just the built-ins.
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

/// The reference layout: `statusline.ps1`'s `$line1` (7 items) then `$line2` (5), in that order.
/// `budget` is the reference's quota-plus-cost pair drawn as one segment, not two.
pub fn default_cometix_lines() -> Vec<Vec<ItemSlot>> {
    workspace_and_runtime_lines(ItemId::Budget)
}

pub(super) fn workspace_and_runtime_lines(usage: ItemId) -> Vec<Vec<ItemSlot>> {
    vec![
        vec![
            ItemSlot::new(ItemId::Model),
            ItemSlot::new(ItemId::Dir),
            ItemSlot::new(ItemId::Git),
            ItemSlot::new(ItemId::Context),
            ItemSlot::new(ItemId::Cache),
            ItemSlot::new(ItemId::Speed),
            ItemSlot::new(usage),
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

/// Whole document for `name`, or `None` when it is unknown. Built-ins ignore `home_dir`; other
/// names are read from `<home>/presets/<name>.toml`, so `None` also covers a missing home or file.
/// Each built-in starts from `cometix_base`, keeping reference defaults for keys it does not set.
pub fn build_preset_doc(name: &str, home_dir: Option<&Path>) -> Option<ConfigDoc> {
    let key = name.trim().to_ascii_lowercase();
    match key.as_str() {
        "default" => Some(ConfigDoc::default()),
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
                // Serialized even when empty, so turning single_line off still yields two rows.
                Vec::new(),
            ];
            Some(doc)
        }
        "dashboard" => {
            let mut doc = ConfigDoc::cometix_base();
            doc.preset = "dashboard".to_string();
            doc.palette = "tokyo-night".to_string();

            // Two quota slots pin separate buckets by name; the reference default shows only one.
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
            // Bakes in what AGY_SHOW_DEFAULT_MODE=1 does for the reference line.
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

            // Narrowing drops priority 5 first: cache at 1 survives longest, git at 4 goes early.
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

/// Copies only the fields in `scope` and always renames `doc.preset`; returns false for an unknown
/// preset, leaving `doc` untouched. `All` means layout plus style, not the whole file: `overflow`,
/// `thresholds`, `git` and `cache_relay` stay user-owned under every scope.
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
    // Reject separators, drive letters and ".." so a preset name cannot escape <home>/presets.
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
    // allow_preset = false: a custom file may not name another preset, so chaining cannot recurse.
    parse_config_overlay(tbl, &mut doc, &mut ignored_warnings, false);
    Some(doc)
}
