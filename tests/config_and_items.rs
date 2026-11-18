//! Config resolution and per-item evaluation: the precedence ladder (AGY_* env > config.toml >
//! preset > built-in default), tolerant per-field degradation, palette/color-depth SGR output,
//! and the 20-item catalog rendered through the shared width-narrowing pipeline.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agyline::{
    ColorDepth, ColorToken, ConfigDoc, EffectiveConfig, GlyphSet, ItemId, ItemSlot, NoopSpawner,
    OverflowMode, PresetScope, RenderEnv, SlotOutcome, ThemePolarity, ThemeSetting, apply_preset,
    builtin_palettes, builtin_presets, discover_palettes, discover_presets, inspect_env_overrides,
    items, load_config_doc, parse_config_doc, render_lines_with_width, run_live_frame,
    serialize_config_doc, str_display_width,
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
    assert!(default_toml.contains("\"model\""));
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
    assert_eq!(builtin_presets().len(), 5);

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
    // Palette catalog: 8 built-ins plus custom `palettes/*.toml` from AGYLINE_HOME.
    assert_eq!(builtin_palettes().len(), 8);

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
        out_256.ansi.contains("\x1b[0;38;5;244m | "),
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
        "  \u{ef23} Model | \u{f49b} 0% | \u{f1c0} cold ! config\n"
    );
    assert!(
        out.ansi.ends_with("\x1b[0;31m ! config\x1b[0m\n"),
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
    assert_eq!(doc.thresholds.context_warn, 50.0);
    assert_eq!(doc.thresholds.context_crit, 80.0);
    assert_eq!(doc.thresholds.quota_warn, 45);
    assert_eq!(doc.thresholds.quota_crit, 20);
    assert_eq!(doc.thresholds.cache_warn_secs, 60);
    assert_eq!(doc.lines.len(), 1);
    assert_eq!(doc.lines[0].len(), 2);
    assert_eq!(doc.lines[0][0].id, ItemId::Model);
    assert_eq!(doc.lines[0][1].id, ItemId::Context);
    assert_eq!(doc.lines[0][1].priority, None);
}

// The catalog is exactly the 20 ItemId::ALL entries in order; quota is the only repeatable item
// (allow_multiple, 4 options), so the configurator can place independent quota buckets.
#[test]
fn items_catalog_exposes_all_twenty_items_with_priorities_and_options() {
    let cat = items::catalog();
    assert_eq!(cat.len(), 20);
    for (idx, expected_id) in ItemId::ALL.iter().copied().enumerate() {
        assert_eq!(cat[idx].id, expected_id);
        assert_eq!(cat[idx].key, expected_id.as_key());
        assert_eq!(cat[idx].default_priority, expected_id.default_priority());
    }
    let quota_meta = cat.iter().find(|c| c.id == ItemId::Quota).expect("quota");
    assert!(quota_meta.allow_multiple);
    assert_eq!(quota_meta.options.len(), 4);

    let model_meta = cat.iter().find(|c| c.id == ItemId::Model).expect("model");
    assert!(!model_meta.allow_multiple);
    assert!(model_meta.options.is_empty());
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
        }),
        cache: agyline::CacheFrameState {
            cache_ts: Some(1_700_000_010 - 115),
            relay_touches: 1,
            relay_status: Some(agyline::RelayStatus::Live),
        },
        last_tps: Some("64.5".to_string()),
        response_model: None,
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
