//! Key and mouse dispatch for the configurator, in four tiers: modal, inline text edit, focused
//! pane, then global hotkeys. A tier that consumes a key stops the ones below it, so typed text
//! never fires a hotkey. Only Press events are dispatched: the Windows console also reports
//! Release and Repeat, which would double-fire every keystroke.

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::config::{
    ConfigDoc, DEFAULT_COMPACT_AT, ItemId, ItemSlot, ItemStyle, MAX_COMPACT_AT, OverflowMode,
    PresetScope,
};
use crate::render::{BarStyle, ColorToken};
use crate::tui::state::{
    ActiveModal, ConfiguratorApp, ConfiguratorExit, HookAction, InspectorField, LayoutPane,
    MAX_LAYOUT_ROWS, RuntimePane, RuntimeSubField, StyleField, StylePane, TextEditTarget,
    ThresholdField, TuiTab, inspector_bool_default, inspector_fields_for_item,
    ordered_catalog_entries, preset_scope_label, preset_scope_next, preset_scope_prev,
};

impl ConfiguratorApp {
    /// Feeds one terminal event to the workbench. `Some(exit)` ends the event loop, `None` keeps
    /// it running. Release/Repeat are dropped here so every tier below sees Press only.
    pub fn handle_event(&mut self, event: Event) -> Option<ConfiguratorExit> {
        match event {
            Event::Key(key) => {
                // Windows consoles deliver Release (and Repeat) beside each Press; acting on them
                // would double every edit, tab switch and exit.
                if key.kind != KeyEventKind::Press {
                    return None;
                }
                self.handle_key_press(key)
            }
            Event::Mouse(mouse) => self.handle_mouse_event(mouse),
            Event::Resize(width, height) => {
                let prev_width = self.term_size.0;
                self.term_size = (width, height);
                // Under 100 cols the 60/40 drawer split starves the panes.
                if prev_width >= 100 && width < 100 {
                    self.toml_drawer_open = false;
                }
                None
            }
            _ => None,
        }
    }

    fn handle_key_press(&mut self, key: KeyEvent) -> Option<ConfiguratorExit> {
        // Tier 1: Active Modal intercepts all input first.
        if self.active_modal.is_some() {
            return self.handle_tier1_modal_key(key);
        }

        // Tier 2: Inline Text Input (absorbs all printable keys & hotkeys!).
        if self.text_edit.is_some() {
            self.handle_tier2_text_input(key);
            return None;
        }

        // Ctrl+C is tested before the size guard so a too-small window can still be left.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            if self.is_dirty() {
                self.active_modal = Some(ActiveModal::DirtyExit { selected: 0 });
                return None;
            }
            return Some(ConfiguratorExit::Cancelled);
        }

        // Same floor as view.rs MIN_TERM_*: while the guard is up only q/Esc act, and a dirty
        // draft still gets the 3-way exit rather than being dropped.
        if self.term_size.0 < 80 || self.term_size.1 < 22 {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                if self.is_dirty() {
                    self.active_modal = Some(ActiveModal::DirtyExit { selected: 0 });
                    return None;
                }
                return Some(ConfiguratorExit::Cancelled);
            }
            return None;
        }

        // Global Ctrl shortcuts (Undo, Redo, Save, Preset modal).
        // Ahead of tier 3 so no pane can swallow them; inert while the resize guard is up.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.undo();
                    return None;
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.redo();
                    return None;
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    return self.request_save(false);
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.active_modal = Some(ActiveModal::PresetApply {
                        preset_idx: 0,
                        scope: PresetScope::All,
                    });
                    return None;
                }
                _ => {}
            }
        }

        // Tier 3: Focused Pane Local Keys
        // true means the pane consumed the key, so the global hotkeys below must not also fire.
        if self.handle_tier3_pane_key(key) {
            return None;
        }

        // Tier 4: Global Keys
        self.handle_tier4_global_key(key)
    }

    /// Modal tier: consumes every key while a modal is open so the pane underneath cannot be
    /// edited by accident. Ctrl+C closes it, except on DirtyExit where it discards and exits.
    fn handle_tier1_modal_key(&mut self, key: KeyEvent) -> Option<ConfiguratorExit> {
        // Cloned so the arms can replace self.active_modal without holding a borrow on it.
        let modal = self.active_modal.clone()?;

        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            return match modal {
                ActiveModal::DirtyExit { .. } => {
                    self.active_modal = None;
                    Some(ConfiguratorExit::Cancelled)
                }
                ActiveModal::BrokenTomlConfirm { .. }
                | ActiveModal::ExternalConflict { .. }
                | ActiveModal::PresetApply { .. }
                | ActiveModal::HookDiff(_)
                | ActiveModal::HelpOverlay => {
                    self.active_modal = None;
                    None
                }
            };
        }

        match modal {
            ActiveModal::DirtyExit { selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.active_modal = None;
                    None
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.active_modal = None;
                    Some(ConfiguratorExit::Cancelled)
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.active_modal = None;
                    self.request_save(true)
                }
                KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.active_modal = Some(ActiveModal::DirtyExit {
                        selected: (selected + 2) % 3,
                    });
                    None
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                    self.active_modal = Some(ActiveModal::DirtyExit {
                        selected: (selected + 1) % 3,
                    });
                    None
                }
                KeyCode::Enter => match selected {
                    0 => {
                        self.active_modal = None;
                        self.request_save(true)
                    }
                    1 => {
                        self.active_modal = None;
                        Some(ConfiguratorExit::Cancelled)
                    }
                    _ => {
                        self.active_modal = None;
                        None
                    }
                },
                _ => None,
            },
            ActiveModal::BrokenTomlConfirm {
                exit_after_save,
                selected,
            } => match key.code {
                KeyCode::Esc => {
                    self.active_modal = None;
                    None
                }
                // 'b' preserves the unparseable original as config.toml.bak before overwriting.
                KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.perform_write_config(true, exit_after_save)
                }
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.perform_write_config(false, exit_after_save)
                }
                KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.active_modal = Some(ActiveModal::BrokenTomlConfirm {
                        exit_after_save,
                        selected: (selected + 2) % 3,
                    });
                    None
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                    self.active_modal = Some(ActiveModal::BrokenTomlConfirm {
                        exit_after_save,
                        selected: (selected + 1) % 3,
                    });
                    None
                }
                KeyCode::Enter => match selected {
                    0 => self.perform_write_config(true, exit_after_save),
                    1 => self.perform_write_config(false, exit_after_save),
                    _ => {
                        self.active_modal = None;
                        None
                    }
                },
                _ => None,
            },
            ActiveModal::ExternalConflict {
                exit_after_save,
                selected,
            } => match key.code {
                KeyCode::Esc => {
                    self.active_modal = None;
                    None
                }
                // Reload adopts disk and pushes the local draft onto undo; nothing is written.
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.reload_from_disk();
                    None
                }
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.perform_write_config(false, exit_after_save)
                }
                KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.active_modal = Some(ActiveModal::ExternalConflict {
                        exit_after_save,
                        selected: (selected + 2) % 3,
                    });
                    None
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                    self.active_modal = Some(ActiveModal::ExternalConflict {
                        exit_after_save,
                        selected: (selected + 1) % 3,
                    });
                    None
                }
                KeyCode::Enter => match selected {
                    0 => {
                        self.active_modal = None;
                        None
                    }
                    1 => {
                        self.reload_from_disk();
                        None
                    }
                    _ => self.perform_write_config(false, exit_after_save),
                },
                _ => None,
            },
            ActiveModal::PresetApply { preset_idx, scope } => {
                let count = self.available_presets.len();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.active_modal = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if count > 0 {
                            let next_idx = if preset_idx == 0 {
                                count - 1
                            } else {
                                preset_idx - 1
                            };
                            self.active_modal = Some(ActiveModal::PresetApply {
                                preset_idx: next_idx,
                                scope,
                            });
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if count > 0 {
                            self.active_modal = Some(ActiveModal::PresetApply {
                                preset_idx: (preset_idx + 1) % count,
                                scope,
                            });
                        }
                    }
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                        self.active_modal = Some(ActiveModal::PresetApply {
                            preset_idx,
                            scope: preset_scope_prev(scope),
                        });
                    }
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                        self.active_modal = Some(ActiveModal::PresetApply {
                            preset_idx,
                            scope: preset_scope_next(scope),
                        });
                    }
                    // One undo step for the whole preset; idx/scope are copied back to the pane.
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if count > 0 {
                            let idx = preset_idx % count;
                            let name = self.available_presets[idx].clone();
                            self.apply_preset_with_undo(&name, scope);
                            self.style_preset_idx = idx;
                            self.style_preset_scope = scope;
                            self.active_modal = None;
                            self.status_toast = Some(format!(
                                "Applied preset '{name}' ({})",
                                preset_scope_label(scope)
                            ));
                        } else {
                            self.active_modal = None;
                        }
                    }
                    _ => {}
                }
                None
            }
            ActiveModal::HookDiff(mut hook_modal) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                        self.active_modal = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        hook_modal.scroll = hook_modal.scroll.saturating_sub(1);
                        self.active_modal = Some(ActiveModal::HookDiff(hook_modal));
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        hook_modal.scroll = hook_modal.scroll.saturating_add(1);
                        self.active_modal = Some(ActiveModal::HookDiff(hook_modal));
                    }
                    // Writes settings.json only; the unsaved config.toml draft is not touched.
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.apply_active_hook_modal();
                    }
                    _ => {}
                }
                None
            }
            ActiveModal::HelpOverlay => {
                if matches!(
                    key.code,
                    KeyCode::Esc
                        | KeyCode::Enter
                        | KeyCode::Char('?')
                        | KeyCode::Char('q')
                        | KeyCode::Char(' ')
                ) {
                    self.active_modal = None;
                }
                None
            }
        }
    }

    /// Inline edit tier: every printable key goes into the buffer, including letters that are
    /// hotkeys elsewhere ('q', 's', 'p', '[', ']'). The doc is not touched until Enter commits, so
    /// Esc discards the buffer and the previous value survives. Ctrl/Alt chords are swallowed.
    fn handle_tier2_text_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                // Nothing was written to the doc, so dropping the buffer restores the old value.
                self.text_edit = None;
            }
            KeyCode::Enter => {
                self.commit_text_edit();
            }
            KeyCode::Left => {
                if let Some(ref mut ed) = self.text_edit {
                    ed.cursor_chars = ed.cursor_chars.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(ref mut ed) = self.text_edit {
                    let max_len = ed.buffer.chars().count();
                    ed.cursor_chars = (ed.cursor_chars + 1).min(max_len);
                }
            }
            KeyCode::Home => {
                if let Some(ref mut ed) = self.text_edit {
                    ed.cursor_chars = 0;
                }
            }
            KeyCode::End => {
                if let Some(ref mut ed) = self.text_edit {
                    ed.cursor_chars = ed.buffer.chars().count();
                }
            }
            KeyCode::Backspace => {
                if let Some(ref mut ed) = self.text_edit
                    && ed.cursor_chars > 0
                {
                    // The cursor counts chars but String indexes bytes: icons can be multi-byte.
                    let remove_at = ed.cursor_chars - 1;
                    let byte_idx = char_to_byte_idx(&ed.buffer, remove_at);
                    ed.buffer.remove(byte_idx);
                    ed.cursor_chars -= 1;
                    ed.error = None;
                }
            }
            KeyCode::Delete => {
                if let Some(ref mut ed) = self.text_edit {
                    let len = ed.buffer.chars().count();
                    if ed.cursor_chars < len {
                        let byte_idx = char_to_byte_idx(&ed.buffer, ed.cursor_chars);
                        ed.buffer.remove(byte_idx);
                        ed.error = None;
                    }
                }
            }
            KeyCode::Char(ch) => {
                // Ctrl/Alt chords are dropped, not typed; tier 2 swallows Ctrl+C mid-edit too.
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && let Some(ref mut ed) = self.text_edit
                {
                    let byte_idx = char_to_byte_idx(&ed.buffer, ed.cursor_chars);
                    ed.buffer.insert(byte_idx, ch);
                    ed.cursor_chars += 1;
                    ed.error = None;
                }
            }
            _ => {}
        }
    }

    /// Routes to the active tab's focused pane. `true` means the key was consumed and tier 4 is
    /// skipped; `false` lets an unbound key fall through to the global hotkeys.
    fn handle_tier3_pane_key(&mut self, key: KeyEvent) -> bool {
        match self.active_tab {
            TuiTab::Layout => self.handle_layout_tab_key(key),
            TuiTab::StylePresets => self.handle_style_tab_key(key),
            TuiTab::ThresholdsRuntime => self.handle_runtime_tab_key(key),
            TuiTab::AgyHook => self.handle_hook_tab_key(key),
        }
    }

    /// Hook tab: the three actions form a ring, so arrows, hjkl and Tab/BackTab all cycle them.
    /// 'u' and 'x' jump straight to their modal; plain 'i' falls through to tier 4.
    fn handle_hook_tab_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Left
            | KeyCode::Char('h')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::BackTab => {
                self.hook_action = self.hook_action.prev();
                true
            }
            KeyCode::Right
            | KeyCode::Char('l')
            | KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Tab => {
                self.hook_action = self.hook_action.next();
                true
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.open_hook_modal(self.hook_action);
                true
            }
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.hook_action = HookAction::Uninstall;
                self.open_hook_modal(HookAction::Uninstall);
                true
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                self.hook_action = HookAction::Reset;
                self.open_hook_modal(HookAction::Reset);
                true
            }
            _ => false,
        }
    }

    fn handle_layout_tab_key(&mut self, key: KeyEvent) -> bool {
        // Tab/BackTab cycle the three panes before any pane handler sees the key.
        match key.code {
            KeyCode::Tab => {
                self.layout_pane = self.layout_pane.next();
                return true;
            }
            KeyCode::BackTab => {
                self.layout_pane = self.layout_pane.prev();
                return true;
            }
            _ => {}
        }

        match self.layout_pane {
            LayoutPane::ActiveRows => self.handle_active_rows_key(key),
            LayoutPane::Catalog => self.handle_catalog_key(key),
            LayoutPane::Inspector => self.handle_inspector_key(key),
        }
    }

    /// Active Rows pane: the cursor walks both rows, and the case of j/k decides whether the slot
    /// moves with it. Digits are priority hotkeys here but plain text in tier 2.
    fn handle_active_rows_key(&mut self, key: KeyEvent) -> bool {
        // Alt+Up/Down is the arrow alias for the J/K reorder below.
        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Up => {
                    self.reorder_selected_slot(-1);
                    return true;
                }
                KeyCode::Down => {
                    self.reorder_selected_slot(1);
                    return true;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_active_rows_cursor(-1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_active_rows_cursor(1);
                true
            }
            KeyCode::Char('K') => {
                self.reorder_selected_slot(-1);
                true
            }
            KeyCode::Char('J') => {
                self.reorder_selected_slot(1);
                true
            }
            KeyCode::Char('m') => {
                self.move_selected_slot_other_row();
                true
            }
            KeyCode::Char('a') | KeyCode::Char('+') => {
                self.add_layout_row();
                true
            }
            // Delete removes the slot here; Shift+X removes an empty trailing/extra row when > 2 rows.
            KeyCode::Char('X') => {
                self.remove_empty_selected_row();
                true
            }
            KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                self.remove_selected_slot();
                true
            }
            // 1..=5 set the slot priority directly; the same digits type text during an edit.
            KeyCode::Char(d @ '1'..='5') => {
                let p = (d as u8) - b'0';
                self.set_selected_slot_priority(p);
                true
            }
            KeyCode::Enter => {
                if self.selected_item_slot().is_some() {
                    self.layout_pane = LayoutPane::Inspector;
                }
                // Consumed even on an empty row so Enter cannot leak into a global hotkey.
                true
            }
            _ => false,
        }
    }

    fn handle_catalog_key(&mut self, key: KeyEvent) -> bool {
        let entries = ordered_catalog_entries();
        let count = entries.len();
        if count == 0 {
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.catalog_idx > 0 {
                    self.catalog_idx -= 1;
                } else {
                    self.catalog_idx = count - 1;
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.catalog_idx = (self.catalog_idx + 1) % count;
                true
            }
            // Space/Enter toggles membership in the selected row: repeatable items (quota) always
            // append a new instance; singletons move from the other row instead of doubling.
            KeyCode::Char(' ') | KeyCode::Enter => {
                let entry = entries[self.catalog_idx.min(count - 1)];
                let max_row = self.doc.lines.len().max(2) - 1;
                let target_row = self.selected_row.min(max_row);
                let mut added_slot_idx: Option<usize> = None;
                self.mutate_doc(|doc| {
                    Self::ensure_two_rows(doc);
                    while doc.lines.len() <= target_row {
                        doc.lines.push(Vec::new());
                    }
                    if entry.allow_multiple {
                        doc.lines[target_row].push(ItemSlot::new(entry.id));
                        added_slot_idx = Some(doc.lines[target_row].len() - 1);
                    } else if let Some(pos) =
                        doc.lines[target_row].iter().position(|s| s.id == entry.id)
                    {
                        doc.lines[target_row].remove(pos);
                    } else {
                        for (r_idx, row) in doc.lines.iter_mut().enumerate() {
                            if r_idx != target_row {
                                row.retain(|s| s.id != entry.id);
                            }
                        }
                        doc.lines[target_row].push(ItemSlot::new(entry.id));
                        added_slot_idx = Some(doc.lines[target_row].len() - 1);
                    }
                });
                // Follow the added slot so the Inspector can customize it without re-navigating.
                if let Some(new_idx) = added_slot_idx {
                    self.selected_row = target_row;
                    self.selected_slot = new_idx;
                    self.inspector_idx = 0;
                }
                true
            }
            _ => false,
        }
    }

    /// Inspector pane: edits one field of the selected slot. h/l/Space step or toggle in place,
    /// Enter opens the inline editor for text fields, and Delete clears the override.
    fn handle_inspector_key(&mut self, key: KeyEvent) -> bool {
        // Snapshot: the arms below mutate the doc, which a borrow of the slot would forbid.
        let Some(slot_snapshot) = self.selected_item_slot().cloned() else {
            return false;
        };
        let fields = inspector_fields_for_item(slot_snapshot.id);
        if fields.is_empty() {
            return false;
        }
        // Field lists differ per item id, so the index is clamped rather than trusted.
        let field = fields[self.inspector_idx.min(fields.len() - 1)];
        let row = self.selected_row;
        let slot_idx = self.selected_slot;

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.inspector_idx > 0 {
                    self.inspector_idx -= 1;
                } else {
                    self.inspector_idx = fields.len() - 1;
                }
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.inspector_idx = (self.inspector_idx + 1) % fields.len();
                true
            }
            KeyCode::Char(d @ '1'..='5') => {
                let p = (d as u8) - b'0';
                self.set_selected_slot_priority(p);
                true
            }
            // h/l step enums and flip bools; free-text fields ignore them and need Enter.
            KeyCode::Left | KeyCode::Char('h') => {
                self.step_or_toggle_inspector_field(row, slot_idx, &slot_snapshot, field, false)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.step_or_toggle_inspector_field(row, slot_idx, &slot_snapshot, field, true)
            }
            KeyCode::Char(' ') => {
                self.step_or_toggle_inspector_field(row, slot_idx, &slot_snapshot, field, true)
            }
            KeyCode::Enter => self.activate_inspector_field(row, slot_idx, &slot_snapshot, field),
            // Back to None (inherit the default), which drops the key from the serialized TOML.
            KeyCode::Delete | KeyCode::Backspace => {
                self.reset_inspector_field(row, slot_idx, field);
                true
            }
            _ => false,
        }
    }

    /// One h/l/Space step. Priority wraps inside 1..=5 and bools flip; the free-text fields
    /// return `false` because only Enter opens an editor for them.
    fn step_or_toggle_inspector_field(
        &mut self,
        row: usize,
        slot_idx: usize,
        slot_snapshot: &ItemSlot,
        field: InspectorField,
        forward: bool,
    ) -> bool {
        match field {
            InspectorField::Priority => {
                let cur = slot_snapshot.effective_priority();
                let next = if forward {
                    if cur >= 5 { 1 } else { cur + 1 }
                } else if cur <= 1 {
                    5
                } else {
                    cur - 1
                };
                self.set_selected_slot_priority(next);
                true
            }
            InspectorField::Style => {
                let options: &[&str] = match slot_snapshot.id {
                    ItemId::Model => &["", "agybar", "hud"],
                    ItemId::Mode => &["", "badge"],
                    ItemId::Git => &["", "branch", "counts"],
                    ItemId::Status => &["", "dot"],
                    ItemId::Tokens => &["", "arrows"],
                    ItemId::QuotaGroup => &["", "inline"],
                    ItemId::Context => &["", "bar"],
                    ItemId::Cache => &["", "hit_rate"],
                    _ => return false,
                };
                let cur = slot_snapshot.style.as_deref().unwrap_or("");
                let idx = options
                    .iter()
                    .position(|o| o.eq_ignore_ascii_case(cur))
                    .unwrap_or(0);
                let next_idx = if forward {
                    (idx + 1) % options.len()
                } else if idx == 0 {
                    options.len() - 1
                } else {
                    idx - 1
                };
                let chosen = options[next_idx];
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) {
                        s.style = (!chosen.is_empty()).then(|| chosen.to_string());
                    }
                });
                true
            }
            InspectorField::MaxLevels => {
                let cur = i32::from(slot_snapshot.max_levels.unwrap_or(2));
                let next = (cur + if forward { 1 } else { -1 }).clamp(1, 8) as u8;
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) {
                        s.max_levels = Some(next);
                    }
                });
                true
            }
            InspectorField::BarStyle => {
                let styles = BarStyle::ALL;
                // Unset slots inherit the top-level `doc.bar_style`, not always `BarStyle::Line`.
                let cur = slot_snapshot
                    .bar_style
                    .as_deref()
                    .and_then(BarStyle::from_key)
                    .unwrap_or(self.doc.bar_style);
                let idx = styles.iter().position(|&s| s == cur).unwrap_or(0);
                let next_idx = if forward {
                    (idx + 1) % styles.len()
                } else if idx == 0 {
                    styles.len() - 1
                } else {
                    idx - 1
                };
                let next_style = styles[next_idx];
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) {
                        s.bar_style = if next_style == doc.bar_style {
                            None
                        } else {
                            Some(next_style.as_key().to_string())
                        };
                    }
                });
                true
            }
            InspectorField::BarWidth => {
                let cur = i32::from(slot_snapshot.bar_width.unwrap_or(10));
                let next = (cur + if forward { 1 } else { -1 }).clamp(4, 20) as u8;
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) {
                        s.bar_width = (next != 10).then_some(next);
                    }
                });
                true
            }
            InspectorField::Family => {
                let fams = ["gemini", "3p", "@model"];
                let cur = slot_snapshot.family.as_deref().unwrap_or("gemini");
                let idx = fams
                    .iter()
                    .position(|f| f.eq_ignore_ascii_case(cur))
                    .unwrap_or(0);
                let next_idx = if forward {
                    (idx + 1) % fams.len()
                } else if idx == 0 {
                    fams.len() - 1
                } else {
                    idx - 1
                };
                let next = fams[next_idx].to_string();
                self.mutate_doc(|doc| {
                    if let Some(s) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) {
                        s.family = Some(next);
                    }
                });
                true
            }
            InspectorField::ShowRemaining
            | InspectorField::ShowUsed
            | InspectorField::ShowBar
            | InspectorField::ShowReset
            | InspectorField::ShowAmount
            | InspectorField::ShowSubagent
            | InspectorField::ShowDefault
            | InspectorField::Compact
            | InspectorField::ShowDetail
            | InspectorField::ShowWhenOff
            | InspectorField::ShowTtl
            | InspectorField::ShowBreakdown
            | InspectorField::ShowPending
            | InspectorField::ShowTokens => {
                self.toggle_inspector_bool(row, slot_idx, field);
                true
            }
            InspectorField::Icon
            | InspectorField::Fg
            | InspectorField::IconColor
            | InspectorField::Bucket
            | InspectorField::Label => false,
        }
    }

    /// Enter on a field: text fields open the inline editor seeded with the current value (empty
    /// when unset), while bools flip and priority steps without leaving the pane.
    fn activate_inspector_field(
        &mut self,
        row: usize,
        slot_idx: usize,
        slot_snapshot: &ItemSlot,
        field: InspectorField,
    ) -> bool {
        match field {
            InspectorField::Priority => {
                let cur = slot_snapshot.effective_priority();
                let next = if cur >= 5 { 1 } else { cur + 1 };
                self.set_selected_slot_priority(next);
                true
            }
            InspectorField::Icon => {
                let init = slot_snapshot.icon.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorIcon {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::Fg => {
                let init = slot_snapshot.fg.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorFg {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::IconColor => {
                let init = slot_snapshot.icon_color.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorIconColor {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::Style => {
                let init = slot_snapshot.style.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorStyle {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::MaxLevels => {
                let init = slot_snapshot
                    .max_levels
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorMaxLevels {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::Bucket => {
                let init = slot_snapshot.bucket.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorBucket {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::Family => {
                let init = slot_snapshot.family.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorFamily {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::Label => {
                let init = slot_snapshot.label.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorLabel {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::BarStyle => {
                let init = slot_snapshot.bar_style.clone().unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorBarStyle {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::BarWidth => {
                let init = slot_snapshot
                    .bar_width
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                self.start_text_edit(
                    TextEditTarget::InspectorBarWidth {
                        row,
                        slot: slot_idx,
                    },
                    init,
                );
                true
            }
            InspectorField::ShowRemaining
            | InspectorField::ShowUsed
            | InspectorField::ShowBar
            | InspectorField::ShowReset
            | InspectorField::ShowAmount
            | InspectorField::ShowSubagent
            | InspectorField::ShowDefault
            | InspectorField::Compact
            | InspectorField::ShowDetail
            | InspectorField::ShowWhenOff
            | InspectorField::ShowTtl
            | InspectorField::ShowBreakdown
            | InspectorField::ShowPending
            | InspectorField::ShowTokens => {
                self.toggle_inspector_bool(row, slot_idx, field);
                true
            }
        }
    }

    fn toggle_inspector_bool(&mut self, row: usize, slot_idx: usize, field: InspectorField) {
        self.mutate_doc(|doc| {
            let Some(slot) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) else {
                return;
            };
            let id = slot.id;
            // Inheriting `None` yields `def`; toggling sets `Some(!def)`, toggling again clears to `None`.
            let flip = |opt: &mut Option<bool>, def: bool| {
                let next = !opt.unwrap_or(def);
                *opt = if next == def { None } else { Some(next) };
            };
            match field {
                InspectorField::ShowRemaining => {
                    flip(&mut slot.show_remaining, inspector_bool_default(id, field));
                }
                InspectorField::ShowUsed => {
                    flip(&mut slot.show_used, inspector_bool_default(id, field));
                }
                InspectorField::ShowBar => {
                    flip(&mut slot.show_bar, inspector_bool_default(id, field));
                }
                InspectorField::ShowReset => {
                    flip(&mut slot.show_reset, inspector_bool_default(id, field));
                }
                InspectorField::ShowAmount => {
                    flip(&mut slot.show_amount, inspector_bool_default(id, field));
                }
                InspectorField::ShowSubagent => {
                    flip(&mut slot.show_subagent, inspector_bool_default(id, field));
                }
                InspectorField::ShowDefault => {
                    flip(&mut slot.show_default, inspector_bool_default(id, field));
                }
                InspectorField::Compact => {
                    flip(&mut slot.compact, inspector_bool_default(id, field));
                }
                InspectorField::ShowDetail => {
                    flip(&mut slot.show_detail, inspector_bool_default(id, field));
                }
                InspectorField::ShowWhenOff => {
                    flip(&mut slot.show_when_off, inspector_bool_default(id, field));
                }
                InspectorField::ShowTtl => {
                    flip(&mut slot.show_ttl, inspector_bool_default(id, field));
                }
                InspectorField::ShowBreakdown => {
                    flip(&mut slot.show_breakdown, inspector_bool_default(id, field));
                }
                InspectorField::ShowPending => {
                    flip(&mut slot.show_pending, inspector_bool_default(id, field));
                }
                InspectorField::ShowTokens => {
                    flip(&mut slot.show_tokens, inspector_bool_default(id, field));
                }
                InspectorField::Priority
                | InspectorField::Icon
                | InspectorField::Fg
                | InspectorField::IconColor
                | InspectorField::Style
                | InspectorField::MaxLevels
                | InspectorField::Bucket
                | InspectorField::Family
                | InspectorField::Label
                | InspectorField::BarStyle
                | InspectorField::BarWidth => {}
            }
        });
    }

    fn reset_inspector_field(&mut self, row: usize, slot_idx: usize, field: InspectorField) {
        self.mutate_doc(|doc| {
            let Some(slot) = doc.lines.get_mut(row).and_then(|r| r.get_mut(slot_idx)) else {
                return;
            };
            match field {
                InspectorField::Priority => slot.priority = None,
                InspectorField::Icon => slot.icon = None,
                InspectorField::Fg => slot.fg = None,
                InspectorField::IconColor => slot.icon_color = None,
                InspectorField::Style => slot.style = None,
                InspectorField::MaxLevels => slot.max_levels = None,
                InspectorField::ShowRemaining => slot.show_remaining = None,
                InspectorField::Bucket => slot.bucket = None,
                InspectorField::Family => slot.family = None,
                InspectorField::Label => slot.label = None,
                InspectorField::ShowUsed => slot.show_used = None,
                InspectorField::ShowBar => slot.show_bar = None,
                InspectorField::BarStyle => slot.bar_style = None,
                InspectorField::BarWidth => slot.bar_width = None,
                InspectorField::ShowReset => slot.show_reset = None,
                InspectorField::ShowAmount => slot.show_amount = None,
                InspectorField::ShowSubagent => slot.show_subagent = None,
                InspectorField::ShowDefault => slot.show_default = None,
                InspectorField::Compact => slot.compact = None,
                InspectorField::ShowDetail => slot.show_detail = None,
                InspectorField::ShowWhenOff => slot.show_when_off = None,
                InspectorField::ShowTtl => slot.show_ttl = None,
                InspectorField::ShowBreakdown => slot.show_breakdown = None,
                InspectorField::ShowPending => slot.show_pending = None,
                InspectorField::ShowTokens => slot.show_tokens = None,
            }
        });
    }

    fn handle_style_tab_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Tab => {
                self.style_pane = self.style_pane.next();
                return true;
            }
            KeyCode::BackTab => {
                self.style_pane = self.style_pane.prev();
                return true;
            }
            _ => {}
        }

        match self.style_pane {
            StylePane::Presets => {
                let count = self.available_presets.len();
                if count == 0 {
                    return false;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.style_preset_idx = if self.style_preset_idx == 0 {
                            count - 1
                        } else {
                            self.style_preset_idx - 1
                        };
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.style_preset_idx = (self.style_preset_idx + 1) % count;
                        true
                    }
                    // h/l change the apply scope, not the highlighted preset.
                    KeyCode::Left | KeyCode::Char('h') => {
                        self.style_preset_scope = preset_scope_prev(self.style_preset_scope);
                        true
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        self.style_preset_scope = preset_scope_next(self.style_preset_scope);
                        true
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let name = self.available_presets[self.style_preset_idx].clone();
                        let scope = self.style_preset_scope;
                        self.apply_preset_with_undo(&name, scope);
                        true
                    }
                    _ => false,
                }
            }
            StylePane::GlobalStyle => {
                let fields = StyleField::ALL;
                let field = fields[self.style_field_idx % fields.len()];
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.style_field_idx = if self.style_field_idx == 0 {
                            fields.len() - 1
                        } else {
                            self.style_field_idx - 1
                        };
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.style_field_idx = (self.style_field_idx + 1) % fields.len();
                        true
                    }
                    // Enum/bool fields and separator presets cycle with Left/Right; Enter edits raw text on separator/indent.
                    KeyCode::Left | KeyCode::Char('h') => match field {
                        StyleField::Palette => {
                            self.cycle_palette(false);
                            true
                        }
                        StyleField::Theme => {
                            self.cycle_theme(false);
                            true
                        }
                        StyleField::Glyphs => {
                            self.cycle_glyphs(false);
                            true
                        }
                        StyleField::ColorDepth => {
                            self.cycle_color_depth(false);
                            true
                        }
                        StyleField::ItemStyle => {
                            self.cycle_item_style(false);
                            true
                        }
                        StyleField::BarStyle => {
                            self.cycle_bar_style(false);
                            true
                        }
                        StyleField::BarGradient => {
                            self.toggle_bar_gradient();
                            true
                        }
                        StyleField::Separator => {
                            self.cycle_separator_preset(false);
                            true
                        }
                        StyleField::Indent => false,
                    },
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => match field {
                        StyleField::Palette => {
                            self.cycle_palette(true);
                            true
                        }
                        StyleField::Theme => {
                            self.cycle_theme(true);
                            true
                        }
                        StyleField::Glyphs => {
                            self.cycle_glyphs(true);
                            true
                        }
                        StyleField::ColorDepth => {
                            self.cycle_color_depth(true);
                            true
                        }
                        StyleField::ItemStyle => {
                            self.cycle_item_style(true);
                            true
                        }
                        StyleField::BarStyle => {
                            self.cycle_bar_style(true);
                            true
                        }
                        StyleField::BarGradient => {
                            self.toggle_bar_gradient();
                            true
                        }
                        StyleField::Separator => {
                            self.cycle_separator_preset(true);
                            true
                        }
                        StyleField::Indent => false,
                    },
                    KeyCode::Enter => match field {
                        StyleField::Palette => {
                            self.cycle_palette(true);
                            true
                        }
                        StyleField::Theme => {
                            self.cycle_theme(true);
                            true
                        }
                        StyleField::Glyphs => {
                            self.cycle_glyphs(true);
                            true
                        }
                        StyleField::ColorDepth => {
                            self.cycle_color_depth(true);
                            true
                        }
                        StyleField::ItemStyle => {
                            self.cycle_item_style(true);
                            true
                        }
                        StyleField::BarStyle => {
                            self.cycle_bar_style(true);
                            true
                        }
                        StyleField::BarGradient => {
                            self.toggle_bar_gradient();
                            true
                        }
                        StyleField::Separator => {
                            let init = self.doc.separator.clone();
                            self.start_text_edit(TextEditTarget::StyleSeparator, init);
                            true
                        }
                        StyleField::Indent => {
                            let init = self.doc.indent.clone();
                            self.start_text_edit(TextEditTarget::StyleIndent, init);
                            true
                        }
                    },
                    KeyCode::Delete | KeyCode::Backspace => match field {
                        StyleField::ItemStyle => {
                            self.mutate_doc(|doc| doc.item_style = ItemStyle::Minimal);
                            true
                        }
                        StyleField::BarStyle => {
                            let def = ConfigDoc::default().bar_style;
                            self.mutate_doc(|doc| doc.bar_style = def);
                            true
                        }
                        StyleField::BarGradient => {
                            let def = ConfigDoc::default().bar_gradient;
                            self.mutate_doc(|doc| doc.bar_gradient = def);
                            true
                        }
                        StyleField::Separator => {
                            let def = ConfigDoc::default().separator;
                            self.mutate_doc(|doc| doc.separator = def);
                            true
                        }
                        StyleField::Indent => {
                            self.mutate_doc(|doc| doc.indent = "  ".to_string());
                            true
                        }
                        StyleField::Palette
                        | StyleField::Theme
                        | StyleField::Glyphs
                        | StyleField::ColorDepth => false,
                    },
                    _ => false,
                }
            }
            StylePane::PaletteOverrides => {
                let tokens = ColorToken::ALL;
                let tok = tokens[self.palette_override_idx % tokens.len()];
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.palette_override_idx = if self.palette_override_idx == 0 {
                            tokens.len() - 1
                        } else {
                            self.palette_override_idx - 1
                        };
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.palette_override_idx = (self.palette_override_idx + 1) % tokens.len();
                        true
                    }
                    // Seeded with the raw override ("" when unset); committing empty removes it.
                    KeyCode::Enter => {
                        let init = self
                            .doc
                            .palette_overrides
                            .get(tok)
                            .unwrap_or("")
                            .to_string();
                        self.start_text_edit(TextEditTarget::PaletteTokenOverride(tok), init);
                        true
                    }
                    KeyCode::Delete | KeyCode::Backspace => {
                        self.mutate_doc(|doc| doc.palette_overrides.remove(tok));
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    fn handle_runtime_tab_key(&mut self, key: KeyEvent) -> bool {
        // Only two panes, so Tab and Shift+Tab both toggle.
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.runtime_pane = self.runtime_pane.toggle();
                return true;
            }
            _ => {}
        }

        match self.runtime_pane {
            RuntimePane::Thresholds => {
                let fields = ThresholdField::ALL;
                let field = fields[self.threshold_field_idx % fields.len()];
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.threshold_field_idx = if self.threshold_field_idx == 0 {
                            fields.len() - 1
                        } else {
                            self.threshold_field_idx - 1
                        };
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.threshold_field_idx = (self.threshold_field_idx + 1) % fields.len();
                        true
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        self.step_threshold_field(field, -1);
                        true
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        self.step_threshold_field(field, 1);
                        true
                    }
                    KeyCode::Char(' ') => match field {
                        ThresholdField::SingleLine | ThresholdField::Overflow => {
                            self.step_threshold_field(field, 1);
                            true
                        }
                        ThresholdField::CompactAt
                        | ThresholdField::ContextWarn
                        | ThresholdField::ContextHigh
                        | ThresholdField::ContextCrit
                        | ThresholdField::QuotaWarn
                        | ThresholdField::QuotaHigh
                        | ThresholdField::QuotaCrit
                        | ThresholdField::CacheWarnSecs => false,
                    },
                    KeyCode::Enter => {
                        self.activate_threshold_field(field);
                        true
                    }
                    KeyCode::Delete | KeyCode::Backspace => {
                        self.reset_threshold_field(field);
                        true
                    }
                    _ => false,
                }
            }
            RuntimePane::Runtime => {
                let fields = RuntimeSubField::ALL;
                let field = fields[self.runtime_field_idx % fields.len()];
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.runtime_field_idx = if self.runtime_field_idx == 0 {
                            fields.len() - 1
                        } else {
                            self.runtime_field_idx - 1
                        };
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.runtime_field_idx = (self.runtime_field_idx + 1) % fields.len();
                        true
                    }
                    // 250 ms steps, floored at 0 and capped at 600000 ms (10 min).
                    KeyCode::Left | KeyCode::Char('h') => match field {
                        RuntimeSubField::GitCacheMs => {
                            let next = self.doc.git_cache_ms.saturating_sub(250);
                            self.mutate_doc(|doc| doc.git_cache_ms = next);
                            true
                        }
                        RuntimeSubField::CacheRelayUrl => false,
                    },
                    KeyCode::Right | KeyCode::Char('l') => match field {
                        RuntimeSubField::GitCacheMs => {
                            let next = (self.doc.git_cache_ms + 250).min(600_000);
                            self.mutate_doc(|doc| doc.git_cache_ms = next);
                            true
                        }
                        RuntimeSubField::CacheRelayUrl => false,
                    },
                    KeyCode::Enter => match field {
                        RuntimeSubField::GitCacheMs => {
                            let init = self.doc.git_cache_ms.to_string();
                            self.start_text_edit(TextEditTarget::RuntimeGitCacheMs, init);
                            true
                        }
                        RuntimeSubField::CacheRelayUrl => {
                            let init = self.doc.cache_relay_url.clone();
                            self.start_text_edit(TextEditTarget::RuntimeCacheRelayUrl, init);
                            true
                        }
                    },
                    // 2000 ms is the default git cache TTL; an empty URL disables the touch.
                    KeyCode::Delete | KeyCode::Backspace => match field {
                        RuntimeSubField::GitCacheMs => {
                            self.mutate_doc(|doc| doc.git_cache_ms = 2000);
                            true
                        }
                        RuntimeSubField::CacheRelayUrl => {
                            self.mutate_doc(|doc| doc.cache_relay_url.clear());
                            true
                        }
                    },
                    _ => false,
                }
            }
        }
    }

    /// One Left/Right step. Context (`warn < high < crit`) and quota (`warn > high > crit`)
    /// triples are strictly clamped against adjacent neighbors so bounds can never cross or tie.
    fn step_threshold_field(&mut self, field: ThresholdField, dir: i32) {
        match field {
            ThresholdField::SingleLine => {
                let next = !self.doc.single_line;
                self.mutate_doc(|doc| doc.single_line = next);
            }
            ThresholdField::CompactAt => {
                let next = if dir >= 0 {
                    self.doc.compact_at.saturating_add(4).min(MAX_COMPACT_AT)
                } else {
                    self.doc.compact_at.saturating_sub(4)
                };
                self.mutate_doc(|doc| doc.compact_at = next);
            }
            ThresholdField::Overflow => self.toggle_overflow(),
            ThresholdField::ContextWarn => {
                let cur = self.doc.thresholds.context_warn;
                let high = self.doc.thresholds.context_high;
                let next = if dir >= 0 {
                    let cand = cur + 1.0;
                    if cand < high {
                        cand
                    } else if high - 1.0 > cur {
                        high - 1.0
                    } else {
                        cur
                    }
                } else {
                    (cur - 1.0).max(0.0)
                };
                if next < high {
                    self.mutate_doc(|doc| doc.thresholds.context_warn = next);
                }
            }
            ThresholdField::ContextHigh => {
                let cur = self.doc.thresholds.context_high;
                let warn = self.doc.thresholds.context_warn;
                let crit = self.doc.thresholds.context_crit;
                let next = if dir >= 0 {
                    let cand = cur + 1.0;
                    if cand < crit {
                        cand
                    } else if crit - 1.0 > cur {
                        crit - 1.0
                    } else {
                        cur
                    }
                } else {
                    let cand = cur - 1.0;
                    if cand > warn {
                        cand
                    } else if warn + 1.0 < cur {
                        warn + 1.0
                    } else {
                        cur
                    }
                };
                if next > warn && next < crit {
                    self.mutate_doc(|doc| doc.thresholds.context_high = next);
                }
            }
            ThresholdField::ContextCrit => {
                let cur = self.doc.thresholds.context_crit;
                let high = self.doc.thresholds.context_high;
                let next = if dir >= 0 {
                    (cur + 1.0).min(100.0)
                } else {
                    let cand = cur - 1.0;
                    if cand > high {
                        cand
                    } else if high + 1.0 < cur {
                        high + 1.0
                    } else {
                        cur
                    }
                };
                if next > high {
                    self.mutate_doc(|doc| doc.thresholds.context_crit = next);
                }
            }
            ThresholdField::QuotaWarn => {
                let min_val = (self.doc.thresholds.quota_high + 1).min(100);
                let next = (self.doc.thresholds.quota_warn + dir).clamp(min_val, 100);
                if next > self.doc.thresholds.quota_high {
                    self.mutate_doc(|doc| doc.thresholds.quota_warn = next);
                }
            }
            ThresholdField::QuotaHigh => {
                let min_val = self.doc.thresholds.quota_crit + 1;
                let max_val = self.doc.thresholds.quota_warn - 1;
                if min_val <= max_val {
                    let next = (self.doc.thresholds.quota_high + dir).clamp(min_val, max_val);
                    self.mutate_doc(|doc| doc.thresholds.quota_high = next);
                }
            }
            ThresholdField::QuotaCrit => {
                let max_val = (self.doc.thresholds.quota_high - 1).max(0);
                let next = (self.doc.thresholds.quota_crit + dir).clamp(0, max_val);
                if next < self.doc.thresholds.quota_high {
                    self.mutate_doc(|doc| doc.thresholds.quota_crit = next);
                }
            }
            // 5 s steps inside 0..=86400 (one day), the range the numeric editor also enforces.
            ThresholdField::CacheWarnSecs => {
                let next = if dir >= 0 {
                    (self.doc.thresholds.cache_warn_secs + 5).min(86_400)
                } else {
                    self.doc.thresholds.cache_warn_secs.saturating_sub(5)
                };
                self.mutate_doc(|doc| doc.thresholds.cache_warn_secs = next);
            }
        }
    }

    fn activate_threshold_field(&mut self, field: ThresholdField) {
        match field {
            ThresholdField::SingleLine => {
                let next = !self.doc.single_line;
                self.mutate_doc(|doc| doc.single_line = next);
            }
            ThresholdField::CompactAt => {
                let init = self.doc.compact_at.to_string();
                self.start_text_edit(TextEditTarget::ThresholdCompactAt, init);
            }
            ThresholdField::Overflow => self.toggle_overflow(),
            ThresholdField::ContextWarn => {
                let init = format!("{}", self.doc.thresholds.context_warn);
                self.start_text_edit(TextEditTarget::ThresholdContextWarn, init);
            }
            ThresholdField::ContextHigh => {
                let init = format!("{}", self.doc.thresholds.context_high);
                self.start_text_edit(TextEditTarget::ThresholdContextHigh, init);
            }
            ThresholdField::ContextCrit => {
                let init = format!("{}", self.doc.thresholds.context_crit);
                self.start_text_edit(TextEditTarget::ThresholdContextCrit, init);
            }
            ThresholdField::QuotaWarn => {
                let init = self.doc.thresholds.quota_warn.to_string();
                self.start_text_edit(TextEditTarget::ThresholdQuotaWarn, init);
            }
            ThresholdField::QuotaHigh => {
                let init = self.doc.thresholds.quota_high.to_string();
                self.start_text_edit(TextEditTarget::ThresholdQuotaHigh, init);
            }
            ThresholdField::QuotaCrit => {
                let init = self.doc.thresholds.quota_crit.to_string();
                self.start_text_edit(TextEditTarget::ThresholdQuotaCrit, init);
            }
            ThresholdField::CacheWarnSecs => {
                let init = self.doc.thresholds.cache_warn_secs.to_string();
                self.start_text_edit(TextEditTarget::ThresholdCacheWarnSecs, init);
            }
        }
    }

    /// Delete/Backspace: resets to canonical defaults (`compact_at` 88, context 50/75/80,
    /// quota 45/25/20, cache 60 s), clamped against neighbors so a reset never inverts bounds.
    fn reset_threshold_field(&mut self, field: ThresholdField) {
        match field {
            ThresholdField::SingleLine => self.mutate_doc(|doc| doc.single_line = false),
            ThresholdField::CompactAt => {
                self.mutate_doc(|doc| doc.compact_at = DEFAULT_COMPACT_AT);
            }
            ThresholdField::Overflow => {
                self.mutate_doc(|doc| doc.overflow = OverflowMode::Fit);
            }
            ThresholdField::ContextWarn => {
                let high = self.doc.thresholds.context_high;
                let def = if 50.0 < high {
                    50.0
                } else if high >= 1.0 {
                    high - 1.0
                } else {
                    high * 0.5
                };
                if def < high {
                    self.mutate_doc(|doc| doc.thresholds.context_warn = def);
                }
            }
            ThresholdField::ContextHigh => {
                let warn = self.doc.thresholds.context_warn;
                let crit = self.doc.thresholds.context_crit;
                let def = if 75.0 > warn && 75.0 < crit {
                    75.0
                } else {
                    (warn + crit) * 0.5
                };
                if def > warn && def < crit {
                    self.mutate_doc(|doc| doc.thresholds.context_high = def);
                }
            }
            ThresholdField::ContextCrit => {
                let high = self.doc.thresholds.context_high;
                let def = if 80.0 > high {
                    80.0
                } else if high <= 99.0 {
                    high + 1.0
                } else {
                    (high + 100.0) * 0.5
                };
                if def > high {
                    self.mutate_doc(|doc| doc.thresholds.context_crit = def);
                }
            }
            ThresholdField::QuotaWarn => {
                let high = self.doc.thresholds.quota_high;
                let def = 45.max((high + 1).min(100));
                if def > high {
                    self.mutate_doc(|doc| doc.thresholds.quota_warn = def);
                }
            }
            ThresholdField::QuotaHigh => {
                let crit = self.doc.thresholds.quota_crit;
                let warn = self.doc.thresholds.quota_warn;
                if warn - crit >= 2 {
                    let def = if 25 > crit && 25 < warn {
                        25
                    } else {
                        (crit + warn) / 2
                    };
                    self.mutate_doc(|doc| doc.thresholds.quota_high = def);
                }
            }
            ThresholdField::QuotaCrit => {
                let high = self.doc.thresholds.quota_high;
                let def = 20.min((high - 1).max(0));
                if def < high {
                    self.mutate_doc(|doc| doc.thresholds.quota_crit = def);
                }
            }
            ThresholdField::CacheWarnSecs => {
                self.mutate_doc(|doc| doc.thresholds.cache_warn_secs = 60);
            }
        }
    }

    /// Moves the selection over all configured rows flattened into one list, wrapping at the ends.
    /// An empty row contributes one position so it stays selectable; Inspector index resets each move.
    fn move_active_rows_cursor(&mut self, delta: isize) {
        let mut positions: Vec<(usize, usize)> = Vec::new();
        let row_count = self.doc.lines.len().max(2);
        for r in 0..row_count {
            let n = self.doc.lines.get(r).map_or(0, Vec::len);
            if n == 0 {
                positions.push((r, 0));
            } else {
                for s in 0..n {
                    positions.push((r, s));
                }
            }
        }
        if positions.is_empty() {
            return;
        }
        let cur = positions
            .iter()
            .position(|&(r, s)| r == self.selected_row && s == self.selected_slot)
            .unwrap_or(0) as isize;
        let len = positions.len() as isize;
        let next_pos = (cur + delta).rem_euclid(len) as usize;
        let (nr, ns) = positions[next_pos];
        self.selected_row = nr;
        self.selected_slot = ns;
        self.inspector_idx = 0;
    }

    /// J/K: swaps the slot with its neighbour inside the same row, without wrapping, and the
    /// selection follows it to the new index.
    fn reorder_selected_slot(&mut self, delta: isize) {
        let row_idx = self.selected_row;
        let slot_idx = self.selected_slot;
        let Some(row_len) = self.doc.lines.get(row_idx).map(Vec::len) else {
            return;
        };
        if row_len <= 1 || slot_idx >= row_len {
            return;
        }
        let target = slot_idx as isize + delta;
        if target < 0 || target >= row_len as isize {
            return;
        }
        let target_idx = target as usize;
        self.mutate_doc(|doc| {
            doc.lines[row_idx].swap(slot_idx, target_idx);
        });
        self.selected_slot = target_idx;
    }

    /// 'm': appends the slot to the end of the next row (`(from_row + 1) % row_count`) and follows
    /// it there. A no-op on an empty row, which has no slot to move.
    fn move_selected_slot_other_row(&mut self) {
        let row_count = self.doc.lines.len().max(2);
        let from_row = self.selected_row.min(row_count - 1);
        let to_row = (from_row + 1) % row_count;
        let slot_idx = self.selected_slot;
        let has_slot = self
            .doc
            .lines
            .get(from_row)
            .is_some_and(|r| slot_idx < r.len());
        if !has_slot {
            return;
        }
        let mut new_slot_idx = 0usize;
        self.mutate_doc(|doc| {
            Self::ensure_two_rows(doc);
            let moved = doc.lines[from_row].remove(slot_idx);
            doc.lines[to_row].push(moved);
            new_slot_idx = doc.lines[to_row].len() - 1;
        });
        self.selected_row = to_row;
        self.selected_slot = new_slot_idx;
    }

    fn add_layout_row(&mut self) {
        if self.doc.lines.len().max(2) >= MAX_LAYOUT_ROWS {
            self.status_toast = Some(format!("Maximum {MAX_LAYOUT_ROWS} rows reached"));
            return;
        }
        let mut new_row = 0usize;
        self.mutate_doc(|doc| {
            Self::ensure_two_rows(doc);
            if doc.lines.len() < MAX_LAYOUT_ROWS {
                doc.lines.push(Vec::new());
            }
            new_row = doc.lines.len() - 1;
        });
        self.selected_row = new_row;
        self.selected_slot = 0;
        self.inspector_idx = 0;
    }

    fn remove_empty_selected_row(&mut self) {
        if self.doc.lines.len() <= 2 {
            return;
        }
        let row_idx = self.selected_row;
        let is_empty = self.doc.lines.get(row_idx).is_some_and(Vec::is_empty);
        if !is_empty {
            self.status_toast = Some("Clear items from row before removing it with X".to_string());
            return;
        }
        self.mutate_doc(|doc| {
            if doc.lines.len() > 2 && row_idx < doc.lines.len() && doc.lines[row_idx].is_empty() {
                doc.lines.remove(row_idx);
            }
        });
    }

    fn remove_selected_slot(&mut self) {
        let row_idx = self.selected_row;
        let slot_idx = self.selected_slot;
        let has_slot = self
            .doc
            .lines
            .get(row_idx)
            .is_some_and(|r| slot_idx < r.len());
        if !has_slot {
            return;
        }
        self.mutate_doc(|doc| {
            doc.lines[row_idx].remove(slot_idx);
        });
    }

    /// Sets priority 1..=5, ignoring anything else. A value equal to the item's default is stored
    /// as `None` so the TOML keeps only real overrides.
    fn set_selected_slot_priority(&mut self, prio: u8) {
        if !(1..=5).contains(&prio) {
            return;
        }
        let row_idx = self.selected_row;
        let slot_idx = self.selected_slot;
        self.mutate_doc(|doc| {
            if let Some(slot) = doc.lines.get_mut(row_idx).and_then(|r| r.get_mut(slot_idx)) {
                if prio == slot.id.default_priority() {
                    slot.priority = None;
                } else {
                    slot.priority = Some(prio);
                }
            }
        });
    }

    /// Global hotkeys, reached only when no modal, editor or pane consumed the key. Quitting a
    /// dirty draft opens the 3-way exit instead of leaving.
    fn handle_tier4_global_key(&mut self, key: KeyEvent) -> Option<ConfiguratorExit> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.is_dirty() {
                    self.active_modal = Some(ActiveModal::DirtyExit { selected: 0 });
                    None
                } else {
                    Some(ConfiguratorExit::Cancelled)
                }
            }
            KeyCode::Char('s') => self.request_save(false),
            KeyCode::Char('r') => {
                self.active_modal = Some(ActiveModal::PresetApply {
                    preset_idx: 0,
                    scope: PresetScope::All,
                });
                None
            }
            // Hook hotkeys work from any tab, not just agy Hook, and only preview a diff.
            KeyCode::Char('i') => {
                self.hook_action = HookAction::Connect;
                self.open_hook_modal(HookAction::Connect);
                None
            }
            KeyCode::Char('u') => {
                self.hook_action = HookAction::Uninstall;
                self.open_hook_modal(HookAction::Uninstall);
                None
            }
            KeyCode::Char('?') => {
                self.active_modal = Some(ActiveModal::HelpOverlay);
                None
            }
            KeyCode::Char('}') => {
                self.active_tab = self.active_tab.next();
                None
            }
            KeyCode::Char('{') => {
                self.active_tab = self.active_tab.prev();
                None
            }
            KeyCode::F(1) => {
                self.active_tab = TuiTab::Layout;
                None
            }
            KeyCode::F(2) => {
                self.active_tab = TuiTab::StylePresets;
                None
            }
            KeyCode::F(3) => {
                self.active_tab = TuiTab::ThresholdsRuntime;
                None
            }
            KeyCode::F(4) => {
                self.active_tab = TuiTab::AgyHook;
                None
            }
            KeyCode::Char('p') => {
                self.cycle_scenario(true);
                None
            }
            KeyCode::Char('P') => {
                self.cycle_scenario(false);
                None
            }
            // +/- alias the brackets. Steps are 4 cols, clamped to 40..=200, and pin the width so
            // cycling scenarios no longer resets it to the scenario default.
            KeyCode::Char('[') | KeyCode::Char('-') => {
                self.adjust_sim_width(-4);
                None
            }
            KeyCode::Char(']') | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.adjust_sim_width(4);
                None
            }
            // '0' toggles unlimited width, remembering the last finite value for the way back.
            KeyCode::Char('0') => {
                self.toggle_unbounded_width();
                None
            }
            KeyCode::Char('t') => {
                self.toml_drawer_open = !self.toml_drawer_open;
                None
            }
            // Preview-only: File-Only ignores the AGY_* overrides without changing the draft.
            KeyCode::Char('e') => {
                self.effective_preview_mode = !self.effective_preview_mode;
                None
            }
            _ => None,
        }
    }

    /// Mouse tier. The wheel synthesizes Up/Down so it moves the same selection as the keyboard;
    /// clicks are ignored under the resize guard and during an inline edit, which owns the keys.
    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Option<ConfiguratorExit> {
        if self.term_size.0 < 80 || self.term_size.1 < 22 || self.text_edit.is_some() {
            return None;
        }

        // A modal swallows all mouse input; only the hook diff scrolls, 2 rows per wheel notch.
        if let Some(ref mut modal) = self.active_modal {
            match modal {
                ActiveModal::HookDiff(hook_modal) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        hook_modal.scroll = hook_modal.scroll.saturating_sub(2);
                    }
                    MouseEventKind::ScrollDown => {
                        hook_modal.scroll = hook_modal.scroll.saturating_add(2);
                    }
                    _ => {}
                },
                ActiveModal::DirtyExit { .. }
                | ActiveModal::BrokenTomlConfirm { .. }
                | ActiveModal::ExternalConflict { .. }
                | ActiveModal::PresetApply { .. }
                | ActiveModal::HelpOverlay => {}
            }
            return None;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
                let _ = self.handle_tier3_pane_key(up);
                None
            }
            MouseEventKind::ScrollDown => {
                let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
                let _ = self.handle_tier3_pane_key(down);
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_left_click(mouse.column, mouse.row);
                None
            }
            _ => None,
        }
    }

    /// Hit-tests a click against the layout view.rs draws. These row and column constants are a
    /// second copy of that layout, so chrome heights, pane percentages and scroll offsets must
    /// stay in sync with `draw_workbench` and the tab drawers.
    fn handle_left_click(&mut self, col: u16, row: u16) {
        // Chrome rows: top bar 1, optional broken banner 1, dynamic preview_h, tab bar 1, then body.
        let preview_y: u16 = if self.loaded_from_broken_toml { 2 } else { 1 };
        let preview_h = self.preview_box_height();
        let tabs_y: u16 = preview_y + preview_h;
        let body_y: u16 = tabs_y + 1;
        let footer_y: u16 = self.term_size.1.saturating_sub(1);

        if row == preview_y {
            self.cycle_scenario(true);
            return;
        }

        if row == tabs_y {
            let mut cursor_x = 0u16;
            for (idx, tab) in TuiTab::ALL.iter().copied().enumerate() {
                // Tabs pads each title with one space per side and puts "|" between them.
                let seg_w = (tab.title_for_width(self.term_size.0).chars().count() as u16) + 2;
                if col >= cursor_x && col < cursor_x + seg_w {
                    self.active_tab = tab;
                    return;
                }
                cursor_x += seg_w + u16::from(idx + 1 < TuiTab::ALL.len());
            }
            // Right of the tabs is the 22-col drawer label reserved by draw_tab_bar.
            if col >= self.term_size.0.saturating_sub(22) {
                self.toml_drawer_open = !self.toml_drawer_open;
            }
            return;
        }

        if row < body_y || row >= footer_y {
            return;
        }

        // The drawer owns the right 40%, so a click there is not a pane click.
        let main_w = if self.toml_drawer_open {
            (self.term_size.0.saturating_mul(60)) / 100
        } else {
            self.term_size.0
        };
        if col >= main_w {
            return;
        }

        // +1 skips the pane's top border, so inner_row indexes the lines the pane drew.
        let inner_row = row.saturating_sub(body_y + 1) as usize;
        let visible_rows = footer_y.saturating_sub(body_y + 2) as usize;
        match self.active_tab {
            TuiTab::Layout => {
                // The body's own top and bottom border rows are not list items.
                if row == body_y || row == footer_y.saturating_sub(1) {
                    return;
                }
                // All three lists scroll by these offsets, so screen row -> index + offset.
                let (rows_offset, catalog_offset) = self.layout_list_offsets(visible_rows);
                let insp_offset = self.inspector_list_offset(visible_rows);
                let c0 = (main_w * 34) / 100;
                let c1 = c0 + (main_w * 33) / 100;
                if col < c0 {
                    self.layout_pane = LayoutPane::ActiveRows;
                    self.click_active_rows_item(inner_row + rows_offset);
                } else if col < c1 {
                    self.layout_pane = LayoutPane::Catalog;
                    self.click_catalog_item(inner_row + catalog_offset);
                } else {
                    self.layout_pane = LayoutPane::Inspector;
                    if inner_row >= 1 {
                        self.inspector_idx = (inner_row - 1) + insp_offset;
                        self.clamp_layout_selection();
                    }
                }
            }
            TuiTab::StylePresets => {
                if row == body_y || row == footer_y.saturating_sub(1) {
                    return;
                }
                let c0 = (main_w * 26) / 100;
                let c1 = c0 + (main_w * 40) / 100;
                if col < c0 {
                    self.style_pane = StylePane::Presets;
                    // Row 0 is the scope header, so scrolled preset indices start at inner_row 1.
                    let preset_offset = self.style_preset_offset(visible_rows);
                    if inner_row >= 1 && !self.available_presets.is_empty() {
                        self.style_preset_idx =
                            ((inner_row - 1) + preset_offset).min(self.available_presets.len() - 1);
                    }
                } else if col < c1 {
                    self.style_pane = StylePane::GlobalStyle;
                    let row_map = self.style_global_row_map();
                    let global_offset = self.style_global_offset(visible_rows);
                    self.style_field_idx = row_map
                        .get(inner_row + global_offset)
                        .copied()
                        .unwrap_or(StyleField::ALL.len() - 1);
                } else {
                    self.style_pane = StylePane::PaletteOverrides;
                    let ov_offset = self.palette_override_offset(visible_rows);
                    self.palette_override_idx =
                        (inner_row + ov_offset).min(ColorToken::ALL.len() - 1);
                }
            }
            TuiTab::ThresholdsRuntime => {
                if row == body_y || row == footer_y.saturating_sub(1) {
                    return;
                }
                let c0 = (main_w * 52) / 100;
                if col < c0 {
                    self.runtime_pane = RuntimePane::Thresholds;
                    let th_offset = self.threshold_list_offset(visible_rows);
                    self.threshold_field_idx =
                        (inner_row + th_offset).min(ThresholdField::ALL.len() - 1);
                } else {
                    self.runtime_pane = RuntimePane::Runtime;
                    self.runtime_field_idx = inner_row.min(RuntimeSubField::ALL.len() - 1);
                }
            }
            TuiTab::AgyHook => {
                // The button row shifts with the optional symlink/command lines above it, so this
                // index only matches when all are drawn. Column cuts split labels and gaps.
                if inner_row == 6 {
                    let action = if col < 24 {
                        HookAction::Connect
                    } else if col < 50 {
                        HookAction::Uninstall
                    } else {
                        HookAction::Reset
                    };
                    self.hook_action = action;
                    self.open_hook_modal(action);
                }
            }
        }
    }

    /// Maps a scrolled Active Rows screen row back to (row, slot) by walking the same header and
    /// empty-row lines `draw_layout_tab` emits. Clicking a line header selects that row's slot 0.
    fn click_active_rows_item(&mut self, inner_row: usize) {
        let mut line_cursor = 0usize;
        let row_count = self.doc.lines.len().max(2);
        for r_idx in 0..row_count {
            if inner_row == line_cursor {
                self.selected_row = r_idx;
                self.selected_slot = 0;
                self.clamp_layout_selection();
                return;
            }
            line_cursor += 1;
            let n = self.doc.lines.get(r_idx).map_or(0, Vec::len);
            if n == 0 {
                if inner_row == line_cursor {
                    self.selected_row = r_idx;
                    self.selected_slot = 0;
                    self.clamp_layout_selection();
                    return;
                }
                line_cursor += 1;
            } else {
                for s_idx in 0..n {
                    if inner_row == line_cursor {
                        self.selected_row = r_idx;
                        self.selected_slot = s_idx;
                        self.clamp_layout_selection();
                        return;
                    }
                    line_cursor += 1;
                }
            }
        }
    }

    /// Same mapping for the Catalog, counting the category header lines the view inserts.
    fn click_catalog_item(&mut self, inner_row: usize) {
        let entries = ordered_catalog_entries();
        let mut line_cursor = 0usize;
        let mut last_cat = None;
        for (c_idx, entry) in entries.iter().enumerate() {
            if last_cat != Some(entry.category) {
                last_cat = Some(entry.category);
                line_cursor += 1;
            }
            if inner_row == line_cursor {
                self.catalog_idx = c_idx;
                return;
            }
            line_cursor += 1;
        }
    }
}

/// Byte offset of char index `char_idx`, or `s.len()` when it is past the end. The edit cursor
/// counts chars while `String::insert`/`remove` need bytes, so multi-byte icons stay intact.
fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}
