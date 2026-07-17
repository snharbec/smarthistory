#![allow(clippy::if_same_then_else)]
#![allow(clippy::map_identity)]
// Render code: the main `ui` entry point plus all the draw_*
// helpers (draw_output_view, draw_help_view, draw_command_menu,
// draw_theme_picker, etc.) and the highlight_matches helpers.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use super::bindings::{Action, format_key_specs};
use super::state::{ExitFilter, HistoryRow, Mode, SortOrder};
use super::theme::palette_storage::PALETTE;
use super::theme::{Theme, ThemePicker};
use super::{
    AddEntryDialog, AddEntryKind, App, CommandMenu, ConfirmMode, CorrectView, DescribeView, HelpView, NotesDateFilter, OutputView, PrefixPicker, QuestionView,
    format_diff, format_time,
};
use super::CodeGraphRelationsPicker;
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
                Constraint::Fill(1),   // list
                Constraint::Length(8), // details row
                Constraint::Length(3), // input
                Constraint::Length(1), // status
            ]
            .as_ref(),
        )
        .split(f.area());

    draw_mode_strip(f, app, chunks[0]);
    draw_list(f, app, chunks[1]);

    match app.pane_visibility {
        crate::tui::state::PaneVisibility::Both => {
            let detail_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
                .split(chunks[2]);
            draw_details(f, app, detail_chunks[0]);
            draw_output_preview(f, app, detail_chunks[1]);
        }
        crate::tui::state::PaneVisibility::Details => {
            draw_details(f, app, chunks[2]);
        }
        crate::tui::state::PaneVisibility::OutputPreview => {
            draw_output_preview(f, app, chunks[2]);
        }
    }

    draw_input(f, app, chunks[3]);
    draw_status(f, app, chunks[4]);

    if let Some(ref mode) = app.confirm_delete {
        draw_confirm_delete(f, app, mode);
    }

    if let Some(view) = app.help_view.as_ref() {
        draw_help_view(f, app, view);
    }

    if let Some(menu) = app.command_menu.as_ref() {
        draw_command_menu(f, app, menu);
    }

    // The prefix picker is
    // another overlay picker
    // (sibling to the command
    // menu). It is drawn after
    // the command menu so it
    // can "nest" on top if both
    // are open (though that
    // only happens if an action
    // opens the prefix picker
    // from the command menu).
    if let Some(picker) = app.prefix_picker.as_ref() {
        draw_prefix_picker(f, app, picker);
    }

    // The CodeGraph relations picker is a sibling overlay of the
    // prefix picker. Drawn after it (and after the completion/
    // theme pickers below) so it sits on top when both happen to
    // be open.
    if let Some(picker) = app.codegraph_relations_picker.as_ref() {
        draw_codegraph_relations_picker(f, app, picker);
    }

    // The completion menu is a
    // third overlay picker
    // (sibling to the command
    // menu and prefix picker).
    // It is drawn after the
    // prefix picker so it can
    // "nest" on top if both
    // are open (though that
    // only happens if an action
    // opens the prefix picker
    // while the completion
    // menu is also open).
    if let Some(menu) = app.completion_menu.as_ref() {
        draw_completion_menu(f, app, menu);
    }

    if let Some(picker) = app.theme_picker.as_ref() {
        draw_theme_picker(f, app, picker);
    }

    // The add-session /
    // add-host dialog is the
    // topmost overlay: drawn
    // last so it sits on top
    // of every other pane.
    if let Some(dialog) = app.add_entry_dialog.as_ref() {
        draw_add_entry_dialog(f, app, dialog);
    }

    // If a comment exists, draw the labeled entries pane as an overlay
    // so that labeled history elements are always available.
    // (Labeled entries are now merged into the main list instead.)
    #[allow(clippy::overly_complex_bool_expr)]
    let _ = !app.labeled_rows.is_empty();
}

fn draw_confirm_delete(f: &mut Frame, app: &App, mode: &ConfirmMode) {
    let area = centered_rect(60, 25, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let (title, message) = match mode {
        ConfirmMode::DeleteSelected => (
            " Delete selected entry ",
            "This will delete ALL history entries with the same command text,\nincluding their comments and captured output.".to_string(),
        ),
        ConfirmMode::DeleteMatching => (
            " Delete ALL matching entries ",
            format!(
                "Are you sure you want to delete all {} matching entries?",
                app.rows.len()
            ),
        ),
        ConfirmMode::DeleteDirectory { directory, count } => (
            " Delete directory history ",
            format!(
                "This will delete ALL {} history entries in:\n  {}\n\nEvery command ever run in that directory will be removed.",
                count,
                crate::util::shorten_home_path(directory, &app.home_list),
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

/// Draw the add-session /
/// add-host dialog. Renders
/// as a centered overlay with
/// one input line per field
/// (the focused field is
/// highlighted), a status
/// hint (the dialog's source
/// directory and command),
/// and a footer showing the
/// key bindings (Tab, Enter,
/// Esc, Ctrl-C).
fn draw_add_entry_dialog(f: &mut Frame, app: &App, dialog: &AddEntryDialog) {
    // Height: 1 (title) +
    // dialog.fields.len()
    // (one per field) + 1
    // (source hint) + 1
    // (footer) + 2 (borders)
    // = fields + 5. Cap at
    // 80% of the screen
    // height to leave room
    // for the underlying
    // TUI to peek through
    // (visual cue that the
    // dialog is a
    // transient overlay).
    let needed = (dialog.fields.len() as u16) + 5;
    let pct = ((needed * 100) / f.area().height.max(1)).min(80);
    let area = centered_rect(70, pct, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let title = match dialog.kind {
        AddEntryKind::Session => " Add session ",
        AddEntryKind::Host => " Add host ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::accent())
        .border_style(Theme::accent());
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split the inner
    // area into one row per
    // field plus a source
    // hint plus a footer.
    let mut constraints: Vec<Constraint> = dialog
        .fields
        .iter()
        .map(|_| Constraint::Length(1))
        .collect();
    constraints.push(Constraint::Length(1)); // source hint
    constraints.push(Constraint::Length(1)); // footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    // Render each field as
    // a single line:
    // `<name>: <value>`
    // with a marker
    // showing the cursor.
    // The focused field is
    // rendered in the
    // highlight color; the
    // rest in the default
    // foreground.
    for (i, field) in dialog.fields.iter().enumerate() {
        let is_focused = i == dialog.focused;
        let label_style = if is_focused {
            Theme::highlight()
        } else {
            Style::default()
        };
        let value_style = if is_focused {
            Theme::highlight()
        } else {
            Style::default()
        };
        // Split the value
        // into the
        // pre-cursor
        // segment, the
        // cursor cell,
        // and the
        // post-cursor
        // segment so
        // the cursor
        // position is
        // visible. (We
        // approximate
        // the cursor
        // with a
        // reversed
        // space when
        // the value
        // is empty;
        // the
        // placeholder
        // hint is
        // shown in
        // dim style.)
        let chars: Vec<char> = field.value.chars().collect();
        let mut spans: Vec<Span> = Vec::new();
        // `<Name>: `
        spans.push(Span::styled(format!("{}: ", field.name), label_style));
        if field.value.is_empty() && is_focused {
            // Empty
            // focused
            // field:
            // show the
            // placeholder
            // in dim
            // style
            // followed
            // by a
            // reversed
            // space
            // (the
            // cursor).
            spans.push(Span::styled(field.placeholder.to_string(), Theme::dim()));
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        } else {
            // Pre-cursor
            // text.
            let pre: String = chars.iter().take(field.cursor).collect();
            spans.push(Span::styled(pre, value_style));
            // Cursor cell.
            if is_focused {
                if field.cursor < chars.len() {
                    // The
                    // cursor
                    // sits
                    // ON a
                    // character
                    // —
                    // show
                    // the
                    // character
                    // in
                    // reverse.
                    let c = chars[field.cursor];
                    spans.push(Span::styled(
                        c.to_string(),
                        Style::default().add_modifier(Modifier::REVERSED),
                    ));
                } else {
                    // Cursor
                    // is at
                    // the
                    // end —
                    // show
                    // a
                    // reversed
                    // space.
                    spans.push(Span::styled(
                        " ",
                        Style::default().add_modifier(Modifier::REVERSED),
                    ));
                }
            }
            // Post-cursor
            // text.
            let post: String = chars
                .iter()
                .skip(if is_focused {
                    field.cursor + if field.cursor < chars.len() { 1 } else { 0 }
                } else {
                    field.cursor
                })
                .collect();
            if !post.is_empty() {
                spans.push(Span::styled(post, value_style));
            }
        }
        // Required-field
        // marker: a
        // trailing `*` so
        // the user knows
        // which fields
        // must be non-
        // empty.
        if field.required {
            spans.push(Span::styled(" *", Theme::warning()));
        }
        // Error indicator:
        // when the dialog
        // has an error
        // and this is the
        // failing field,
        // show a small
        // marker.
        if let Some(err) = &dialog.error
            && err.contains(field.name)
        {
            spans.push(Span::styled(format!("  ({})", err), Theme::error()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), chunks[i]);
    }

    // Source hint: a dim
    // single line showing
    // where the entry's
    // pre-filled values
    // came from.
    let hint_idx = dialog.fields.len();
    let hint = Line::from(vec![
        Span::styled("from: ", Theme::dim()),
        Span::styled(
            format!(
                "{:?} in {}",
                dialog.source_command,
                crate::util::shorten_home_path(&dialog.source_directory, &app.home_list,),
            ),
            Theme::dim(),
        ),
    ]);
    f.render_widget(Paragraph::new(hint), chunks[hint_idx]);

    // Footer: key
    // bindings hint.
    let footer_idx = hint_idx + 1;
    let footer = Line::from(vec![
        Span::styled("Tab", Theme::highlight()),
        Span::raw("/"),
        Span::styled("S-Tab", Theme::highlight()),
        Span::raw(" next/prev field, "),
        Span::styled("Enter", Theme::highlight()),
        Span::raw(" commit, "),
        Span::styled("Esc", Theme::highlight()),
        Span::raw(" cancel, "),
        Span::styled("Ctrl-U", Theme::highlight()),
        Span::raw(" clear, "),
        Span::styled("Ctrl-W", Theme::highlight()),
        Span::raw(" delete word"),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[footer_idx]);
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
    // The overlay text may carry ANSI escape codes: tags &
    // codegraph modes pipe source context through `bat
    // --color=always`, and ag matches carry ANSI from `ag`.
    // The markdown `render_preview_line` path doesn't parse
    // ANSI (it mangles `\x1b[...m` through the inline parser),
    // so when the text contains an escape we route every
    // visible line through `parse_ansi_line` instead. Plain
    // text (no escape) still goes through the markdown
    // parser so JIRA `##` headings and `**bold**` labels in
    // the JIRA overlay keep their styling.
    let has_ansi = view.text.contains('\x1b');
    let visible: Vec<Line> = if has_ansi {
        all_lines[start..end]
            .iter()
            .map(|l| Line::from(parse_ansi_line(l)))
            .collect()
    } else {
        all_lines[start..end]
            .iter()
            // Each line is run through the
            // markdown parser so the JIRA
            // overlay's `##` headings and
            // `**bold**` labels render with
            // proper styling (instead of as
            // raw text). Non-JIRA overlays
            // (regular captured output) have
            // no markdown structure, so the
            // parser produces plain text
            // spans — same visual result as
            // before, but consistent with
            // the details-pane path.
            .map(|l| render_preview_line(l))
            .collect()
    };

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
pub(super) fn build_help_lines(app: &App) -> Vec<Line<'static>> {
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
    // Theme cycling used to default to C-n / C-p; those keys
    // are now claimed by per-mode query-history recall
    // (PreviousHistory / NextHistory), so theme cycling ships
    // unbound by default. Users who want keyboard theme
    // cycling can rebind it (e.g. `M-n` / `M-p`) in the config
    // file.
    row(
        &mut lines,
        binding_for(Action::PreviousHistory),
        "previous history entry for the current mode (readline `previous-history`)",
    );
    row(
        &mut lines,
        binding_for(Action::NextHistory),
        "next history entry for the current mode (readline `next-history`)",
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
        binding_for(Action::CodegraphRelations),
        "browse callers / callees of the selected & / $ symbol and open one in $EDITOR",
    );
    row(
        &mut lines,
        binding_for(Action::SmartOpen),
        "context dive: & / $ opens callers/callees; - opens the JIRA issue in the browser (background); ! toggles the selected todo's checkbox; ~ opens the selected file via the per-extension command from `smart-open.<ext>` in the config; else selects the row",
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

    // ----- Config -----
    //
    // The two `Add*` actions
    // open a multi-field
    // dialog that writes a
    // new `session.<id>` or
    // `host.<id>` line to
    // `~/.config/smarthistory/config`
    // and refreshes the
    // in-memory list. They
    // work in any mode where
    // a row is selected (the
    // dialog pre-fills from
    // the row's `directory`).
    row(
        &mut lines,
        binding_for(Action::AddSession),
        "add the selected directory as a new named session",
    );
    row(
        &mut lines,
        binding_for(Action::AddHost),
        "add the selected directory as a new host (SSH connection)",
    );

    lines.push(Line::from(""));

    // ----- Panes filter -----
    //
    // The three `FilterPanes*`
    // actions toggle the `*`-mode
    // panes view between showing
    // all sections (live multiplexer
    // panes + `# sessions` + `# hosts`)
    // and showing only one section.
    // Pressing the active filter's
    // key again resets to All.
    row(
        &mut lines,
        binding_for(Action::FilterPanesWindows),
        "panes: show only live multiplexer windows / panes",
    );
    row(
        &mut lines,
        binding_for(Action::FilterPanesHosts),
        "panes: show only the `# hosts` block",
    );
    row(
        &mut lines,
        binding_for(Action::FilterPanesSessions),
        "panes: show only the `# sessions` block",
    );

    lines.push(Line::from(""));

    // ----- Cancel -----
    row(
        &mut lines,
        format!("{} (also closes overlays)", binding_for(Action::Cancel)),
        "cancel without selecting",
    );

    lines.push(Line::from(""));

    // ----- Search modes -----
    //
    // Lists every prefix-switchable mode and
    // its trigger character. The four
    // "F3-cycled" modes (plain / regex /
    // fuzzy / output) are also reachable
    // via `Action::ToggleSearchMode`, but
    // the remaining eight (LLM / question
    // / notes / todo / directories / panes
    // / JIRA / files) require the user to type the
    // prefix character directly. Listing
    // them all in the help is the only way
    // the user discovers the LLM, question,
    // panes, and JIRA modes exist at all.
    //
    // The prefix column shows the *user's
    // configured* prefix (from
    // `app.query_prefixes`), not the
    // default — the help reflects the
    // live config. The descriptions are
    // intentionally short (one line each)
    // so the full table fits in the
    // visible help area on an 80-col
    // terminal without scrolling.
    lines.push(Line::from(vec![Span::styled(
        "Search modes",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(
        "  Type a prefix to switch mode. The match algorithm (SUBSTR/",
    ));
    lines.push(Line::from(
        "  FUZZY/REGEX) applies to all modes except JIRA; cycle it with",
    ));
    lines.push(Line::from(format!(
        "  {} (the toggle-search-mode key).",
        format_key_specs(app.bindings.specs(Action::ToggleSearchMode)),
    )));
    lines.push(Line::from(
        "  Prefix characters are configurable in ~/.config/smarthistory/",
    ));
    lines.push(Line::from("  config (prefix.<name>=)."));
    lines.push(Line::from(""));

    // Helper: render one row of the
    // search-modes table. Three columns:
    // mode name (left, dim), prefix
    // (middle, warning — the colour is
    // the same as the markdown renderer's
    // inline-code style, so the prefix
    // reads as a "code token"), and a
    // short description (right, plain).
    //
    // The styles are constructed inline
    // via `Theme::dim()` / `Theme::warning()`
    // rather than the `dim` and `warning`
    // locals used by `row` above; the
    // nested `fn` items can't capture
    // local variables (a Rust closure
    // limitation), so the styles have to
    // be rebuilt at the call site.
    fn mode_row(
        lines: &mut Vec<Line<'static>>,
        name: &'static str,
        prefix: String,
        desc: &'static str,
    ) {
        let prefix_text = if prefix.is_empty() {
            "\u{2014}".to_string() // em-dash for "no prefix"
        } else {
            format!(" {}", prefix) // leading space for column padding
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<14}", name), Theme::dim()),
            Span::styled(
                format!("{:<7}", prefix_text),
                Style::default().fg(Theme::warning_color()),
            ),
            Span::raw(desc),
        ]));
    }

    let qp = &app.query_prefixes;
    mode_row(
        &mut lines,
        "history",
        String::new(),
        "search the shell history (match algorithm: SUBSTR → FUZZY → REGEX via C-f)",
    );
    mode_row(
        &mut lines,
        "output",
        qp.output.to_string(),
        "match against the captured output of each command (not the command itself)",
    );
    mode_row(
        &mut lines,
        "LLM command",
        qp.llm.to_string(),
        "send the body to ollama, generate a Bash command, stage it for execution",
    );
    mode_row(
        &mut lines,
        "question",
        qp.question.to_string(),
        "send the body to ollama, get a short answer (4 sentences max) in an overlay",
    );
    mode_row(
        &mut lines,
        "notes",
        qp.notes.to_string(),
        "search the note_search SQLite database (needs notes.database + notes.dir)",
    );
    mode_row(
        &mut lines,
        "todo",
        qp.todo.to_string(),
        "list open todos from the note_search database (selecting one opens $EDITOR at the line)",
    );
    mode_row(
        &mut lines,
        "directories",
        qp.directories.to_string(),
        "list every directory in the global history (sorted by most-recent activity)",
    );
    mode_row(
        &mut lines,
        "panes",
        qp.panes.to_string(),
        // The `*`-mode view lists
        // every pane across every
        // tmux session / herdr
        // workspace (selecting one
        // jumps to it; each pane
        // row carries a `[label]`
        // badge so the user can
        // tell which session /
        // workspace the pane
        // belongs to, and the
        // filter is "group-aware":
        // typing a token that
        // matches a workspace
        // label keeps the whole
        // workspace (header + all
        // child panes), and
        // typing a pane command
        // keeps that pane + its
        // parent workspace
        // header).
        "list every pane across all tmux sessions / herdr workspaces (organized as a per-session / per-workspace tree with the panes indented underneath; each pane row carries a [label] badge showing its session / workspace; the filter is group-aware: a match on the workspace label keeps the whole workspace, a match on a pane keeps the pane and its parent header)",
    );
    mode_row(
        &mut lines,
        "JIRA",
        qp.jira.to_string(),
        "search JIRA issues (needs JIRA_SERVER + JIRA_API_TOKEN env vars); Enter opens the issue in the browser, Ctrl-M-s downloads it as a local note via `note_search jira-issue <KEY>`",
    );
    mode_row(
        &mut lines,
        "files",
        qp.files.to_string(),
        "list every file in the current directory (selecting one opens it in $EDITOR)",
    );
    mode_row(
        &mut lines,
        "tags",
        qp.tags.to_string(),
        "list every symbol from the `tags` file (selecting one opens $EDITOR +LINE file); `@lang` filters by file extension and highlights the preview",
    );
    mode_row(
        &mut lines,
        "codegraph",
        qp.codegraph.to_string(),
        "search symbols in the local `.codegraph/codegraph.db` index (FTS5); the selected row's preview shows source context plus callers/callees; `@lang` filters by language; selecting one opens $EDITOR +LINE file; also the fallback for `tags` mode when no `TAGS` file exists",
    );
    mode_row(
        &mut lines,
        "ag",
        qp.ag.to_string(),
        "search file contents with ag (The Silver Searcher); `*` tokens restrict file patterns, `@lang` filters by language",
    );

    lines.push(Line::from(""));

    // ----- JIRA-mode tags -----
    //
    // A sub-section under "Search modes"
    // because the JIRA-mode tags only
    // work when the body starts with
    // the JIRA prefix (`-`). They
    // expand to JQL clauses server-side.
    // The reserved names (`me`, `today`,
    // `week`, `month`) are built-in
    // aliases; the `@<name>` pattern is
    // for user-defined fragments from
    // the `jira.search.<name>=<jql>`
    // config keys.
    lines.push(Line::from(vec![Span::styled(
        "JIRA-mode tags",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(
        "  Only meaningful when the body starts with the JIRA prefix above.",
    ));
    lines.push(Line::from(
        "  Each tag is a whole-word token (case-insensitive, `@` optional).",
    ));
    lines.push(Line::from(""));

    // Reuse the same 3-column layout as
    // the modes table: tag (left, warning
    // + bold so the `@name` reads as a
    // distinct token), JQL (middle, dim
    // — exact clause the tag expands to),
    // one-line description (right, plain).
    //
    // Style construction is inline
    // because of the same `fn`-item
    // capture limitation as
    // `mode_row` above.
    fn jira_tag_row(
        lines: &mut Vec<Line<'static>>,
        tag: &'static str,
        jql: &'static str,
        desc: &'static str,
    ) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  @{:<11}", tag),
                Style::default()
                    .fg(Theme::warning_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:<24}", jql), Theme::dim()),
            Span::raw(desc),
        ]));
    }

    jira_tag_row(
        &mut lines,
        "me",
        "assignee = currentUser()",
        "only issues assigned to the current user (per the API token)",
    );
    jira_tag_row(
        &mut lines,
        "today",
        "updated >= \"<today-1d>\"",
        "only issues updated in the last 24 hours (date is UTC)",
    );
    jira_tag_row(
        &mut lines,
        "week",
        "updated >= \"<today-7d>\"",
        "only issues updated in the last 7 days",
    );
    jira_tag_row(
        &mut lines,
        "month",
        "updated >= \"<today-31d>\"",
        "only issues updated in the last 31 days (one day longer than the notes-mode @month)",
    );
    jira_tag_row(
        &mut lines,
        "<name>",
        "(config-defined)",
        "a user-defined JQL fragment (jira.search.<name>=<jql> in the config file); reserved names me/today/week/month are dropped with a warning",
    );

    lines.push(Line::from(""));

    // ----- Tips -----
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
        // Pad the action label so the key column lines up.
        // Width 22 is enough for "Edit (cursor at start)"
        // plus a space; the key column is 24 so two-spec
        // bindings like "C-w, M-Backspace" fit without
        // overflowing into the category bracket.
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
                format!("{:>24}", key),
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

fn draw_prefix_picker(f: &mut Frame, app: &App, picker: &PrefixPicker) {
    use ratatui::widgets::List;

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    // The picker is a small
    // centred popup — the
    // list has only 12
    // entries so it doesn't
    // need to be huge.
    let area = centered_rect(60, 40, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        format!("{} close", cancel_keys)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(" Select mode  Enter apply / {} ", close_hint))
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Theme::dim_color());
    let accent_style = Theme::accent();

    // Scroll so the selected
    // row stays visible (in
    // the unlikely event the
    // terminal is so short
    // 12 rows don't fit).
    let visible_rows = inner.height as usize;
    let total = picker.options.len();
    let start = picker
        .selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(total.saturating_sub(visible_rows));
    let end = (start + visible_rows).min(total);

    let mut items: Vec<ListItem> = Vec::new();
    for (row_pos, opt) in picker
        .options
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        let is_selected = row_pos == picker.selected;
        let prefix_label = match opt.prefix {
            Some(c) => format!("  {} ", c),
            None => "    ".to_string(),
        };
        let spans = vec![
            Span::styled(
                if is_selected { " > " } else { "   " },
                if is_selected {
                    highlight_style
                } else {
                    dim_style
                },
            ),
            Span::styled(
                format!("{:>14}", opt.label),
                if is_selected {
                    highlight_style
                } else {
                    Style::default().fg(fg)
                },
            ),
            Span::styled(
                prefix_label,
                if is_selected {
                    highlight_style
                } else {
                    accent_style
                },
            ),
            Span::styled(
                format!("{}  ", opt.description),
                if is_selected {
                    highlight_style
                } else {
                    dim_style
                },
            ),
        ];
        // "(current)" marker when
        // the row matches the
        // query's actual leading
        // char (or lack thereof)
        // when the picker opened.
        // We don't track the
        // original theme here like
        // the theme picker does —
        // we just show where the
        // user was at open time by
        // pre-selecting that row.
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
    f.render_stateful_widget(list, inner, &mut list_state);
}

/// Overlay renderer for the CodeGraph callers/callees picker.
/// The picker is a centred popup list with two sections (callers,
/// then callees) separated by header rows; navigation skips the
/// headers (they're synthesized at render time, not entries).
fn draw_codegraph_relations_picker(
    f: &mut Frame,
    app: &App,
    picker: &CodeGraphRelationsPicker,
) {
    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    // Wider than the prefix picker so the qualified names and
    // `@file_path:line` suffix are readable.
    let area = centered_rect(80, 60, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let enter_keys = format_key_specs(app.bindings.specs(Action::Run));
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let enter_hint = if enter_keys.is_empty() {
        "Enter".to_string()
    } else {
        enter_keys
    };
    let close_hint = if cancel_keys.is_empty() {
        "Esc".to_string()
    } else {
        format!("{} close", cancel_keys)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(
            " Callers / callees of {}  {} open / {} ",
            picker.symbol, enter_hint, close_hint
        ))
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Theme::dim_color());
    let section_style = Theme::accent();
    let path_style = Style::default().fg(Theme::dim_color());

    // Build the display rows (section headers + entries) and
    // paginate around the selected entry so the cursor stays
    // visible. The visible-window math operates on the *entry*
    // positions because headers are not independently scrollable.
    let visible_rows = inner.height as usize;
    let n = picker.entries.len();
    let start_entry = picker
        .selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(n.saturating_sub(visible_rows));
    let end_entry = (start_entry + visible_rows).min(n);

    // We render line-by-line so section headers can be interleaved
    // without disturbing the entry-index ↔ selected mapping.
    let mut items: Vec<ListItem> = Vec::new();
    let mut last_section: Option<crate::tui::CodegraphRelationSection> = None;
    for (row_pos, entry) in picker
        .entries
        .iter()
        .enumerate()
        .skip(start_entry)
        .take(end_entry.saturating_sub(start_entry))
    {
        // Emit a section header whenever the section changes
        // (including the first entry).
        if last_section != Some(entry.section) {
            items.push(ListItem::new(Line::from(vec![Span::styled(
                format!(" {} ", entry.section.header()),
                section_style,
            )])));
            last_section = Some(entry.section);
        }
        let is_selected = row_pos == picker.selected;
        let cursor = if is_selected { " > " } else { "   " };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(cursor, if is_selected { highlight_style } else { dim_style }),
            Span::styled(
                format!("{} ", entry.node.qualified_name),
                if is_selected {
                    highlight_style
                } else {
                    Style::default().fg(fg)
                },
            ),
            Span::styled(
                format!("@{}:{} ", entry.node.file_path, entry.node.start_line),
                if is_selected { highlight_style } else { path_style },
            ),
        ])));
    }
    let list = List::new(items)
        .style(Style::default().bg(bg))
        .highlight_style(highlight_style)
        .highlight_symbol("")
        .repeat_highlight_symbol(false);
    f.render_widget(list, inner);
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

/// Render the tab-completion
/// menu. The menu is a small
/// centred popup that shows the
/// list of candidates when the
/// user presses `Tab` and the
/// completion is ambiguous. The
/// user navigates with `Up`/
/// `Down` and commits with
/// `Enter`; the title always
/// shows the `Cancel` binding
/// for dismissal.
fn draw_completion_menu(f: &mut Frame, app: &App, menu: &super::CompletionMenu) {
    use ratatui::widgets::List;

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    // The menu is a small
    // centred popup. The list
    // is short (typically 2-10
    // candidates) so a 50%
    // × 40% popup is plenty.
    let outer = centered_rect(50, 40, f.area());
    f.render_widget(ratatui::widgets::Clear, outer);

    // The title shows the
    // `Cancel` binding(s) so
    // the user always knows
    // how to dismiss the
    // menu.
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        format!("{} close", cancel_keys)
    };
    // The "kind" label
    // describes what kind of
    // completion the menu
    // is showing. The raw
    // kind enum isn't
    // user-facing, so we
    // map it to a label.
    let kind_label = match menu.kind {
        super::CompletionKind::JiraField => "JIRA field",
        super::CompletionKind::JiraAlias => "JIRA alias",
        super::CompletionKind::NotesTag => "tag",
        super::CompletionKind::NotesLink => "link",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(format!(
            " {} candidates  Enter apply / {} ",
            kind_label, close_hint
        ))
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    // Reserve the last line
    // for a footer hint so
    // the user sees the
    // navigation keys
    // (Up/Down + Enter).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(1)].as_ref())
        .split(inner);

    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Theme::dim_color());

    // Scroll so the
    // selected row stays
    // visible (in the
    // unlikely event the
    // terminal is so short
    // the list doesn't
    // fit).
    let visible_rows = chunks[0].height as usize;
    let total = menu.candidates.len();
    let start = menu
        .selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(total.saturating_sub(visible_rows));
    let end = (start + visible_rows).min(total);

    let mut items: Vec<ListItem> = Vec::new();
    for (row_pos, candidate) in menu
        .candidates
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        let is_selected = row_pos == menu.selected;
        let mut spans = vec![Span::styled(
            if is_selected { " > " } else { "   " },
            if is_selected {
                highlight_style
            } else {
                dim_style
            },
        )];
        spans.push(Span::styled(
            candidate.as_str(),
            if is_selected {
                highlight_style
            } else {
                Style::default().fg(fg)
            },
        ));
        items.push(ListItem::new(Line::from(spans)));
    }

    let list = List::new(items)
        .style(Style::default().bg(bg))
        .highlight_style(highlight_style)
        .highlight_symbol("")
        .repeat_highlight_symbol(false);
    let mut list_state = ListState::default();
    if end > start {
        list_state.select(Some(menu.selected.saturating_sub(start)));
    }
    f.render_stateful_widget(list, chunks[0], &mut list_state);

    // Footer with
    // navigation hint.
    let footer = Line::from(vec![
        Span::styled(format!(" {}/{} ", menu.selected + 1, total), dim_style),
        Span::styled("  up/down move  Enter apply  ", dim_style),
    ]);
    let footer_para = Paragraph::new(footer).style(Style::default().bg(bg));
    f.render_widget(footer_para, chunks[1]);
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
    // Directory-source chip:
    // shown only in
    // directories mode and
    // only when the
    // source is not the
    // default (`All`). The
    // user's current
    // `ALL` / `TMUX` /
    // `CFG` choice is the
    // load-bearing
    // information here, so
    // it's worth a chip
    // when the user has
    // chosen a non-default
    // source.
    let dirsrc_chip = if app.is_directories_query()
        && app.directory_source != crate::tui::state::DirectorySource::All
    {
        Some(directory_source_badge(
            app.directory_source,
            app.multiplexer.name(),
        ))
    } else {
        None
    };
    // Panes-filter chip:
    // shown only in panes
    // mode (`*`) and only
    // when the filter is
    // not the default
    // (`All`). The user's
    // current filter
    // (Windows / Hosts /
    // Sessions) is the
    // load-bearing
    // information here, so
    // it's worth a chip
    // when the user has
    // chosen a non-default
    // filter.
    let panes_filter_chip = if app.is_panes_query() && !app.panes_filter.is_default() {
        Some(panes_filter_badge(app.panes_filter))
    } else {
        None
    };
    // Ag-mode chip: shown only in ag mode.
    let ag_chip = if app.is_ag_query() {
        Some(Span::styled(
            " AG ",
            Style::default()
                .fg(Theme::badge_fg_color())
                .bg(Theme::warning_color())
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        None
    };
    // Pane-visibility chip: shown only when the layout is
    // not the default (`Both`). Lets the user know at a
    // glance that one of the detail panes is hidden.
    let pane_vis_chip = if app.pane_visibility != crate::tui::state::PaneVisibility::Both {
        let label = app.pane_visibility.label();
        Some(Span::styled(
            format!(" {} ", label.to_ascii_uppercase()),
            Style::default()
                .fg(Theme::badge_fg_color())
                .bg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        ))
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
    if let Some(chip) = dirsrc_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    if let Some(chip) = panes_filter_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    if let Some(chip) = ag_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    if let Some(chip) = pane_vis_chip {
        spans.push(Span::styled("  ", Theme::default()));
        spans.push(chip);
    }
    // Match-algorithm chip. Shown only when the
    // algorithm is NOT the default Substring.
    // Reminds the user which algorithm
    // (FUZZY / REGEX) is currently applied to
    // their search.
    if let Some(chip) = match_algorithm_badge(app.match_algorithm) {
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

/// Match-algorithm chip. Shown whenever the
/// algorithm is not the default Substring.
/// `SUB` (default — hidden), `FUZZY` (green),
/// `REGEX` (yellow). The chip reminds the user
/// which algorithm is active so they don't
/// forget they cycled to regex and are now
/// confused why their plain text is treated
/// as a regex pattern.
fn match_algorithm_badge(algo: crate::tui::state::MatchAlgorithm) -> Option<Span<'static>> {
    if algo == crate::tui::state::MatchAlgorithm::Substring {
        return None;
    }
    let (label, color) = match algo {
        crate::tui::state::MatchAlgorithm::Substring => return None,
        crate::tui::state::MatchAlgorithm::Fuzzy => ("FUZZY", Theme::success_color()),
        crate::tui::state::MatchAlgorithm::Regex => ("REGEX", Theme::warning_color()),
    };
    Some(Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(color)
            .add_modifier(Modifier::BOLD),
    ))
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

fn directory_source_badge(
    source: crate::tui::state::DirectorySource,
    backend_name: &'static str,
) -> Span<'static> {
    // The `Tmux` source
    // variant in
    // `DirectorySource` is
    // the
    // "active-context"
    // filter — it shows
    // rows whose directory
    // matches an active
    // context. The actual
    // multiplexer
    // (tmux or herdr) is
    // reported by the
    // backend; the chip
    // reads the backend's
    // name (e.g.
    // `DIR:HERDR` when
    // the user has
    // `multiplexer=herdr` in
    // their config) so the
    // user knows *which*
    // backend is producing
    // the marker, not the
    // (stale) source
    // enum. The `All` and
    // `Config` sources
    // don't depend on the
    // backend (they show
    // every row, or only
    // the `sessiondirs=...`
    // rows), so they keep
    // their enum-derived
    // labels.
    let label: &'static str = match source {
        crate::tui::state::DirectorySource::All => "ALL",
        crate::tui::state::DirectorySource::Tmux => {
            // `backend_name` is
            // `&'static str`
            // (the
            // `MultiplexerBackend::name`
            // contract
            // guarantees a
            // string
            // literal),
            // so this
            // leak is
            // safe.
            match backend_name {
                "herdr" => "HERDR",
                // Fall
                // back
                // to
                // the
                // source
                // enum's
                // own
                // label
                // for
                // any
                // other
                // backend
                // (today:
                // "tmux").
                _ => source.label(),
            }
        }
        crate::tui::state::DirectorySource::Config => "CFG",
    };
    Span::styled(
        format!(" DIR:{} ", label),
        Style::default()
            .fg(Theme::badge_fg_color())
            .bg(Theme::highlight_color())
            .add_modifier(Modifier::BOLD),
    )
}

fn panes_filter_badge(filter: crate::tui::state::PanesFilter) -> Span<'static> {
    // The panes-filter chip
    // uses the warning color
    // (`yellow` by default)
    // so it stands out from
    // the accent-colored DIR
    // chip and the
    // success-colored SESS
    // mode badge. The label
    // is the filter's
    // `label()` ("PANES" /
    // "HOSTS" / "SESSIONS").
    let label = filter.label();
    Span::styled(
        format!(" *:{} ", label),
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
    //
    // **Panes mode (`*`) is different.** The rows are produced by
    // `fetch_session_panes_impl` as a tree: `workspace` header row,
    // then its `pane` child rows, then the next workspace header,
    // etc. The data order IS the display order — the tree must read
    // top-to-bottom (header, then the panes it owns). Reversing it
    // would put a workspace's pane rows ABOVE that workspace's header,
    // which destroys the visual grouping. So panes mode skips the
    // `.rev()` and treats the rows as already display-ordered.
    let is_panes = app.is_panes_query();
    let real_items: Vec<ListItem> = if is_panes {
        merged
            .iter()
            .enumerate()
            .map(|(data_idx, r)| {
                let is_selected = app.list_state.selected() == Some(data_idx);
                ListItem::new(render_row(r, app, is_selected, age_width))
            })
            .collect()
    } else {
        merged
            .iter()
            .enumerate()
            .rev()
            .map(|(data_idx, r)| {
                let is_selected = app.list_state.selected() == Some(data_idx);
                ListItem::new(render_row(r, app, is_selected, age_width))
            })
            .collect()
    };

    // Bottom-align: when there are fewer real rows than the visible
    // height, pad the top with empty items so the real rows sit at
    // the bottom of the widget. `area.height` includes the top and
    // bottom borders; subtract 2 for the content area.
    //
    // **Panes mode**: the tree reads top-to-bottom, so we DON'T pad
    // the top — the rows sit at the top of the widget instead. The
    // behavior matches the user's mental model of a tree view
    // (header at the top, indentation underneath).
    let visible_height = area.height.saturating_sub(2) as usize;
    let real_count = real_items.len();
    let pad = if is_panes {
        0
    } else {
        visible_height.saturating_sub(real_count)
    };

    let mut items: Vec<ListItem> = if is_panes {
        Vec::with_capacity(real_count)
    } else {
        (0..pad).map(|_| ListItem::new("")).collect()
    };
    items.extend(real_items);

    // The stored selection is in data coordinates (0 = newest).
    // Map it to the rendered list coordinates where the newest item
    // is the last real item.
    //
    // **Panes mode**: data index IS the rendered index (0 = first
    // row at the top). No flip.
    let rendered_idx = if is_panes {
        app.list_state.selected().map(|data_idx| data_idx)
    } else {
        app.list_state
            .selected()
            .map(|data_idx| pad + (real_count.saturating_sub(1) - data_idx))
    };

    // Always start the list from the bottom of the visible window.
    // When the list fits within the visible height we pad with empty
    // items above; when it is taller, we anchor the offset so the
    // last entry sits at the bottom and the user scrolls upward to
    // see older entries.
    //
    // **Panes mode**: anchor at the TOP — offset = 0 — so the first
    // workspace header is the first visible row and the user can
    // scroll DOWN to see more panes. The bottom-anchor logic for
    // the reverse-sorted history list doesn't apply here.
    let offset = if is_panes {
        0
    } else if real_count >= visible_height {
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
            // Make the selected row stand out more
            // than the original muted gray did, but
            // lighter than a full-on accent-color
            // background (which the user found too
            // bright). The balance: the
            // accent-tinted `selection_color`
            // palette slot for the row background
            // (it's still theme-following and
            // noticeably color-tinted, but
            // it's the palette's dedicated
            // "selection" color so it sits
            // lighter on the eye than the
            // saturated accent color). The
            // FOREGROUND flips to the
            // highlight color slot so the
            // command text reads in a brighter
            // shade than the surrounding rows
            // (without going all the way to
            // invert/contrast against an
            // accent background). Bold +
            // UNDERLINED text modifiers for
            // the rest of the visual weight —
            // those plus thevivid
            // `▌` left-edge bar in the accent
            // color (see `highlight_symbol`
            // below) carry the selection
            // obviousness without the full row
            // being a colored slab.
            Style::default()
                .bg(Theme::selection_color())
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
        )
        // A solid left-half block character
        // (▌) repeated across the symbol
        // area gives a thick colored bar
        // running the height of the
        // selected row — like a VSCode
        // selection marker. Pairs with
        // the accent-background highlight
        // style above to make the row
        // unmissable.
        .highlight_symbol("▌")
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

    // The `*`-mode list now has a
    // **tree** layout:
    //   workspace_header
    //     · pane_row
    //     · pane_row
    //   workspace_header
    //     · pane_row
    // For `pane` rows,
    // prepend an indent + a
    // tree connector (`  · `)
    // so the pane is visually
    // grouped under its
    // workspace header above.
    // The old `[label]` badge
    // that identified the
    // workspace per-row is no
    // longer needed — the
    // workspace header row
    // above already provides
    // that context, and the
    // indent makes the
    // grouping clear.
    //
    // For `workspace` rows,
    // prepend a bold
    // `# ` marker (same
    // convention as the
    // directories-mode
    // header rows) so the
    // user can tell at a
    // glance that this is a
    // workspace-level row,
    // not a pane. Selecting
    // The `*`-mode tree groups
    // rows visually. Every row
    // gets a tree-position
    // marker (so the connector
    // is consistent for both
    // unfiltered and filtered
    // views), then the row's
    // primary content. The
    // markers are:
    //   - `workspace` rows: a
    //     bold `# ` accent prefix
    //     identifying the
    //     workspace as the
    //     group header.
    //   - `pane` / `session` /
    //     `host` rows: `  · ` to
    //     indent them under
    //     their parent.
    if row.mode == "pane" || row.mode == "session" || row.mode == "host" {
        // `pane`, `session`, and `host` rows are
        // all children of a `workspace` header row
        // in the `*`-mode tree. Indent them with
        // the same `  · ` tree connector so they're
        // visually grouped under their header.
        // Without this, `# sessions` and `# hosts`
        // header rows would have their child rows
        // flush with the left margin, looking like
        // flat history rows rather than tree
        // children.
        spans.push(Span::raw("  · "));
    } else if row.mode == "workspace" {
        spans.push(Span::styled(
            "# ",
            Style::default()
                .fg(Theme::accent_color())
                .add_modifier(Modifier::BOLD),
        ));
    }

    // For `pane` rows in the
    // `*`-mode tree, show the
    // parent workspace /
    // session name as a chip
    // after the tree connector
    // and BEFORE the row's
    // command text. This is
    // what the user asked for:
    // "the workspace (herdr) or
    // session name (tmux) should
    // be added to the panes as
    // well". The badge is
    // important when:
    //   - the user filters the
    //     list down to a single
    //     workspace (the
    //     header is still
    //     visible, but the
    //     `· ` indent alone
    //     doesn't say which
    //     workspace it belongs
    //     to);
    //   - the user types a token
    //     that matches a pane
    //     command — the
    //     group-aware filter
    //     keeps the parent
    //     header, but having
    //     the label visible on
    //     every pane row
    //     makes scanning a
    //     long list easier.
    // The chip uses the
    // `info` slot's colour (the
    // same blue the `+`-output
    // mode uses) so it's
    // visually distinct from
    // the row's command /
    // cwd content.
    if row.mode == "pane" && !row.workspace_label.is_empty() {
        spans.push(Span::styled(
            format!("[{}] ", row.workspace_label),
            Style::default()
                .fg(Theme::badge_fg_color())
                .bg(Theme::info_color())
                .add_modifier(Modifier::BOLD),
        ));
    }

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
    // Multiline commands (containing real newlines) would break the
    // single-line row layout. Replace each newline with the visible
    // separator `↵` so the row stays on one line while still showing
    // where the line breaks are. The full command (with real
    // newlines) is available in the details pane.
    let cmd_display: String = row.command.replace('\n', "↵").replace('\r', "");
    // For `workspace` rows in
    // the `*`-mode tree,
    // render the label (a
    // workspace id like `wA`
    // or a tmux session name)
    // bold + accent so it
    // visually stands out as a
    // header above its pane
    // children. Other rows use
    // the normal highlight path.
    if row.mode == "workspace" {
        spans.push(Span::styled(
            format!("{} ", cmd_display),
            Style::default()
                .fg(Theme::accent_color())
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.is_regex_query() {
        spans.extend(highlight_regex_matches(
            &cmd_display,
            app.query_regex.as_ref(),
        ));
    } else {
        spans.extend(highlight_matches(&cmd_display, &app.query));
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
fn highlight_regex_matches(text: &str, regex: Option<&Regex>) -> Vec<Span<'static>> {
    let Some(re) = regex else {
        return vec![Span::raw(text.to_string())];
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
        spans.push(Span::raw(text.to_string()));
    }
    spans
}

/// Return a sequence of spans that wrap every occurrence of `query`
pub(super) fn highlight_matches(text: &str, query: &str) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::raw(text.to_string())];
    }

    let words: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if words.is_empty() {
        return vec![Span::raw(text.to_string())];
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
    // fixed 6-row layout. We join all
    // lines with `↵` so multiline
    // commands are visible in full (the
    // separator marks where each physical
    // line break was), and if that exceeds
    // the available column width we
    // ellipsize it so the layout still
    // holds. The full text remains
    // available in the Output Preview pane
    // below, where the user can scroll if
    // they need the rest.
    let cmd_single_line = row.command.replace('\n', "↵").replace('\r', "");
    let cmd_visible = truncate_cmd_for_details_pane(&cmd_single_line, area.width as usize);

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

/// Render a single preview line as styled
/// spans. Supports a small subset of
/// Markdown:
///
/// **Block-level** (detected at line start):
///
/// | Marker          | Element       | Style                              |
/// |-----------------|---------------|------------------------------------|
/// | `# text`        | H1 heading    | bold + `success()`                 |
/// | `## text`       | H2 heading    | bold + `accent()`                  |
/// | `### text`      | H3 heading    | bold + `dim()` + 2-space indent    |
/// | `> text`        | Blockquote    | italic + `info()` `│ ` gutter     |
/// | `- text`        | Bullet list   | `accent()` `• ` marker, plain text |
/// | `* text`        | Bullet list   | same as `-`                        |
/// | `1. text`       | Ordered list  | `accent()` `N. ` marker            |
/// | `---`           | Horizontal    | `dim()` full-width `─` rule         |
/// | (anything else) | Plain text    | inline parser                      |
///
/// **Inline** (within a plain-text line):
///
/// | Marker            | Style                                   |
/// |-------------------|-----------------------------------------|
/// | `**bold**`         | `Modifier::BOLD`                        |
/// | `*italic*`         | `Modifier::ITALIC`                      |
/// | `_italic_`         | `Modifier::ITALIC` (alias for `*`)     |
/// | `` `code` ``       | `warning()` + `Modifier::BOLD`         |
/// | `~~strike~~`       | `Modifier::CROSSED_OUT`                 |
/// | `[text](url)`      | `accent()` + `Modifier::UNDERLINED`     |
///
/// The block-level detection runs *first* and
/// short-circuits the inline parser — a heading
/// line is the whole content of the line, and
/// any `**...**` inside it would be part of
/// the heading text (a future feature, not used
/// today). This avoids the ambiguity of "is
/// this a bold span inside a heading, or a
/// heading marker followed by text" without
/// needing an escape mechanism.
///
/// **Composition**: inline markers compose.
/// `**bold *italic***` produces a bold span
/// containing an italic span. The parser is
/// left-to-right and finds the earliest
/// applicable marker; nested markers (an
/// italic span inside a bold span) work
/// naturally because the inline parser is
/// recursive.
///
/// **Unclosed markers** fall through to plain
/// text so a stray literal `**` in any future
/// mode's output doesn't corrupt the
/// rendering — the user sees the literal
/// characters instead of a missing closing
/// marker eating the rest of the line.
///
/// **Empty lines** yield a single empty plain
/// span so the resulting `Line` is never empty
/// (ratatui collapses empty lines in some
/// configurations).
///
/// **Adjacent plain segments** are merged so
/// the output doesn't have a sequence of
/// single-character spans (matters for
/// ratatui's layout pass on long lines).
fn render_preview_line(line: &str) -> Line<'static> {
    // Block-level detection first. A line
    // that starts with `# ` / `## ` / `### `
    // (heading), `> ` (blockquote), `- ` or
    // `* ` (bullet), `N. ` (ordered), or
    // matches the horizontal-rule pattern
    // short-circuits the inline parser.
    match parse_block(line) {
        MdBlock::Plain(_text) => {
            // No block-level marker. Run the
            // inline parser on the original
            // line so we preserve the
            // user's exact whitespace
            // (block parsers strip leading
            // whitespace before matching
            // the marker, so we can't
            // pass `text` here — we
            // need the full line).
            let base = Theme::default();
            let spans = render_inline(line, base);
            let spans = if spans.is_empty() {
                vec![Span::styled(String::new(), base)]
            } else {
                spans
            };
            Line::from(spans)
        }
        block => render_block(block),
    }
}

/// A block-level element detected at the
/// start of a line. Each variant carries
/// the *content* of the element (the text
/// after the marker). The renderer
/// (`render_block`) decides the visual
/// style.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MdBlock {
    /// `# text` — top-level heading.
    Heading1(String),
    /// `## text` — mid-level heading.
    /// The most common in the JIRA
    /// overlay (the section names and
    /// per-comment sub-headings).
    Heading2(String),
    /// `### text` — sub-heading.
    Heading3(String),
    /// `> text` — blockquote. The text is
    /// rendered in italic with a `│ `
    /// gutter in `info()` color.
    Blockquote(String),
    /// `- text` or `* text` — bullet
    /// list item. Rendered with a `• `
    /// marker in `accent()` color.
    Bullet(String),
    /// `1. text`, `2. text`, etc. —
    /// ordered list item. The first
    /// number is preserved (the parser
    /// doesn't auto-number across lines;
    /// each line is independent). The
    /// marker is `N. ` in `accent()`
    /// color.
    Ordered(u32, String),
    /// A line of only `---` (3+ dashes),
    /// `***` (3+ asterisks), or `___` (3+
    /// underscores) — horizontal rule.
    /// Rendered as a full-width `─` line
    /// in `dim()` color.
    HorizontalRule,
    /// Any line that doesn't match a
    /// block-level marker. The content
    /// is the original line (the inline
    /// parser runs on the full line, not
    /// on a stripped form, to preserve
    /// any leading whitespace the user
    /// intended).
    Plain(String),
}

/// Detect the block-level element at the
/// start of `line`. Leading whitespace is
/// tolerated (a `# heading` is treated the
/// same as `   # heading`); the heading
/// marker must be the *first non-space*
/// character(s) on the line. A line that
/// starts with `#tag` (no space) is plain
/// text, not a heading.
fn parse_block(line: &str) -> MdBlock {
    let trimmed = line.trim_start();
    // Headings: 1-3 `#` chars followed by
    // a space. 4+ `#`s is plain text
    // (CommonMark: max 3 levels; anything
    // beyond is treated as text).
    if let Some(rest) = stripped_heading(trimmed, 1) {
        return MdBlock::Heading1(rest.to_string());
    }
    if let Some(rest) = stripped_heading(trimmed, 2) {
        return MdBlock::Heading2(rest.to_string());
    }
    if let Some(rest) = stripped_heading(trimmed, 3) {
        return MdBlock::Heading3(rest.to_string());
    }
    // Horizontal rule: a line consisting
    // only of `---` (3+ dashes), `***` (3+
    // asterisks), or `___` (3+ underscores),
    // optionally with leading / trailing
    // whitespace and spaces between the
    // characters. The string must be at
    // least 3 characters of the same
    // marker.
    if is_horizontal_rule(trimmed) {
        return MdBlock::HorizontalRule;
    }
    // Blockquote: `> ` prefix.
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return MdBlock::Blockquote(rest.to_string());
    }
    // Bullet list: `- ` or `* ` prefix.
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return MdBlock::Bullet(rest.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return MdBlock::Bullet(rest.to_string());
    }
    // Ordered list: `<digits>. ` prefix.
    if let Some((n, rest)) = parse_ordered_prefix(trimmed) {
        return MdBlock::Ordered(n, rest.to_string());
    }
    MdBlock::Plain(line.to_string())
}

/// Helper: detect a heading with `level`
/// `#` chars followed by a space. Returns
/// the text after the marker (with the
/// leading space stripped). Returns
/// `None` for `#tag` (no space) or
/// `##` alone (marker without text — that's
/// a horizontal-rule-like pattern but
/// CommonMark requires at least one
/// non-space character after the marker).
fn stripped_heading(s: &str, level: usize) -> Option<&str> {
    let prefix: String = std::iter::repeat_n('#', level).collect();
    let after = s.strip_prefix(&prefix)?;
    // Must be followed by a space AND
    // have at least one non-space
    // character after the space.
    // `##` (no text) is plain text.
    let after_space = after.strip_prefix(' ')?;
    if after_space.is_empty() {
        return None;
    }
    Some(after_space)
}

/// True if `s` is a horizontal rule: 3+ of
/// the same marker character (`-`, `*`, or
/// `_`), optionally with leading / trailing
/// whitespace and internal spaces. A line
/// that mixes markers (e.g. `-*-`) is not
/// a rule.
fn is_horizontal_rule(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 {
        return false;
    }
    // `s.chars().next()` on a non-empty
    // string is always `Some`; we use
    // `if let` rather than `?` because
    // the function returns `bool`, not
    // `Option`.
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    // Every character is either the marker
    // or a space.
    s.chars().all(|c| c == first || c.is_whitespace())
}

/// Parse an ordered-list prefix: 1-9
/// digits followed by `. ` and the rest.
/// Returns `Some((number, rest))` on
/// success.
fn parse_ordered_prefix(s: &str) -> Option<(u32, &str)> {
    let bytes = s.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() && idx < 9 {
        idx += 1;
    }
    if idx == 0 {
        return None;
    }
    let after_digits = &s[idx..];
    let after_dot = after_digits.strip_prefix(". ")?;
    let n: u32 = s[..idx].parse().ok()?;
    Some((n, after_dot))
}

/// Render a `Block` as a styled `Line`.
/// This is the visual half of the parser:
/// each block variant has a specific style
/// (heading level, list marker, blockquote
/// gutter, etc.) that's applied here.
fn render_block(block: MdBlock) -> Line<'static> {
    let base = Theme::default();
    match block {
        MdBlock::Heading1(text) => {
            // H1: a `▸ ` glyph in the
            // success color, then the
            // heading text in the same
            // color + bold. The glyph
            // gives H1 a distinct
            // visual anchor that H2
            // (the existing `## ` style)
            // lacks.
            let marker = Span::styled("▸ ", Theme::success());
            let text = Span::styled(text, Theme::success().add_modifier(Modifier::BOLD));
            Line::from(vec![marker, text])
        }
        MdBlock::Heading2(text) => {
            // H2: bold + accent color.
            // The most common style
            // in the JIRA overlay
            // (the section names and
            // per-comment sub-headings).
            let text = Span::styled(text, Theme::accent().add_modifier(Modifier::BOLD));
            Line::from(text)
        }
        MdBlock::Heading3(text) => {
            // H3: 2-space indent (to
            // suggest a sub-level
            // below the section
            // headings) + bold + dim
            // color. Subdued so it
            // doesn't compete with
            // H1 / H2.
            let indent = Span::raw("  ");
            let text = Span::styled(text, Theme::dim().add_modifier(Modifier::BOLD));
            Line::from(vec![indent, text])
        }
        MdBlock::Blockquote(text) => {
            // Blockquote: a `│ ` gutter
            // in the info color, then
            // the content in italic.
            // The italic modifier is
            // applied to the content
            // spans via `render_inline`,
            // which gets a `base` Style
            // pre-decorated with
            // ITALIC.
            let marker = Span::styled("│ ", Theme::info());
            let italic_base = base.add_modifier(Modifier::ITALIC);
            let content = render_inline(&text, italic_base);
            let mut spans = vec![marker];
            spans.extend(content);
            Line::from(spans)
        }
        MdBlock::Bullet(text) => {
            // Bullet list: a `• `
            // marker in the accent
            // color, then the
            // content in the default
            // style. The inline parser
            // runs on the content so
            // `**bold**` inside a
            // bullet item still
            // produces a bold span.
            let marker = Span::styled("• ", Theme::accent());
            let content = render_inline(&text, base);
            let mut spans = vec![marker];
            spans.extend(content);
            Line::from(spans)
        }
        MdBlock::Ordered(n, text) => {
            // Ordered list: a `N. `
            // marker in the accent
            // color (where N is the
            // number from the source
            // line), then the
            // content. We don't
            // auto-number across
            // lines because the
            // parser is line-by-line;
            // the user is responsible
            // for writing the
            // numbers in their
            // content.
            let marker = Span::styled(format!("{}. ", n), Theme::accent());
            let content = render_inline(&text, base);
            let mut spans = vec![marker];
            spans.extend(content);
            Line::from(spans)
        }
        MdBlock::HorizontalRule => {
            // A horizontal rule is a
            // full-width line of `─`
            // characters in the dim
            // color. We emit a fixed
            // 40-character string; the
            // `Paragraph` widget's
            // wrap setting
            // (`Wrap { trim: false }`)
            // leaves the trailing
            // whitespace intact so
            // the line stays the same
            // length regardless of
            // terminal width. A
            // wider terminal shows
            // the rule as
            // 40 characters long; a
            // narrower one truncates
            // (the user can scroll
            // horizontally if the
            // widget supports it).
            // A future improvement
            // could compute the rule's
            // width from the area at
            // render time, but the
            // current shape is
            // sufficient.
            Line::from(Span::styled("─".repeat(40), Theme::dim()))
        }
        MdBlock::Plain(text) => {
            // Unreachable in
            // practice: `render_preview_line`
            // only calls
            // `render_block` for
            // non-`Plain` variants.
            // Kept for completeness.
            let spans = render_inline(&text, base);
            Line::from(spans)
        }
    }
}

/// Render an inline span of text. Walks
/// `text` left-to-right, finding the
/// earliest inline marker and emitting
/// plain text + a styled span. The inline
/// markers recognised:
///
/// - `**bold**` — bold
/// - `*italic*` — italic
/// - `_italic_` — italic (alias for `*`)
/// - `` `code` `` — code (warning color + bold)
/// - `~~strike~~` — strikethrough
/// - `[text](url)` — link (accent color + underline)
///
/// **Priority**: when multiple markers
/// could match at the same position, the
/// longer one wins (`**` before `*`, `~~`
/// before `~`). The parser checks the
/// specific double-char markers first
/// and the single-char ones after.
///
/// **Composition**: a bold span can
/// contain an italic span (e.g.
/// `**bold *italic***`). The parser is
/// recursive: the *content* between a
/// pair of markers is run through
/// `render_inline` again, so nested
/// markers are styled correctly. This
/// means `**bold *italic***` produces:
///
/// ```text
/// [bold [
///   "bold "
///   italic[ "italic" ]
/// ]]
/// ```
///
/// which ratatui renders as bold "bold "
/// followed by bold-italic "italic".
///
/// **Unclosed markers** fall through to
/// plain text (the rest of the line,
/// including the literal marker
/// characters, is rendered without
/// styling). The user sees the literal
/// `**` rather than a missing closing
/// marker eating the rest of the line.
fn render_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        // Find the earliest
        // applicable marker. We
        // check the specific
        // double-char markers
        // (`**`, `~~`) before the
        // single-char ones
        // (`*`, `_`, `` ` ``,
        // `[`) so `**` is
        // recognised as bold
        // rather than two
        // consecutive italic
        // openers.
        //
        // The return value
        // includes the literal
        // character that opened the
        // marker (for italic, this
        // distinguishes `*` from
        // `_` so the close uses
        // the same character).
        let next = find_next_marker(rest);
        let Some((idx, marker_kind, marker_len, marker_char)) = next else {
            // No more markers in
            // the line. Push the
            // remaining text as
            // a plain span and
            // stop.
            if !rest.is_empty() {
                push_plain_span(&mut spans, rest.to_string(), base);
            }
            break;
        };
        // Plain text before the
        // marker.
        if idx > 0 {
            push_plain_span(&mut spans, rest[..idx].to_string(), base);
        }
        let after_open = &rest[idx + marker_len..];
        // Try to find the matching
        // close marker. The close
        // marker is the same as
        // the open marker (e.g.
        // `**...**`). For `*`
        // and `_` italic, the
        // close marker is the
        // same single char.
        // For links, the close
        // marker is `](...)`
        // which is structurally
        // different from the
        // open `[`.
        let close = find_close_marker(after_open, marker_kind, marker_char);
        match close {
            Some((close_idx, close_len, _kind)) => {
                let content = &after_open[..close_idx];
                if !content.is_empty() {
                    let style = style_for_marker(marker_kind, base);
                    // The content
                    // itself is
                    // recursively
                    // parsed
                    // (so
                    // `**bold *italic***`
                    // works).
                    let inner = render_inline(content, style);
                    spans.extend(inner);
                }
                rest = &after_open[close_idx + close_len..];
            }
            None => {
                // Unclosed
                // marker.
                // Render the
                // rest of
                // the line
                // (including
                // the
                // literal
                // marker)
                // as plain
                // text.
                push_plain_span(
                    &mut spans,
                    format!("{}{}", marker_str(marker_kind), after_open),
                    base,
                );
                rest = "";
            }
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

/// Marker kinds recognised by the inline
/// parser. Used as a typed enum to avoid
/// the magic-string / magic-int code
/// paths the previous stringly-typed
/// implementation had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    /// `**` — bold.
    Bold,
    /// `*` or `_` — italic.
    Italic,
    /// `` ` `` — inline code.
    Code,
    /// `~~` — strikethrough.
    Strikethrough,
    /// `[` — link (close is `](...)`).
    Link,
}

/// The literal characters that open a
/// given marker kind. Used when an
/// unclosed marker falls through to
/// plain text (we re-attach the literal
/// characters so the user sees them).
fn marker_str(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Bold => "**",
        MarkerKind::Italic => "*",
        MarkerKind::Code => "`",
        MarkerKind::Strikethrough => "~~",
        MarkerKind::Link => "[",
    }
}

/// Find the earliest inline marker in
/// `s`. Returns `(byte_offset, kind, length, open_char)`
/// of the marker, or `None` if no marker
/// is present. The byte offset is the
/// position of the first character of
/// the marker (not the start of the
/// content). The `open_char` is the
/// literal character that opened the
/// marker — `*` for italic, `_` for
/// italic, `` ` `` for code, `[` for
/// link; the first two chars of the
/// double-char markers (`*` for bold,
/// `~` for strikethrough).
///
/// Priority: `**` and `~~` (double-char
/// markers) are checked before the
/// single-char ones (`*`, `_`, `` ` ``,
/// `[`). This means `**bold**` is
/// recognised as bold, not as two
/// consecutive italic openers.
fn find_next_marker(s: &str) -> Option<(usize, MarkerKind, usize, char)> {
    // Check double-char markers first.
    if let Some(idx) = s.find("**") {
        return Some((idx, MarkerKind::Bold, 2, '*'));
    }
    if let Some(idx) = s.find("~~") {
        return Some((idx, MarkerKind::Strikethrough, 2, '~'));
    }
    // Single-char markers. We use
    // `bytes().position()` to find the
    // first occurrence of each
    // character, then return the
    // minimum-index one.
    let mut best: Option<(usize, MarkerKind, usize, char)> = None;
    for (marker, kind) in [
        ('*', MarkerKind::Italic),
        ('_', MarkerKind::Italic),
        ('`', MarkerKind::Code),
        ('[', MarkerKind::Link),
    ] {
        if let Some(idx) = s.find(marker) {
            // Don't match a `*` that
            // is part of a `**`
            // sequence (which
            // should already have
            // matched Bold above,
            // but be defensive).
            if kind == MarkerKind::Italic && idx + 1 < s.len() && s.as_bytes()[idx + 1] == b'*' {
                continue;
            }
            // Don't match a `~`
            // that is part of a
            // `~~` sequence.
            if kind == MarkerKind::Strikethrough {
                continue; // already handled above
            }
            if best.is_none_or(|(b, _, _, _)| idx < b) {
                best = Some((idx, kind, 1, marker));
            }
        }
    }
    best
}

/// Find the matching close marker for
/// `open_kind` in `s` (the content
/// after the open marker). Returns
/// `(close_offset, close_length, kind)`. For
/// italic, the close character must
/// match the open character (either
/// `*` or `_`); we look for the
/// specific character that opened
/// the italic span.
fn find_close_marker(
    s: &str,
    open_kind: MarkerKind,
    open_char: char,
) -> Option<(usize, usize, MarkerKind)> {
    match open_kind {
        MarkerKind::Bold => {
            // First-match: the closing
            // `**` is the first `**` in
            // `s` after the opener. This
            // is the standard approach
            // and works for the common
            // case (`**Label**: value`).
            // Nested markers like
            // `**bold *italic***` aren't
            // produced by the JIRA
            // overlay's `build_jira_overlay_text`
            // (every bold span is a
            // simple `**Label**: value`
            // or section-name heading) so
            // the limitation is
            // acceptable for the
            // current use case. A future
            // improvement could use a
            // balanced matcher for proper
            // CommonMark support.
            s.find("**").map(|idx| (idx, 2, MarkerKind::Bold))
        }
        MarkerKind::Strikethrough => s.find("~~").map(|idx| (idx, 2, MarkerKind::Strikethrough)),
        MarkerKind::Italic | MarkerKind::Code => {
            // Single-char close. The
            // italic and code
            // parsers use the
            // single character
            // that opened them. For
            // italic, the open
            // character can be `*`
            // OR `_`; we close
            // with the same
            // character (CommonMark's
            // rule). For code, the
            // open is always `` ` ``
            // and so is the close.
            let c = match open_kind {
                MarkerKind::Italic => open_char,
                MarkerKind::Code => '`',
                _ => unreachable!(),
            };
            s.find(c).map(|idx| (idx, 1, open_kind))
        }
        MarkerKind::Link => {
            // Link close: `](url)`.
            // The content is
            // everything between
            // the `[` and the
            // `]`. Then `(` and
            // `)` wrap the URL.
            // Returns the offset
            // of the `]` (the
            // close of the
            // content); the
            // caller advances
            // past the full
            // `](url)` (the
            // close length is
            // the `](...)`
            // string).
            let close_bracket = s.find(']')?;
            let after_bracket = &s[close_bracket..];
            let url_start = after_bracket.find('(')?;
            // The `]` is at
            // close_bracket; the
            // full close is
            // `](url)`. Find
            // the matching `)`.
            let url_content = &after_bracket[url_start + 1..];
            let url_end = url_content.find(')')?;
            // Total close
            // length: from the
            // `]` to the end of
            // `)` inclusive.
            // That's
            // close_bracket +
            // url_start (the
            // `(`) + 1
            // (the `(`) + url_end
            // + 1 (the `)`).
            // Hmm, simpler:
            // the close
            // string is
            // `](url)` of
            // length 1 +
            // url_start + 1
            // (the `(`) +
            // url_end + 1
            // (the `)`).
            // Wait let me
            // just compute it
            // from the byte
            // positions.
            // We have:
            // - `]` at
            //   close_bracket
            // - `(` at
            //   close_bracket + url_start
            // - URL
            //   between
            //   (the URL
            //   is the
            //   substring
            //   between
            //   `(` and
            //   `)`)
            // - `)` at
            //   close_bracket + url_start + 1 + url_end
            // The close
            // length is
            // the distance
            // from `]` to
            // `)`+1
            // inclusive.
            // That's
            // (close_bracket + url_start + 1 + url_end + 1)
            // -
            // close_bracket
            // =
            // url_start + 1 + url_end + 1
            // =
            // url_start + url_end + 2.
            //
            // But we
            // return the
            // offset
            // *within* `s`,
            // not the
            // original
            // line.
            // The close is
            // the substring
            // starting at
            // `]` and
            // ending at
            // `)`+1
            // inclusive.
            // The
            // close_offset
            // is
            // close_bracket
            // (where `]`
            // starts).
            // The
            // close_length
            // is the
            // number of
            // bytes from
            // `]` to
            // `)`+1
            // inclusive,
            // which is
            // (url_start + 1 + url_end + 1).
            let close_len = url_start + 1 + url_end + 1;
            // The URL is
            // between the
            // `(` and `)`.
            // We don't
            // surface it in
            // the render
            // (the link text
            // is shown, the
            // URL is
            // decorative),
            // but we could
            // log it for
            // debugging.
            let _ = &url_content[..url_end];
            Some((close_bracket, close_len, MarkerKind::Link))
        }
    }
}

/// Compute the `Style` for the content
/// inside a marker pair. The
/// `render_inline` parser uses this to
/// pass a pre-decorated `base` to the
/// recursive call so nested markers
/// compose correctly.
fn style_for_marker(kind: MarkerKind, base: Style) -> Style {
    match kind {
        MarkerKind::Bold => base.add_modifier(Modifier::BOLD),
        MarkerKind::Italic => base.add_modifier(Modifier::ITALIC),
        MarkerKind::Code => {
            // Inline code: warning
            // color + bold for a
            // distinct
            // code-like
            // visual. The
            // base
            // foreground
            // is
            // overridden
            // by the
            // warning
            // color.
            Style::default()
                .fg(Theme::warning_color())
                .add_modifier(Modifier::BOLD)
        }
        MarkerKind::Strikethrough => base.add_modifier(Modifier::CROSSED_OUT),
        MarkerKind::Link => {
            // Link: accent
            // color +
            // underline.
            // Convention
            // for
            // "link"
            // treatment
            // in
            // terminals.
            Style::default()
                .fg(Theme::accent_color())
                .add_modifier(Modifier::UNDERLINED)
        }
    }
}

/// Push a plain-style span, merging with
/// the previous span when both are plain.
/// Avoids the per-character span list
/// that a naive split would produce — a
/// long JIRA description line with no
/// `**` markers would otherwise turn
/// into many single-character spans,
/// which can hurt ratatui's layout pass
/// on wide terminals.
fn push_plain_span(spans: &mut Vec<Span<'static>>, text: String, base: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == base
    {
        // Re-use the existing span by
        // appending. The owned `String`
        // lives in the span; we have
        // to replace it with a longer
        // one.
        let prev = std::mem::take(&mut last.content);
        let combined = format!("{}{}", prev.into_owned(), text);
        *last = Span::styled(combined, base);
    } else {
        spans.push(Span::styled(text, base));
    }
}

/// Parse a single line containing ANSI escape sequences (as produced by
/// `bat --color=always`) into ratatui `Span`s with the appropriate
/// foreground colors and modifiers.
///
/// Supported sequences:
/// - `\x1b[m` or `\x1b[0m` → reset
/// - `\x1b[1m` → bold
/// - `\x1b[3m` → italic
/// - `\x1b[38;2;R;G;Bm` → truecolor foreground
/// - `\x1b[38;5;Nm` → 256-color foreground (approximated)
fn parse_ansi_line(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current_text = String::new();
    let mut current_style = Style::default();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            let mut params = String::new();
            let mut cmd = '\0';
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_alphabetic() {
                    cmd = ch;
                    chars.next();
                    break;
                }
                params.push(ch);
                chars.next();
            }
            if cmd == 'm' {
                if !current_text.is_empty() {
                    spans.push(Span::styled(current_text.clone(), current_style));
                    current_text.clear();
                }
                current_style = apply_ansi_sgr(&params, current_style);
            }
        } else {
            current_text.push(c);
        }
    }
    if !current_text.is_empty() {
        spans.push(Span::styled(current_text, current_style));
    }
    spans
}

/// Apply a single SGR parameter string to a base style.
fn apply_ansi_sgr(params: &str, style: Style) -> Style {
    let parts: Vec<&str> = params.split(';').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return Style::default(); // ESC[m = reset
    }
    let primary = parts[0];
    match primary {
        "0" => Style::default(),
        "1" => style.add_modifier(Modifier::BOLD),
        "3" => style.add_modifier(Modifier::ITALIC),
        "38" if parts.len() >= 3 => match parts[1] {
            "2" if parts.len() >= 5 => {
                let r = parts[2].parse::<u8>().unwrap_or(0);
                let g = parts[3].parse::<u8>().unwrap_or(0);
                let b = parts[4].parse::<u8>().unwrap_or(0);
                style.fg(ratatui::style::Color::Rgb(r, g, b))
            }
            "5" if parts.len() >= 3 => {
                let n = parts[2].parse::<u8>().unwrap_or(0);
                let (r, g, b) = xterm256_to_rgb(n);
                style.fg(ratatui::style::Color::Rgb(r, g, b))
            }
            _ => style,
        },
        "48" if parts.len() >= 3 => match parts[1] {
            "2" if parts.len() >= 5 => {
                let r = parts[2].parse::<u8>().unwrap_or(0);
                let g = parts[3].parse::<u8>().unwrap_or(0);
                let b = parts[4].parse::<u8>().unwrap_or(0);
                style.bg(ratatui::style::Color::Rgb(r, g, b))
            }
            "5" if parts.len() >= 3 => {
                let n = parts[2].parse::<u8>().unwrap_or(0);
                let (r, g, b) = xterm256_to_rgb(n);
                style.bg(ratatui::style::Color::Rgb(r, g, b))
            }
            _ => style,
        },
        _ => style,
    }
}

/// Convert a standard xterm 256-color index to an RGB triple.
fn xterm256_to_rgb(n: u8) -> (u8, u8, u8) {
    match n {
        0..=7 => {
            const COLORS: [(u8, u8, u8); 8] = [
                (0, 0, 0),
                (205, 0, 0),
                (0, 205, 0),
                (205, 205, 0),
                (0, 0, 238),
                (205, 0, 205),
                (0, 205, 205),
                (229, 229, 229),
            ];
            COLORS[n as usize]
        }
        8..=15 => {
            const COLORS: [(u8, u8, u8); 8] = [
                (127, 127, 127),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (92, 92, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            COLORS[(n - 8) as usize]
        }
        16..=231 => {
            let c = n - 16;
            let r = c / 36;
            let g = (c % 36) / 6;
            let b = c % 6;
            (
                if r == 0 { 0 } else { r * 40 + 55 },
                if g == 0 { 0 } else { g * 40 + 55 },
                if b == 0 { 0 } else { b * 40 + 55 },
            )
        }
        _ => {
            let gray = n - 232;
            let v = gray * 10 + 8;
            (v, v, v)
        }
    }
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

    // Ag-mode, tags-mode, and codegraph-mode rows carry up to
    // [`SOURCE_CONTEXT_LINES`] (50) lines of source context
    // plus a callers/callees overlay. The inline pane's height
    // caps the actually-visible count (ratatui renders only
    // what fits), but we don't clamp the slice here so a tall
    // terminal / a scrolled `Ctrl-O` overlay can show every
    // loaded line. Plain history rows keep their tighter
    // 4-line preview.
    let take_n = if row.mode == "ag" || row.mode == "tags" || row.mode == "codegraph" {
        crate::tui::SOURCE_CONTEXT_LINES
    } else {
        4
    };
    // `highlight_with_bat` (`--color=always`) emits ANSI escape
    // codes for tags/codegraph rows, and `ag` itself emits ANSI
    // for matched-line previews. The markdown `render_preview_line`
    // path doesn't parse ANSI (it would mangle `\x1b[...m` through
    // the inline parser), so any output containing an escape must
    // go through `parse_ansi_line` instead. This is mode-agnostic:
    // a codegraph/tags row falls back to plain text when bat is
    // unavailable (no ANSI), and an ag row whose match had no
    // coloring proceeds through the markdown path cleanly.
    let has_ansi = row.output.contains('\x1b');
    let preview_lines: Vec<Line> = if has_ansi {
        row.output
            .lines()
            .take(take_n)
            .map(parse_ansi_line)
            .map(Line::from)
            .collect()
    } else {
        row.output
            .lines()
            .take(take_n)
            .map(render_preview_line)
            .collect()
    };

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
        Some(ref buf) => {
            // The comment-edit buffer is
            // shared between the local
            // `command_comments` path and
            // the JIRA `add_comment`
            // path. The JIRA path keys on
            // `jira_add_comment_target` being
            // `Some(issue_key)` — when set,
            // the user is composing a new
            // comment to POST to JIRA, not
            // editing a local command
            // note. The prompt and border
            // title change to make the
            // mode obvious: "jira>" + " jira
            // comment " (info tint, matching
            // the JIRA search mode's colour
            // so the user immediately
            // recognises this is a JIRA
            // action, not a local one).
            if app.jira_add_comment_target.is_some() {
                (
                    "jira> ".to_string(),
                    " jira comment ".to_string(),
                    buf.as_str(),
                )
            } else {
                (
                    "comment> ".to_string(),
                    " comment ".to_string(),
                    buf.as_str(),
                )
            }
        }
        None => {
            // The prompt and title are determined by
            // the PREFIX MODE, not the match algorithm.
            // The algorithm (Substring/Fuzzy/Regex) is a
            // separate orthogonal toggle (C-f) that
            // determines HOW the body is matched, not
            // which view the user is in.
            //
            // The algorithm is shown as a `·``algoname`
            // suffix in the border title so the user
            // knows which algorithm is active without
            // looking at the mode strip chip.
            let algo = match app.match_algorithm {
                crate::tui::state::MatchAlgorithm::Substring => "",
                crate::tui::state::MatchAlgorithm::Fuzzy => " · fuzzy",
                crate::tui::state::MatchAlgorithm::Regex => " · regex",
            };
            if is_output {
                (
                    "+".to_string(),
                    format!(" output{} ", algo),
                    app.query.as_str(),
                )
            } else if is_llm {
                ("=".to_string(), " LLM ".to_string(), app.query.as_str())
            } else if is_notes {
                (
                    "@".to_string(),
                    format!(" notes{} ", algo),
                    app.query.as_str(),
                )
            } else if is_question {
                ("%".to_string(), " ? ".to_string(), app.query.as_str())
            } else if is_todo {
                (
                    "!".to_string(),
                    format!(" todo{} ", algo),
                    app.query.as_str(),
                )
            } else if is_directories {
                (
                    "#".to_string(),
                    format!(" directories{} ", algo),
                    app.query.as_str(),
                )
            } else if app.is_panes_query() {
                (
                    "*".to_string(),
                    format!(" panes{} ", algo),
                    app.query.as_str(),
                )
            } else if app.is_jira_query() {
                let jql_title = app
                    .jira_last_jql
                    .as_deref()
                    .map_or_else(|| " jira ".to_string(), |j| format!(" jira ({}) ", j));
                ("-".to_string(), jql_title, app.query.as_str())
            } else if app.is_files_query() {
                (
                    "~".to_string(),
                    format!(" files{} ", algo),
                    app.query.as_str(),
                )
            } else if app.is_tags_query() {
                (
                    "$".to_string(),
                    format!(" symbols{} ", algo),
                    app.query.as_str(),
                )
            } else if app.is_codegraph_query() {
                (
                    "&".to_string(),
                    format!(" codegraph{} ", algo),
                    app.query.as_str(),
                )
            } else if app.is_ag_query() {
                (",".to_string(), format!(" ag{} ", algo), app.query.as_str())
            } else {
                (
                    "> ".to_string(),
                    format!(" history{} ", algo),
                    app.query.as_str(),
                )
            }
        }
    };
    let input = Paragraph::new(Line::from(vec![
        Span::styled(prompt.clone(), Theme::accent()),
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
            .title_style(if is_output {
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
            } else if app.is_panes_query() {
                Style::default().fg(Theme::success_color())
            } else if app.is_jira_query() {
                Style::default().fg(Theme::info_color())
            } else if app.is_files_query() {
                Style::default().fg(Theme::success_color())
            } else if app.is_tags_query() {
                Style::default().fg(Theme::success_color())
            } else if app.is_codegraph_query() {
                Style::default().fg(Theme::accent_color())
            } else if app.is_ag_query() {
                Style::default().fg(Theme::warning_color())
            } else if is_regex {
                Style::default().fg(Theme::warning_color())
            } else if is_fuzzy {
                Style::default().fg(Theme::success_color())
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
                if app.jira_add_comment_target.is_some() {
                    Style::default().fg(Theme::info_color())
                } else {
                    Style::default().fg(Theme::warning_color())
                }
            } else if app.notes_query_error {
                Style::default().fg(Theme::error_color())
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
            } else if app.is_panes_query() {
                Style::default().fg(Theme::success_color())
            } else if app.is_jira_query() {
                Style::default().fg(Theme::info_color())
            } else if app.is_files_query() {
                Style::default().fg(Theme::success_color())
            } else if app.is_tags_query() {
                Style::default().fg(Theme::success_color())
            } else if app.is_codegraph_query() {
                Style::default().fg(Theme::accent_color())
            } else if app.is_ag_query() {
                Style::default().fg(Theme::warning_color())
            } else if is_regex {
                Style::default().fg(Theme::warning_color())
            } else if is_fuzzy {
                Style::default().fg(Theme::success_color())
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
    // When the comment-edit buffer is active, the cursor
    // should follow the comment buffer (which is always at
    // the end since push_char appends and backspace pops).
    // Using `query_cursor` here would track the search
    // query's cursor instead — a bug the user reported as
    // "the cursor stays at the same position while I
    // type in the comment field".
    let cursor_offset = if app.comment_edit.is_some() {
        content.chars().count() as u16
    } else {
        app.query_cursor as u16
    };
    let cursor_x = area.x + 1 + prompt_width + cursor_offset;
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
    let help_palette = format_key_specs(app.bindings.specs(Action::CommandAction));
    let help_clear = format_key_specs(app.bindings.specs(Action::ClearQuery));
    let help = format!(
        " {} help · {} palette · {} clear",
        help_open, help_palette, help_clear
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

    /// The `DIR:HERDR` chip
    /// rename is the
    /// user-facing surface
    /// of the multiplexer
    /// abstraction: when
    /// the user has
    /// `multiplexer=herdr`
    /// in their config and
    /// the directory source
    /// is set to
    /// `Tmux` (the
    /// "show me
    /// active-context
    /// rows" filter), the
    /// chip reads
    /// `DIR:HERDR` rather
    /// than `DIR:TMUX` so
    /// the user knows
    /// *which* backend is
    /// producing the
    /// marker. The
    /// `All` and `Config`
    /// sources keep their
    /// enum-derived labels
    /// (they don't depend
    /// on the backend).
    ///
    /// The `tmux` backend
    /// is the historical
    /// behaviour: `DIR:TMUX`
    /// when the source is
    /// `Tmux`.
    #[test]
    fn directory_source_badge_renames_tmux_to_backend_name() {
        use crate::tui::state::DirectorySource;
        use ratatui::text::Span;
        // herdr backend +
        // Tmux source =
        // `DIR:HERDR`.
        let chip = super::directory_source_badge(DirectorySource::Tmux, "herdr");
        let span: &Span = &chip;
        let text = span.content.to_string();
        assert_eq!(
            text, " DIR:HERDR ",
            "herdr backend must rename the chip to DIR:HERDR, got: {text:?}"
        );
        // tmux backend +
        // Tmux source =
        // `DIR:TMUX`
        // (historical
        // behaviour).
        let chip = super::directory_source_badge(DirectorySource::Tmux, "tmux");
        let text = chip.content.to_string();
        assert_eq!(
            text, " DIR:TMUX ",
            "tmux backend must keep the chip as DIR:TMUX, got: {text:?}"
        );
        // `All` source
        // ignores the
        // backend (shows
        // every row).
        let chip = super::directory_source_badge(DirectorySource::All, "herdr");
        let text = chip.content.to_string();
        assert_eq!(
            text, " DIR:ALL ",
            "All source must keep its enum-derived label, got: {text:?}"
        );
        // `Config` source
        // ignores the
        // backend (shows
        // only
        // `sessiondirs=...`
        // rows).
        let chip = super::directory_source_badge(DirectorySource::Config, "herdr");
        let text = chip.content.to_string();
        assert_eq!(
            text, " DIR:CFG ",
            "Config source must keep its enum-derived label, got: {text:?}"
        );
    }

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

    // ---- render_preview_line (the **...** bold parser) ----

    use super::super::theme::Theme;
    /// The helper is in the same module as
    /// the tests, so a single-level `super`
    /// import reaches it.
    use super::render_preview_line;
    use ratatui::style::Modifier;

    /// A line with no `**` markers
    /// renders as a single plain span
    /// (preserving the no-marker path's
    /// backward compatibility for the
    /// non-JIRA modes that don't emit
    /// bold markup).
    #[test]
    fn preview_line_plain_text_unchanged() {
        let line = render_preview_line("Status: Open");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "Status: Open");
        // No BOLD modifier on the span.
        // `Style::add_modifier` is a public
        // `bitflags!` Modifier field, so we
        // can use its generated `contains`
        // method to check.
        assert!(!line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// A line with one `**Label**` pair
    /// splits into two spans: a bold
    /// span for the label and a plain
    /// span for the trailing value.
    #[test]
    fn preview_line_single_bold_label() {
        let line = render_preview_line("**Status**: Open");
        // 2 spans: bold "Status" + plain ": Open".
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "Status");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        // Second span is the trailing text,
        // without BOLD.
        assert_eq!(line.spans[1].content, ": Open");
        assert!(!line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    /// Multiple `**...**` pairs on the
    /// same line produce a sequence of
    /// bold + plain + bold + plain
    /// spans. The user's spec renders
    /// five attributes per row, so this
    /// is the common-case shape (one
    /// pair per line, but the parser
    /// should handle multiple).
    #[test]
    fn preview_line_multiple_bold_labels() {
        // Hypothetical "inline" format
        // (the JIRA row builder uses one
        // bold pair per line; this is the
        // parser's robustness test).
        let line = render_preview_line("**A**: 1, **B**: 2");
        // 4 spans: bold "A" + plain ": 1, "
        // + bold "B" + plain ": 2".
        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[0].content, "A");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].content, ": 1, ");
        assert!(!line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[2].content, "B");
        assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[3].content, ": 2");
        assert!(!line.spans[3].style.add_modifier.contains(Modifier::BOLD));
    }

    /// An unclosed `**` (one with no
    /// matching close) is rendered as a
    /// plain span containing the literal
    /// `**` plus the rest of the line.
    /// The user gets a visible hint that
    /// something is off rather than a
    /// half-styled fragment.
    #[test]
    fn preview_line_unclosed_marker_falls_through_to_plain() {
        let line = render_preview_line("**no closer here");
        // 1 plain span containing the full
        // line including the literal `**`.
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "**no closer here");
        assert!(!line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// An empty line produces a
    /// single empty span (never an
    /// empty `Vec` — ratatui collapses
    /// empty `Line`s in some configurations
    /// which can cause layout glitches).
    #[test]
    fn preview_line_empty_input_yields_one_empty_span() {
        let line = render_preview_line("");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "");
    }

    /// The plain-text segments before
    /// and after a bold marker get
    /// merged into a single span by
    /// `push_plain_span` (not three
    /// separate single-character spans).
    /// A long description line without
    /// `**` is the worst case for
    /// span-fragmentation; this test
    /// asserts the optimisation.
    #[test]
    fn preview_line_plain_segments_are_merged() {
        // No `**` markers → a single
        // plain span, not many
        // single-character spans.
        let line = render_preview_line("a long description with no markers");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "a long description with no markers");
    }

    /// A line that starts with `## `
    /// (the heading marker) is rendered as
    /// a single heading-styled span. The
    /// `**` bold parser does NOT run on
    /// heading lines — the heading is the
    /// whole content of the line.
    #[test]
    fn preview_line_heading_marker_renders_as_heading() {
        let line = render_preview_line("## Comments");
        assert_eq!(line.spans.len(), 1);
        // The `## ` prefix is stripped —
        // only the heading text is in the
        // span content.
        assert_eq!(line.spans[0].content, "Comments");
        // The heading style is bold and
        // tinted with the accent color.
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        // The accent color is the
        // foreground (not empty /
        // default).
        assert!(line.spans[0].style.fg.is_some());
    }

    /// A heading line with multiple words
    /// keeps the full heading text in a
    /// single span. Whitespace between
    /// words is preserved.
    #[test]
    fn preview_line_heading_with_multiple_words() {
        let line = render_preview_line("## Comments by Alice");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "Comments by Alice");
    }

    /// A line that contains `## ` but
    /// doesn't start with it (e.g. as
    /// inline text) is treated as plain
    /// text, NOT a heading. Only the
    /// line-start position triggers
    /// the heading style.
    #[test]
    fn preview_line_inline_hash_mark_is_not_a_heading() {
        let line = render_preview_line("see ## section for details");
        // 1 plain span — the `## ` in
        // the middle of the line is just
        // text.
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "see ## section for details");
        // No BOLD modifier.
        assert!(!line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// A line that starts with `##` but
    /// no space (e.g. `##tag`) is NOT
    /// a heading — the marker is
    /// `## ` (with a space), not just
    /// `##`. The line is treated as
    /// plain text. This avoids false
    /// positives on markdown-like
    /// content where `##` is used as
    /// a non-heading character.
    #[test]
    fn preview_line_double_hash_without_space_is_not_a_heading() {
        let line = render_preview_line("##tag");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "##tag");
        assert!(!line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// A line that's just `##` (marker
    /// but no text) is also not a
    /// heading — the space is required.
    /// Falls through to the bold parser
    /// (which produces a single empty
    /// span, since the marker alone
    /// has no enclosing `**...**`).
    #[test]
    fn preview_line_double_hash_alone_is_not_a_heading() {
        let line = render_preview_line("##");
        // Treated as plain text (no
        // heading style). The exact
        // span count depends on the
        // bold parser; assert that no
        // heading style is applied.
        for span in &line.spans {
            // No BOLD modifier on any
            // span — the heading
            // detector didn't fire.
            assert!(!span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    // ---- block-level elements ----

    /// `# text` is an H1 heading: bold +
    /// the success color, with a leading
    /// `▸ ` glyph in the same color.
    #[test]
    fn preview_line_heading1_renders_with_success_color() {
        let line = render_preview_line("# Big Title");
        // Two spans: marker + text.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "▸ ");
        assert_eq!(line.spans[1].content, "Big Title");
        // Both spans are bold; the
        // text uses the success color.
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].style.fg, Some(Theme::success_color()));
    }

    /// `## text` is an H2 heading: bold +
    /// the accent color, no leading glyph.
    /// (This is the existing `## ` style,
    /// locked in by the
    /// `preview_line_heading_marker_renders_as_heading`
    /// test above.)
    #[test]
    fn preview_line_heading2_renders_with_accent_color() {
        let line = render_preview_line("## Section");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "Section");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[0].style.fg, Some(Theme::accent_color()));
    }

    /// `### text` is an H3 heading:
    /// 2-space indent + bold + the
    /// dim color. Subdued so it
    /// doesn't compete with H1 / H2.
    #[test]
    fn preview_line_heading3_renders_indented_and_dim() {
        let line = render_preview_line("### Subsection");
        // Two spans: indent + text.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "  ");
        assert_eq!(line.spans[1].content, "Subsection");
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].style.fg, Some(Theme::dim_color()));
    }

    /// `####` (4+ hashes) is plain text
    /// per CommonMark — headings are
    /// capped at 3 levels.
    #[test]
    fn preview_line_four_or_more_hashes_is_plain_text() {
        let line = render_preview_line("#### too many");
        // Not a heading — no
        // `Theme::accent` / `success` /
        // `dim` foreground on the text
        // (the leading `#### ` survives
        // as plain text).
        for span in &line.spans {
            // No BOLD modifier.
            assert!(!span.style.add_modifier.contains(Modifier::BOLD));
        }
        // The content includes the
        // `####` prefix.
        assert!(line.spans.iter().any(|s| s.content.contains("####")));
    }

    /// `> text` is a blockquote: italic
    /// text with a `│ ` gutter in the
    /// info color.
    #[test]
    fn preview_line_blockquote_renders_with_gutter() {
        let line = render_preview_line("> a wise quote");
        // Two spans: gutter + italic
        // text.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "│ ");
        assert_eq!(line.spans[1].content, "a wise quote");
        // The text is italic.
        assert!(line.spans[1].style.add_modifier.contains(Modifier::ITALIC));
        // The gutter is the info
        // color.
        assert_eq!(line.spans[0].style.fg, Some(Theme::info_color()));
    }

    /// `- item` is a bullet list item:
    /// `• ` marker in the accent color,
    /// content in plain text.
    #[test]
    fn preview_line_bullet_renders_with_marker() {
        let line = render_preview_line("- first item");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "• ");
        assert_eq!(line.spans[1].content, "first item");
        assert_eq!(line.spans[0].style.fg, Some(Theme::accent_color()));
    }

    /// `* item` (asterisk + space) is
    /// also a bullet — same rendering
    /// as `- item`.
    #[test]
    fn preview_line_asterisk_bullet_renders_with_marker() {
        let line = render_preview_line("* star item");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "• ");
        assert_eq!(line.spans[1].content, "star item");
    }

    /// `1. item` is an ordered list
    /// item: `1. ` marker in the
    /// accent color, content plain.
    #[test]
    fn preview_line_ordered_list_renders_with_number() {
        let line = render_preview_line("7. seventh item");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "7. ");
        assert_eq!(line.spans[1].content, "seventh item");
        assert_eq!(line.spans[0].style.fg, Some(Theme::accent_color()));
    }

    /// `---` (3+ dashes) is a horizontal
    /// rule: full-width `─` line in the
    /// dim color.
    #[test]
    fn preview_line_three_dashes_is_horizontal_rule() {
        let line = render_preview_line("---");
        assert_eq!(line.spans.len(), 1);
        // 40 `─` chars (the
        // renderer's fixed width).
        assert_eq!(line.spans[0].content.chars().count(), 40);
        assert!(line.spans[0].content.chars().all(|c| c == '─'));
        assert_eq!(line.spans[0].style.fg, Some(Theme::dim_color()));
    }

    /// `***` (3+ asterisks) is also a
    /// horizontal rule.
    #[test]
    fn preview_line_three_asterisks_is_horizontal_rule() {
        let line = render_preview_line("***");
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].content.chars().all(|c| c == '─'));
    }

    /// A line with only two dashes is
    /// plain text (need 3+ for a
    /// horizontal rule).
    #[test]
    fn preview_line_two_dashes_is_plain_text() {
        let line = render_preview_line("--");
        // Treated as plain text; the
        // `--` is preserved verbatim.
        assert!(line.spans.iter().any(|s| s.content.contains("--")));
        // No dim color (the dim color
        // is reserved for the
        // horizontal-rule path).
        for span in &line.spans {
            // (No BOLD either, but the
            // main check is that
            // we're in the Plain
            // path.)
            assert!(!span.style.add_modifier.contains(Modifier::BOLD));
        }
    }

    // ---- inline markers ----

    /// `*foo*` is italic.
    #[test]
    fn preview_line_italic_marker_renders_italic() {
        let line = render_preview_line("this is *italic* text");
        // 3 spans: plain, italic,
        // plain.
        assert!(line.spans.len() >= 3);
        // Find the italic span.
        let italic_span = line
            .spans
            .iter()
            .find(|s| s.content == "italic")
            .expect("italic span");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    /// `_foo_` is italic (alias for
    /// `*foo*`).
    #[test]
    fn preview_line_underscore_italic_marker_renders_italic() {
        let line = render_preview_line("this is _italic_ text");
        let italic_span = line
            .spans
            .iter()
            .find(|s| s.content == "italic")
            .expect("italic span");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));
    }

    /// `` `code` `` is inline code:
    /// warning color + bold.
    #[test]
    fn preview_line_inline_code_renders_with_warning_color() {
        let line = render_preview_line("call `foo()` here");
        let code_span = line
            .spans
            .iter()
            .find(|s| s.content == "foo()")
            .expect("code span");
        assert_eq!(code_span.style.fg, Some(Theme::warning_color()));
        assert!(code_span.style.add_modifier.contains(Modifier::BOLD));
    }

    /// `~~strike~~` is strikethrough.
    #[test]
    fn preview_line_strikethrough_marker_renders_crossed_out() {
        let line = render_preview_line("this is ~~old~~ text");
        let strike_span = line
            .spans
            .iter()
            .find(|s| s.content == "old")
            .expect("strike span");
        assert!(
            strike_span
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
    }

    /// `[text](url)` is a link: accent
    /// color + underline. The URL is
    /// hidden (the link text is
    /// shown).
    #[test]
    fn preview_line_link_renders_with_underline() {
        let line = render_preview_line("see [docs](https://example.com) here");
        // The link text "docs" is
        // rendered as a link. The URL
        // "https://example.com" is NOT
        // in the rendered output.
        let link_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("docs"))
            .expect("link span");
        assert_eq!(link_span.style.fg, Some(Theme::accent_color()));
        assert!(link_span.style.add_modifier.contains(Modifier::UNDERLINED));
        // The URL is hidden (not in
        // any span's content).
        for span in &line.spans {
            assert!(!span.content.contains("https://"));
        }
    }

    /// Nested markers like `**bold
    /// *italic***` aren't produced
    /// by the JIRA overlay's
    /// `build_jira_overlay_text` (every
    /// bold span is a simple
    /// `**Label**: value` or section
    /// heading). The first-match
    /// close strategy (which the
    /// current parser uses) handles
    /// the common case correctly and
    /// is good enough for the JIRA
    /// use case. This test pins down
    /// the simple `**bold**` /
    /// `**foo bar**` shapes that the
    /// JIRA overlay does emit.
    #[test]
    fn preview_line_bold_simple_marker_styling() {
        // A simple `**bold**` produces
        // exactly one bold span with
        // the expected text. No
        // content outside the span.
        let line = render_preview_line("**bold**");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "bold");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// `**foo** and **bar**` (two
    /// simple bold spans) produces
    /// 3 spans: bold, plain,
    /// bold. The first-match close
    /// strategy works because the
    /// `**` pairs are well-separated
    /// and each one closes at its
    /// expected position.
    #[test]
    fn preview_line_two_simple_bold_spans() {
        let line = render_preview_line("**foo** and **bar**");
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "foo");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].content, " and ");
        assert!(!line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[2].content, "bar");
        assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    /// An unclosed `` ` `` (inline
    /// code) falls through to plain
    /// text — the rest of the line,
    /// including the literal `` ` ``,
    /// is rendered without code
    /// styling.
    #[test]
    fn preview_line_unclosed_inline_marker_falls_through() {
        let line = render_preview_line("`unclosed code");
        // No warning color (the
        // unclosed marker fell through
        // to plain text).
        for span in &line.spans {
            assert_ne!(span.style.fg, Some(Theme::warning_color()));
        }
        // The literal ` is in the
        // rendered output.
        let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains('`'));
    }

    /// A `- ` (single dash + space) at
    /// the start of a line is a
    /// bullet list marker, not a
    /// horizontal rule (need 3+
    /// dashes for an HR).
    #[test]
    fn preview_line_single_dash_is_bullet_not_hr() {
        let line = render_preview_line("- just a bullet");
        // Bullet marker is `• `, NOT
        // a horizontal rule (which
        // would be 40 `─` chars).
        assert_eq!(line.spans[0].content, "• ");
        // Content is the rest.
        assert_eq!(line.spans[1].content, "just a bullet");
    }

    /// A `1.item` (no space after the
    /// dot) is plain text, NOT an
    /// ordered list.
    #[test]
    fn preview_line_ordered_list_requires_space_after_dot() {
        let line = render_preview_line("1.no-space");
        // No `1. ` marker; the
        // content is the original
        // line.
        let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(content, "1.no-space");
    }

    /// `1.` with no text after is
    /// plain text (need at least one
    /// non-space character after
    /// `. `).
    #[test]
    fn preview_line_ordered_list_requires_text() {
        let line = render_preview_line("1. ");
        // The `1. ` doesn't trigger an
        // ordered list because
        // there's no text after the
        // space. Falls through to
        // plain text.
        let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(content, "1. ");
    }

    /// Empty input yields a single
    /// empty plain span (the
    /// renderer's contract: never an
    /// empty `Vec<Span>`).
    #[test]
    fn preview_line_empty_input_with_new_parser() {
        let line = render_preview_line("");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "");
    }
}

// Fix the test expectations — adjacent text after a reset gets merged
// into a single default-style span, so 3 spans not 4.
#[cfg(test)]
mod ansi_tests {
    use super::*;

    #[test]
    fn parse_ansi_truecolor() {
        let input = "\x1b[38;2;102;217;239mfn\x1b[0m \x1b[38;2;166;226;46mmain\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "fn");
        assert_eq!(
            spans[0].style.fg,
            Some(ratatui::style::Color::Rgb(102, 217, 239))
        );
        assert_eq!(spans[1].content, " ");
        assert_eq!(spans[2].content, "main");
        assert_eq!(
            spans[2].style.fg,
            Some(ratatui::style::Color::Rgb(166, 226, 46))
        );
    }

    #[test]
    fn parse_ansi_bold_and_italic() {
        let input = "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 3);
        assert!(spans[0].style.add_modifier == ratatui::style::Modifier::BOLD);
        assert_eq!(spans[1].content, " ");
        assert!(spans[2].style.add_modifier == ratatui::style::Modifier::ITALIC);
    }

    #[test]
    fn parse_ansi_plain_text_unchanged() {
        let input = "no escapes here";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "no escapes here");
    }

    #[test]
    fn parse_ansi_256_color() {
        let input = "\x1b[38;5;196mred\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "red");
        assert_eq!(
            spans[0].style.fg,
            Some(ratatui::style::Color::Rgb(255, 0, 0))
        );
    }
}
