//! Ratatui drawing for the 4-tab configurator. The preview is the live renderer's own output
//! converted to spans, never re-parsed ANSI, so it shows what `agy` would print. Under
//! MIN_TERM_WIDTH x MIN_TERM_HEIGHT a recoverable guard replaces the workbench. Pane splits,
//! list offsets and modal button order here are mirrored by the hit-testing in `input.rs`.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

use crate::collect::touch::is_valid_loopback_relay_url;
use crate::config::{ItemSlot, serialize_config_doc};
use crate::install::{
    HookStatus, InstallPlan, SnapshotAction, UninstallPlan, resolve_command_string,
};
use crate::items::ItemCategory;
use crate::render::{ColorToken, RenderedFrame, SlotOutcome};
use crate::tui::convert::{
    build_width_ruler, rendered_frame_to_ratatui_lines, resolved_color_to_ratatui,
};
use crate::tui::state::{
    ActiveModal, ConfiguratorApp, HookAction, HookModalPlan, HookModalState, InspectorField,
    LayoutPane, RuntimePane, RuntimeSubField, StyleField, StylePane, ThresholdField, TuiTab,
    inspector_bool_default, inspector_fields_for_item, ordered_catalog_entries, preset_scope_label,
    preview_box_height,
};

/// Smallest terminal the workbench is drawn in. 80 cols keep the tab bar's Min(50) plus its
/// 22-col drawer label readable; `input.rs` applies the same floor to keys and mouse.
pub const MIN_TERM_WIDTH: u16 = 80;
/// Height floor: 8 rows of chrome plus the body's Min(10), with room for the broken-TOML banner.
pub const MIN_TERM_HEIGHT: u16 = 22;

/// Draws one configurator frame, for the live terminal and the TestBackend alike. Under the
/// minimum size it draws the resize guard instead and skips the workbench entirely.
pub fn draw_workbench(frame: &mut Frame<'_>, app: &ConfiguratorApp) {
    let area = frame.area();
    if area.width < MIN_TERM_WIDTH || area.height < MIN_TERM_HEIGHT {
        draw_resize_guard(frame, area, app);
        return;
    }

    let (rendered_preview, effective_cfg) = app.preview_rendered_frame();
    let preview_h = preview_box_height(rendered_preview.lines.len());

    let has_broken_banner = app.loaded_from_broken_toml;
    // Chrome in draw order: top bar, optional banner, preview, tab bar, body, footer. The idx
    // walk below must skip the banner slot exactly when it was not pushed here.
    let mut constraints = Vec::with_capacity(6);
    constraints.push(Constraint::Length(1));
    if has_broken_banner {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(preview_h));
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Min(10));
    constraints.push(Constraint::Length(1));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut idx = 0usize;
    let top_area = chunks[idx];
    idx += 1;
    let banner_area = if has_broken_banner {
        let a = chunks[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let preview_area = chunks[idx];
    idx += 1;
    let tabs_area = chunks[idx];
    idx += 1;
    let body_area = chunks[idx];
    idx += 1;
    let footer_area = chunks[idx];

    draw_top_bar(frame, top_area, app);
    if let Some(b_area) = banner_area {
        draw_broken_banner(frame, b_area, app);
    }
    draw_preview_box(frame, preview_area, app, &rendered_preview, &effective_cfg);
    draw_tab_bar(frame, tabs_area, app);

    // The drawer takes the right 40%; input.rs recomputes the same 60% cut to route clicks.
    let main_body_area = if app.toml_drawer_open {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(body_area);
        draw_toml_drawer(frame, split[1], app);
        split[0]
    } else {
        body_area
    };

    match app.active_tab {
        TuiTab::Layout => draw_layout_tab(frame, main_body_area, app, &rendered_preview),
        TuiTab::StylePresets => draw_style_tab(frame, main_body_area, app, &effective_cfg),
        TuiTab::ThresholdsRuntime => draw_runtime_tab(frame, main_body_area, app),
        TuiTab::AgyHook => draw_hook_tab(frame, main_body_area, app),
    }

    draw_footer_or_text_input(frame, footer_area, app);

    if let Some(ref modal) = app.active_modal {
        draw_active_modal(frame, area, app, modal);
    }
}

/// Shown instead of the workbench under the minimum size, where the fixed chrome would squeeze
/// panes to nothing. Enlarging the window recovers with no state loss; q/Esc still exit.
fn draw_resize_guard(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    let dirty_note = if app.is_dirty() {
        " (* unsaved changes)"
    } else {
        ""
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "Terminal window too small for agyline configurator",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Current: {} x {}  |  Minimum required: {} x {}{}",
            area.width, area.height, MIN_TERM_WIDTH, MIN_TERM_HEIGHT, dirty_note
        )),
        Line::from(Span::styled(
            "Enlarge window to resume, or press [q] / [Esc] to exit.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    // The 3-way exit popup cannot fit here, so its choices are inlined. input.rs still opens
    // DirtyExit on q/Esc, so a small window can never silently drop an unsaved draft.
    if let Some(ActiveModal::DirtyExit { .. }) = app.active_modal {
        lines.push(Line::from(Span::styled(
            "Unsaved changes: [S] Save & Exit  [D] Discard  [Esc] Cancel",
            Style::default()
                .fg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        )));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" agyline resize guard ");
    let para = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(para, area);
}

/// Middle-ellipsized to `max_chars`, keeping twice as much tail as head: the file name and last
/// directories identify the config, the home prefix does not. Under 8 chars it only truncates.
fn shorten_middle_path(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars || max_chars < 8 {
        return chars.into_iter().take(max_chars).collect();
    }
    let tail_keep = (max_chars - 3) * 2 / 3;
    let head_keep = max_chars - 3 - tail_keep;
    let head: String = chars[..head_keep].iter().collect();
    let tail: String = chars[chars.len() - tail_keep..].iter().collect();
    format!("{head}...{tail}")
}

fn draw_top_bar(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if !app.file_existed {
        right_spans.push(Span::styled("[new] ", Style::default().fg(Color::Cyan)));
    }
    if app.is_dirty() {
        right_spans.push(Span::styled(
            "* [unsaved] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !app.undo_stack.is_empty() {
        right_spans.push(Span::styled(
            format!("[Undo:{}] ", app.undo_stack.len()),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if !app.env_overrides.is_empty() {
        let mode_lbl = if app.effective_preview_mode {
            "Effective"
        } else {
            "File-Only"
        };
        right_spans.push(Span::styled(
            format!("[ENV:{} {}] ", app.env_overrides.len(), mode_lbl),
            Style::default().fg(Color::Yellow),
        ));
    }
    let hook_span = match app.hook_inspection.as_ref().map(|h| &h.status) {
        Some(HookStatus::Connected { .. }) => {
            Span::styled("[agy: Connected]", Style::default().fg(Color::Green))
        }
        Some(HookStatus::ForeignStatusLine { .. }) => {
            Span::styled("[agy: External]", Style::default().fg(Color::Yellow))
        }
        Some(HookStatus::MissingSettings) | Some(HookStatus::NotConfigured) | None => Span::styled(
            "[agy: Not configured]",
            Style::default().fg(Color::DarkGray),
        ),
        Some(HookStatus::InvalidSettings { .. }) => {
            Span::styled("[agy: Invalid JSON]", Style::default().fg(Color::Red))
        }
    };
    right_spans.push(hook_span);

    // Badges are capped and the left side keeps Min(16), so they can never push the brand off.
    let right_width: usize = right_spans.iter().map(|s| s.content.chars().count()).sum();
    let right_cols = (right_width as u16).min(area.width.saturating_sub(16));
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(16), Constraint::Length(right_cols)])
        .split(area);

    // " agyline " plus its gap is 10 cols; the spare 1 keeps the text ellipsizing, not wrapping.
    let left_budget = (split[0].width as usize).saturating_sub(11);
    let cfg_path = app.env.home_dir.join("config.toml");
    // A toast borrows this slot so save/undo results are visible without opening a popup.
    let left_text = if let Some(ref msg) = app.status_toast {
        shorten_middle_path(msg, left_budget)
    } else {
        shorten_middle_path(&cfg_path.display().to_string(), left_budget)
    };
    let left_style = if app.status_toast.is_some() {
        Style::default().fg(Color::LightGreen)
    } else {
        Style::default().fg(Color::Gray)
    };

    let left_spans = vec![
        Span::styled(
            " agyline ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(left_text, left_style),
    ];

    frame.render_widget(Paragraph::new(Line::from(left_spans)), split[0]);
    frame.render_widget(
        Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
        split[1],
    );
}

/// Persistent warning that the on-disk TOML did not parse: the draft in memory is the default
/// base, and overwriting the broken file needs the .bak confirmation. Owns a whole layout row.
fn draw_broken_banner(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    let err_txt = app
        .syntax_error
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "syntax error".to_string());
    let line = Line::from(vec![
        Span::styled(
            " ! Broken config.toml: ",
            Style::default()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {err_txt} (press [s] to backup .bak & save)"),
            Style::default().fg(Color::LightRed),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The preview box. `rendered` comes from `render_lines_with_width`, so this is the live line
/// rather than a second implementation; inner row 0 is the width ruler, rows 1.. are the line(s).
fn draw_preview_box(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &ConfiguratorApp,
    rendered: &RenderedFrame,
    effective_cfg: &crate::config::EffectiveConfig,
) {
    let sc = app.active_scenario();
    let width_label = match app.sim_width {
        Some(cols) => format!("{cols}c"),
        None => "∞".to_string(),
    };
    let mode_label = if app.effective_preview_mode {
        "Effective"
    } else {
        "File-Only"
    };
    let title = format!(
        " Live Preview [p]: {} | Width [[/]/0]: {} | [e]: {} ",
        sc.name, width_label, mode_label
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let ruler_str = build_width_ruler(app.sim_width, inner.width as usize);
    let mut lines = Vec::with_capacity(3);
    lines.push(Line::from(Span::styled(
        ruler_str,
        Style::default().fg(Color::DarkGray),
    )));

    let preview_lines = rendered_frame_to_ratatui_lines(rendered, effective_cfg);
    // Distinguishes an empty render (e.g. every slot dropped) from a box that never drew.
    if preview_lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (empty output)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.extend(preview_lines);
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_tab_bar(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    // 22 cols are reserved for the drawer label; input.rs clicks the drawer at width - 22.
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(50), Constraint::Length(22)])
        .split(area);

    // Titles shorten below 100 cols so all four still fit the Min(50) side.
    let titles: Vec<Line<'static>> = TuiTab::ALL
        .iter()
        .map(|t| Line::from(t.title_for_width(area.width)))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.active_tab.index())
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .divider("|");
    frame.render_widget(tabs, split[0]);

    let drawer_txt = if app.toml_drawer_open {
        "[t] TOML: OPEN"
    } else {
        "[t] TOML: closed"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            drawer_txt,
            Style::default().fg(if app.toml_drawer_open {
                Color::Cyan
            } else {
                Color::DarkGray
            }),
        )))
        .alignment(Alignment::Right),
        split[1],
    );
}

/// Narrowing badge for a rendered slot; `None` when it rendered in full or is absent from the
/// frame. Both come from the same render, so a badge cannot disagree with the preview.
fn outcome_badge(outcome: Option<SlotOutcome>) -> Option<(&'static str, Color)> {
    let out = outcome?;
    match out {
        SlotOutcome::Full => None,
        SlotOutcome::Empty => Some(("HIDE", Color::DarkGray)),
        SlotOutcome::Compact => Some(("COMPACT", Color::Yellow)),
        SlotOutcome::Dropped => Some(("DROP", Color::LightYellow)),
        SlotOutcome::Truncated => Some(("TRUNC", Color::LightRed)),
    }
}

/// `[ENV: VAR=value]` when an AGY_* variable overrides this TOML key, which is why the preview
/// can disagree with the draft. `None` when the key is not overridden.
fn env_badge_for_key(app: &ConfiguratorApp, target_key: &str) -> Option<String> {
    app.env_overrides
        .iter()
        .find(|ov| ov.target_key == target_key)
        .map(|ov| format!(" [ENV: {}={}]", ov.var_name, ov.raw_value))
}

fn draw_layout_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &ConfiguratorApp,
    rendered: &RenderedFrame,
) {
    // The 34/33/33 cuts are duplicated in input.rs click routing; change both together.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);

    // Both lists scroll to keep the selection visible; -2 is the pane's top and bottom border.
    // A click maps back to a list index only after adding the same offset.
    let (rows_offset, catalog_offset) =
        app.layout_list_offsets(area.height.saturating_sub(2) as usize);

    // Pane 0: Active Rows
    let rows_focused = match app.layout_pane {
        LayoutPane::ActiveRows => true,
        LayoutPane::Catalog | LayoutPane::Inspector => false,
    };
    let mut row_items: Vec<ListItem<'static>> = Vec::new();
    let row_count = app.doc.lines.len().max(2);
    for r_idx in 0..row_count {
        let inactive_note = if r_idx > 0 && app.doc.single_line {
            " (inactive: single_line)"
        } else {
            ""
        };
        row_items.push(ListItem::new(Line::from(Span::styled(
            format!("-- Line {}{} --", r_idx + 1, inactive_note),
            Style::default()
                .fg(if r_idx > 0 && app.doc.single_line {
                    Color::DarkGray
                } else {
                    Color::Cyan
                })
                .add_modifier(Modifier::BOLD),
        ))));
        let slots = app.doc.lines.get(r_idx).map(Vec::as_slice).unwrap_or(&[]);
        if slots.is_empty() {
            let selected = app.selected_row == r_idx && app.selected_slot == 0;
            let prefix = if selected { "> " } else { "  " };
            row_items.push(ListItem::new(Line::from(Span::styled(
                format!("{prefix}(empty row)"),
                Style::default().fg(if selected {
                    Color::Yellow
                } else {
                    Color::DarkGray
                }),
            ))));
        } else {
            for (s_idx, slot) in slots.iter().enumerate() {
                let selected = app.selected_row == r_idx && app.selected_slot == s_idx;
                let prefix = if selected { "> " } else { "  " };
                let mut spans = vec![
                    Span::raw(prefix),
                    Span::styled(
                        slot.id.as_key().to_string(),
                        if selected {
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD)
                        } else if r_idx > 0 && app.doc.single_line {
                            Style::default().fg(Color::DarkGray)
                        } else {
                            Style::default().fg(Color::Gray)
                        },
                    ),
                ];
                if let Some(ref b) = slot.bucket
                    && !b.is_empty()
                {
                    spans.push(Span::styled(
                        format!(":{b}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                } else if let Some(ref f) = slot.family
                    && !f.is_empty()
                {
                    spans.push(Span::styled(
                        format!(":{f}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                spans.push(Span::styled(
                    format!(" P{}", slot.effective_priority()),
                    Style::default().fg(Color::Cyan),
                ));
                if let Some((badge, col)) = outcome_badge(rendered.slot_outcome(r_idx, s_idx)) {
                    spans.push(Span::styled(
                        format!(" [{badge}]"),
                        Style::default().fg(col).add_modifier(Modifier::BOLD),
                    ));
                }
                row_items.push(ListItem::new(Line::from(spans)));
            }
        }
    }
    let rows_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if rows_focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(" Active Rows (J/K, m row, a/+ add, x/X del) ");
    frame.render_widget(
        List::new(row_items.into_iter().skip(rows_offset).collect::<Vec<_>>()).block(rows_block),
        cols[0],
    );

    // Pane 1: Catalog
    let cat_focused = match app.layout_pane {
        LayoutPane::Catalog => true,
        LayoutPane::ActiveRows | LayoutPane::Inspector => false,
    };
    let ordered = ordered_catalog_entries();
    let mut cat_items: Vec<ListItem<'static>> = Vec::with_capacity(24);
    let mut last_cat: Option<ItemCategory> = None;
    for (c_idx, entry) in ordered.iter().enumerate() {
        if last_cat != Some(entry.category) {
            last_cat = Some(entry.category);
            let cat_label = match entry.category {
                ItemCategory::Workspace => "Workspace",
                ItemCategory::Usage => "Usage",
                ItemCategory::Runtime => "Runtime",
                ItemCategory::Session => "Identity / Session",
            };
            cat_items.push(ListItem::new(Line::from(Span::styled(
                format!("-- {cat_label} --"),
                Style::default().fg(Color::Cyan),
            ))));
        }
        // [x] is membership in the selected row only, not in either row.
        let in_row = app
            .doc
            .lines
            .get(app.selected_row)
            .is_some_and(|r| r.iter().any(|s| s.id == entry.id));
        let mark = if in_row { "[x]" } else { "[ ]" };
        let selected = cat_focused && app.catalog_idx == c_idx;
        let prefix = if selected { "> " } else { "  " };
        // Repeatable items (quota) show +multi: each instance is a separate slot, not a priority.
        let tag = if entry.allow_multiple {
            "+multi".to_string()
        } else {
            format!("P{}", entry.default_priority)
        };
        cat_items.push(ListItem::new(Line::from(Span::styled(
            format!("{prefix}{mark} {} ({tag})", entry.key),
            if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ))));
    }
    let cat_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if cat_focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(format!(" Catalog -> Line {} ", app.selected_row + 1));
    frame.render_widget(
        List::new(
            cat_items
                .into_iter()
                .skip(catalog_offset)
                .collect::<Vec<_>>(),
        )
        .block(cat_block),
        cols[1],
    );

    // Pane 2: Inspector
    let insp_focused = match app.layout_pane {
        LayoutPane::Inspector => true,
        LayoutPane::ActiveRows | LayoutPane::Catalog => false,
    };
    let insp_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if insp_focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(" Inspector (Enter edit, Del default) ");
    let mut insp_lines: Vec<Line<'static>> = Vec::new();
    if let Some(slot) = app.selected_item_slot() {
        insp_lines.push(Line::from(Span::styled(
            format!(
                "{} (Line {} #{})",
                slot.id.as_key(),
                app.selected_row + 1,
                app.selected_slot + 1
            ),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        // The field list depends on the item id, so inspector_idx is re-clamped on every move.
        let fields = inspector_fields_for_item(slot.id);
        for (f_idx, &field) in fields.iter().enumerate() {
            let selected = insp_focused && app.inspector_idx == f_idx;
            let prefix = if selected { "> " } else { "  " };
            let val_str = format_inspector_value(slot, field);
            let mut extra_env = String::new();
            if slot.id == crate::config::ItemId::Mode
                && field == InspectorField::ShowDefault
                && let Some(b) = env_badge_for_key(app, "mode.show_default")
            {
                extra_env = b;
            }
            insp_lines.push(Line::from(Span::styled(
                format!("{prefix}{}: {val_str}{extra_env}", field.key_label()),
                if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            )));
        }
    } else {
        insp_lines.push(Line::from(Span::styled(
            "Empty row selected. Use Catalog (Tab) to add items.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Paragraph::new(insp_lines).block(insp_block), cols[2]);
}

/// Inspector text for one field. Unset values print as a default so the draft keeps only real
/// overrides; the icon is tri-state and an explicit empty string means "hidden".
fn format_inspector_value(slot: &ItemSlot, field: InspectorField) -> String {
    let fmt_bool = |opt: Option<bool>| -> String {
        let def = inspector_bool_default(slot.id, field);
        match opt {
            Some(v) => format!("{v}"),
            None if def => "true (default)".to_string(),
            None => "false".to_string(),
        }
    };
    match field {
        InspectorField::Priority => match slot.priority {
            Some(p) => format!("P{p}"),
            None => format!("P{} (default)", slot.id.default_priority()),
        },
        // Some("") hides the icon; None inherits the item's own glyph.
        InspectorField::Icon => match &slot.icon {
            Some(ic) if ic.is_empty() => "\"\" (hidden)".to_string(),
            Some(ic) => format!("{ic:?}"),
            None => "(default)".to_string(),
        },
        InspectorField::Fg => slot.fg.clone().unwrap_or_else(|| "(default)".to_string()),
        InspectorField::IconColor => slot
            .icon_color
            .clone()
            .unwrap_or_else(|| "(default)".to_string()),
        InspectorField::Style => slot
            .style
            .clone()
            .unwrap_or_else(|| "default (default)".to_string()),
        InspectorField::MaxLevels => match slot.max_levels {
            Some(n) => format!("{n}"),
            None => "leaf (default)".to_string(),
        },
        InspectorField::Bucket => slot
            .bucket
            .clone()
            .unwrap_or_else(|| "(auto min)".to_string()),
        InspectorField::Family => slot
            .family
            .clone()
            .unwrap_or_else(|| "gemini (default)".to_string()),
        InspectorField::Label => slot
            .label
            .clone()
            .unwrap_or_else(|| "(default)".to_string()),
        InspectorField::BarStyle => slot
            .bar_style
            .clone()
            .unwrap_or_else(|| "solid (default)".to_string()),
        InspectorField::BarWidth => match slot.bar_width {
            Some(w) => format!("{w}"),
            None => "10 (default)".to_string(),
        },
        InspectorField::ShowRemaining => fmt_bool(slot.show_remaining),
        InspectorField::ShowUsed => fmt_bool(slot.show_used),
        InspectorField::ShowBar => fmt_bool(slot.show_bar),
        InspectorField::ShowReset => fmt_bool(slot.show_reset),
        InspectorField::ShowAmount => fmt_bool(slot.show_amount),
        InspectorField::ShowSubagent => fmt_bool(slot.show_subagent),
        InspectorField::ShowDefault => fmt_bool(slot.show_default),
        InspectorField::Compact => fmt_bool(slot.compact),
        InspectorField::ShowDetail => fmt_bool(slot.show_detail),
        InspectorField::ShowWhenOff => fmt_bool(slot.show_when_off),
        InspectorField::ShowTtl => fmt_bool(slot.show_ttl),
        InspectorField::ShowBreakdown => fmt_bool(slot.show_breakdown),
        InspectorField::ShowPending => fmt_bool(slot.show_pending),
    }
}

fn draw_style_tab(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &ConfiguratorApp,
    effective_cfg: &crate::config::EffectiveConfig,
) {
    // The 26/40/34 cuts are duplicated in input.rs click routing.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(26),
            Constraint::Percentage(40),
            Constraint::Percentage(34),
        ])
        .split(area);

    // Pane 0: Presets
    let presets_focused = match app.style_pane {
        StylePane::Presets => true,
        StylePane::GlobalStyle | StylePane::PaletteOverrides => false,
    };
    let mut preset_lines: Vec<Line<'static>> = Vec::new();
    preset_lines.push(Line::from(Span::styled(
        format!(
            "Scope [h/l]: {}",
            preset_scope_label(app.style_preset_scope)
        ),
        Style::default().fg(Color::Cyan),
    )));
    for (idx, name) in app.available_presets.iter().enumerate() {
        let selected = presets_focused && app.style_preset_idx == idx;
        let active = app.doc.preset == *name;
        let prefix = if selected { "> " } else { "  " };
        let active_tag = if active { " (active)" } else { "" };
        preset_lines.push(Line::from(Span::styled(
            format!("{prefix}{name}{active_tag}"),
            if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        )));
    }
    frame.render_widget(
        Paragraph::new(preset_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if presets_focused {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(" Presets (Enter apply) "),
        ),
        cols[0],
    );

    // Pane 1: Global Style
    let style_focused = match app.style_pane {
        StylePane::GlobalStyle => true,
        StylePane::Presets | StylePane::PaletteOverrides => false,
    };
    let mut style_lines: Vec<Line<'static>> = Vec::with_capacity(10);
    for (idx, field) in StyleField::ALL.iter().enumerate() {
        let selected = style_focused && app.style_field_idx == idx;
        let prefix = if selected { "> " } else { "  " };
        let (row_txt, env_note) = match field {
            StyleField::Palette => {
                // A fixed-polarity RGB palette on the opposite theme falls back to terminal.
                let fb = if effective_cfg.resolved_palette.polarity_fell_back {
                    " (polarity fallback)"
                } else {
                    ""
                };
                (format!("{prefix}palette: {}{fb}", app.doc.palette), None)
            }
            StyleField::Theme => (
                format!("{prefix}theme: {}", app.doc.theme.as_key()),
                env_badge_for_key(app, "theme"),
            ),
            StyleField::Glyphs => (format!("{prefix}glyphs: {}", app.doc.glyphs.as_key()), None),
            StyleField::ColorDepth => (
                format!("{prefix}color_depth: {}", app.doc.color_depth.as_key()),
                None,
            ),
            StyleField::Separator => (format!("{prefix}separator: {:?}", app.doc.separator), None),
            StyleField::Indent => (
                format!("{prefix}indent: {:?}", app.doc.indent),
                env_badge_for_key(app, "indent"),
            ),
        };
        style_lines.push(Line::from(Span::styled(
            row_txt,
            if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        )));
        if let Some(badge) = env_note {
            style_lines.push(Line::from(Span::styled(
                format!("   {badge}"),
                Style::default().fg(Color::Yellow),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(style_lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(if style_focused {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    }))
                    .title(" Global Style "),
            ),
        cols[1],
    );

    // Pane 2: [palette_overrides]
    let ov_focused = match app.style_pane {
        StylePane::PaletteOverrides => true,
        StylePane::Presets | StylePane::GlobalStyle => false,
    };
    let mut ov_lines: Vec<Line<'static>> = Vec::with_capacity(ColorToken::ALL.len());
    for (idx, tok) in ColorToken::ALL.iter().copied().enumerate() {
        let selected = ov_focused && app.palette_override_idx == idx;
        let prefix = if selected { "> " } else { "  " };
        // Swatch is the resolved color (palette + override + env); text is the raw draft.
        let swatch_col = resolved_color_to_ratatui(effective_cfg.resolve_span_color(tok))
            .unwrap_or(Color::Reset);
        let val = app.doc.palette_overrides.get(tok).unwrap_or("(default)");
        ov_lines.push(Line::from(vec![
            Span::raw(prefix),
            Span::styled("██ ", Style::default().fg(swatch_col)),
            Span::styled(
                format!("{}: {val}", tok.as_key()),
                if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(ov_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if ov_focused {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(" [palette_overrides] "),
        ),
        cols[2],
    );
}

fn draw_runtime_tab(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    // The 52/48 cut is duplicated in input.rs click routing.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(area);

    let th_focused = match app.runtime_pane {
        RuntimePane::Thresholds => true,
        RuntimePane::Runtime => false,
    };
    let mut th_lines: Vec<Line<'static>> = Vec::with_capacity(8);
    for (idx, field) in ThresholdField::ALL.iter().enumerate() {
        let selected = th_focused && app.threshold_field_idx == idx;
        let prefix = if selected { "> " } else { "  " };
        let txt = match field {
            ThresholdField::SingleLine => {
                let env_b = env_badge_for_key(app, "single_line").unwrap_or_default();
                format!("{prefix}single_line: {}{env_b}", app.doc.single_line)
            }
            ThresholdField::Overflow => {
                format!("{prefix}overflow: {}", app.doc.overflow.as_key())
            }
            ThresholdField::ContextWarn => {
                format!("{prefix}context_warn: {}%", app.doc.thresholds.context_warn)
            }
            ThresholdField::ContextCrit => {
                format!("{prefix}context_crit: {}%", app.doc.thresholds.context_crit)
            }
            ThresholdField::QuotaWarn => {
                format!("{prefix}quota_warn: {}%", app.doc.thresholds.quota_warn)
            }
            ThresholdField::QuotaCrit => {
                format!("{prefix}quota_crit: {}%", app.doc.thresholds.quota_crit)
            }
            ThresholdField::CacheWarnSecs => format!(
                "{prefix}cache_warn_secs: {}s",
                app.doc.thresholds.cache_warn_secs
            ),
        };
        th_lines.push(Line::from(Span::styled(
            txt,
            if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        )));
    }
    frame.render_widget(
        Paragraph::new(th_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if th_focused {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(" Layout & [thresholds] "),
        ),
        cols[0],
    );

    let rt_focused = match app.runtime_pane {
        RuntimePane::Runtime => true,
        RuntimePane::Thresholds => false,
    };
    let mut rt_lines: Vec<Line<'static>> = Vec::with_capacity(8);
    for (idx, field) in RuntimeSubField::ALL.iter().enumerate() {
        let selected = rt_focused && app.runtime_field_idx == idx;
        let prefix = if selected { "> " } else { "  " };
        let txt = match field {
            RuntimeSubField::GitCacheMs => {
                format!("{prefix}[git].cache_ms: {} ms", app.doc.git_cache_ms)
            }
            RuntimeSubField::CacheRelayUrl => {
                let env_b = env_badge_for_key(app, "cache_relay.url").unwrap_or_default();
                format!(
                    "{prefix}[cache_relay].url: {:?}{env_b}",
                    app.doc.cache_relay_url
                )
            }
        };
        rt_lines.push(Line::from(Span::styled(
            txt,
            if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        )));
    }

    // The URL is stored as typed; this is the same loopback gate the __touch worker parses with,
    // so a warning here means no touch will ever be sent.
    let raw_url = app.doc.cache_relay_url.trim();
    if raw_url.is_empty() || raw_url.eq_ignore_ascii_case("off") {
        rt_lines.push(Line::from(Span::styled(
            "  Touch: disabled (passive relay state)",
            Style::default().fg(Color::DarkGray),
        )));
    } else if is_valid_loopback_relay_url(raw_url) {
        rt_lines.push(Line::from(Span::styled(
            "  Touch: valid loopback http:// URL",
            Style::default().fg(Color::Green),
        )));
    } else {
        rt_lines.push(Line::from(Span::styled(
            "  ! Require loopback http:// URL",
            Style::default().fg(Color::Yellow),
        )));
        rt_lines.push(Line::from(Span::styled(
            "    (127.0.0.1, [::1], or localhost)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(
        Paragraph::new(rt_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if rt_focused {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(" [git] & [cache_relay] "),
        ),
        cols[1],
    );
}

fn draw_hook_tab(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(16);
    match &app.hook_inspection {
        Some(insp) => {
            lines.push(Line::from(format!(
                "settings.json: {}",
                insp.settings_path.display()
            )));
            // Only for a symlinked settings.json, and it shifts every row below it by one.
            if insp.resolved_settings_path != insp.settings_path {
                lines.push(Line::from(Span::styled(
                    format!("Symlink target: {}", insp.resolved_settings_path.display()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            let (status_desc, status_col) = match &insp.status {
                HookStatus::Connected { command, enabled } => (
                    format!("Connected (enabled={enabled}, command={command})"),
                    Color::Green,
                ),
                HookStatus::ForeignStatusLine { command } => (
                    format!(
                        "External ({})",
                        command.as_deref().unwrap_or("<non-command>")
                    ),
                    Color::Yellow,
                ),
                HookStatus::NotConfigured => ("Not configured".to_string(), Color::Gray),
                HookStatus::MissingSettings => {
                    ("settings.json not created yet".to_string(), Color::DarkGray)
                }
                HookStatus::InvalidSettings { reason } => {
                    (format!("Invalid JSON ({reason})"), Color::Red)
                }
            };
            lines.push(Line::from(vec![
                Span::raw("Status: "),
                Span::styled(status_desc, Style::default().fg(status_col)),
            ]));

            // The exact command string install would write, or the reason it cannot be built.
            if let Ok(ref ienv) = app.env.install_env {
                match resolve_command_string(ienv) {
                    Ok(cmd) => lines.push(Line::from(Span::styled(
                        format!("Resolved command: {cmd}"),
                        Style::default().fg(Color::Cyan),
                    ))),
                    Err(err) => lines.push(Line::from(Span::styled(
                        format!("Command diagnostic: {err}"),
                        Style::default().fg(Color::Yellow),
                    ))),
                }
            }

            // The snapshot is what uninstall restores; --reset discards it instead.
            let snap_desc = if insp.has_previous_snapshot {
                format!(
                    "Saved prior statusLine snapshot: {}",
                    insp.snapshot_path.display()
                )
            } else {
                "Saved prior statusLine snapshot: none".to_string()
            };
            lines.push(Line::from(Span::styled(
                snap_desc,
                Style::default().fg(Color::DarkGray),
            )));
        }
        None => {
            lines.push(Line::from("settings.json: unavailable"));
        }
    }

    lines.push(Line::from(""));
    let mut btn_spans: Vec<Span<'static>> = Vec::new();
    for action in HookAction::ALL {
        let selected = app.hook_action == action;
        let label = match action {
            HookAction::Connect => " [i] Connect agyline ",
            HookAction::Uninstall => " [u] Restore / Uninstall ",
            HookAction::Reset => " [x] Reset (--reset) ",
        };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        };
        btn_spans.push(Span::styled(label, style));
        btn_spans.push(Span::raw("  "));
    }
    lines.push(Line::from(btn_spans));

    lines.push(Line::from(""));
    // Mirrors the loader's precedence and the AGY_* names both READMEs document.
    lines.push(Line::from(Span::styled(
        "Precedence: AGY_* env > config.toml > preset > default",
        Style::default().fg(Color::Cyan),
    )));
    for (var_name, target_desc) in [
        ("AGY_THEME", "theme (auto|dark|light)"),
        (
            "AGY_STATUSLINE_LINES",
            "single_line (1 => true, 2 => false)",
        ),
        ("AGY_STATUSLINE_INDENT", "indent prefix"),
        ("AGY_SHOW_DEFAULT_MODE", "mode.show_default"),
        ("AGY_CACHE_RELAY", "[cache_relay].url ('off' disables)"),
    ] {
        let cur = app
            .env
            .env_vars
            .get(var_name)
            .map(|v| format!("={v:?}"))
            .unwrap_or_else(|| " (unset)".to_string());
        lines.push(Line::from(Span::styled(
            format!("  {var_name}{cur} -> {target_desc}"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" agy Hook (settings.json) "),
        ),
        area,
    );
}

/// Popup rect at the given percentages, floored at 48x9 so text stays readable and capped 2
/// cols/rows inside `area`; on a tiny terminal the cap wins and the popup still fits.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let w = ((area.width.saturating_mul(percent_x)) / 100)
        .max(48)
        .min(area.width.saturating_sub(2));
    let h = ((area.height.saturating_mul(percent_y)) / 100)
        .max(9)
        .min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn button_span(label: &'static str, selected: bool, accent: Color) -> Span<'static> {
    if selected {
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!(" {label} "), Style::default().fg(accent))
    }
}

/// Draws the top-most modal over the workbench, clearing its rect first so panes cannot show
/// through. Each arm's button order is the order input.rs maps `selected` and Enter onto.
fn draw_active_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &ConfiguratorApp,
    modal: &ActiveModal,
) {
    match modal {
        ActiveModal::DirtyExit { selected } => {
            let popup = centered_rect(64, 42, area);
            frame.render_widget(Clear, popup);
            let lines = vec![
                Line::from(Span::styled(
                    "You have unsaved changes to config.toml.",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!("Target: {}", app.config_path().display())),
                Line::from(""),
                // Indices: 0 Save & Exit, 1 Discard & Exit, 2 Cancel.
                Line::from(vec![
                    button_span("[S] Save & Exit", *selected == 0, Color::Green),
                    Span::raw("  "),
                    button_span("[D] Discard & Exit", *selected == 1, Color::LightRed),
                    Span::raw("  "),
                    button_span("[Esc] Cancel", *selected == 2, Color::Gray),
                ]),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Unsaved Changes ");
            frame.render_widget(
                Paragraph::new(lines)
                    .block(block)
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        ActiveModal::BrokenTomlConfirm {
            exit_after_save: _,
            selected,
        } => {
            let popup = centered_rect(72, 48, area);
            frame.render_widget(Clear, popup);
            let err_summary = app
                .syntax_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "syntax error".to_string());
            let bak_path = app.env.home_dir.join("config.toml.bak");
            let lines = vec![
                Line::from(Span::styled(
                    "Existing config.toml on disk has a TOML syntax error:",
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!("  {err_summary}")),
                Line::from(format!("Backup path: {}", bak_path.display())),
                Line::from(""),
                Line::from(vec![
                    button_span("[B] Backup .bak & Save", *selected == 0, Color::Green),
                    Span::raw("  "),
                    button_span("[O] Overwrite", *selected == 1, Color::Yellow),
                    Span::raw("  "),
                    button_span("[Esc] Cancel", *selected == 2, Color::Gray),
                ]),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightRed))
                .title(" Recover Broken config.toml ");
            frame.render_widget(
                Paragraph::new(lines)
                    .block(block)
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        ActiveModal::ExternalConflict {
            exit_after_save: _,
            selected,
        } => {
            let popup = centered_rect(72, 46, area);
            frame.render_widget(Clear, popup);
            let lines = vec![
                Line::from(Span::styled(
                    "config.toml was modified externally since this session started.",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from("Choose how to resolve the disk conflict:"),
                Line::from(""),
                // Unlike DirtyExit, index 0 is Cancel here; the Enter arms follow this order.
                Line::from(vec![
                    button_span("[Esc] Cancel", *selected == 0, Color::Gray),
                    Span::raw("  "),
                    button_span("[R] Reload from disk", *selected == 1, Color::Cyan),
                    Span::raw("  "),
                    button_span("[O] Overwrite disk", *selected == 2, Color::Yellow),
                ]),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Disk Conflict Detected ");
            frame.render_widget(
                Paragraph::new(lines)
                    .block(block)
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        ActiveModal::PresetApply { preset_idx, scope } => {
            // Highlight and scope live in the modal; nothing reaches the doc until Enter.
            let popup = centered_rect(64, 58, area);
            frame.render_widget(Clear, popup);
            let mut lines = Vec::with_capacity(12);
            lines.push(Line::from(Span::styled(
                format!("Scope [Left/Right]: {}", preset_scope_label(*scope)),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(
                "Select Preset [Up/Down], press [Enter] to apply:",
            ));
            lines.push(Line::from(""));
            for (idx, name) in app.available_presets.iter().enumerate() {
                let is_sel = idx == *preset_idx;
                let prefix = if is_sel { "> " } else { "  " };
                lines.push(Line::from(Span::styled(
                    format!("{prefix}{name}"),
                    if is_sel {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "[Enter] Apply (Ctrl+Z to undo)   [Esc] Cancel",
                Style::default().fg(Color::DarkGray),
            )));
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Apply Preset ");
            frame.render_widget(Paragraph::new(lines).block(block), popup);
        }
        ActiveModal::HookDiff(hook_modal) => {
            draw_hook_diff_modal(frame, area, hook_modal);
        }
        ActiveModal::HelpOverlay => {
            let popup = centered_rect(76, 72, area);
            frame.render_widget(Clear, popup);
            let lines = vec![
                Line::from(Span::styled(
                    "4-Tier Keyboard & Mouse Reference",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from("  { / } or F1..F4 : Switch top-level tabs"),
                Line::from("  Tab / Shift+Tab : Cycle panes inside active tab"),
                Line::from("  p / P           : Cycle preview scenario"),
                Line::from("  [ / ] / 0       : Adjust simulated width (±4 cols) / toggle ∞"),
                Line::from(
                    "  e               : Toggle Effective (with AGY_*) vs File-Only preview",
                ),
                Line::from("  t               : Toggle live read-only config.toml drawer"),
                Line::from("  r / Ctrl+R      : Apply Preset (All / Layout / Style)"),
                Line::from("  i / u           : Preview settings.json install / uninstall diff"),
                Line::from("  Ctrl+Z / Ctrl+Y : Undo / Redo (up to 100 steps)"),
                Line::from("  s / Ctrl+S      : Save config.toml atomically"),
                Line::from("  q / Esc         : Exit configurator"),
                Line::from(""),
                Line::from(Span::styled(
                    "Press [Esc], [Enter], or [?] to close.",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Help ");
            frame.render_widget(Paragraph::new(lines).block(block), popup);
        }
    }
}

/// One line describing the plan's snapshot side effect; `None` when it touches no snapshot.
fn format_snapshot_note(action: &SnapshotAction) -> Option<String> {
    match action {
        SnapshotAction::None => None,
        SnapshotAction::SaveNew { path, .. } => {
            Some(format!("Snapshot prior statusLine -> {}", path.display()))
        }
        SnapshotAction::PreserveExisting { path } => {
            Some(format!("Preserve existing snapshot -> {}", path.display()))
        }
        SnapshotAction::RestoreFrom { path } => {
            Some(format!("Restore prior statusLine <- {}", path.display()))
        }
        SnapshotAction::DiscardByReset { path } => Some(format!(
            "Discard saved snapshot (--reset) -> {}",
            path.display()
        )),
    }
}

/// Colors a unified diff. The +++/---/@@ headers must be tested before the single +/- prefixes,
/// or file headers would be painted as added and removed lines.
fn push_colored_diff_lines(lines: &mut Vec<Line<'static>>, diff: &str) {
    for raw_line in diff.lines() {
        let style = if raw_line.starts_with("+++")
            || raw_line.starts_with("---")
            || raw_line.starts_with("@@")
        {
            Style::default().fg(Color::Cyan)
        } else if raw_line.starts_with('+') {
            Style::default().fg(Color::Green)
        } else if raw_line.starts_with('-') {
            Style::default().fg(Color::LightRed)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(raw_line.to_string(), style)));
    }
}

/// Standalone settings.json transaction: the plan was computed when the modal opened, so the diff
/// shown is exactly what Enter applies. The unsaved config.toml draft is not part of it.
fn draw_hook_diff_modal(frame: &mut Frame<'_>, area: Rect, hook_modal: &HookModalState) {
    let popup = centered_rect(84, 78, area);
    frame.render_widget(Clear, popup);

    let title = match hook_modal.action {
        HookAction::Connect => " Preview settings.json Connect (agyline install) ",
        HookAction::Uninstall => " Preview settings.json Restore / Uninstall ",
        HookAction::Reset => " Preview settings.json Reset (--reset) ",
    };

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(24);
    lines.push(Line::from(Span::styled(
        "Standalone transaction: modifies settings.json only (unsaved config.toml draft stays untouched).",
        Style::default().fg(Color::DarkGray),
    )));

    match &hook_modal.plan {
        HookModalPlan::Install(plan) => match plan {
            InstallPlan::AlreadyInstalled {
                settings_path,
                resolved_settings_path: _,
                command,
            } => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "agyline is already configured in {} ({command}).",
                        settings_path.display()
                    ),
                    Style::default().fg(Color::Green),
                )));
                lines.push(Line::from("[Enter / Esc] Close"));
            }
            InstallPlan::WouldChange {
                command: _,
                mutation,
            } => {
                lines.push(Line::from(format!(
                    "Target: {}",
                    mutation.settings_path.display()
                )));
                if let Some(note) = format_snapshot_note(&mutation.snapshot_action) {
                    lines.push(Line::from(Span::styled(
                        note,
                        Style::default().fg(Color::Yellow),
                    )));
                }
                push_colored_diff_lines(&mut lines, &mutation.diff);
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "[Enter / y] Apply to settings.json   [Esc / n] Cancel   [Up/Down] Scroll",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
            }
        },
        HookModalPlan::Uninstall(plan) => match plan {
            UninstallPlan::NotInstalled { settings_path, .. } => {
                lines.push(Line::from(format!(
                    "agyline is not currently configured in {}.",
                    settings_path.display()
                )));
                lines.push(Line::from("[Enter / Esc] Close"));
            }
            UninstallPlan::ForeignStatusLine {
                settings_path,
                current_command,
                ..
            } => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "External statusLine in {} ({}); leaving untouched.",
                        settings_path.display(),
                        current_command.as_deref().unwrap_or("<non-command>")
                    ),
                    Style::default().fg(Color::Yellow),
                )));
                lines.push(Line::from("[Enter / Esc] Close"));
            }
            UninstallPlan::WouldChange { mutation, .. } => {
                lines.push(Line::from(format!(
                    "Target: {}",
                    mutation.settings_path.display()
                )));
                if let Some(note) = format_snapshot_note(&mutation.snapshot_action) {
                    lines.push(Line::from(Span::styled(
                        note,
                        Style::default().fg(Color::Yellow),
                    )));
                }
                push_colored_diff_lines(&mut lines, &mutation.diff);
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "[Enter / y] Apply to settings.json   [Esc / n] Cancel   [Up/Down] Scroll",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
            }
        },
        HookModalPlan::Error(msg) => {
            lines.push(Line::from(Span::styled(
                format!("Error: {msg}"),
                Style::default().fg(Color::LightRed),
            )));
            lines.push(Line::from("[Esc] Close"));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    // The modal owns this vertical offset; Up/Down and the wheel in input.rs move it.
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((hook_modal.scroll, 0)),
        popup,
    );
}

/// Read-only view of the in-memory draft, re-serialized every frame so unsaved edits appear at
/// once. It is not the file on disk, which may be older or externally modified.
fn draw_toml_drawer(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    let toml_str = serialize_config_doc(&app.doc);
    let lines: Vec<Line<'static>> = toml_str
        .lines()
        .map(|l| Line::from(l.to_string()))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Live config.toml [t] ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The one footer row doubles as the inline edit prompt: while `text_edit` is active it shows the
/// buffer with commit/cancel keys instead of the hotkey legend.
fn draw_footer_or_text_input(frame: &mut Frame<'_>, area: Rect, app: &ConfiguratorApp) {
    if let Some(ref ed) = app.text_edit {
        let mut spans = vec![
            Span::styled(
                format!(" EDIT {} ", ed.target.prompt_label()),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(ed.buffer.clone(), Style::default().fg(Color::White)),
            Span::styled(
                "_  [Enter] Commit  [Esc] Cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ];
        if let Some(ref err) = ed.error {
            spans.push(Span::styled(
                format!("  ! {err}"),
                Style::default().fg(Color::LightRed),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let text = " { / } Tab | Tab/S-Tab Pane | p Scenario | [/]/0 Width | t TOML | e Env | r Preset | i Hook | s Save | q Quit ";
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}
