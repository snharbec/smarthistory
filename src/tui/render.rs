// Render code: the main `ui` entry point plus all the draw_*
// helpers (draw_output_view, draw_help_view, draw_command_menu,
// draw_theme_picker, etc.) and the highlight_matches helpers.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::bindings::{format_key_specs, Action};
use super::state::{ExitFilter, HistoryRow, Mode};
use super::theme::palette_storage::PALETTE;
use super::theme::{Theme, ThemePicker};
use super::{format_diff, format_time, App, CommandMenu, ConfirmMode, HelpView, OutputView};
use regex::Regex;

pub(super) fn ui(f: &mut Frame, app: &mut App) {
    if let Some(ref view) = app.output_view {
        draw_output_view(f, view);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1), // mode strip
                Constraint::Fill(1),   // list: take all remaining space
                Constraint::Length(8), // details: fixed 8 lines incl. header/borders
                Constraint::Length(3), // input
                Constraint::Length(1), // status
            ]
            .as_ref(),
        )
        .split(f.area());

    draw_mode_strip(f, app, chunks[0]);
    draw_list(f, app, chunks[1]);

    let detail_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
        .split(chunks[2]);

    draw_details(f, app, detail_chunks[0]);
    draw_output_preview(f, app, detail_chunks[1]);

    draw_input(f, app, chunks[3]);
    draw_status(f, app, chunks[4]);

    if let Some(mode) = app.confirm_delete {
        draw_confirm_delete(f, app, mode);
    }

    if let Some(view) = app.help_view.as_ref() {
        draw_help_view(f, app, view);
    }

    if let Some(menu) = app.command_menu.as_ref() {
        draw_command_menu(f, app, menu);
    }

    if let Some(picker) = app.theme_picker.as_ref() {
        draw_theme_picker(f, app, picker);
    }

    // If a comment exists, draw the labeled entries pane as an overlay
    // so that labeled history elements are always available.
    // (Labeled entries are now merged into the main list instead.)
    #[allow(clippy::overly_complex_conditional)]
    let _ = !app.labeled_rows.is_empty();
}

fn draw_confirm_delete(f: &mut Frame, app: &App, mode: ConfirmMode) {
    let area = centered_rect(60, 25, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let (title, message) = match mode {
        ConfirmMode::DeleteSelected => (
            " Delete selected entry ",
            "Are you sure you want to delete the selected history entry?".to_string(),
        ),
        ConfirmMode::DeleteMatching => (
            " Delete ALL matching entries ",
            format!(
                "Are you sure you want to delete all {} matching entries?",
                app.rows.len()
            ),
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::error())
        .border_style(Theme::error());

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            message,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("y", Theme::highlight()),
            Span::raw(" to confirm, "),
            Span::styled("n", Theme::highlight()),
            Span::raw(" or "),
            Span::styled("Esc", Theme::highlight()),
            Span::raw(" to cancel."),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

fn draw_output_view(f: &mut Frame, view: &OutputView) {
    let area = f.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Captured output (\u{2191}\u{2193} scroll, ^E edit, ^L close) ")
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let all_lines: Vec<&str> = view.text.lines().collect();
    let total = all_lines.len();
    // Inner height excludes the top and bottom borders.
    let inner_h = area.height.saturating_sub(2) as usize;
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = view.scroll.min(max_scroll);

    // Window of visible lines.
    let end = (scroll + inner_h).min(total);
    let start = scroll;
    let visible: Vec<Line> = all_lines[start..end]
        .iter()
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let paragraph = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);

    // Footer with scroll position (only if there is room inside the
    // border).
    if area.height >= 3 {
        let footer = format!(" {}/{} ", end, total);
        let para = Paragraph::new(Line::from(Span::styled(footer, Theme::dim())));
        let footer_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        f.render_widget(para, footer_area);
    }
}

fn draw_help_view(f: &mut Frame, app: &App, view: &HelpView) {
    // Cover the whole screen so the help is the only thing visible.
    let area = f.area();
    f.render_widget(ratatui::widgets::Clear, area);

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Help — Esc/Enter/q to close ")
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));

    let inner_h = area.height.saturating_sub(2) as usize;
    let lines = build_help_lines(app);
    let total = lines.len();

    // Clamp the scroll position to a valid range.
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = view.scroll.min(max_scroll);

    // Color the default text (rows that have no per-span style)
    // using the theme foreground so the help is readable on any
    // background — including light themes.
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(scroll)
        .take(inner_h)
        .map(|line| {
            let spans: Vec<Span> = line
                .spans
                .into_iter()
                .map(|s| {
                    if s.style.fg.is_none() && s.style.bg.is_none() {
                        Span::styled(s.content, Style::default().fg(fg).bg(bg))
                    } else {
                        // Make sure spans that already have a style
                        // also pick up the theme background, so
                        // gaps between styled runs don't show
                        // through to the terminal's default.
                        let mut style = s.style;
                        style = style.bg(bg);
                        Span::styled(s.content, style)
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect();

    let paragraph = Paragraph::new(visible)
        .block(block)
        .style(Style::default().bg(bg))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);

    // Footer with scroll position.
    if area.height >= 3 {
        let footer = format!(
            " {}-{} / {}  ↑↓ scroll · PgUp/PgDn page · Home/End jump ",
            scroll + 1,
            (scroll + inner_h).min(total),
            total
        );
        let para = Paragraph::new(Line::from(Span::styled(footer, Theme::dim())))
            .style(Style::default().bg(bg));
        let footer_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        f.render_widget(para, footer_area);
    }
}

/// Build the lines shown in the help overlay. The first section
/// reflects the user's current settings; the second section is the
/// canonical shortcut reference.
fn build_help_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let accent = Theme::accent();
    let dim = Theme::dim();
    let warning = Style::default().fg(Theme::warning_color());

    // ----- Current settings -----
    lines.push(Line::from(vec![Span::styled(
        "Current settings",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    let mode_str = match app.mode {
        Mode::Sess => "SESS  (current session only)",
        Mode::Dir => "DIR  (current directory only)",
        Mode::Global => "GLOBAL  (all history)",
        Mode::Stats => "STATS  (probability + age)",
    };
    lines.push(Line::from(vec![
        Span::styled("  Mode            ", dim),
        Span::styled(mode_str, accent),
    ]));

    let dup_str = if app.duplicate_filter {
        "ON  (newest entry per command)"
    } else {
        "OFF  (every entry shown)"
    };
    lines.push(Line::from(vec![
        Span::styled("  Duplicate filter", dim),
        Span::styled(dup_str, accent),
    ]));

    lines.push(Line::from(vec![
        Span::styled("  Theme          ", dim),
        Span::styled(app.theme.display_name(), accent),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Keyboard shortcuts",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(
        "  Bindings can be remapped in ~/.config/smarthistory/config",
    ));
    lines.push(Line::from(
        "  (key.<action>=<C-/M-/Esc/Up/...>). Use `key.<action>=none`",
    ));
    lines.push(Line::from("  to disable a default binding entirely."));
    lines.push(Line::from(
        "  Comma-separate multiple keys to bind the same action to",
    ));
    lines.push(Line::from("  several, e.g. `key.open-help=C-h, F1`."));
    lines.push(Line::from(""));

    // Helper to render a single shortcut row from the live binding
    // table so the help always reflects what the user has actually
    // configured.
    fn row(lines: &mut Vec<Line<'static>>, key_text: String, desc: &'static str) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<14}", key_text),
                Style::default().fg(Theme::highlight_color()),
            ),
            Span::raw(desc),
        ]));
    }

    let binding_for = |a: Action| -> String {
        if app.bindings.is_unbound(a) {
            "(unbound)".to_string()
        } else {
            let specs = app.bindings.specs(a);
            if specs.is_empty() {
                "?".to_string()
            } else {
                format_key_specs(specs)
            }
        }
    };

    // ----- Search / navigation -----
    row(
        &mut lines,
        "type".to_string(),
        "type to filter (plain text multi-word AND; prefix `/` for regex)",
    );
    row(
        &mut lines,
        binding_for(Action::Backspace),
        "delete one character from the query",
    );
    row(
        &mut lines,
        binding_for(Action::ClearQuery),
        "clear the query",
    );
    row(
        &mut lines,
        format!(
            "{} / {}",
            binding_for(Action::Up),
            binding_for(Action::Down)
        ),
        "move the cursor through the history list",
    );
    row(
        &mut lines,
        format!(
            "{} / {}",
            binding_for(Action::PageUp),
            binding_for(Action::PageDown)
        ),
        "jump 10 rows at a time",
    );
    row(
        &mut lines,
        format!(
            "{} / {}",
            binding_for(Action::Home),
            binding_for(Action::End)
        ),
        "jump to oldest / newest entry",
    );
    row(
        &mut lines,
        format!(
            "{} / {}",
            binding_for(Action::EditStart),
            binding_for(Action::EditEnd)
        ),
        "prefill the line for editing (cursor at start / end)",
    );
    row(
        &mut lines,
        binding_for(Action::Run),
        "run the selected command",
    );

    lines.push(Line::from(""));

    // ----- Scopes / filters -----
    row(
        &mut lines,
        binding_for(Action::CycleMode),
        "cycle search scope: SESS → DIR → GLOBAL → STATS → SESS",
    );
    row(
        &mut lines,
        binding_for(Action::ToggleDuplicateFilter),
        "toggle duplicate filter (LAST only \u{2194} ALL entries)",
    );
    row(
        &mut lines,
        binding_for(Action::CycleExitFilter),
        "cycle exit-code filter: ALL → OK → ERR → ALL",
    );
    row(
        &mut lines,
        binding_for(Action::CycleThemeNext),
        "cycle to the next theme",
    );
    row(
        &mut lines,
        binding_for(Action::CycleThemePrev),
        "cycle to the previous theme",
    );

    lines.push(Line::from(""));

    // ----- Annotations / output -----
    row(
        &mut lines,
        binding_for(Action::EditComment),
        "edit the comment of the selected entry",
    );
    row(
        &mut lines,
        binding_for(Action::ShowOutput),
        "open the captured-output view (when available)",
    );
    row(
        &mut lines,
        binding_for(Action::YankSelection),
        "yank the output (or selected command) to the clipboard",
    );
    row(
        &mut lines,
        binding_for(Action::OpenHelp),
        "open this help overlay",
    );
    row(
        &mut lines,
        binding_for(Action::CommandAction),
        "open the command palette (run any action by name)",
    );
    row(
        &mut lines,
        binding_for(Action::ThemePicker),
        "open the theme picker (live preview, Enter commits, Esc reverts)",
    );

    lines.push(Line::from(""));

    // ----- Deletion -----
    row(
        &mut lines,
        binding_for(Action::DeleteSelected),
        "delete the selected entry (with confirmation)",
    );
    row(
        &mut lines,
        binding_for(Action::DeleteMatching),
        "delete ALL matching entries (with confirmation)",
    );

    lines.push(Line::from(""));

    // ----- Cancel -----
    row(
        &mut lines,
        format!("{} (also closes overlays)", binding_for(Action::Cancel)),
        "cancel without selecting",
    );

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Tips",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));
    lines.push(Line::from(
        "  \u{2022} When the search starts with `/`, the rest is treated as a regular expression.",
    ));
    lines.push(Line::from(
        "  \u{2022} Implicit `.*` anchors are added unless you use `^` or `$`.",
    ));
    lines.push(Line::from(
        "  \u{2022} Highlighted matches are bold; the match range is shown exactly.",
    ));
    lines.push(Line::from(
        "  \u{2022} The session file (~/.local/cache/smarthistory/session) remembers",
    ));
    lines.push(Line::from(
        "    mode, query, duplicate filter, and theme between launches.",
    ));
    lines.push(Line::from(
        "  \u{2022} Config-file colors are used when the theme is \"no theme\".",
    ));
    lines.push(Line::from(
        "  \u{2022} Key bindings live in the config file as `key.<action>=<spec>`,",
    ));
    lines.push(Line::from(
        "    e.g. `key.open-help=M-h` to bind the help overlay to Alt+h.",
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Press Esc, Enter, or q to close this help.",
        warning,
    )]));

    lines
}

fn draw_command_menu(f: &mut Frame, app: &App, menu: &CommandMenu) {
    use ratatui::widgets::List;

    // The palette is centered horizontally and vertically. The
    // width is generous so even long action names fit on one line.
    let area = centered_rect(70, 70, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Command palette  Esc/q to close ")
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split the inner area into:
    //   [0] query input (3 lines: border, prompt+text, border)
    //   [1] action list  (everything else)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ]
            .as_ref(),
        )
        .split(inner);

    // ---- Query line ----
    let prompt = if menu.query.is_empty() {
        Span::styled("> ", Theme::accent())
    } else {
        Span::styled("> ", Theme::accent())
    };
    let placeholder = if menu.query.is_empty() {
        Span::styled(
            "Type an action name (e.g. \"cycle\", \"delete\") or a key",
            Style::default()
                .fg(Theme::dim_color())
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        Span::styled(menu.query.clone(), Style::default().fg(fg))
    };
    let query_line = Line::from(vec![prompt, placeholder]);
    let query_para = Paragraph::new(query_line)
        .style(Style::default().bg(bg))
        .wrap(Wrap { trim: false });
    f.render_widget(query_para, chunks[0]);

    // Place the cursor at the end of the typed query so the user
    // sees where their next character will go.
    if menu.touched || !menu.query.is_empty() {
        let prompt_width = "> ".chars().count() as u16;
        let cursor_x = chunks[0].x + prompt_width + menu.query.chars().count() as u16;
        let cursor_y = chunks[0].y;
        f.set_cursor_position((
            cursor_x.min(
                chunks[0]
                    .x
                    .saturating_add(chunks[0].width)
                    .saturating_sub(2),
            ),
            cursor_y,
        ));
    }

    // ---- Action list ----
    let filtered = menu.filtered_indices();
    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Theme::dim_color());
    let accent_style = Theme::accent();
    let warning_style = Style::default().fg(Theme::warning_color());

    // Show only what fits, scrolling so the selected row is
    // always visible.
    let visible_rows = chunks[1].height as usize;
    let start = if filtered.is_empty() || visible_rows == 0 {
        0
    } else {
        menu.selected
            .saturating_sub(visible_rows.saturating_sub(1))
            .min(filtered.len().saturating_sub(visible_rows))
    };
    let end = (start + visible_rows).min(filtered.len());

    let mut items: Vec<ListItem> = Vec::new();
    for (row_pos, &idx) in filtered.iter().enumerate().skip(start).take(end - start) {
        let action = menu.actions[idx];
        let label = action.display_name();
        let key = if app.bindings.is_unbound(action) {
            " (unbound)".to_string()
        } else {
            let specs = app.bindings.specs(action);
            if specs.is_empty() {
                " (?)".to_string()
            } else {
                format!(" {}", format_key_specs(specs))
            }
        };
        let is_selected = row_pos == menu.selected;
        let category = action.category();
        // Pad the action label so the key column lines up. Width
        // 22 is enough for "Edit (cursor at start)" plus a space.
        let mut spans = vec![
            Span::styled(
                format!("  {:<22}", label),
                if is_selected {
                    highlight_style
                } else {
                    Style::default().fg(fg)
                },
            ),
            Span::styled(
                format!("{:>14}", key),
                if is_selected {
                    highlight_style
                } else {
                    accent_style
                },
            ),
            Span::styled(
                format!("  [{}]", category),
                if is_selected {
                    highlight_style
                } else {
                    dim_style
                },
            ),
        ];
        if app.bindings.is_unbound(action) {
            spans.insert(
                1,
                Span::styled(
                    " ⚠ ",
                    if is_selected {
                        highlight_style
                    } else {
                        warning_style
                    },
                ),
            );
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::from(vec![Span::styled(
            "  (no action matches your query)",
            dim_style,
        )])));
    }

    let list = List::new(items)
        .style(Style::default().bg(bg))
        .highlight_style(highlight_style)
        .highlight_symbol("> ")
        .repeat_highlight_symbol(false);

    let mut list_state = ListState::default();
    if !filtered.is_empty() {
        list_state.select(Some(menu.selected.saturating_sub(start)));
    }
    f.render_stateful_widget(list, chunks[1], &mut list_state);

    // ---- Footer ----
    let footer = Line::from(vec![
        Span::styled(
            format!(" {}/{} actions", filtered.len(), menu.actions.len()),
            dim_style,
        ),
        Span::raw("  up/down move  Enter run  Esc close"),
    ]);
    let footer_para = Paragraph::new(footer).style(Style::default().bg(bg));
    f.render_widget(footer_para, chunks[2]);
}

fn draw_theme_picker(f: &mut Frame, _app: &App, picker: &ThemePicker) {
    use ratatui::widgets::List;

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    // Centered popup. Two horizontal columns:
    //   [0] the list of themes (55% of width)
    //   [1] a preview pane (45% of width) showing the live
    //       palette in action.
    let outer = centered_rect(75, 70, f.area());
    f.render_widget(ratatui::widgets::Clear, outer);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Theme picker  Enter commits / Esc reverts ")
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let inner = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)].as_ref())
        .split(inner);

    // ---- Theme list (left column) ----
    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Theme::dim_color());

    // Scroll so the selected row stays visible.
    let visible_rows = inner[0].height as usize;
    let total = picker.themes.len();
    let start = picker
        .selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(total.saturating_sub(visible_rows));
    let end = (start + visible_rows).min(total);

    let mut items: Vec<ListItem> = Vec::new();
    for (row_pos, theme) in picker
        .themes
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        let is_selected = row_pos == picker.selected;
        let is_original = *theme == picker.original;
        let mut spans = Vec::new();
        // Selection marker.
        spans.push(Span::styled(
            if is_selected { " > " } else { "   " },
            if is_selected {
                highlight_style
            } else {
                dim_style
            },
        ));
        // Slug (left-aligned) so the eye scans down a column.
        spans.push(Span::styled(
            format!("{:<14}", theme.slug()),
            if is_selected {
                highlight_style
            } else {
                Style::default().fg(fg)
            },
        ));
        // Display name.
        spans.push(Span::styled(
            theme.display_name(),
            if is_selected {
                highlight_style
            } else {
                Style::default().fg(fg)
            },
        ));
        // "(current)" marker on the row that matches the
        // pre-picker theme.
        if is_original && !is_selected {
            spans.push(Span::styled("  (current)", dim_style));
        }
        items.push(ListItem::new(Line::from(spans)));
    }

    let list = List::new(items)
        .style(Style::default().bg(bg))
        .highlight_style(highlight_style)
        .highlight_symbol("")
        .repeat_highlight_symbol(false);
    let mut list_state = ListState::default();
    if end > start {
        list_state.select(Some(picker.selected.saturating_sub(start)));
    }
    f.render_stateful_widget(list, inner[0], &mut list_state);

    // ---- Preview pane (right column) ----
    // The preview shows the *active* palette colors (the live
    // preview already installed by `install_palette`), which is
    // exactly what the user is about to commit to.
    let preview_lines: Vec<Line> = {
        let p = PALETTE.with(|c| *c.borrow());
        vec![
            Line::from(vec![Span::styled(
                "  Theme preview",
                Style::default().add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  fg   ", dim_style),
                Span::styled("the quick brown fox", Style::default().fg(p.fg)),
            ]),
            Line::from(vec![
                Span::styled("  acc  ", dim_style),
                Span::styled("jumps over the lazy dog", Style::default().fg(p.accent)),
            ]),
            Line::from(vec![
                Span::styled("  succ ", dim_style),
                Span::styled("git status: clean", Style::default().fg(p.success)),
            ]),
            Line::from(vec![
                Span::styled("  err  ", dim_style),
                Span::styled("error: something broke", Style::default().fg(p.error)),
            ]),
            Line::from(vec![
                Span::styled("  warn ", dim_style),
                Span::styled("warning: check the docs", Style::default().fg(p.warning)),
            ]),
            Line::from(vec![
                Span::styled("  dim  ", dim_style),
                Span::styled("(dimmed text)", Style::default().fg(p.dim)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Current selection: ", dim_style),
                Span::styled(
                    picker.current().display_name(),
                    Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Original theme:   ", dim_style),
                Span::styled(picker.original.display_name(), Style::default().fg(p.fg)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Press ", dim_style),
                Span::styled("Enter", Style::default().fg(p.accent)),
                Span::styled(" to commit, ", dim_style),
                Span::styled("Esc", Style::default().fg(p.accent)),
                Span::styled(" to revert.", dim_style),
            ]),
        ]
    };
    let preview = Paragraph::new(preview_lines)
        .style(Style::default().bg(bg))
        .block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Theme::dim())
                .style(Style::default().bg(bg)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(preview, inner[1]);
}

fn draw_mode_strip(f: &mut Frame, app: &App, area: Rect) {
    let bg = PALETTE.with(|p| p.borrow().bg);
    let dup_label = if app.duplicate_filter {
        "last only"
    } else {
        "all entries"
    };
    // Exit-filter chip is hidden entirely when the filter is at
    // its default (`All`). Showing it always would be visual
    // noise — the All/OK/ERR distinction only matters once the
    // user has changed it away from "show everything".
    let exit_chip = if app.exit_filter == ExitFilter::default() {
        None
    } else {
        Some(exit_filter_badge(app.exit_filter))
    };
    let mut spans = vec![
        Span::styled("smart", Theme::dim()),
        Span::styled("history", Theme::accent()),
        Span::styled("  ", Theme::default()),
        mode_badge(app.mode),
        Span::styled("  ", Theme::default()),
        duplicate_filter_badge(app.duplicate_filter),
    ];
    if let Some(chip) = exit_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    spans.push(Span::styled(
        format!(
            "  {} · {} ",
            match app.mode {
                Mode::Sess => "current session only",
                Mode::Dir => "current directory only",
                Mode::Global => "all history",
                Mode::Stats => "predicted next + newest",
            },
            dup_label,
        ),
        Theme::dim(),
    ));
    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).style(Style::default().bg(bg));
    f.render_widget(paragraph, area);
}

fn duplicate_filter_badge(on: bool) -> Span<'static> {
    let (label, color) = if on {
        ("LAST", Theme::success_color())
    } else {
        ("ALL", Theme::accent_color())
    };
    Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

fn exit_filter_badge(filter: ExitFilter) -> Span<'static> {
    let (label, color) = match filter {
        ExitFilter::All => ("ALL", Theme::accent_color()),
        ExitFilter::Success => ("OK", Theme::success_color()),
        ExitFilter::Failed => ("ERR", Theme::error_color()),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

fn mode_badge(mode: Mode) -> Span<'static> {
    let (label, color) = match mode {
        Mode::Sess => ("SESS", Theme::success_color()),
        Mode::Dir => ("DIR", Theme::warning_color()),
        Mode::Global => ("GLOBAL", Theme::accent_color()),
        Mode::Stats => ("STATS", Theme::warning_color()),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let merged = app.merged_rows();
    let age_width = merged
        .iter()
        .map(|r| format_diff(r.timestamp).chars().count())
        .max()
        .unwrap_or(3)
        .max(3);

    // Build the real row items. Rows are stored newest-first; for
    // display we want oldest at the top and newest at the bottom,
    // so reverse the order. Pass `is_selected` based on the data index.
    let real_items: Vec<ListItem> = merged
        .iter()
        .enumerate()
        .rev()
        .map(|(data_idx, r)| {
            let is_selected = app.list_state.selected() == Some(data_idx);
            ListItem::new(render_row(r, app, is_selected, age_width))
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
    let rendered_idx = app
        .list_state
        .selected()
        .map(|data_idx| pad + (real_count.saturating_sub(1) - data_idx));

    // Always start the list from the bottom of the visible window.
    // When the list fits within the visible height we pad with empty
    // items above; when it is taller, we anchor the offset so the
    // last entry sits at the bottom and the user scrolls upward to
    // see older entries.
    let offset = if real_count >= visible_height {
        // Anchor at the bottom: offset = real_count - visible_height.
        // This positions the newest entry at the bottom row and leaves
        // older entries visible above as the user scrolls up.
        real_count.saturating_sub(visible_height)
    } else {
        0
    };

    // Replace the state so we can set the offset explicitly. Preserve
    // the rendered selection for this frame.
    let mut render_state = ListState::default().with_offset(offset);
    render_state.select(rendered_idx);

    let title = format!(" History — {} ", merged.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .title(title)
                .title_style(Theme::accent())
                .border_style(Theme::dim())
                .style(Style::default().bg(PALETTE.with(|p| p.borrow().list_bg))),
        )
        .highlight_style(
            Style::default()
                .bg(Theme::selection_color())
                .fg(PALETTE.with(|p| p.borrow().fg))
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

    // Maintain a separate selection index for the "all labeled" view so
    // that switching back and forth between the two panes preserves the
    // cursor position in each.
    if app.is_labeled_view() {
        app.labeled_list_state = ListState::default().with_offset(0);
        app.labeled_list_state.select(data_idx);
    } else {
        app.list_state = ListState::default().with_offset(0);
        app.list_state.select(data_idx);
    }
}

/// Render a single history row as a `Line` with optional query
/// highlighting. The layout is a fixed-width columnar form:
///
///   [age] [status]  command  ·  time
///
/// `age_width` is the right-aligned width of the age column so rows
/// line up.
fn render_row<'a>(row: &'a HistoryRow, app: &App, is_selected: bool, age_width: usize) -> Line<'a> {
    let age = format_diff(row.timestamp);
    let age_padded = format!("{:>age_width$}", age);

    let exit_marker = if row.exit_code == 0 { "✓" } else { "✗" };
    let exit_style = if row.exit_code == 0 {
        Theme::success()
    } else {
        Theme::error()
    };

    // Capture indicator. A bright `o ` shows the row has captured
    // output available (press ^L to view); a dim `. ` is shown
    // otherwise so columns stay aligned.
    let capture_span = if !row.output.is_empty() {
        Span::styled(
            " o ",
            Style::default()
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" . ", Theme::dim())
    };

    let mut spans = vec![
        capture_span,
        Span::styled(format!(" {} ", age_padded), Theme::accent()),
        Span::raw(" "),
        Span::styled(format!(" {} ", exit_marker), exit_style),
        Span::raw(" "),
    ];

    // Highlight query matches inside the command. When the query is
    // a regex (prefixed with `/`) we use the compiled regex to find
    // all matches and bold each one. Otherwise the standard plain-
    // text multi-word highlight runs.
    if app.is_regex_query() {
        spans.extend(highlight_regex_matches(
            &row.command,
            app.query_regex.as_ref(),
        ));
    } else {
        spans.extend(highlight_matches(&row.command, &app.query));
    }

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
                .fg(Theme::warning_color())
                .add_modifier(Modifier::ITALIC),
        ));
    } else if is_selected {
        spans.push(Span::styled(format!("· {} ", row.directory), Theme::dim()));
    }

    Line::from(spans)
}

/// Return a sequence of spans that wrap every occurrence of `query`
/// in `text` with a highlight style. Matching is case-insensitive and
/// based on Unicode scalar values. Adjacent non-matching characters
/// are coalesced into a single span.
fn highlight_regex_matches<'a>(text: &'a str, regex: Option<&Regex>) -> Vec<Span<'a>> {
    let Some(re) = regex else {
        return vec![Span::raw(text)];
    };
    let text_chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut last_end = 0usize;
    for m in re.find_iter(text) {
        // `m.start()`/`m.end()` are byte offsets; convert to char
        // indices so we slice `text_chars` (a `Vec<char>`).
        let start_char = text[..m.start()].chars().count();
        let end_char = start_char + m.as_str().chars().count();
        if start_char > last_end {
            let prefix: String = text_chars[last_end..start_char].iter().collect();
            spans.push(Span::raw(prefix));
        }
        let matched: String = text_chars[start_char..end_char].iter().collect();
        spans.push(Span::styled(
            matched,
            Style::default()
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        ));
        last_end = end_char;
    }
    if last_end < text_chars.len() {
        let tail: String = text_chars[last_end..].iter().collect();
        spans.push(Span::raw(tail));
    }
    if spans.is_empty() {
        spans.push(Span::raw(text));
    }
    spans
}

/// Return a sequence of spans that wrap every occurrence of `query`
pub(super) fn highlight_matches<'a>(text: &'a str, query: &str) -> Vec<Span<'a>> {
    if query.is_empty() {
        return vec![Span::raw(text)];
    }

    let words: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if words.is_empty() {
        return vec![Span::raw(text)];
    }

    let lower_text = text.to_lowercase();
    let text_chars: Vec<char> = text.chars().collect();
    let mut highlights = vec![false; text_chars.len()];

    for word in words {
        let word_chars: Vec<char> = word.chars().collect();
        if word_chars.is_empty() {
            continue;
        }
        let mut i = 0;
        while i + word_chars.len() <= text_chars.len() {
            if lower_text
                .chars()
                .skip(i)
                .take(word_chars.len())
                .collect::<Vec<char>>()
                == word_chars
            {
                for j in 0..word_chars.len() {
                    highlights[i + j] = true;
                }
                i += word_chars.len();
            } else {
                i += 1;
            }
        }
    }

    let mut spans = Vec::new();
    let mut i = 0;
    while i < text_chars.len() {
        let start = i;
        let is_highlight = highlights[i];
        while i < text_chars.len() && highlights[i] == is_highlight {
            i += 1;
        }
        let segment: String = text_chars[start..i].iter().collect();
        if is_highlight {
            spans.push(Span::styled(
                segment,
                Style::default()
                    .fg(Theme::highlight_color())
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(segment));
        }
    }

    spans
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Details ")
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)));

    let Some(row) = app.selected_row() else {
        let empty = Paragraph::new(Line::from(vec![Span::styled(
            "No command selected",
            Theme::dim(),
        )]))
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
            Span::styled("Cmd  ", Theme::dim()),
            Span::styled(
                row.command.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Dir  ", Theme::dim()),
            Span::raw(row.directory.clone()),
        ]),
        Line::from(vec![
            Span::styled("Sess ", Theme::dim()),
            Span::raw(row.session_id.clone()),
        ]),
        Line::from(vec![
            Span::styled("Time ", Theme::dim()),
            Span::raw(format!(
                "{} · {}",
                format_time(row.timestamp),
                format_diff(row.timestamp),
            )),
        ]),
        Line::from(vec![
            Span::styled("Stat ", Theme::dim()),
            Span::styled(format!("{} {}", exit_marker, exit_text), Theme::success()),
        ]),
    ];

    // Add the comment line only when one exists.
    if !row.comment.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Rem  ", Theme::dim()),
            Span::styled(
                row.comment.clone(),
                Style::default()
                    .fg(Theme::warning_color())
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_output_preview(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Output Preview ")
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)));

    let Some(row) = app.selected_row() else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("", Theme::default())))
                .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)))
                .block(block),
            area,
        );
        return;
    };

    if row.output.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No output captured", Theme::dim()))
                .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)))
                .block(block),
            area,
        );
        return;
    }

    let preview_lines: Vec<Line> = row
        .output
        .lines()
        .take(4) // Show up to 4 lines to fit the new larger detail pane
        .map(|l| Line::from(Span::styled(l.to_string(), Theme::default())))
        .collect();

    let paragraph = Paragraph::new(preview_lines)
        .block(block)
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let is_regex = app.is_regex_query();
    let (prompt, title, content) = match app.comment_edit {
        Some(ref buf) => ("comment> ", " comment ", buf.as_str()),
        None => {
            if is_regex {
                ("/", " regex ", app.query.as_str())
            } else {
                ("> ", " search ", app.query.as_str())
            }
        }
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
            .title_style(if is_regex {
                Style::default().fg(Theme::warning_color())
            } else {
                Theme::accent()
            })
            .border_style(if app.comment_edit.is_some() {
                Style::default().fg(Theme::warning_color())
            } else if is_regex {
                Style::default().fg(Theme::warning_color())
            } else {
                Theme::dim()
            })
            .style(Style::default().bg(PALETTE.with(|p| p.borrow().input_bg))),
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
        Some(row) if !row.output.is_empty() => " ^H help · ^D del · ^X del all · ^U clear",
        Some(_) => " ^H help · ^D del · ^X del all · ^U clear",
        None => " ^H help · ^D del · ^X del all · ^U clear",
    };

    // Active theme badge. Rendered at the right edge of the status
    // bar so the help text keeps its existing left-anchored layout.
    let theme_label = format!(" theme: {} ", app.theme.display_name());

    // Transient feedback (e.g. "Yanked 12 chars") takes
    // precedence over the help hint when present, so the user
    // can't miss the result of an action like yank. The
    // success / failure colour is chosen by the message
    // contents: anything that starts with "Yank failed" is
    // treated as an error so the user notices even on a
    // brief glance.
    let status = app.status_message.as_ref().map(|(m, _)| m.as_str());
    let (middle_text, middle_style) = match status {
        Some(m) if m.starts_with("Yank failed") => (format!(" {} ", m), Theme::error()),
        Some(m) => (format!(" {} ", m), Theme::success()),
        None => (help.to_string(), Theme::dim()),
    };

    let line = Line::from(vec![
        Span::styled(format!(" {}  ", count), Theme::highlight()),
        Span::styled(middle_text, middle_style),
        Span::styled(theme_label, Theme::accent()),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(PALETTE.with(|p| p.borrow().status_bg))),
        area,
    );
}
