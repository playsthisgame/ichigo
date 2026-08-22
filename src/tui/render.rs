use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    symbols,
    widgets::{Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use ratatui_image::StatefulImage;
use super::edit::Edit;
use super::{App, DiscardChoice, Mode, PairKind, ResponseKind};
use super::tree::{Entry, EntryKind, VisibleRow, visible_rows};

/// One text field's value with the caret drawn where it sits.
///
/// Insert mode gets a thin bar between characters, normal mode a block *over*
/// the character it is on — the same shapes a terminal cursor takes, which is
/// the only signal of which mode a field is in. Nothing else says so, and
/// nothing else needs to.
fn field_spans(value: &str, edit: Option<&Edit>, color: Color) -> Vec<Span<'static>> {
    spans_with_caret(value, edit.map(|e| (e.caret, e.insert)), color)
}

/// `field_spans` with the caret passed as a position rather than an `Edit`, for
/// the body pane: there the caret is one offset into a multi-line value, and the
/// line holding it needs it re-measured from that line's start.
fn spans_with_caret(value: &str, caret: Option<(usize, bool)>, color: Color) -> Vec<Span<'static>> {
    let text = Style::default().fg(color);
    let Some((caret_at, insert)) = caret else {
        return vec![Span::styled(value.to_string(), text)];
    };

    // The caret can outlive the value it was measured against — a draft that
    // went through another pane, a row deleted at the focused index — so it is
    // clamped here rather than trusted.
    let caret = value
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(value.len()))
        .take_while(|i| *i <= caret_at)
        .last()
        .unwrap_or(0);
    let (before, after) = value.split_at(caret);
    let cursor = Style::default().fg(Color::Black).bg(Color::White);

    let mut spans = vec![Span::styled(before.to_string(), text)];
    match (insert, after.chars().next()) {
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

/// A multi-line value as one drawn line per line of text, with the caret on
/// whichever line holds it.
///
/// Splitting on `\n` rather than wrapping the whole thing in a `Paragraph`:
/// the caret is a byte offset into the value, and only the line containing it
/// can place it. `split('\n')` keeps a trailing empty segment for a value
/// ending in a newline, which is exactly the empty last line the caret sits on
/// after pressing Enter.
fn text_area_lines(value: &str, edit: Option<&Edit>, indent: &str) -> Vec<Line<'static>> {
    let color = if edit.is_some() { Color::White } else { Color::DarkGray };
    let mut start = 0;
    let mut lines = Vec::new();
    for segment in value.split('\n') {
        let end = start + segment.len();
        // A caret at `end` belongs to this line's end; the next line starts at
        // `end + 1`, past the newline, so no offset is claimed twice.
        let caret = edit
            .filter(|e| e.caret >= start && e.caret <= end)
            .map(|e| (e.caret - start, e.insert));
        let mut spans = vec![Span::raw(indent.to_string())];
        spans.extend(spans_with_caret(segment, caret, color));
        lines.push(Line::from(spans));
        start = end + 1;
    }
    lines
}

/// The caret to draw in a field: `Some` only for the focused one.
fn caret(is_focused: bool, edit: &Edit) -> Option<&Edit> {
    is_focused.then_some(edit)
}

/// One of the request form's four action rows — global, headers, query, profiles.
///
/// These are Tab stops with no text in them, so focus cannot be a caret. It is
/// the `▸` marker plus the label going green, matching the way a focused
/// field's label above already goes green. The hint only appears on the focused
/// row: three permanent hints read as clutter, one reads as an instruction.
fn action_row(label: &str, color: Color, hint: &str, is_focused: bool) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            if is_focused { "▸ " } else { "  " },
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            label.to_string(),
            Style::default()
                .fg(if is_focused { Color::Green } else { color })
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if is_focused {
        spans.push(Span::styled(
            format!("  {hint}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// The columns `input_line` spends before the value itself: two of indent, the
/// `> ` marker.
const INPUT_PREFIX: u16 = 4;

/// The columns inside a bordered pane — the width a row actually has.
fn interior(area: Rect) -> u16 {
    area.width.saturating_sub(2)
}

/// Which slice of a value a field shows, in characters, and whether text is
/// hidden either side of it.
///
/// The form panes are `Paragraph`s with no wrapping, so a value wider than the
/// pane is simply cut off at the border — and with it the caret, which is the
/// one thing on the row that has to stay on screen. This slides a window along
/// the value instead, keeping the caret inside it.
///
/// It is computed fresh each frame from the caret rather than stored and
/// nudged, so there is no scroll offset to keep in step with a value the form
/// can replace underneath it. The cost of being stateless is that the window
/// centres on the caret rather than following it lazily: text slides under a
/// caret moving through the middle of a long value, which is what vim's own
/// `scrolloff=999` does and is far better than the alternative of losing the
/// caret off the edge.
#[derive(Debug, PartialEq, Eq)]
struct Window {
    start: usize,
    end: usize,
    left: bool,
    right: bool,
}

/// The window for a value of `len` characters with the caret `caret` characters
/// into it, drawn in `width` columns.
///
/// Both markers are paid for whenever windowing happens at all, even when only
/// one is drawn. Charging for them only when they appear would change the
/// window's width as the caret reached either end of the value, sliding the
/// text by a column for a reason that has nothing to do with the caret moving.
fn window(len: usize, caret: usize, width: usize) -> Window {
    // The insert caret is drawn *between* characters as its own glyph, so a
    // value needs one column more than it has characters to show a caret at its
    // end. A width too small to hold anything is left unwindowed: there is
    // nothing useful to show, and the arithmetic below would have no budget.
    if width < 4 || len < width {
        return Window { start: 0, end: len, left: false, right: false };
    }
    let budget = width - 3;
    let start = caret.saturating_sub(budget / 2).min(len - budget);
    let end = (start + budget).min(len);
    Window { start, end, left: start > 0, right: end < len }
}

/// A `  > value` input row, dimmed unless it is the one holding the caret.
///
/// `width` is the whole row's width — the pane's interior. A row too narrow to
/// window is drawn whole and clipped by the pane, which is what every row did
/// before there was a window at all.
fn input_line(value: &str, caret: Option<&Edit>, width: u16) -> Line<'static> {
    let color = if caret.is_some() { Color::White } else { Color::DarkGray };
    let mut spans = vec![Span::styled("  > ", Style::default().fg(Color::DarkGray))];

    // Only the focused row has a caret to keep on screen. The rest are cut off
    // at the border as before: without a caret there is no one place in the
    // value that has to be visible, and a window would just hide the start.
    let Some(edit) = caret else {
        spans.extend(field_spans(value, None, color));
        return Line::from(spans);
    };

    let chars: Vec<(usize, char)> = value.char_indices().collect();
    let caret_col = chars.iter().take_while(|(i, _)| *i < edit.caret).count();
    let win = window(chars.len(), caret_col, usize::from(width.saturating_sub(INPUT_PREFIX)));

    let from = chars.get(win.start).map_or(value.len(), |(i, _)| *i);
    let to = chars.get(win.end).map_or(value.len(), |(i, _)| *i);
    let marker = Style::default().fg(Color::DarkGray);
    if win.left {
        spans.push(Span::styled("‹", marker));
    }
    spans.extend(spans_with_caret(
        &value[from..to],
        Some((edit.caret.saturating_sub(from), edit.insert)),
        color,
    ));
    if win.right {
        spans.push(Span::styled("›", marker));
    }
    Line::from(spans)
}

/// The rows inside a bordered pane — the height its lines actually have.
fn interior_height(area: Rect) -> u16 {
    area.height.saturating_sub(2)
}

/// The lines of a pane its focused row occupies, recorded while the pane's
/// lines are built.
///
/// A pane knows which of its rows is focused only as it draws that row, and the
/// scroll offset needs the row's position in the finished list — so each
/// renderer marks the range as it goes and hands it to `vscroll` at the end.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FocusRows {
    start: usize,
    end: usize,
}

impl FocusRows {
    /// Records `start..end` as the focused block, if this is the focused row.
    /// Called with the length taken before the block was pushed and the length
    /// after it, so a pane cannot record a range it never closes.
    fn mark(&mut self, focused: bool, start: usize, end: usize) {
        if focused {
            self.start = start;
            self.end = end;
        }
    }

    /// The block at `row`, for a pane whose focused row is a single line.
    fn at(row: usize) -> Self {
        FocusRows { start: row, end: row + 1 }
    }
}

/// How far down a pane's lines are scrolled so its focused row stays on screen.
///
/// The vertical half of `window`, and stateless for the same reason: a form's
/// contents change under it — a header row deleted at the focused index, a
/// draft that toured another pane and came back — and an offset stored and
/// nudged would be describing a list of lines that no longer exists. Recomputed
/// each frame from the focused row, it cannot go stale.
///
/// It centres that row for the same reason `window` centres the caret, and at
/// the same price: the form slides under a focus moving through the middle of
/// it, which is vim's own `scrolloff=999` and is the better half of the trade
/// against a caret that is simply not on the screen. Centring also leaves half
/// a pane of slack either side of the row, which is what lets the two wrapping
/// panes scroll on an *estimate* of where `Wrap` broke their lines.
fn vscroll(total: usize, rows: FocusRows, height: usize) -> u16 {
    if height == 0 || total <= height {
        return 0;
    }
    let middle = rows.start + rows.end.saturating_sub(rows.start) / 2;
    // Never past the end of the content, and never past the focused block's own
    // first line: a block taller than the pane shows its top rather than
    // centring a middle that would take the top off the screen.
    let offset = middle
        .saturating_sub(height / 2)
        .min(total - height)
        .min(rows.start);
    u16::try_from(offset).unwrap_or(u16::MAX)
}

/// The character index each rendered row of `line` starts at, in a pane `width`
/// columns wide.
///
/// An *estimate* of what `Wrap { trim: false }` does — break at the last space
/// that fits, mid-word for a word wider than the pane — because the two
/// wrapping panes scroll by rendered rows and only the widget knows for certain
/// where it broke them. `vscroll` centres, so a row of disagreement costs a row
/// of centring rather than the caret. Every line has a first row, so the result
/// is never empty.
fn wrap_starts(line: &str, width: usize) -> Vec<usize> {
    let mut starts = vec![0usize];
    if width == 0 {
        return starts;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut row_start = 0;
    // The index just past the last space seen on the current row, which is
    // where the next row begins if the row fills before another space arrives.
    let mut last_break: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        if i - row_start == width {
            let at = last_break.filter(|b| *b > row_start).unwrap_or(i);
            starts.push(at);
            row_start = at;
            last_break = None;
            // `at` may be behind `i`, so the characters after the break are
            // measured again against the new row rather than skipped.
            continue;
        }
        if chars[i].is_whitespace() {
            last_break = Some(i + 1);
        }
        i += 1;
    }
    starts
}

/// The rendered row each of `lines` starts on, and the rendered height of all
/// of them, for a pane that wraps.
fn wrapped_offsets(lines: &[Line<'static>], width: usize) -> (Vec<usize>, usize) {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut total = 0;
    for line in lines {
        offsets.push(total);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        total += wrap_starts(&text, width).len();
    }
    (offsets, total)
}

/// The rendered row of `line` that the character at `col` falls on.
fn wrapped_row_of(line: &str, col: usize, width: usize) -> usize {
    wrap_starts(line, width)
        .iter()
        .take_while(|start| **start <= col)
        .count()
        .saturating_sub(1)
}

/// The body pane's caret, as the line of `data` it sits on and the rendered
/// row *within* that line once `Wrap` has broken it.
///
/// The two halves are separate because only the first can be turned into a
/// position in the pane — the rows above the data are the pane's own, and it
/// counts those itself.
fn body_caret_row(data: &str, caret: usize, indent: &str, width: usize) -> (usize, usize) {
    let (line, col) = caret_line_col(data, caret);
    let text = format!("{indent}{}", data.split('\n').nth(line).unwrap_or(""));
    (line, wrapped_row_of(&text, indent.chars().count() + col, width))
}

/// The line of a multi-line value the caret sits on, and how many characters
/// into that line it is.
///
/// Must agree with `text_area_lines`, which places the caret by the same rule:
/// a caret at a line's end belongs to that line, and the next line starts past
/// the newline.
fn caret_line_col(value: &str, caret: usize) -> (usize, usize) {
    let mut start = 0;
    for (i, segment) in value.split('\n').enumerate() {
        let end = start + segment.len();
        if caret <= end {
            let col = segment[..caret.saturating_sub(start).min(segment.len())]
                .chars()
                .count();
            return (i, col);
        }
        start = end + 1;
    }
    (0, 0)
}

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let full = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(full);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(app.split_pct),
            Constraint::Percentage(100 - app.split_pct),
        ])
        .split(outer[0]);

    // Hand the divider's drawn position back to the mouse handler. Recording it
    // here is what keeps the hit test honest across a resize: the next click is
    // tested against the frame the user is actually looking at, not against a
    // percentage re-solved at a width that may have changed since.
    app.split_x = panes[1].x;
    app.term_width = full.width;

    let filtered = app.filtered_indices();
    let tree_rows_storage: Option<Vec<VisibleRow>> = if app.using_tree() {
        Some(visible_rows(&app.tree, &app.collapsed_folders))
    } else {
        None
    };
    let tree_rows = tree_rows_storage.as_deref();
    draw_list(frame, panes[0], &app.entries, tree_rows, &filtered, &mut app.list_state, &app.filter, app.filter_active, app.use_nerd_fonts);

    let mut response_view_height = None;
    // Where `draw_response` left room for an image. Filled in below and drawn
    // after the match, because the widget needs `&mut app.mode` and the match
    // holds it immutably — the Browse arm reads `app.entries` through `&self`
    // methods in the same scope, so the whole match cannot become mutable.
    let mut image_area = None;
    let has_image = matches!(&app.mode, Mode::Response { image: Some(_), .. });
    // Asked once, before the match borrows the mode: every form pane's title
    // carries the same marker, and each of them applies its own pending edits
    // to a copy of the draft to answer — see `Mode::unsaved`.
    let unsaved = app.mode.unsaved();
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
        Mode::Response { kind, body, summary, headers, show_headers, scroll, response_filter, response_filter_active, cursor, anchor, status, .. } => {
            let (height, area) = draw_response(
                frame, panes[1], kind, body, summary, headers, *show_headers, *scroll, response_filter,
                *response_filter_active, *cursor, *anchor, status.as_deref(), has_image,
            );
            response_view_height = Some(height);
            image_area = area;
        }
        Mode::TestResponse { results } => {
            draw_test_results(frame, panes[1], results);
        }
        Mode::NewRequest { draft, error } => {
            draw_new_request(frame, panes[1], &draft.fields, draft.focused, &draft.edit, &draft.profiles, draft.headers(), draft.query(), draft.body(), draft.original_name.is_some(), draft.global, unsaved, error.as_deref());
        }
        Mode::ImportCurl { buffer, error } => {
            draw_import_curl(frame, panes[1], buffer, error.as_deref());
        }
        Mode::EditPairs { kind, pairs, focused, edit, error, .. } => {
            draw_edit_pairs(frame, panes[1], *kind, pairs, *focused, edit, unsaved, error.as_deref());
        }
        Mode::EditBody { content_type, data, focused, edit, error, .. } => {
            draw_edit_body(frame, panes[1], content_type, data, *focused, edit, unsaved, error.as_deref());
        }
        Mode::ProfileList { draft, selected } => {
            draw_profile_list(frame, panes[1], &draft.profiles, *selected, &draft.fields[0].1, unsaved);
        }
        Mode::NewProfile { name, params, focused, edit, error, editing, .. } => {
            draw_new_profile(frame, panes[1], name, params, *focused, edit, unsaved, error.as_deref(), editing.is_some());
        }
        Mode::ConfirmDelete { entry_name, ..} => {
            draw_confirm_delete(frame, panes[1], entry_name);
        }
    }
    if let Some(h) = response_view_height {
        app.response_view_height = h;
    }

    // `StatefulImage` re-fits at render time, so a terminal resize and a drag
    // of the split divider both just work — the area is recomputed each frame
    // and the protocol re-encodes when it changes.
    if let (Some(area), Mode::Response { image: Some(protocol), .. }) = (image_area, &mut app.mode) {
        frame.render_stateful_widget(StatefulImage::new(), area, protocol.as_mut());
    }

    draw_help(frame, outer[1], &app.mode);

    // Last, so it lands on top of both panes and the hint line.
    if app.show_help {
        draw_help_overlay(frame, full);
    }
    if let Some(choice) = app.confirm_discard {
        draw_confirm_discard(frame, full, choice);
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
    let mut rows = FocusRows::default();

    let is_none_selected = selected == 0;
    let none_style = if is_none_selected {
        Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    rows.mark(is_none_selected, lines.len(), lines.len() + 1);
    lines.push(Line::from(Span::styled("  (no profile)", none_style)));
    lines.push(Line::raw(""));

    for (i, profile) in profiles.iter().enumerate() {
        let is_selected = selected == i + 1;
        let at = lines.len();
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
        rows.mark(is_selected, at, lines.len());
        lines.push(Line::raw(""));
    }

    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

/// A pane title, with the unsaved marker on it when the request behind the pane
/// has changes no save has written.
///
/// One helper for all five form panes: the marker means the same thing in each,
/// and one that read differently from one pane to the next would be a different
/// marker. It rides in the title rather than in the body of a pane because the
/// panes scroll and the title does not — and because the sub-panes are where
/// the changes get made, so the frame around them is what has to say they are
/// not written yet.
fn pane_title(title: &str, unsaved: bool) -> Line<'static> {
    let mut spans = vec![Span::raw(title.to_string())];
    if unsaved {
        spans.push(Span::styled(
            UNSAVED_MARKER,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// The marker itself, once, so the five panes and the prompt agree on it.
const UNSAVED_MARKER: &str = "● unsaved ";

#[allow(clippy::too_many_arguments)]
fn draw_new_request(
    frame: &mut Frame,
    area: Rect,
    fields: &[(String, String)],
    focused: usize,
    edit: &Edit,
    profiles: &[crate::config::Profile],
    headers: &std::collections::HashMap<String, String>,
    query: &std::collections::HashMap<String, String>,
    body: Option<&crate::config::Body>,
    is_edit: bool,
    global: bool,
    dirty: bool,
    error: Option<&str>,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];
    let mut rows = FocusRows::default();

    for (i, (label, value)) in fields.iter().enumerate() {
        let is_focused = i == focused;
        let is_required = i < 3; // name, method, url are required
        let at = lines.len();

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

        lines.push(input_line(value, caret(is_focused, edit), interior(area)));
        rows.mark(is_focused, at, lines.len());
        lines.push(Line::raw(""));
    }

    // The rows Tab reaches after the last field. Their state is on the row
    // itself, so focus is the marker and the colour rather than a caret.
    let (global_check, global_color) = if global {
        ("[x] global", Color::Cyan)
    } else {
        ("[ ] global", Color::DarkGray)
    };
    let at = lines.len();
    lines.push(action_row(
        global_check,
        global_color,
        "Enter to toggle",
        focused == fields.len(),
    ));
    rows.mark(focused == fields.len(), at, lines.len());
    lines.push(Line::raw(""));

    // Headers are edited in their own pane, but shown here so the form is not
    // silent about a part of the request it carries.
    let at = lines.len();
    lines.push(action_row(
        "headers",
        Color::DarkGray,
        "Enter to edit",
        focused == fields.len() + 1,
    ));
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
    rows.mark(focused == fields.len() + 1, at, lines.len());
    lines.push(Line::raw(""));

    // Query params, on the same terms as headers and drawn right after them
    // because that is the order a request config lists them in.
    let at = lines.len();
    lines.push(action_row(
        "query params",
        Color::DarkGray,
        "Enter to edit",
        focused == fields.len() + 2,
    ));
    if query.is_empty() {
        lines.push(Line::from(Span::styled(
            "    none",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let mut sorted: Vec<(&String, &String)> = query.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in sorted {
            lines.push(Line::from(vec![
                Span::styled(format!("    {}: ", k), Style::default().fg(Color::Cyan)),
                Span::styled(v.clone(), Style::default().fg(Color::White)),
            ]));
        }
    }
    rows.mark(focused == fields.len() + 2, at, lines.len());
    lines.push(Line::raw(""));

    // The body, in the config's own order: after query, before profiles.
    let at = lines.len();
    lines.push(action_row(
        "body",
        Color::DarkGray,
        "Enter to edit",
        focused == fields.len() + 3,
    ));
    match body {
        None => lines.push(Line::from(Span::styled(
            "    none",
            Style::default().fg(Color::DarkGray),
        ))),
        Some(body) => {
            lines.push(Line::from(vec![
                Span::styled("    content type: ", Style::default().fg(Color::Cyan)),
                Span::styled(body.content_type.clone(), Style::default().fg(Color::White)),
            ]));
            for line in body_preview(&body.data) {
                lines.push(Line::from(Span::styled(
                    format!("    {line}"),
                    Style::default().fg(Color::White),
                )));
            }
        }
    }
    rows.mark(focused == fields.len() + 3, at, lines.len());
    lines.push(Line::raw(""));

    // Drawn even when empty, unlike before: it is a Tab stop now, and a row
    // that vanishes when there is nothing in it is one focus can land on
    // invisibly.
    let at = lines.len();
    lines.push(action_row(
        "profiles",
        Color::DarkGray,
        "Enter to edit",
        focused == fields.len() + 4,
    ));
    if profiles.is_empty() {
        lines.push(Line::from(Span::styled(
            "    none",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
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
    }
    rows.mark(focused == fields.len() + 4, at, lines.len());
    lines.push(Line::raw(""));

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let title = pane_title(if is_edit { " Edit Request " } else { " New Request " }, dirty);
    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green)),
    );
    frame.render_widget(paragraph, area);
}

/// The body as the request form shows it: at most `BODY_PREVIEW_LINES` lines,
/// then a count of what is left.
///
/// The form scrolls to whichever row focus is on, so an eighty-line body no
/// longer hides the rows below it — but it would still put eighty lines between
/// `body` and `profiles`, which is a page of scrolling to cross a row that says
/// nothing the body pane does not say better. The preview is what the *form*
/// has to say about a body; reading one is what `EditBody` is for. Headers and
/// query params can grow the same way in principle, but a request with sixty
/// headers is a request someone built by hand; a sixty-line body is an ordinary
/// POST.
fn body_preview(data: &str) -> Vec<String> {
    let lines: Vec<&str> = data.lines().collect();
    let mut preview: Vec<String> = lines
        .iter()
        .take(BODY_PREVIEW_LINES)
        .map(|line| (*line).to_string())
        .collect();
    if let Some(hidden) = lines.len().checked_sub(BODY_PREVIEW_LINES).filter(|n| *n > 0) {
        preview.push(format!("… {hidden} more line{}", if hidden == 1 { "" } else { "s" }));
    }
    preview
}

/// Enough to recognize a body by, short enough that the row below it stays on
/// screen in a pane of ordinary height.
const BODY_PREVIEW_LINES: usize = 6;

/// The body pane: the content type on one row, the body itself below it.
///
/// The body is the one field in the TUI drawn as a text *area* — every other
/// one is a single `  > value` row. Hence `Wrap`: a long single-line JSON body
/// is the common case, and clipping it at the pane edge would hide the caret
/// along with the text.
#[allow(clippy::too_many_arguments)]
fn draw_edit_body(
    frame: &mut Frame,
    area: Rect,
    content_type: &str,
    data: &str,
    focused: usize,
    edit: &Edit,
    unsaved: bool,
    error: Option<&str>,
) {
    let indent = "  ";
    let mut lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  content type",
            Style::default()
                .fg(if focused == 0 { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )),
        input_line(content_type, caret(focused == 0, edit), interior(area)),
        Line::raw(""),
        Line::from(Span::styled(
            "  body",
            Style::default()
                .fg(if focused == 1 { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )),
    ];
    // Where the data's first line lands, so the caret's rendered row can be
    // found without counting the rows above it a second time.
    let data_at = lines.len();
    lines.extend(text_area_lines(data, caret(focused == 1, edit), indent));

    if let Some(err) = error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    // This pane wraps, so it scrolls by *rendered* rows rather than by lines,
    // and the caret's row has to be found inside the line holding it: a body
    // that is one long line of JSON is the common case, and pinning the top of
    // that line would leave the caret as far off the bottom as before.
    let width = usize::from(interior(area));
    let (offsets, total) = wrapped_offsets(&lines, width);
    let rows = if focused == 0 {
        FocusRows::at(offsets[2])
    } else {
        let (line, within) = body_caret_row(data, edit.caret, indent, width);
        FocusRows::at(offsets.get(data_at + line).copied().unwrap_or(0) + within)
    };

    let offset = vscroll(total, rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((offset, 0)).block(
        Block::default()
            .title(pane_title(" Body ", unsaved))
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

    // The cursor is always at the end of the buffer, so the row to keep on
    // screen is the last one there is — a pasted command longer than the pane
    // otherwise scrolled its own tail off the bottom.
    let (_, total) = wrapped_offsets(&lines, usize::from(interior(area)));
    let offset = vscroll(
        total,
        FocusRows::at(total.saturating_sub(1)),
        usize::from(interior_height(area)),
    );
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0))
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
    unsaved: bool,
    error: Option<&str>,
    is_edit: bool,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];
    let mut rows = FocusRows::default();

    // Profile name field
    let name_focused = focused == 0;
    let at = lines.len();
    lines.push(Line::from(vec![
        Span::styled(
            "  profile name",
            Style::default()
                .fg(if name_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" *", Style::default().fg(Color::Red)),
    ]));
    lines.push(input_line(name, caret(name_focused, edit), interior(area)));
    rows.mark(name_focused, at, lines.len());
    lines.push(Line::raw(""));

    // Param pairs: focused index 1+2i = key, 2+2i = value
    for (i, (key, value)) in params.iter().enumerate() {
        let key_focused = focused == 1 + 2 * i;
        let val_focused = focused == 2 + 2 * i;

        let at = lines.len();
        lines.push(Line::from(Span::styled(
            format!("  param {} key", i + 1),
            Style::default()
                .fg(if key_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(key, caret(key_focused, edit), interior(area)));
        rows.mark(key_focused, at, lines.len());

        let at = lines.len();
        lines.push(Line::from(Span::styled(
            format!("  param {} value", i + 1),
            Style::default()
                .fg(if val_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(val_focused, edit), interior(area)));
        rows.mark(val_focused, at, lines.len());
        lines.push(Line::raw(""));
    }

    if let Some(err) = error {
        lines.push(Line::from(Span::styled(
            format!("  error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }

    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
        Block::default()
            .title(pane_title(
                if is_edit { " Edit Profile " } else { " New Profile " },
                unsaved,
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(paragraph, area);
}

/// The draft's headers or query params as name/value field pairs.
/// focused: 2i = pairs[i].name, 2i+1 = pairs[i].value
///
/// One renderer for both maps: `kind` supplies the title and the noun, and
/// nothing else about the pane differs.
#[allow(clippy::too_many_arguments)]
fn draw_edit_pairs(
    frame: &mut Frame,
    area: Rect,
    kind: PairKind,
    pairs: &[(String, String)],
    focused: usize,
    edit: &Edit,
    unsaved: bool,
    error: Option<&str>,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];
    let mut rows = FocusRows::default();
    let noun = kind.noun();

    for (i, (name, value)) in pairs.iter().enumerate() {
        let name_focused = focused == 2 * i;
        let value_focused = focused == 2 * i + 1;

        let at = lines.len();
        lines.push(Line::from(Span::styled(
            format!("  {} {} name", noun, i + 1),
            Style::default()
                .fg(if name_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(name, caret(name_focused, edit), interior(area)));
        rows.mark(name_focused, at, lines.len());

        let at = lines.len();
        lines.push(Line::from(Span::styled(
            format!("  {} {} value", noun, i + 1),
            Style::default()
                .fg(if value_focused { Color::Green } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(value_focused, edit), interior(area)));
        rows.mark(value_focused, at, lines.len());
        lines.push(Line::raw(""));
    }

    if pairs.is_empty() {
        lines.push(Line::from(Span::styled(
            kind.empty_hint(),
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

    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
        Block::default()
            .title(pane_title(kind.title(), unsaved))
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
    unsaved: bool,
) {
    let mut lines: Vec<Line<'static>> = vec![Line::raw("")];
    let mut rows = FocusRows::default();

    for (i, profile) in profiles.iter().enumerate() {
        let is_selected = i == selected;
        let marker = if is_selected { "  ▶ " } else { "    " };
        let at = lines.len();
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
        rows.mark(is_selected, at, lines.len());
        lines.push(Line::raw(""));
    }

    let new_selected = selected >= profiles.len();
    rows.mark(new_selected, lines.len(), lines.len() + 1);
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

    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
        Block::default()
            .title(pane_title(&format!(" Profiles — {} ", request_name), unsaved))
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
    let mut rows = FocusRows::default();

    for (i, (name, value)) in vars.iter().enumerate() {
        let is_focused = i == focused;
        let at = lines.len();

        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default()
                .fg(if is_focused { Color::Yellow } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(is_focused, edit), interior(area)));
        rows.mark(is_focused, at, lines.len());
        lines.push(Line::raw(""));
    }

    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
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
    let mut rows = FocusRows::default();

    for (i, (name, value)) in vars.iter().enumerate() {
        let is_focused = i == focused;
        let at = lines.len();
        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default()
                .fg(if is_focused { Color::Yellow } else { Color::DarkGray })
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(input_line(value, caret(is_focused, edit), interior(area)));
        rows.mark(is_focused, at, lines.len());
        lines.push(Line::raw(""));
    }

    let iter_focused = focused == vars.len();
    let at = lines.len();
    lines.push(Line::from(Span::styled(
        "  iterations",
        Style::default()
            .fg(if iter_focused { Color::Yellow } else { Color::DarkGray })
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(input_line(iterations, caret(iter_focused, edit), interior(area)));
    rows.mark(iter_focused, at, lines.len());

    let offset = vscroll(lines.len(), rows, usize::from(interior_height(area)));
    let paragraph = Paragraph::new(lines).scroll((offset, 0)).block(
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
    summary: &str,
    headers: &str,
    show_headers: bool,
    scroll: u16,
    response_filter: &str,
    response_filter_active: bool,
    cursor: usize,
    anchor: Option<usize>,
    status: Option<&str>,
    has_image: bool,
) -> (u16, Option<Rect>) {
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
    // The `H` toggle lives in the title rather than the hint line, which is
    // already 78 of its 80 columns. It doubles as the indicator of which way
    // the toggle currently sits — lit when the headers are in the pane, dim
    // when they are behind it — and is drawn only when there are headers, so
    // it never advertises a key that would do nothing.
    if !headers.is_empty() {
        title_spans.push(Span::styled(
            " H headers ",
            if show_headers {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
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
    // The row the cursor line has to fill: the block's interior, so it runs from
    // border to border however the split divider has been dragged.
    let band_width = chunks[1].width.saturating_sub(2);
    let text = super::response_text(summary, headers, show_headers, body);
    let lines: Vec<Line<'static>> = super::visible_response_lines(&text, response_filter)
        .into_iter()
        .map(colorize_json_line)
        .enumerate()
        .map(|(i, line)| {
            if anchor.is_some() && i >= from && i <= to {
                highlight(line, SELECTION_BG, Band::Text)
            } else if anchor.is_none() && i == cursor {
                highlight(line, CURSOR_LINE_BG, Band::Row(band_width))
            } else {
                line
            }
        })
        .collect();

    let line_count = lines.len() as u16;
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(badge_color));
    // Taken before the block moves into the paragraph, so the image lands
    // inside the border rather than over it.
    let inner = block.inner(chunks[1]);
    let paragraph = Paragraph::new(lines).block(block).scroll((scroll, 0));

    frame.render_widget(paragraph, chunks[1]);

    let image_area = has_image.then(|| image_area(inner, line_count)).flatten();

    (chunks[1].height.saturating_sub(2), image_area)
}

/// The part of the response block an image gets: everything below the body
/// text, with one blank row between them.
///
/// Its own function so the arithmetic is unit-tested, the same reason
/// `split_pct_at` is one. The cases that matter are the degenerate ones — a
/// pane too short for both, or dragged narrow enough to have no interior at
/// all — where the answer has to be `None` rather than a zero-sized or
/// wrapped-around `Rect`.
fn image_area(inner: Rect, text_rows: u16) -> Option<Rect> {
    let offset = text_rows.saturating_add(1).min(inner.height);
    let height = inner.height.saturating_sub(offset);
    (height > 0 && inner.width > 0).then(|| Rect {
        x: inner.x,
        y: inner.y + offset,
        width: inner.width,
        height,
    })
}


fn draw_help(frame: &mut Frame, area: Rect, mode: &Mode) {
    let spans: Vec<Span<'static>> = match mode {
        // Deliberately short. The full keymap lives behind `?`; listing all
        // fifteen bindings here ran to 155 columns, which an 80-column terminal
        // truncates without a mark, hiding the last five outright. These seven
        // come to 61, so there is room but not much — measure before adding an
        // eighth, and put it in `HELP_COLUMNS` instead if it does not fit.
        Mode::Browse => vec![
            Span::styled(" r ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run", Style::default().fg(Color::DarkGray)),
            Span::styled("   t ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("test", Style::default().fg(Color::DarkGray)),
            Span::styled("   n ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("new", Style::default().fg(Color::DarkGray)),
            Span::styled("   e ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("edit", Style::default().fg(Color::DarkGray)),
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
        // 75 columns, and it has to stay under 80 — a terminal that narrow
        // truncates silently, hiding the tail rather than wrapping it. Adding
        // `^q query` to the six entries this used to carry came to 87, so `^g
        // global` gave up its slot: of the six it is the one that loses least,
        // because the row it names renders its own state as `[ ] global` and
        // Enter on it toggles, whereas the chords that open a pane are the ones
        // worth advertising. Anything added here has to be measured first —
        // `^b body` would make it 85, which is why the body pane's accelerator
        // is documented rather than hinted: `Tab` is already listed, and the
        // `body` row it reaches carries its own `Enter to edit`.
        Mode::NewRequest { .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("save", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("fields", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^e ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("headers", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("query", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^p ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("profiles", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        // The noun follows `kind`, so the pane never says "header" while
        // editing query params.
        Mode::EditPairs { kind, .. } => vec![
            Span::styled(" Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("apply", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("fields", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^a ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!("add {}", kind.noun()), Style::default().fg(Color::DarkGray)),
            Span::styled("   ^d ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!("remove {}", kind.noun()), Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ],
        // `^s apply` first, because it is the one key here that is not where a
        // form usually puts it — Enter types a newline in a body, so it cannot
        // also be the key that leaves.
        Mode::EditBody { .. } => vec![
            Span::styled(" ^s ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("apply", Style::default().fg(Color::DarkGray)),
            Span::styled("   Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("newline", Style::default().fg(Color::DarkGray)),
            Span::styled("   Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("fields", Style::default().fg(Color::DarkGray)),
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
            Span::styled("   Tab ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("fields", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^a ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("add param", Style::default().fg(Color::DarkGray)),
            Span::styled("   ^d ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("remove param", Style::default().fg(Color::DarkGray)),
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

/// The prompt that stands between Esc and a request form with unsaved changes.
///
/// Drawn as the keymap overlay is — centered, cleared, bordered — because it
/// interrupts a pane the same way and answers back to it, and a prompt that
/// looked like a different kind of thing would read as one. The border is
/// yellow rather than blue: this one is about to throw something away, and it
/// matches the marker it is explaining.
///
/// The answers are a *list* rather than three letters to memorize: `j`/`k` and
/// the arrows walk it and Enter picks, which is what every other list in the
/// TUI already does, and it puts the consequence of each answer on the screen
/// next to the answer itself.
fn draw_confirm_discard(frame: &mut Frame, area: Rect, selected: DiscardChoice) {
    let mut lines: Vec<Line<'static>> = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "  This request has unsaved changes.",
            Style::default().fg(Color::White),
        )),
        Line::raw(""),
    ];

    for choice in DiscardChoice::ALL {
        let is_selected = choice == selected;
        // The same marker-and-highlight the profile picker uses, so a list
        // looks like a list wherever one turns up.
        let marker = if is_selected { "  ▶ " } else { "    " };
        let label_style = if is_selected {
            Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled(
                format!("{:<width$}", choice.label(), width = DISCARD_LABEL_WIDTH),
                label_style,
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("      {}", choice.hint()),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("j/k", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  move    ", Style::default().fg(Color::DarkGray)),
        Span::styled("Enter", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled("  choose", Style::default().fg(Color::DarkGray)),
    ]));

    let height = lines.len() as u16 + 2; // + borders
    let popup = centered_rect(DISCARD_WIDTH, height, area);

    let widget = Paragraph::new(lines).block(
        Block::default()
            .title(" Unsaved changes ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );

    // Clear first: the form underneath has already been drawn into these cells.
    frame.render_widget(Clear, popup);
    frame.render_widget(widget, popup);
}

/// Wide enough for the longest hint the prompt draws, plus its borders.
const DISCARD_WIDTH: u16 = 46;
/// Pads the highlight to a constant width, so the selected row is a bar rather
/// than a ragged patch the length of whichever answer it happens to be on.
const DISCARD_LABEL_WIDTH: usize = 18;

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

/// Style applied to a `{{VAR}}` placeholder wherever one is shown verbatim.
const VAR_STYLE: Style = Style::new()
    .fg(Color::Magenta)
    .add_modifier(Modifier::BOLD);

/// Splits `text` into spans so `{{VAR}}` placeholders render in [`VAR_STYLE`]
/// and everything else keeps `base`.
///
/// The scan matches `extract_var_names` in `mod.rs` — the pane must highlight
/// exactly what the `VarInput` prompt will ask for, so an unterminated `{{`
/// is left as ordinary text here just as it is skipped there.
fn var_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };

        if start > 0 {
            spans.push(Span::styled(rest[..start].to_string(), base));
        }
        spans.push(Span::styled(
            format!("{{{{{}}}}}", &after[..end]),
            VAR_STYLE,
        ));
        rest = &after[end + 2..];
    }

    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), base));
    }
    spans
}

/// The band under the cursor line, and the one under a selection.
///
/// Two neutral greys, a step apart: the cursor line is always somewhere, so it
/// is the quieter, and entering visual mode lifts it to something you cannot
/// miss. That step is the vim `CursorLine` → `Visual` relationship, and the
/// values are calibrated against it — gruvbox puts its `CursorLine` about ΔE 8
/// from the page and its `Visual` about ΔE 25, and these land at 8.3 and 21.3.
///
/// Indexed rather than named colours, which matters twice over. `Blue` and
/// `DarkGray` are whatever the reader's theme says they are, and that is
/// exactly how the old highlight went wrong — `colorize_json_line` paints
/// punctuation `DarkGray`, so the cursor line's `DarkGray` background erased
/// every brace, colon and comma on the line it was meant to be showing you. A
/// background a foreground can collide with has to be a known value. These come
/// from the **greyscale ramp** (232–255) rather than the colour cube, which is
/// the half of the 256 palette nothing remaps: `base16-shell` and friends
/// rewrite 16–21 to carry their extra shades, so a band living there could
/// become an arbitrary colour and re-create the collision. The ramp also needs
/// no truecolor, so it survives a tmux without `Tc`.
///
/// Grey rather than a hue because a neutral band sits inside any palette — a
/// warm scheme like gruvbox reads these as its own greys (ΔE 2.4 from its
/// `bg1`) and a cold one does the same. When themes land these become the dark
/// theme's values rather than the only ones.
const CURSOR_LINE_BG: Color = Color::Indexed(237); // #3a3a3a, medium grey
const SELECTION_BG: Color = Color::Indexed(240); // #585858, a step lighter

/// How far a band runs, which is the difference between marking a *position*
/// and marking an *extent*.
///
/// The cursor line runs to the end of the row: it says where you are, and a
/// band that stopped at the last character would jump about in width as you
/// moved through lines of different lengths — the one thing a position marker
/// must not do. A selection stops at the end of its text, because its right
/// edge is information: it is showing you exactly what `y` is about to take,
/// and padding it out to the row would claim trailing spaces that are not in
/// any of those lines. This is vim's own distinction between `cursorline` and
/// charwise-ragged `Visual`, and it is the reason the two are one enum rather
/// than a `bool` nobody could read at the call site.
#[derive(Clone, Copy)]
enum Band {
    /// To the end of the row, `u16` wide.
    Row(u16),
    /// To the end of the text.
    Text,
}

/// Re-styles a coloured line for the band it is about to be drawn on.
///
/// The line keeps its syntax colouring — that is what the base-style highlight
/// was protecting, and it is still worth protecting — but every colour goes to
/// its bright twin and the whole line goes bold. Contrast then comes from the
/// ink as well as the page, which is what the two backgrounds alone could not
/// give: `Green` on a dark blue band is legible, and `DarkGray` on one is
/// technically legible and practically not.
///
/// A base `Line::style` only paints the cells its spans occupy, so `Band::Row`
/// is delivered by padding the line with blanks. Lines longer than the pane are
/// clipped by the `Paragraph` as before; the padding only ever adds, and it is
/// added at render time so it never reaches `body` — `y` and `c` copy the lines
/// without it.
///
/// The background and the bold live on the `Line` rather than on each span,
/// because a base style paints behind the spans and patches their modifiers
/// without touching the foregrounds they set.
fn highlight(line: Line<'static>, bg: Color, band: Band) -> Line<'static> {
    let pad = match band {
        Band::Row(width) => (width as usize).saturating_sub(line.width()),
        Band::Text => 0,
    };
    let mut spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| match span.style.fg {
            Some(fg) => {
                let style = span.style.fg(brighten(fg));
                Span::styled(span.content, style)
            }
            // No foreground of its own: the indent, which is spaces.
            None => span,
        })
        .collect();
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    Line::from(spans).style(Style::default().bg(bg).add_modifier(Modifier::BOLD))
}

/// The bright twin of a syntax colour.
///
/// `DarkGray` is the one that is not a twin but a rescue: its bright partner
/// would be `Gray`, which is still dim enough to lose against a highlight, and
/// punctuation is the part of a JSON line you most need in order to read the
/// structure. It goes to `White`.
///
/// Anything already bright, or with no dark twin, is returned unchanged, so
/// this stays safe to run over spans it was not written for.
fn brighten(color: Color) -> Color {
    match color {
        Color::DarkGray => Color::White,
        Color::Red => Color::LightRed,
        Color::Green => Color::LightGreen,
        Color::Yellow => Color::LightYellow,
        Color::Blue => Color::LightBlue,
        Color::Magenta => Color::LightMagenta,
        Color::Cyan => Color::LightCyan,
        Color::Gray => Color::White,
        other => other,
    }
}

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
            let mut url_line = vec![Span::styled(" url    ", Style::default().fg(Color::DarkGray))];
            url_line.extend(var_spans(url, Style::default().fg(Color::Cyan)));
            lines.push(Line::from(url_line));
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
                    let mut row =
                        vec![Span::styled(format!("  {}: ", k), Style::default().fg(Color::Yellow))];
                    row.extend(var_spans(v, Style::default()));
                    lines.push(Line::from(row));
                }
            }
            if !query.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    " Query",
                    Style::default().fg(Color::DarkGray),
                )));
                for (k, v) in query {
                    let mut row =
                        vec![Span::styled(format!("  {}: ", k), Style::default().fg(Color::Yellow))];
                    row.extend(var_spans(v, Style::default()));
                    lines.push(Line::from(row));
                }
            }
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(text, is_placeholder)` per span, which is all the callers care about.
    fn split(text: &str) -> Vec<(String, bool)> {
        var_spans(text, Style::default())
            .into_iter()
            .map(|s| (s.content.to_string(), s.style == VAR_STYLE))
            .collect()
    }

    /// The bug this exists for: `colorize_json_line` paints punctuation
    /// `DarkGray`, and the cursor line's background used to be `DarkGray` too,
    /// so every brace, colon and comma on the highlighted line was drawn in its
    /// own background colour and disappeared.
    #[test]
    fn no_highlighted_foreground_is_its_own_background() {
        let line = highlight(colorize_json_line("  \"name\": \"berry\","), CURSOR_LINE_BG, Band::Text);
        for span in &line.spans {
            assert_ne!(span.style.fg, Some(CURSOR_LINE_BG));
            assert_ne!(span.style.fg, Some(SELECTION_BG));
        }
        assert!(line.spans.iter().any(|s| s.style.fg == Some(Color::White)));
    }

    #[test]
    fn a_highlighted_line_keeps_its_syntax_colours_as_bright_twins() {
        let plain = colorize_json_line("  \"name\": \"berry\",");
        let lit = highlight(plain.clone(), SELECTION_BG, Band::Text);

        assert_eq!(plain.spans.len(), lit.spans.len());
        // The key was Yellow and the string Green; both keep their identity.
        assert_eq!(plain.spans[1].style.fg, Some(Color::Yellow));
        assert_eq!(lit.spans[1].style.fg, Some(Color::LightYellow));
        assert!(plain.spans.iter().any(|s| s.style.fg == Some(Color::Green)));
        assert!(lit.spans.iter().any(|s| s.style.fg == Some(Color::LightGreen)));
    }

    #[test]
    fn the_band_and_the_bold_go_on_the_line_not_the_spans() {
        let line = highlight(colorize_json_line("  \"n\": 42"), CURSOR_LINE_BG, Band::Text);
        assert_eq!(line.style.bg, Some(CURSOR_LINE_BG));
        assert!(line.style.add_modifier.contains(Modifier::BOLD));
        // A span setting its own background would punch a hole in the band.
        assert!(line.spans.iter().all(|s| s.style.bg.is_none()));
    }

    /// The indent is a bare `Span` with no colour of its own; brightening must
    /// leave it alone rather than inventing one.
    #[test]
    fn a_span_with_no_foreground_is_untouched() {
        let line = highlight(colorize_json_line("    \"n\": 42"), CURSOR_LINE_BG, Band::Text);
        assert_eq!(line.spans[0].content.as_ref(), "    ");
        assert_eq!(line.spans[0].style.fg, None);
    }

    /// A line selection has to look like whole lines. Without the padding the
    /// band stopped at the last character, so a selection over lines of
    /// different lengths had a ragged edge and read as highlighted words.
    /// The cursor line marks a position, so its band is the whole row — a band
    /// that stopped at the last character would change width as you moved
    /// through lines of different lengths.
    #[test]
    fn the_cursor_line_band_fills_the_row() {
        let plain = colorize_json_line("  \"n\": 42");
        let text_width = plain.width();
        let line = highlight(plain, CURSOR_LINE_BG, Band::Row(40));
        assert_eq!(line.width(), 40);
        // The pad is the tail, and it is blank: it exists to carry the band,
        // not to add characters a copy would pick up.
        let tail = line.spans.last().unwrap();
        assert_eq!(tail.content.len(), 40 - text_width);
        assert!(tail.content.chars().all(|c| c == ' '));
    }

    /// A selection marks an extent, so its right edge is information: it shows
    /// exactly what `y` is about to take, and must not claim trailing spaces
    /// that are in none of those lines.
    #[test]
    fn a_selection_band_stops_at_the_end_of_the_text() {
        let plain = colorize_json_line("  \"n\": 42");
        let text_width = plain.width();
        let line = highlight(plain, SELECTION_BG, Band::Text);
        assert_eq!(line.width(), text_width);
    }

    /// Padding only ever adds: a line already wider than the pane keeps every
    /// character, and the `Paragraph` clips it exactly as it did before.
    #[test]
    fn a_line_wider_than_the_row_is_left_alone() {
        let long = colorize_json_line("  \"key\": \"a rather long value indeed\",");
        let width = long.width();
        let line = highlight(long, CURSOR_LINE_BG, Band::Row(10));
        assert_eq!(line.width(), width);
    }

    /// The two bands are a step apart in weight, not two arbitrary greys: the
    /// selection is the lighter, which is what makes entering visual mode read
    /// as a change.
    #[test]
    fn the_selection_band_is_lighter_than_the_cursor_line() {
        let grey = |c: Color| match c {
            Color::Indexed(i) => i,
            _ => panic!("bands are indexed greys"),
        };
        assert!(grey(SELECTION_BG) > grey(CURSOR_LINE_BG));
    }

    #[test]
    fn brightening_is_idempotent_and_leaves_unknown_colours_alone() {
        assert_eq!(brighten(brighten(Color::Green)), Color::LightGreen);
        assert_eq!(brighten(Color::LightCyan), Color::LightCyan);
        assert_eq!(brighten(Color::Indexed(42)), Color::Indexed(42));
    }

    /// The two bands have to stay distinguishable — a selection that looked
    /// like the cursor line would make `V` feel like it had done nothing.
    #[test]
    fn the_cursor_line_and_a_selection_are_different_bands() {
        assert_ne!(CURSOR_LINE_BG, SELECTION_BG);
    }

    #[test]
    fn splits_placeholders_from_surrounding_text() {
        assert_eq!(
            split("https://{{HOST}}/v1/{{ID}}"),
            vec![
                ("https://".to_string(), false),
                ("{{HOST}}".to_string(), true),
                ("/v1/".to_string(), false),
                ("{{ID}}".to_string(), true),
            ]
        );
    }

    #[test]
    fn plain_text_stays_one_span() {
        assert_eq!(split("Bearer token"), vec![("Bearer token".to_string(), false)]);
    }

    #[test]
    fn placeholder_may_be_the_whole_value() {
        assert_eq!(split("{{TOKEN}}"), vec![("{{TOKEN}}".to_string(), true)]);
    }

    // An unterminated `{{` is not a variable to `extract_var_names`, so it must
    // not look like one here either.
    #[test]
    fn unterminated_open_is_not_highlighted() {
        assert_eq!(split("a {{B"), vec![("a {{B".to_string(), false)]);
        assert_eq!(
            split("{{A}} then {{B"),
            vec![
                ("{{A}}".to_string(), true),
                (" then {{B".to_string(), false),
            ]
        );
    }

    #[test]
    fn empty_value_yields_no_spans() {
        assert!(split("").is_empty());
    }

    // ─── image_area ───────────────────────────────────────────────────────

    fn inner(width: u16, height: u16) -> Rect {
        Rect { x: 3, y: 5, width, height }
    }

    /// One summary line leaves everything below the blank separator.
    #[test]
    fn image_gets_the_rows_below_the_text() {
        let area = image_area(inner(40, 20), 1).expect("room for an image");
        assert_eq!(area.x, 3, "must stay inside the block's left border");
        assert_eq!(area.y, 5 + 2, "one text row plus one blank separator");
        assert_eq!(area.width, 40);
        assert_eq!(area.height, 18);
    }

    /// The decode-failure summary runs to three lines; the image is gone by
    /// then, but the arithmetic still has to hold for a longer body.
    #[test]
    fn more_text_leaves_less_image() {
        let area = image_area(inner(40, 20), 3).expect("room for an image");
        assert_eq!(area.y, 5 + 4);
        assert_eq!(area.height, 16);
    }

    /// A pane too short for both is text-only rather than a zero-height Rect.
    #[test]
    fn no_room_below_the_text_yields_none() {
        assert_eq!(image_area(inner(40, 2), 1), None);
        assert_eq!(image_area(inner(40, 1), 1), None);
        assert_eq!(image_area(inner(40, 0), 1), None);
    }

    /// The divider can be dragged until the detail pane has no interior at
    /// all. `y` must not run past the block either.
    #[test]
    fn no_width_yields_none() {
        assert_eq!(image_area(inner(0, 20), 1), None);
    }

    /// A body longer than the pane must clamp rather than wrap the offset
    /// past the bottom of the block.
    #[test]
    fn text_taller_than_the_pane_clamps() {
        assert_eq!(image_area(inner(40, 10), 40), None);
        assert_eq!(image_area(inner(40, 10), u16::MAX), None);
    }

    // ─── The body ─────────────────────────────────────────────────────────────

    /// The form has no scrolling, so a long body must not push the rows below
    /// it — `profiles` among them — off the bottom of the pane.
    #[test]
    fn a_long_body_is_previewed_rather_than_printed() {
        let data = (1..=10).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let preview = body_preview(&data);

        assert_eq!(preview.len(), BODY_PREVIEW_LINES + 1);
        assert_eq!(preview[0], "1");
        assert_eq!(preview[BODY_PREVIEW_LINES], "… 4 more lines");
    }

    /// A body that fits gets no note, and the singular reads as English.
    #[test]
    fn a_short_body_is_shown_whole() {
        assert_eq!(body_preview("{\"a\":1}"), vec!["{\"a\":1}".to_string()]);
        assert!(body_preview("").is_empty());

        let data = (0..=BODY_PREVIEW_LINES).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        assert_eq!(body_preview(&data).last().unwrap(), "… 1 more line");
    }

    /// The caret goes on the line that holds it, and an offset at a line's end
    /// is not claimed by the line after it.
    #[test]
    fn a_text_area_puts_the_caret_on_one_line_only() {
        // Caret at the end of "ab\n" is offset 3 — the start of the second line.
        let edit = Edit::at_end("ab\n");
        let lines = text_area_lines("ab\ncd", Some(&edit), "  ");
        assert_eq!(lines.len(), 2);

        // Caret 3 is the first byte of the second line, so the bar is drawn
        // there and the first line is plain text.
        let drawn: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(drawn[0], "  ab");
        assert_eq!(drawn[1], "  │cd");
    }

    /// A body ending in a newline has an empty last line, which is where the
    /// caret sits after pressing Enter.
    #[test]
    fn a_trailing_newline_keeps_its_empty_line() {
        assert_eq!(text_area_lines("a\n", None, "").len(), 2);
    }

    #[test]
    fn a_value_that_fits_is_not_windowed() {
        // Nine characters and a caret glyph in ten columns is exactly the
        // widest a value can be and still be shown whole.
        assert_eq!(window(9, 9, 10), Window { start: 0, end: 9, left: false, right: false });
    }

    #[test]
    fn a_caret_at_the_end_of_a_long_value_stays_visible() {
        let win = window(100, 100, 20);
        assert!(win.start <= 100 && 100 <= win.end, "caret outside {win:?}");
        assert_eq!(win.end, 100);
        assert!(win.left && !win.right);
    }

    #[test]
    fn a_caret_at_the_start_of_a_long_value_stays_visible() {
        let win = window(100, 0, 20);
        assert_eq!(win.start, 0);
        assert!(!win.left && win.right);
    }

    /// The property the whole thing exists for: wherever the caret is, the
    /// window holds it.
    #[test]
    fn the_caret_is_inside_the_window_at_every_position() {
        for width in [4, 5, 12, 40, 80] {
            for len in [0, 1, 7, 40, 200] {
                for caret in 0..=len {
                    let win = window(len, caret, width);
                    assert!(
                        win.start <= caret && caret <= win.end,
                        "len {len} caret {caret} width {width} gave {win:?}",
                    );
                    assert!(win.end <= len);
                }
            }
        }
    }

    /// Both markers are always paid for, so the window is the same size
    /// wherever it sits — text must not shift sideways just because a marker
    /// stopped being drawn.
    #[test]
    fn the_window_is_the_same_width_wherever_it_sits() {
        let widths: Vec<usize> = (0..=200).map(|c| window(200, c, 30)).map(|w| w.end - w.start).collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
    }

    /// A pane dragged down to a sliver has no room to window in; the row is
    /// drawn whole and clipped, exactly as it was before windows existed.
    #[test]
    fn a_pane_too_narrow_to_window_is_left_alone() {
        assert_eq!(window(50, 30, 3), Window { start: 0, end: 50, left: false, right: false });
    }

    #[test]
    fn a_windowed_row_marks_the_text_it_hides() {
        let edit = Edit::at_end(&"x".repeat(60));
        let line = input_line(&"x".repeat(60), Some(&edit), 24);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.starts_with("  > ‹"), "{rendered}");
        // The caret is at the end, so there is nothing hidden to its right.
        assert!(!rendered.contains('›'), "{rendered}");
        assert!(rendered.chars().count() <= 24, "{rendered}");
    }

    /// An unfocused row has no caret, and so no one place that has to be on
    /// screen — it is drawn whole and clipped by the pane.
    #[test]
    fn an_unfocused_row_is_never_windowed() {
        let line = input_line(&"x".repeat(60), None, 24);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rendered, format!("  > {}", "x".repeat(60)));
    }

    /// The window is measured in characters, so a multi-byte value must not
    /// slice one in half.
    #[test]
    fn a_multibyte_value_is_sliced_on_char_boundaries() {
        let value = "é".repeat(60);
        let edit = Edit::at_end(&value);
        let line = input_line(&value, Some(&edit), 24);
        let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains('é'));
    }

    // ---- vertical scrolling ----------------------------------------------

    /// Nothing is scrolled while everything fits: the offset exists to rescue a
    /// row off the bottom, not to move a pane that has room for all of them.
    #[test]
    fn a_pane_that_fits_is_never_scrolled() {
        assert_eq!(vscroll(10, FocusRows::at(9), 20), 0);
        assert_eq!(vscroll(20, FocusRows::at(19), 20), 0);
    }

    /// The property the whole thing exists for: wherever focus is, its row is
    /// on the screen.
    #[test]
    fn the_focused_row_is_on_screen_at_every_position() {
        for height in [1, 2, 7, 24, 60] {
            for total in [0, 1, 9, 24, 200] {
                for row in 0..total {
                    let offset = usize::from(vscroll(total, FocusRows::at(row), height));
                    assert!(
                        offset <= row && row < offset + height,
                        "total {total} row {row} height {height} gave {offset}",
                    );
                    assert!(offset + height <= total.max(height));
                }
            }
        }
    }

    /// The last row scrolls to the end of the content and no further — a pane
    /// showing blank rows below the last line has scrolled past it.
    #[test]
    fn the_last_row_stops_at_the_end_of_the_content() {
        assert_eq!(vscroll(100, FocusRows::at(99), 20), 80);
    }

    /// A block taller than the pane shows its top. Centring it would put the
    /// label and the input row above the screen, which is the thing being
    /// fixed rather than a new way to draw it.
    #[test]
    fn a_block_taller_than_the_pane_shows_its_top() {
        assert_eq!(vscroll(200, FocusRows { start: 40, end: 90 }, 20), 40);
    }

    /// A pane dragged to no interior at all has nowhere to scroll to.
    #[test]
    fn a_pane_with_no_height_is_not_scrolled() {
        assert_eq!(vscroll(50, FocusRows::at(40), 0), 0);
    }

    // ---- wrapping ---------------------------------------------------------

    #[test]
    fn a_line_that_fits_is_one_row() {
        assert_eq!(wrap_starts("short", 20), vec![0]);
        assert_eq!(wrap_starts("", 20), vec![0]);
    }

    /// Breaking at the last space that fits, as `Wrap` does.
    #[test]
    fn a_long_line_breaks_at_a_space() {
        // "aaa bbb ccc" in 8 columns: "aaa bbb " then "ccc".
        assert_eq!(wrap_starts("aaa bbb ccc", 8), vec![0, 8]);
    }

    /// A word wider than the pane has no space to break at, so it is broken
    /// where the row ends — a URL or a one-line JSON body is the common case.
    #[test]
    fn a_word_wider_than_the_pane_is_broken_mid_word() {
        assert_eq!(wrap_starts(&"x".repeat(25), 10), vec![0, 10, 20]);
    }

    /// A pane with no interior cannot be divided into rows.
    #[test]
    fn a_zero_width_pane_leaves_one_row() {
        assert_eq!(wrap_starts("anything at all", 0), vec![0]);
    }

    // ---- the body pane's caret --------------------------------------------

    /// A caret at a line's end belongs to that line, matching the rule
    /// `text_area_lines` places it by.
    #[test]
    fn a_caret_at_a_lines_end_belongs_to_that_line() {
        assert_eq!(caret_line_col("ab\ncd", 2), (0, 2));
        assert_eq!(caret_line_col("ab\ncd", 3), (1, 0));
        assert_eq!(caret_line_col("ab\ncd", 5), (1, 2));
    }

    /// The column is in characters, since that is what the wrap is measured in.
    #[test]
    fn the_caret_column_counts_characters_not_bytes() {
        let value = "ééé";
        assert_eq!(caret_line_col(value, value.len()), (0, 3));
    }

    /// The case the body pane's arithmetic exists for: one long line of JSON,
    /// with the caret at the end of it. Counting only lines would put it on row
    /// zero and leave the caret as far off the bottom as before.
    #[test]
    fn a_caret_deep_in_a_wrapped_line_is_rows_below_its_lines_start() {
        let data = "x".repeat(400);
        let (line, within) = body_caret_row(&data, data.len(), "  ", 40);
        assert_eq!(line, 0);
        assert_eq!(within, 10);
    }

    /// And with real lines above it, both halves count.
    #[test]
    fn a_caret_on_a_later_line_reports_that_line() {
        let data = "one\ntwo\nthree";
        let (line, within) = body_caret_row(data, data.len(), "  ", 40);
        assert_eq!((line, within), (2, 0));
    }

    // ---- the panes themselves ---------------------------------------------

    /// What the pane drew, in a terminal of the given size.
    fn drawn(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut Frame, Rect),
    ) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, frame.area())).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Everything the pane drew, as one string.
    fn rendered(width: u16, height: u16, draw: impl FnOnce(&mut Frame, Rect)) -> String {
        drawn(width, height, draw)
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn form_fields() -> Vec<(String, String)> {
        ["name", "method", "url", "extract"]
            .iter()
            .map(|label| ((*label).to_string(), String::new()))
            .collect()
    }

    /// The request form in a pane too short for it, with `focused` on one row.
    fn form(focused: usize, fields: &[(String, String)]) -> String {
        let edit = Edit::default();
        let headers = std::collections::HashMap::new();
        let query = std::collections::HashMap::new();
        rendered(40, 12, |frame: &mut Frame, area: Rect| {
            draw_new_request(
                frame, area, fields, focused, &edit, &[], &headers, &query, None, false, false,
                false, None,
            );
        })
    }

    /// The request form drawn wide enough for its title, dirty or not.
    fn titled_form(dirty: bool) -> String {
        let fields = form_fields();
        let edit = Edit::default();
        let headers = std::collections::HashMap::new();
        let query = std::collections::HashMap::new();
        rendered(60, 12, |frame: &mut Frame, area: Rect| {
            draw_new_request(
                frame, area, &fields, 0, &edit, &[], &headers, &query, None, true, false, dirty,
                None,
            );
        })
    }

    /// The cue the form owes anyone coming back from a sub-pane: the edits are
    /// in the draft and nothing has written them yet.
    #[test]
    fn an_unsaved_form_says_so_in_its_title() {
        assert!(titled_form(true).contains("unsaved"), "{}", titled_form(true));
    }

    /// And says nothing when there is nothing to say — a marker that is always
    /// there is not a marker.
    #[test]
    fn a_saved_form_has_no_marker() {
        assert!(!titled_form(false).contains("unsaved"), "{}", titled_form(false));
    }

    /// Every form pane carries the same marker, not just the request form: the
    /// sub-panes are where the changes get made, and coming back through four
    /// of them without one is how the edits got lost in the first place.
    #[test]
    fn every_form_pane_can_show_the_marker() {
        let edit = Edit::default();
        let pairs = [("Accept".to_string(), "application/json".to_string())];
        let panes: Vec<(&str, String)> = vec![
            ("headers", rendered(60, 12, |f: &mut Frame, a: Rect| {
                draw_edit_pairs(f, a, PairKind::Headers, &pairs, 0, &edit, true, None)
            })),
            ("body", rendered(60, 12, |f: &mut Frame, a: Rect| {
                draw_edit_body(f, a, "application/json", "{}", 0, &edit, true, None)
            })),
            ("profile list", rendered(60, 12, |f: &mut Frame, a: Rect| {
                draw_profile_list(f, a, &[], 0, "api", true)
            })),
            ("profile", rendered(60, 12, |f: &mut Frame, a: Rect| {
                draw_new_profile(f, a, "dev", &[], 0, &edit, true, None, false)
            })),
        ];
        for (name, pane) in panes {
            assert!(pane.contains("unsaved"), "{name}: {pane}");
        }
    }

    /// And says nothing in any of them when there is nothing to say.
    #[test]
    fn a_clean_sub_pane_has_no_marker() {
        let edit = Edit::default();
        let pane = rendered(60, 12, |f: &mut Frame, a: Rect| {
            draw_edit_pairs(f, a, PairKind::Headers, &[], 0, &edit, false, None)
        });
        assert!(!pane.contains("unsaved"), "{pane}");
    }

    /// The prompt names all three answers and the keys that walk them: a list
    /// whose bindings are not on it is one people guess at.
    #[test]
    fn the_discard_prompt_offers_save_discard_and_cancel() {
        let pane = rendered(60, 24, |frame: &mut Frame, area: Rect| {
            draw_confirm_discard(frame, area, DiscardChoice::Save)
        });
        assert!(pane.contains("unsaved changes"), "{pane}");
        for choice in DiscardChoice::ALL {
            assert!(pane.contains(choice.label()), "{pane}");
        }
        // The keys that walk it, so nobody has to guess at j/k.
        assert!(pane.contains("j/k"), "{pane}");
        assert!(pane.contains("Enter"), "{pane}");
    }

    /// What the issue was: the request form is taller than the pane, so the
    /// last Tab stops were simply not drawn and focus went somewhere invisible.
    #[test]
    fn a_focused_row_below_the_pane_is_drawn_anyway() {
        let fields = form_fields();

        // The first field is on screen either way; the last action row is
        // twenty-odd lines below the top of a twelve-row pane.
        assert!(form(0, &fields).contains("name"));
        let last = form(fields.len() + 4, &fields);
        assert!(last.contains("profiles"), "{last}");
    }

    /// The pane still starts at the top while the focus is near it: scrolling
    /// on the first field would hide the title row for nothing.
    #[test]
    fn a_form_focused_at_the_top_is_not_scrolled() {
        let top = form(0, &form_fields());
        assert!(top.contains("name"), "{top}");
        assert!(top.contains("method"), "{top}");
    }

    /// The body pane wraps, so its caret is rows rather than lines below the
    /// top — a long one-line JSON body is the case the row arithmetic is for.
    #[test]
    fn a_caret_at_the_end_of_a_long_body_is_on_screen() {
        let data = "x".repeat(600);
        let edit = Edit::landing(&data, false);
        let pane = drawn(30, 10, |frame: &mut Frame, area: Rect| {
            draw_edit_body(frame, area, "application/json", &data, 1, &edit, false, None);
        });
        // In normal mode the caret is the character it rests on, drawn in
        // reverse — nothing else in this pane has a white background.
        assert!(
            pane.content.iter().any(|cell| cell.bg == Color::White),
            "no caret drawn",
        );
    }

    /// The same body with the caret back at the top: the pane must not have
    /// scrolled away from it.
    #[test]
    fn a_caret_at_the_start_of_a_long_body_is_on_screen() {
        let data = "x".repeat(600);
        let edit = Edit::landing("", false);
        let pane = drawn(30, 10, |frame: &mut Frame, area: Rect| {
            draw_edit_body(frame, area, "application/json", &data, 1, &edit, false, None);
        });
        assert!(
            pane.content.iter().any(|cell| cell.bg == Color::White),
            "no caret drawn",
        );
    }
}
