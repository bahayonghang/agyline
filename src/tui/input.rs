use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::config::{ItemSlot, PresetScope};
use crate::render::ColorToken;
use crate::tui::state::{
    ActiveModal, ConfiguratorApp, ConfiguratorExit, HookAction, InspectorField, LayoutPane,
    RuntimePane, RuntimeSubField, StyleField, StylePane, TextEditTarget, ThresholdField, TuiTab,
    inspector_fields_for_item, ordered_catalog_entries, preset_scope_label, preset_scope_next,
    preset_scope_prev,
};

impl ConfiguratorApp {
    pub fn handle_event(&mut self, event: Event) -> Option<ConfiguratorExit> {
        match event {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    return None;
                }
                self.handle_key_press(key)
            }
            Event::Mouse(mouse) => self.handle_mouse_event(mouse),
            Event::Resize(width, height) => {
                let prev_width = self.term_size.0;
                self.term_size = (width, height);
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

        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            if self.is_dirty() {
                self.active_modal = Some(ActiveModal::DirtyExit { selected: 0 });
                return None;
            }
            return Some(ConfiguratorExit::Cancelled);
        }

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
        if self.handle_tier3_pane_key(key) {
            return None;
        }

        // Tier 4: Global Keys
        self.handle_tier4_global_key(key)
    }

    fn handle_tier1_modal_key(&mut self, key: KeyEvent) -> Option<ConfiguratorExit> {
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

    fn handle_tier2_text_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
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

    fn handle_tier3_pane_key(&mut self, key: KeyEvent) -> bool {
        match self.active_tab {
            TuiTab::Layout => self.handle_layout_tab_key(key),
            TuiTab::StylePresets => self.handle_style_tab_key(key),
            TuiTab::ThresholdsRuntime => self.handle_runtime_tab_key(key),
            TuiTab::AgyHook => self.handle_hook_tab_key(key),
        }
    }

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

    fn handle_active_rows_key(&mut self, key: KeyEvent) -> bool {
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
            KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                self.remove_selected_slot();
                true
            }
            KeyCode::Char(d @ '1'..='5') => {
                let p = (d as u8) - b'0';
                self.set_selected_slot_priority(p);
                true
            }
            KeyCode::Enter => {
                if self.selected_item_slot().is_some() {
                    self.layout_pane = LayoutPane::Inspector;
                }
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
            KeyCode::Char(' ') | KeyCode::Enter => {
                let entry = entries[self.catalog_idx.min(count - 1)];
                let target_row = self.selected_row.min(1);
                let mut added_slot_idx: Option<usize> = None;
                self.mutate_doc(|doc| {
                    Self::ensure_two_rows(doc);
                    if entry.allow_multiple {
                        doc.lines[target_row].push(ItemSlot::new(entry.id));
                        added_slot_idx = Some(doc.lines[target_row].len() - 1);
                    } else if let Some(pos) =
                        doc.lines[target_row].iter().position(|s| s.id == entry.id)
                    {
                        doc.lines[target_row].remove(pos);
                    } else {
                        let other_row = 1 - target_row;
                        doc.lines[other_row].retain(|s| s.id != entry.id);
                        doc.lines[target_row].push(ItemSlot::new(entry.id));
                        added_slot_idx = Some(doc.lines[target_row].len() - 1);
                    }
                });
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

    fn handle_inspector_key(&mut self, key: KeyEvent) -> bool {
        let Some(slot_snapshot) = self.selected_item_slot().cloned() else {
            return false;
        };
        let fields = inspector_fields_for_item(slot_snapshot.id);
        if fields.is_empty() {
            return false;
        }
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
            KeyCode::Delete | KeyCode::Backspace => {
                self.reset_inspector_field(row, slot_idx, field);
                true
            }
            _ => false,
        }
    }

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
            InspectorField::ShowRemaining
            | InspectorField::ShowReset
            | InspectorField::ShowAmount
            | InspectorField::ShowSubagent
            | InspectorField::ShowDefault
            | InspectorField::Compact
            | InspectorField::ShowDetail => {
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
            InspectorField::ShowRemaining
            | InspectorField::ShowReset
            | InspectorField::ShowAmount
            | InspectorField::ShowSubagent
            | InspectorField::ShowDefault
            | InspectorField::Compact
            | InspectorField::ShowDetail => {
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
            let flip = |opt: &mut Option<bool>| {
                *opt = if opt.unwrap_or(false) {
                    None
                } else {
                    Some(true)
                };
            };
            match field {
                InspectorField::ShowRemaining => flip(&mut slot.show_remaining),
                InspectorField::ShowReset => flip(&mut slot.show_reset),
                InspectorField::ShowAmount => flip(&mut slot.show_amount),
                InspectorField::ShowSubagent => flip(&mut slot.show_subagent),
                InspectorField::ShowDefault => flip(&mut slot.show_default),
                InspectorField::Compact => flip(&mut slot.compact),
                InspectorField::ShowDetail => flip(&mut slot.show_detail),
                InspectorField::Priority
                | InspectorField::Icon
                | InspectorField::Fg
                | InspectorField::IconColor
                | InspectorField::Bucket
                | InspectorField::Label => {}
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
                InspectorField::ShowRemaining => slot.show_remaining = None,
                InspectorField::Bucket => slot.bucket = None,
                InspectorField::Label => slot.label = None,
                InspectorField::ShowReset => slot.show_reset = None,
                InspectorField::ShowAmount => slot.show_amount = None,
                InspectorField::ShowSubagent => slot.show_subagent = None,
                InspectorField::ShowDefault => slot.show_default = None,
                InspectorField::Compact => slot.compact = None,
                InspectorField::ShowDetail => slot.show_detail = None,
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
                        StyleField::Separator | StyleField::Indent => false,
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
                        StyleField::Separator | StyleField::Indent => false,
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
                        StyleField::Separator => {
                            self.mutate_doc(|doc| doc.separator = " | ".to_string());
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
                        ThresholdField::ContextWarn
                        | ThresholdField::ContextCrit
                        | ThresholdField::QuotaWarn
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

    fn step_threshold_field(&mut self, field: ThresholdField, dir: i32) {
        match field {
            ThresholdField::SingleLine => {
                let next = !self.doc.single_line;
                self.mutate_doc(|doc| doc.single_line = next);
            }
            ThresholdField::Overflow => self.toggle_overflow(),
            ThresholdField::ContextWarn => {
                let max_val = (self.doc.thresholds.context_crit - 1.0).max(0.0);
                let next = (self.doc.thresholds.context_warn + f64::from(dir)).clamp(0.0, max_val);
                self.mutate_doc(|doc| doc.thresholds.context_warn = next);
            }
            ThresholdField::ContextCrit => {
                let min_val = (self.doc.thresholds.context_warn + 1.0).min(100.0);
                let next =
                    (self.doc.thresholds.context_crit + f64::from(dir)).clamp(min_val, 100.0);
                self.mutate_doc(|doc| doc.thresholds.context_crit = next);
            }
            ThresholdField::QuotaWarn => {
                let min_val = (self.doc.thresholds.quota_crit + 1).min(100);
                let next = (self.doc.thresholds.quota_warn + dir).clamp(min_val, 100);
                self.mutate_doc(|doc| doc.thresholds.quota_warn = next);
            }
            ThresholdField::QuotaCrit => {
                let max_val = (self.doc.thresholds.quota_warn - 1).max(0);
                let next = (self.doc.thresholds.quota_crit + dir).clamp(0, max_val);
                self.mutate_doc(|doc| doc.thresholds.quota_crit = next);
            }
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
            ThresholdField::Overflow => self.toggle_overflow(),
            ThresholdField::ContextWarn => {
                let init = format!("{}", self.doc.thresholds.context_warn);
                self.start_text_edit(TextEditTarget::ThresholdContextWarn, init);
            }
            ThresholdField::ContextCrit => {
                let init = format!("{}", self.doc.thresholds.context_crit);
                self.start_text_edit(TextEditTarget::ThresholdContextCrit, init);
            }
            ThresholdField::QuotaWarn => {
                let init = self.doc.thresholds.quota_warn.to_string();
                self.start_text_edit(TextEditTarget::ThresholdQuotaWarn, init);
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

    fn reset_threshold_field(&mut self, field: ThresholdField) {
        match field {
            ThresholdField::SingleLine => self.mutate_doc(|doc| doc.single_line = false),
            ThresholdField::Overflow => {
                self.mutate_doc(|doc| doc.overflow = crate::config::OverflowMode::Fit)
            }
            ThresholdField::ContextWarn => {
                let def = 50.0_f64.min((self.doc.thresholds.context_crit - 1.0).max(0.0));
                self.mutate_doc(|doc| doc.thresholds.context_warn = def);
            }
            ThresholdField::ContextCrit => {
                let def = 80.0_f64.max((self.doc.thresholds.context_warn + 1.0).min(100.0));
                self.mutate_doc(|doc| doc.thresholds.context_crit = def);
            }
            ThresholdField::QuotaWarn => {
                let def = 45.max((self.doc.thresholds.quota_crit + 1).min(100));
                self.mutate_doc(|doc| doc.thresholds.quota_warn = def);
            }
            ThresholdField::QuotaCrit => {
                let def = 20.min((self.doc.thresholds.quota_warn - 1).max(0));
                self.mutate_doc(|doc| doc.thresholds.quota_crit = def);
            }
            ThresholdField::CacheWarnSecs => {
                self.mutate_doc(|doc| doc.thresholds.cache_warn_secs = 60);
            }
        }
    }

    fn move_active_rows_cursor(&mut self, delta: isize) {
        let mut positions: Vec<(usize, usize)> = Vec::new();
        for r in 0..2 {
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

    fn move_selected_slot_other_row(&mut self) {
        let from_row = self.selected_row.min(1);
        let to_row = 1 - from_row;
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
            KeyCode::Char('[') | KeyCode::Char('-') => {
                self.adjust_sim_width(-4);
                None
            }
            KeyCode::Char(']') | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.adjust_sim_width(4);
                None
            }
            KeyCode::Char('0') => {
                self.toggle_unbounded_width();
                None
            }
            KeyCode::Char('t') => {
                self.toml_drawer_open = !self.toml_drawer_open;
                None
            }
            KeyCode::Char('e') => {
                self.effective_preview_mode = !self.effective_preview_mode;
                None
            }
            _ => None,
        }
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Option<ConfiguratorExit> {
        if self.term_size.0 < 80 || self.term_size.1 < 22 || self.text_edit.is_some() {
            return None;
        }

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

    fn handle_left_click(&mut self, col: u16, row: u16) {
        let preview_y: u16 = if self.loaded_from_broken_toml { 2 } else { 1 };
        let tabs_y: u16 = preview_y + 5;
        let body_y: u16 = tabs_y + 1;
        let footer_y: u16 = self.term_size.1.saturating_sub(1);

        if row == preview_y {
            self.cycle_scenario(true);
            return;
        }

        if row == tabs_y {
            let mut cursor_x = 0u16;
            for (idx, tab) in TuiTab::ALL.iter().copied().enumerate() {
                let seg_w = (tab.title().chars().count() as u16) + 2;
                if col >= cursor_x && col < cursor_x + seg_w {
                    self.active_tab = tab;
                    return;
                }
                cursor_x += seg_w + u16::from(idx + 1 < TuiTab::ALL.len());
            }
            if col >= self.term_size.0.saturating_sub(22) {
                self.toml_drawer_open = !self.toml_drawer_open;
            }
            return;
        }

        if row < body_y || row >= footer_y {
            return;
        }

        let main_w = if self.toml_drawer_open {
            (self.term_size.0.saturating_mul(60)) / 100
        } else {
            self.term_size.0
        };
        if col >= main_w {
            return;
        }

        let inner_row = row.saturating_sub(body_y + 1) as usize;
        match self.active_tab {
            TuiTab::Layout => {
                let c0 = (main_w * 34) / 100;
                let c1 = c0 + (main_w * 33) / 100;
                if col < c0 {
                    self.layout_pane = LayoutPane::ActiveRows;
                    self.click_active_rows_item(inner_row);
                } else if col < c1 {
                    self.layout_pane = LayoutPane::Catalog;
                    self.click_catalog_item(inner_row);
                } else {
                    self.layout_pane = LayoutPane::Inspector;
                    if inner_row >= 1 {
                        self.inspector_idx = inner_row - 1;
                        self.clamp_layout_selection();
                    }
                }
            }
            TuiTab::StylePresets => {
                let c0 = (main_w * 26) / 100;
                let c1 = c0 + (main_w * 40) / 100;
                if col < c0 {
                    self.style_pane = StylePane::Presets;
                    if inner_row >= 1 && !self.available_presets.is_empty() {
                        self.style_preset_idx =
                            (inner_row - 1).min(self.available_presets.len() - 1);
                    }
                } else if col < c1 {
                    self.style_pane = StylePane::GlobalStyle;
                    self.style_field_idx = inner_row.min(StyleField::ALL.len() - 1);
                } else {
                    self.style_pane = StylePane::PaletteOverrides;
                    self.palette_override_idx = inner_row.min(ColorToken::ALL.len() - 1);
                }
            }
            TuiTab::ThresholdsRuntime => {
                let c0 = (main_w * 52) / 100;
                if col < c0 {
                    self.runtime_pane = RuntimePane::Thresholds;
                    self.threshold_field_idx = inner_row.min(ThresholdField::ALL.len() - 1);
                } else {
                    self.runtime_pane = RuntimePane::Runtime;
                    self.runtime_field_idx = inner_row.min(RuntimeSubField::ALL.len() - 1);
                }
            }
            TuiTab::AgyHook => {
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

    fn click_active_rows_item(&mut self, inner_row: usize) {
        let mut line_cursor = 0usize;
        for r_idx in 0..2 {
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

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}
