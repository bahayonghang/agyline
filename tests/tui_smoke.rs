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
    Ansi16Color, ColorDepth, ConfigDoc, ConfiguratorApp, ConfiguratorEnv, ConfiguratorExit,
    EffectiveConfig, InstallEnv, ItemId, ItemSlot, PlatformKind, SlotOutcome, TuiTab,
    builtin_scenarios, emit_plain, render_lines_with_width, render_tui_frame,
    rendered_frame_to_ratatui_lines, serialize_config_doc,
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
    // GlobalStyle fields: 0=palette, 1=theme, 2=glyphs, 3=color_depth, 4=separator, 5=indent
    for _ in 0..4 {
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
    // Fields in Thresholds pane: 0=single_line, 1=overflow, 2=context_warn (50), 3=context_crit (80), 4=quota_warn (45), 5=quota_crit (20)
    app.handle_event(press(KeyCode::Down));
    app.handle_event(press(KeyCode::Down)); // context_warn = 50, context_crit = 80
    // Try stepping context_warn up 40 times; must clamp strictly below context_crit (79)!
    for _ in 0..40 {
        app.handle_event(press(KeyCode::Right));
    }
    assert!(
        app.doc().thresholds.context_warn < app.doc().thresholds.context_crit,
        "context_warn ({}) must stay < context_crit ({})",
        app.doc().thresholds.context_warn,
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
    // quota_group fields: 0..3 base, 4=family, 5=label, 6=show_used
    for _ in 0..6 {
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
