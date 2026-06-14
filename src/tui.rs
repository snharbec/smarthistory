use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use rusqlite::{params, Connection};
use std::time::Duration;

use crate::util::{format_diff, format_time};

/// Search scope for the TUI. Mirrors the line-editor widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sess,
    Dir,
    Global,
}

impl Mode {
    fn next(self) -> Self {
        match self {
            Mode::Sess => Mode::Dir,
            Mode::Dir => Mode::Global,
            Mode::Global => Mode::Sess,
        }
    }
    /// Parse a string like "SESS", "SESSION", "DIR", "DIRECTORY",
    /// "GLOBAL" (case-insensitive). Returns None for anything else.
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SESS" | "SESSION" => Some(Mode::Sess),
            "DIR" | "DIRECTORY" => Some(Mode::Dir),
            "GLOBAL" => Some(Mode::Global),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields are kept for future display
struct HistoryRow {
    id: i64,
    command: String,
    directory: String,
    session_id: String,
    exit_code: i32,
    timestamp: i64,
    comment: String,
    output: String,
}

/// How the parent shell should treat the chosen command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickMode {
    /// `Enter` — run the command (parent should submit the line).
    Run,
    /// `Left` — prefill the line for editing, cursor at the start.
    EditStart,
    /// `Right` — prefill the line for editing, cursor at the end.
    EditEnd,
}

/// Exit codes returned by the TUI binary, also used by the line-editor
/// widget to dispatch on. The shell snippet in `init zsh` reads these
/// to decide what to do with the chosen command.
pub mod exit_code {
    /// User pressed `Enter` — run the command (parent should submit
    /// the line).
    pub const RUN: i32 = 0;
    /// User pressed `Esc` / `Ctrl+C` — cancel, no command was chosen.
    pub const CANCEL: i32 = 1;
    /// User pressed `Right` — prefill the line for editing, cursor at
    /// the end.
    pub const EDIT_END: i32 = 2;
    /// User pressed `Left` — prefill the line for editing, cursor at
    /// the start.
    pub const EDIT_START: i32 = 3;
}

impl PickMode {
    fn exit_code(self) -> i32 {
        match self {
            PickMode::Run => exit_code::RUN,
            PickMode::EditEnd => exit_code::EDIT_END,
            PickMode::EditStart => exit_code::EDIT_START,
        }
    }
}

/// Consistent color palette and styles for the TUI.
struct Theme;

impl Theme {
    const BG: Color = Color::Black;
    const FG: Color = Color::Gray;
    const ACCENT: Color = Color::Cyan;
    const SUCCESS: Color = Color::Green;
    const ERROR: Color = Color::Red;
    const WARNING: Color = Color::Yellow;
    const DIM: Color = Color::DarkGray;
    const HIGHLIGHT: Color = Color::Yellow;

    fn default() -> Style {
        Style::default().fg(Self::FG).bg(Self::BG)
    }

    fn accent() -> Style {
        Style::default().fg(Self::ACCENT)
    }

    fn success() -> Style {
        Style::default().fg(Self::SUCCESS)
    }

    fn error() -> Style {
        Style::default().fg(Self::ERROR)
    }

    fn dim() -> Style {
        Style::default().fg(Self::DIM)
    }

    fn highlight() -> Style {
        Style::default().fg(Self::HIGHLIGHT)
    }
}

/// Filter by exit status. Cycled with Ctrl+S in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `label` kept for future use (e.g. larger displays)
enum ExitFilter {
    /// No exit-code filter.
    All,
    /// Only successful commands (exit_code == 0).
    Success,
    /// Only failed commands (exit_code != 0).
    Failed,
}

impl ExitFilter {
    fn next(self) -> Self {
        match self {
            ExitFilter::All => ExitFilter::Success,
            ExitFilter::Success => ExitFilter::Failed,
            ExitFilter::Failed => ExitFilter::All,
        }
    }
}

struct App {
    conn: Connection,
    mode: Mode,
    exit_filter: ExitFilter,
    query: String,
    rows: Vec<HistoryRow>,
    list_state: ListState,
    selection: Option<String>,
    pick_mode: Option<PickMode>,
    cancelled: bool,
    /// When `Some`, we are editing the comment of a history row.
    /// The `String` is the live edit buffer.
    comment_edit: Option<String>,
    /// When `Some`, we are viewing the captured output of a history
    /// row in a full-screen overlay.
    output_view: Option<String>,
}

impl App {
    fn new(conn: Connection, initial_mode: Mode, initial_query: String) -> Self {
        let list_state = ListState::default();
        let mut app = App {
            conn,
            mode: initial_mode,
            exit_filter: ExitFilter::All,
            query: initial_query,
            rows: Vec::new(),
            list_state,
            selection: None,
            pick_mode: None,
            cancelled: false,
            comment_edit: None,
            output_view: None,
        };
        app.refresh();
        // Rows are ordered newest first; index 0 is the newest entry.
        // Keep the selection on the newest match so it appears at the
        // bottom of the bottom-aligned list.
        if !app.rows.is_empty() {
            app.list_state.select(Some(0));
        }
        app
    }

    /// Re-query the database with the current mode + query.
    /// After re-querying, land on the newest match (index 0).
    fn refresh(&mut self) {
        self.rows = self.fetch().unwrap_or_default();
        if self.rows.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    fn fetch(&self) -> Result<Vec<HistoryRow>> {
        let (where_clause, params) = self.build_where();
        let sql = format!(
            "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output \
             FROM history h \
             LEFT JOIN command_comments c ON h.command = c.command \
             LEFT JOIN history_output o ON h.id = o.history_id{} \
             ORDER BY h.timestamp DESC LIMIT 1000",
            where_clause
        );
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(&params_ref[..], |row| {
                Ok(HistoryRow {
                    id: row.get(0)?,
                    command: row.get(1)?,
                    directory: row.get(2)?,
                    session_id: row.get(3)?,
                    exit_code: row.get(4)?,
                    timestamp: row.get(5)?,
                    comment: row.get(6).unwrap_or_default(),
                    output: row.get(7).unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn build_where(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut clause = String::from(" WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if !self.query.is_empty() {
            let escaped = crate::util::escape_like(&self.query);
            clause.push_str(" AND (h.command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')");
            params.push(Box::new(format!("%{}%", escaped)));
            params.push(Box::new(format!("%{}%", escaped)));
        }
        match self.exit_filter {
            ExitFilter::Success => clause.push_str(" AND h.exit_code = 0"),
            ExitFilter::Failed => clause.push_str(" AND h.exit_code != 0"),
            ExitFilter::All => {}
        }
        match self.mode {
            Mode::Sess => {
                if let Ok(s) = std::env::var("SMART_HISTORY_SESSION")
                    && !s.is_empty()
                {
                    clause.push_str(" AND h.session_id = ?");
                    params.push(Box::new(s));
                }
            }
            Mode::Dir => {
                if let Ok(pwd) = std::env::var("PWD")
                    && !pwd.is_empty()
                {
                    clause.push_str(" AND h.directory = ?");
                    params.push(Box::new(pwd));
                }
            }
            Mode::Global => {}
        }
        (clause, params)
    }

    fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        self.refresh();
    }

    fn cycle_exit_filter(&mut self) {
        self.exit_filter = self.exit_filter.next();
        self.refresh();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len();
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.list_state.select(Some(next));
    }

    fn select_for_run(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::Run);
        }
    }

    fn select_for_edit_start(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::EditStart);
        }
    }

    fn select_for_edit_end(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i)
        {
            self.selection = Some(row.command.clone());
            self.pick_mode = Some(PickMode::EditEnd);
        }
    }

    fn push_char(&mut self, c: char) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.push(c);
        } else {
            self.query.push(c);
            self.refresh();
        }
    }

    fn backspace(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.pop();
        } else {
            self.query.pop();
            self.refresh();
        }
    }

    fn clear_query(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.clear();
        } else {
            self.query.clear();
            self.refresh();
        }
    }

    fn start_comment_edit(&mut self) {
        if let Some(row) = self.selected_row() {
            self.comment_edit = Some(row.comment.clone());
        }
    }

    fn cancel_comment_edit(&mut self) {
        self.comment_edit = None;
    }

    fn save_comment_edit(&mut self) -> Result<()> {
        if let Some(ref comment) = self.comment_edit
            && let Some(row) = self.selected_row()
        {
            self.conn.execute(
                "INSERT INTO command_comments (command, comment) VALUES (?1, ?2) \
                 ON CONFLICT (command) DO UPDATE SET comment = excluded.comment",
                params![row.command, comment],
            )?;
        }
        self.comment_edit = None;
        self.refresh();
        Ok(())
    }

    fn show_output_view(&mut self) {
        if let Some(row) = self.selected_row().filter(|r| !r.output.is_empty()) {
            self.output_view = Some(row.output.clone());
        }
    }

    fn close_output_view(&mut self) {
        self.output_view = None;
    }

    fn selected_row(&self) -> Option<&HistoryRow> {
        self.list_state
            .selected()
            .and_then(|i| self.rows.get(i))
    }

    fn is_comment_editing(&self) -> bool {
        self.comment_edit.is_some()
    }

    fn is_output_viewing(&self) -> bool {
        self.output_view.is_some()
    }
}

/// Run the TUI.
///
/// The TUI renders to **stderr** (so it doesn't pollute the parent
/// shell's `$(...)` capture, which reads stdout). The selected command
/// is printed to **stdout** by the caller (`main`).
pub fn run_tui_to_stdout(
    initial_mode: String,
    initial_query: String,
    conn: Connection,
) -> Result<Option<(String, i32)>> {
    let mode = Mode::parse(&initial_mode).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown TUI mode {:?}; expected one of SESS, SESSION, DIR, DIRECTORY, GLOBAL",
            initial_mode
        )
    })?;
    let mut app = App::new(conn, mode, initial_query);

    let mut render = std::io::stderr();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        render,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;

    let backend = CrosstermBackend::new(render);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, &mut app);

    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = crossterm::terminal::disable_raw_mode();

    result?;
    if app.cancelled {
        Ok(None)
    } else if let Some(sel) = app.selection {
        let mode = app.pick_mode.unwrap_or(PickMode::Run);
        Ok(Some((sel, mode.exit_code())))
    } else {
        Ok(None)
    }
}

fn run_loop<B>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()>
where
    B: Backend,
{
    loop {
        if let Err(e) = terminal.draw(|f| ui(f, app)) {
            return Err(anyhow::anyhow!("terminal draw failed: {}", e));
        }

        if crossterm::event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && handle_key(app, key)
        {
            return Ok(());
        }
    }
}

/// Returns `true` if the app should exit (selection made or cancelled).
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // When viewing captured output, only a small set of keys apply.
    if app.is_output_viewing() {
        return handle_output_view_key(app, key);
    }

    // When editing a comment, most keys go to the comment buffer.
    if app.is_comment_editing() {
        return handle_comment_edit_key(app, key);
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                app.cancelled = true;
                return true;
            }
            KeyCode::Char('g') => {
                app.cycle_mode();
                return false;
            }
            KeyCode::Char('s') => {
                app.cycle_exit_filter();
                return false;
            }
            KeyCode::Char('e') => {
                app.start_comment_edit();
                return false;
            }
            KeyCode::Char('l') => {
                app.show_output_view();
                return false;
            }
            KeyCode::Char('u') => {
                app.clear_query();
                return false;
            }
            KeyCode::Char('p') => {
                app.move_selection(1);
                return false;
            }
            KeyCode::Char('n') => {
                app.move_selection(-1);
                return false;
            }
            _ => return false,
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.cancelled = true;
            true
        }
        KeyCode::Enter => {
            app.select_for_run();
            true
        }
        KeyCode::Left => {
            app.select_for_edit_start();
            true
        }
        KeyCode::Right => {
            app.select_for_edit_end();
            true
        }
        KeyCode::Backspace => {
            app.backspace();
            false
        }
        // Rows are ordered newest-first (index 0 = newest). The list
        // is bottom-aligned, so the newest entry sits at the bottom.
        // Up moves visually upward = older = higher index.
        KeyCode::Up => {
            app.move_selection(1);
            false
        }
        KeyCode::Down => {
            app.move_selection(-1);
            false
        }
        KeyCode::PageUp => {
            app.move_selection(10);
            false
        }
        KeyCode::PageDown => {
            app.move_selection(-10);
            false
        }
        // Home jumps to the oldest entry (last index), End to the
        // newest (index 0, bottom of the list).
        KeyCode::Home => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(app.rows.len() - 1));
            }
            false
        }
        KeyCode::End => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(0));
            }
            false
        }
        KeyCode::Char(c) => {
            app.push_char(c);
            false
        }
        _ => false,
    }
}

/// Key handler used while viewing captured output. Returns `true` only
/// when the user aborts the whole TUI with Ctrl+C.
fn handle_output_view_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc
        | KeyCode::Enter
        | KeyCode::Char('q') => {
            app.close_output_view();
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            true
        }
        _ => false,
    }
}

/// Key handler used while editing a comment. Returns `true` only when
/// the user aborts the whole TUI with Ctrl+C.
fn handle_comment_edit_key(app: &mut App, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => {
                app.cancelled = true;
                return true;
            }
            KeyCode::Char('u') => {
                app.clear_query();
                return false;
            }
            _ => return false,
        }
    }

    match key.code {
        KeyCode::Esc => {
            app.cancel_comment_edit();
            false
        }
        KeyCode::Enter => {
            let _ = app.save_comment_edit();
            false
        }
        KeyCode::Backspace => {
            app.backspace();
            false
        }
        KeyCode::Char(c) => {
            app.push_char(c);
            false
        }
        _ => false,
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    if let Some(ref output) = app.output_view {
        draw_output_view(f, output);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1),   // mode strip
                Constraint::Fill(1),     // list: take all remaining space
                Constraint::Length(6),   // details: fixed 6 lines incl. header/borders
                Constraint::Length(3),   // input
                Constraint::Length(1),   // status
            ]
            .as_ref(),
        )
        .split(f.area());

    draw_mode_strip(f, app, chunks[0]);
    draw_list(f, app, chunks[1]);
    draw_details(f, app, chunks[2]);
    draw_input(f, app, chunks[3]);
    draw_status(f, app, chunks[4]);
}

fn draw_output_view(f: &mut Frame, output: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Captured output (^L close) ")
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let lines: Vec<Line> = output
        .lines()
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, f.area());
}

fn draw_mode_strip(f: &mut Frame, app: &App, area: Rect) {
    let spans = vec![
        Span::styled("smart", Theme::dim()),
        Span::styled("history", Theme::accent()),
        Span::styled("  ", Theme::default()),
        mode_badge(app.mode),
        Span::styled("  ", Theme::default()),
        exit_filter_badge(app.exit_filter),
        Span::styled(
            format!(
                "  {} · {} ",
                match app.mode {
                    Mode::Sess => "current session only",
                    Mode::Dir => "current directory only",
                    Mode::Global => "all history",
                },
                match app.exit_filter {
                    ExitFilter::All => "all exit codes",
                    ExitFilter::Success => "successful only",
                    ExitFilter::Failed => "failed only",
                }
            ),
            Theme::dim(),
        ),
    ];
    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    f.render_widget(paragraph, area);
}

fn exit_filter_badge(filter: ExitFilter) -> Span<'static> {
    let (label, color) = match filter {
        ExitFilter::All => ("ALL", Theme::ACCENT),
        ExitFilter::Success => ("OK", Theme::SUCCESS),
        ExitFilter::Failed => ("ERR", Theme::ERROR),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD),
    )
}

fn mode_badge(mode: Mode) -> Span<'static> {
    let (label, color) = match mode {
        Mode::Sess => ("SESS", Theme::SUCCESS),
        Mode::Dir => ("DIR", Theme::WARNING),
        Mode::Global => ("GLOBAL", Theme::ACCENT),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD),
    )
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let age_width = app
        .rows
        .iter()
        .map(|r| format_diff(r.timestamp).chars().count())
        .max()
        .unwrap_or(3)
        .max(3);

    // Build the real row items. Rows are stored newest-first; for
    // display we want oldest at the top and newest at the bottom,
    // so reverse the order. Pass `is_selected` based on the data index.
    let real_items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .rev()
        .map(|(data_idx, r)| {
            let is_selected = app.list_state.selected() == Some(data_idx);
            ListItem::new(render_row(r, &app.query, is_selected, age_width))
        })
        .collect();

    // Bottom-align: when there are fewer real rows than the visible
    // height, pad the top with empty items so the real rows sit at
    // the bottom of the widget. `area.height` includes the top and
    // bottom borders; subtract 2 for the content area.
    let visible_height = area.height.saturating_sub(2) as usize;
    let real_count = real_items.len();
    let pad = visible_height.saturating_sub(real_count);

    let mut items: Vec<ListItem> = (0..pad).map(|_| ListItem::new("")).collect();
    items.extend(real_items);

    // The stored selection is in data coordinates (0 = newest).
    // Map it to the rendered list coordinates where the newest item
    // is the last real item.
    let rendered_idx = app.list_state.selected().map(|data_idx| {
        pad + (real_count.saturating_sub(1) - data_idx)
    });

    // Anchor the visible window so the selected row appears at the
    // bottom. If we padded, start from the top; otherwise start from
    // the position that puts the selection at the bottom of the view.
    let offset = if let Some(ri) = rendered_idx {
        if real_count >= visible_height {
            ri.saturating_sub(visible_height.saturating_sub(1))
        } else {
            0
        }
    } else {
        0
    };

    // Replace the state so we can set the offset explicitly. Preserve
    // the rendered selection for this frame.
    let mut render_state = ListState::default().with_offset(offset);
    render_state.select(rendered_idx);

    let title = format!(" History — {} ", app.rows.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .title(title)
                .title_style(Theme::accent())
                .border_style(Theme::dim()),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(symbols::line::THICK_VERTICAL_RIGHT)
        .repeat_highlight_symbol(true);

    f.render_stateful_widget(list, area, &mut render_state);

    // ratatui may have scrolled the state; read its final offset and
    // selection back into app.list_state in data coordinates.
    let final_selected = render_state.selected();
    let data_idx = final_selected.and_then(|ri| {
        if ri < pad {
            None
        } else {
            let real = ri - pad;
            Some(real_count.saturating_sub(1) - real)
        }
    });
    app.list_state = ListState::default().with_offset(0);
    app.list_state.select(data_idx);
}

/// Render a single history row as a `Line` with optional query
/// highlighting. The layout is a fixed-width columnar form:
///
///   [age] [status]  command  ·  time
///
/// `age_width` is the right-aligned width of the age column so rows
/// line up.
fn render_row<'a>(row: &'a HistoryRow, query: &str, is_selected: bool, age_width: usize) -> Line<'a> {
    let age = format_diff(row.timestamp);
    let age_padded = format!("{:>age_width$}", age);

    let exit_marker = if row.exit_code == 0 { "✓" } else { "✗" };
    let exit_style = if row.exit_code == 0 {
        Theme::success()
    } else {
        Theme::error()
    };

    let mut spans = vec![
        Span::styled(format!(" {} ", age_padded), Theme::accent()),
        Span::raw(" "),
        Span::styled(format!(" {} ", exit_marker), exit_style),
        Span::raw(" "),
    ];

    // Highlight query matches inside the command.
    spans.extend(highlight_matches(&row.command, query));

    spans.push(Span::styled(
        format!("  · {} ", format_time(row.timestamp)),
        Theme::dim(),
    ));

    // Show a non-empty comment inline for every row, and fall back to
    // the directory on the selected row when there is no comment.
    if !row.comment.is_empty() {
        spans.push(Span::styled(
            format!("# {} ", row.comment),
            Style::default()
                .fg(Theme::WARNING)
                .add_modifier(Modifier::ITALIC),
        ));
    } else if is_selected {
        spans.push(Span::styled(
            format!("· {} ", row.directory),
            Theme::dim(),
        ));
    }

    Line::from(spans)
}

/// Return a sequence of spans that wrap every occurrence of `query`
/// in `text` with a highlight style. Matching is case-insensitive and
/// based on Unicode scalar values. Adjacent non-matching characters
/// are coalesced into a single span.
fn highlight_matches<'a>(text: &'a str, query: &str) -> Vec<Span<'a>> {
    if query.is_empty() {
        return vec![Span::raw(text)];
    }

    let lower_query: Vec<char> = query.to_lowercase().chars().collect();
    let lower_text: Vec<char> = text.to_lowercase().chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    let qlen = lower_query.len();
    let mut spans: Vec<Span<'a>> = Vec::new();
    let mut i = 0;
    let mut pending_start = 0;

    while i + qlen <= lower_text.len() {
        if lower_text[i..i + qlen] == lower_query[..] {
            // Emit pending non-matching prefix, if any.
            if i > pending_start {
                let prefix: String = text_chars[pending_start..i].iter().collect();
                spans.push(Span::raw(prefix));
            }
            let matched: String = text_chars[i..i + qlen].iter().collect();
            spans.push(Span::styled(
                matched,
                Style::default()
                    .fg(Theme::HIGHLIGHT)
                    .add_modifier(Modifier::BOLD),
            ));
            i += qlen;
            pending_start = i;
        } else {
            i += 1;
        }
    }

    if pending_start < text_chars.len() {
        let tail: String = text_chars[pending_start..].iter().collect();
        spans.push(Span::raw(tail));
    }

    spans
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Details ")
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let Some(row) = app.selected_row() else {
        let empty = Paragraph::new(
            Line::from(vec![Span::styled("No command selected", Theme::dim())]),
        )
        .block(block);
        f.render_widget(empty, area);
        return;
    };

    let exit_marker = if row.exit_code == 0 { "✓" } else { "✗" };
    let exit_text = if row.exit_code == 0 {
        "success".to_string()
    } else {
        format!("exit {}", row.exit_code)
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Command  ", Theme::dim()),
            Span::styled(row.command.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Dir      ", Theme::dim()),
            Span::raw(row.directory.clone()),
        ]),
        Line::from(vec![
            Span::styled("Session  ", Theme::dim()),
            Span::raw(row.session_id.clone()),
        ]),
        Line::from(vec![
            Span::styled("Time     ", Theme::dim()),
            Span::raw(format!(
                "{} · {} · epoch {}",
                format_time(row.timestamp),
                format_diff(row.timestamp),
                row.timestamp
            )),
        ]),
        Line::from(vec![
            Span::styled("Status   ", Theme::dim()),
            Span::styled(format!("{} {}", exit_marker, exit_text), Theme::success()),
        ]),
    ];

    // Add the comment line only when one exists.
    if !row.comment.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Comment  ", Theme::dim()),
            Span::styled(
                row.comment.clone(),
                Style::default()
                    .fg(Theme::WARNING)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    // Add the captured output line(s) when output exists.
    if !row.output.is_empty() {
        lines.push(Line::from(vec![Span::styled("Output   ", Theme::dim())]));
        for line in row.output.lines() {
            lines.push(Line::from(vec![Span::raw(format!("    {}", line))]));
        }
    }

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let (prompt, title, content) = match app.comment_edit {
        Some(ref buf) => {
            ("comment> ", " comment ", buf.as_str())
        }
        None => ("> ", " search ", app.query.as_str()),
    };

    let input = Paragraph::new(Line::from(vec![
        Span::styled(prompt, Theme::accent()),
        Span::raw(content),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(title)
            .title_style(Theme::accent())
            .border_style(if app.comment_edit.is_some() {
                Style::default().fg(Theme::WARNING)
            } else {
                Theme::dim()
            }),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(input, area);

    // Place the cursor at the end of the active buffer.
    // The visible text starts at area.x + 1 (one cell for the left
    // border). The prompt string includes its own trailing space.
    let prompt_width = prompt.chars().count() as u16;
    let cursor_x = area.x + 1 + prompt_width + content.chars().count() as u16;
    let cursor_y = area.y + 1;
    f.set_cursor_position((
        cursor_x.min(area.x.saturating_add(area.width).saturating_sub(2)),
        cursor_y,
    ));
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let n = app.rows.len();
    let count = match n {
        0 => "0 matches".to_string(),
        1 => "1 match".to_string(),
        x => format!("{} matches", x),
    };

    let help = match app.selected_row() {
        Some(row) if !row.output.is_empty() => "Enter run · ←→ edit · ↑↓ nav · ^G scope · ^S status · ^E comment · ^L output · ^U clear · Esc cancel",
        Some(_) => "Enter run · ←→ edit · ↑↓ nav · ^G scope · ^S status · ^E comment · ^U clear · Esc cancel",
        None => "Type to search · ^G scope · ^S status · ^E comment · ^U clear · Esc cancel",
    };

    let line = Line::from(vec![
        Span::styled(format!(" {}  ", count), Theme::highlight()),
        Span::styled(help, Theme::dim()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_matches_empty_query() {
        let spans = highlight_matches("hello world", "");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world".to_string());
    }

    #[test]
    fn highlight_matches_single() {
        let spans = highlight_matches("git status", "stat");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git ", "stat", "us"]);
    }

    #[test]
    fn highlight_matches_case_insensitive() {
        let spans = highlight_matches("Git Status", "stat");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["Git ", "Stat", "us"]);
    }

    #[test]
    fn highlight_matches_multiple() {
        let spans = highlight_matches("foo bar foo", "foo");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["foo", " bar ", "foo"]);
    }

    #[test]
    fn highlight_matches_no_match() {
        let spans = highlight_matches("hello world", "xyz");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world".to_string());
    }
}
