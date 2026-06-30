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
use super::state::{ExitFilter, HistoryRow, Mode, SortOrder};
use super::theme::palette_storage::PALETTE;
use super::theme::{Theme, ThemePicker};
use super::{
    format_diff, format_time, App, CommandMenu, ConfirmMode, CorrectView, DescribeView, HelpView,
    NotesDateFilter, OutputView, QuestionView,
};
use regex::Regex;

pub(super) fn ui(f: &mut Frame, app: &mut App) {
    if let Some(ref view) = app.output_view {
        draw_output_view(f, app, view);
        return;
    }

    if let Some(ref view) = app.describe_view {
        draw_describe_view(f, app, view);
        return;
    }

    if let Some(ref view) = app.correct_view {
        draw_correct_view(f, app, view);
        return;
    }

    if let Some(ref view) = app.question_view {
        draw_question_view(f, app, view);
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

    // Use the user's actual `Cancel`
    // binding(s) instead of
    // hard-coding `Esc`. The
    // dialog has its own
    // dedicated handler
    // (`handle_confirm_delete_key`)
    // that closes on the user's
    // Cancel binding plus `n`
    // and `Ctrl+C`, so the
    // label here matches the
    // behavior. Falls back to a
    // short hint when Cancel is
    // fully unbound so the
    // pane doesn't show a stale
    // spec.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let cancel_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        cancel_keys
    };
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
            Span::styled(cancel_hint, Theme::highlight()),
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

fn draw_output_view(f: &mut Frame, app: &App, view: &OutputView) {
    let area = f.area();
    // The output view toggles on
    // its own open key (default
    // `Ctrl+L` —
    // `Action::ShowOutput`),
    // configurable via
    // `key.show-output=...`.
    // Show the actual binding(s)
    // in the title so the user
    // can see what to press, and
    // add the `Cancel` binding
    // so they can also see how
    // to dismiss the view
    // without toggling it back
    // on.^E (edit-comment)
    // stays literal because
    // that's a different
    // independent action.
    let show_keys = format_key_specs(app.bindings.specs(Action::ShowOutput));
    let toggle_hint = if show_keys.is_empty() {
        "no key".to_string()
    } else {
        format!("{} toggle", show_keys)
    };
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        "no key".to_string()
    } else {
        format!("{} close", cancel_keys)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(
            " Captured output (\u{2191}\u{2193} scroll, ^E edit, {}, {}) ",
            toggle_hint, close_hint
        ))
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

/// Full-screen overlay that shows the LLM's
/// description of the selected history row.
///
/// The shape mirrors the captured-output overlay
/// (`draw_output_view`): a rounded border, a
/// descriptive title, a scrollable body, and a
/// scroll-position footer. The title is built
/// from the row's command so the user can see
/// exactly which row is being described (useful
/// when navigating the list while the overlay is
/// open — the LLM was asked about a specific
/// command, not the current selection).
///
/// Long responses are handled by the scroll
/// offset; short ones (the typical case — the
/// prompt asks for at most four sentences) fit on
/// a single screen and don't need scrolling.
fn draw_describe_view(f: &mut Frame, app: &App, view: &DescribeView) {
    let area = f.area();
    // Use the actual `Describe` binding(s)
    // (default `Ctrl+K`,
    // configurable via `key.describe=...`).
    // Describe toggles on the same
    // key that opened it, so the
    // "close hint" is the same
    // spec. We separate the
    // strings so multi-key
    // bindings render both
    // options ("Ctrl+K, F1 close").
    let describe_keys = format_key_specs(app.bindings.specs(Action::Describe));
    let close_hint = if describe_keys.is_empty() {
        "no key bound".to_string()
    } else {
        format!("{} close", describe_keys)
    };
    // Account for the close
    // hint's length so the
    // command text isn't
    // over-truncated on narrow
    // panes. The 20 was a rough
    // estimate of "(↑↓ scroll, ^K close)".
    let hint_len = close_hint.chars().count() + 4;
    // Build a short title that shows the command
    // being described. Long commands are truncated
    // with an ellipsis so the title stays
    // single-line and within the border.
    let title = {
        let max = (area.width as usize).saturating_sub(15 + hint_len).max(20);
        if view.command.chars().count() > max {
            let keep = max.saturating_sub(1);
            let mut s: String = view.command.chars().take(keep).collect();
            s.push('…');
            format!(
                " Describe: {} (\u{2191}\u{2193} scroll, {}) ",
                s, close_hint
            )
        } else {
            format!(
                " Describe: {} (\u{2191}\u{2193} scroll, {}) ",
                view.command, close_hint
            )
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let all_lines: Vec<&str> = view.text.lines().collect();
    let total = all_lines.len();
    // Inner height excludes the top and bottom borders.
    let inner_h = area.height.saturating_sub(2) as usize;
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = view.scroll.min(max_scroll);

    // Window of visible lines. Wrap is enabled so
    // a single very long sentence (a URL pasted
    // into a command, for example) flows across
    // multiple terminal lines rather than getting
    // truncated. The max-scroll computation uses
    // `lines().count()` which is the un-wrapped
    // line count, so we may end up with a few
    // empty lines at the bottom of the body on
    // very narrow terminals — that's harmless.
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

    // Footer with scroll position (only if there
    // is room inside the border). The "1/1" form
    // is a single page; "3/7" means line 3 of 7
    // is the bottom of the visible window.
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

/// Full-screen modal overlay for the LLM "correct"
/// action.
///
/// The layout is two stacked panes:
///
/// 1. **Original command** (top) — a small,
///    read-only label showing what the user had
///    selected. Includes the directory and exit
///    code as a sanity check (so the user can see
///    "ah, the LLM was correcting THIS row, not
///    some other one").
/// 2. **Corrected command** (middle) — the LLM's
///    proposal, drawn in the accent color so it
///    stands out as the actionable item.
/// 3. **Footer** (bottom) — a one-line prompt
///    reminding the user that `Enter` accepts and
///    `Esc` cancels.
///
/// The corrected command is shown as plain text
/// (no syntax highlighting, no markdown) because
/// the LLM is the source of truth for the string
/// and we don't want a render-time mistake to
/// make a working command look broken (or vice
/// versa). Long commands wrap across lines via
/// ratatui's `Wrap` widget; very long commands
/// are handled by the height of the available
/// space and the user can resize the terminal if
/// they need more room.
fn draw_correct_view(f: &mut Frame, app: &App, view: &CorrectView) {
    use ratatui::text::Span;
    let area = f.area();
    // Render the user's actual
    // Cancel binding(s) instead
    // of hard-coding `Esc`.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let cancel_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        cancel_keys
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(
            " Correct (Enter to run corrected, {} to cancel) ",
            cancel_hint
        ))
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    // The body is two paragraphs stacked
    // vertically. We split the inner area (minus
    // the border) at 50/50 by default, but let the
    // original-command pane shrink to a single
    // line when the command is short and the
    // corrected-command pane take the rest.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    // Reserve the bottom row for the footer
    // prompt, then split the rest into two panes.
    let (body_area, footer_area) = if inner.height >= 4 {
        let footer_h: u16 = 1;
        let body = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: inner.height.saturating_sub(footer_h),
        };
        let footer = Rect {
            x: inner.x,
            y: inner.y + body.height,
            width: inner.width,
            height: footer_h,
        };
        (Some(body), Some(footer))
    } else {
        // Tiny terminal: skip the footer entirely
        // and let the body fill the inner area.
        (Some(inner), None)
    };

    if let Some(body) = body_area {
        // Split the body in two: original (top),
        // corrected (bottom). The original is a
        // small label, so give it one line; the
        // corrected takes the rest.
        let original_h: u16 = if body.height >= 2 { 2 } else { 1 };
        let original_area = Rect {
            x: body.x,
            y: body.y,
            width: body.width,
            height: original_h,
        };
        let corrected_area = Rect {
            x: body.x,
            y: body.y + original_h,
            width: body.width,
            height: body.height.saturating_sub(original_h),
        };

        // Original command: a dimmed label
        // showing what was being corrected.
        // Long commands wrap; the user can
        // see the full string by looking at
        // the corrected pane alongside it.
        let original_para = Paragraph::new(Line::from(Span::styled(
            format!("Original:  {}", view.original_command),
            Theme::dim(),
        )))
        .wrap(Wrap { trim: false });
        f.render_widget(original_para, original_area);

        // Corrected command: the accent
        // color makes it the focal point of
        // the overlay. A `>` prefix echoes
        // shell-prompt conventions and
        // signals "this is the proposed
        // command".
        let corrected_para = Paragraph::new(Line::from(Span::styled(
            format!("Corrected: {}", view.corrected_command),
            Theme::accent(),
        )))
        .wrap(Wrap { trim: false });
        f.render_widget(corrected_para, corrected_area);
    }

    if let Some(footer) = footer_area {
        let footer_para = Paragraph::new(Line::from(Span::styled(
            " \u{21B5} Enter: run corrected  \u{00B7}  Esc: cancel  \u{00B7}  ^C: abort TUI ",
            Theme::dim(),
        )));
        f.render_widget(footer_para, footer);
    }

    // The block is the visual frame; we draw it
    // last so the border sits cleanly on top of
    // any sub-pixel rounding from the inner
    // widgets.
    f.render_widget(block, area);
}

/// Full-screen overlay for the general question
/// action (prefixed with `%`).
///
/// Mirrors the describe overlay in shape (a piece of
/// text + a scroll offset) but is driven by the user's
/// question rather than by a command description.
fn draw_question_view(f: &mut Frame, app: &App, view: &QuestionView) {
    let area = f.area();
    // Build a short title that
    // shows the question. The
    // close hint reflects the
    // user's `Cancel` binding
    // (default `Esc`,
    // configurable via
    // `key.cancel=...`). The
    // legacy `q/Esc` hardcoded
    // hint was misleading when
    // the user had rebound
    // Cancel away from Esc —
    // and the question overlay
    // historically closed on
    // both `q` and `Esc`, so
    // showing only one true
    // form keeps the label and
    // behavior consistent.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        format!("{} close", cancel_keys)
    };
    // Account for the
    // close-hint's length so the
    // question text isn't
    // over-truncated on narrow
    // panes. The 25 was a
    // rough estimate of
    // "(↑↓ scroll, q/Esc close)"
    // — we now use a tighter
    // bound based on the actual
    // hint string.
    let hint_len = close_hint.chars().count() + 4;
    let title = {
        let max = (area.width as usize).saturating_sub(15 + hint_len).max(20);
        if view.question.chars().count() > max {
            let keep = max.saturating_sub(1);
            let mut s: String = view.question.chars().take(keep).collect();
            s.push('…');
            format!(
                " Question: {} (\u{2191}\u{2193} scroll, {}) ",
                s, close_hint
            )
        } else {
            format!(
                " Question: {} (\u{2191}\u{2193} scroll, {}) ",
                view.question, close_hint
            )
        }
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::accent())
        .border_style(Theme::dim());

    let all_lines: Vec<&str> = view.text.lines().collect();
    let total = all_lines.len();
    let inner_h = area.height.saturating_sub(2) as usize;
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = view.scroll.min(max_scroll);

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

    // Footer with scroll position.
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

    // Render the user's configured
    // `Cancel` and `OpenHelp`
    // bindings (rebindable via
    // `key.cancel=...` /
    // `key.open-help=...`) so the
    // title always tells them how
    // to close / reopen. The
    // legacy `q` fallback was
    // hard-coded and lied when
    // the user had moved the
    // bindings.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        String::from("(no key bound)")
    } else {
        format!("{} to close", cancel_keys)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(" Help — {} ", close_hint))
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
        "type to filter (plain text multi-word AND; prefix `/` for regex, `?` for fuzzy, `=` for LLM command generation)",
    );
    row(
        &mut lines,
        binding_for(Action::Backspace),
        "delete one character from the query",
    );
    row(
        &mut lines,
        binding_for(Action::DeleteWordBackward),
        "delete one word backward (readline `Ctrl-W` semantics)",
    );
    row(
        &mut lines,
        binding_for(Action::ClearQuery),
        "clear the query",
    );
    row(
        &mut lines,
        binding_for(Action::ToggleSearchMode),
        "cycle search mode: plain → regex (`/`) → fuzzy (`?`) → output (`+`) → plain",
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
        binding_for(Action::CycleSortOrder),
        "cycle sort order: AGE (newest first) → FREQ (most-run first) → AGE",
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
        binding_for(Action::EditFileReference),
        "open a filename referenced in the selected command in $EDITOR",
    );
    row(
        &mut lines,
        binding_for(Action::Describe),
        "ask the LLM what the selected command does (4-sentence summary)",
    );
    row(
        &mut lines,
        binding_for(Action::Correct),
        "ask the LLM to fix the selected command (Enter to run the corrected version)",
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
    // Footer hint: mirror the
    // user's actual Cancel
    // binding(s) here too, so
    // the close hint is
    // consistent between the
    // title and the body of
    // the help. The legacy
    // "Esc, Enter, or q"
    // message was wrong on
    // two counts: `q` only
    // closed the help if the
    // user hadn't rebound
    // Cancel, and `Enter` is
    // a real (separate) key
    // in the help overlay
    // (it would close it
    // because the user
    // hasn't yet rebound
    // Cancel).
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let hint = if cancel_keys.is_empty() {
        "Press the configured key to close this help.".to_string()
    } else {
        format!("Press {} to close this help.", cancel_keys)
    };
    lines.push(Line::from(vec![Span::styled(hint, warning)]));

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

    // Render the user's configured
    // `Cancel` bindings (default
    // `Esc`, configurable via
    // `key.cancel=...`) so the
    // title always tells them how
    // to close the palette. We
    // used to hard-code
    // "Esc/q to close" here —
    // which both lied (the user
    // couldn't actually close
    // with `q` if they had bound
    // Cancel to something else)
    // and tripped users typing
    // `q` into the filter box.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let title = if cancel_keys.is_empty() {
        // User unbound Cancel
        // entirely. Still show
        // something so the pane
        // doesn't look unlabelled.
        String::from(" Command palette ")
    } else {
        format!(" Command palette — {} to close ", cancel_keys)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
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
        let config_key = action.config_key();
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
                format!("  {:<22} ({})", label, config_key),
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
    // Render the actual `Cancel`
    // binding(s) instead of
    // hard-coding `Esc`. The
    // footer is the user's only
    // reminder of how to dismiss
    // the picker; a misleading
    // label (`Esc close` when
    // Esc isn't bound to Cancel)
    // would be worse than no
    // label. Falls back to a
    // short "no key" hint when
    // Cancel is unbound so the
    // pane doesn't show a stale
    // key spec.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        format!("{} close", cancel_keys)
    };
    let footer = Line::from(vec![
        Span::styled(
            format!(" {}/{} actions", filtered.len(), menu.actions.len()),
            dim_style,
        ),
        Span::raw(format!("  up/down move  Enter run  {} ", close_hint)),
    ]);
    let footer_para = Paragraph::new(footer).style(Style::default().bg(bg));
    f.render_widget(footer_para, chunks[2]);
}

fn draw_theme_picker(f: &mut Frame, app: &App, picker: &ThemePicker) {
    use ratatui::widgets::List;

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    // Centered popup. Two horizontal columns:
    //   [0] the list of themes (55% of width)
    //   [1] a preview pane (45% of width) showing the live
    //       palette in action.
    let outer = centered_rect(75, 70, f.area());
    f.render_widget(ratatui::widgets::Clear, outer);

    // Use the user's actual
    // `Cancel` binding(s) in
    // the title. Enter commits
    // is fixed (the theme
    // picker has no `Commit`
    // action — only Enter can
    // commit because that's
    // the universal "select
    // this row" key in
    // `draw_list`). The revert
    // hint is dynamic.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let revert_hint = if cancel_keys.is_empty() {
        "no key".to_string()
    } else {
        format!("{} reverts", cancel_keys)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(" Theme picker  Enter commits / {} ", revert_hint))
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
    // Same logic for the LLM chip: it's only useful when the
    // user has typed `=...` to ask the LLM to generate a
    // command. Showing it always would add visual noise
    // similar to the exit-filter chip above.
    let llm_chip = if app.is_llm_query() {
        Some(llm_mode_badge(app.llm.is_some()))
    } else {
        None
    };
    // Same gating logic for the output-mode chip. The
    // chip is only useful when the user has typed `+...`
    // to ask for "which command produced this output?";
    // showing it always would be noise. There is no
    // "not configured" state — output mode is always
    // available, just useless for commands that have no
    // captured output.
    let output_chip = if app.is_output_query() {
        Some(output_mode_badge())
    } else {
        None
    };
    // Sort-order chip is hidden when the order is at
    // its default (`Age`, the historical timestamp-DESC
    // behaviour). Showing it always would be visual
    // noise — the user has to actively choose
    // `Frequency` to see this chip, so its presence
    // is itself the signal.
    // Notes-mode date-filter chip is shown only
    // when (a) we're in notes mode AND (b) a
    // date-filter alias is currently active
    // (`@today` / `@week` / `@month` / `@year`).
    // Otherwise it stays hidden so the chip strip
    // is uncluttered for users who don't use the
    // aliases.
    let notes_date_chip = if app.is_notes_query() && app.notes_date_filter != NotesDateFilter::All {
        Some(notes_date_filter_badge(app.notes_date_filter))
    } else {
        None
    };
    let sort_chip = if app.sort_order != SortOrder::default() {
        Some(sort_order_badge(app.sort_order))
    } else {
        None
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
    if let Some(chip) = llm_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    if let Some(chip) = output_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    if let Some(chip) = sort_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    if let Some(chip) = notes_date_chip {
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

/// The LLM-mode chip. Tinted magenta when ollama is configured
/// (the user can press Enter and expect a generated command);
/// tinted red when the query starts with `=` but ollama isn't
/// configured (Enter will surface a "not configured" status
/// instead of generating a command). The colour difference is
/// a small affordance — the user would otherwise have to press
/// Enter to learn the feature is unavailable.
fn llm_mode_badge(configured: bool) -> Span<'static> {
    let color = if configured {
        Theme::accent_color()
    } else {
        Theme::error_color()
    };
    Span::styled(
        " LLM ".to_string(),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

/// The output-mode chip. Tinted with the `info` color
/// (blue by default, override via `tuicolor.info=`) so
/// the user can see at a glance that the query is being
/// matched against captured output. There is no
/// "configured" / "not configured" state — the feature
/// is always available; the chip just reminds the user
/// they're in output-search mode.
fn output_mode_badge() -> Span<'static> {
    Span::styled(
        " OUTPUT ".to_string(),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(Theme::info_color())
            .add_modifier(Modifier::BOLD),
    )
}

/// The notes-mode date-filter chip. Shown only
/// when (a) the user is in notes search mode and
/// (b) the current query contains an active
/// date-filter alias (`@today`, `@week`,
/// `@month`, `@year`). The chip label is the
/// alias name in uppercase, tinted with the
/// success color (green) so it's visually
/// distinct from the existing `OUTPUT` /
/// `FREQ` / `LLM` chips.
///
/// We surface the filter in the mode strip
/// because the date filter is invisible in the
/// list itself: the user typed `@today test`,
/// sees notes matching `test` and the current
/// day, and might wonder why some notes that
/// obviously contain `test` are missing. The
/// chip answers the question.
fn notes_date_filter_badge(filter: NotesDateFilter) -> Span<'static> {
    let label = match filter {
        NotesDateFilter::All => "ALL",
        NotesDateFilter::Today => "TODAY",
        NotesDateFilter::Week => "WEEK",
        NotesDateFilter::Month => "MONTH",
        NotesDateFilter::Year => "YEAR",
    };
    Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(Theme::success_color())
            .add_modifier(Modifier::BOLD),
    )
}

/// The sort-order chip. Shown only when the sort
/// differs from the default (`Age`); the user has to
/// actively choose `Frequency` to see it, so the chip
/// itself is the signal that the list is in a
/// non-default order. Tinted with the warning color
/// (yellow by default) so it stands out from the mode
/// chips — the user should notice they've moved away
/// from the historical age-DESC sort.
fn sort_order_badge(order: SortOrder) -> Span<'static> {
    let label = match order {
        SortOrder::Age => "AGE",
        SortOrder::Frequency => "FREQ",
    };
    Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(Theme::warning_color())
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

    // The LLM preview row has `exit_code == -1` (the
    // "never executed" sentinel) and a negative `id`.
    // We render it with a distinctive `~` marker and the
    // accent color so the user can tell at a glance that
    // this is a suggestion, not a command they've already
    // run. The `✓`/`✗` markers mean success/failure and
    // would be misleading for a command that hasn't been
    // executed yet.
    // **Important**: the check is on
    // `exit_code == -1`, NOT on
    // `row.id < 0`. Negative ids
    // are also used by todo rows
    // (which encode the 1-based
    // line number as
    // `id = -(line_number)`), so
    // `id < 0` would falsely
    // classify every todo row as
    // an LLM preview. The
    // `exit_code` sentinel is the
    // load-bearing distinction.
    let (exit_marker, exit_style) = if row.is_llm_preview() {
        ("~", Theme::accent())
    } else if row.exit_code == 0 {
        ("✓", Theme::success())
    } else {
        ("✗", Theme::error())
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

    // Tmux-pane activity marker.
    // A bright ` T ` shows that
    // there's at least one tmux
    // pane whose cwd matches
    // this row's `directory`
    // (after canonicalization);
    // a dim `.` keeps the column
    // width stable otherwise.
    // Only fired for directory
    // rows (`row.mode == "directory"`)
    // since the canonical
    // contract for the rest of
    // the history is "the cwd
    // the user ran the command
    // in", which doesn't have a
    // single pane attached to it
    // at any given moment.
    let tmux_span =
        if row.mode == "directory" && app.directory_tmux_pane_id(&row.directory).is_some() {
            Span::styled(
                " T ",
                Style::default()
                    .fg(Theme::accent_color())
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(" . ", Theme::dim())
        };

    // LLM preview marker. The
    // synthetic row the auto-call
    // produces is identified by
    // `exit_code == -1` (the
    // "never executed" sentinel;
    // real history rows always
    // have `exit_code >= 0`).
    // We mark it with a short
    // `[LLM]` tag in the accent
    // color so the user can tell
    // at a glance that this isn't
    // a command they've actually
    // run — it's a suggestion.
    // The exit marker is
    // suppressed for preview
    // rows (the `✓`/`✗` would
    // be misleading because the
    // command hasn't been
    // executed yet).
    // **Important**: the check is
    // on `exit_code == -1`, NOT
    // on `row.id < 0`. Negative
    // ids are also used by todo
    // rows (which encode the
    // 1-based line number as
    // `id = -(line_number)`), so
    // `id < 0` would falsely
    // classify every todo row as
    // an LLM preview. The
    // `exit_code` sentinel is the
    // load-bearing distinction.
    let is_llm_preview = row.is_llm_preview();
    let llm_preview_span = if is_llm_preview {
        Span::styled(
            " [LLM] ",
            Style::default()
                .fg(Theme::accent_color())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let mut spans = vec![
        capture_span,
        tmux_span,
        llm_preview_span,
        Span::styled(format!(" {} ", age_padded), Theme::accent()),
        Span::raw(" "),
        Span::styled(format!(" {} ", exit_marker), exit_style),
        Span::raw(" "),
    ];

    // Highlight query matches
    // inside `row.command`.
    // When the query is a regex
    // (prefixed with `/`) we
    // use the compiled regex to
    // find all matches and bold
    // each one. Otherwise the
    // standard plain-text
    // multi-word highlight
    // runs.
    //
    // For directory rows,
    // `fetch_directories`
    // stores the **directory**
    // (in shell-shortened form)
    // in `row.command` and the
    // last command run there
    // in `row.comment`. So the
    // primary text slot shows
    // the directory (with
    // query matches
    // highlighted against the
    // user's typed path
    // pattern), and the
    // secondary `# ...` slot
    // shows the last command.
    // This is the inverse of
    // the layout for normal
    // history rows (where
    // `row.command` is the
    // runnable command and
    // `row.comment` is a free-
    // form note). The field
    // semantics are the same
    // — only the rendering
    // swaps them — so action
    // handlers (which branch
    // on `row.mode ==
    // "directory"`) keep
    // working unchanged.
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
    // a contextual hint on the selected row when there is no
    // comment. (The `comment` field carries the last command run in
    // a directory for `#`-mode rows, so the secondary slot is the
    // command — we don't `~`-expand it because it's not a path.)
    if !row.comment.is_empty() {
        // The secondary slot is
        // the user's free-form
        // comment for normal
        // rows, and the last
        // command run in the
        // directory for `#`-mode
        // rows. In neither case
        // is it a path that
        // needs `~` expansion;
        // we just display the
        // string verbatim.
        let comment_display = row.comment.clone();
        spans.push(Span::styled(
            format!("# {} ", comment_display),
            Style::default()
                .fg(Theme::warning_color())
                .add_modifier(Modifier::ITALIC),
        ));
    } else if is_selected {
        // Selected-row fallback:
        // the primary text is
        // already the directory
        // for `#`-mode rows, so
        // the fallback hint is
        // the last command run
        // there; for normal rows
        // the primary is the
        // command, so the hint
        // is the directory (with
        // `~` expansion to match
        // the shell convention).
        if row.mode == "directory" {
            let cmd_first_line = row.command.lines().next().unwrap_or("");
            spans.push(Span::styled(format!("· {} ", cmd_first_line), Theme::dim()));
        } else {
            let dir_display = std::borrow::Cow::Borrowed(row.directory.as_str());
            spans.push(Span::styled(format!("· {} ", dir_display), Theme::dim()));
        }
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

/// Truncate a multi-line command string
/// to a single line that fits within
/// the Details pane's Cmd row. The Cmd
/// row uses a fixed-width label column
/// (5 chars for the longest label
/// `Stat `) and lives inside a bordered
/// block (2 chars), so the available
/// width for the cmd text is `pane_width
/// - 7`.
///
/// Returns just the first line of the
/// input, ellipsized (`…`) if it
/// overflows the available width. Empty
/// panes (width 0 or less than the
/// label/border total) return an empty
/// string.
///
/// `pane_width` is the outer width of
/// the Details pane (the `Rect::width`
/// passed to `draw_details`).
fn truncate_cmd_for_details_pane(cmd: &str, pane_width: usize) -> String {
    let label_width = 5usize;
    let border_width = 2usize;
    let max_cmd_width = pane_width.saturating_sub(label_width + border_width);
    if max_cmd_width == 0 {
        return String::new();
    }
    let first_line = cmd.lines().next().unwrap_or("");
    if first_line.chars().count() > max_cmd_width {
        // Keep at least 1 char of the
        // original text + the ellipsis.
        // If the available width is 1 we
        // show just the ellipsis.
        let take = max_cmd_width.saturating_sub(1).max(1);
        let mut s: String = first_line.chars().take(take).collect();
        s.push('…');
        s
    } else {
        first_line.to_string()
    }
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

    // The `Cmd` line must stay on a single
    // line: a todo's `command` text is a
    // free-form markdown string and could
    // be very long (a 200-char sentence,
    // multiple lines, embedded code, etc.).
    // Showing the full multi-line string
    // here would push the rest of the
    // Details rows (Dir / Sess / Time /
    // Stat / Rem) off-screen and break the
    // fixed 6-row layout. We take just the
    // first line, and if that line itself
    // exceeds the available column width
    // we ellipsize it so the layout still
    // holds. The full text remains
    // available in the Output Preview pane
    // below, where the user can scroll if
    // they need the rest.
    let cmd_first_line = row.command.lines().next().unwrap_or("").to_string();
    let cmd_visible = truncate_cmd_for_details_pane(&cmd_first_line, area.width as usize);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Cmd  ", Theme::dim()),
            Span::styled(cmd_visible, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Dir  ", Theme::dim()),
            // Show the directory with
            // `~` expansion so the
            // user sees the short
            // form (matching what
            // they'd type in the
            // shell). The
            // un-abbreviated form is
            // available in the
            // capture column's `·`
            // text for the selected
            // row only, but the
            // Details pane shows the
            // short form too — it's
            // the same convention
            // everywhere, which is
            // what the user asked
            // for ("as much as
            // possible").
            Span::raw(crate::util::expand_home(&row.directory).into_owned()),
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
    // The input border's prompt character and title
    // change based on the active query mode. We
    // compute all the predicates up front so the
    // match below is a single dispatch instead of
    // a long if/else chain (the modes are mutually
    // exclusive — only one prefix can be active
    // at a time, since each one matches a different
    // leading character).
    //
    // Each mode has a distinct visual identity:
    // - plain: accent (cyan/default).
    // - regex (`/`): warning (yellow).
    // - fuzzy (`?`): success (green).
    // - output (`+`): info (blue).
    // - llm (`=`): accent (cyan — same as plain
    //   but the prefix character itself is the
    //   primary signal).
    // - notes (`@`): success (green — search/
    //   navigation colour).
    // - question (`%`): info (blue — queries
    //   return information).
    // - todo (`!`): warning (yellow — calls
    //   attention to action items).
    //
    // Where two modes share a colour, the prefix
    // character is the differentiator. The colour
    // is the secondary reinforcement.
    let is_regex = app.is_regex_query();
    let is_fuzzy = app.is_fuzzy_query();
    let is_output = app.is_output_query();
    let is_llm = app.is_llm_query();
    let is_notes = app.is_notes_query();
    let is_question = app.is_question_query();
    let is_todo = app.is_todo_query();
    let is_directories = app.is_directories_query();
    let (prompt, title, content) = match app.comment_edit {
        Some(ref buf) => ("comment> ", " comment ", buf.as_str()),
        None => {
            if is_regex {
                ("/", " regex ", app.query.as_str())
            } else if is_fuzzy {
                ("?", " fuzzy ", app.query.as_str())
            } else if is_output {
                ("+", " output ", app.query.as_str())
            } else if is_llm {
                // LLM mode is signalled by both a dedicated
                // prefix and a dedicated title in the input
                // border, mirroring the yellow `regex` and
                // green `fuzzy` tints. The LLM tint uses
                // the accent colour (magenta by default)
                // to keep the visual signal distinct from
                // the search-result modes.
                ("=", " LLM ", app.query.as_str())
            } else if is_notes {
                // Notes mode: searching an external
                // note_search SQLite database. Accent
                // (magenta) like the LLM mode — both
                // modes go "outside" the local shell
                // history (notes and LLM), so we share
                // the colour family.
                ("@", " notes ", app.query.as_str())
            } else if is_question {
                // Question mode: a short LLM answer is
                // requested. Uses the info colour
                // (blue by default) to signal
                // "information, not a command".
                ("%", " ? ", app.query.as_str())
            } else if is_todo {
                // Todo mode: scan every file in
                // `notes.dir` for todo lines. The
                // warning colour (yellow) calls
                // attention — the user is now in a
                // scan-everything mode that crosses
                // the boundary between shell history
                // and external note files.
                ("!", " todo ", app.query.as_str())
            } else if is_directories {
                // Directories mode: list every
                // unique directory that's
                // been used in the global
                // history, sorted by recency.
                // Each row surfaces the
                // directory's latest
                // command; selecting the
                // row stages `cd <path>`.
                // The accent (cyan) tint
                // signals "browse"-style —
                // the user is moving
                // *between* directories
                // rather than running a
                // command.
                ("#", " directories ", app.query.as_str())
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
            // The title colour matches the
            // active mode so the input border
            // visually announces which mode
            // you're in, even when the prefix
            // character is off-screen (e.g.
            // you've typed more than fits in the
            // input box).
            .title_style(if is_regex {
                Style::default().fg(Theme::warning_color())
            } else if is_fuzzy {
                Style::default().fg(Theme::success_color())
            } else if is_output {
                Style::default().fg(Theme::info_color())
            } else if is_llm {
                Style::default().fg(Theme::accent_color())
            } else if is_notes {
                Style::default().fg(Theme::accent_color())
            } else if is_question {
                Style::default().fg(Theme::info_color())
            } else if is_todo {
                Style::default().fg(Theme::warning_color())
            } else if is_directories {
                Style::default().fg(Theme::accent_color())
            } else {
                Theme::accent()
            })
            // The border colour matches the title
            // colour for the same reason. We
            // additionally tint the border red
            // when the last notes query failed
            // to parse — that's an error state
            // that's independent of the active
            // mode.
            .border_style(if app.comment_edit.is_some() {
                Style::default().fg(Theme::warning_color())
            } else if app.notes_query_error {
                Style::default().fg(Theme::error_color())
            } else if is_regex {
                Style::default().fg(Theme::warning_color())
            } else if is_fuzzy {
                Style::default().fg(Theme::success_color())
            } else if is_output {
                Style::default().fg(Theme::info_color())
            } else if is_llm {
                Style::default().fg(Theme::accent_color())
            } else if is_notes {
                Style::default().fg(Theme::accent_color())
            } else if is_question {
                Style::default().fg(Theme::info_color())
            } else if is_todo {
                Style::default().fg(Theme::warning_color())
            } else if is_directories {
                Style::default().fg(Theme::accent_color())
            } else {
                Theme::dim()
            })
            .style(Style::default().bg(PALETTE.with(|p| p.borrow().input_bg))),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(input, area);

    // Place the cursor at the current `query_cursor`
    // position. For non-LLM query modes the cursor is
    // always at the end (the input loop ignores Left/Right
    // in those modes), so the visual position is the same
    // as the historical "end of buffer" placement. For LLM
    // mode the user can move the cursor with Left/Right and
    // it follows the typed text. The visible position is
    // computed in *characters* — the same unit
    // `query_cursor` uses — to stay aligned with the
    // rendered glyphs, regardless of how many bytes each
    // character takes in UTF-8.
    //
    // The visible text starts at `area.x + 1` (one cell for
    // the left border). The prompt string includes its own
    // trailing space, so the cursor lands one cell after
    // the prompt and `query_cursor` cells into the buffer.
    let prompt_width = prompt.chars().count() as u16;
    let cursor_x = area.x + 1 + prompt_width + app.query_cursor as u16;
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

    // Build the help hint from the actual configured key bindings
    // so it always reflects what the user has configured.
    let help_open = format_key_specs(app.bindings.specs(Action::OpenHelp));
    let help_del = format_key_specs(app.bindings.specs(Action::DeleteSelected));
    let help_del_all = format_key_specs(app.bindings.specs(Action::DeleteMatching));
    let help_clear = format_key_specs(app.bindings.specs(Action::ClearQuery));
    let help = format!(
        " {} help · {} del · {} del all · {} clear",
        help_open, help_del, help_del_all, help_clear
    );

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
        None => {
            if app.llm_in_flight {
                // Show a loading indicator when an LLM request is in flight.
                (" LLM request in progress… ".to_string(), Theme::warning())
            } else {
                (help.to_string(), Theme::dim())
            }
        }
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

#[cfg(test)]
mod tests {
    use super::truncate_cmd_for_details_pane;

    /// A short single-line cmd fits
    /// unchanged inside the pane.
    #[test]
    fn truncate_short_cmd_unchanged() {
        assert_eq!(truncate_cmd_for_details_pane("ls -la", 80), "ls -la");
    }

    /// A multi-line cmd is reduced to its
    /// first line — the rest of the
    /// command stays in the Output
    /// Preview pane.
    #[test]
    fn truncate_keeps_first_line_only() {
        assert_eq!(
            truncate_cmd_for_details_pane("first line\nsecond line\nthird line", 80),
            "first line"
        );
    }

    /// A single-line cmd that exceeds
    /// the available width is
    /// ellipsized. The total length
    /// (visible chars + ellipsis) must
    /// equal the available width, so
    /// the row never overflows its
    /// cell.
    #[test]
    fn truncate_long_cmd_is_ellipsized() {
        // 80 - 5 (label) - 2 (border) = 73
        // available chars; cmd is 100 chars
        // long; result is 73 chars (72
        // visible + 1 ellipsis).
        let cmd = "a".repeat(100);
        let truncated = truncate_cmd_for_details_pane(&cmd, 80);
        assert_eq!(truncated.chars().count(), 73);
        assert!(truncated.ends_with('…'));
        // The visible portion is the
        // first 72 `a`s, then the
        // ellipsis.
        assert_eq!(truncated, format!("{}…", "a".repeat(72)));
    }

    /// Multi-byte UTF-8 cmd text is
    /// measured in characters, not
    /// bytes. Without this, an emoji
    /// would count as 4 bytes (and
    /// overflow the cell by 3).
    #[test]
    fn truncate_respects_char_boundaries() {
        // 8 panes wide → 1 char available.
        // The cmd is a single emoji, which
        // is exactly 1 char, so it fits.
        assert_eq!(truncate_cmd_for_details_pane("🚀", 8), "🚀");
        // Same pane width, cmd is 2
        // chars (two emoji); the
        // ellipsize should keep 1 char +
        // `…`.
        let truncated = truncate_cmd_for_details_pane("🚀🚀", 8);
        assert_eq!(truncated.chars().count(), 2);
        assert!(truncated.starts_with('🚀'));
        assert!(truncated.ends_with('…'));
    }

    /// A pane that's too narrow for the
    /// label/border overhead (less
    /// than 7 chars wide) returns an
    /// empty string, so we don't try to
    /// render a half-truncated cell
    /// that would break the layout.
    #[test]
    fn truncate_returns_empty_for_very_narrow_pane() {
        assert_eq!(truncate_cmd_for_details_pane("anything", 0), "");
        assert_eq!(truncate_cmd_for_details_pane("anything", 6), "");
        // Width 7 = exactly label + border
        // → 0 chars available → empty.
        assert_eq!(truncate_cmd_for_details_pane("anything", 7), "");
        // Width 8 → 1 char available.
        assert_eq!(truncate_cmd_for_details_pane("a", 8), "a");
    }

    /// The minimum-width result must
    /// always contain at least one
    /// visible character when the
    /// input is non-empty and the pane
    /// is at least one char wider than
    /// label+border. Otherwise a
    /// single-char cmd would render
    /// as nothing at all, which is
    /// confusing.
    #[test]
    fn truncate_minimum_one_visible_char() {
        // Cmd is 10 chars, available is
        // 1 → result is 1 char + ellipsis
        // (still 2 chars total, but at
        // least 1 is real text).
        let truncated = truncate_cmd_for_details_pane("helloworld", 8);
        assert_eq!(truncated.chars().count(), 2);
        assert!(truncated.starts_with('h'));
        assert!(truncated.ends_with('…'));
    }

    /// Empty input is preserved as
    /// empty output. The caller
    /// already handles the
    /// `selected_row().is_none()`
    /// case separately; this is just
    /// for the defensive case where
    /// the row's command is somehow
    /// an empty string.
    #[test]
    fn truncate_empty_input() {
        assert_eq!(truncate_cmd_for_details_pane("", 80), "");
        assert_eq!(truncate_cmd_for_details_pane("", 0), "");
    }
}
