//! Config resolution and per-item evaluation: the precedence ladder (AGY_* env > config.toml >
//! preset > built-in default), tolerant per-field degradation, palette/color-depth SGR output,
//! and the 20-item catalog rendered through the shared width-narrowing pipeline.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agyline::{
    ColorDepth, ColorSpec, ColorToken, ConfigDoc, DEFAULT_COMPACT_AT, EffectiveConfig, GlyphSet,
    ItemId, ItemSlot, ItemStyle, NoopSpawner, OverflowMode, PresetScope, RenderEnv, SlotOutcome,
    ThemePolarity, ThemeSetting, apply_preset, builtin_palettes, builtin_presets,
    discover_palettes, discover_presets, emit_ansi, emit_plain, inspect_env_overrides, items,
    load_config_doc, parse_config_doc, render_lines_with_width, rendered_frame_to_ratatui_lines,
    resolve_separator, run_live_frame, serialize_config_doc, str_display_width,
};

static SANDBOX_SEQ: AtomicUsize = AtomicUsize::new(0);

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "agyline_cfg_test_{}_{nanos}_{seq}_{tag}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create sandbox");
        Self { root }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// Serialization round-trips losslessly: uncustomized items stay string shorthands, customized
// items become inline tables, empty sections are omitted, and parse(serialize(doc)) == doc.
#[test]
fn config_doc_serializes_shorthands_and_inline_tables_and_roundtrips() {
    let mut doc = ConfigDoc::default();
    let default_toml = serialize_config_doc(&doc);
    assert!(default_toml.contains("\"subagents\""));
    assert!(default_toml.contains("id = \"model\""));
    assert!(!default_toml.contains("[palette_overrides]"));

    let mut quota_slot = ItemSlot::new(ItemId::Quota);
    quota_slot.bucket = Some("gemini-5h".to_string());
    quota_slot.label = Some("gem".to_string());
    quota_slot.show_reset = Some(true);
    quota_slot.priority = Some(2);
    doc.lines[0].push(quota_slot);
    doc.palette_overrides
        .set(ColorToken::Model, "#89dceb".to_string());

    let custom_toml = serialize_config_doc(&doc);
    assert!(custom_toml.contains("[palette_overrides]"));
    assert!(custom_toml.contains("model = \"#89dceb\""));
    assert!(custom_toml.contains("id = \"quota\""));
    assert!(custom_toml.contains("bucket = \"gemini-5h\""));
    assert!(custom_toml.contains("label = \"gem\""));
    assert!(custom_toml.contains("show_reset = true"));
    assert!(custom_toml.contains("priority = 2"));

    let (parsed, warnings) = parse_config_doc(&custom_toml).expect("roundtrip parse");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(parsed, doc);

    // Also accept `item = "..."` inline table alias on load.
    let alias_toml = r#"
[[line]]
items = ["model", { item = "quota", bucket = "flash", label = "fl", show_reset = true }]
"#;
    let (alias_doc, alias_warnings) = parse_config_doc(alias_toml).expect("parse item alias");
    assert!(alias_warnings.is_empty());
    assert_eq!(alias_doc.lines.len(), 1);
    assert_eq!(alias_doc.lines[0].len(), 2);
    assert_eq!(alias_doc.lines[0][1].id, ItemId::Quota);
    assert_eq!(alias_doc.lines[0][1].bucket.as_deref(), Some("flash"));
    assert_eq!(alias_doc.lines[0][1].label.as_deref(), Some("fl"));
    assert_eq!(alias_doc.lines[0][1].show_reset, Some(true));
}

// Preset scope is field-limited (Layout vs Style vs All); custom presets load from AGYLINE_HOME
// and explicit config.toml keys still override the preset's fields.
#[test]
fn presets_apply_scopes_and_load_custom_presets_from_agyline_home() {
    assert_eq!(builtin_presets().len(), 7);

    let mut doc = ConfigDoc {
        separator: " / ".to_string(),
        ..ConfigDoc::default()
    };
    assert!(apply_preset(&mut doc, "minimal", PresetScope::Layout, None));
    assert!(doc.single_line);
    assert_eq!(doc.lines[0].len(), 4);
    assert_eq!(doc.separator, " / ", "Layout scope must keep style fields");

    assert!(apply_preset(
        &mut doc,
        "dashboard",
        PresetScope::Style,
        None
    ));
    assert_eq!(doc.palette, "tokyo-night");
    assert_eq!(
        doc.lines[0].len(),
        4,
        "Style scope must keep current row layout"
    );

    assert!(apply_preset(
        &mut doc,
        "cache-focus",
        PresetScope::All,
        None
    ));
    assert_eq!(doc.palette, "catppuccin-mocha");
    assert!(!doc.single_line);
    assert_eq!(doc.lines[0].len(), 5);

    let sb = Sandbox::new("custom_preset");
    let presets_dir = sb.root.join("presets");
    fs::create_dir_all(&presets_dir).expect("create presets dir");
    fs::write(
        presets_dir.join("compact-dev.toml"),
        r#"
palette = "nord"
separator = " · "
single_line = true

[[line]]
items = ["model", "git"]
"#,
    )
    .expect("write custom preset");
    fs::write(
        sb.root.join("config.toml"),
        "preset = \"compact-dev\"\nindent = \"\"\n",
    )
    .expect("write config.toml");

    let discovered = discover_presets(&sb.root);
    assert!(discovered.iter().any(|p| p == "compact-dev"));

    let eff = EffectiveConfig::resolve(&sb.root, &BTreeMap::new());
    assert_eq!(eff.preset, "compact-dev");
    assert_eq!(eff.palette, "nord");
    assert_eq!(eff.separator, " · ");
    assert_eq!(eff.indent, "");
    assert!(eff.single_line);
    assert_eq!(eff.lines.len(), 1);
    assert_eq!(eff.lines[0].len(), 2);
}

#[test]
fn palettes_color_depths_polarity_fallback_and_custom_palettes_emit_expected_sgr() {
    // Palette catalog: 10 built-ins (terminal, catppuccin, mono, plus 7 fixed-polarity RGB sets)
    // plus custom `palettes/*.toml` from AGYLINE_HOME.
    assert_eq!(builtin_palettes().len(), 10);

    let sb = Sandbox::new("palettes");
    let palettes_dir = sb.root.join("palettes");
    fs::create_dir_all(&palettes_dir).expect("create palettes dir");
    fs::write(
        palettes_dir.join("ocean.toml"),
        r##"
polarity = "dark"
separator = "#445566"
muted = "#667788"
model = "#00d7ff"
dir_icon = "#ffaf00"
dir = "#5fd787"
git = "#5fafff"
context = "#af87ff"
cache = "#00afff"
ok = "#5fd787"
warn = "#ffaf00"
crit = "#ff5f5f"
auto = "#ff875f"
"##,
    )
    .expect("write custom palette");
    assert!(discover_palettes(&sb.root).iter().any(|p| p == "ocean"));

    let spawner = NoopSpawner;
    let payload = br#"{"session_id":"","model":{"display_name":"Opus 4.6"}}"#;

    // 1. Truecolor RGB palette (`catppuccin-mocha`: model = #89dceb -> 137;220;235)
    fs::write(
        sb.root.join("config.toml"),
        r#"
palette = "catppuccin-mocha"
color_depth = "truecolor"
[[line]]
items = ["model"]
"#,
    )
    .expect("write mocha config");
    let env = RenderEnv {
        home_dir: sb.root.clone(),
        temp_dir: sb.root.join("tmp"),
        conversations_dir: sb.root.join("conv"),
        env_vars: BTreeMap::new(),
        now_ms: 1_700_000_000_000,
        spawner: &spawner,
    };
    let out_tc = run_live_frame(payload, &env);
    assert_eq!(
        out_tc.ansi,
        "  \x1b[0m\x1b[0;38;2;137;220;235m\u{ef23} Opus 4.6\x1b[0m\n"
    );

    // 2. Explicit `theme = "light"` clashing with dark-only `catppuccin-mocha` falls back to terminal light (`\x1b[0;36m`)
    fs::write(
        sb.root.join("config.toml"),
        r#"
palette = "catppuccin-mocha"
theme = "light"
[[line]]
items = ["model"]
"#,
    )
    .expect("write polarity clash config");
    let out_fallback = run_live_frame(payload, &env);
    assert_eq!(
        out_fallback.ansi,
        "  \x1b[0m\x1b[0;36m\u{ef23} Opus 4.6\x1b[0m\n"
    );

    // 3. `color_depth = "256"` quantizes RGB to `\x1b[0;38;5;<idx>m`, and `[palette_overrides]` supports integer 256 index
    fs::write(
        sb.root.join("config.toml"),
        r#"
palette = "ocean"
color_depth = "256"
[palette_overrides]
separator = 244
[[line]]
items = ["model", "context"]
"#,
    )
    .expect("write 256 config");
    let out_256 = run_live_frame(payload, &env);
    // #00d7ff is exact xterm-256 index 45 (0, 215, 255); separator override 244 -> \x1b[0;38;5;244m
    assert!(
        out_256.ansi.contains("\x1b[0;38;5;45m\u{ef23} Opus 4.6"),
        "actual: {:?}",
        out_256.ansi
    );
    assert!(
        out_256.ansi.contains("\x1b[0;38;5;244m \u{2502} "),
        "actual: {:?}",
        out_256.ansi
    );

    // 4. `color_depth = "auto"` respects NO_COLOR=1 and TERM=dumb
    fs::write(
        sb.root.join("config.toml"),
        "palette = \"catppuccin-mocha\"\ncolor_depth = \"auto\"\n[[line]]\nitems = [\"model\"]\n",
    )
    .expect("write auto depth config");
    let mut no_color_env = env.env_vars.clone();
    no_color_env.insert("NO_COLOR".to_string(), "1".to_string());
    let env_nc = RenderEnv {
        home_dir: sb.root.clone(),
        temp_dir: sb.root.join("tmp"),
        conversations_dir: sb.root.join("conv"),
        env_vars: no_color_env,
        now_ms: 1_700_000_000_000,
        spawner: &spawner,
    };
    let out_nc = run_live_frame(payload, &env_nc);
    assert_eq!(out_nc.ansi, "  \u{ef23} Opus 4.6\n");
}

// Precedence: AGY_* env beats config.toml, and COLORFGBG only steers `theme = "auto"`. The
// effective/file-only toggle applies or ignores env without mutating the loaded doc.
#[test]
fn env_vars_override_config_toml_and_colorfgbg_only_applies_to_auto_theme() {
    let sb = Sandbox::new("env_prec");
    fs::write(
        sb.root.join("config.toml"),
        r#"
theme = "dark"
indent = ">>"
single_line = false
[cache_relay]
url = "http://127.0.0.1:8787"
[[line]]
items = ["model"]
[[line]]
items = [{ id = "mode", show_default = true }]
"#,
    )
    .expect("write config");

    // COLORFGBG=0;15 (light) must NOT override explicit `theme = "dark"` in config.toml!
    let mut env_map = BTreeMap::new();
    env_map.insert("COLORFGBG".to_string(), "0;15".to_string());
    let eff_dark = EffectiveConfig::resolve(&sb.root, &env_map);
    assert_eq!(eff_dark.theme_setting, ThemeSetting::Dark);
    assert_eq!(eff_dark.polarity, ThemePolarity::Dark);

    // Explicit AGY_* overrides config.toml!
    env_map.insert("AGY_THEME".to_string(), "light".to_string());
    env_map.insert("AGY_STATUSLINE_LINES".to_string(), "1".to_string());
    env_map.insert("AGY_STATUSLINE_INDENT".to_string(), "   ".to_string());
    env_map.insert("AGY_SHOW_DEFAULT_MODE".to_string(), "0".to_string());
    env_map.insert("AGY_CACHE_RELAY".to_string(), "off".to_string());

    // Only the five AGY_* knobs count as overrides; COLORFGBG is not one of them.
    let active = inspect_env_overrides(&env_map);
    assert_eq!(active.len(), 5);

    let loaded = load_config_doc(&sb.root);
    let eff_with_env = EffectiveConfig::from_doc(&loaded.doc, Some(&sb.root), &env_map, true);
    assert_eq!(eff_with_env.polarity, ThemePolarity::Light);
    assert!(eff_with_env.single_line);
    assert_eq!(eff_with_env.indent, "   ");
    assert_eq!(eff_with_env.cache_relay_url, None);

    let eff_file_only = EffectiveConfig::from_doc(&loaded.doc, Some(&sb.root), &env_map, false);
    assert_eq!(eff_file_only.polarity, ThemePolarity::Dark);
    assert!(!eff_file_only.single_line);
    assert_eq!(eff_file_only.indent, ">>");
    assert_eq!(
        eff_file_only.cache_relay_url.as_deref(),
        Some("http://127.0.0.1:8787")
    );
}

// A syntax-broken config.toml degrades to the default line plus a red " ! config" Crit badge,
// and the loader reports the exact line/column; the live render itself stays silent.
#[test]
fn broken_toml_renders_default_line_with_crit_config_badge_and_line_col_diagnostic() {
    let sb = Sandbox::new("broken_toml");
    fs::write(
        sb.root.join("config.toml"),
        "version = 1\npalette = \"terminal\"\nthis is = [unclosed\n",
    )
    .expect("write broken config.toml");

    let loaded = load_config_doc(&sb.root);
    assert!(loaded.config_exists);
    let err = loaded.syntax_error.expect("must report TOML syntax error");
    assert_eq!(err.line, Some(3));
    assert!(err.column.is_some());

    let spawner = NoopSpawner;
    let env = RenderEnv {
        home_dir: sb.root.clone(),
        temp_dir: sb.root.join("tmp"),
        conversations_dir: sb.root.join("conv"),
        env_vars: BTreeMap::new(),
        now_ms: 1_700_000_000_000,
        spawner: &spawner,
    };
    let out = run_live_frame(b"{}", &env);
    assert_eq!(
        out.plain,
        concat!(
            "  \u{2726} Model \u{2502} \u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1} 0.0% ! config\n",
            "  \u{25cb} idle\n",
        )
    );
    let first_ansi_line = out.ansi.lines().next().expect("row 1 ansi");
    assert!(
        first_ansi_line.ends_with("\x1b[0;38;2;243;139;168m ! config\x1b[0m"),
        "actual ansi: {:?}",
        out.ansi
    );
}

// Tolerant per-field degradation: unknown keys/items/sections are ignored, invalid enums and
// inverted/negative thresholds reset to defaults, and a bad priority is dropped, not fatal.
#[test]
fn unknown_keys_unknown_items_and_inverted_thresholds_degrade_per_field() {
    let raw = r#"
version = 99
future_flag = true
glyphs = "ascii"
color_depth = "bogus_depth"
overflow = "bogus_overflow"

[thresholds]
context_warn = 90
context_crit = 40
quota_warn = 10
quota_crit = 50
cache_warn_secs = -5

[unknown_section]
foo = "bar"

[[line]]
items = ["model", "future_unknown_item", { id = "context", priority = 99, unknown_opt = 123 }]
"#;
    let (doc, _warnings) = parse_config_doc(raw).expect("tolerant parse must succeed");
    assert_eq!(doc.glyphs, GlyphSet::Ascii);
    assert_eq!(doc.color_depth, ColorDepth::Auto);
    assert_eq!(doc.overflow, OverflowMode::Fit);
    assert_eq!(doc.thresholds.context_warn, 60.0);
    assert_eq!(doc.thresholds.context_crit, 90.0);
    assert_eq!(doc.thresholds.quota_warn, 40);
    assert_eq!(doc.thresholds.quota_crit, 10);
    assert_eq!(doc.thresholds.cache_warn_secs, 60);
    assert_eq!(doc.lines.len(), 1);
    assert_eq!(doc.lines[0].len(), 2);
    assert_eq!(doc.lines[0][0].id, ItemId::Model);
    assert_eq!(doc.lines[0][1].id, ItemId::Context);
    assert_eq!(doc.lines[0][1].priority, None);
}

// The catalog is exactly the 24 ItemId::ALL entries in order; quota and quota_group are
// repeatable (`allow_multiple`), so the configurator can place independent quota buckets/families.
#[test]
fn items_catalog_exposes_all_twenty_four_items_with_priorities_and_options() {
    let cat = items::catalog();
    assert_eq!(cat.len(), 24);
    for (idx, expected_id) in ItemId::ALL.iter().copied().enumerate() {
        assert_eq!(cat[idx].id, expected_id);
        assert_eq!(cat[idx].key, expected_id.as_key());
        assert_eq!(cat[idx].default_priority, expected_id.default_priority());
    }
    let quota_meta = cat.iter().find(|c| c.id == ItemId::Quota).expect("quota");
    assert!(quota_meta.allow_multiple);
    assert_eq!(quota_meta.options.len(), 8);

    let qg_meta = cat
        .iter()
        .find(|c| c.id == ItemId::QuotaGroup)
        .expect("quota_group");
    assert!(qg_meta.allow_multiple);
    assert_eq!(qg_meta.options.len(), 9);

    let model_meta = cat.iter().find(|c| c.id == ItemId::Model).expect("model");
    assert!(!model_meta.allow_multiple);
    assert_eq!(model_meta.options.len(), 1);
}

// Renders all 20 items at once: email PII and subagent role prompts never reach the line, and
// plan_tier renders but is never persisted to session state.
#[test]
fn all_twenty_items_evaluate_full_and_compact_and_respect_privacy_and_role_exclusions() {
    let sb = Sandbox::new("twenty_items");
    fs::write(
        sb.root.join("config.toml"),
        r##"
preset = "dashboard"
palette = "terminal"
overflow = "none"

[[line]]
items = [
  { id = "model", icon_color = "#ff8800" },
  "plan",
  { id = "dir", icon = "" },
  "git",
  { id = "context", show_remaining = true, fg = "cyan" },
  "cache",
  "speed",
  { id = "quota", bucket = "gemini-5h", label = "gem", show_reset = true },
  { id = "quota", bucket = "3p-5h", label = "3p", show_reset = true },
  { id = "quota", bucket = "credits", label = "cred", show_amount = true },
  { id = "cost", show_subagent = true },
]

[[line]]
items = [
  { id = "mode", show_default = true, compact = true },
  "vim",
  "agent",
  { id = "subagents", show_detail = true },
  "tasks",
  "queue",
  "artifacts",
  "sandbox",
  "version",
  "title",
]
"##,
    )
    .expect("write dashboard config");

    // now_ms = 1_700_000_000_000 -> now_sec = 1_700_000_000 (2023-11-14T22:13:20Z).
    // 2023-11-15T00:00:20Z is +6420s -> 1h47m!
    let payload_json = serde_json::json!({
        "session_id": "sess_20_items",
        "agent_state": "working",
        "execution_mode": "accept-edits",
        "model": { "display_name": "Gemini 2.5 Pro", "effort": "high" },
        "cwd": "D:\\Code\\agyline",
        "vcs": { "branch": "feat/full-catalog", "dirty": true },
        "sandbox": { "enabled": true, "allow_network": true },
        "context_window": {
            "current_usage": { "input_tokens": 10000, "output_tokens": 500, "cache_read_input_tokens": 75000 },
            "total_output_tokens": 500,
            "context_window_size": 100000,
            "used_percentage": 85.0
        },
        "cost": { "total_usd": 1.42, "subagent_usd": 0.64 },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.68, "reset_in_seconds": 11520 },
            "3p-5h": { "remaining_fraction": 0.53, "reset_time": "2023-11-15T00:00:20Z" },
            "credits": { "remaining_amount": 420 }
        },
        "subagents": [
            { "status": "running", "name": "research", "model": "gemini-2.5-flash", "role": "HUGE PROMPT SHOULD NOT LEAK" },
            { "status": "working", "name": "coder", "model": "claude-sonnet-4-5" },
            { "status": "thinking", "name": "reviewer", "model": "gpt-5" },
            { "status": "running", "role": "UNTITLED LONG PROMPT MUST BE IGNORED", "model": "claude-haiku-4-5" }
        ],
        "task_count": 2,
        "pending_input_count": 1,
        "vim": { "mode": "insert" },
        "conversation_title": "Slice 26\n\tfull   item catalog!",
        "agent": { "name": "coordinator" },
        "artifact_count": 3,
        "version": "v1.5.9",
        "account": { "email": "secret-pii@example.com", "plan": "Ultra" },
        "email": "top-secret-pii@example.com"
    });

    let spawner = NoopSpawner;
    let env = RenderEnv {
        home_dir: sb.root.clone(),
        temp_dir: sb.root.join("tmp"),
        conversations_dir: sb.root.join("conv"),
        env_vars: BTreeMap::new(),
        now_ms: 1_700_000_000_000,
        spawner: &spawner,
    };

    let raw_bytes = serde_json::to_vec(&payload_json).expect("serialize payload");
    let out = run_live_frame(&raw_bytes, &env);

    // Privacy & role-prompt safety checks
    assert!(!out.plain.contains("secret-pii"));
    assert!(!out.plain.contains("HUGE PROMPT"));
    assert!(!out.plain.contains("UNTITLED LONG PROMPT"));
    let session_files: Vec<_> = fs::read_dir(sb.root.join("state").join("sessions"))
        .expect("sessions dir")
        .flatten()
        .collect();
    assert_eq!(session_files.len(), 1);
    let saved_session = fs::read_to_string(session_files[0].path()).expect("read session json");
    assert!(
        !saved_session.contains("Ultra"),
        "plan_tier must stay in-memory"
    );
    assert!(!saved_session.contains("secret-pii"));

    // Row 1 & Row 2 content checks
    let rows: Vec<&str> = out.plain.lines().collect();
    assert_eq!(rows.len(), 2, "expected 2 rows, got: {:?}", out.plain);
    assert!(rows[0].contains("\u{ef23} Gemini 2.5 Pro \u{00b7} High"));
    assert!(rows[0].contains("\u{f01a5} Ultra"));
    assert!(
        rows[0].contains("| agyline |"),
        "icon=\"\" must hide folder icon: {}",
        rows[0]
    );
    assert!(rows[0].contains("15% left \u{00b7} 85k tokens"));
    // Context used_pct is 85% >= 80% crit, so custom fg = "cyan" MUST NOT mask Crit (\x1b[0;31m)!
    assert!(
        out.ansi.contains("\x1b[0;31m 15% left"),
        "Crit alert color must not be masked by custom fg: {:?}",
        out.ansi
    );
    assert!(rows[0].contains("68% gem (3h12m)"));
    assert!(rows[0].contains("53% 3p (1h47m)"));
    assert!(rows[0].contains("420 cred"));
    assert!(rows[0].contains("$1.42 (sub $0.64)"));

    assert!(rows[1].contains("\u{25b8}\u{25b8} accept-edits"));
    assert!(rows[1].contains("\u{e62b} INSERT"));
    assert!(rows[1].contains("\u{f06a9} coordinator"));
    assert!(
        rows[1].contains(
            "4/4 agents running (research \u{00b7} Gemini 2.5 Flash, coder \u{00b7} Sonnet 4.5, +1)"
        ),
        "actual row 2: {}",
        rows[1]
    );
    assert!(rows[1].contains("2 tasks"));
    assert!(rows[1].contains("1 queued"));
    assert!(rows[1].contains("\u{f1c9} 3 artifacts"));
    assert!(rows[1].contains("sandbox+net"));
    assert!(rows[1].contains("v1.5.9") && !rows[1].contains("vv1.5.9"));
    assert!(rows[1].contains("\u{f0b79} Slice 26 full item catalog!"));
}

#[test]
fn three_stage_width_narrowing_downgrades_drops_and_grapheme_truncates_safely() {
    // Display width is grapheme-based: Nerd PUA icons count 1, CJK and ZWJ emoji count 2, so the
    // narrowing budget matches what the terminal actually renders.
    assert_eq!(str_display_width("abc"), 3);
    assert_eq!(str_display_width("\u{ef23} \u{f024b}"), 3);
    assert_eq!(str_display_width("中文"), 4);
    assert_eq!(str_display_width("👨\u{200d}👩\u{200d}👧\u{200d}👦"), 2);

    let sb = Sandbox::new("narrowing");
    let spawner = NoopSpawner;

    let payload_json = serde_json::json!({
        "session_id": "sess_narrow",
        "agent_state": "working",
        "model": { "display_name": "Claude Opus 4.6 Extended", "effort": "xHigh" },
        "cwd": "D:\\Work\\antigravity-statusline-rs",
        "vcs": { "branch": "feat/narrowing-three-stage-pipeline", "dirty": true },
        "context_window": {
            "current_usage": { "input_tokens": 10000, "output_tokens": 645, "cache_read_input_tokens": 620000 },
            "total_output_tokens": 645,
            "context_window_size": 1000000
        },
        "quota": {
            "3p-5h": { "remaining_fraction": 0.39, "reset_in_seconds": 4320 }
        },
        "cost": { "total_usd": 0.95 }
    });
    let raw = serde_json::to_vec(&payload_json).expect("serialize");
    let env = RenderEnv {
        home_dir: sb.root.clone(),
        temp_dir: sb.root.join("tmp"),
        conversations_dir: sb.root.join("conv"),
        env_vars: BTreeMap::new(),
        now_ms: 1_700_000_010_000,
        spawner: &spawner,
    };

    // Seed session so speed is active ("64.5")
    let mut cfg =
        EffectiveConfig::from_doc(&ConfigDoc::cometix_base(), None, &BTreeMap::new(), false);
    let parsed = agyline::parse_stdin_payload(&raw).expect("parse");
    let mut frame = agyline::FrameData {
        payload: parsed,
        now_sec: 1_700_000_010,
        git: Some(agyline::GitFrameState {
            branch: "feat/narrowing-three-stage-pipeline".to_string(),
            dirty: true,
            has_conflict: false,
            ahead: 12,
            behind: 3,
            ..agyline::GitFrameState::default()
        }),
        cache: agyline::CacheFrameState {
            cache_ts: Some(1_700_000_010 - 115),
            relay_touches: 1,
            relay_status: Some(agyline::RelayStatus::Live),
        },
        last_tps: Some("64.5".to_string()),
        response_model: None,
        ..agyline::FrameData::default()
    };

    // 1. Widthless -> all Full, zero narrowing
    let full_rendered = render_lines_with_width(&frame, &cfg, None);
    let full_plain = agyline::emit_plain(&full_rendered, &cfg);
    let full_w = str_display_width(full_plain.trim_end_matches('\n'));
    assert!(
        full_w > 110,
        "expected wide row > 110 cols, got {full_w}: {full_plain}"
    );
    assert_eq!(full_rendered.slot_outcome(0, 5), Some(SlotOutcome::Full)); // speed (P5)
    assert_eq!(full_rendered.slot_outcome(0, 4), Some(SlotOutcome::Full)); // cache (P4)

    // 2. Stage 1 only: budget slightly below full_w downgrades P5 (speed) & P4 (cache) first, keeping all 7 items!
    let stage1_budget = full_w - 8;
    let s1_rendered = render_lines_with_width(&frame, &cfg, Some(stage1_budget));
    let s1_plain = agyline::emit_plain(&s1_rendered, &cfg);
    let s1_w = str_display_width(s1_plain.trim_end_matches('\n'));
    assert!(
        s1_w <= stage1_budget,
        "{s1_w} > {stage1_budget}: {s1_plain}"
    );
    assert_eq!(s1_rendered.lines[0].items.len(), 7);
    assert_eq!(s1_rendered.slot_outcome(0, 5), Some(SlotOutcome::Compact)); // speed compacted first (P5)
    assert!(s1_plain.contains("64.5/s"));

    // 3. Stage 2: 74 columns drops lower-priority items right-to-left while keeping P1 items (`model`, `context`)
    let s2_rendered = render_lines_with_width(&frame, &cfg, Some(74));
    let s2_plain = agyline::emit_plain(&s2_rendered, &cfg);
    let s2_w = str_display_width(s2_plain.trim_end_matches('\n'));
    assert!(s2_w <= 74, "{s2_w} > 74: {s2_plain}");
    assert_eq!(s2_rendered.slot_outcome(0, 5), Some(SlotOutcome::Dropped)); // speed dropped
    assert_eq!(s2_rendered.slot_outcome(0, 4), Some(SlotOutcome::Dropped)); // cache dropped
    assert!(s2_rendered.lines[0].items.len() >= 2);

    // 4. Stage 3: CJK + ZWJ emoji + broken config at tight width (24 cols and ultra-tiny 8 cols)
    frame.payload.model_display_name =
        "中文模型👨\u{200d}👩\u{200d}👧\u{200d}👦超长名称测试".to_string();
    cfg.config_broken = true;
    for tight_w in [26usize, 15, 8] {
        let s3_rendered = render_lines_with_width(&frame, &cfg, Some(tight_w));
        let s3_plain = agyline::emit_plain(&s3_rendered, &cfg);
        let s3_ansi = agyline::emit_ansi(&s3_rendered, &cfg);
        let line_no_nl = s3_plain.trim_end_matches('\n');
        let w = str_display_width(line_no_nl);
        assert!(
            w <= tight_w,
            "width {w} exceeded {tight_w}: plain={s3_plain:?}"
        );
        assert!(
            s3_ansi.ends_with("\x1b[0m\n"),
            "missing ANSI reset: {s3_ansi:?}"
        );
        assert_eq!(s3_rendered.slot_outcome(0, 0), Some(SlotOutcome::Truncated));
        if tight_w >= 15 {
            assert!(
                line_no_nl.ends_with(" ! config"),
                "must preserve ' ! config' when width allows ({tight_w}): {line_no_nl:?}"
            );
        }
    }

    // 5. overflow = "none" disables narrowing even when payload.terminal_width is small
    cfg.config_broken = false;
    cfg.overflow = OverflowMode::None;
    frame.payload.terminal_width = Some(20);
    let unnarrowed = agyline::render_lines(&frame, &cfg);
    let unnarrowed_plain = agyline::emit_plain(&unnarrowed, &cfg);
    assert!(str_display_width(unnarrowed_plain.trim_end_matches('\n')) > 80);
    let _ = env;
}

#[test]
fn hud_items_and_new_slot_options_roundtrip_degrade_and_render_full_and_compact() {
    let toml_src = r#"
palette = "terminal"
bar_style = "line"
bar_gradient = false
compact_at = 0
separator = " │ "
indent = "  "

[thresholds]
context_warn = 60
context_crit = 90
quota_warn = 40
quota_crit = 10

[[line]]
items = [
  { id = "status", show_pending = true },
  { id = "mode", style = "badge" },
  { id = "model", style = "hud", icon = "", fg = "context" },
  { id = "context", style = "bar", bar_style = "solid", bar_width = 10, icon = "" },
  { id = "dir", max_levels = 2, icon = "", fg = "model" },
  { id = "git", style = "counts", icon = "" },
]

[[line]]
items = [
  { id = "quota_group", family = "gemini", label = "Gemini", show_used = true, show_bar = true, bar_style = "line", show_reset = true },
  { id = "quota_group", family = "3p", label = "3P", show_used = false, show_bar = false, show_reset = true },
]

[[line]]
items = [
  { id = "sandbox", show_when_off = true, icon = "" },
  { id = "tokens", show_breakdown = true },
  { id = "cache", style = "hit_rate", show_ttl = true, icon = "" },
  "duration",
]
"#;

    let (doc, warnings) = parse_config_doc(toml_src).expect("parse hud slot options");
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    let serialized = serialize_config_doc(&doc);
    let (reparsed, warnings2) = parse_config_doc(&serialized).expect("roundtrip hud options");
    assert!(warnings2.is_empty(), "warnings2: {warnings2:?}");
    assert_eq!(reparsed, doc);

    // Invalid option bounds warn and drop gracefully
    let bad_toml = r#"
[[line]]
items = [{ id = "dir", max_levels = 99 }, { id = "context", style = "bar", bar_style = "neon", bar_width = 2 }]
"#;
    let (bad_doc, bad_warnings) = parse_config_doc(bad_toml).expect("tolerant parse");
    assert_eq!(bad_doc.lines[0][0].max_levels, None);
    assert_eq!(bad_doc.lines[0][1].bar_style, None);
    assert_eq!(bad_doc.lines[0][1].bar_width, None);
    assert!(bad_warnings.len() >= 3);

    // Evaluate all HUD styles
    let mut env_vars = BTreeMap::new();
    env_vars.insert("HOME".to_string(), "/home/dev".to_string());
    env_vars.insert("USERPROFILE".to_string(), "/home/dev".to_string());
    let cfg = EffectiveConfig::from_doc(&doc, None, &env_vars, true);

    let payload = agyline::parse_stdin_payload(
        br#"{
            "session_id": "hud_test",
            "agent_state": "working",
            "execution_mode": "accept-edits",
            "pending_input_count": 2,
            "vim": { "mode": "insert" },
            "model": { "display_name": "Gemini 4 Neon (XHigh)" },
            "cwd": "/home/dev/projects/antigravity-hud",
            "sandbox": { "enabled": false },
            "context_window": {
                "current_usage": { "input_tokens": 19700, "cache_read_input_tokens": 177300 },
                "total_input_tokens": 4478000,
                "total_output_tokens": 122000,
                "context_window_size": 1000000
            },
            "quota": {
                "gemini-5h": { "remaining_fraction": 0.927, "reset_in_seconds": 9840 },
                "gemini-weekly": { "remaining_fraction": 0.797, "reset_in_seconds": 446400 },
                "3p-5h": { "remaining_fraction": 1.0, "reset_in_seconds": 18000 }
            }
        }"#,
    )
    .expect("parse hud payload");

    let frame = agyline::FrameData {
        payload,
        now_sec: 1_700_003_480,
        git: Some(agyline::GitFrameState {
            branch: "main".to_string(),
            dirty: true,
            modified: 1,
            untracked: 7,
            ..agyline::GitFrameState::default()
        }),
        cache: agyline::CacheFrameState {
            cache_ts: Some(1_700_003_400),
            relay_touches: 1,
            relay_status: Some(agyline::RelayStatus::Live),
        },
        session_start_sec: Some(1_700_000_000), // 3480s = 58m
        user_home: Some(PathBuf::from("/home/dev")),
        ..agyline::FrameData::default()
    };

    let rendered = agyline::render_lines(&frame, &cfg);
    let plain = agyline::emit_plain(&rendered, &cfg);
    let rows: Vec<&str> = plain.lines().collect();
    assert_eq!(rows.len(), 3, "plain: {plain:?}");
    assert!(rows[0].contains("WORKING (+2) │ EDITS [I] │ Gemini 4 Neon (XHigh) │ ctx "));
    assert!(rows[0].contains(" 19.7% │ ~/projects/antigravity-hud │ main* !1 ?7"));
    assert!(rows[1].contains(
        "\u{f0081} Gemini 5h \u{2578}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 7.3% 2h44m / week \u{2501}\u{2501}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} 20.3% 5d4h"
    ));
    assert!(rows[1].contains(" 20.3% 5d4h │ \u{f0079} 3P 5h 100.0% 5h0m"));
    assert!(rows[2].contains("sandbox off │ \u{e26b} token 4.6M (4.5M↑ 122k↓) │ cache 90.0%"));
    assert!(rows[2].contains("3:40"));
    assert!(rows[2].ends_with("│ \u{f43a} 58m"));

    let ansi = agyline::emit_ansi(&rendered, &cfg);
    // Bold WORKING (`;1;`), Italic model (`;3;`), Bold 4.6M (`;1;`)
    assert!(ansi.contains(";1;96m⚙ WORKING\x1b[0;33m (+2)"));
    assert!(ansi.contains(";3;95mGemini 4 Neon (XHigh)"));
    assert!(ansi.contains(";1;37m4.6M"));

    // Enabled sandbox with `show_when_off = true` paints its icon in `Ok` green instead of `Muted`.
    let mut sbx_frame = frame;
    sbx_frame.payload.sandbox_enabled = true;
    let mut sbx_cfg = cfg;
    sbx_cfg.lines[2][0].icon = None;
    let sbx_ansi = agyline::emit_ansi(&agyline::render_lines(&sbx_frame, &sbx_cfg), &sbx_cfg);
    assert!(
        sbx_ansi.contains("\x1b[0;92m\u{f132} \x1b[0;90msandbox \x1b[0;1;92mON"),
        "enabled sandbox icon must be Ok green: {sbx_ansi:?}"
    );
}

// Ticket #39: 4-tier semantic tokens (`high`, `surface`, `surface_hi`), dual-polarity
// `catppuccin` (Mocha dark / Latte light) and `mono` palettes, plus legacy custom palette loading.
#[test]
fn four_tier_color_tokens_and_dual_polarity_catppuccin_and_mono_palettes() {
    use agyline::{
        ColorToken, PaletteOverrides, PalettePolarity, ResolvedColor, ThemePolarity,
        resolve_palette,
    };

    assert_eq!(ColorToken::ALL.len(), 15);
    for (key, expected) in [
        ("high", ColorToken::High),
        ("surface", ColorToken::Surface),
        ("surface_hi", ColorToken::SurfaceHi),
    ] {
        assert_eq!(ColorToken::from_key(key), Some(expected));
        assert_eq!(expected.as_key(), key);
    }

    let builtins = builtin_palettes();
    let cat = builtins
        .iter()
        .find(|p| p.name == "catppuccin")
        .expect("catppuccin built-in");
    assert_eq!(cat.polarity, PalettePolarity::Dual);
    let mono = builtins
        .iter()
        .find(|p| p.name == "mono")
        .expect("mono built-in");
    assert_eq!(mono.polarity, PalettePolarity::Dual);

    let empty_ov = PaletteOverrides::default();

    // Dual-polarity `catppuccin`: Mocha on Dark, Latte on Light, no polarity fallback.
    let mocha = resolve_palette("catppuccin", ThemePolarity::Dark, &empty_ov, None);
    assert!(!mocha.polarity_fell_back);
    assert_eq!(
        mocha.get(ColorToken::Model),
        ResolvedColor::Rgb(0xcb, 0xa6, 0xf7)
    );
    assert_eq!(
        mocha.get(ColorToken::Git),
        ResolvedColor::Rgb(0x89, 0xb4, 0xfa)
    );
    assert_eq!(
        mocha.get(ColorToken::Cache),
        ResolvedColor::Rgb(0x94, 0xe2, 0xd5)
    );
    assert_eq!(
        mocha.get(ColorToken::Ok),
        ResolvedColor::Rgb(0xa6, 0xe3, 0xa1)
    );
    assert_eq!(
        mocha.get(ColorToken::Warn),
        ResolvedColor::Rgb(0xf9, 0xe2, 0xaf)
    );
    assert_eq!(
        mocha.get(ColorToken::High),
        ResolvedColor::Rgb(0xfa, 0xb3, 0x87)
    );
    assert_eq!(
        mocha.get(ColorToken::Crit),
        ResolvedColor::Rgb(0xf3, 0x8b, 0xa8)
    );
    assert_eq!(
        mocha.get(ColorToken::Surface),
        ResolvedColor::Rgb(0x31, 0x32, 0x44)
    );
    assert_eq!(
        mocha.get(ColorToken::SurfaceHi),
        ResolvedColor::Rgb(0x45, 0x47, 0x5a)
    );

    let latte = resolve_palette("catppuccin", ThemePolarity::Light, &empty_ov, None);
    assert!(!latte.polarity_fell_back);
    assert_eq!(
        latte.get(ColorToken::Model),
        ResolvedColor::Rgb(0x88, 0x39, 0xef)
    );
    assert_eq!(
        latte.get(ColorToken::Git),
        ResolvedColor::Rgb(0x1e, 0x66, 0xf5)
    );
    assert_eq!(
        latte.get(ColorToken::Cache),
        ResolvedColor::Rgb(0x17, 0x92, 0x99)
    );
    assert_eq!(
        latte.get(ColorToken::Ok),
        ResolvedColor::Rgb(0x40, 0xa0, 0x2b)
    );
    assert_eq!(
        latte.get(ColorToken::Warn),
        ResolvedColor::Rgb(0xdf, 0x8e, 0x1d)
    );
    assert_eq!(
        latte.get(ColorToken::High),
        ResolvedColor::Rgb(0xfe, 0x64, 0x0b)
    );
    assert_eq!(
        latte.get(ColorToken::Crit),
        ResolvedColor::Rgb(0xd2, 0x0f, 0x39)
    );
    assert_eq!(
        latte.get(ColorToken::Surface),
        ResolvedColor::Rgb(0xcc, 0xd0, 0xda)
    );
    assert_eq!(
        latte.get(ColorToken::SurfaceHi),
        ResolvedColor::Rgb(0xbc, 0xc0, 0xcc)
    );

    // Dual-polarity `mono`: neutral grayscale everyday tokens + distinct urgency contrast.
    for polarity in [ThemePolarity::Dark, ThemePolarity::Light] {
        let m = resolve_palette("mono", polarity, &empty_ov, None);
        assert!(!m.polarity_fell_back);
        for tok in [
            ColorToken::Separator,
            ColorToken::Muted,
            ColorToken::Model,
            ColorToken::Dir,
            ColorToken::Git,
            ColorToken::Ok,
            ColorToken::Surface,
            ColorToken::SurfaceHi,
        ] {
            match m.get(tok) {
                ResolvedColor::Rgb(r, g, b) => {
                    assert!(
                        r == g && g == b,
                        "mono {polarity:?} {tok:?} should be neutral grayscale, got ({r},{g},{b})"
                    );
                }
                other => panic!("mono {tok:?} should resolve to RGB, got {other:?}"),
            }
        }
        assert_ne!(m.get(ColorToken::Warn), m.get(ColorToken::High));
        assert_ne!(m.get(ColorToken::High), m.get(ColorToken::Crit));
        assert_ne!(m.get(ColorToken::Ok), m.get(ColorToken::Warn));
    }

    // Every built-in RGB palette defines distinct RGB `high`, `surface`, `surface_hi`,
    // and `terminal` supplies ANSI-16 fallbacks.
    for info in builtins {
        let pol = match info.polarity {
            PalettePolarity::Light => ThemePolarity::Light,
            PalettePolarity::Dark | PalettePolarity::Dual => ThemePolarity::Dark,
        };
        let resolved = resolve_palette(info.name, pol, &empty_ov, None);
        assert!(!resolved.polarity_fell_back);
        if info.name == "terminal" {
            assert!(matches!(
                resolved.get(ColorToken::High),
                ResolvedColor::Ansi16(_)
            ));
            assert!(matches!(
                resolved.get(ColorToken::Surface),
                ResolvedColor::Ansi16(_)
            ));
            assert!(matches!(
                resolved.get(ColorToken::SurfaceHi),
                ResolvedColor::Ansi16(_)
            ));
        } else {
            assert!(matches!(
                resolved.get(ColorToken::High),
                ResolvedColor::Rgb(..)
            ));
            assert!(matches!(
                resolved.get(ColorToken::Surface),
                ResolvedColor::Rgb(..)
            ));
            assert!(matches!(
                resolved.get(ColorToken::SurfaceHi),
                ResolvedColor::Rgb(..)
            ));
            assert_ne!(
                resolved.get(ColorToken::Surface),
                resolved.get(ColorToken::SurfaceHi)
            );
        }
    }

    // Legacy custom palette omitting `high`/`surface`/`surface_hi` still loads cleanly,
    // and `[palette_overrides]` can override the new tokens.
    let sb = Sandbox::new("legacy_pal");
    let pal_dir = sb.root.join("palettes");
    fs::create_dir_all(&pal_dir).expect("mkdir palettes");
    fs::write(
        pal_dir.join("legacy12.toml"),
        "polarity = \"dark\"\nmodel = \"#112233\"\nok = \"#22aa44\"\n",
    )
    .expect("write legacy custom palette");
    let mut ov = PaletteOverrides::default();
    ov.set(ColorToken::High, "#ff8800");
    ov.set(ColorToken::Surface, "#101018");
    let custom = resolve_palette("legacy12", ThemePolarity::Dark, &ov, Some(&sb.root));
    assert!(!custom.polarity_fell_back);
    assert_eq!(
        custom.get(ColorToken::Model),
        ResolvedColor::Rgb(0x11, 0x22, 0x33)
    );
    assert_eq!(
        custom.get(ColorToken::High),
        ResolvedColor::Rgb(0xff, 0x88, 0x00)
    );
    assert_eq!(
        custom.get(ColorToken::Surface),
        ResolvedColor::Rgb(0x10, 0x10, 0x18)
    );
    assert!(matches!(
        custom.get(ColorToken::SurfaceHi),
        ResolvedColor::Ansi16(_)
    ));
}

// Ticket #39: four-tier `[thresholds]` (`context_high`, `quota_high`) parsing, tolerant fallback
// for legacy configs omitting them, out-of-order guards, and TOML round-trip.
#[test]
fn four_tier_thresholds_parse_validate_degrade_and_roundtrip() {
    let default_doc = ConfigDoc::default();
    assert_eq!(default_doc.thresholds.context_high, 75.0);
    assert_eq!(default_doc.thresholds.quota_high, 25);

    let full_toml = r#"
[thresholds]
context_warn = 55
context_high = 76.5
context_crit = 92
quota_warn = 50
quota_high = 28
quota_crit = 12
cache_warn_secs = 45
"#;
    let (doc, warnings) = parse_config_doc(full_toml).expect("parse 4-tier thresholds");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(doc.thresholds.context_warn, 55.0);
    assert_eq!(doc.thresholds.context_high, 76.5);
    assert_eq!(doc.thresholds.context_crit, 92.0);
    assert_eq!(doc.thresholds.quota_warn, 50);
    assert_eq!(doc.thresholds.quota_high, 28);
    assert_eq!(doc.thresholds.quota_crit, 12);

    let serialized = serialize_config_doc(&doc);
    assert!(serialized.contains("context_high = 76.5"));
    assert!(serialized.contains("quota_high = 28"));
    let (reparsed, rewarnings) = parse_config_doc(&serialized).expect("reparse");
    assert!(rewarnings.is_empty(), "rewarnings: {rewarnings:?}");
    assert_eq!(reparsed.thresholds, doc.thresholds);

    // Legacy config omitting `context_high` and `quota_high` whose range excludes 75 / 25
    // must still load cleanly with valid derived midpoints instead of rejecting valid warn/crit.
    let legacy_toml = r#"
[thresholds]
context_warn = 40
context_crit = 70
quota_warn = 20
quota_crit = 5
"#;
    let (legacy_doc, legacy_warnings) = parse_config_doc(legacy_toml).expect("parse legacy");
    assert!(
        legacy_warnings.is_empty(),
        "legacy warnings: {legacy_warnings:?}"
    );
    assert_eq!(legacy_doc.thresholds.context_warn, 40.0);
    assert_eq!(legacy_doc.thresholds.context_crit, 70.0);
    assert!(
        legacy_doc.thresholds.context_warn < legacy_doc.thresholds.context_high
            && legacy_doc.thresholds.context_high < legacy_doc.thresholds.context_crit
    );
    assert_eq!(legacy_doc.thresholds.quota_warn, 20);
    assert_eq!(legacy_doc.thresholds.quota_crit, 5);
    assert!(
        legacy_doc.thresholds.quota_crit < legacy_doc.thresholds.quota_high
            && legacy_doc.thresholds.quota_high < legacy_doc.thresholds.quota_warn
    );

    // Out-of-order explicit `context_high` / `quota_high` emit warnings and keep valid order.
    let bad_high_toml = r#"
[thresholds]
context_warn = 50
context_high = 95
context_crit = 80
quota_warn = 45
quota_high = 10
quota_crit = 20
"#;
    let (bad_doc, bad_warnings) = parse_config_doc(bad_high_toml).expect("tolerant parse");
    assert!(
        bad_warnings.len() >= 2,
        "expected warnings for out-of-order high thresholds, got: {bad_warnings:?}"
    );
    assert!(
        bad_doc.thresholds.context_warn < bad_doc.thresholds.context_high
            && bad_doc.thresholds.context_high < bad_doc.thresholds.context_crit
    );
    assert!(
        bad_doc.thresholds.quota_crit < bad_doc.thresholds.quota_high
            && bad_doc.thresholds.quota_high < bad_doc.thresholds.quota_warn
    );
}

// Ticket #39: `color_depth = "auto"` preserves NO_COLOR / TERM=dumb, downgrades Apple_Terminal
// to 256-color, and promotes known modern TrueColor terminals ahead of generic `*256color`.
#[test]
fn auto_color_depth_detects_apple_terminal_and_modern_truecolor_hints() {
    use agyline::{EffectiveColorDepth, resolve_effective_color_depth};

    let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    };

    // NO_COLOR and TERM=dumb still outrank everything.
    assert_eq!(
        resolve_effective_color_depth(
            ColorDepth::Auto,
            &map(&[
                ("NO_COLOR", "1"),
                ("COLORTERM", "truecolor"),
                ("WT_SESSION", "1")
            ])
        ),
        EffectiveColorDepth::None
    );
    assert_eq!(
        resolve_effective_color_depth(
            ColorDepth::Auto,
            &map(&[("TERM", "dumb"), ("TERM_PROGRAM", "WezTerm")])
        ),
        EffectiveColorDepth::None
    );

    // Apple_Terminal forces 256-color both with and without TERM=xterm-256color.
    assert_eq!(
        resolve_effective_color_depth(
            ColorDepth::Auto,
            &map(&[("TERM_PROGRAM", "Apple_Terminal")])
        ),
        EffectiveColorDepth::Ansi256
    );
    assert_eq!(
        resolve_effective_color_depth(
            ColorDepth::Auto,
            &map(&[
                ("TERM_PROGRAM", "Apple_Terminal"),
                ("TERM", "xterm-256color")
            ])
        ),
        EffectiveColorDepth::Ansi256
    );

    // Modern TrueColor TERM_PROGRAM hints beat generic `TERM=xterm-256color`.
    for prog in [
        "iTerm.app",
        "WezTerm",
        "vscode",
        "WarpTerminal",
        "ghostty",
        "alacritty",
        "kitty",
        "Hyper",
        "Tabby",
        "rio",
    ] {
        assert_eq!(
            resolve_effective_color_depth(
                ColorDepth::Auto,
                &map(&[("TERM", "xterm-256color"), ("TERM_PROGRAM", prog)])
            ),
            EffectiveColorDepth::TrueColor,
            "TERM_PROGRAM={prog} should resolve to TrueColor"
        );
    }

    // Dedicated modern terminal env markers beat generic `TERM=xterm-256color`.
    for env_key in [
        "WT_SESSION",
        "ITERM_SESSION_ID",
        "KITTY_WINDOW_ID",
        "GHOSTTY_RESOURCES_DIR",
        "WEZTERM_EXECUTABLE",
    ] {
        assert_eq!(
            resolve_effective_color_depth(
                ColorDepth::Auto,
                &map(&[("TERM", "xterm-256color"), (env_key, "active")])
            ),
            EffectiveColorDepth::TrueColor,
            "{env_key} should resolve to TrueColor"
        );
    }

    // Generic 256color without modern hints stays Ansi256; plain xterm stays Ansi16.
    assert_eq!(
        resolve_effective_color_depth(ColorDepth::Auto, &map(&[("TERM", "xterm-256color")])),
        EffectiveColorDepth::Ansi256
    );
    assert_eq!(
        resolve_effective_color_depth(ColorDepth::Auto, &map(&[("TERM", "xterm")])),
        EffectiveColorDepth::Ansi16
    );
}

// Container Style (`item_style`), semantic separator aliases, and `compact_at` parse, validate,
// preserve existing preset compatibility, respect PresetScope, and round-trip cleanly.
#[test]
fn container_style_separator_aliases_and_compact_at_parse_validate_and_roundtrip() {
    assert_eq!(DEFAULT_COMPACT_AT, 88);
    assert_eq!(ItemStyle::default(), ItemStyle::Minimal);
    assert_eq!(
        ItemStyle::ALL,
        [ItemStyle::Minimal, ItemStyle::Badge, ItemStyle::Pill]
    );
    assert_eq!(ItemStyle::Minimal.as_key(), "minimal");
    assert_eq!(ItemStyle::Badge.as_key(), "badge");
    assert_eq!(ItemStyle::Pill.as_key(), "pill");
    assert_eq!(ItemStyle::from_key("  PILL "), Some(ItemStyle::Pill));
    assert_eq!(ItemStyle::from_key("Badge"), Some(ItemStyle::Badge));
    assert_eq!(ItemStyle::from_key("minimal"), Some(ItemStyle::Minimal));
    assert_eq!(ItemStyle::from_key("glass"), None);

    // Preserved presets keep minimal containers and compact_at = 0; new `default` uses 88.
    let def_doc = agyline::build_preset_doc("default", None).expect("default preset");
    assert_eq!(def_doc.item_style, ItemStyle::Minimal);
    assert_eq!(def_doc.compact_at, DEFAULT_COMPACT_AT);
    for preset in ["hud", "classic", "cometix"] {
        let doc = agyline::build_preset_doc(preset, None).expect("builtin preset");
        assert_eq!(doc.item_style, ItemStyle::Minimal, "preset={preset}");
        assert_eq!(doc.compact_at, 0, "preset={preset}");
    }

    // Semantic separator aliases resolve case-insensitively; raw strings survive with control chars stripped.
    assert_eq!(resolve_separator("pipe"), " \u{2502} ");
    assert_eq!(resolve_separator("  PIPE  "), " \u{2502} ");
    assert_eq!(resolve_separator("dashed"), " \u{250a} ");
    assert_eq!(resolve_separator("dot"), " \u{2022} ");
    assert_eq!(resolve_separator("square"), " \u{25aa} ");
    assert_eq!(resolve_separator("none"), "  ");
    assert_eq!(resolve_separator(" | "), " | ");
    assert_eq!(resolve_separator(" │ "), " │ ");
    assert_eq!(resolve_separator(" / "), " / ");
    assert_eq!(resolve_separator(""), "");
    assert_eq!(resolve_separator(" \x07|\n\t\r "), " | ");
    assert_eq!(resolve_separator(" \x1b/\x7f "), " / ");

    // TOML round-trip keeps the user's semantic alias while EffectiveConfig resolves it (and degrades for ASCII).
    let raw = r#"
item_style = "pill"
separator = "dashed"
compact_at = 88
"#;
    let (doc, warnings) = parse_config_doc(raw).expect("parse valid config");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(doc.item_style, ItemStyle::Pill);
    assert_eq!(doc.separator, "dashed");
    assert_eq!(doc.compact_at, 88);

    let serialized = serialize_config_doc(&doc);
    assert!(serialized.contains("item_style = \"pill\""));
    assert!(serialized.contains("separator = \"dashed\""));
    assert!(serialized.contains("compact_at = 88"));
    let (reparsed, rewarnings) = parse_config_doc(&serialized).expect("reparse");
    assert!(
        rewarnings.is_empty(),
        "unexpected rewarnings: {rewarnings:?}"
    );
    assert_eq!(reparsed, doc);

    let eff_nerd = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);
    assert_eq!(eff_nerd.separator, " \u{250a} ");
    assert_eq!(eff_nerd.item_style, ItemStyle::Pill);
    assert_eq!(eff_nerd.compact_at, 88);

    let mut ascii_doc = doc.clone();
    ascii_doc.glyphs = GlyphSet::Ascii;
    let eff_ascii = EffectiveConfig::from_doc(&ascii_doc, None, &BTreeMap::new(), false);
    assert_eq!(eff_ascii.separator, " | ");

    for (alias, expected_ascii) in [
        ("pipe", " | "),
        ("dashed", " | "),
        ("dot", " . "),
        ("square", " - "),
        ("none", "  "),
    ] {
        ascii_doc.separator = alias.to_string();
        let eff = EffectiveConfig::from_doc(&ascii_doc, None, &BTreeMap::new(), false);
        assert_eq!(eff.separator, expected_ascii, "ascii alias={alias}");
    }

    // Invalid item_style / compact_at degrade with warnings instead of failing load.
    let bad_raw = r#"
item_style = "bubble"
compact_at = 501
"#;
    let (bad_doc, bad_warnings) = parse_config_doc(bad_raw).expect("tolerant parse");
    assert_eq!(bad_doc.item_style, ItemStyle::Minimal);
    assert_eq!(bad_doc.compact_at, DEFAULT_COMPACT_AT);
    assert!(bad_warnings.iter().any(|w| w.contains("item_style")));
    assert!(bad_warnings.iter().any(|w| w.contains("compact_at")));

    let neg_raw = "compact_at = -10\n";
    let (neg_doc, neg_warnings) = parse_config_doc(neg_raw).expect("tolerant negative parse");
    assert_eq!(neg_doc.compact_at, DEFAULT_COMPACT_AT);
    assert!(neg_warnings.iter().any(|w| w.contains("compact_at")));

    // PresetScope::Style copies item_style only; Layout copies compact_at only; All copies both.
    let sb = Sandbox::new("preset_container_scope");
    let presets_dir = sb.root.join("presets");
    fs::create_dir_all(&presets_dir).expect("create presets dir");
    fs::write(
        presets_dir.join("pill-compact.toml"),
        r#"
item_style = "pill"
compact_at = 96
separator = "dot"

[[line]]
items = ["model", "git"]
"#,
    )
    .expect("write custom preset");

    let mut target = ConfigDoc::default();
    assert!(apply_preset(
        &mut target,
        "pill-compact",
        PresetScope::Style,
        Some(&sb.root)
    ));
    assert_eq!(target.item_style, ItemStyle::Pill);
    assert_eq!(target.separator, "dot");
    assert_eq!(
        target.compact_at, DEFAULT_COMPACT_AT,
        "Style scope must not touch compact_at"
    );

    let mut target_layout = ConfigDoc::default();
    assert!(apply_preset(
        &mut target_layout,
        "pill-compact",
        PresetScope::Layout,
        Some(&sb.root)
    ));
    assert_eq!(target_layout.compact_at, 96);
    assert_eq!(
        target_layout.item_style,
        ItemStyle::Minimal,
        "Layout scope must not touch item_style"
    );

    let mut target_all = ConfigDoc::default();
    assert!(apply_preset(
        &mut target_all,
        "pill-compact",
        PresetScope::All,
        Some(&sb.root)
    ));
    assert_eq!(target_all.item_style, ItemStyle::Pill);
    assert_eq!(target_all.compact_at, 96);
}

// Container Style frames eligible items in `badge` (Surface/SurfaceHi) and `pill` (Powerline caps),
// leaves graphic/lightweight items un-boxed, degrades cleanly by glyph/depth, and never overflows.
#[test]
fn container_style_wraps_eligible_items_and_degrades_across_capabilities() {
    let mut base_doc = ConfigDoc::cometix_base();
    base_doc.palette = "catppuccin".to_string();
    base_doc.theme = ThemeSetting::Dark;
    base_doc.separator = "pipe".to_string();

    let mut ctx_bar = ItemSlot::new(ItemId::Context);
    ctx_bar.style = Some("bar".to_string());
    let ctx_plain = ItemSlot::new(ItemId::Context);
    let mut q_group = ItemSlot::new(ItemId::QuotaGroup);
    q_group.family = Some("gemini".to_string());

    base_doc.lines = vec![vec![
        ItemSlot::new(ItemId::Status),
        ItemSlot::new(ItemId::Model),
        ctx_bar,
        ctx_plain,
        ItemSlot::new(ItemId::Git),
        ItemSlot::new(ItemId::Dir),
        q_group,
        ItemSlot::new(ItemId::Tokens),
    ]];

    let payload_bytes = serde_json::to_vec(&serde_json::json!({
        "session_id": "container_test",
        "agent_state": "working",
        "model": { "display_name": "Claude Opus 4.6", "effort": "high" },
        "cwd": "/workspace/agyline",
        "vcs": { "branch": "feat/containers", "dirty": true },
        "context_window": {
            "current_usage": {
                "input_tokens": 12000,
                "output_tokens": 2400,
                "cache_read_input_tokens": 88000
            },
            "total_input_tokens": 100000,
            "total_output_tokens": 2400,
            "context_window_size": 1000000
        },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.72, "reset_in_seconds": 7200 }
        }
    }))
    .expect("serialize");
    let frame = agyline::FrameData {
        payload: agyline::parse_stdin_payload(&payload_bytes).expect("parse"),
        now_sec: 1_700_000_000,
        git: Some(agyline::GitFrameState {
            branch: "feat/containers".to_string(),
            dirty: true,
            ..agyline::GitFrameState::default()
        }),
        ..agyline::FrameData::default()
    };

    // Capability degradation matrix for effective_item_style & effective_row_separator.
    let check_cap = |style: ItemStyle,
                     depth: ColorDepth,
                     glyphs: GlyphSet,
                     exp_style: ItemStyle,
                     exp_sep: &str| {
        let mut d = base_doc.clone();
        d.item_style = style;
        d.color_depth = depth;
        d.glyphs = glyphs;
        let cfg = EffectiveConfig::from_doc(&d, None, &BTreeMap::new(), false);
        assert_eq!(cfg.effective_item_style(), exp_style);
        assert_eq!(cfg.effective_row_separator(), exp_sep);
    };
    check_cap(
        ItemStyle::Pill,
        ColorDepth::TrueColor,
        GlyphSet::Nerd,
        ItemStyle::Pill,
        " ",
    );
    check_cap(
        ItemStyle::Pill,
        ColorDepth::Ansi256,
        GlyphSet::Unicode,
        ItemStyle::Badge,
        " ",
    );
    check_cap(
        ItemStyle::Pill,
        ColorDepth::TrueColor,
        GlyphSet::Ascii,
        ItemStyle::Badge,
        " ",
    );
    check_cap(
        ItemStyle::Pill,
        ColorDepth::Ansi16,
        GlyphSet::Nerd,
        ItemStyle::Minimal,
        " \u{2502} ",
    );
    check_cap(
        ItemStyle::Badge,
        ColorDepth::None,
        GlyphSet::Nerd,
        ItemStyle::Minimal,
        " \u{2502} ",
    );
    check_cap(
        ItemStyle::Badge,
        ColorDepth::Ansi256,
        GlyphSet::Ascii,
        ItemStyle::Badge,
        " ",
    );

    // Badge mode on TrueColor: Model gets SurfaceHi, Git/Tokens/non-bar Context get Surface,
    // while Status, Context(bar), Dir, and QuotaGroup stay un-boxed.
    let mut badge_doc = base_doc.clone();
    badge_doc.item_style = ItemStyle::Badge;
    badge_doc.color_depth = ColorDepth::TrueColor;
    badge_doc.glyphs = GlyphSet::Nerd;
    let badge_cfg = EffectiveConfig::from_doc(&badge_doc, None, &BTreeMap::new(), false);
    let badge_frame = render_lines_with_width(&frame, &badge_cfg, None);
    let badge_items = &badge_frame.lines[0].items;
    assert_eq!(badge_items.len(), 8);

    // 0: Status (un-boxed)
    assert!(badge_items[0].spans.iter().all(|s| s.style.bg.is_none()));
    // 1: Model (boxed with SurfaceHi)
    assert_eq!(
        badge_items[1].spans.first().map(|s| s.text.as_str()),
        Some(" ")
    );
    assert_eq!(
        badge_items[1].spans.last().map(|s| s.text.as_str()),
        Some(" ")
    );
    assert!(
        badge_items[1]
            .spans
            .iter()
            .all(|s| s.style.bg == Some(ColorSpec::Token(ColorToken::SurfaceHi)))
    );
    // 2: Context bar (un-boxed)
    assert!(badge_items[2].spans.iter().all(|s| s.style.bg
        != Some(ColorSpec::Token(ColorToken::Surface))
        && s.style.bg != Some(ColorSpec::Token(ColorToken::SurfaceHi))));
    // 3: Context plain (boxed with Surface)
    assert!(
        badge_items[3]
            .spans
            .iter()
            .all(|s| s.style.bg == Some(ColorSpec::Token(ColorToken::Surface)))
    );
    // 4: Git (boxed with Surface)
    assert!(
        badge_items[4]
            .spans
            .iter()
            .all(|s| s.style.bg == Some(ColorSpec::Token(ColorToken::Surface)))
    );
    // 5: Dir (un-boxed)
    assert!(badge_items[5].spans.iter().all(|s| s.style.bg.is_none()));
    // 6: QuotaGroup (un-boxed)
    assert!(badge_items[6].spans.iter().all(|s| s.style.bg
        != Some(ColorSpec::Token(ColorToken::Surface))
        && s.style.bg != Some(ColorSpec::Token(ColorToken::SurfaceHi))));
    // 7: Tokens (boxed with Surface)
    assert!(
        badge_items[7]
            .spans
            .iter()
            .all(|s| s.style.bg == Some(ColorSpec::Token(ColorToken::Surface)))
    );

    let badge_ansi = emit_ansi(&badge_frame, &badge_cfg);
    // Mocha Surface1 (SurfaceHi) = 0x45,0x47,0x5a (69;71;90), Surface0 (Surface) = 0x31,0x32,0x44 (49;50;68).
    assert!(badge_ansi.contains("48;2;69;71;90"), "ansi={badge_ansi:?}");
    assert!(badge_ansi.contains("48;2;49;50;68"), "ansi={badge_ansi:?}");
    assert!(!badge_ansi.contains('\u{e0b6}'));
    assert!(!badge_ansi.contains('\u{2502}'));

    // Ratatui preview parity matches emit_plain verbatim with zero ANSI re-parse.
    let badge_plain = emit_plain(&badge_frame, &badge_cfg);
    let ratatui_lines = rendered_frame_to_ratatui_lines(&badge_frame, &badge_cfg);
    let ratatui_text: String = ratatui_lines
        .iter()
        .map(|l| {
            let mut s: String = l.spans.iter().map(|sp| sp.content.as_ref()).collect();
            s.push('\n');
            s
        })
        .collect();
    assert_eq!(ratatui_text, badge_plain);

    // Pill mode on TrueColor + Nerd adds rounded Powerline endcaps matching capsule bg.
    let mut pill_doc = base_doc.clone();
    pill_doc.item_style = ItemStyle::Pill;
    pill_doc.color_depth = ColorDepth::TrueColor;
    pill_doc.glyphs = GlyphSet::Nerd;
    let pill_cfg = EffectiveConfig::from_doc(&pill_doc, None, &BTreeMap::new(), false);
    let pill_frame = render_lines_with_width(&frame, &pill_cfg, None);
    let pill_model = &pill_frame.lines[0].items[1];
    assert_eq!(
        pill_model.spans.first().map(|s| s.text.as_str()),
        Some("\u{e0b6}")
    );
    assert_eq!(
        pill_model.spans.first().map(|s| s.color),
        Some(ColorSpec::Token(ColorToken::SurfaceHi))
    );
    assert_eq!(pill_model.spans.first().and_then(|s| s.style.bg), None);
    assert_eq!(
        pill_model.spans.last().map(|s| s.text.as_str()),
        Some("\u{e0b4}")
    );
    assert_eq!(
        pill_model.spans.last().map(|s| s.color),
        Some(ColorSpec::Token(ColorToken::SurfaceHi))
    );
    assert_eq!(pill_model.spans.last().and_then(|s| s.style.bg), None);
    // Un-boxed Dir has no Powerline caps.
    assert!(
        !pill_frame.lines[0].items[5]
            .spans
            .iter()
            .any(|s| s.text.contains('\u{e0b6}'))
    );

    // Pill degrades to Badge on Unicode glyphs (no Powerline PUA chars, keeps SurfaceHi bg).
    let mut pill_uni = pill_doc.clone();
    pill_uni.glyphs = GlyphSet::Unicode;
    let pill_uni_cfg = EffectiveConfig::from_doc(&pill_uni, None, &BTreeMap::new(), false);
    let pill_uni_frame = render_lines_with_width(&frame, &pill_uni_cfg, None);
    let pill_uni_ansi = emit_ansi(&pill_uni_frame, &pill_uni_cfg);
    assert!(!pill_uni_ansi.contains('\u{e0b6}'));
    assert!(!pill_uni_ansi.contains('\u{e0b4}'));
    assert!(pill_uni_ansi.contains("48;2;69;71;90"));

    // Pill degrades to Minimal on Ansi16 (restores resolved " │ " separator, no surface bg).
    let mut pill_16 = pill_doc.clone();
    pill_16.color_depth = ColorDepth::Ansi16;
    let pill_16_cfg = EffectiveConfig::from_doc(&pill_16, None, &BTreeMap::new(), false);
    let pill_16_frame = render_lines_with_width(&frame, &pill_16_cfg, None);
    let pill_16_plain = emit_plain(&pill_16_frame, &pill_16_cfg);
    assert!(pill_16_plain.contains(" \u{2502} "));
    assert!(!pill_16_plain.contains('\u{e0b6}'));

    // Pill/Badge narrowing never exceeds tight budgets across all 3 narrowing stages.
    for budget in [90usize, 50, 24, 10, 4] {
        for cfg in [&badge_cfg, &pill_cfg] {
            let narrowed = render_lines_with_width(&frame, cfg, Some(budget));
            let plain = emit_plain(&narrowed, cfg);
            let ansi = emit_ansi(&narrowed, cfg);
            let w = str_display_width(plain.trim_end_matches('\n'));
            assert!(w <= budget, "width {w} > {budget}: plain={plain:?}");
            assert!(ansi.ends_with("\x1b[0m\n"));
        }
    }
}

// Compact Threshold (`compact_at`) collapses multi-row layouts strictly below cutoff into one
// compact row before priority narrowing, while explicit `single_line` / `AGY_STATUSLINE_LINES=1`
// keeps its original precedence.
#[test]
fn compact_threshold_collapses_multi_row_below_cutoff_and_respects_single_line_precedence() {
    let doc = ConfigDoc {
        compact_at: DEFAULT_COMPACT_AT, // 88
        palette: "catppuccin".to_string(),
        theme: ThemeSetting::Dark,
        color_depth: ColorDepth::TrueColor,
        ..agyline::build_preset_doc("hud", None).expect("hud preset")
    }; // 3-row HUD layout

    let payload_bytes = serde_json::to_vec(&serde_json::json!({
        "session_id": "compact_at_test",
        "agent_state": "working",
        "execution_mode": "accept-edits",
        "model": { "display_name": "Claude Opus 4.6", "effort": "xHigh" },
        "cwd": "/home/dev/projects/agyline",
        "sandbox": { "enabled": false },
        "context_window": {
            "current_usage": {
                "input_tokens": 12000,
                "output_tokens": 3400,
                "cache_read_input_tokens": 180000
            },
            "total_input_tokens": 192000,
            "total_output_tokens": 3400,
            "context_window_size": 1000000
        },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.68, "reset_in_seconds": 7200 },
            "3p-5h": { "remaining_fraction": 0.42, "reset_in_seconds": 5400 }
        },
        "cost": { "total_usd": 1.25 }
    }))
    .expect("serialize");
    let frame = agyline::FrameData {
        payload: agyline::parse_stdin_payload(&payload_bytes).expect("parse"),
        now_sec: 1_700_000_000,
        git: Some(agyline::GitFrameState {
            branch: "main".to_string(),
            dirty: true,
            modified: 2,
            ..agyline::GitFrameState::default()
        }),
        session_start_sec: Some(1_700_000_000 - 300),
        ..agyline::FrameData::default()
    };

    let cfg = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);

    // Wide (None and 120) and exact threshold boundary (88) must NOT collapse: stays 3 rows.
    for width in [None, Some(120), Some(88)] {
        let rendered = render_lines_with_width(&frame, &cfg, width);
        assert_eq!(
            rendered.lines.len(),
            3,
            "width={width:?} should remain 3 rows"
        );
        if let Some(budget) = width {
            for line in emit_plain(&rendered, &cfg).lines() {
                assert!(
                    str_display_width(line) <= budget,
                    "row exceeded {budget}: {line:?}"
                );
            }
        }
    }

    // Strictly below 88 collapses to a single compact row and fits every budget without wrapping.
    for budget in [87usize, 68, 40, 18, 8] {
        let rendered = render_lines_with_width(&frame, &cfg, Some(budget));
        assert_eq!(
            rendered.lines.len(),
            1,
            "budget={budget} < 88 must collapse to 1 row"
        );
        let plain = emit_plain(&rendered, &cfg);
        let row_w = str_display_width(plain.trim_end_matches('\n'));
        assert!(row_w <= budget, "row_w={row_w} > {budget}: {plain:?}");
    }

    // At 87 cols, compact forms are applied on the collapsed row (context bar drops bar -> Compact)
    // and row 1/2 high-priority items (like Gemini quota_group at P2) can survive alongside row 0.
    let collapsed_87 = render_lines_with_width(&frame, &cfg, Some(87));
    assert_eq!(collapsed_87.slot_outcome(0, 3), Some(SlotOutcome::Compact)); // context bar compacted
    assert_eq!(collapsed_87.slot_outcome(1, 0), Some(SlotOutcome::Compact)); // Gemini quota_group compacted & kept

    // compact_at = 0 disables automatic collapse even below 88.
    let mut disabled_doc = doc.clone();
    disabled_doc.compact_at = 0;
    let disabled_cfg = EffectiveConfig::from_doc(&disabled_doc, None, &BTreeMap::new(), false);
    let rendered_74 = render_lines_with_width(&frame, &disabled_cfg, Some(74));
    assert_eq!(rendered_74.lines.len(), 3);

    // Explicit AGY_STATUSLINE_LINES=1 takes precedence over compact_at: preserves single_line drop
    // semantics for non-@model secondary rows.
    let mut env_single = BTreeMap::new();
    env_single.insert("AGY_STATUSLINE_LINES".to_string(), "1".to_string());
    let single_cfg = EffectiveConfig::from_doc(&doc, None, &env_single, true);
    assert!(single_cfg.single_line);
    let single_rendered = render_lines_with_width(&frame, &single_cfg, Some(80));
    assert_eq!(single_rendered.lines.len(), 1);
    // In explicit single_line mode, non-@model row-1/row-2 items are immediately Dropped.
    assert_eq!(
        single_rendered.slot_outcome(1, 0),
        Some(SlotOutcome::Dropped)
    );
    assert_eq!(
        single_rendered.slot_outcome(2, 0),
        Some(SlotOutcome::Dropped)
    );

    // Pill container style + compact_at collapse stays wrap-free across wide, boundary, and narrow widths.
    let mut pill_doc = doc.clone();
    pill_doc.item_style = ItemStyle::Pill;
    let pill_cfg = EffectiveConfig::from_doc(&pill_doc, None, &BTreeMap::new(), false);
    assert_eq!(
        render_lines_with_width(&frame, &pill_cfg, Some(88))
            .lines
            .len(),
        3
    );
    for budget in [87usize, 50, 16] {
        let rendered = render_lines_with_width(&frame, &pill_cfg, Some(budget));
        assert_eq!(rendered.lines.len(), 1);
        let plain = emit_plain(&rendered, &pill_cfg);
        assert!(
            str_display_width(plain.trim_end_matches('\n')) <= budget,
            "pill collapsed exceeded {budget}: {plain:?}"
        );
    }
}

// Ticket #41: top-level `bar_style` / `bar_gradient` and `context.show_tokens` parsing, validation,
// tolerant alias normalization, preset scopes, TOML round-trip, and slot override precedence.
#[test]
fn top_level_bar_style_bar_gradient_and_context_show_tokens_parse_roundtrip_and_override() {
    use agyline::BarStyle;

    let def = ConfigDoc::default();
    assert_eq!(def.bar_style, BarStyle::Parallelogram);
    assert!(def.bar_gradient);
    let hud = agyline::build_preset_doc("hud", None).expect("hud preset");
    assert_eq!(hud.bar_style, BarStyle::Line);
    assert!(!hud.bar_gradient);
    let com = ConfigDoc::cometix_base();
    assert_eq!(com.bar_style, BarStyle::Line);
    assert!(!com.bar_gradient);

    let toml_src = r#"
palette = "catppuccin"
bar_style = "blocks"
bar_gradient = true

[[line]]
items = [
  { id = "context", style = "bar", show_tokens = true, label = "", icon = "" },
  { id = "quota_group", family = "gemini" },
  { id = "quota_group", family = "3p", bar_style = "braille" },
]
"#;
    let (doc, warnings) = parse_config_doc(toml_src).expect("parse top-level bar config");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(doc.bar_style, BarStyle::Parallelogram);
    assert!(doc.bar_gradient);
    assert_eq!(doc.lines[0][0].show_tokens, Some(true));
    assert!(!doc.lines[0][0].is_shorthand());

    let serialized = serialize_config_doc(&doc);
    assert!(serialized.contains("bar_style = \"parallelogram\""));
    assert!(serialized.contains("bar_gradient = true"));
    assert!(serialized.contains("show_tokens = true"));
    let (reparsed, rewarnings) = parse_config_doc(&serialized).expect("reparse bar config");
    assert!(rewarnings.is_empty(), "rewarnings: {rewarnings:?}");
    assert_eq!(reparsed, doc);

    // Invalid top-level values warn and retain defaults.
    let (bad_doc, bad_warnings) = parse_config_doc(
        r#"
bar_style = "wavy"
bar_gradient = "yes"
[[line]]
items = [{ id = "context", show_tokens = "nope" }]
"#,
    )
    .expect("tolerant parse");
    assert_eq!(bad_doc.bar_style, BarStyle::Parallelogram);
    assert!(bad_doc.bar_gradient);
    assert_eq!(bad_doc.lines[0][0].show_tokens, None);
    assert!(bad_warnings.len() >= 3, "warnings: {bad_warnings:?}");

    // Catalog exposes `show_tokens` on `context`.
    let cat = items::catalog();
    let ctx_meta = cat
        .iter()
        .find(|c| c.id == ItemId::Context)
        .expect("context");
    assert!(ctx_meta.options.iter().any(|o| o.key == "show_tokens"));

    // PresetScope::Layout preserves bar_style/bar_gradient; Style and All overwrite them.
    let mut scoped = doc.clone();
    assert!(apply_preset(
        &mut scoped,
        "minimal",
        PresetScope::Layout,
        None
    ));
    assert_eq!(scoped.bar_style, BarStyle::Parallelogram);
    assert!(scoped.bar_gradient);
    assert!(apply_preset(&mut scoped, "hud", PresetScope::Style, None));
    assert_eq!(scoped.bar_style, BarStyle::Line);
    assert!(!scoped.bar_gradient);

    // Top-level `bar_style` applies to slots without an explicit override; slot `bar_style` wins.
    let mut eff_doc = doc;
    eff_doc.color_depth = ColorDepth::TrueColor;
    let eff = EffectiveConfig::from_doc(&eff_doc, None, &BTreeMap::new(), false);
    assert_eq!(eff.bar_style, BarStyle::Parallelogram);
    assert!(eff.bar_gradient);

    let payload = agyline::parse_stdin_payload(
        br#"{
            "session_id": "bar_override",
            "context_window": {
                "current_usage": { "input_tokens": 50000 },
                "context_window_size": 200000
            },
            "quota": {
                "gemini-5h": { "remaining_fraction": 0.50 },
                "3p-5h": { "remaining_fraction": 0.50 }
            }
        }"#,
    )
    .expect("parse payload");
    let frame = agyline::FrameData {
        payload,
        now_sec: 1_700_000_000,
        ..agyline::FrameData::default()
    };
    let plain = agyline::emit_plain(&agyline::render_lines(&frame, &eff), &eff);
    assert!(
        plain.contains("▰▰▰▱▱▱▱▱▱▱ 25.0% (50k/200k)"),
        "context should inherit parallelogram: {plain:?}"
    );
    assert!(
        plain.contains("Gemini 5h ▰▰▰▰▰▱▱▱▱▱ 50.0%"),
        "gemini quota_group should inherit parallelogram: {plain:?}"
    );
    assert!(
        plain.contains("3P 5h ⣿⣿⣿⣿⣿⣀⣀⣀⣀⣀ 50.0%"),
        "3p quota_group should use slot braille override: {plain:?}"
    );
}

// Ticket #41: `context` (`style = "bar"`) four-tier colors, `(used/window)` counts, bold `→ /compact`
// crit alert, compact bar shortening, ASCII fallback, TUI span parity, and 4-tier `quota_group`.
#[test]
fn context_bar_four_tiers_tokens_ratio_compact_alert_and_quota_group_four_tiers() {
    use agyline::{BarStyle, emit_ansi, emit_plain, rendered_frame_to_ratatui_lines};
    use ratatui::style::{Color, Modifier};

    let mut ctx_slot = ItemSlot::new(ItemId::Context);
    ctx_slot.style = Some("bar".to_string());
    ctx_slot.show_tokens = Some(true);
    ctx_slot.label = Some(String::new());
    ctx_slot.icon = Some(String::new());
    ctx_slot.bar_width = Some(10);

    let mut qg_slot = ItemSlot::new(ItemId::QuotaGroup);
    qg_slot.family = Some("gemini".to_string());
    qg_slot.show_used = Some(false);
    qg_slot.show_reset = Some(false);
    qg_slot.icon = Some(String::new());

    let doc = ConfigDoc {
        palette: "catppuccin".to_string(),
        theme: ThemeSetting::Dark,
        color_depth: ColorDepth::TrueColor,
        bar_style: BarStyle::Parallelogram,
        bar_gradient: true,
        lines: vec![vec![ctx_slot, qg_slot]],
        ..ConfigDoc::default()
    };
    let cfg = EffectiveConfig::from_doc(&doc, None, &BTreeMap::new(), false);

    let make_frame = |used_toks: i64, q_rem: f64| -> agyline::FrameData {
        let json = serde_json::json!({
            "session_id": "ctx_4tier",
            "context_window": {
                "current_usage": { "input_tokens": used_toks },
                "context_window_size": 200_000
            },
            "quota": {
                "gemini-5h": { "remaining_fraction": q_rem }
            }
        });
        let payload =
            agyline::parse_stdin_payload(&serde_json::to_vec(&json).unwrap()).expect("payload");
        agyline::FrameData {
            payload,
            now_sec: 1_700_000_000,
            ..agyline::FrameData::default()
        }
    };

    // Ok (20%), Warn (65%), High (80%), Crit (92%) with Catppuccin Mocha RGB endpoints:
    // Ok=#a6e3a1 (166,227,161), Warn=#f9e2af (249,226,175), High=#fab387 (250,179,135), Crit=#f38ba8 (243,139,168).
    let tiers = [
        (
            40_000_i64,
            0.80_f64,
            "20.0% (40k/200k)",
            "166;227;161",
            false,
        ),
        (130_000, 0.30, "65.0% (130k/200k)", "249;226;175", false),
        (160_000, 0.20, "80.0% (160k/200k)", "250;179;135", false),
        (184_000, 0.05, "92.0% (184k/200k)", "243;139;168", true),
    ];
    for (used_toks, q_rem, exp_ctx, exp_rgb, exp_crit) in tiers {
        let frame = make_frame(used_toks, q_rem);
        let rendered = render_lines_with_width(&frame, &cfg, None);
        let plain = emit_plain(&rendered, &cfg);
        let ansi = emit_ansi(&rendered, &cfg);
        assert!(plain.contains(exp_ctx), "missing {exp_ctx} in {plain:?}");
        assert!(
            ansi.contains(&format!(";38;2;{exp_rgb}m")),
            "expected endpoint RGB {exp_rgb} in {ansi:?}"
        );
        if exp_crit {
            assert!(
                plain.contains("92.0% (184k/200k) \u{2192} /compact"),
                "expected compact hint in {plain:?}"
            );
            assert!(
                ansi.contains(";1;38;2;243;139;168m \u{2192} /compact"),
                "compact hint must be bold Crit in {ansi:?}"
            );
            // Shared Ratatui preview matches bold Crit compact hint and RGB gradient.
            let tui = rendered_frame_to_ratatui_lines(&rendered, &cfg);
            let alert_span = tui[0]
                .spans
                .iter()
                .find(|s| s.content.as_ref() == " \u{2192} /compact")
                .expect("tui alert span");
            assert_eq!(alert_span.style.fg, Some(Color::Rgb(243, 139, 168)));
            assert!(alert_span.style.add_modifier.contains(Modifier::BOLD));
        } else {
            assert!(!plain.contains("/compact"), "unexpected alert in {plain:?}");
        }
    }

    // Compact evaluation of crit context drops `(used/window)`, shortens bar to 5 cols, keeps `→ /compact`.
    let crit_frame = make_frame(184_000, 0.05);
    let evaluated =
        items::evaluate_slot_dual(&doc.lines[0][0], &crit_frame, &cfg).expect("ctx eval");
    let compact_text: String = evaluated
        .compact_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(compact_text, "▰▰▰▰▰ 92.0% \u{2192} /compact");

    // ASCII glyph mode degrades bar to #- and alert to ` -> /compact`.
    let mut asc_doc = doc.clone();
    asc_doc.glyphs = GlyphSet::Ascii;
    let asc_cfg = EffectiveConfig::from_doc(&asc_doc, None, &BTreeMap::new(), false);
    let asc_plain = emit_plain(
        &render_lines_with_width(&crit_frame, &asc_cfg, None),
        &asc_cfg,
    );
    assert!(
        asc_plain.contains("#########- 92.0% (184k/200k) -> /compact"),
        "ascii crit plain: {asc_plain:?}"
    );

    // Unconfounded 4-tier `quota_group`: context stays Ok (10%), quota hits High (20% rem -> Peach #fab387).
    let q_high_frame = make_frame(20_000, 0.20);
    let q_high_ansi = emit_ansi(&render_lines_with_width(&q_high_frame, &cfg, None), &cfg);
    assert!(
        q_high_ansi.contains(";38;2;250;179;135m 20.0%"),
        "quota_group High tier must emit Peach #fab387 independently of context: {q_high_ansi:?}"
    );

    // Custom label (`Some("mem")`), default label (`None` -> "ctx"), and missing window size omit `(used/window)`.
    let mut custom_lbl = doc.lines[0][0].clone();
    custom_lbl.label = Some("mem".to_string());
    let no_win_payload = agyline::parse_stdin_payload(
        br#"{ "session_id": "no_win", "context_window": { "used_percentage": 40.0 } }"#,
    )
    .expect("no_win payload");
    let no_win_frame = agyline::FrameData {
        payload: no_win_payload,
        now_sec: 1_700_000_000,
        ..agyline::FrameData::default()
    };
    let custom_eval = items::evaluate_slot_dual(&custom_lbl, &no_win_frame, &cfg).expect("eval");
    let custom_txt: String = custom_eval
        .full_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(custom_txt, "mem ▰▰▰▰▱▱▱▱▱▱ 40.0%");

    let mut default_lbl = custom_lbl;
    default_lbl.label = None;
    let def_eval = items::evaluate_slot_dual(&default_lbl, &no_win_frame, &cfg).expect("eval");
    let def_txt: String = def_eval
        .full_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(def_txt, "ctx ▰▰▰▰▱▱▱▱▱▱ 40.0%");
}

// Ticket #43: streaming `ai_credits` parsing, all 6 opt-in agybar Item variants + TOML round-trip,
// and the new 2-row Catppuccin `default` Preset alongside preserved `hud`/`classic`/`cometix`.
#[test]
fn payload_parses_ai_credits_leniently_and_new_item_variants_roundtrip_and_render() {
    use agyline::{
        BarStyle, FrameData, GitFrameState, QuotaBucketData, evaluate_slot_dual,
        parse_stdin_payload,
    };

    // 1. Streaming `ai_credits` tolerates numbers, floats, missing, and invalid types.
    let p_num = parse_stdin_payload(br#"{"ai_credits": 420}"#).expect("int ai_credits");
    assert_eq!(p_num.ai_credits, Some(420.0));
    let p_flt = parse_stdin_payload(br#"{"ai_credits": 12.5}"#).expect("float ai_credits");
    assert_eq!(p_flt.ai_credits, Some(12.5));
    let p_miss = parse_stdin_payload(br#"{}"#).expect("missing ai_credits");
    assert_eq!(p_miss.ai_credits, None);
    let p_bad =
        parse_stdin_payload(br#"{"ai_credits": {"nested": true}}"#).expect("bad ai_credits");
    assert_eq!(p_bad.ai_credits, None);

    // 2. Default Preset is the new 2-row Catppuccin gradient layout; hud/classic/cometix preserved.
    let def = ConfigDoc::default();
    assert_eq!(def.preset, "default");
    assert_eq!(def.palette, "catppuccin");
    assert_eq!(def.bar_style, BarStyle::Parallelogram);
    assert!(def.bar_gradient);
    assert_eq!(def.item_style, ItemStyle::Minimal);
    assert_eq!(def.separator, " │ ");
    assert_eq!(def.compact_at, 88);
    assert_eq!(def.lines.len(), 2);
    assert_eq!(
        def.lines[0].iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![
            ItemId::Model,
            ItemId::Context,
            ItemId::Tokens,
            ItemId::QuotaGroup
        ]
    );
    assert_eq!(
        def.lines[1].iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![
            ItemId::Status,
            ItemId::Git,
            ItemId::Dir,
            ItemId::Subagents,
            ItemId::Tasks,
            ItemId::Artifacts,
            ItemId::Sandbox,
        ]
    );

    let hud = agyline::build_preset_doc("hud", None).expect("hud preset");
    assert_eq!(hud.palette, "terminal");
    assert_eq!(hud.bar_style, BarStyle::Line);
    assert!(!hud.bar_gradient);
    assert_eq!(hud.compact_at, 0);
    assert_eq!(hud.lines.len(), 3);

    // Round-trip serialization of the new default preset.
    let serialized = serialize_config_doc(&def);
    let (reparsed, warnings) = parse_config_doc(&serialized).expect("roundtrip default");
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    assert_eq!(reparsed, def);

    // 3. Opt-in Item variants: model(agybar), tokens(arrows), quota_group(inline), status(dot),
    // git(branch), and home-tildefied middle-ellipsized dir.
    let mut env = BTreeMap::new();
    env.insert("AGY_THEME".to_string(), "dark".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    let cfg = EffectiveConfig::from_doc(&def, None, &env, true);

    // Model deduplication + Response Model substitution.
    let mut model_slot = ItemSlot::new(ItemId::Model);
    model_slot.style = Some("agybar".to_string());

    let mut frame = FrameData {
        payload: parse_stdin_payload(
            br#"{
                "session_id": "s43",
                "agent_state": "thinking",
                "model": { "display_name": "Claude Opus 4.6", "effort": "high" },
                "cwd": "/home/dev/work/clients/agyline",
                "context_window": {
                    "current_usage": { "input_tokens": 16000, "cache_read_input_tokens": 80000 },
                    "total_input_tokens": 48000,
                    "total_output_tokens": 12000,
                    "context_window_size": 200000
                },
                "quota": {
                    "3p-5h": { "remaining_fraction": 0.82, "reset_in_seconds": 8100 },
                    "3p-weekly": { "remaining_fraction": 0.64, "reset_in_seconds": 259200 },
                    "gemini-5h": { "remaining_fraction": 0.12, "reset_in_seconds": 600 }
                }
            }"#,
        )
        .expect("parse s43"),
        now_sec: 1_700_000_000,
        git: Some(GitFrameState {
            branch: "main".to_string(),
            dirty: true,
            ..GitFrameState::default()
        }),
        user_home: Some(PathBuf::from("/home/dev")),
        ..FrameData::default()
    };

    let m_eval = evaluate_slot_dual(&model_slot, &frame, &cfg).expect("model agybar");
    let m_full: String = m_eval.full_spans.iter().map(|s| s.text.as_str()).collect();
    let m_comp: String = m_eval
        .compact_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(m_full, "✦ Claude Opus 4.6 [high]");
    assert_eq!(m_comp, "✦ Claude Opus 4.6");
    assert!(m_eval.full_spans[0].style.bold);

    // Effort embedded in name must not duplicate `[high]`.
    frame.payload.model_display_name = "Gemini 2.5 Pro High".to_string();
    let m_dedup = evaluate_slot_dual(&model_slot, &frame, &cfg).expect("model dedup");
    let m_dedup_txt: String = m_dedup.full_spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(m_dedup_txt, "✦ Gemini 2.5 Pro High");

    // Response Model still substitutes picker alias.
    frame.payload.model_display_name = "Auto".to_string();
    frame.payload.model_effort = "xHigh".to_string();
    frame.response_model = Some("claude-sonnet-4-6".to_string());
    let m_resp = evaluate_slot_dual(&model_slot, &frame, &cfg).expect("response model");
    let m_resp_txt: String = m_resp.full_spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(m_resp_txt, "✦ Sonnet 4.6 [xhigh]");
    frame.response_model = None;
    frame.payload.model_display_name = "Claude Opus 4.6".to_string();
    frame.payload.model_effort = "high".to_string();

    // Tokens `arrows` style.
    let mut tok_slot = ItemSlot::new(ItemId::Tokens);
    tok_slot.style = Some("arrows".to_string());
    let tok_eval = evaluate_slot_dual(&tok_slot, &frame, &cfg).expect("tokens arrows");
    let tok_full: String = tok_eval
        .full_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(tok_full, "↑48k ↓12k");

    // Quota group `inline` style: active-model-matched (`3p-*`) first, urgency-sorted (`7d:64%` before `5h:82%`),
    // capped at 2 wide / 1 compact; `gemini-5h` (12%) does not beat active-model buckets.
    let mut q_slot = ItemSlot::new(ItemId::QuotaGroup);
    q_slot.style = Some("inline".to_string());
    q_slot.family = Some("@model".to_string());
    q_slot.show_reset = Some(true);
    let q_eval = evaluate_slot_dual(&q_slot, &frame, &cfg).expect("quota inline");
    let q_full: String = q_eval.full_spans.iter().map(|s| s.text.as_str()).collect();
    let q_comp: String = q_eval
        .compact_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(q_full, "⚡ 7d:64%(3d) · 5h:82%(2h15m)");
    assert_eq!(q_comp, "⚡ 7d:64%(3d)");

    // `ai_credits` fallback when quota buckets are absent or sparse (1 bucket).
    let mut cr_frame = frame.clone();
    cr_frame.payload.quota_buckets.clear();
    cr_frame.payload.ai_credits = Some(420.0);
    let cr_only = evaluate_slot_dual(&q_slot, &cr_frame, &cfg).expect("ai_credits only");
    let cr_only_txt: String = cr_only.full_spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(cr_only_txt, "⚡ cr:420");

    cr_frame.payload.quota_buckets = vec![QuotaBucketData {
        name: "3p-5h".to_string(),
        disabled: false,
        remaining_fraction: Some(0.18),
        remaining_amount: None,
        reset_in_seconds: Some(18 * 60),
        reset_time: None,
    }];
    let cr_sparse = evaluate_slot_dual(&q_slot, &cr_frame, &cfg).expect("sparse + cr");
    let cr_sparse_full: String = cr_sparse
        .full_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    let cr_sparse_comp: String = cr_sparse
        .compact_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(cr_sparse_full, "⚡ 5h:18%(18m) · cr:420");
    assert_eq!(cr_sparse_comp, "⚡ 5h:18%(18m)");

    // Status `dot` style: 5 canonical states + dot-only compact + CONFIRM urgency preserved in compact.
    let mut st_slot = ItemSlot::new(ItemId::Status);
    st_slot.style = Some("dot".to_string());
    for (state, exp_full, exp_comp) in [
        ("thinking", "◉ thinking", "◉"),
        ("working", "● working", "●"),
        ("tool_use", "⚙ tool_use", "⚙"),
        ("error", "✖ error", "✖"),
        ("idle", "○ idle", "○"),
    ] {
        let mut sf = frame.clone();
        sf.payload.agent_state = state.to_string();
        let ev = evaluate_slot_dual(&st_slot, &sf, &cfg).expect("status dot");
        let ft: String = ev.full_spans.iter().map(|s| s.text.as_str()).collect();
        let ct: String = ev.compact_spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(ft, exp_full, "state={state}");
        assert_eq!(ct, exp_comp, "state={state}");
    }
    let mut conf_frame = frame.clone();
    conf_frame.payload.tool_confirmation_pending = true;
    let conf_ev = evaluate_slot_dual(&st_slot, &conf_frame, &cfg).expect("status confirm");
    let conf_comp: String = conf_ev
        .compact_spans
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert!(
        conf_comp.contains("CONFIRM"),
        "compact status dot must preserve CONFIRM urgency: {conf_comp:?}"
    );

    // Git `branch` style: `⎇ <branch>` + bold warn `*`.
    let mut git_slot = ItemSlot::new(ItemId::Git);
    git_slot.style = Some("branch".to_string());
    let g_ev = evaluate_slot_dual(&git_slot, &frame, &cfg).expect("git branch");
    let g_txt: String = g_ev.full_spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(g_txt, "⎇ main*");
    assert!(
        g_ev.full_spans
            .last()
            .is_some_and(|s| s.text == "*" && s.style.bold && s.color == ColorToken::Warn.into())
    );

    // Dir home-tildefied middle ellipsis `~/…/<parent>/<leaf>` honoring `icon = ""`.
    let mut dir_slot = ItemSlot::new(ItemId::Dir);
    dir_slot.max_levels = Some(2);
    dir_slot.icon = Some(String::new());
    let d_ev = evaluate_slot_dual(&dir_slot, &frame, &cfg).expect("dir middle ellipsis");
    let d_txt: String = d_ev.full_spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(d_txt, "~/…/clients/agyline");
}
