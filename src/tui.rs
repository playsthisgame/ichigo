use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Bar, BarChart, BarGroup, Block, Borders, List, ListItem, ListState, Paragraph, Sparkline},
    Frame, Terminal,
};
use std::{collections::HashMap, fs, io};

use crate::config::{list_requests, resolve_config_path, ChainConfig, RequestConfig};

// ─── Data ─────────────────────────────────────────────────────────────────────

enum EntryKind {
    Request {
        method: String,
        url: String,
        description: Option<String>,
        headers: HashMap<String, String>,
        query: HashMap<String, String>,
        body_data: Option<String>,
    },
    Chain {
        steps: Vec<String>,
    },
}

struct Entry {
    name: String,
    global: bool,
    kind: EntryKind,
}

fn load_entries() -> Result<Vec<Entry>> {
    let raw = list_requests()?;
    let mut entries = Vec::new();
    for r in raw {
        let path = resolve_config_path(&r.name).unwrap();
        let content = fs::read_to_string(&path)?;
        let kind = if content.contains("steps:") {
            let chain: ChainConfig = serde_yaml::from_str(&content)
                .unwrap_or(ChainConfig { name: r.name.clone(), steps: vec![] });
            EntryKind::Chain {
                steps: chain.steps.iter().map(|s| s.name.clone()).collect(),
            }
        } else {
            let config: RequestConfig = serde_yaml::from_str(&content)?;
            EntryKind::Request {
                method: config.method,
                url: config.url,
                description: config.description,
                headers: config.headers,
                query: config.query,
                body_data: config.body.map(|b| b.data),
            }
        };
        entries.push(Entry { name: r.name, global: r.global, kind });
    }
    Ok(entries)
}

// ─── Modes ────────────────────────────────────────────────────────────────────

enum Mode {
    Browse,
    VarInput {
        entry_name: String,
        vars: Vec<(String, String)>, // (placeholder name, value being typed)
        focused: usize,              // which field the cursor is in
    },
    TestInput {
        entry_name: String,
        vars: Vec<(String, String)>,
        iterations: String,
        focused: usize, // 0..vars.len() = var fields, vars.len() = iterations field
    },
    Response {
        status: u16,
        body: String,
        scroll: u16,
    },
    TestResponse {
        results: crate::tester::TestResults,
    },
}

// ─── App state ────────────────────────────────────────────────────────────────

struct App {
    entries: Vec<Entry>,
    list_state: ListState,
    pending_g: bool,
    mode: Mode,
}

impl App {
    fn new() -> Result<Self> {
        let entries = load_entries()?;
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0));
        }
        Ok(Self { entries, list_state, pending_g: false, mode: Mode::Browse })
    }

    fn move_down(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) => (i + 1).min(self.entries.len() - 1),
            None => 0,
        };
        self.list_state.select(Some(next));
    }

    fn move_up(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.list_state.select(Some(next));
    }

    fn move_top(&mut self) {
        if !self.entries.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn move_bottom(&mut self) {
        if !self.entries.is_empty() {
            self.list_state.select(Some(self.entries.len() - 1));
        }
    }

    fn try_run_selected(&mut self) {
        let Some(idx) = self.list_state.selected() else { return };
        let entry = &self.entries[idx];

        // Clone everything we need before mutating self.mode.
        // With NLL, the borrow of `entry` ends after this block.
        let entry_name = entry.name.clone();
        let (var_names, is_chain) = match &entry.kind {
            EntryKind::Chain { .. } => (vec![], true),
            EntryKind::Request { url, headers, query, body_data, .. } => {
                (extract_var_names(url, headers, query, body_data.as_deref()), false)
            }
        };

        if is_chain {
            self.mode = Mode::Response {
                status: 0,
                body: "Chain execution is not supported in the TUI yet.".to_string(),
                scroll: 0,
            };
            return;
        }

        // Pre-fill any var that's already set in the environment.
        let vars: Vec<(String, String)> = var_names
            .into_iter()
            .map(|name| {
                let value = std::env::var(&name).unwrap_or_default();
                (name, value)
            })
            .collect();

        if vars.is_empty() {
            self.execute_request(&entry_name, &HashMap::new());
        } else {
            self.mode = Mode::VarInput { entry_name, vars, focused: 0 };
        }
    }

    fn try_test_selected(&mut self) {
        let Some(idx) = self.list_state.selected() else { return };
        let entry = &self.entries[idx];

        let entry_name = entry.name.clone();
        let is_chain = matches!(&entry.kind, EntryKind::Chain { .. });

        if is_chain {
            self.mode = Mode::Response {
                status: 0,
                body: "Testing chains is not supported.".to_string(),
                scroll: 0,
            };
            return;
        }

        let var_names = match &entry.kind {
            EntryKind::Request { url, headers, query, body_data, .. } => {
                extract_var_names(url, headers, query, body_data.as_deref())
            }
            _ => vec![],
        };

        let vars: Vec<(String, String)> = var_names
            .into_iter()
            .map(|name| {
                let value = std::env::var(&name).unwrap_or_default();
                (name, value)
            })
            .collect();

        self.mode = Mode::TestInput { entry_name, vars, iterations: "10".to_string(), focused: 0 };
    }

    fn execute_test(&mut self, entry_name: &str, vars: &HashMap<String, String>, iterations: usize) {
        match RequestConfig::load(entry_name)
            .and_then(|config| crate::tester::collect_test_results(&config, vars, iterations))
        {
            Ok(results) => self.mode = Mode::TestResponse { results },
            Err(e) => self.mode = Mode::Response { status: 0, body: format!("Error: {e}"), scroll: 0 },
        }
    }

    fn execute_request(&mut self, entry_name: &str, vars: &HashMap<String, String>) {
        let result = RequestConfig::load(entry_name)
            .and_then(|config| crate::utils::send_request(&config, vars, false));

        let (status, body) = match result {
            Ok(response) => {
                let status = response.status().as_u16();
                let text = response.text().unwrap_or_else(|e| e.to_string());
                // Pretty-print JSON if the body parses as such.
                let body = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or(text);
                (status, body)
            }
            Err(e) => (0, format!("Error: {e}")),
        };

        self.mode = Mode::Response { status, body, scroll: 0 };
    }

    // Returns true when the event loop should exit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if matches!(self.mode, Mode::Browse) {
            return self.handle_key_browse(key.code);
        }
        if matches!(self.mode, Mode::VarInput { .. }) {
            return self.handle_key_var_input(key);
        }
        if matches!(self.mode, Mode::TestInput { .. }) {
            return self.handle_key_test_input(key);
        }
        self.handle_key_response(key.code)
    }

    fn handle_key_browse(&mut self, code: KeyCode) -> bool {
        let was_pending_g = self.pending_g;
        self.pending_g = false;
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('G') => self.move_bottom(),
            KeyCode::Char('g') => {
                if was_pending_g {
                    self.move_top();
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('r') | KeyCode::Enter => self.try_run_selected(),
            KeyCode::Char('t') => self.try_test_selected(),
            _ => {}
        }
        false
    }

    fn handle_key_var_input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Enter => {
                // Clone out the data we need; the borrow of self.mode ends
                // when this block exits, letting execute_request take &mut self.
                let (entry_name, var_map) = match &self.mode {
                    Mode::VarInput { entry_name, vars, .. } => (
                        entry_name.clone(),
                        vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>(),
                    ),
                    _ => return false,
                };
                self.execute_request(&entry_name, &var_map);
            }
            KeyCode::Tab => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len();
                    *focused = (*focused + 1) % len;
                }
            }
            KeyCode::BackTab => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len();
                    *focused = focused.checked_sub(1).unwrap_or(len - 1);
                }
            }
            KeyCode::Char(c) => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    vars[*focused].1.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Mode::VarInput { vars, focused, .. } = &mut self.mode {
                    vars[*focused].1.pop();
                }
            }
            _ => {}
        }
        false
    }

    fn handle_key_test_input(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Enter => {
                let (entry_name, var_map, iterations) = match &self.mode {
                    Mode::TestInput { entry_name, vars, iterations, .. } => (
                        entry_name.clone(),
                        vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<_, _>>(),
                        iterations.parse::<usize>().unwrap_or(10),
                    ),
                    _ => return false,
                };
                self.execute_test(&entry_name, &var_map, iterations);
            }
            KeyCode::Tab => {
                if let Mode::TestInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len() + 1;
                    *focused = (*focused + 1) % len;
                }
            }
            KeyCode::BackTab => {
                if let Mode::TestInput { vars, focused, .. } = &mut self.mode {
                    let len = vars.len() + 1;
                    *focused = focused.checked_sub(1).unwrap_or(len - 1);
                }
            }
            KeyCode::Char(c) => {
                if let Mode::TestInput { vars, focused, iterations, .. } = &mut self.mode {
                    if *focused < vars.len() {
                        vars[*focused].1.push(c);
                    } else if c.is_ascii_digit() {
                        iterations.push(c);
                    }
                }
            }
            KeyCode::Backspace => {
                if let Mode::TestInput { vars, focused, iterations, .. } = &mut self.mode {
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

    fn handle_key_response(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Response { scroll, .. } = &mut self.mode {
                    *scroll = scroll.saturating_add(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Response { scroll, .. } = &mut self.mode {
                    *scroll = scroll.saturating_sub(1);
                }
            }
            KeyCode::Char('c') => {
                let text = match &self.mode {
                    Mode::Response { body, .. } => body.clone(),
                    Mode::TestResponse { results } => format_test_results_text(results),
                    _ => return false,
                };
                copy_to_clipboard(&text);
            }
            _ => {}
        }
        false
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    let mut app = App::new()?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if let Event::Key(key) = event::read()?
            && app.handle_key(key)
        {
            break;
        }
    }
    Ok(())
}

// ─── Drawing ──────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[0]);

    // Pass list_state by field so the mutable borrow ends before we read
    // app.mode and app.entries below.
    draw_list(frame, panes[0], &app.entries, &mut app.list_state);

    match &app.mode {
        Mode::Browse => {
            // Accessing app.list_state and app.entries here is fine: they are
            // different fields from app.mode, and Rust can split-borrow them.
            let selected = app.list_state.selected().map(|i| &app.entries[i]);
            draw_detail_browse(frame, panes[1], selected);
        }
        Mode::VarInput { vars, focused, entry_name } => {
            draw_var_input(frame, panes[1], vars, *focused, entry_name);
        }
        Mode::TestInput { vars, focused, iterations, entry_name } => {
            draw_test_input(frame, panes[1], vars, iterations, *focused, entry_name);
        }
        Mode::Response { status, body, scroll } => {
            draw_response(frame, panes[1], *status, body, *scroll);
        }
        Mode::TestResponse { results } => {
            draw_test_results(frame, panes[1], results);
        }
    }

    draw_help(frame, outer[1], &app.mode);
}

fn draw_list(frame: &mut Frame, area: Rect, entries: &[Entry], list_state: &mut ListState) {
    let items: Vec<ListItem> = entries.iter().map(make_list_item).collect();
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
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, list_state);
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

fn draw_var_input(
    frame: &mut Frame,
    area: Rect,
    vars: &[(String, String)],
    focused: usize,
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

        let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
        // Append a block cursor character to the focused field's text.
        let display = if is_focused { format!("{}█", value) } else { value.clone() };
        let input = Span::styled(
            display,
            Style::default().fg(if is_focused { Color::White } else { Color::DarkGray }),
        );
        lines.push(Line::from(vec![prompt, input]));
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
        let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
        let display = if is_focused { format!("{}█", value) } else { value.clone() };
        let input = Span::styled(
            display,
            Style::default().fg(if is_focused { Color::White } else { Color::DarkGray }),
        );
        lines.push(Line::from(vec![prompt, input]));
        lines.push(Line::raw(""));
    }

    let iter_focused = focused == vars.len();
    lines.push(Line::from(Span::styled(
        "  iterations",
        Style::default()
            .fg(if iter_focused { Color::Yellow } else { Color::DarkGray })
            .add_modifier(Modifier::BOLD),
    )));
    let prompt = Span::styled("  > ", Style::default().fg(Color::DarkGray));
    let display = if iter_focused { format!("{}█", iterations) } else { iterations.to_string() };
    let input = Span::styled(
        display,
        Style::default().fg(if iter_focused { Color::White } else { Color::DarkGray }),
    );
    lines.push(Line::from(vec![prompt, input]));

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

    // Status distribution bar chart
    let max_count = results.statuses.iter().map(|s| s.count as u64).max().unwrap_or(1);
    let bars: Vec<Bar> = results
        .statuses
        .iter()
        .map(|sc| {
            let color = status_color(sc.status);
            Bar::default()
                .value(sc.count as u64)
                .label(Line::from(sc.status.to_string()))
                .style(Style::default().fg(color))
                .value_style(Style::default().fg(Color::Black).bg(color))
        })
        .collect();
    let bar_chart = BarChart::default()
        .block(
            Block::default()
                .title(" Status Distribution ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .data(BarGroup::default().bars(&bars))
        .bar_width(7)
        .bar_gap(2)
        .max(max_count);
    frame.render_widget(bar_chart, chunks[0]);

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

    // Per-iteration latency sparkline
    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .title(" Latency (ms) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .data(results.timings.iter().copied().collect::<Vec<u64>>())
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(sparkline, chunks[2]);
}

fn status_color(status: u16) -> Color {
    if status < 300 { Color::Green }
    else if status < 400 { Color::Cyan }
    else if status < 500 { Color::Yellow }
    else { Color::Red }
}

fn draw_response(frame: &mut Frame, area: Rect, status: u16, body: &str, scroll: u16) {
    let (status_color, status_label) = if status == 0 {
        (Color::Red, " Error ".to_string())
    } else if status < 300 {
        (Color::Green, format!(" {} ", status))
    } else if status < 400 {
        (Color::Cyan, format!(" {} ", status))
    } else if status < 500 {
        (Color::Yellow, format!(" {} ", status))
    } else {
        (Color::Red, format!(" {} ", status))
    };

    let title = Line::from(vec![
        Span::raw(" Response "),
        Span::styled(status_label, Style::default().fg(Color::Black).bg(status_color)),
    ]);

    let lines: Vec<Line<'static>> = body.lines().map(colorize_json_line).collect();

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(status_color)),
        )
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

fn draw_help(frame: &mut Frame, area: Rect, mode: &Mode) {
    let spans: Vec<Span<'static>> = match mode {
        Mode::Browse => vec![
            Span::styled(" q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quit", Style::default().fg(Color::DarkGray)),
            Span::styled("   j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("navigate", Style::default().fg(Color::DarkGray)),
            Span::styled("   gg ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("top", Style::default().fg(Color::DarkGray)),
            Span::styled("   G ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("bottom", Style::default().fg(Color::DarkGray)),
            Span::styled("   r/Enter ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("run", Style::default().fg(Color::DarkGray)),
            Span::styled("   t ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("test", Style::default().fg(Color::DarkGray)),
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
        Mode::Response { .. } | Mode::TestResponse { .. } => vec![
            Span::styled(" j/k ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("scroll", Style::default().fg(Color::DarkGray)),
            Span::styled("   c ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("copy", Style::default().fg(Color::DarkGray)),
            Span::styled("   Esc ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("back", Style::default().fg(Color::DarkGray)),
            Span::styled("   q ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("quit", Style::default().fg(Color::DarkGray)),
        ],
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
    if trimmed.starts_with('"') {
        if let Some(pos) = trimmed.find("\": ") {
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
    }

    // Array value or plain text
    let mut spans = vec![Span::raw(indent)];
    spans.extend(colorize_json_value(trimmed));
    Line::from(spans)
}

fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
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

// Scans all string fields of a config for {{NAME}} placeholders, preserving
// the order they first appear and deduplicating. Mirrors the logic in utils::interpolate.
fn extract_var_names(
    url: &str,
    headers: &HashMap<String, String>,
    query: &HashMap<String, String>,
    body_data: Option<&str>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let sources = std::iter::once(url)
        .chain(headers.values().map(String::as_str))
        .chain(query.values().map(String::as_str))
        .chain(body_data);

    for s in sources {
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            rest = &rest[start + 2..];
            if let Some(end) = rest.find("}}") {
                let name = rest[..end].to_string();
                if seen.insert(name.clone()) {
                    names.push(name);
                }
                rest = &rest[end + 2..];
            }
        }
    }

    names
}

fn format_test_results_text(results: &crate::tester::TestResults) -> String {
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

fn make_list_item(entry: &Entry) -> ListItem<'static> {
    let scope = if entry.global { " global" } else { " local" };
    let line = match &entry.kind {
        EntryKind::Chain { .. } => Line::from(vec![
            Span::styled(
                " CHAIN ",
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(scope, Style::default().fg(Color::DarkGray)),
        ]),
        EntryKind::Request { method, description, .. } => {
            let color = method_color(method);
            let mut spans = vec![
                Span::styled(
                    format!(" {:<6}", method),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ];
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

fn make_detail_lines(entry: &Entry) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![];

    match &entry.kind {
        EntryKind::Chain { steps } => {
            lines.push(Line::from(vec![
                Span::styled(
                    " CHAIN ",
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ]));
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
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", method),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ]));
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
