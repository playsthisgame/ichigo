use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::fs;
use crate::config::{dir_for, prune_empty_parents, global_config_path, local_config_path};
use super::{App, Mode, PendingAction, RequestDraft, ResponseKind};
use super::tree::{VisibleRow, visible_rows, build_tree, load_entries};

pub(super) fn handle_key_new_request(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Browse;
        }
        KeyCode::Tab => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                draft.focused = (draft.focused + 1) % draft.fields.len();
            }
        }
        KeyCode::BackTab => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                let len = draft.fields.len();
                draft.focused = draft.focused.checked_sub(1).unwrap_or(len - 1);
            }
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                draft.global = !draft.global;
            }
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let draft = match &app.mode {
                Mode::NewRequest { draft, .. } => Some(draft.clone()),
                _ => None,
            };
            // Into the list rather than straight to a blank profile: it is the
            // only door to NewProfile, so add/edit/delete all start alike. An
            // empty list opens on the "new" row, so this is still one Enter
            // away from the old behaviour.
            if let Some(draft) = draft {
                app.mode = Mode::ProfileList { draft, selected: 0 };
            }
        }
        KeyCode::Char(c) => {
            if let Mode::NewRequest { draft, error } = &mut app.mode {
                let focused = draft.focused;
                draft.fields[focused].1.push(c);
                *error = None;
            }
        }
        KeyCode::Backspace => {
            if let Mode::NewRequest { draft, error } = &mut app.mode {
                let focused = draft.focused;
                draft.fields[focused].1.pop();
                *error = None;
            }
        }
        KeyCode::Enter => {
            app.save_new_request();
        }
        _ => {}
    }
    false
}

/// The import buffer.
///
/// Enter inserts a newline rather than confirming: a terminal without bracketed
/// paste delivers a pasted multi-line command as characters with Enters between
/// the lines, so treating Enter as "confirm" would parse the first line alone.
/// Ctrl+s confirms instead.
pub(super) fn handle_key_import_curl(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => app.mode = Mode::Browse,
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.confirm_import_curl();
        }
        KeyCode::Enter => {
            if let Mode::ImportCurl { buffer, error } = &mut app.mode {
                buffer.push('\n');
                *error = None;
            }
        }
        KeyCode::Char(c) => {
            if let Mode::ImportCurl { buffer, error } = &mut app.mode {
                buffer.push(c);
                *error = None;
            }
        }
        KeyCode::Backspace => {
            if let Mode::ImportCurl { buffer, error } = &mut app.mode {
                buffer.pop();
                *error = None;
            }
        }
        _ => {}
    }
    false
}

/// The draft's profiles. Rows are `0..profiles.len()` plus a trailing "new"
/// row, so `selected == profiles.len()` means "add one" — an empty list opens
/// there and Enter creates straight away.
pub(super) fn handle_key_profile_list(app: &mut App, key: KeyEvent) -> bool {
    let Mode::ProfileList { draft, selected } = &mut app.mode else { return false };
    let new_row = draft.profiles.len();

    match key.code {
        // Back to the form. Profile edits live in the draft until the request
        // itself is saved, so leaving here writes nothing.
        KeyCode::Esc => {
            app.mode = Mode::NewRequest { draft: draft.clone(), error: None };
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if *selected < new_row {
                *selected += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            *selected = selected.saturating_sub(1);
        }
        KeyCode::Char('n') => {
            app.mode = Mode::NewProfile {
                draft: draft.clone(),
                editing: None,
                name: String::new(),
                params: Vec::new(),
                focused: 0,
                error: None,
            };
        }
        KeyCode::Char('d') => {
            if *selected < new_row {
                draft.profiles.remove(*selected);
                // The list just got shorter; keep the cursor on a real row.
                *selected = (*selected).min(draft.profiles.len());
            }
        }
        KeyCode::Enter => {
            let idx = *selected;
            let draft = draft.clone();
            app.mode = match draft.profiles.get(idx) {
                Some(profile) => {
                    // A profile's params are a HashMap, which has no order of
                    // its own; sort so the fields do not shuffle between edits.
                    let mut params: Vec<(String, String)> = profile
                        .params
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    params.sort_by(|a, b| a.0.cmp(&b.0));
                    Mode::NewProfile {
                        name: profile.name.clone(),
                        editing: Some(idx),
                        draft,
                        params,
                        focused: 0,
                        error: None,
                    }
                }
                None => Mode::NewProfile {
                    draft,
                    editing: None,
                    name: String::new(),
                    params: Vec::new(),
                    focused: 0,
                    error: None,
                },
            };
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_new_profile(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            // Back to the list without saving. `selected` returns to the
            // profile being edited, or to the "new" row when adding one.
            if let Mode::NewProfile { draft, editing, .. } = &app.mode {
                let selected = editing.unwrap_or(draft.profiles.len());
                app.mode = Mode::ProfileList { draft: draft.clone(), selected };
            }
        }
        KeyCode::Tab => {
            if let Mode::NewProfile { params, focused, .. } = &mut app.mode {
                let total = 1 + 2 * params.len();
                *focused = (*focused + 1) % total.max(1);
            }
        }
        KeyCode::BackTab => {
            if let Mode::NewProfile { params, focused, .. } = &mut app.mode {
                let total = 1 + 2 * params.len();
                *focused = focused.checked_sub(1).unwrap_or(total.saturating_sub(1));
            }
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewProfile { params, focused, .. } = &mut app.mode {
                let new_idx = 1 + 2 * params.len();
                params.push((String::new(), String::new()));
                *focused = new_idx;
            }
        }
        KeyCode::Char(c) => {
            if let Mode::NewProfile { name, params, focused, error, .. } = &mut app.mode {
                *error = None;
                if *focused == 0 {
                    name.push(c);
                } else {
                    let param_idx = (*focused - 1) / 2;
                    let is_value = (*focused - 1) % 2 == 1;
                    if let Some((k, v)) = params.get_mut(param_idx) {
                        if is_value { v.push(c) } else { k.push(c) }
                    }
                }
            }
        }
        KeyCode::Backspace => {
            if let Mode::NewProfile { name, params, focused, .. } = &mut app.mode {
                if *focused == 0 {
                    name.pop();
                } else {
                    let param_idx = (*focused - 1) / 2;
                    let is_value = (*focused - 1) % 2 == 1;
                    if let Some((k, v)) = params.get_mut(param_idx) {
                        if is_value { v.pop() } else { k.pop() };
                    }
                }
            }
        }
        KeyCode::Enter => {
            app.save_new_profile();
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_profile_select(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Esc => {
            app.mode = Mode::Browse;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Mode::ProfileSelect { profiles, selected, .. } = &mut app.mode {
                *selected = (*selected + 1).min(profiles.len());
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Mode::ProfileSelect { selected, .. } = &mut app.mode {
                *selected = selected.saturating_sub(1);
            }
        }
        KeyCode::Enter => {
            app.confirm_profile_select();
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_browse(app: &mut App, code: KeyCode) -> bool {
    let was_pending_g = app.pending_g;
    app.pending_g = false;
    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Char('G') => app.move_bottom(),
        KeyCode::Char('g') => {
            if was_pending_g {
                app.move_top();
            } else {
                app.pending_g = true;
            }
        }
        KeyCode::Char(' ') if app.using_tree() => {
            if let Some(pos) = app.list_state.selected() {
                let rows = visible_rows(&app.tree, &app.collapsed_folders);
                if let Some(VisibleRow::Folder { path, .. }) = rows.get(pos) {
                    app.toggle_folder(path.clone());
                }
            }
        }
        KeyCode::Char(' ') => {}
        // Capital R, because lowercase r already runs the selected config.
        KeyCode::Char('R') => app.reload_entries(None),
        KeyCode::Char('r') | KeyCode::Enter => {
            if app.using_tree()
                && let Some(pos) = app.list_state.selected() {
                let rows = visible_rows(&app.tree, &app.collapsed_folders);
                if let Some(VisibleRow::Folder { path, .. }) = rows.get(pos) {
                    app.toggle_folder(path.clone());
                    return false;
                }
            }
            app.try_run_selected()
        }
        KeyCode::Char('t') => app.try_test_selected(),
        KeyCode::Char('y') => app.try_copy_curl_selected(),
        KeyCode::Char('n') => {
            app.mode = Mode::NewRequest { draft: RequestDraft::blank(), error: None };
        }
        KeyCode::Char('i') => {
            app.mode = Mode::ImportCurl { buffer: String::new(), error: None };
        }
        KeyCode::Char('e') => app.edit_selected(),
        KeyCode::Char('c') => app.clone_selected(),
        KeyCode::Char('p') => app.edit_profiles_selected(),
        KeyCode::Char('d') => {
            let Some(idx) = app.selected_entry_index() else { return false };
            let entry = &app.entries[idx];
            app.mode = Mode::ConfirmDelete { entry_name: entry.name.clone(), global: entry.global };
        }
        KeyCode::Char('f') => {
            app.filter_active = true;
            let count = app.filtered_indices().len();
            app.list_state.select(if count == 0 { None } else { Some(0) });
        }
        KeyCode::Char('?') => app.show_help = true,
        _ => {}
    }
    false
}

pub(super) fn handle_key_filter(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.filter_active = false;
            app.filter.clear();
            // Esc returns to tree mode; restore cursor at top of tree.
            let count = app.visible_count();
            app.list_state.select(if count == 0 { None } else { Some(0) });
        }
        KeyCode::Enter => {
            app.filter_active = false;
        }
        KeyCode::Char(c) => {
            app.filter.push(c);
            let count = app.visible_count();
            app.list_state.select(if count == 0 { None } else { Some(0) });
        }
        KeyCode::Backspace => {
            app.filter.pop();
            let count = app.visible_count();
            app.list_state.select(if count == 0 { None } else { Some(0) });
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_response_filter(app: &mut App, key: KeyEvent) -> bool {
    if let Mode::Response { response_filter, response_filter_active, scroll, .. } = &mut app.mode {
        match key.code {
            KeyCode::Esc => {
                response_filter.clear();
                *response_filter_active = false;
                *scroll = 0;
            }
            KeyCode::Enter => {
                *response_filter_active = false;
            }
            KeyCode::Char(c) => {
                response_filter.push(c);
                *scroll = 0;
            }
            KeyCode::Backspace => {
                response_filter.pop();
                *scroll = 0;
            }
            _ => {}
        }
    }
    false
}

pub(super) fn handle_key_confirm_delete(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let (entry_name, global) = match &app.mode {
                Mode::ConfirmDelete { entry_name , global} => (entry_name.clone(), *global),
                _ => return false,
            };

            let path = if global {
                global_config_path(&entry_name)
            } else {
                local_config_path(&entry_name)
            };
            let _ = fs::remove_file(&path);
            prune_empty_parents(&path, &dir_for(global));
            if let Ok(entries) = load_entries() {
                app.tree = build_tree(&entries);
                app.entries = entries;
                let count = app.visible_count();
                let new_pos = app.list_state.selected()
                    .map(|i| i.min(count.saturating_sub(1)));
                app.list_state.select(if count == 0 { None } else { new_pos });
            }
            app.mode = Mode::Browse;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.mode = Mode::Browse;
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_var_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Browse;
        }
        KeyCode::Enter => {
            // Clone out the data we need; the borrow of app.mode ends
            // when this block exits, letting execute_request take &mut app.
            let (entry_name, var_map, action) = match &app.mode {
                Mode::VarInput { entry_name, vars, action, .. } => (
                    entry_name.clone(),
                    vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>(),
                    *action,
                ),
                _ => return false,
            };
            // Dispatch on the action the form was opened for. Without this,
            // every request whose profile misses a placeholder would run,
            // whatever the user actually asked for.
            match action {
                PendingAction::Curl => app.render_curl(&entry_name, &var_map),
                _ if app.is_chain_entry(&entry_name) => app.execute_chain(&entry_name, &var_map),
                _ => app.execute_request(&entry_name, &var_map),
            }
        }
        KeyCode::Tab => {
            if let Mode::VarInput { vars, focused, .. } = &mut app.mode {
                let len = vars.len();
                *focused = (*focused + 1) % len;
            }
        }
        KeyCode::BackTab => {
            if let Mode::VarInput { vars, focused, .. } = &mut app.mode {
                let len = vars.len();
                *focused = focused.checked_sub(1).unwrap_or(len - 1);
            }
        }
        KeyCode::Char(c) => {
            if let Mode::VarInput { vars, focused, .. } = &mut app.mode {
                vars[*focused].1.push(c);
            }
        }
        KeyCode::Backspace => {
            if let Mode::VarInput { vars, focused, .. } = &mut app.mode {
                vars[*focused].1.pop();
            }
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_test_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Browse;
        }
        KeyCode::Enter => {
            let (entry_name, var_map, iterations) = match &app.mode {
                Mode::TestInput { entry_name, vars, iterations, .. } => (
                    entry_name.clone(),
                    vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>(),
                    iterations.parse::<usize>().unwrap_or(10),
                ),
                _ => return false,
            };
            app.execute_test(&entry_name, &var_map, iterations);
        }
        KeyCode::Tab => {
            if let Mode::TestInput { vars, focused, .. } = &mut app.mode {
                let len = vars.len() + 1;
                *focused = (*focused + 1) % len;
            }
        }
        KeyCode::BackTab => {
            if let Mode::TestInput { vars, focused, .. } = &mut app.mode {
                let len = vars.len() + 1;
                *focused = focused.checked_sub(1).unwrap_or(len - 1);
            }
        }
        KeyCode::Char(c) => {
            if let Mode::TestInput { vars, focused, iterations, .. } = &mut app.mode {
                if *focused < vars.len() {
                    vars[*focused].1.push(c);
                } else if c.is_ascii_digit() {
                    iterations.push(c);
                }
            }
        }
        KeyCode::Backspace => {
            if let Mode::TestInput { vars, focused, iterations, .. } = &mut app.mode {
                if *focused < vars.len() {
                    vars[*focused].1.pop();
                } else {
                    iterations.pop();
                }
            }
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_response(app: &mut App, code: KeyCode) -> bool {
    use super::render::format_test_results_text;
    use super::render::copy_to_clipboard;
    let was_pending_g = app.pending_g;
    app.pending_g = false;
    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Esc => {
            app.mode = Mode::Browse;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Mode::Response { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_add(1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Mode::Response { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_sub(1);
            }
        }
        KeyCode::Char('g') => {
            if was_pending_g {
                if let Mode::Response { scroll, .. } = &mut app.mode {
                    *scroll = 0;
                }
            } else {
                app.pending_g = true;
            }
        }
        KeyCode::Char('G') => {
            let view_height = app.response_view_height;
            if let Mode::Response { scroll, body, response_filter, .. } = &mut app.mode {
                let q = response_filter.to_lowercase();
                let line_count = body
                    .lines()
                    .filter(|line| q.is_empty() || line.to_lowercase().contains(&q))
                    .count()
                    .min(u16::MAX as usize) as u16;
                *scroll = line_count.saturating_sub(view_height);
            }
        }
        KeyCode::Char('c') => {
            let text = match &app.mode {
                Mode::Response { body, .. } => body.clone(),
                Mode::TestResponse { results } => format_test_results_text(results),
                _ => return false,
            };
            if let Err(e) = copy_to_clipboard(&text) {
                app.show_message(ResponseKind::Error, format!("Copy failed: {e}"));
            }
        }
        KeyCode::Char('f') => {
            if let Mode::Response { response_filter_active, .. } = &mut app.mode {
                *response_filter_active = true;
            }
        }
        _ => {}
    }
    false
}
