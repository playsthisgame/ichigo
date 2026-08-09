use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    symbols,
    widgets::{Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use super::edit::Edit;
use super::{App, Mode, ResponseKind};
use super::tree::{Entry, EntryKind, VisibleRow, visible_rows};

/// One text field's value with the caret drawn where it sits.
///
/// Insert mode gets a thin bar between characters, normal mode a block *over*
/// the character it is on — the same shapes a terminal cursor takes, which is
/// the only signal of which mode a field is in. Nothing else says so, and
/// nothing else needs to.
fn field_spans(value: &str, edit: Option<&Edit>, color: Color) -> Vec<Span<'static>> {
    let text = Style::default().fg(color);
    let Some(edit) = edit else {
        return vec![Span::styled(value.to_string(), text)];
    };

    // The caret can outlive the value it was measured against — a draft that
    // went through another pane, a row deleted at the focused index — so it is
    // clamped here rather than trusted.
    let caret = value
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(value.len()))
        .take_while(|i| *i <= edit.caret)
        .last()
        .unwrap_or(0);
    let (before, after) = value.split_at(caret);
    let cursor = Style::default().fg(Color::Black).bg(Color::White);

    let mut spans = vec![Span::styled(before.to_string(), text)];
    match (edit.insert, after.chars().next()) {
        (true, _) => {
            spans.push(Span::styled("│", Style::default().fg(Color::White)));
            spans.push(Span::styled(after.to_string(), text));
        }
        // Normal mode past the end of the line happens only on an empty value;
        // `Edit` pulls the caret back onto a character otherwise.
        (false, None) => spans.push(Span::styled("█", Style::default().fg(Color::White))),
        (false, Some(under)) => {
            spans.push(Span::styled(under.to_string(), cursor));
            spans.push(Span::styled(after[under.len_utf8()..].to_string(), text));
        }
    }
    spans
}

/// The caret to draw in a field: `Some` only for the focused one.
fn caret(is_focused: bool, edit: &Edit) -> Option<&Edit> {
    is_focused.then_some(edit)
}

/// A `  > value` input row, dimmed unless it is the one holding the caret.
fn input_line(value: &str, caret: Option<&Edit>) -> Line<'static> {
    let color = if caret.is_some() { Color::White } else { Color::DarkGray };
    let mut spans = vec![Span::styled("  > ", Style::default().fg(Color::DarkGray))];
    spans.extend(field_spans(value, caret, color));
    Line::from(spans)
}

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let full = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(full);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[0]);

    let filtered = app.filtered_indices();
    let tree_rows_storage: Option<Vec<VisibleRow>> = if app.using_tree() {
        Some(visible_rows(&app.tree, &app.collapsed_folders))
    } else {
        None
    };
    let tree_rows = tree_rows_storage.as_deref();
    draw_list(frame, panes[0], &app.entries, tree_rows, &filtered, &mut app.list_state, &app.filter, app.filter_active, app.use_nerd_fonts);

    let mut response_view_height = None;
    match &app.mode {
        Mode::Browse => {
            // Accessing app.list_state and app.entries here is fine: they are
            // different fields from app.mode, and Rust can split-borrow them.
            let selected = app.selected_entry_index().map(|i| &app.entries[i]);
            draw_detail_browse(frame, panes[1], selected);
        }
        Mode::ProfileSelect { profiles, selected, entry_name, .. } => {
            draw_profile_select(frame, panes[1], profiles, *selected, entry_name);
        }
        Mode::VarInput { vars, focused, edit, entry_name, .. } => {
            draw_var_input(frame, panes[1], vars, *focused, edit, entry_name);
        }
        Mode::TestInput { vars, focused, iterations, edit, entry_name } => {
            draw_test_input(frame, panes[1], vars, iterations, *focused, edit, entry_name);
        }
        Mode::Response { kind, body, scroll, response_filter, response_filter_active, cursor, anchor, status } => {
            response_view_height = Some(draw_response(
                frame, panes[1], kind, body, *scroll, response_filter,
                *response_filter_active, *cursor, *anchor, status.as_deref(),
            ));
        }
        Mode::TestResponse { results } => {
            draw_test_results(frame, panes[1], results);
        }
        Mode::NewRequest { draft, error } => {
            draw_new_request(frame, panes[1], &draft.fields, draft.focused, &draft.edit, &draft.profiles, draft.headers(), draft.original_name.is_some(), draft.global, error.as_deref());
        }
        Mode::ImportCurl { buffer, error } => {
            draw_import_curl(frame, panes[1], buffer, error.as_deref());
        }
        Mode::EditHeaders { pairs, focused, edit, error, .. } => {
            draw_edit_headers(frame, panes[1], pairs, *focused, edit, error.as_deref());
        }
        Mode::ProfileList { draft, selected } => {
            draw_profile_list(frame, panes[1], &draft.profiles, *selected, &draft.fields[0].1);
        }
        Mode::NewProfile { name, params, focused, edit, error, editing, .. } => {
            draw_new_profile(frame, panes[1], name, params, *focused, edit, error.as_deref(), editing.is_some());
        }
        Mode::ConfirmDelete { entry_name, ..} => {
            draw_confirm_delete(frame, panes[1], entry_name);
        }
    }
    if let Some(h) = response_view_height {
        app.response_view_height = h;
    }

    draw_help(frame, outer[1], &app.mode);

    // Last, so it lands on top of both panes and the hint line.
    if app.show_help {
        draw_help_overlay(frame, full);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    entries: &[Entry],
    tree_rows: Option<&[VisibleRow]>,
    filtered: &[usize],
    list_state: &mut ListState,
    filter: &str,
    filter_active: bool,
    use_nerd_fonts: bool,
) {
    let show_filter = filter_active || !filter.is_empty();
    let chunks = if show_filter {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    };

    if show_filter {
        let display = if filter_active { format!("{}█", filter) } else { filter.to_string() };
        let filter_widget = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(display, Style::default().fg(Color::White)),
        ]))
        .block(
            Block::default()
                .title(" Filter ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if filter_active { Color::Yellow } else { Color::DarkGray })),
        );
        frame.render_widget(filter_widget, chunks[0]);
    }

    let (items, highlight_sym): (Vec<ListItem>, &str) = if let Some(rows) = tree_rows {
        let items = rows.iter().map(|row| match row {
            VisibleRow::Folder { path, expanded, prefix, .. } => {
                make_tree_folder_item(path, *expanded, prefix, use_nerd_fonts)
            }
            VisibleRow::Leaf { entry_idx, prefix, .. } => {
                make_tree_leaf_item(&entries[*entry_idx], prefix, use_nerd_fonts)
            }
        }).collect();
        (items, "▶ ")
    } else {
        let items = filtered.iter().map(|&i| make_list_item(&entries[i])).collect();
        (items, "▶ ")
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Requests ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(highlight_sym);
    frame.render_stateful_widget(list, chunks[1], list_state);
}

fn draw_detail_browse(frame: &mut Frame, area: Rect, selected: Option<&Entry>) {
    let lines = selected
        .map(make_detail_lines)
        .unwrap_or_else(|| vec![Line::from(" No request selected.")]);
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Details ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_profile_select(
    frame: &mut Frame,
    area: Rect,
    profiles: &[crate::config::Profile],
    selected: usize,
    entry_name: &str,
) {
    let title = format!(" Profile — {} ", entry_name);
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    let is_none_selected = selected == 0;
    let none_style = if is_none_selected {
        Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(Span::styled("  (no profile)", none_style)));
    lines.push(Line::raw(""));

    for (i, profile) in profiles.iter().enumerate() {
        let is_selected = selected == i + 1;
        let name_style = if is_selected {
            Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("  {}", profile.name),
            name_style,
        )));
        for (k, v) in &profile.params {
            lines.push(Line::from(vec![
                Span::styled(format!("    {}: ", k), Style::default().fg(Color::DarkGray)),
                Span::styled(v.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_arguments)]
fn draw_new_request(
    frame: &mut Frame,
    area: Rect,
    fields: &[(String, String)],
    focused: usize,
    edit: &Edit,
    profiles: &[crate::config::Profile],
    headers: &std::collections::HashMap<String, String>,
    is_edit: bool,
    global: bool,
    error: Option<&str>,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (label, value)) in fields.iter().enumerate() {
        let is_focused = i == focused;
        let is_required = i < 3; // name, method, url are required

        let mut label_spans = vec![Span::styled(
            format!("  {}", label),
            Style::default()
                .fg(if is_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )];
        if is_required {
            label_spans.push(Span::styled(" *", Style::default().fg(Color::Red)));
        }
        lines.push(Line::from(label_spans));

        lines.push(input_line(value, caret(is_focused, edit)));
        lines.push(Line::raw(""));
    }

    let (global_check, global_color) = if global {
        ("[x] global", Color::Cyan)
    } else {
        ("[ ] global", Color::DarkGray)
    };
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(global_check, Style::default().fg(global_color).add_modifier(Modifier::BOLD)),
        Span::styled("  Ctrl+g to toggle", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::raw(""));

    // Headers are edited in their own pane, but shown here so the form is not
    // silent about a part of the request it carries.
    lines.push(Line::from(vec![
        Span::styled(
            "  headers",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Ctrl+e to edit", Style::default().fg(Color::DarkGray)),
    ]));
    if headers.is_empty() {
        lines.push(Line::from(Span::styled(
            "    none",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let mut sorted: Vec<(&String, &String)> = headers.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in sorted {
            lines.push(Line::from(vec![
                Span::styled(format!("    {}: ", k), Style::default().fg(Color::Cyan)),
                Span::styled(v.clone(), Style::default().fg(Color::White)),
            ]));
        }
    }
    lines.push(Line::raw(""));

    if !profiles.is_empty() {
        lines.push(Line::from(Span::styled(
            "  profiles",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )));
        for profile in profiles {
            lines.push(Line::from(vec![
                Span::styled("    ", Style::default()),
                Span::styled(profile.name.clone(), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]));
            for (k, v) in &profile.params {
                lines.push(Line::from(vec![
                    Span::styled(format!("      {}: ", k), Style::default().fg(Color::DarkGray)),
                    Span::styled(v.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let title = if is_edit { " Edit Request " } else { " New Request " };
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_import_curl(frame: &mut Frame, area: Rect, buffer: &str, error: Option<&str>) {
    let mut lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  Paste a cURL command, then Ctrl+s to import",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
    ];

    if buffer.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  > ", Style::default().fg(Color::DarkGray)),
            Span::styled("█", Style::default().fg(Color::White)),
        ]));
    } else {
        // The cursor sits at the end of the last line: the buffer is only ever
        // appended to, so there is nowhere else for it to be.
        let last = buffer.lines().count().saturating_sub(1);
        for (i, line) in buffer.lines().enumerate() {
            let text = if i == last { format!("{}█", line) } else { line.to_string() };
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(text, Style::default().fg(Color::White)),
            ]));
        }
    }

    if let Some(err) = error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(" Import cURL ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        );
    frame.render_widget(paragraph, area);
}

#[allow(clippy::too_many_arguments)]
fn draw_new_profile(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    params: &[(String, String)],
    focused: usize,
    edit: &Edit,
    error: Option<&str>,
    is_edit: bool,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    // Profile name field
    let name_focused = focused == 0;
    lines.push(Line::from(vec![
        Span::styled(
            "  profile name",
            Style::default()
                .fg(if name_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" *", Style::default().fg(Color::Red)),
    ]));
    lines.push(input_line(name, caret(name_focused, edit)));
    lines.push(Line::raw(""));

    // Param pairs: focused index 1+2i = key, 2+2i = value
    for (i, (key, value)) in params.iter().enumerate() {
        let key_focused = focused == 1 + 2 * i;
        let val_focused = focused == 2 + 2 * i;

        lines.push(Line::from(Span::styled(
            format!("  param {} key", i + 1),
            Style::default()
                .fg(if key_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(key, caret(key_focused, edit)));

        lines.push(Line::from(Span::styled(
            format!("  param {} value", i + 1),
            Style::default()
                .fg(if val_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(val_focused, edit)));
        lines.push(Line::raw(""));
    }

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(if is_edit { " Edit Profile " } else { " New Profile " })
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

/// The draft's headers as name/value field pairs.
/// focused: 2i = pairs[i].name, 2i+1 = pairs[i].value
fn draw_edit_headers(
    frame: &mut Frame,
    area: Rect,
    pairs: &[(String, String)],
    focused: usize,
    edit: &Edit,
    error: Option<&str>,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (name, value)) in pairs.iter().enumerate() {
        let name_focused = focused == 2 * i;
        let value_focused = focused == 2 * i + 1;

        lines.push(Line::from(Span::styled(
            format!("  header {} name", i + 1),
            Style::default()
                .fg(if name_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(name, caret(name_focused, edit)));

        lines.push(Line::from(Span::styled(
            format!("  header {} value", i + 1),
            Style::default()
                .fg(if value_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(value_focused, edit)));
        lines.push(Line::raw(""));
    }

    if pairs.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No headers. Ctrl+a adds one.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::raw(""));
    }

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Headers ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(paragraph, area);
}

/// The draft's profiles, one block each, with a trailing "new profile" row at
/// index `profiles.len()`.
fn draw_profile_list(
    frame: &mut Frame,
    area: Rect,
    profiles: &[crate::config::Profile],
    selected: usize,
    request_name: &str,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, profile) in profiles.iter().enumerate() {
        let is_selected = i == selected;
        let marker = if is_selected { "  ▶ " } else { "    " };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(Color::Magenta)),
            Span::styled(
                profile.name.clone(),
                Style::default()
                    .fg(if is_selected { Color::White } else { Color::Magenta })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("   {} var{}", profile.params.len(), if profile.params.len() == 1 { "" } else { "s" }),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        // Params only for the selected profile: showing every one turns a
        // handful of environments into a wall of secrets.
        if is_selected {
            let mut params: Vec<(&String, &String)> = profile.params.iter().collect();
            params.sort_by(|a, b| a.0.cmp(b.0));
            for (k, v) in params {
                lines.push(Line::from(vec![
                    Span::styled(format!("      {}: ", k), Style::default().fg(Color::DarkGray)),
                    Span::styled(v.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }

    let new_selected = selected >= profiles.len();
    lines.push(Line::from(vec![
        Span::styled(
            if new_selected { "  ▶ " } else { "    " },
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            "+ new profile",
            Style::default()
                .fg(if new_selected { Color::White } else { Color::Green })
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if profiles.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  No profiles yet. A profile bundles {{VAR}} values under a name,",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  so you can switch environments without retyping them.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(format!(" Profiles — {} ", request_name))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_var_input(
    frame: &mut Frame,
    area: Rect,
    vars: &[(String, String)],
    focused: usize,
    edit: &Edit,
    entry_name: &str,
) {
    let title = format!(" Variables — {} ", entry_name);
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (name, value)) in vars.iter().enumerate() {
        let is_focused = i == focused;

        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default()
                .fg(if is_focused { Color::Yellow } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(is_focused, edit)));
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_test_input(
    frame: &mut Frame,
    area: Rect,
    vars: &[(String, String)],
    iterations: &str,
    focused: usize,
    edit: &Edit,
    entry_name: &str,
) {
    let title = format!(" Test — {} ", entry_name);
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];

    for (i, (name, value)) in vars.iter().enumerate() {
        let is_focused = i == focused;
        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default()
                .fg(if is_focused { Color::Yellow } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(is_focused, edit)));
        lines.push(Line::raw(""));
    }

    let iter_focused = focused == vars.len();
    lines.push(Line::from(Span::styled(
        "  iterations",
        Style::default()
            .fg(if iter_focused { Color::Yellow } else { Color::DarkGray })
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(input_line(iterations, caret(iter_focused, edit)));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_test_results(frame: &mut Frame, area: Rect, results: &crate::tester::TestResults) {
    let iterations = results.timings.len();
    let title = Line::from(vec![
        Span::raw(" Test Results "),
        Span::styled(
            format!(" {} iterations ", iterations),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
    ]);

    let outer = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3), Constraint::Length(5)])
        .split(inner);

    // Status distribution horizontal gauges
    let max_count = results.statuses.iter().map(|s| s.count).max().unwrap_or(1);
    let bar_width = 30usize;
    let mut gauge_lines: Vec<Line<'static>> = vec![Line::raw("")];
    for sc in &results.statuses {
        let color = status_color(sc.status);
        let filled = ((sc.count * bar_width) / max_count).max(1);
        let empty = bar_width - filled;
        gauge_lines.push(Line::from(vec![
            Span::styled(format!("  {:>3}  ", sc.status), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled("█".repeat(filled), Style::default().fg(color)),
            Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("  ×{}", sc.count), Style::default().fg(Color::White)),
        ]));
    }
    let gauge_widget = Paragraph::new(gauge_lines).block(
        Block::default()
            .title(" Status Distribution ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(gauge_widget, chunks[0]);

    // Timing stats
    let stats = Paragraph::new(Line::from(vec![
        Span::styled("  avg ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.2?}", results.avg), Style::default().fg(Color::Yellow)),
        Span::styled("   min ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.2?}", results.min), Style::default().fg(Color::Green)),
        Span::styled("   max ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:.2?}", results.max), Style::default().fg(Color::Red)),
    ]));
    frame.render_widget(stats, chunks[1]);

    // Per-iteration latency line chart
    let max_ms = results.timings.iter().copied().max().unwrap_or(1);
    let latency_points: Vec<(f64, f64)> = results
        .timings
        .iter()
        .enumerate()
        .map(|(i, &ms)| (i as f64, ms as f64))
        .collect();
    let latency_dataset = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&latency_points);
    let latency_chart = Chart::new(vec![latency_dataset])
        .block(
            Block::default()
                .title(" Latency (ms) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, results.timings.len().saturating_sub(1) as f64]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, max_ms as f64])
                .labels(vec![Span::raw("0"), Span::raw(format!("{}ms", max_ms))]),
        );
    frame.render_widget(latency_chart, chunks[2]);
}

fn status_color(status: u16) -> Color {
    if status < 300 { Color::Green }
    else if status < 400 { Color::Cyan }
    else if status < 500 { Color::Yellow }
    else { Color::Red }
}

/// Returns the height of the body viewport (inside the borders).
#[allow(clippy::too_many_arguments)]
fn draw_response(
    frame: &mut Frame,
    area: Rect,
    kind: &ResponseKind,
    body: &str,
    scroll: u16,
    response_filter: &str,
    response_filter_active: bool,
    cursor: usize,
    anchor: Option<usize>,
    status: Option<&str>,
) -> u16 {
    // A generated command is neither a response nor an error, so it gets its
    // own heading rather than borrowing a status code it doesn't have.
    let (heading, badge_color, badge) = match kind {
        ResponseKind::Error => (" Response ", Color::Red, " Error ".to_string()),
        ResponseKind::Http(status) => {
            let color = if *status < 300 {
                Color::Green
            } else if *status < 400 {
                Color::Cyan
            } else if *status < 500 {
                Color::Yellow
            } else {
                Color::Red
            };
            (" Response ", color, format!(" {} ", status))
        }
        ResponseKind::Curl { copied: Ok(()) } => (" cURL ", Color::Green, " copied ".to_string()),
        ResponseKind::Curl { copied: Err(e) } => {
            (" cURL ", Color::Red, format!(" copy failed: {} ", e))
        }
    };

    let mut title_spans = vec![
        Span::raw(heading),
        Span::styled(badge, Style::default().fg(Color::Black).bg(badge_color)),
    ];
    if anchor.is_some() {
        title_spans.push(Span::styled(
            " VISUAL ",
            Style::default().fg(Color::Black).bg(Color::Blue).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(status) = status {
        title_spans.push(Span::styled(
            format!(" {status} "),
            Style::default().fg(Color::Black).bg(Color::Green),
        ));
    }
    let title = Line::from(title_spans);

    let show_filter = response_filter_active || !response_filter.is_empty();
    let chunks = if show_filter {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    };

    if show_filter {
        let display = if response_filter_active { format!("{}█", response_filter) } else { response_filter.to_string() };
        let filter_widget = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(display, Style::default().fg(Color::White)),
        ]))
        .block(
            Block::default()
                .title(" Filter ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if response_filter_active { Color::Yellow } else { Color::DarkGray })),
        );
        frame.render_widget(filter_widget, chunks[0]);
    }

    let (from, to) = super::selection_range(cursor, anchor);
    let lines: Vec<Line<'static>> = super::visible_response_lines(body, response_filter)
        .into_iter()
        .map(colorize_json_line)
        .enumerate()
        .map(|(i, line)| {
            // A base style on the Line paints behind the spans, so the JSON
            // colouring survives the highlight rather than being replaced.
            if anchor.is_some() && i >= from && i <= to {
                line.style(Style::default().bg(Color::Blue))
            } else if anchor.is_none() && i == cursor {
                line.style(Style::default().bg(Color::DarkGray))
            } else {
                line
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(badge_color)),
        )
        .scroll((scroll, 0));

    frame.render_widget(paragraph, chunks[1]);
    chunks[1].height.saturating_sub(2)
}


fn draw_help(frame: &mut Frame, area: Rect, mode: &Mode) {
    let spans: Vec<Span<'static>> = match mode {
        // Deliberately short. The full keymap lives behind `?`; listing all
        // fifteen bindings here ran to 155 columns, which an 80-column terminal
        // truncates without a mark, hiding the last five outright.
        Mode::Browse => vec![
            Span::styled(" r ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run", Style::default().fg(Color::DarkGray)),
            Span::styled("   t ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("test", Style::default().fg(Color::DarkGray)),
            Span::styled("   n ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("new", Style::default().fg(Color::DarkGray)),
            Span::styled("   f ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("filter", Style::default().fg(Color::DarkGray)),
            Span::styled("   ? ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("help", Style::default().fg(Color::DarkGray)),
            Span::styled("   q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quit", Style::default().fg(Color::DarkGray)),
        ],
        Mode::ImportCurl { .. } => vec![
            Span::styled(" Ctrl+s ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("import", Style::default().fg(Color::DarkGray)),
            Span::styled("   Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("newline", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::ProfileSelect { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("select", Style::default().fg(Color::DarkGray)),
            Span::styled("   j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("navigate", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::VarInput { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("switch field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::TestInput { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run test", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("switch field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::NewRequest { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("save", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("fields", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^e ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("headers", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^p ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("profiles", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^g ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("global", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::EditHeaders { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("apply", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("fields", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^a ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("add header", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^d ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("remove header", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::ProfileList { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("edit", Style::default().fg(Color::DarkGray)),
            Span::styled("   j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("navigate", Style::default().fg(Color::DarkGray)),
            Span::styled("   n ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("new", Style::default().fg(Color::DarkGray)),
            Span::styled("   d ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("delete", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("back to request", Style::default().fg(Color::DarkGray)),
        ],
        Mode::NewProfile { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("save profile", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab/S-Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("next/prev field", Style::default().fg(Color::DarkGray)),
            Span::styled("   Ctrl+a ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("add param", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::ConfirmDelete { .. } => vec![
            Span::styled(" y/Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("confirm delete", Style::default().fg(Color::DarkGray)),
            Span::styled("   n/Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        // While selecting, the hints are only the keys that finish or abandon
        // the selection — everything else is noise with a half-made one on
        // screen.
        Mode::Response { anchor: Some(_), .. } => vec![
            Span::styled(" j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("extend", Style::default().fg(Color::DarkGray)),
            Span::styled("   y ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("copy selection", Style::default().fg(Color::DarkGray)),
            Span::styled("   V/Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        Mode::Response { .. } | Mode::TestResponse { .. } => vec![
            Span::styled(" j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("move", Style::default().fg(Color::DarkGray)),
            Span::styled("   V ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("select", Style::default().fg(Color::DarkGray)),
            Span::styled("   y ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("copy line", Style::default().fg(Color::DarkGray)),
            Span::styled("   f ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("filter", Style::default().fg(Color::DarkGray)),
            Span::styled("   c ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("copy all", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("back", Style::default().fg(Color::DarkGray)),
            Span::styled("   q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quit", Style::default().fg(Color::DarkGray)),
        ],
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The three columns of the `?` overlay, grouped by what you are trying to do
/// rather than listed flat — a flat list styles `gg` and `i` identically, when
/// one is muscle memory and the other is a whole feature.
const HELP_COLUMNS: [(&str, &[(&str, &str)]); 3] = [
    (
        "Navigate",
        &[
            ("j/k", "up/down"),
            ("gg/G", "top/bottom"),
            ("Space", "fold"),
            ("f", "filter"),
        ],
    ),
    (
        "Run",
        &[
            ("r/Enter", "run"),
            ("t", "load test"),
            ("y", "copy cURL"),
            ("i", "import cURL"),
        ],
    ),
    (
        "Manage",
        &[
            ("n", "new"),
            ("e", "edit"),
            ("h", "headers"),
            ("p", "profiles"),
            ("c", "clone"),
            ("d", "delete"),
            ("R", "refresh"),
        ],
    ),
];

const HELP_KEY_WIDTH: usize = 8;
// Wide enough that the longest label ("import cURL") still clears the next column.
const HELP_LABEL_WIDTH: usize = 13;
const HELP_WIDTH: u16 = (2 + 3 * (HELP_KEY_WIDTH + HELP_LABEL_WIDTH)) as u16;

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let rows = HELP_COLUMNS.iter().map(|(_, keys)| keys.len()).max().unwrap_or(0);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows + 4);

    let mut header = vec![Span::raw("  ")];
    for (title, _) in HELP_COLUMNS {
        header.push(Span::styled(
            format!("{:<width$}", title, width = HELP_KEY_WIDTH + HELP_LABEL_WIDTH),
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(header));
    lines.push(Line::raw(""));

    for row in 0..rows {
        let mut spans = vec![Span::raw("  ")];
        for (_, keys) in HELP_COLUMNS {
            match keys.get(row) {
                Some((key, label)) => {
                    spans.push(Span::styled(
                        format!("{:<width$}", key, width = HELP_KEY_WIDTH),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        format!("{:<width$}", label, width = HELP_LABEL_WIDTH),
                        Style::default().fg(Color::White),
                    ));
                }
                // Columns are ragged; pad so the next one still lines up.
                None => spans.push(Span::raw(" ".repeat(HELP_KEY_WIDTH + HELP_LABEL_WIDTH))),
            }
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("q", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  quit          ", Style::default().fg(Color::DarkGray)),
        Span::styled("any key", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  close this", Style::default().fg(Color::DarkGray)),
    ]));

    let height = lines.len() as u16 + 2; // + borders
    let popup = centered_rect(HELP_WIDTH, height, area);

    let widget = Paragraph::new(lines).block(
        Block::default()
            .title(" Keys ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );

    // Clear first: the pane underneath has already been drawn into these cells.
    frame.render_widget(Clear, popup);
    frame.render_widget(widget, popup);
}

/// Centers a `width` × `height` box in `area`, shrinking to fit rather than
/// overflowing when the terminal is smaller than the box.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn draw_confirm_delete(frame: &mut Frame, area: Rect, entry_name: &str) {
    let lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Delete  ", Style::default().fg(Color::DarkGray)),
            Span::styled(entry_name.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("?", Style::default().fg(Color::DarkGray)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  This action cannot be undone.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  y / Enter  ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("yes, delete", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("  n / Esc    ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Confirm Delete ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red)),
    );
    frame.render_widget(paragraph, area);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn colorize_json_value(val: &str) -> Vec<Span<'static>> {
    let (content, trailing) = val.strip_suffix(',').map_or((val, ""), |s| (s, ","));

    let color = if content.starts_with('"') {
        Color::Green
    } else if matches!(content, "true" | "false") {
        Color::Magenta
    } else if content == "null" {
        Color::Red
    } else if matches!(content, "{" | "}" | "[" | "]" | "{}" | "[]") {
        Color::DarkGray
    } else {
        Color::Cyan // numbers
    };

    let mut spans = vec![Span::styled(content.to_string(), Style::default().fg(color))];
    if !trailing.is_empty() {
        spans.push(Span::styled(",".to_string(), Style::default().fg(Color::DarkGray)));
    }
    spans
}

fn colorize_json_line(line: &str) -> Line<'static> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = line[..indent_len].to_string();
    let trimmed = &line[indent_len..];

    if matches!(trimmed, "{" | "}" | "[" | "]" | "}," | "]," | "{}" | "[]" | "{}," | "[],") {
        return Line::from(vec![
            Span::raw(indent),
            Span::styled(trimmed.to_string(), Style::default().fg(Color::DarkGray)),
        ]);
    }

    // Key-value line: "key": value
    if trimmed.starts_with('"')
        && let Some(pos) = trimmed.find("\": ") {
            let key = trimmed[..pos + 1].to_string(); // includes closing "
            let value = &trimmed[pos + 3..];          // skip ": "
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(key, Style::default().fg(Color::Yellow)),
                Span::styled(": ".to_string(), Style::default().fg(Color::DarkGray)),
            ];
            spans.extend(colorize_json_value(value));
            return Line::from(spans);
        }

    // Array value or plain text
    let mut spans = vec![Span::raw(indent)];
    spans.extend(colorize_json_value(trimmed));
    Line::from(spans)
}

/// Clipboard tools tried in order: macOS, then Wayland, then X11.
const CLIPBOARD_BACKENDS: &[(&str, &[&str])] = &[
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// Copies `text` to the system clipboard, returning why it failed if it did.
///
/// Backends are probed by trying to spawn them rather than by reading
/// `$WAYLAND_DISPLAY` / `$DISPLAY`: those lie under tmux and over SSH, and
/// probing keeps it to a single mechanism.
pub(super) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    for (program, args) in CLIPBOARD_BACKENDS {
        if run_clipboard_backend(program, args, text) {
            return Ok(());
        }
    }
    Err(format!(
        "no clipboard tool found (tried {})",
        CLIPBOARD_BACKENDS
            .iter()
            .map(|(p, _)| *p)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn run_clipboard_backend(program: &str, args: &[&str], text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Silence the child's output: we're inside the alternate screen, and a
    // backend printing a diagnostic would corrupt the display.
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let wrote = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()).is_ok(),
        None => false,
    };

    // Always reap, so a failed backend doesn't leave a zombie behind. A
    // non-zero exit counts as failure so the next backend gets its turn.
    let exited_ok = matches!(child.wait(), Ok(status) if status.success());
    wrote && exited_ok
}

fn method_color(method: &str) -> Color {
    match method {
        "GET" => Color::Cyan,
        "POST" => Color::Green,
        "PUT" | "PATCH" => Color::Yellow,
        "DELETE" => Color::Red,
        _ => Color::White,
    }
}

pub(super) fn format_test_results_text(results: &crate::tester::TestResults) -> String {
    let mut lines = vec!["Results".to_string(), "".to_string()];
    for sc in &results.statuses {
        let reason = reqwest::StatusCode::from_u16(sc.status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Unknown");
        lines.push(format!("  {} {}  x {}", sc.status, reason, sc.count));
    }
    lines.push("".to_string());
    lines.push("Timings".to_string());
    lines.push("".to_string());
    lines.push(format!("  avg:  {:.2?}", results.avg));
    lines.push(format!("  min:  {:.2?}", results.min));
    lines.push(format!("  max:  {:.2?}", results.max));
    lines.join("\n")
}

fn folder_name_spans(name: &str) -> Vec<Span<'static>> {
    match name.rfind('/') {
        Some(pos) => vec![
            Span::styled(
                format!("{}/ ", &name[..pos]),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                name[pos + 1..].to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ],
        None => vec![Span::styled(
            name.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )],
    }
}

fn make_list_item(entry: &Entry) -> ListItem<'static> {
    let scope = if entry.global { " global" } else { " local" };
    let line = match &entry.kind {
        EntryKind::Chain { .. } => {
            let mut spans = vec![
                Span::styled(
                    " CHAIN ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            spans.extend(folder_name_spans(&entry.name));
            spans.push(Span::styled(scope, Style::default().fg(Color::DarkGray)));
            Line::from(spans)
        }
        EntryKind::Request { method, description, .. } => {
            let color = method_color(method);
            let mut spans = vec![
                Span::styled(
                    format!(" {:<6}", method),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            spans.extend(folder_name_spans(&entry.name));
            if let Some(desc) = description
                && !desc.is_empty()
            {
                spans.push(Span::styled(
                    format!(" — {}", desc),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::styled(scope, Style::default().fg(Color::DarkGray)));
            Line::from(spans)
        }
    };
    ListItem::new(line)
}

fn make_tree_folder_item(path: &str, expanded: bool, prefix: &str, use_nerd_fonts: bool) -> ListItem<'static> {
    let display_name = path.rsplit('/').next().unwrap_or(path).to_string();
    let line = if use_nerd_fonts {
        let icon = if expanded { "\u{f07c} " } else { "\u{f07b} " };
        Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(icon.to_string(), Style::default().fg(Color::Yellow)),
            Span::styled(display_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ])
    } else {
        Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}/", display_name), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ])
    };
    ListItem::new(line)
}

fn make_tree_leaf_item(entry: &Entry, prefix: &str, _use_nerd_fonts: bool) -> ListItem<'static> {
    let leaf_name = entry.name.rsplit('/').next().unwrap_or(&entry.name).to_string();
    let line = match &entry.kind {
        EntryKind::Chain { .. } => Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(" CHAIN ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(leaf_name, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        EntryKind::Request { method, description, .. } => {
            let color = method_color(method);
            let mut spans = vec![
                Span::styled(prefix.to_string(), Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {:<6}", method), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(leaf_name, Style::default().add_modifier(Modifier::BOLD)),
            ];
            if let Some(desc) = description.as_ref().filter(|d| !d.is_empty()) {
                spans.push(Span::styled(format!(" — {}", desc), Style::default().fg(Color::DarkGray)));
            }
            Line::from(spans)
        }
    };
    ListItem::new(line)
}

fn make_detail_lines(entry: &Entry) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![];

    match &entry.kind {
        EntryKind::Chain { steps } => {
            let mut header = vec![
                Span::styled(
                    " CHAIN ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            header.extend(folder_name_spans(&entry.name));
            lines.push(Line::from(header));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                " Steps",
                Style::default().fg(Color::DarkGray),
            )));
            for (i, step) in steps.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {}. ", i + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(step.clone()),
                ]));
            }
        }
        EntryKind::Request { method, url, description, headers, query, .. } => {
            let color = method_color(method);
            let mut header = vec![
                Span::styled(
                    format!(" {} ", method),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            header.extend(folder_name_spans(&entry.name));
            lines.push(Line::from(header));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(" url    ", Style::default().fg(Color::DarkGray)),
                Span::styled(url.clone(), Style::default().fg(Color::Cyan)),
            ]));
            if let Some(desc) = description
                && !desc.is_empty()
            {
                lines.push(Line::from(vec![
                    Span::styled(" desc   ", Style::default().fg(Color::DarkGray)),
                    Span::raw(desc.clone()),
                ]));
            }
            if !headers.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    " Headers",
                    Style::default().fg(Color::DarkGray),
                )));
                for (k, v) in headers {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}: ", k), Style::default().fg(Color::Yellow)),
                        Span::raw(v.clone()),
                    ]));
                }
            }
            if !query.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    " Query",
                    Style::default().fg(Color::DarkGray),
                )));
                for (k, v) in query {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}: ", k), Style::default().fg(Color::Yellow)),
                        Span::raw(v.clone()),
                    ]));
                }
            }
        }
    }

    lines
}
