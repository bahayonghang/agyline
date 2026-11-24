//! Configurator contracts on a TestBackend: the preview must equal the live renderer with no ANSI
//! re-parsing, edits must survive navigation, modals and undo/redo, and saving must never clobber
//! a broken or externally changed config.toml. The agy Hook tab installs and restores
//! settings.json without implicitly saving the config draft.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

use agyline::{
    Ansi16Color, BarStyle, ColorDepth, ConfigDoc, ConfiguratorApp, ConfiguratorEnv,
    ConfiguratorExit, DEFAULT_COMPACT_AT, EffectiveConfig, GlyphSet, InstallEnv, ItemId, ItemSlot,
    ItemStyle, PlatformKind, SlotOutcome, ThemeSetting, TuiTab, builtin_scenarios, emit_ansi,
    emit_plain, render_lines_with_width, render_tui_frame, rendered_frame_to_ratatui_lines,
    serialize_config_doc,
};

static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

/// One temp root for the config home, settings.json and exe path, so a case can assert exactly
/// which files the configurator created. Removed on drop.
struct Sandbox {
    root: PathBuf,
    agyline_home: PathBuf,
    settings_path: PathBuf,
    exe_path: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "agyline_tui_smoke_{}_{nanos}_{seq}",
            std::process::id()
        ));
        let agyline_home = root.join("agyline-home");
        let settings_path = root
            .join("user-home")
            .join(".gemini")
            .join("antigravity-cli")
            .join("settings.json");
        let exe_path = root.join("bin").join("agyline");
        Self {
            root,
            agyline_home,
            settings_path,
            exe_path,
        }
    }

    /// ConfiguratorEnv pinned to the scenario anchor clock and the Unix platform, so previews and
    /// install diffs stay deterministic across hosts.
    fn env(&self, env_vars: BTreeMap<String, String>) -> ConfiguratorEnv {
        let install_env = InstallEnv {
            settings_path: self.settings_path.clone(),
            agyline_home: self.agyline_home.clone(),
            user_home: Some(self.root.join("user-home")),
            exe_path: self.exe_path.clone(),
            path_env: None,
            platform: PlatformKind::Unix,
            short_path_resolver: Arc::new(|_: &Path| None),
        };
        ConfiguratorEnv::for_sandbox(
            self.agyline_home.clone(),
            env_vars,
            install_env,
            1_700_000_300_000,
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn ctrl_press(ch: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn left_click(col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// Windows console delivers Release for the same key press; the app must ignore it or every edit
/// would apply twice.
fn release(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    })
}

/// Auto-repeat event, ignored like Release so a held key cannot double-apply.
fn repeat_key(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Repeat,
        state: KeyEventState::NONE,
    })
}

/// Flattens the drawn buffer to trimmed rows: assertions search what is actually visible, and
/// mouse clicks are aimed at columns found here.
fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Span text per line with no styling: comparing this against `emit_plain` is what proves the
/// preview never round-trips through ANSI.
fn lines_to_plain_string(lines: &[ratatui::text::Line<'static>]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            out.push_str(span.content.as_ref());
        }
        out.push('\n');
    }
    out
}

// The preview shares the live renderer: conversion must be lossless for text and for color at
// every depth, across all built-in scenarios.
#[test]
fn convert_matches_render_lines_plain_text_and_maps_colors_losslessly() {
    let env_vars = BTreeMap::new();
    let mut doc = ConfigDoc::cometix_base();

    // Every built-in scenario must convert to the same plain text the live renderer emits.
    for sc in builtin_scenarios() {
        let cfg = EffectiveConfig::from_doc(&doc, None, &env_vars, true);
        let frame = sc.frame_for_env(&env_vars);
        let rendered = render_lines_with_width(&frame, &cfg, Some(sc.default_width));
        let ratatui_lines = rendered_frame_to_ratatui_lines(&rendered, &cfg);
        let converted_plain = lines_to_plain_string(&ratatui_lines);
        let expected_plain = emit_plain(&rendered, &cfg);
        assert_eq!(
            converted_plain, expected_plain,
            "scenario {} plain text must match emit_plain 1:1",
            sc.name
        );
    }

    // Verify broken config appends row-1 " ! config" in Crit color without ANSI round-trip.
    let mut broken_cfg = EffectiveConfig::from_doc(&doc, None, &env_vars, false);
    broken_cfg.config_broken = true;
    let normal = &builtin_scenarios()[0];
    let broken_rendered = render_lines_with_width(&normal.frame, &broken_cfg, Some(112));
    let broken_lines = rendered_frame_to_ratatui_lines(&broken_rendered, &broken_cfg);
    assert_eq!(
        lines_to_plain_string(&broken_lines),
        emit_plain(&broken_rendered, &broken_cfg)
    );
    let last_span = broken_lines[0].spans.last().expect("broken suffix span");
    assert_eq!(last_span.content.as_ref(), " ! config");
    assert_eq!(last_span.style.fg, Some(Color::Red));

    // Verify TrueColor RGB, ANSI 256, and None color depths map directly to ratatui::style::Color.
    doc.palette = "catppuccin-mocha".to_string();
    doc.color_depth = ColorDepth::TrueColor;
    let rgb_cfg = EffectiveConfig::from_doc(&doc, None, &env_vars, false);
    let rgb_rendered = render_lines_with_width(&normal.frame, &rgb_cfg, Some(112));
    let rgb_lines = rendered_frame_to_ratatui_lines(&rgb_rendered, &rgb_cfg);
    assert!(
        rgb_lines[0]
            .spans
            .iter()
            .any(|sp| matches!(sp.style.fg, Some(Color::Rgb(_, _, _)))),
        "TrueColor palette must emit Color::Rgb spans"
    );

    doc.color_depth = ColorDepth::Ansi256;
    let c256_cfg = EffectiveConfig::from_doc(&doc, None, &env_vars, false);
    let c256_rendered = render_lines_with_width(&normal.frame, &c256_cfg, Some(112));
    let c256_lines = rendered_frame_to_ratatui_lines(&c256_rendered, &c256_cfg);
    assert!(
        c256_lines[0]
            .spans
            .iter()
            .any(|sp| matches!(sp.style.fg, Some(Color::Indexed(_)))),
        "Ansi256 depth must emit Color::Indexed spans"
    );

    doc.color_depth = ColorDepth::None;
    let plain_cfg = EffectiveConfig::from_doc(&doc, None, &env_vars, false);
    let plain_rendered = render_lines_with_width(&normal.frame, &plain_cfg, Some(112));
    let plain_lines = rendered_frame_to_ratatui_lines(&plain_rendered, &plain_cfg);
    assert!(
        plain_lines[0].spans.iter().all(|sp| sp.style.fg.is_none()),
        "ColorDepth::None must emit uncolored spans"
    );
    let _ = Ansi16Color::ALL;
}

// Selection must scroll into view at both supported sizes, and a mouse click must hit the item
// actually drawn at that cell, not the item at that index.
#[test]
fn layout_catalog_selection_scrolls_into_view_and_mouse_uses_visible_items() {
    for (w, h) in [(80, 22), (100, 28)] {
        let sb = Sandbox::new();
        let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
        app.handle_event(Event::Resize(w, h));
        app.handle_event(press(KeyCode::Tab));
        // Walk past the visible window across all 24 catalog items so Plan (index 22) scrolls into view.
        for _ in 0..22 {
            app.handle_event(press(KeyCode::Down));
        }
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        render_tui_frame(&mut terminal, &app).expect("draw last catalog item");
        let screen = buffer_text(&terminal);
        assert!(
            screen.contains("> [ ] plan"),
            "selected Plan must be visible at {w}x{h}:\n{screen}"
        );
        // Click the preceding visible Catalog item, then add it to the active row.
        app.handle_event(left_click(w * 34 / 100 + 2, h - 4));
        app.handle_event(press(KeyCode::Enter));
        assert_eq!(
            app.doc().lines[0].last().map(|s| s.id),
            Some(ItemId::Version)
        );
    }
}

// Active Rows obeys the same scroll and mouse contract as the catalog, with a full 20-item row
// loaded from config.toml; 'x' must remove the clicked slot, not its neighbor.
#[test]
fn active_rows_selection_scrolls_into_view_and_mouse_uses_visible_items() {
    for (w, h) in [(80, 22), (100, 28)] {
        let sb = Sandbox::new();
        std::fs::create_dir_all(&sb.agyline_home).expect("config home");
        let mut doc = ConfigDoc::cometix_base();
        // Fill row 1 with every catalog item so the pane must scroll at both sizes.
        doc.lines = vec![
            ItemId::ALL.into_iter().map(ItemSlot::new).collect(),
            Vec::new(),
        ];
        std::fs::write(
            sb.agyline_home.join("config.toml"),
            serialize_config_doc(&doc),
        )
        .expect("config");
        let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
        app.handle_event(Event::Resize(w, h));
        for _ in 0..19 {
            app.handle_event(press(KeyCode::Down));
        }
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        render_tui_frame(&mut terminal, &app).expect("draw last active item");
        let screen = buffer_text(&terminal);
        assert!(
            screen.contains("> plan P5"),
            "selected Plan must be visible at {w}x{h}:\n{screen}"
        );
        // Hit-testing must use the scrolled viewport, not the unscrolled item list.
        app.handle_event(left_click(2, h - 4));
        app.handle_event(press(KeyCode::Char('x')));
        assert!(
            !app.doc().lines[0]
                .iter()
                .any(|slot| slot.id == ItemId::Version)
        );
        assert!(
            app.doc().lines[0]
                .iter()
                .any(|slot| slot.id == ItemId::Plan)
        );
    }
}

// At the minimum 80x22 all four tab titles must be drawn and clickable at the column where they
// actually appear.
#[test]
fn minimum_terminal_exposes_all_four_clickable_tabs() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app.handle_event(Event::Resize(80, 22));
    let mut terminal = Terminal::new(TestBackend::new(80, 22)).expect("terminal");
    for (title, tab) in [
        ("1: Layout", TuiTab::Layout),
        ("2: Style", TuiTab::StylePresets),
        ("3: Runtime", TuiTab::ThresholdsRuntime),
        ("4: agy Hook", TuiTab::AgyHook),
    ] {
        render_tui_frame(&mut terminal, &app).expect("draw tabs");
        let screen = buffer_text(&terminal);
        let tab_row = app.preview_box_height() + 1;
        let tab_line = screen.lines().nth(tab_row as usize).expect("tab line");
        // Click the column where the title was drawn, not a hardcoded offset.
        let column = tab_line
            .find(title)
            .unwrap_or_else(|| panic!("missing {title}: {tab_line}"));
        app.handle_event(left_click(column as u16 + 2, tab_row));
        assert_eq!(app.active_tab(), tab);
    }
}

#[test]
fn all_four_tabs_scenarios_width_ruler_and_resize_guard_render_cleanly() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));

    // Key Release and Repeat events must be ignored (Windows console double-fire guard).
    assert_eq!(app.active_scenario().name, "normal");
    assert_eq!(app.handle_event(release(KeyCode::Char('p'))), None);
    assert_eq!(app.handle_event(repeat_key(KeyCode::Char('p'))), None);
    assert_eq!(app.active_scenario().name, "normal");
    assert_eq!(app.handle_event(release(KeyCode::Char('q'))), None);

    // Render all 4 tabs at 100x28 and 80x22.
    for (w, h) in [(100_u16, 28_u16), (80_u16, 22_u16)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
        app.handle_event(Event::Resize(w, h));

        for (expected_tab, expected_marker) in [
            (TuiTab::Layout, "Active Rows"),
            (TuiTab::StylePresets, "palette"),
            (TuiTab::ThresholdsRuntime, "context_warn"),
            (TuiTab::AgyHook, "settings.json"),
        ] {
            assert_eq!(app.active_tab(), expected_tab);
            render_tui_frame(&mut terminal, &app).expect("draw tab");
            let screen = buffer_text(&terminal);
            assert!(
                screen.contains(expected_marker),
                "tab {expected_tab:?} at {w}x{h} missing '{expected_marker}':\n{screen}"
            );
            app.handle_event(press(KeyCode::Char('}')));
        }
        assert_eq!(app.active_tab(), TuiTab::Layout);
    }

    // Cycle through all 9 scenarios and verify preview matches render_lines_with_width;
    // switching to narrow_stress defaults simulated width to 74 cols when unpinned.
    let mut seen_scenarios = Vec::new();
    for _ in 0..builtin_scenarios().len() {
        seen_scenarios.push(app.active_scenario().name);
        let (rendered, eff) = app.preview_rendered_frame();
        let direct = render_lines_with_width(
            &app.active_scenario().frame_for_env(&BTreeMap::new()),
            &eff,
            app.simulated_width(),
        );
        assert_eq!(rendered, direct);
        assert_eq!(
            lines_to_plain_string(&app.preview_lines()),
            emit_plain(&direct, &eff)
        );
        if app.active_scenario().name == "narrow_stress" {
            assert_eq!(app.simulated_width(), Some(74));
            // At 74 cols on narrow_stress, narrowing outcomes must include Compact or Dropped!
            let has_narrowed = rendered.slot_outcomes[0].iter().any(|o| {
                matches!(
                    o,
                    SlotOutcome::Compact | SlotOutcome::Dropped | SlotOutcome::Truncated
                )
            });
            assert!(
                has_narrowed,
                "narrow_stress at 74 cols must trigger narrowing badges"
            );

            let mut term = Terminal::new(TestBackend::new(100, 28)).expect("term");
            render_tui_frame(&mut term, &app).expect("draw narrow_stress");
            let txt = buffer_text(&term);
            assert!(
                txt.contains("COMPACT") || txt.contains("DROP") || txt.contains("TRUNC"),
                "Layout Active Rows must display live COMPACT/DROP/TRUNC badge:\n{txt}"
            );
        }
        app.handle_event(press(KeyCode::Char('p')));
    }
    assert_eq!(
        seen_scenarios,
        vec![
            "normal",
            "pre_session",
            "cache_warm_touch",
            "busy_subagents",
            "warn_crit",
            "narrow_stress",
            "showcase_wide",
            "showcase_critical",
            "showcase_compact",
        ]
    );

    // Adjust width via '[' / ']' (±4 cols clamped to 40..=200) and '0' for unlimited '∞'.
    assert_eq!(app.simulated_width(), Some(112));
    app.handle_event(press(KeyCode::Char('[')));
    assert_eq!(app.simulated_width(), Some(108));
    app.handle_event(press(KeyCode::Char(']')));
    app.handle_event(press(KeyCode::Char(']')));
    assert_eq!(app.simulated_width(), Some(116));
    app.handle_event(press(KeyCode::Char('0')));
    assert_eq!(app.simulated_width(), None);
    app.handle_event(press(KeyCode::Char('0')));
    assert_eq!(app.simulated_width(), Some(116));

    // Resize below 80x22 (60x15) renders resize guard without panicking, and recovers at 80x22.
    let mut small_term = Terminal::new(TestBackend::new(60, 15)).expect("small term");
    app.handle_event(Event::Resize(60, 15));
    render_tui_frame(&mut small_term, &app).expect("draw small guard");
    let small_txt = buffer_text(&small_term);
    assert!(
        small_txt.contains("60")
            && small_txt.contains("15")
            && small_txt.contains("80")
            && small_txt.contains("22"),
        "resize guard must show current 60x15 and required 80x22:\n{small_txt}"
    );

    let mut recovered_term = Terminal::new(TestBackend::new(80, 22)).expect("recovered term");
    app.handle_event(Event::Resize(80, 22));
    render_tui_frame(&mut recovered_term, &app).expect("draw recovered");
    let rec_txt = buffer_text(&recovered_term);
    assert!(rec_txt.contains("Active Rows"));

    // Clean exit on 'q' when unmodified creates zero files.
    let exit = app.handle_event(press(KeyCode::Char('q')));
    assert_eq!(exit, Some(ConfiguratorExit::Cancelled));
    assert!(!sb.agyline_home.exists());
}

/// Types a string as Press events: each must be consumed by the inline editor, never as a hotkey
/// or an exit.
fn type_str(app: &mut ConfiguratorApp, s: &str) {
    for ch in s.chars() {
        assert_eq!(
            app.handle_event(press(KeyCode::Char(ch))),
            None,
            "typing '{ch}' during inline edit must not exit"
        );
    }
}

#[test]
fn layout_quota_multi_buckets_reorder_inspector_and_toml_drawer_shorthands() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(&sb.agyline_home).expect("mkdir");
    std::fs::write(
        sb.agyline_home.join("config.toml"),
        "preset = \"cometix\"\n",
    )
    .expect("write cometix");
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));

    // Reference Line 1 ends with 'budget' (index 6). Navigate to 'budget' and remove it with 'x'.
    for _ in 0..6 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Char('x')));
    assert!(!app.doc().lines[0].iter().any(|s| s.id == ItemId::Budget));

    // Switch to Catalog (Tab), navigate down to 'quota' (Workspace: 3 items, Usage: context, cache, speed, budget, quota -> index 7).
    app.handle_event(press(KeyCode::Tab));
    for _ in 0..7 {
        app.handle_event(press(KeyCode::Down));
    }
    // Add TWO independent 'quota' instances to Line 1 via Space twice!
    app.handle_event(press(KeyCode::Char(' ')));
    app.handle_event(press(KeyCode::Char(' ')));
    let quota_indices: Vec<usize> = app.doc().lines[0]
        .iter()
        .enumerate()
        .filter_map(|(i, s)| (s.id == ItemId::Quota).then_some(i))
        .collect();
    assert_eq!(quota_indices.len(), 2, "must allow multiple quota slots");

    // Focus Inspector (Tab) for the newly added second quota slot; customize bucket, label, show_reset.
    app.handle_event(press(KeyCode::Tab));
    // Inspector fields for quota: 0=priority, 1=icon, 2=fg, 3=icon_color, 4=bucket, 5=label, 6=show_reset, 7=show_amount
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text());
    type_str(&mut app, "3p-5h");
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.is_editing_text());

    app.handle_event(press(KeyCode::Down)); // label
    app.handle_event(press(KeyCode::Enter));
    type_str(&mut app, "3p");
    app.handle_event(press(KeyCode::Enter));

    app.handle_event(press(KeyCode::Down)); // show_reset
    app.handle_event(press(KeyCode::Char(' ')));

    // Set priority to P2 via hotkey '2', then reset priority back to default via Delete on priority row!
    app.handle_event(press(KeyCode::Char('2')));
    assert_eq!(app.doc().lines[0][7].priority, Some(2));
    for _ in 0..6 {
        app.handle_event(press(KeyCode::Up));
    }
    app.handle_event(press(KeyCode::Delete));
    assert_eq!(app.doc().lines[0][7].priority, None);

    // Return to Active Rows (Tab), reorder customized quota up with 'K', and move first quota to Line 2 with 'm'.
    app.handle_event(press(KeyCode::Tab));
    app.handle_event(press(KeyCode::Char('K')));
    assert_eq!(
        app.doc().lines[0][6].bucket.as_deref(),
        Some("3p-5h"),
        "'K' must move selected slot up within row"
    );
    app.handle_event(press(KeyCode::Down)); // select uncustomized quota at index 7
    app.handle_event(press(KeyCode::Char('m'))); // move to Line 2
    assert_eq!(app.doc().lines[1].last().map(|s| s.id), Some(ItemId::Quota));

    // Toggle TOML drawer with 't' and verify string shorthands for uncustomized items + inline table for customized quota.
    app.handle_event(press(KeyCode::Char('t')));
    assert!(app.is_toml_drawer_open());
    let toml_out = serialize_config_doc(app.doc());
    assert!(
        toml_out.contains("\"model\","),
        "uncustomized items in mixed row must remain string shorthands:\n{toml_out}"
    );
    assert!(
        toml_out
            .contains("{ id = \"quota\", bucket = \"3p-5h\", label = \"3p\", show_reset = true }"),
        "customized quota must serialize as inline table:\n{toml_out}"
    );

    let mut term = Terminal::new(TestBackend::new(120, 30)).expect("term");
    render_tui_frame(&mut term, &app).expect("draw with toml drawer");
    let screen = buffer_text(&term);
    assert!(screen.contains("Live config.toml"));
    assert!(screen.contains("3p-5h"));
}

#[test]
fn tier2_inline_text_input_isolates_hotkeys_and_runtime_validates_thresholds_and_url() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));

    // Switch to Tab 2 (Style & Presets -> F2), focus Global Style pane, edit `separator`.
    app.handle_event(press(KeyCode::F(2)));
    assert_eq!(app.active_tab(), TuiTab::StylePresets);
    app.handle_event(press(KeyCode::Tab)); // Presets -> GlobalStyle
    // GlobalStyle fields: 0=palette, 1=theme, 2=glyphs, 3=color_depth, 4=item_style, 5=bar_style, 6=bar_gradient, 7=separator, 8=indent
    for _ in 0..7 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text());

    // Clear existing " | " with Backspace x3 and type hotkey-heavy string "qspt1[]{}".
    for _ in 0..3 {
        app.handle_event(press(KeyCode::Backspace));
    }
    let scenario_before = app.active_scenario().name;
    let width_before = app.simulated_width();
    let drawer_before = app.is_toml_drawer_open();
    type_str(&mut app, "qspt1[]{}");
    app.handle_event(press(KeyCode::Enter));

    assert!(!app.is_editing_text());
    assert_eq!(app.doc().separator, "qspt1[]{}");
    assert_eq!(app.active_tab(), TuiTab::StylePresets);
    assert_eq!(app.active_scenario().name, scenario_before);
    assert_eq!(app.simulated_width(), width_before);
    assert_eq!(app.is_toml_drawer_open(), drawer_before);

    // Start another inline edit on `separator`, type garbage, press Esc -> must restore "qspt1[]{}".
    app.handle_event(press(KeyCode::Enter));
    type_str(&mut app, "discarded");
    app.handle_event(press(KeyCode::Esc));
    assert!(!app.is_editing_text());
    assert_eq!(app.doc().separator, "qspt1[]{}");

    // Switch to Tab 3 (Thresholds & Runtime -> F3): verify threshold ordering & loopback URL warning.
    app.handle_event(press(KeyCode::F(3)));
    assert_eq!(app.active_tab(), TuiTab::ThresholdsRuntime);
    // Fields in Thresholds pane: 0=single_line, 1=compact_at, 2=overflow, 3=context_warn, 4=context_high, 5=context_crit
    for _ in 0..3 {
        app.handle_event(press(KeyCode::Down));
    }
    // Try stepping context_warn up 40 times; must clamp strictly below context_high (< context_crit)!
    for _ in 0..40 {
        app.handle_event(press(KeyCode::Right));
    }
    assert!(
        app.doc().thresholds.context_warn < app.doc().thresholds.context_high
            && app.doc().thresholds.context_high < app.doc().thresholds.context_crit,
        "context thresholds must keep warn ({}) < high ({}) < crit ({})",
        app.doc().thresholds.context_warn,
        app.doc().thresholds.context_high,
        app.doc().thresholds.context_crit
    );

    // Focus Runtime pane (Tab): 0=[git].cache_ms, 1=[cache_relay].url
    app.handle_event(press(KeyCode::Tab));
    app.handle_event(press(KeyCode::Down)); // url
    app.handle_event(press(KeyCode::Enter));
    type_str(&mut app, "https://evil.example.com:8080");
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().cache_relay_url, "https://evil.example.com:8080");

    let mut term = Terminal::new(TestBackend::new(100, 28)).expect("term");
    render_tui_frame(&mut term, &app).expect("draw runtime warning");
    let screen = buffer_text(&term);
    assert!(
        screen.contains("loopback") && screen.contains("http://"),
        "non-loopback relay url must render live loopback http:// warning:\n{screen}"
    );
}

#[test]
fn preset_modal_scopes_and_hundred_step_undo_redo_restore_exact_doc() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    let initial_doc = app.doc().clone();

    // Open the preset modal from the default layout.
    app.handle_event(press(KeyCode::Char('r')));
    assert!(app.has_active_modal());
    // Select 'dashboard' (Down x5), scope 'Style' (Right x2: All -> Layout -> Style), Enter!
    for _ in 0..5 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Right));
    app.handle_event(press(KeyCode::Right));
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.has_active_modal());
    assert_eq!(app.doc().palette, "tokyo-night");
    assert_eq!(
        app.doc().lines,
        initial_doc.lines,
        "Style scope must not alter row layout"
    );
    let after_style = app.doc().clone();

    // Open Preset modal again: select 'minimal' (Down x4), scope 'Layout' (Right x1)
    app.handle_event(press(KeyCode::Char('r')));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Right)); // All -> Layout
    app.handle_event(press(KeyCode::Enter));
    assert!(app.doc().single_line);
    assert_eq!(app.doc().lines[0].len(), 4);
    assert_eq!(
        app.doc().palette,
        "tokyo-night",
        "Layout scope must preserve existing style"
    );
    let after_layout = app.doc().clone();

    // Open Preset modal: select 'cache-focus' (Down x6), scope 'All' (default), Enter
    app.handle_event(press(KeyCode::Char('r')));
    for _ in 0..6 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().palette, "catppuccin-mocha");
    assert!(!app.doc().single_line);
    let after_all = app.doc().clone();

    // Undo x3 restores exact intermediate states and initial doc; Redo x3 replays them!
    app.handle_event(ctrl_press('z'));
    assert_eq!(app.doc(), &after_layout);
    app.handle_event(ctrl_press('z'));
    assert_eq!(app.doc(), &after_style);
    app.handle_event(ctrl_press('z'));
    assert_eq!(app.doc(), &initial_doc);
    assert!(!app.is_dirty());

    app.handle_event(ctrl_press('y'));
    assert_eq!(app.doc(), &after_style);
    app.handle_event(ctrl_press('y'));
    assert_eq!(app.doc(), &after_layout);
    app.handle_event(ctrl_press('y'));
    assert_eq!(app.doc(), &after_all);

    // Bounded 100-step undo history: push 110 priority changes and undo 105 times without panic.
    for i in 0..110_u8 {
        let key_digit = char::from(b'1' + (i % 5));
        app.handle_event(press(KeyCode::Char(key_digit)));
    }
    for _ in 0..105 {
        app.handle_event(ctrl_press('z'));
    }
}

#[test]
fn env_override_badges_and_effective_vs_file_only_toggle_preserve_doc() {
    let sb = Sandbox::new();
    // AGY_* overrides must reach the preview without editing the doc or marking it dirty.
    let mut env_vars = BTreeMap::new();
    env_vars.insert("AGY_THEME".to_string(), "light".to_string());
    env_vars.insert("AGY_STATUSLINE_LINES".to_string(), "1".to_string());
    env_vars.insert("AGY_STATUSLINE_INDENT".to_string(), ">>>>".to_string());

    let mut app = ConfiguratorApp::new(sb.env(env_vars));
    assert!(app.is_effective_preview_mode());
    assert!(!app.is_dirty());

    let (rendered_eff, cfg_eff) = app.preview_rendered_frame();
    assert!(cfg_eff.single_line);
    assert_eq!(cfg_eff.indent, ">>>>");
    assert_eq!(rendered_eff.lines.len(), 1);
    assert!(lines_to_plain_string(&app.preview_lines()).starts_with(">>>>"));

    // Verify badges appear in top bar and Style & Presets tab.
    app.handle_event(press(KeyCode::F(2)));
    let mut term = Terminal::new(TestBackend::new(110, 28)).expect("term");
    render_tui_frame(&mut term, &app).expect("draw style env badges");
    let style_screen = buffer_text(&term);
    assert!(style_screen.contains("[ENV:3 Effective]"));
    assert!(style_screen.contains("AGY_THEME=light"));
    assert!(style_screen.contains("AGY_STATUSLINE_INDENT=>>>>"));

    // Toggle 'e' -> File-Only preview: ignores AGY_* overrides without mutating ConfigDoc or dirty!
    app.handle_event(press(KeyCode::Char('e')));
    assert!(!app.is_effective_preview_mode());
    assert!(!app.is_dirty());
    assert_eq!(app.doc(), &ConfigDoc::default());

    let (rendered_file, cfg_file) = app.preview_rendered_frame();
    assert!(!cfg_file.single_line);
    assert_eq!(cfg_file.indent, "  ");
    assert_eq!(rendered_file.lines.len(), 2);
    assert!(lines_to_plain_string(&app.preview_lines()).starts_with("  "));
}

#[test]
fn broken_toml_banner_protects_file_on_cancel_and_writes_bak_on_confirm() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(&sb.agyline_home).expect("mkdir");
    let cfg_path = sb.agyline_home.join("config.toml");
    let bak_path = sb.agyline_home.join("config.toml.bak");
    let broken_raw = "version = 1\ntheme = [unclosed\n";
    std::fs::write(&cfg_path, broken_raw).expect("write broken config");

    // 1. Launch & quit immediately ('q') -> must NOT overwrite or touch broken file!
    let mut app_cancel = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    assert!(app_cancel.loaded_from_broken_toml());
    let syn_err = app_cancel.syntax_error().expect("syntax error");
    assert_eq!(syn_err.line, Some(2));

    let mut term = Terminal::new(TestBackend::new(110, 28)).expect("term");
    render_tui_frame(&mut term, &app_cancel).expect("draw broken banner");
    let banner_screen = buffer_text(&term);
    assert!(banner_screen.contains("Broken config.toml"));
    assert!(banner_screen.contains("line 2"));

    assert_eq!(
        app_cancel.handle_event(press(KeyCode::Char('q'))),
        Some(ConfiguratorExit::Cancelled)
    );
    assert_eq!(std::fs::read_to_string(&cfg_path).unwrap(), broken_raw);
    assert!(!bak_path.exists());

    // 2. Launch again, press 's' -> opens BrokenTomlConfirm modal; Esc cancels, 'b' backs up .bak & writes valid TOML!
    let mut app_save = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app_save.handle_event(press(KeyCode::Char('s')));
    assert!(app_save.has_active_modal());
    render_tui_frame(&mut term, &app_save).expect("draw broken modal");
    assert!(buffer_text(&term).contains("config.toml.bak"));

    app_save.handle_event(press(KeyCode::Esc));
    assert!(!app_save.has_active_modal());
    assert_eq!(std::fs::read_to_string(&cfg_path).unwrap(), broken_raw);

    app_save.handle_event(press(KeyCode::Char('s')));
    app_save.handle_event(press(KeyCode::Char('b')));
    assert!(!app_save.has_active_modal());
    assert!(!app_save.loaded_from_broken_toml());
    assert_eq!(std::fs::read_to_string(&bak_path).unwrap(), broken_raw);
    let clean_saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(clean_saved.contains("preset = \"default\""));
}

#[test]
fn external_conflict_unmodified_noop_and_dirty_exit_three_way_modal() {
    // 1. Unmodified launch without config.toml: 's' is a no-op (creates zero files), 'q' exits cleanly.
    let sb_empty = Sandbox::new();
    let mut app_empty = ConfiguratorApp::new(sb_empty.env(BTreeMap::new()));
    app_empty.handle_event(press(KeyCode::Char('s')));
    assert!(
        !sb_empty.agyline_home.exists(),
        "unmodified save must not create empty/redundant config.toml"
    );
    assert_eq!(
        app_empty.handle_event(press(KeyCode::Char('q'))),
        Some(ConfiguratorExit::Cancelled)
    );
    assert!(!sb_empty.agyline_home.exists());

    // 2. Dirty launch + 'q' -> 3-way modal: Esc cancels, 'd' discards & exits, 's' saves & exits!
    let sb_dirty = Sandbox::new();
    let mut app_discard = ConfiguratorApp::new(sb_dirty.env(BTreeMap::new()));
    app_discard.handle_event(press(KeyCode::Char('3'))); // change model priority -> dirty!
    assert!(app_discard.is_dirty());
    assert_eq!(app_discard.handle_event(press(KeyCode::Char('q'))), None);
    assert!(app_discard.has_active_modal());
    app_discard.handle_event(press(KeyCode::Esc));
    assert!(!app_discard.has_active_modal());
    assert_eq!(app_discard.handle_event(press(KeyCode::Char('q'))), None);
    assert_eq!(
        app_discard.handle_event(press(KeyCode::Char('d'))),
        Some(ConfiguratorExit::Cancelled)
    );
    assert!(!sb_dirty.agyline_home.join("config.toml").exists());

    let mut app_save_quit = ConfiguratorApp::new(sb_dirty.env(BTreeMap::new()));
    app_save_quit.handle_event(press(KeyCode::Char('4')));
    assert_eq!(app_save_quit.handle_event(press(KeyCode::Char('q'))), None);
    assert_eq!(
        app_save_quit.handle_event(press(KeyCode::Char('s'))),
        Some(ConfiguratorExit::Saved)
    );
    assert!(sb_dirty.agyline_home.join("config.toml").is_file());

    // 3. External disk modification conflict modal ([Esc] cancel, [R] reload, [O] overwrite).
    let cfg_path = sb_dirty.agyline_home.join("config.toml");
    let mut app_conflict = ConfiguratorApp::new(sb_dirty.env(BTreeMap::new()));
    app_conflict.handle_event(press(KeyCode::Char('2'))); // local edit: priority = 2

    // Simulate external editor modifying config.toml on disk.
    let external_toml = "version = 1\npalette = \"nord\"\n";
    std::fs::write(&cfg_path, external_toml).expect("external write");

    app_conflict.handle_event(press(KeyCode::Char('s')));
    assert!(
        app_conflict.has_active_modal(),
        "external edit must trigger conflict modal"
    );
    let mut term = Terminal::new(TestBackend::new(100, 28)).expect("term");
    render_tui_frame(&mut term, &app_conflict).expect("draw conflict modal");
    assert!(buffer_text(&term).contains("modified externally"));

    // Press 'r' to reload external changes from disk!
    app_conflict.handle_event(press(KeyCode::Char('r')));
    assert!(!app_conflict.has_active_modal());
    assert_eq!(app_conflict.doc().palette, "nord");
    assert!(!app_conflict.is_dirty());

    // Edit again and modify disk again -> press 'o' to overwrite disk with in-memory doc!
    app_conflict.handle_event(press(KeyCode::Char('5')));
    std::fs::write(&cfg_path, "version = 1\npalette = \"dracula\"\n").expect("external write 2");
    app_conflict.handle_event(press(KeyCode::Char('s')));
    assert!(app_conflict.has_active_modal());
    app_conflict.handle_event(press(KeyCode::Char('o')));
    assert!(!app_conflict.has_active_modal());
    let disk_after = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(disk_after.contains("palette = \"nord\""));
    assert!(disk_after.contains("priority = 5"));
}

#[test]
fn agy_hook_diff_modal_installs_and_restores_without_saving_config_draft_and_mouse_clicks() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.settings_path.parent().unwrap()).expect("mkdir settings dir");
    let seed_settings = "{\n  \"colorScheme\": \"dark\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"python3 ~/.gemini/antigravity-cli/statusline.py\",\n    \"enabled\": true\n  },\n  \"verbosity\": \"high\"\n}\n";
    std::fs::write(&sb.settings_path, seed_settings).expect("seed settings.json");

    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app.handle_event(Event::Resize(100, 28));

    // Make an unsaved edit to ConfigDoc so app.is_dirty() == true.
    app.handle_event(press(KeyCode::Char('5')));
    assert!(app.is_dirty());

    // Mouse click on Tab 4 ("4: agy Hook" on dynamic tab row `preview_box_height + 1`) switches tab!
    let tab_row = app.preview_box_height() + 1;
    assert_eq!(tab_row, 6);
    app.handle_event(left_click(62, tab_row));
    assert_eq!(app.active_tab(), TuiTab::AgyHook);

    // Press 'i' to preview settings.json install diff modal.
    app.handle_event(press(KeyCode::Char('i')));
    assert!(app.has_active_modal());
    let mut term = Terminal::new(TestBackend::new(100, 28)).expect("term");
    render_tui_frame(&mut term, &app).expect("draw hook diff modal");
    let diff_screen = buffer_text(&term);
    assert!(diff_screen.contains("statusline.py"));
    assert!(diff_screen.contains("agyline"));

    // Confirm install with Enter -> updates settings.json + snapshot, does NOT write config.toml!
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.has_active_modal());
    let installed_json = std::fs::read_to_string(&sb.settings_path).unwrap();
    assert!(installed_json.contains("agyline"));
    assert!(
        sb.agyline_home
            .join("state")
            .join("previous_statusline.json")
            .is_file()
    );
    assert!(
        !sb.agyline_home.join("config.toml").exists(),
        "applying settings.json hook must NEVER implicitly save unsaved config.toml draft"
    );
    assert!(app.is_dirty());

    // Press 'u' on agy Hook tab -> previews uninstall diff, Enter restores exact seeded settings.json!
    app.handle_event(press(KeyCode::Char('u')));
    assert!(app.has_active_modal());
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.has_active_modal());
    assert_eq!(
        std::fs::read_to_string(&sb.settings_path).unwrap(),
        seed_settings
    );
}

#[test]
fn hud_three_row_preview_emits_styles_and_syncs_mouse_clicks_at_dynamic_heights() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(&sb.agyline_home).expect("mkdir");
    std::fs::write(sb.agyline_home.join("config.toml"), "preset = \"hud\"\n")
        .expect("write hud preset");
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app.handle_event(Event::Resize(100, 28));

    // Default 3-row HUD preview height = 6 -> tab bar at row 7; spans carry BOLD/ITALIC and pure-fg line bars.
    assert_eq!(app.preview_box_height(), 6);
    let preview = app.preview_lines();
    assert_eq!(preview.len(), 3);
    assert!(
        preview[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        preview[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::ITALIC))
    );
    assert!(preview[0].spans.iter().all(|s| s.style.bg.is_none()));
    assert!(preview[0].spans.iter().any(|s| s.content.contains('━')));

    app.handle_event(left_click(15, 7)); // Tab 2 (Style & Presets) at row 7
    assert_eq!(app.active_tab(), TuiTab::StylePresets);

    // Switch preset to `cometix` (2 rows rendered -> preview height = 5 -> tab bar at row 6).
    app.handle_event(press(KeyCode::Char('r')));
    for _ in 0..3 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().preset, "cometix");
    assert_eq!(app.preview_box_height(), 5);
    app.handle_event(left_click(36, 6)); // Tab 3 (Runtime) at row 6
    assert_eq!(app.active_tab(), TuiTab::ThresholdsRuntime);
}

#[test]
fn multi_row_editor_navigates_adds_moves_removes_rows_and_edits_new_inspector_fields() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(&sb.agyline_home).expect("mkdir");
    std::fs::write(sb.agyline_home.join("config.toml"), "preset = \"hud\"\n")
        .expect("write hud preset");
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    assert_eq!(app.doc().lines.len(), 3);

    // Test cyclic 'm' slot move across 3 rows (Row 1 -> Row 2 -> Row 3 -> Row 1).
    let moved_id = app.doc().lines[0][0].id;
    app.handle_event(press(KeyCode::Char('m'))); // Row 1 -> Row 2
    assert_eq!(app.doc().lines[1].last().map(|s| s.id), Some(moved_id));
    app.handle_event(press(KeyCode::Char('m'))); // Row 2 -> Row 3
    assert_eq!(app.doc().lines[2].last().map(|s| s.id), Some(moved_id));
    app.handle_event(press(KeyCode::Char('m'))); // Row 3 -> Row 1
    assert_eq!(app.doc().lines[0].last().map(|s| s.id), Some(moved_id));

    // Add Row 4 with 'a', remove empty Row 4 with 'X', and undo/redo restores row counts cleanly.
    app.handle_event(press(KeyCode::Char('a')));
    assert_eq!(app.doc().lines.len(), 4);
    assert!(app.doc().lines[3].is_empty());

    app.handle_event(press(KeyCode::Char('X')));
    assert_eq!(app.doc().lines.len(), 3);
    app.handle_event(ctrl_press('z'));
    assert_eq!(app.doc().lines.len(), 4);
    app.handle_event(ctrl_press('y'));
    assert_eq!(app.doc().lines.len(), 3);

    // Select Row 1 slot 0 first (resetting after Row 4 removal above), then move to `context` (index 3).
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    for _ in 0..3 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    // context fields: 0=priority,1=icon,2=fg,3=icon_color,4=style,5=show_remaining,6=show_bar,7=bar_style,8=bar_width
    for _ in 0..7 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Right)); // bar_style: line -> solid
    assert_eq!(app.doc().lines[0][3].bar_style.as_deref(), Some("solid"));
    app.handle_event(press(KeyCode::Right)); // bar_style: solid -> block
    assert_eq!(app.doc().lines[0][3].bar_style.as_deref(), Some("block"));

    app.handle_event(press(KeyCode::Down)); // bar_width
    app.handle_event(press(KeyCode::Enter));
    type_str(&mut app, "12");
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().lines[0][3].bar_width, Some(12));

    // Return to Active Rows, navigate to Row 2 `quota_group:gemini` (index 6 in flattened slots),
    // and toggle `show_used` (default true on QuotaGroup) -> Some(false) -> remaining % in preview!
    app.handle_event(press(KeyCode::Tab)); // ActiveRows
    for _ in 0..3 {
        app.handle_event(press(KeyCode::Down)); // context(3)->dir(4)->git(5)->quota_group:gemini(6)
    }
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    // quota_group fields: 0..3 base, 4=style, 5=family, 6=label, 7=show_used
    for _ in 0..7 {
        app.handle_event(press(KeyCode::Down));
    }
    app.handle_event(press(KeyCode::Char(' ')));
    assert_eq!(app.doc().lines[1][0].show_used, Some(false));

    let (rendered, cfg) = app.preview_rendered_frame();
    let plain = emit_plain(&rendered, &cfg);
    assert!(
        plain.contains("Gemini 5h ") && plain.contains("68.0% 3h12m"),
        "actual:\n{plain}"
    );
    assert!(plain.contains("███░░░░░░░░░ 24.0%"), "actual:\n{plain}");

    let saved = serialize_config_doc(app.doc());
    assert!(saved.contains("bar_style = \"block\""));
    assert!(saved.contains("bar_width = 12"));
    assert!(saved.contains("show_used = false"));
}

// Style & Presets tab exposes dual-polarity catppuccin/mono, Container Style with degradation
// notes and RGB surface/cap preview parity, top-level bar_style/bar_gradient, and separator
// presets + custom text edit, all surviving a config.toml save/reload round-trip.
#[test]
fn style_tab_v03_palettes_container_style_bars_gradient_separator_and_roundtrip() {
    let sb = Sandbox::new();
    let mut env_vars = BTreeMap::new();
    env_vars.insert("COLORTERM".to_string(), "truecolor".to_string());
    let mut app = ConfiguratorApp::new(sb.env(env_vars.clone()));
    app.handle_event(Event::Resize(120, 30));

    // Tab 2 -> GlobalStyle pane
    app.handle_event(press(KeyCode::F(2)));
    app.handle_event(press(KeyCode::Tab));

    // Default palette is catppuccin (dual-polarity); verify no polarity fallback on Dark or Light.
    assert_eq!(app.doc().palette, "catppuccin");
    app.handle_event(press(KeyCode::Down)); // Theme
    app.handle_event(press(KeyCode::Right)); // Auto -> Dark
    assert_eq!(app.doc().theme, ThemeSetting::Dark);
    let mut term = Terminal::new(TestBackend::new(120, 30)).expect("term");
    render_tui_frame(&mut term, &app).expect("draw catppuccin dark");
    assert!(!buffer_text(&term).contains("polarity fallback"));

    app.handle_event(press(KeyCode::Right)); // Dark -> Light
    assert_eq!(app.doc().theme, ThemeSetting::Light);
    render_tui_frame(&mut term, &app).expect("draw catppuccin light");
    assert!(
        !buffer_text(&term).contains("polarity fallback"),
        "catppuccin is dual-polarity and must not show polarity fallback on light theme"
    );

    // Cycle Palette forward to mono (also dual-polarity on Light and Dark).
    app.handle_event(press(KeyCode::Up)); // Palette
    app.handle_event(press(KeyCode::Right)); // catppuccin -> mono
    assert_eq!(app.doc().palette, "mono");
    render_tui_frame(&mut term, &app).expect("draw mono light");
    assert!(
        !buffer_text(&term).contains("polarity fallback"),
        "mono is dual-polarity and must not show polarity fallback on light theme"
    );
    app.handle_event(press(KeyCode::Left)); // back to catppuccin
    assert_eq!(app.doc().palette, "catppuccin");

    // Navigate to ItemStyle (index 4): minimal -> badge -> pill, verify RGB surface & Powerline caps.
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down));
    }
    assert_eq!(app.doc().item_style, ItemStyle::Minimal);
    app.handle_event(press(KeyCode::Right)); // minimal -> badge
    assert_eq!(app.doc().item_style, ItemStyle::Badge);
    let (badge_rendered, badge_cfg) = app.preview_rendered_frame();
    let badge_lines = app.preview_lines();
    assert_eq!(
        lines_to_plain_string(&badge_lines),
        emit_plain(&badge_rendered, &badge_cfg)
    );
    assert!(
        badge_lines[0]
            .spans
            .iter()
            .any(|sp| matches!(sp.style.bg, Some(Color::Rgb(_, _, _)))),
        "badge mode must emit Color::Rgb surface backgrounds in preview"
    );

    app.handle_event(press(KeyCode::Char(' '))); // badge -> pill
    assert_eq!(app.doc().item_style, ItemStyle::Pill);
    let (pill_rendered, pill_cfg) = app.preview_rendered_frame();
    let pill_lines = app.preview_lines();
    let pill_plain = lines_to_plain_string(&pill_lines);
    assert_eq!(pill_plain, emit_plain(&pill_rendered, &pill_cfg));
    assert!(
        pill_plain.contains('\u{e0b6}') && pill_plain.contains('\u{e0b4}'),
        "pill mode with Nerd + TrueColor must render Powerline endcaps:\n{pill_plain}"
    );
    assert!(
        pill_lines[0]
            .spans
            .iter()
            .any(|sp| matches!(sp.style.bg, Some(Color::Rgb(_, _, _)))),
        "pill mode must emit Color::Rgb surface backgrounds in preview"
    );
    let pill_ansi = emit_ansi(&pill_rendered, &pill_cfg);
    let pill_bg_rgb = pill_lines[0]
        .spans
        .iter()
        .find_map(|sp| match sp.style.bg {
            Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
            _ => None,
        })
        .expect("pill RGB bg");
    assert!(
        pill_ansi.contains(&format!(
            "48;2;{};{};{}",
            pill_bg_rgb.0, pill_bg_rgb.1, pill_bg_rgb.2
        )),
        "preview pill surface RGB {pill_bg_rgb:?} must match emit_ansi background"
    );

    // Downgrade Glyphs -> Unicode: view must surface effective degraded style note for pill -> badge.
    app.handle_event(press(KeyCode::Up)); // ColorDepth (3)
    app.handle_event(press(KeyCode::Up)); // Glyphs (2)
    app.handle_event(press(KeyCode::Right)); // Nerd -> Unicode
    assert_eq!(app.doc().glyphs, GlyphSet::Unicode);
    render_tui_frame(&mut term, &app).expect("draw degraded pill -> badge");
    let screen_deg_glyph = buffer_text(&term);
    assert!(
        screen_deg_glyph.contains("item_style: pill") && screen_deg_glyph.contains("badge"),
        "non-Nerd glyphs must show effective badge degradation note:\n{screen_deg_glyph}"
    );

    // Downgrade ColorDepth -> Ansi16: view must surface effective minimal degradation note.
    app.handle_event(press(KeyCode::Down)); // ColorDepth (3)
    app.handle_event(press(KeyCode::Right)); // Auto -> TrueColor
    app.handle_event(press(KeyCode::Right)); // TrueColor -> Ansi256
    app.handle_event(press(KeyCode::Right)); // Ansi256 -> Ansi16
    assert_eq!(app.doc().color_depth, ColorDepth::Ansi16);
    render_tui_frame(&mut term, &app).expect("draw degraded pill -> minimal");
    let screen_deg_depth = buffer_text(&term);
    assert!(
        screen_deg_depth.contains("item_style: pill") && screen_deg_depth.contains("minimal"),
        "Ansi16 must show effective minimal degradation note:\n{screen_deg_depth}"
    );

    // Restore TrueColor + Nerd + Badge for gradient bar verification.
    app.handle_event(press(KeyCode::Left)); // Ansi256
    app.handle_event(press(KeyCode::Left)); // TrueColor
    app.handle_event(press(KeyCode::Up)); // Glyphs
    app.handle_event(press(KeyCode::Left)); // Nerd
    app.handle_event(press(KeyCode::Down)); // ColorDepth
    app.handle_event(press(KeyCode::Down)); // ItemStyle (4)
    app.handle_event(press(KeyCode::Left)); // pill -> badge

    // Navigate to BarStyle (5): cycle through all 6 BarStyle::ALL variants.
    app.handle_event(press(KeyCode::Down)); // BarStyle (5)
    assert_eq!(app.doc().bar_style, BarStyle::Parallelogram);
    let mut seen_bars = vec![app.doc().bar_style];
    for _ in 0..5 {
        app.handle_event(press(KeyCode::Right));
        seen_bars.push(app.doc().bar_style);
    }
    assert_eq!(seen_bars.len(), 6);
    assert!(seen_bars.contains(&BarStyle::Parallelogram));
    assert!(seen_bars.contains(&BarStyle::Braille));
    assert!(seen_bars.contains(&BarStyle::Line));
    assert!(seen_bars.contains(&BarStyle::Solid));
    assert!(seen_bars.contains(&BarStyle::Block));
    assert!(seen_bars.contains(&BarStyle::Ascii));
    app.handle_event(press(KeyCode::Right)); // wrap back to Parallelogram
    app.handle_event(press(KeyCode::Right)); // Braille
    assert_eq!(app.doc().bar_style, BarStyle::Braille);

    // Navigate to BarGradient (6): check per-cell RGB gradient vs flat toggle.
    app.handle_event(press(KeyCode::Down)); // BarGradient (6)
    assert!(app.doc().bar_gradient);
    // Switch scenario to warn_crit so context bar has multi-cell filled gradient.
    while app.active_scenario().name != "warn_crit" {
        app.handle_event(press(KeyCode::Char('p')));
    }
    let grad_lines = app.preview_lines();
    let grad_rgb_fgs: std::collections::BTreeSet<(u8, u8, u8)> = grad_lines[0]
        .spans
        .iter()
        .filter(|sp| sp.content.contains('⣿') || sp.content.contains('⣤'))
        .filter_map(|sp| match sp.style.fg {
            Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
            _ => None,
        })
        .collect();
    assert!(
        grad_rgb_fgs.len() >= 2,
        "bar_gradient = true must produce multi-stop per-cell Color::Rgb spans in preview: {grad_rgb_fgs:?}"
    );
    let (grad_rendered, grad_cfg) = app.preview_rendered_frame();
    let grad_ansi = emit_ansi(&grad_rendered, &grad_cfg);
    for (r, g, b) in &grad_rgb_fgs {
        assert!(
            grad_ansi.contains(&format!("38;2;{r};{g};{b}")),
            "preview gradient cell RGB ({r},{g},{b}) must match emit_ansi verbatim"
        );
    }

    app.handle_event(press(KeyCode::Char(' '))); // toggle gradient off
    assert!(!app.doc().bar_gradient);
    let flat_lines = app.preview_lines();
    let flat_rgb_fgs: std::collections::BTreeSet<(u8, u8, u8)> = flat_lines[0]
        .spans
        .iter()
        .filter(|sp| sp.content.contains('⣿') || sp.content.contains('⣤'))
        .filter_map(|sp| match sp.style.fg {
            Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
            _ => None,
        })
        .collect();
    assert_eq!(
        flat_rgb_fgs.len(),
        1,
        "bar_gradient = false must render flat single-color filled cells: {flat_rgb_fgs:?}"
    );

    // Navigate to Separator (7): cycle semantic presets with Left/Right, check resolved glyph preview, then Enter custom edit.
    app.handle_event(press(KeyCode::Down)); // Separator (7)
    app.handle_event(press(KeyCode::Right)); // " │ " (pipe) -> "dashed"
    assert_eq!(app.doc().separator, "dashed");
    render_tui_frame(&mut term, &app).expect("draw separator dashed preview");
    let sep_screen = buffer_text(&term);
    assert!(
        sep_screen.contains("dashed") && sep_screen.contains('┊'),
        "separator alias must show resolved glyph preview alongside alias:\n{sep_screen}"
    );
    app.handle_event(press(KeyCode::Right)); // dot
    assert_eq!(app.doc().separator, "dot");
    app.handle_event(press(KeyCode::Right)); // square
    assert_eq!(app.doc().separator, "square");
    app.handle_event(press(KeyCode::Right)); // none
    assert_eq!(app.doc().separator, "none");
    app.handle_event(press(KeyCode::Right)); // pipe
    assert_eq!(app.doc().separator, "pipe");
    app.handle_event(press(KeyCode::Left)); // none
    assert_eq!(app.doc().separator, "none");

    // Enter opens custom separator inline editor.
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text());
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, " :: ");
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().separator, " :: ");

    // Save with 's' and reload into a fresh ConfiguratorApp to verify lossless TOML round-trip.
    app.handle_event(press(KeyCode::Char('s')));
    let reloaded = ConfiguratorApp::new(sb.env(env_vars));
    assert_eq!(reloaded.doc().palette, "catppuccin");
    assert_eq!(reloaded.doc().item_style, ItemStyle::Badge);
    assert_eq!(reloaded.doc().bar_style, BarStyle::Braille);
    assert!(!reloaded.doc().bar_gradient);
    assert_eq!(reloaded.doc().separator, " :: ");
}

// Thresholds & Runtime exposes compact_at, context_high, and quota_high with strict four-tier
// ordering enforcement on both Left/Right stepping and inline text edits, plus reset defaults.
#[test]
fn thresholds_tab_compact_at_and_four_tier_ordering_validation() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app.handle_event(Event::Resize(100, 28));
    app.handle_event(press(KeyCode::F(3))); // Thresholds & Runtime

    // 1. CompactAt (index 1): default 88, ±4 step, inline 0 -> "off (0)", invalid >500 rejected, Del -> 88.
    app.handle_event(press(KeyCode::Down)); // CompactAt
    assert_eq!(app.doc().compact_at, DEFAULT_COMPACT_AT);
    app.handle_event(press(KeyCode::Left));
    assert_eq!(app.doc().compact_at, 84);
    app.handle_event(press(KeyCode::Right));
    app.handle_event(press(KeyCode::Right));
    assert_eq!(app.doc().compact_at, 92);

    // Inline edit invalid 501 must be rejected and leave editor open.
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "501");
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text(), "compact_at > 500 must be rejected");
    assert_eq!(app.doc().compact_at, 92);

    // Replace with 0 -> disables compact collapse and renders "off (0)".
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "0");
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.is_editing_text());
    assert_eq!(app.doc().compact_at, 0);

    let mut term = Terminal::new(TestBackend::new(100, 28)).expect("term");
    render_tui_frame(&mut term, &app).expect("draw compact_at off");
    assert!(
        buffer_text(&term).contains("off (0)"),
        "compact_at = 0 must render as 'off (0)':\n{}",
        buffer_text(&term)
    );

    // Reset with Delete -> restores DEFAULT_COMPACT_AT (88).
    app.handle_event(press(KeyCode::Delete));
    assert_eq!(app.doc().compact_at, DEFAULT_COMPACT_AT);

    // Verify compact_at collapses 2-row default preview to 1 row when width < compact_at.
    while app.active_scenario().name != "showcase_compact" {
        app.handle_event(press(KeyCode::Char('p')));
    }
    assert_eq!(app.preview_lines().len(), 1);
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "0");
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.preview_lines().len(), 2);
    app.handle_event(press(KeyCode::Delete)); // restore 88

    // 2. ContextWarn (3), ContextHigh (4), ContextCrit (5): default 60 / 75 / 90.
    app.handle_event(press(KeyCode::Down)); // Overflow (2)
    app.handle_event(press(KeyCode::Down)); // ContextWarn (3)
    assert_eq!(app.doc().thresholds.context_warn, 60.0);
    assert_eq!(app.doc().thresholds.context_high, 75.0);
    assert_eq!(app.doc().thresholds.context_crit, 90.0);

    // Stepping ContextWarn right must clamp strictly below context_high (74 < 75).
    for _ in 0..30 {
        app.handle_event(press(KeyCode::Right));
    }
    assert_eq!(app.doc().thresholds.context_warn, 74.0);
    app.handle_event(press(KeyCode::Delete)); // reset warn -> 60.0 (active `default` preset)
    assert_eq!(app.doc().thresholds.context_warn, 60.0);

    // Move to ContextHigh (4): step right clamps < 90, step left clamps > 60, invalid inline rejected.
    app.handle_event(press(KeyCode::Down)); // ContextHigh (4)
    for _ in 0..30 {
        app.handle_event(press(KeyCode::Right));
    }
    assert_eq!(app.doc().thresholds.context_high, 89.0);
    for _ in 0..50 {
        app.handle_event(press(KeyCode::Left));
    }
    assert_eq!(app.doc().thresholds.context_high, 61.0);

    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "90"); // equal to context_crit -> invalid!
    app.handle_event(press(KeyCode::Enter));
    assert!(
        app.is_editing_text(),
        "context_high >= context_crit must fail"
    );
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "60"); // equal to context_warn -> invalid!
    app.handle_event(press(KeyCode::Enter));
    assert!(
        app.is_editing_text(),
        "context_high <= context_warn must fail"
    );
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "72");
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.is_editing_text());
    assert_eq!(app.doc().thresholds.context_high, 72.0);
    app.handle_event(press(KeyCode::Delete)); // reset to 75.0 (within 60..90)
    assert_eq!(app.doc().thresholds.context_high, 75.0);

    // When 75.0 falls outside (warn, crit), Delete on ContextHigh resets to the exact midpoint.
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "86");
    app.handle_event(press(KeyCode::Enter));
    app.handle_event(press(KeyCode::Up)); // ContextWarn
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "80");
    app.handle_event(press(KeyCode::Enter));
    app.handle_event(press(KeyCode::Down)); // ContextHigh (now 86 inside 80..90)
    app.handle_event(press(KeyCode::Delete)); // midpoint of (80, 90) = 85.0
    assert_eq!(app.doc().thresholds.context_high, 85.0);
    // Undo restores 86, 60, and 75 cleanly.
    app.handle_event(ctrl_press('z'));
    app.handle_event(ctrl_press('z'));
    app.handle_event(ctrl_press('z'));
    assert_eq!(app.doc().thresholds.context_warn, 60.0);
    assert_eq!(app.doc().thresholds.context_high, 75.0);

    // Move to ContextCrit (5): step left clamps strictly above context_high (76 > 75).
    app.handle_event(press(KeyCode::Down)); // ContextCrit (5)
    for _ in 0..30 {
        app.handle_event(press(KeyCode::Left));
    }
    assert_eq!(app.doc().thresholds.context_crit, 76.0);
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "75"); // equal to context_high -> rejected!
    app.handle_event(press(KeyCode::Enter));
    assert!(
        app.is_editing_text(),
        "context_crit <= context_high must fail"
    );
    app.handle_event(press(KeyCode::Esc));
    app.handle_event(press(KeyCode::Delete)); // reset crit -> 90.0 (active `default` preset)
    assert_eq!(app.doc().thresholds.context_crit, 90.0);

    // 3. QuotaWarn (6), QuotaHigh (7), QuotaCrit (8): default 40 / 25 / 10 (warn > high > crit).
    app.handle_event(press(KeyCode::Down)); // QuotaWarn (6)
    for _ in 0..30 {
        app.handle_event(press(KeyCode::Left));
    }
    assert_eq!(app.doc().thresholds.quota_warn, 26); // strictly > quota_high (25)

    app.handle_event(press(KeyCode::Down)); // QuotaHigh (7)
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "10"); // equal to quota_crit -> invalid!
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text(), "quota_high <= quota_crit must fail");
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "26"); // equal to quota_warn -> invalid!
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text(), "quota_high >= quota_warn must fail");
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "18");
    app.handle_event(press(KeyCode::Enter));
    assert!(!app.is_editing_text());
    assert_eq!(app.doc().thresholds.quota_high, 18);

    // Reset QuotaHigh when 25 is inside (10, 26) -> restores 25; when 25 is outside (crit, warn), resets to midpoint.
    app.handle_event(press(KeyCode::Delete));
    assert_eq!(app.doc().thresholds.quota_high, 25);
    app.handle_event(press(KeyCode::Up)); // QuotaWarn (26 -> 50)
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "50");
    app.handle_event(press(KeyCode::Enter));
    app.handle_event(press(KeyCode::Down)); // QuotaHigh (25 -> 48)
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "48");
    app.handle_event(press(KeyCode::Enter));
    app.handle_event(press(KeyCode::Down)); // QuotaCrit (10 -> 30)
    app.handle_event(press(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Backspace));
    }
    type_str(&mut app, "30");
    app.handle_event(press(KeyCode::Enter));
    app.handle_event(press(KeyCode::Up)); // QuotaHigh (48 inside 30..50)
    app.handle_event(press(KeyCode::Delete)); // midpoint of (30, 50) = 40
    assert_eq!(app.doc().thresholds.quota_high, 40);
    // Undo x4 returns to QuotaWarn=26, QuotaHigh=25, QuotaCrit=10.
    for _ in 0..4 {
        app.handle_event(ctrl_press('z'));
    }
    assert_eq!(app.doc().thresholds.quota_high, 25);

    // Move to QuotaCrit (8): step right clamps strictly below quota_high (24 < 25).
    app.handle_event(press(KeyCode::Down)); // QuotaCrit (8)
    for _ in 0..30 {
        app.handle_event(press(KeyCode::Right));
    }
    assert_eq!(app.doc().thresholds.quota_crit, 24);
    assert!(
        app.doc().thresholds.quota_crit < app.doc().thresholds.quota_high
            && app.doc().thresholds.quota_high < app.doc().thresholds.quota_warn
    );

    // Save & reload round-trip preserves compact_at, context_high, and quota_high.
    app.handle_event(press(KeyCode::Char('s')));
    let reloaded = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    assert_eq!(reloaded.doc().compact_at, DEFAULT_COMPACT_AT);
    assert_eq!(reloaded.doc().thresholds.context_high, 75.0);
    assert_eq!(reloaded.doc().thresholds.quota_high, 25);
    assert_eq!(reloaded.doc().thresholds.quota_crit, 24);
}

// Switching presets across default (2-row), hud (3-row), classic, cometix, minimal, dashboard,
// and cache-focus updates dynamic preview height and copies v0.3 fields according to PresetScope.
#[test]
fn preset_switching_default_and_hud_updates_dynamic_height_and_scopes() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app.handle_event(Event::Resize(110, 28));

    // Starts on 2-row default preset.
    assert_eq!(app.doc().preset, "default");
    assert_eq!(app.preview_box_height(), 5);
    assert_eq!(app.doc().bar_style, BarStyle::Parallelogram);
    assert!(app.doc().bar_gradient);
    assert_eq!(app.doc().compact_at, DEFAULT_COMPACT_AT);

    // Tab 2 Presets pane: apply `hud` (index 1) -> 3 rows, preview height 6, line/flat/compact_at=0.
    app.handle_event(press(KeyCode::F(2)));
    app.handle_event(press(KeyCode::Down)); // default (0) -> hud (1)
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().preset, "hud");
    assert_eq!(app.preview_box_height(), 6);
    assert_eq!(app.doc().palette, "terminal");
    assert_eq!(app.doc().bar_style, BarStyle::Line);
    assert!(!app.doc().bar_gradient);
    assert_eq!(app.doc().compact_at, 0);

    // Preset modal ('r'): apply `default` with Style scope -> keeps 3-row hud layout & compact_at=0,
    // updates palette/bar_style/bar_gradient!
    app.handle_event(press(KeyCode::Char('r')));
    // Current preset index in modal starts at 0 ("default").
    app.handle_event(press(KeyCode::Right)); // All -> Layout
    app.handle_event(press(KeyCode::Right)); // Layout -> Style
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().palette, "catppuccin");
    assert_eq!(app.doc().bar_style, BarStyle::Parallelogram);
    assert!(app.doc().bar_gradient);
    assert_eq!(app.doc().lines.len(), 3);
    assert_eq!(app.doc().compact_at, 0);
    assert_eq!(app.preview_box_height(), 6);

    // Preset modal ('r'): apply `default` with Layout scope -> restores 2 rows and compact_at=88.
    app.handle_event(press(KeyCode::Char('r')));
    app.handle_event(press(KeyCode::Right)); // All -> Layout
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().lines.len(), 2);
    assert_eq!(app.doc().compact_at, DEFAULT_COMPACT_AT);
    assert_eq!(app.preview_box_height(), 5);

    // Undo/redo restores 3-row state and 2-row state cleanly.
    app.handle_event(ctrl_press('z'));
    assert_eq!(app.preview_box_height(), 6);
    app.handle_event(ctrl_press('y'));
    assert_eq!(app.preview_box_height(), 5);

    // Cycle remaining built-in presets via Preset modal: classic, cometix, minimal, dashboard, cache-focus.
    for (idx, expected_name, expected_h) in [
        (2_usize, "classic", 5_u16),
        (3, "cometix", 5),
        (4, "minimal", 5),
        (5, "dashboard", 5),
        (6, "cache-focus", 5),
        (1, "hud", 6),
        (0, "default", 5),
    ] {
        app.handle_event(press(KeyCode::Char('r')));
        for _ in 0..idx {
            app.handle_event(press(KeyCode::Down));
        }
        app.handle_event(press(KeyCode::Enter));
        assert_eq!(app.doc().preset, expected_name);
        assert_eq!(app.preview_box_height(), expected_h);
    }
}

// Layout Inspector exposes all v0.3 slot options (model agybar, git branch, status dot, tokens
// arrows, quota_group inline, context label + show_tokens + 6-style bar_style), updates live
// preview & TOML drawer immediately, and scrolls + hit-tests cleanly at 80x22.
#[test]
fn inspector_v03_slot_options_live_preview_toml_drawer_and_80x22_scroll() {
    let sb = Sandbox::new();
    let mut app = ConfiguratorApp::new(sb.env(BTreeMap::new()));
    app.handle_event(Event::Resize(80, 22));

    // Row 1 of default preset: 0=model(agybar), 1=context(bar, label="", show_tokens=true), 2=tokens(arrows), 3=quota_group(inline)
    // Row 2 of default preset: 0=status(dot), 1=git(branch), 2=dir, ...

    // 1. Model: Style cycles default / agybar / hud.
    app.handle_event(press(KeyCode::Tab)); // ActiveRows -> Catalog
    app.handle_event(press(KeyCode::Tab)); // Catalog -> Inspector
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down)); // Style (index 4)
    }
    assert_eq!(app.doc().lines[0][0].style.as_deref(), Some("agybar"));
    app.handle_event(press(KeyCode::Right)); // agybar -> hud
    assert_eq!(app.doc().lines[0][0].style.as_deref(), Some("hud"));
    app.handle_event(press(KeyCode::Right)); // hud -> default (None)
    assert_eq!(app.doc().lines[0][0].style, None);
    app.handle_event(press(KeyCode::Right)); // default -> agybar
    assert_eq!(app.doc().lines[0][0].style.as_deref(), Some("agybar"));

    // 2. Context (Row 1 #1): BarStyle cycles from global parallelogram -> braille, Label empty hides
    // "ctx " while Delete restores default "ctx", ShowTokens toggles (used/window).
    app.handle_event(press(KeyCode::Tab)); // ActiveRows
    app.handle_event(press(KeyCode::Down)); // context
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    // context fields: 0..3 base, 4=style, 5=show_remaining, 6=show_bar, 7=bar_style, 8=bar_width, 9=label, 10=show_tokens
    for _ in 0..7 {
        app.handle_event(press(KeyCode::Down)); // bar_style (7)
    }
    app.handle_event(press(KeyCode::Right)); // inherits parallelogram -> overrides to braille
    assert_eq!(app.doc().lines[0][1].bar_style.as_deref(), Some("braille"));
    app.handle_event(press(KeyCode::Right)); // braille -> ascii
    app.handle_event(press(KeyCode::Right)); // ascii -> line (must stay Some("line"), NOT None!)
    assert_eq!(app.doc().lines[0][1].bar_style.as_deref(), Some("line"));

    app.handle_event(press(KeyCode::Down)); // bar_width (8)
    app.handle_event(press(KeyCode::Down)); // label (9)
    assert_eq!(app.doc().lines[0][1].label.as_deref(), Some(""));
    app.handle_event(press(KeyCode::Delete)); // restore default ("ctx")
    assert_eq!(app.doc().lines[0][1].label, None);
    assert!(
        lines_to_plain_string(&app.preview_lines()).contains("ctx "),
        "resetting context.label to None must show 'ctx ' in live preview"
    );
    app.handle_event(press(KeyCode::Enter)); // commit empty string -> Some("") hides "ctx " again!
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(app.doc().lines[0][1].label.as_deref(), Some(""));
    assert!(
        !lines_to_plain_string(&app.preview_lines()).contains("ctx "),
        "empty context.label must hide 'ctx ' in live preview"
    );

    app.handle_event(press(KeyCode::Down)); // show_tokens (10) - must scroll into view at 80x22!
    let mut term80 = Terminal::new(TestBackend::new(80, 22)).expect("term80");
    render_tui_frame(&mut term80, &app).expect("draw inspector show_tokens at 80x22");
    assert!(
        buffer_text(&term80).contains("show_tokens"),
        "Inspector must scroll show_tokens into view at 80x22:\n{}",
        buffer_text(&term80)
    );
    assert_eq!(app.doc().lines[0][1].show_tokens, Some(true));
    assert!(lines_to_plain_string(&app.preview_lines()).contains('('));
    app.handle_event(press(KeyCode::Char(' '))); // toggle show_tokens off
    assert_eq!(app.doc().lines[0][1].show_tokens, None);

    // 3. Tokens (Row 1 #2): Style cycles default / arrows (before ShowBreakdown).
    app.handle_event(press(KeyCode::Tab)); // ActiveRows
    app.handle_event(press(KeyCode::Down)); // tokens
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down)); // Style (4)
    }
    assert_eq!(app.doc().lines[0][2].style.as_deref(), Some("arrows"));
    app.handle_event(press(KeyCode::Right)); // arrows -> default
    assert_eq!(app.doc().lines[0][2].style, None);
    app.handle_event(press(KeyCode::Right)); // default -> arrows
    assert_eq!(app.doc().lines[0][2].style.as_deref(), Some("arrows"));
    app.handle_event(press(KeyCode::Down)); // ShowBreakdown (5)
    app.handle_event(press(KeyCode::Char(' '))); // toggle default true -> Some(false)
    assert_eq!(app.doc().lines[0][2].show_breakdown, Some(false));

    // 4. QuotaGroup (Row 1 #3): Style cycles default / inline.
    app.handle_event(press(KeyCode::Tab)); // ActiveRows
    app.handle_event(press(KeyCode::Down)); // quota_group
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down)); // Style (4)
    }
    assert_eq!(app.doc().lines[0][3].style.as_deref(), Some("inline"));
    app.handle_event(press(KeyCode::Right)); // inline -> default
    assert_eq!(app.doc().lines[0][3].style, None);
    app.handle_event(press(KeyCode::Right)); // default -> inline
    assert_eq!(app.doc().lines[0][3].style.as_deref(), Some("inline"));

    // 5. Status (Row 2 #0): Style cycles default / dot (before ShowPending).
    app.handle_event(press(KeyCode::Tab)); // ActiveRows
    app.handle_event(press(KeyCode::Down)); // status
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down)); // Style (4)
    }
    assert_eq!(app.doc().lines[1][0].style.as_deref(), Some("dot"));
    app.handle_event(press(KeyCode::Right)); // dot -> default
    assert_eq!(app.doc().lines[1][0].style, None);
    app.handle_event(press(KeyCode::Right)); // default -> dot
    assert_eq!(app.doc().lines[1][0].style.as_deref(), Some("dot"));
    app.handle_event(press(KeyCode::Down)); // ShowPending (5)
    app.handle_event(press(KeyCode::Char(' ')));
    assert_eq!(app.doc().lines[1][0].show_pending, Some(false));

    // 6. Git (Row 2 #1): Style cycles default / branch / counts.
    app.handle_event(press(KeyCode::Tab)); // ActiveRows
    app.handle_event(press(KeyCode::Down)); // git
    app.handle_event(press(KeyCode::Tab)); // Catalog
    app.handle_event(press(KeyCode::Tab)); // Inspector
    for _ in 0..4 {
        app.handle_event(press(KeyCode::Down)); // Style (4)
    }
    assert_eq!(app.doc().lines[1][1].style.as_deref(), Some("branch"));
    app.handle_event(press(KeyCode::Right)); // branch -> counts
    assert_eq!(app.doc().lines[1][1].style.as_deref(), Some("counts"));
    app.handle_event(press(KeyCode::Right)); // counts -> default
    assert_eq!(app.doc().lines[1][1].style, None);
    app.handle_event(press(KeyCode::Right)); // default -> branch
    assert_eq!(app.doc().lines[1][1].style.as_deref(), Some("branch"));

    // Open TOML drawer ('t') and verify edits reflect immediately in live TOML + preview!
    app.handle_event(press(KeyCode::Char('t')));
    let toml_out = serialize_config_doc(app.doc());
    assert!(toml_out.contains("bar_style = \"line\""));
    assert!(toml_out.contains("style = \"branch\""));
    assert!(toml_out.contains("style = \"dot\""));
    assert!(toml_out.contains("style = \"arrows\""));
    assert!(toml_out.contains("style = \"inline\""));
    let (rendered, cfg) = app.preview_rendered_frame();
    assert_eq!(
        lines_to_plain_string(&app.preview_lines()),
        emit_plain(&rendered, &cfg)
    );

    // Verify 80x22 scrolling & mouse clicks on Style (GlobalStyle, PaletteOverrides) and Thresholds panes.
    app.handle_event(press(KeyCode::Char('t'))); // close drawer
    app.handle_event(press(KeyCode::F(2))); // Style & Presets
    // Click GlobalStyle field 4 (`item_style`) at body_y=7 -> inner_row 4 => row 12, col 30.
    app.handle_event(left_click(30, 12));
    app.handle_event(press(KeyCode::Right));
    assert_eq!(app.doc().item_style, ItemStyle::Badge);

    app.handle_event(press(KeyCode::Tab)); // GlobalStyle -> PaletteOverrides
    for _ in 0..14 {
        app.handle_event(press(KeyCode::Down)); // reach last token surface_hi (index 14)
    }
    render_tui_frame(&mut term80, &app).expect("draw scrolled palette_overrides at 80x22");
    assert!(
        buffer_text(&term80).contains("surface_hi"),
        "PaletteOverrides must scroll surface_hi into view at 80x22:\n{}",
        buffer_text(&term80)
    );
    // Click bottom visible row of PaletteOverrides (row 19, col 60) -> selects surface_hi (14), Enter edits it!
    app.handle_event(left_click(60, 19));
    app.handle_event(press(KeyCode::Enter));
    assert!(app.is_editing_text());
    type_str(&mut app, "#45475a");
    app.handle_event(press(KeyCode::Enter));
    assert_eq!(
        app.doc()
            .palette_overrides
            .get(agyline::ColorToken::SurfaceHi),
        Some("#45475a")
    );

    app.handle_event(press(KeyCode::F(3))); // Thresholds & Runtime
    // Click `compact_at` (inner_row 1 -> row 9, col 5) and step right +4 (88 -> 92).
    app.handle_event(left_click(5, 9));
    app.handle_event(press(KeyCode::Right));
    assert_eq!(app.doc().compact_at, DEFAULT_COMPACT_AT + 4);

    for _ in 0..8 {
        app.handle_event(press(KeyCode::Down)); // CacheWarnSecs (index 9)
    }
    render_tui_frame(&mut term80, &app).expect("draw scrolled thresholds at 80x22");
    assert!(
        buffer_text(&term80).contains("cache_warn_secs"),
        "Thresholds pane must show cache_warn_secs cleanly at 80x22:\n{}",
        buffer_text(&term80)
    );

    // Preset modal at 80x22 keeps the bottom preset ("cache-focus") and footer hint visible.
    app.handle_event(press(KeyCode::Char('r')));
    for _ in 0..6 {
        app.handle_event(press(KeyCode::Down));
    }
    render_tui_frame(&mut term80, &app).expect("draw preset modal at 80x22");
    let modal_txt = buffer_text(&term80);
    assert!(
        modal_txt.contains("cache-focus") && modal_txt.contains("[Enter] Apply"),
        "PresetApply modal at 80x22 must show selected preset and footer:\n{modal_txt}"
    );
    app.handle_event(press(KeyCode::Esc));
}
