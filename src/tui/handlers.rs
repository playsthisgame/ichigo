use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::fs;
use crate::config::{dir_for, prune_empty_parents, global_config_path, local_config_path};
use super::edit::{self, Applied, Edit};
use super::{App, Focus, Mode, PairKind, PendingAction, RequestDraft, ResponseKind, apply_body, body_field, pair_field, profile_field, step_row};
use super::tree::{VisibleRow, visible_rows, build_tree, load_entries};

pub(super) fn handle_key_new_request(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        // Tab covers the action rows too, so global/headers/query/profiles are
        // found by walking the form rather than by knowing a chord.
        KeyCode::Tab => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                draft.step_focus(true);
            }
        }
        KeyCode::BackTab => {
            if let Mode::NewRequest { draft, .. } = &mut app.mode {
                draft.step_focus(false);
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
        // The body. `Ctrl+b` is the mnemonic and, under tmux, the prefix key —
        // which is a reason to be sure the row exists, not a reason to pick a
        // worse letter: a chord tmux swallows costs a tmux user nothing, since
        // Tab to the `body` row is the path nothing intercepts. It is `Ctrl+h`
        // that could not be the only binding, because that one *arrives* as a
        // different key rather than not arriving at all.
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Mode::NewRequest { draft, .. } = &app.mode {
                app.mode = Mode::edit_body(draft.clone());
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
                Focus::Headers | Focus::Query | Focus::Body | Focus::Profiles => {
                    let Mode::NewRequest { draft, .. } = &app.mode else { return false };
                    let draft = draft.clone();
                    app.mode = match target {
                        Focus::Headers => Mode::edit_pairs(PairKind::Headers, draft),
                        Focus::Query => Mode::edit_pairs(PairKind::Query, draft),
                        Focus::Body => Mode::edit_body(draft),
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
            // A second field borrow, disjoint from `app.mode`: the register has
            // to outlive the field it is put into, so it cannot live on the
            // `Edit` the way the undo history does.
            let register = &mut app.register;
            let mut leaving = false;
            if let Mode::NewRequest { draft, error } = &mut app.mode {
                match draft.focus_target() {
                    Focus::Field(i) => {
                        match edit::apply(
                            &mut draft.fields[i].1,
                            &mut draft.edit,
                            key,
                            keys,
                            register,
                        ) {
                            Applied::Yes => *error = None,
                            Applied::Exit => leaving = true,
                            // The normal-mode walk between rows. Not an error
                            // clear: moving the focus does not answer whatever
                            // the last save complained about.
                            Applied::FocusNext => draft.step_focus(true),
                            Applied::FocusPrev => draft.step_focus(false),
                            Applied::No => {}
                        }
                    }
                    // No text under the caret, so no insert mode to drop out of
                    // first: one Esc leaves the form here, where a field takes
                    // two. `j`/`k` still walk — an action row has nothing to
                    // type into, and without them `j` onto `global` would be a
                    // one-way trip out of the walk. Every other key is inert.
                    _ => match walk_key(key) {
                        Some(forward) => draft.step_focus(forward),
                        None => leaving = key.code == KeyCode::Esc,
                    },
                }
            }
            // Not straight to Browse: a draft with unsaved changes gets the
            // discard prompt first, since Esc is the only way out of the form
            // and everything the sub-panes edited lives in the draft alone.
            if leaving {
                app.leave_form();
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
                step_pairs(pairs, focused, edit, true);
            }
        }
        KeyCode::BackTab => {
            if let Mode::EditPairs { pairs, focused, edit, .. } = &mut app.mode {
                step_pairs(pairs, focused, edit, false);
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
                    let insert = edit.insert;
                    *edit = caret_for(pair_field(pairs, *focused), insert);
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
            let register = &mut app.register;
            let mut leaving = None;
            if let Mode::EditPairs { draft, pairs, focused, edit, error, .. } = &mut app.mode {
                // `focused` stays a `&mut` so the walk arms below can move it;
                // `idx` is the copy the field lookup needs.
                let idx = *focused;
                if let Some(value) = pair_field(pairs, idx) {
                    match edit::apply(value, edit, key, keys, register) {
                        Applied::Yes => *error = None,
                        // Back to the form without saving: these edits live
                        // in the draft until the request itself is written.
                        Applied::Exit => leaving = Some(draft.clone()),
                        Applied::FocusNext => step_pairs(pairs, focused, edit, true),
                        Applied::FocusPrev => step_pairs(pairs, focused, edit, false),
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

/// The draft's body: the content type on row 0, the data on row 1.
///
/// `Ctrl+s` applies and Enter types a newline — the cURL import buffer's
/// contract rather than `EditPairs`'s, and for its reason: a terminal without
/// bracketed paste delivers a pasted body as characters with Enters between the
/// lines, so an Enter that applied would keep the first line and scatter the
/// rest. On the content-type row, where a newline means nothing, Enter steps
/// down to the body instead, so typing straight through the pane works.
///
/// Edits land in the draft only when `Ctrl+s` succeeds; Esc drops them, and the
/// request itself is still unwritten until saved from the form.
pub(super) fn handle_key_edit_body(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Tab => step_body(app, true),
        KeyCode::BackTab => step_body(app, false),
        KeyCode::Char('s') if ctrl => {
            let taken = match &app.mode {
                Mode::EditBody { draft, content_type, data, .. } => {
                    Some((draft.clone(), content_type.clone(), data.clone()))
                }
                _ => None,
            };
            let Some((mut draft, content_type, data)) = taken else { return false };
            match apply_body(&mut draft, &content_type, &data) {
                Ok(()) => app.mode = Mode::NewRequest { draft, error: None },
                Err(message) => {
                    if let Mode::EditBody { error, .. } = &mut app.mode {
                        *error = Some(message);
                    }
                }
            }
        }
        // Enter on the content type is a step down, not an apply: the row below
        // is where Enter types a newline, and one key meaning "save" on one row
        // and "newline" on the next is the trap this pane exists inside of.
        KeyCode::Enter if matches!(&app.mode, Mode::EditBody { focused: 0, .. }) => {
            step_body(app, true);
        }
        _ => {
            let keys = app.keys;
            let register = &mut app.register;
            let mut leaving = None;
            if let Mode::EditBody { draft, content_type, data, focused, edit, error } = &mut app.mode {
                let value = body_field(content_type, data, *focused);
                // The data row is the one field in the TUI that may hold
                // newlines, so it is the one that goes through `apply_multiline`
                // — where Enter types one and `j`/`k` move by line before they
                // become a row walk.
                let applied = if *focused == 0 {
                    edit::apply(value, edit, key, keys, register)
                } else {
                    edit::apply_multiline(value, edit, key, keys, register)
                };
                match applied {
                    Applied::Yes => *error = None,
                    // Back to the form without applying: these edits live in
                    // the draft until the request itself is written.
                    Applied::Exit => leaving = Some(draft.clone()),
                    Applied::FocusNext => step_body_at(content_type, data, focused, edit, true),
                    Applied::FocusPrev => step_body_at(content_type, data, focused, edit, false),
                    Applied::No => {}
                }
            }
            if let Some(draft) = leaving {
                app.mode = Mode::NewRequest { draft, error: None };
            }
        }
    }
    false
}

fn step_body(app: &mut App, forward: bool) {
    if let Mode::EditBody { content_type, data, focused, edit, .. } = &mut app.mode {
        step_body_at(content_type, data, focused, edit, forward);
    }
}

/// One of the pane's two rows to the other, re-anchoring the caret and keeping
/// the mode — the same obligation every focus move in the TUI carries.
fn step_body_at(
    content_type: &mut String,
    data: &mut String,
    focused: &mut usize,
    edit: &mut Edit,
    forward: bool,
) {
    *focused = step_row(*focused, 2, forward);
    let insert = edit.insert;
    *edit = Edit::landing(body_field(content_type, data, *focused), insert);
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
                step_profile(name, params, focused, edit, true);
            }
        }
        KeyCode::BackTab => {
            if let Mode::NewProfile { name, params, focused, edit, .. } = &mut app.mode {
                step_profile(name, params, focused, edit, false);
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
                let insert = edit.insert;
                *edit = caret_for(profile_field(name, params, *focused), insert);
                *error = None;
            }
        }
        KeyCode::Enter => {
            app.save_new_profile();
        }
        _ => {
            let keys = app.keys;
            let register = &mut app.register;
            let mut leaving = None;
            if let Mode::NewProfile { draft, editing, name, params, focused, edit, error } =
                &mut app.mode
            {
                let idx = *focused;
                if let Some(value) = profile_field(name, params, idx) {
                    match edit::apply(value, edit, key, keys, register) {
                        Applied::Yes => *error = None,
                        // Back to the list without saving: `selected` returns to
                        // the profile being edited, or to the "new" row when
                        // adding one.
                        Applied::Exit => {
                            leaving = Some((draft.clone(), editing.unwrap_or(draft.profiles.len())))
                        }
                        Applied::FocusNext => step_profile(name, params, focused, edit, true),
                        Applied::FocusPrev => step_profile(name, params, focused, edit, false),
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

/// The caret for a field that just took focus — at its end in `insert`'s mode,
/// or a fresh one when the index no longer points at a field at all.
///
/// The mode is carried across the move for the reason `RequestDraft::focus`
/// spells out: a walk is not a decision about typing, and a `j`/`k` walk that
/// landed in insert mode would type its next key into the field.
fn caret_for(value: Option<&mut String>, insert: bool) -> Edit {
    value.map_or_else(Edit::default, |v| Edit::landing(v, insert))
}

/// `j`/`k` read as a row walk on a row that has no text to edit.
///
/// `edit::apply` reports the same two keys as `Applied::FocusNext`/`FocusPrev`
/// for a field, and this is the same rule where there is no field: the action
/// rows of the request form. Modified keys are refused here exactly as they are
/// there, so an unbound `Ctrl+j` does not silently move the focus.
fn walk_key(key: KeyEvent) -> Option<bool> {
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Char('j') => Some(true),
        KeyCode::Char('k') => Some(false),
        _ => None,
    }
}

/// One step of the `Mode::EditPairs` walk: two fields per row, wrapping, and
/// inert on an empty list. Tab, BackTab, and the normal-mode `j`/`k` share it.
fn step_pairs(pairs: &mut [(String, String)], focused: &mut usize, edit: &mut Edit, forward: bool) {
    let total = 2 * pairs.len();
    if total == 0 {
        return;
    }
    let insert = edit.insert;
    *focused = step_row(*focused, total, forward);
    *edit = caret_for(pair_field(pairs, *focused), insert);
}

/// One step of the `Mode::NewProfile` walk: the name row, then two fields per
/// param.
fn step_profile(
    name: &mut String,
    params: &mut [(String, String)],
    focused: &mut usize,
    edit: &mut Edit,
    forward: bool,
) {
    let total = 1 + 2 * params.len();
    let insert = edit.insert;
    *focused = step_row(*focused, total, forward);
    *edit = caret_for(profile_field(name, params, *focused), insert);
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
                step_vars(vars, focused, edit, true);
            }
        }
        KeyCode::BackTab => {
            if let Mode::VarInput { vars, focused, edit, .. } = &mut app.mode {
                step_vars(vars, focused, edit, false);
            }
        }
        _ => {
            let keys = app.keys;
            let register = &mut app.register;
            let mut leaving = false;
            if let Mode::VarInput { vars, focused, edit, .. } = &mut app.mode {
                let idx = *focused;
                match edit::apply(&mut vars[idx].1, edit, key, keys, register) {
                    Applied::Exit => leaving = true,
                    Applied::FocusNext => step_vars(vars, focused, edit, true),
                    Applied::FocusPrev => step_vars(vars, focused, edit, false),
                    Applied::Yes | Applied::No => {}
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
                step_test(vars, iterations, focused, edit, true);
            }
        }
        KeyCode::BackTab => {
            if let Mode::TestInput { vars, focused, iterations, edit, .. } = &mut app.mode {
                step_test(vars, iterations, focused, edit, false);
            }
        }
        _ => {
            let keys = app.keys;
            let register = &mut app.register;
            let mut leaving = false;
            if let Mode::TestInput { vars, focused, iterations, edit, .. } = &mut app.mode {
                let idx = *focused;
                // The iterations field only ever accepts digits — but only in
                // insert mode, or the motions would be filtered out with them.
                let typing_non_digit = edit.insert
                    && matches!(key.code, KeyCode::Char(c) if !c.is_ascii_digit());
                let value = if idx < vars.len() {
                    Some(&mut vars[idx].1)
                } else if typing_non_digit {
                    None
                } else {
                    Some(&mut *iterations)
                };
                if let Some(value) = value {
                    match edit::apply(value, edit, key, keys, register) {
                        Applied::Exit => leaving = true,
                        Applied::FocusNext => {
                            step_test(vars, iterations, focused, edit, true)
                        }
                        Applied::FocusPrev => {
                            step_test(vars, iterations, focused, edit, false)
                        }
                        Applied::Yes | Applied::No => {}
                    }
                }
            }
            if leaving {
                app.mode = Mode::Browse;
            }
        }
    }
    false
}

/// One step of the `Mode::VarInput` walk. Inert on an empty list, which cannot
/// happen — the form only opens when a request has placeholders — but `% 0`
/// panics, and a guard is cheaper than that guarantee staying true.
fn step_vars(
    vars: &[(String, String)],
    focused: &mut usize,
    edit: &mut Edit,
    forward: bool,
) {
    if vars.is_empty() {
        return;
    }
    *focused = step_row(*focused, vars.len(), forward);
    *edit = Edit::landing(&vars[*focused].1, edit.insert);
}

/// One step of the `Mode::TestInput` walk: the variables, then the iteration
/// count.
fn step_test(
    vars: &[(String, String)],
    iterations: &str,
    focused: &mut usize,
    edit: &mut Edit,
    forward: bool,
) {
    *focused = step_row(*focused, vars.len() + 1, forward);
    *edit = Edit::landing(test_field(vars, iterations, *focused), edit.insert);
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
            if let Mode::Response { scroll, cursor, body, headers, show_headers, response_filter, .. } =
                &mut app.mode
            {
                let text = super::response_text(headers, *show_headers, body);
                let count = super::visible_response_lines(&text, response_filter).len();
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
                Mode::Response { body, headers, show_headers, response_filter, cursor, anchor, .. } => {
                    let text = super::response_text(headers, *show_headers, body);
                    let lines = super::visible_response_lines(&text, response_filter);
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
                Mode::Response { body, headers, show_headers, response_filter, .. } => {
                    let text = super::response_text(headers, *show_headers, body);
                    super::visible_response_lines(&text, response_filter).join("\n")
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
        // Toggles the response's headers in above the body. Inert when there
        // are none — an error pane, a generated cURL command, a chain — rather
        // than toggling an empty block nobody can see.
        KeyCode::Char('H') => {
            if let Mode::Response { headers, show_headers, scroll, cursor, anchor, .. } =
                &mut app.mode
                && !headers.is_empty()
            {
                *show_headers = !*show_headers;
                // The line list just changed under them, so they are reset for
                // the same reason an edit to the filter resets them: carrying
                // them over leaves the cursor on an unrelated line and a
                // selection spanning lines the user never saw.
                *scroll = 0;
                *cursor = 0;
                *anchor = None;
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
    let Mode::Response { body, headers, show_headers, response_filter, cursor, scroll, .. } =
        &mut app.mode
    else {
        return;
    };
    let text = super::response_text(headers, *show_headers, body);
    let count = super::visible_response_lines(&text, response_filter).len();
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
