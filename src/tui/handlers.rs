use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::fs;
use crate::config::{dir_for, prune_empty_parents, global_config_path, local_config_path};
use super::edit::{self, Applied, Edit};
use super::{App, Focus, Mode, PairKind, PendingAction, RequestDraft, ResponseKind, pair_field, profile_field};
use super::tree::{VisibleRow, visible_rows, build_tree, load_entries};

pub(super) fn handle_key_new_request(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        // Tab covers the action rows too, so global/headers/query/profiles are
        // found by walking the form rather than by knowing a chord.
        KeyCode::Tab => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                let next = (draft.focused + 1) % draft.rows();
                draft.focus(next);
            }
        }
        KeyCode::BackTab => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                let rows = draft.rows();
                let prev = draft.focused.checked_sub(1).unwrap_or(rows - 1);
                draft.focus(prev);
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
        // Ctrl+e is the documented key. Ctrl+h is kept because it is the better
        // mnemonic, but it cannot be the only one: plenty of terminals and tmux
        // configs bind Ctrl+H to backward-delete-char and send 0x7F, which
        // arrives as a plain Backspace and silently deletes a character
        // instead.
        KeyCode::Char('e' | 'h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewRequest { draft, .. } = &app.mode {
                app.mode = Mode::edit_pairs(PairKind::Headers, draft.clone());
            }
        }
        // Query params. Ctrl+q is the mnemonic, and unlike Ctrl+h it is safe to
        // press: raw mode clears IXON, so the terminal driver no longer eats it
        // as XON/XOFF flow control. It is still only an accelerator — the Tab
        // row is the path that nothing can intercept, which is why the query
        // pane, like headers, is reachable without knowing this chord at all.
        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewRequest { draft, .. } = &app.mode {
                app.mode = Mode::edit_pairs(PairKind::Query, draft.clone());
            }
        }
        // Enter saves from a text field and does the row's thing on an action
        // row. Two meanings for one key, but the row under focus says which,
        // and a form whose only Enter saved would leave the action rows inert.
        KeyCode::Enter => {
            let target = match &app.mode {
                Mode::NewRequest { draft, .. } => draft.focus_target(),
                _ => return false,
            };
            match target {
                Focus::Field(_) => app.save_new_request(),
                Focus::Global => {
                    if let Mode::NewRequest { draft, .. } = &mut app.mode {
                        draft.global = !draft.global;
                    }
                }
                // Cloned out first: the new mode owns the draft, so it cannot
                // be built while `app.mode` is still borrowed.
                Focus::Headers | Focus::Query | Focus::Profiles => {
                    let Mode::NewRequest { draft, .. } = &app.mode else { return false };
                    let draft = draft.clone();
                    app.mode = match target {
                        Focus::Headers => Mode::edit_pairs(PairKind::Headers, draft),
                        Focus::Query => Mode::edit_pairs(PairKind::Query, draft),
                        // 0 is the first profile, or the "new" row when empty.
                        _ => Mode::ProfileList { draft, selected: 0 },
                    };
                }
            }
        }
        // Everything else is text editing in the focused field. `edit::apply`
        // owns the Char/Backspace/motion arms, including refusing Ctrl+<letter>
        // — crossterm reports those as Char + CONTROL, so without that refusal
        // an unbound Ctrl+h would type a literal 'h'.
        _ => {
            // Copied out before `app.mode` is borrowed: the two are separate
            // fields, so Rust can split-borrow them, but not through one `&mut`.
            let keys = app.keys;
            let mut leaving = false;
            if let Mode::NewRequest { draft, error } = &mut app.mode {
                match draft.focus_target() {
                    Focus::Field(i) => {
                        match edit::apply(&mut draft.fields[i].1, &mut draft.edit, key, keys) {
                            Applied::Yes => *error = None,
                            Applied::Exit => leaving = true,
                            Applied::No => {}
                        }
                    }
                    // No text under the caret, so no insert mode to drop out of
                    // first: one Esc leaves the form here, where a field takes
                    // two. Every other key is inert rather than typed.
                    _ => leaving = key.code == KeyCode::Esc,
                }
            }
            if leaving {
                app.mode = Mode::Browse;
            }
        }
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

/// The draft's headers. Two focusable fields per header — `2i` is the name,
/// `2i+1` the value — so Tab walks name, value, name, value.
///
/// Edits land in the draft only when Enter succeeds; Esc drops them, and the
/// request itself is still unwritten until saved from the form.
pub(super) fn handle_key_edit_pairs(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Tab => {
            if let Mode::EditPairs { pairs, focused, edit, .. } = &mut app.mode {
                let total = 2 * pairs.len();
                if total > 0 {
                    *focused = (*focused + 1) % total;
                    *edit = caret_for(pair_field(pairs, *focused));
                }
            }
        }
        KeyCode::BackTab => {
            if let Mode::EditPairs { pairs, focused, edit, .. } = &mut app.mode {
                let total = 2 * pairs.len();
                if total > 0 {
                    *focused = focused.checked_sub(1).unwrap_or(total - 1);
                    *edit = caret_for(pair_field(pairs, *focused));
                }
            }
        }
        KeyCode::Char('a') if ctrl => {
            if let Mode::EditPairs { pairs, focused, edit, error, .. } = &mut app.mode {
                *focused = 2 * pairs.len();
                pairs.push((String::new(), String::new()));
                *edit = Edit::default();
                *error = None;
            }
        }
        KeyCode::Char('d') if ctrl => {
            if let Mode::EditPairs { pairs, focused, edit, error, .. } = &mut app.mode {
                let idx = *focused / 2;
                if idx < pairs.len() {
                    pairs.remove(idx);
                    // The list just got shorter; keep focus on a field that
                    // still exists (0 when the last header is gone).
                    *focused = (*focused).min((2 * pairs.len()).saturating_sub(1));
                    *edit = caret_for(pair_field(pairs, *focused));
                    *error = None;
                }
            }
        }
        KeyCode::Enter => {
            let taken = match &app.mode {
                Mode::EditPairs { kind, draft, pairs, .. } => {
                    Some((*kind, draft.clone(), pairs.clone()))
                }
                _ => None,
            };
            let Some((kind, mut draft, pairs)) = taken else { return false };
            match kind.apply(&mut draft, pairs) {
                Ok(()) => app.mode = Mode::NewRequest { draft, error: None },
                Err(message) => {
                    if let Mode::EditPairs { error, .. } = &mut app.mode {
                        *error = Some(message);
                    }
                }
            }
        }
        _ => {
            let keys = app.keys;
            let mut leaving = None;
            if let Mode::EditPairs { draft, pairs, focused, edit, error, .. } = &mut app.mode {
                let focused = *focused;
                if let Some(value) = pair_field(pairs, focused) {
                    match edit::apply(value, edit, key, keys) {
                        Applied::Yes => *error = None,
                        // Back to the form without saving: these edits live
                        // in the draft until the request itself is written.
                        Applied::Exit => leaving = Some(draft.clone()),
                        Applied::No => {}
                    }
                } else if matches!(key.code, KeyCode::Esc) {
                    // An empty list has no field to hold a mode, so Esc
                    // leaves straight away rather than waiting for a second one.
                    leaving = Some(draft.clone());
                }
            }
            if let Some(draft) = leaving {
                app.mode = Mode::NewRequest { draft, error: None };
            }
        }
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
            app.mode = Mode::new_profile(draft.clone(), None, String::new(), Vec::new());
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
            // A profile's params are a HashMap, which has no order of its own;
            // sort so the fields do not shuffle between edits.
            let picked = draft.profiles.get(idx).map(|profile| {
                let mut params: Vec<(String, String)> = profile
                    .params
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                params.sort_by(|a, b| a.0.cmp(&b.0));
                (profile.name.clone(), params)
            });
            app.mode = match picked {
                Some((name, params)) => Mode::new_profile(draft, Some(idx), name, params),
                None => Mode::new_profile(draft, None, String::new(), Vec::new()),
            };
        }
        _ => {}
    }
    false
}

pub(super) fn handle_key_new_profile(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Tab => {
            if let Mode::NewProfile { name, params, focused, edit, .. } = &mut app.mode {
                let total = 1 + 2 * params.len();
                *focused = (*focused + 1) % total.max(1);
                *edit = caret_for(profile_field(name, params, *focused));
            }
        }
        KeyCode::BackTab => {
            if let Mode::NewProfile { name, params, focused, edit, .. } = &mut app.mode {
                let total = 1 + 2 * params.len();
                *focused = focused.checked_sub(1).unwrap_or(total.saturating_sub(1));
                *edit = caret_for(profile_field(name, params, *focused));
            }
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewProfile { params, focused, edit, .. } = &mut app.mode {
                *focused = 1 + 2 * params.len();
                params.push((String::new(), String::new()));
                *edit = Edit::default();
            }
        }
        // The counterpart to `Ctrl+a`, and the same chord `EditPairs` uses to
        // drop a row. Inert on the name field, which is not a param and is the
        // one row a profile cannot do without.
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewProfile { name, params, focused, edit, error, .. } = &mut app.mode
                && let Some(idx) = focused.checked_sub(1).map(|offset| offset / 2)
                && idx < params.len()
            {
                params.remove(idx);
                // The list just got shorter; keep focus on a field that still
                // exists (the name row when the last param is gone).
                *focused = (*focused).min(2 * params.len());
                *edit = caret_for(profile_field(name, params, *focused));
                *error = None;
            }
        }
        KeyCode::Enter => {
            app.save_new_profile();
        }
        _ => {
            let keys = app.keys;
            let mut leaving = None;
            if let Mode::NewProfile { draft, editing, name, params, focused, edit, error } =
                &mut app.mode
            {
                let focused = *focused;
                if let Some(value) = profile_field(name, params, focused) {
                    match edit::apply(value, edit, key, keys) {
                        Applied::Yes => *error = None,
                        // Back to the list without saving: `selected` returns to
                        // the profile being edited, or to the "new" row when
                        // adding one.
                        Applied::Exit => {
                            leaving = Some((draft.clone(), editing.unwrap_or(draft.profiles.len())))
                        }
                        Applied::No => {}
                    }
                }
            }
            if let Some((draft, selected)) = leaving {
                app.mode = Mode::ProfileList { draft, selected };
            }
        }
    }
    false
}

/// The caret for a field that just took focus — at its end, or a fresh one when
/// the index no longer points at a field at all.
fn caret_for(value: Option<&mut String>) -> Edit {
    value.map_or_else(Edit::default, |v| Edit::at_end(v))
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
        // Headers and profiles are deliberately *not* bound here. They are
        // parts of a request, not things to do to one, so they are reached by
        // Tabbing to their row inside the form — one door instead of two, and
        // no way to edit a request's headers without seeing the request.
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
    if let Mode::Response {
        response_filter, response_filter_active, scroll, cursor, anchor, ..
    } = &mut app.mode
    {
        // Every edit to the filter changes which lines are visible, and the
        // cursor and anchor index *those*. Carrying them over would leave the
        // cursor on an unrelated line and a selection spanning lines the user
        // never saw, so both reset with the view.
        let mut reset = || {
            *scroll = 0;
            *cursor = 0;
            *anchor = None;
        };
        match key.code {
            KeyCode::Esc => {
                response_filter.clear();
                *response_filter_active = false;
                reset();
            }
            KeyCode::Enter => {
                *response_filter_active = false;
            }
            KeyCode::Char(c) => {
                response_filter.push(c);
                reset();
            }
            KeyCode::Backspace => {
                response_filter.pop();
                reset();
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
            if let Mode::VarInput { vars, focused, edit, .. } = &mut app.mode {
                let len = vars.len();
                *focused = (*focused + 1) % len;
                *edit = Edit::at_end(&vars[*focused].1);
            }
        }
        KeyCode::BackTab => {
            if let Mode::VarInput { vars, focused, edit, .. } = &mut app.mode {
                let len = vars.len();
                *focused = focused.checked_sub(1).unwrap_or(len - 1);
                *edit = Edit::at_end(&vars[*focused].1);
            }
        }
        _ => {
            let keys = app.keys;
            let mut leaving = false;
            if let Mode::VarInput { vars, focused, edit, .. } = &mut app.mode {
                let focused = *focused;
                if let Applied::Exit = edit::apply(&mut vars[focused].1, edit, key, keys) {
                    leaving = true;
                }
            }
            if leaving {
                app.mode = Mode::Browse;
            }
        }
    }
    false
}

pub(super) fn handle_key_test_input(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
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
            if let Mode::TestInput { vars, focused, iterations, edit, .. } = &mut app.mode {
                let len = vars.len() + 1;
                *focused = (*focused + 1) % len;
                *edit = Edit::at_end(test_field(vars, iterations, *focused));
            }
        }
        KeyCode::BackTab => {
            if let Mode::TestInput { vars, focused, iterations, edit, .. } = &mut app.mode {
                let len = vars.len() + 1;
                *focused = focused.checked_sub(1).unwrap_or(len - 1);
                *edit = Edit::at_end(test_field(vars, iterations, *focused));
            }
        }
        _ => {
            let keys = app.keys;
            let mut leaving = false;
            if let Mode::TestInput { vars, focused, iterations, edit, .. } = &mut app.mode {
                let focused = *focused;
                // The iterations field only ever accepts digits — but only in
                // insert mode, or the motions would be filtered out with them.
                let typing_non_digit = edit.insert
                    && matches!(key.code, KeyCode::Char(c) if !c.is_ascii_digit());
                let value = if focused < vars.len() {
                    Some(&mut vars[focused].1)
                } else if typing_non_digit {
                    None
                } else {
                    Some(iterations)
                };
                if let Some(value) = value
                    && let Applied::Exit = edit::apply(value, edit, key, keys)
                {
                    leaving = true;
                }
            }
            if leaving {
                app.mode = Mode::Browse;
            }
        }
    }
    false
}

/// The `Mode::TestInput` field a focus index points at: the variables first,
/// then the iteration count.
fn test_field<'a>(
    vars: &'a [(String, String)],
    iterations: &'a str,
    focused: usize,
) -> &'a str {
    vars.get(focused).map_or(iterations, |(_, value)| value.as_str())
}

pub(super) fn handle_key_response(app: &mut App, code: KeyCode) -> bool {
    use super::render::format_test_results_text;
    use super::render::copy_to_clipboard;
    let was_pending_g = app.pending_g;
    app.pending_g = false;
    let view_height = app.response_view_height;

    // Feedback from the previous key has been read by now.
    if let Mode::Response { status, .. } = &mut app.mode {
        *status = None;
    }

    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Esc => {
            // Esc drops a selection first and leaves the pane only once there
            // is none, so cancelling a mis-started selection does not also
            // throw away the response it was made in.
            let selecting = matches!(app.mode, Mode::Response { anchor: Some(_), .. });
            if selecting {
                if let Mode::Response { anchor, .. } = &mut app.mode {
                    *anchor = None;
                }
            } else {
                app.mode = Mode::Browse;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => move_cursor(app, 1, view_height),
        KeyCode::Char('k') | KeyCode::Up => move_cursor(app, -1, view_height),
        KeyCode::Char('g') => {
            if was_pending_g {
                if let Mode::Response { scroll, cursor, .. } = &mut app.mode {
                    *cursor = 0;
                    *scroll = 0;
                }
            } else {
                app.pending_g = true;
            }
        }
        KeyCode::Char('G') => {
            if let Mode::Response { scroll, cursor, body, response_filter, .. } = &mut app.mode {
                let count = super::visible_response_lines(body, response_filter).len();
                *cursor = count.saturating_sub(1);
                let last = count.min(u16::MAX as usize) as u16;
                *scroll = last.saturating_sub(view_height);
            }
        }
        // Start or drop a line selection at the cursor.
        KeyCode::Char('V') => {
            if let Mode::Response { anchor, cursor, .. } = &mut app.mode {
                *anchor = match anchor {
                    Some(_) => None,
                    None => Some(*cursor),
                };
            }
        }
        KeyCode::Char('y') => {
            let picked = match &app.mode {
                Mode::Response { body, response_filter, cursor, anchor, .. } => {
                    let lines = super::visible_response_lines(body, response_filter);
                    let (from, to) = super::selection_range(*cursor, *anchor);
                    lines
                        .get(from..=to.min(lines.len().saturating_sub(1)))
                        .map(|slice| (slice.join("\n"), slice.len()))
                }
                _ => None,
            };
            let Some((text, count)) = picked else { return false };
            match copy_to_clipboard(&text) {
                Ok(()) => {
                    if let Mode::Response { anchor, status, .. } = &mut app.mode {
                        *anchor = None;
                        *status = Some(format!(
                            "copied {} line{}",
                            count,
                            if count == 1 { "" } else { "s" }
                        ));
                    }
                }
                Err(e) => app.show_message(ResponseKind::Error, format!("Copy failed: {e}")),
            }
        }
        // Copies what the pane shows, so a filtered view copies the lines you
        // can see rather than the whole body behind them.
        KeyCode::Char('c') => {
            let text = match &app.mode {
                Mode::Response { body, response_filter, .. } => {
                    super::visible_response_lines(body, response_filter).join("\n")
                }
                Mode::TestResponse { results } => format_test_results_text(results),
                _ => return false,
            };
            match copy_to_clipboard(&text) {
                Ok(()) => {
                    if let Mode::Response { status, response_filter, .. } = &mut app.mode {
                        *status = Some(if response_filter.is_empty() {
                            "copied response".to_string()
                        } else {
                            "copied filtered lines".to_string()
                        });
                    }
                }
                Err(e) => app.show_message(ResponseKind::Error, format!("Copy failed: {e}")),
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

/// Moves the response cursor by `delta`, scrolling only far enough to keep it
/// on screen. The pane scrolls to follow the cursor rather than the other way
/// round, so a selection cannot be extended past the edge of the view.
fn move_cursor(app: &mut App, delta: isize, view_height: u16) {
    let Mode::Response { body, response_filter, cursor, scroll, .. } = &mut app.mode else {
        return;
    };
    let count = super::visible_response_lines(body, response_filter).len();
    if count == 0 {
        return;
    }
    let last = count - 1;
    *cursor = cursor.saturating_add_signed(delta).min(last);

    let view = view_height.max(1) as usize;
    let top = *scroll as usize;
    if *cursor < top {
        *scroll = *cursor as u16;
    } else if *cursor >= top + view {
        *scroll = (*cursor + 1 - view).min(u16::MAX as usize) as u16;
    }
}
