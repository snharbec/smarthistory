use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use rusqlite::Connection;
use std::time::Duration;

/// Search scope for the TUI. Mirrors the line-editor widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sess,
    Dir,
    Global,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Sess => "SESS",
            Mode::Dir => "DIR",
            Mode::Global => "GLOBAL",
        }
    }
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
    command: String,
    directory: String,
    session_id: String,
    exit_code: i32,
    timestamp: i64,
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

struct App {
    conn: Connection,
    mode: Mode,
    query: String,
    rows: Vec<HistoryRow>,
    /// Number of empty items prepended to the list to bottom-align
    /// the real rows. Recomputed every render based on the list's
    /// available height.
    pad: usize,
    list_state: ListState,
    selection: Option<String>,
    pick_mode: Option<PickMode>,
    cancelled: bool,
}

impl App {
    fn new(conn: Connection, initial_mode: Mode, initial_query: String) -> Self {
        let list_state = ListState::default();
        let mut app = App {
            conn,
            mode: initial_mode,
            query: initial_query,
            rows: Vec::new(),
            pad: 0,
            list_state,
            selection: None,
            pick_mode: None,
            cancelled: false,
        };
        app.refresh();
        // Default to the newest entry (last row) so the user lands
        // on the most recent match and can scroll up to see older
        // history. If there are no rows, leave the selection unset.
        if !app.rows.is_empty() {
            // Initial selection is the *real* index of the newest
            // entry; draw_list will add the padding offset when it
            // updates `app.pad` on the first render.
            app.list_state.select(Some(app.rows.len() - 1));
        }
        app
    }

    /// Re-query the database with the current mode + query. Cheap on
    /// local SQLite; if it ever becomes a bottleneck, move to a
    /// background thread.
    fn refresh(&mut self) {
        self.rows = self.fetch().unwrap_or_default();
        if self.rows.is_empty() {
            self.list_state.select(None);
        } else {
            let i = self.list_state.selected().unwrap_or(0).min(self.rows.len() - 1);
            self.list_state.select(Some(i));
        }
    }

    fn fetch(&self) -> Result<Vec<HistoryRow>> {
        let (where_clause, params) = self.build_where();
        let sql = format!(
            "SELECT command, directory, session_id, exit_code, timestamp \
             FROM history {} ORDER BY timestamp ASC LIMIT 1000",
            where_clause
        );
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(&params_ref[..], |row| {
                Ok(HistoryRow {
                    command: row.get(0)?,
                    directory: row.get(1)?,
                    session_id: row.get(2)?,
                    exit_code: row.get(3)?,
                    timestamp: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn build_where(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut clause = String::from(" WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if !self.query.is_empty() {
            // Escape LIKE wildcards in the user query (re-uses the
            // helper from `crate::util`). The ESCAPE clause enables
            // backslash escapes in SQLite's LIKE.
            let escaped = crate::util::escape_like(&self.query);
            clause.push_str(" AND command LIKE ? ESCAPE '\\'");
            params.push(Box::new(format!("%{}%", escaped)));
        }
        match self.mode {
            Mode::Sess => {
                if let Ok(s) = std::env::var("SMART_HISTORY_SESSION")
                    && !s.is_empty() {
                        clause.push_str(" AND session_id = ?");
                        params.push(Box::new(s));
                    }
            }
            Mode::Dir => {
                if let Ok(pwd) = std::env::var("PWD")
                    && !pwd.is_empty() {
                        clause.push_str(" AND directory = ?");
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
        // After switching scope, jump the highlight to the most
        // recent entry so the user lands on the newest command in
        // the new mode. Without this, the previous selection index
        // would either be out of bounds (clamped to a different
        // entry) or point at a row that no longer makes sense in
        // the new mode.
        if !self.rows.is_empty() {
            self.list_state.select(Some(self.rows.len() - 1));
        } else {
            self.list_state.select(None);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len();
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.list_state.select(Some(next));
    }

    fn select_for_run(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i) {
                self.selection = Some(row.command.clone());
                self.pick_mode = Some(PickMode::Run);
            }
    }

    fn select_for_edit_start(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i) {
                self.selection = Some(row.command.clone());
                self.pick_mode = Some(PickMode::EditStart);
            }
    }

    fn select_for_edit_end(&mut self) {
        if let Some(i) = self.list_state.selected()
            && let Some(row) = self.rows.get(i) {
                self.selection = Some(row.command.clone());
                self.pick_mode = Some(PickMode::EditEnd);
            }
    }

    fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refresh();
    }

    fn backspace(&mut self) {
        self.query.pop();
        self.refresh();
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.refresh();
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

    // Render to stderr so stdout is free for the selected command.
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

    // Always restore the terminal, even on error.
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
        // `terminal.draw` returns `Result<(), B::Error>`. We can't
        // use `?` because the error type may not be Send. Treat
        // draw errors as terminal-unrecoverable and bail out.
        if let Err(e) = terminal.draw(|f| ui(f, app)) {
            return Err(anyhow::anyhow!("terminal draw failed: {}", e));
        }

        if crossterm::event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
                && handle_key(app, key) {
                    return Ok(());
                }
    }
}

/// Returns `true` if the app should exit (selection made or cancelled).
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // Ctrl-modified keys first.
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
            KeyCode::Char('u') => {
                app.clear_query();
                return false;
            }
            KeyCode::Char('p') => {
                app.move_selection(-1);
                return false;
            }
            KeyCode::Char('n') => {
                app.move_selection(1);
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
            // Edit mode with cursor at the start of the line.
            app.select_for_edit_start();
            true
        }
        KeyCode::Right => {
            // Edit mode with cursor at the end of the line.
            app.select_for_edit_end();
            true
        }
        KeyCode::Backspace => {
            app.backspace();
            false
        }
        KeyCode::Up => {
            app.move_selection(-1);
            false
        }
        KeyCode::Down => {
            app.move_selection(1);
            false
        }
        KeyCode::PageUp => {
            app.move_selection(-10);
            false
        }
        KeyCode::PageDown => {
            app.move_selection(10);
            false
        }
        KeyCode::Home => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(0));
            }
            false
        }
        KeyCode::End => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(app.rows.len() - 1));
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

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ]
            .as_ref(),
        )
        .split(f.area());

    draw_list(f, app, chunks[0]);
    draw_input(f, app, chunks[1]);
    draw_status(f, app, chunks[2]);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items_real: Vec<ListItem> = app
        .rows
        .iter()
        .map(|r| {
            let time = format_time(r.timestamp);
            let exit_marker = if r.exit_code == 0 { "OK" } else { "ERR" };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {}  ", time),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", exit_marker),
                    Style::default().fg(if r.exit_code == 0 {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Span::styled(r.command.clone(), Style::default()),
                Span::styled(
                    format!("  ({})", r.directory),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    // Bottom-align: when there are fewer items than the list's
    // available height, pad the top with empty items so the actual
    // rows sit at the bottom of the widget instead of the top.
    // `area.height` includes the top and bottom borders; subtract 2
    // for those. If the list is taller than the available area,
    // `saturating_sub` clamps `pad` to zero (no padding).
    let visible_height = area.height.saturating_sub(2) as usize;
    let pad = visible_height.saturating_sub(items_real.len());
    app.pad = pad;
    let mut items: Vec<ListItem> = (0..pad).map(|_| ListItem::new("")).collect();
    items.extend(items_real);

    // Translate the stored real-row index into a rendered index.
    // If no selection (e.g. empty list), leave it unset.
    if let Some(real_idx) = app.list_state.selected() {
        if real_idx < app.rows.len() {
            app.list_state.select(Some(real_idx + pad));
        } else {
            app.list_state.select(None);
        }
    }

    let title = format!(" History ({}) ", app.rows.len());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .bg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.list_state);

    // Translate the rendered index back to a real-row index so the
    // stored value is always in real-row coordinates.
    if let Some(rendered_idx) = app.list_state.selected() {
        let real = rendered_idx.saturating_sub(pad);
        app.list_state.select(Some(real));
    }
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let input = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(app.query.as_str()),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" search "))
    .wrap(Wrap { trim: false });
    f.render_widget(input, area);
    // Place the cursor at the end of the query.
    let cursor_x = area.x + 3 + app.query.chars().count() as u16;
    let cursor_y = area.y + 1;
    f.set_cursor_position((
        cursor_x.min(area.x.saturating_add(area.width).saturating_sub(2)),
        cursor_y,
    ));
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let n = app.rows.len();
    let count = match n {
        1 => "1 match".to_string(),
        x => format!("{} matches", x),
    };
    let mode_str = format!("[{}]", app.mode.label());
    let help = "Arrows nav  PgUp/PgDn  Home/End  Enter run  Left/Right edit  ^G scope  ^U clear  Esc cancel";
    let line = Line::from(vec![
        Span::styled(format!(" {}  ", count), Style::default().fg(Color::Yellow)),
        Span::styled(format!("{}  ", mode_str), Style::default().fg(Color::Cyan)),
        Span::styled(help, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// `format_time` lives in `crate::util` and is re-exported.
use crate::util::format_time;
