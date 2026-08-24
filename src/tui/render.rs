#![allow(clippy::if_same_then_else)]
#![allow(clippy::map_identity)]
// Render code: the main `ui` entry point plus all the draw_*
// helpers (draw_output_view, draw_help_view, draw_command_menu,
// draw_theme_picker, etc.) and the highlight_matches helpers.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap},
};

use super::bindings::{ALL_ACTIONS, Action, format_key_spec, format_key_specs};
use super::state::{
    ExitFilter, HistoryRow, KeyBindingsEditor, Mode, NoteComposeDialog, NoteCreateDialog,
    NoteCreateField, SortOrder,
};
use super::theme::palette_storage::PALETTE;
use super::theme::{Theme, ThemePicker};
use super::{
    AddEntryDialog, AddEntryKind, App, CommandMenu, ConfirmMode, CorrectView, DescribeView, HelpView, NotesDateFilter, OutputView, PrefixHelpView, PrefixPicker, QuestionView,
    char_to_byte_index, format_diff, format_time, mark_key,
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

    // The details row height adapts to the user's
    // `pane_height` setting (Default: 8 lines,
    // Tall: ~70% of the list area). `page_size`
    // is the total terminal height minus the
    // fixed chrome; `detail_row_height` returns
    // the right value for each variant.
    let detail_h = app
        .pane_height
        .detail_row_height(f.area().height as usize);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1), // mode strip
                Constraint::Fill(1),   // list
                Constraint::Length(detail_h), // details row
                Constraint::Length(3), // input
                Constraint::Length(1), // status
            ]
            .as_ref(),
        )
        .split(f.area());

    let mode_strip_start = std::time::Instant::now();
    draw_mode_strip(f, app, chunks[0]);
    let mode_strip_elapsed = mode_strip_start.elapsed();

    let list_start = std::time::Instant::now();
    draw_list(f, app, chunks[1]);
    let list_elapsed = list_start.elapsed();

    let mut details_elapsed = std::time::Duration::ZERO;
    let mut output_preview_elapsed = std::time::Duration::ZERO;
    match app.pane_visibility {
        crate::tui::state::PaneVisibility::Both => {
            let detail_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
                .split(chunks[2]);
            let details_start = std::time::Instant::now();
            draw_details(f, app, detail_chunks[0]);
            details_elapsed = details_start.elapsed();
            let output_preview_start = std::time::Instant::now();
            draw_output_preview(f, app, detail_chunks[1]);
            output_preview_elapsed = output_preview_start.elapsed();
        }
        crate::tui::state::PaneVisibility::Details => {
            let details_start = std::time::Instant::now();
            draw_details(f, app, chunks[2]);
            details_elapsed = details_start.elapsed();
        }
        crate::tui::state::PaneVisibility::OutputPreview => {
            let output_preview_start = std::time::Instant::now();
            draw_output_preview(f, app, chunks[2]);
            output_preview_elapsed = output_preview_start.elapsed();
        }
    }

    let input_start = std::time::Instant::now();
    draw_input(f, app, chunks[3]);
    let input_elapsed = input_start.elapsed();

    let status_start = std::time::Instant::now();
    draw_status(f, app, chunks[4]);
    let status_elapsed = status_start.elapsed();

    // Breaks down a slow `terminal.draw()` call (already logged as
    // one number by `run_loop`) into which sub-widget is actually
    // responsible — reported after a `draw=11839ms`-style stall
    // narrowed the freeze to *somewhere* inside `ui()`, but not to
    // which pane.
    if mode_strip_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
        || list_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
        || details_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
        || output_preview_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
        || input_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
        || status_elapsed.as_millis() >= crate::tui::PERF_LOG_THRESHOLD_MS
    {
        crate::tui::perf_debug_log(&format!(
            "ui: mode_strip={}ms list={}ms details={}ms output_preview={}ms input={}ms status={}ms rows={} selected={:?}",
            mode_strip_elapsed.as_millis(),
            list_elapsed.as_millis(),
            details_elapsed.as_millis(),
            output_preview_elapsed.as_millis(),
            input_elapsed.as_millis(),
            status_elapsed.as_millis(),
            app.merged_rows().len(),
            app.list_state.selected(),
        ));
    }

    if let Some(ref mode) = app.confirm_delete {
        draw_confirm_delete(f, app, mode);
    }

    if let Some(ref signal) = app.confirm_signal {
        draw_confirm_signal(f, app, signal);
    }

    if let Some(ref prompt) = app.zoxide_save_prompt {
        draw_zoxide_save_prompt(f, app, prompt);
    }

    if let Some(ref prompt) = app.project_since_prompt {
        draw_project_since_prompt(f, app, prompt);
    }

    if let Some(ref prompt) = app.template_name_prompt {
        draw_template_name_prompt(f, app, prompt);
    }

    if let Some(ref flow) = app.worktree_create_flow {
        draw_worktree_create_flow(f, flow);
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

    // The prefix query-syntax help overlay can be opened FROM the
    // picker above (via `F3` on a highlighted row) without closing
    // it, so it's drawn after — full-screen `Clear` means it covers
    // the picker entirely while open, matching `handle_key`'s
    // priority ordering for the same pair.
    if let Some(view) = app.prefix_help_view.as_ref() {
        draw_prefix_help_view(f, app, view);
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

    if let Some(editor) = app.key_bindings_editor.as_ref() {
        draw_key_bindings_editor(f, app, editor);
    }

    // The add-session /
    // add-host dialog is the
    // topmost overlay: drawn
    // last so it sits on top
    // of every other pane.
    if let Some(dialog) = app.add_entry_dialog.as_ref() {
        draw_add_entry_dialog(f, app, dialog);
    }

    // The "create JIRA issue" dialog is a sibling of the add-entry
    // dialog: also drawn last (topmost), also mutually exclusive
    // with it in practice.
    if let Some(dialog) = app.create_jira_issue_dialog.as_ref() {
        draw_create_jira_issue_dialog(f, dialog);
    }

    // The "create JIRA issue from template" picker is a sibling of the
    // dialog it opens: also drawn last, mutually exclusive with it (the
    // picker closes itself before the dialog opens).
    if let Some(picker) = app.jira_template_picker.as_ref() {
        draw_jira_template_picker(f, picker);
    }

    // The note/todo compose overlay is a sibling of the
    // add-entry dialog: also drawn last (topmost). The two are
    // mutually exclusive in practice (each opens via its own
    // dedicated key and `handle_key`'s precedence chain routes
    // all input to whichever is open), so draw order between
    // them doesn't matter for correctness — this is just
    // "newest overlay wins" for consistency with the pattern
    // above.
    if let Some(dialog) = app.note_compose.as_ref() {
        draw_note_compose(f, app, dialog);
    }

    // The two-field
    // `create-note` dialog
    // is a sibling of the
    // single-field
    // `note_compose` overlay:
    // also drawn last
    // (topmost). The two
    // are mutually
    // exclusive in
    // practice (the
    // `handle_key`
    // precedence chain
    // routes input to
    // whichever is open),
    // so draw order
    // between them
    // doesn't matter for
    // correctness — this
    // is just "newest
    // overlay wins" for
    // consistency with
    // the pattern above.
    if let Some(dialog) = app.note_create.as_ref() {
        draw_note_create(f, app, dialog);
        // The "save or drop?" confirmation is a small overlay on
        // top of the create-note dialog itself (not a sibling of
        // it) — drawn right after so it always sits above, matching
        // `handle_note_create_confirm_key`'s precedence over the
        // dialog's own keymap.
        if dialog.confirm_discard {
            draw_note_create_confirm(f, app);
        }
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
        ConfirmMode::DeleteMarked { count } => (
            " Delete marked entries ",
            format!(
                "Are you sure you want to delete all {} marked {}?",
                count,
                if *count == 1 { "entry" } else { "entries" },
            ),
        ),
        ConfirmMode::DisposeWorktree { path, label, dirty, unpushed, .. } => {
            let warnings = crate::tui::mode::worktree::dispose_warnings(*dirty, *unpushed);
            let warning_text = if warnings.is_empty() {
                String::new()
            } else {
                format!("\n\nWarning: this worktree has {}.", warnings.join(" and "))
            };
            (
                " Dispose worktree ",
                format!(
                    "Remove the worktree for {}:\n  {}{}",
                    label,
                    crate::util::shorten_home_path(path, &app.home_list),
                    warning_text,
                ),
            )
        }
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

/// The `%` (processes) mode signal-confirmation dialog
/// (`app.confirm_signal`), opened by `App::stage_process_signal_prompt`
/// when Enter is pressed on a process row. Modeled directly on
/// `draw_confirm_delete` (same red/error styling — sending a signal
/// is destructive, unlike the non-destructive `zoxide_save_prompt`
/// below) with one addition: the message is built fresh from
/// `signal.signal` every frame, so Tab/Shift-Tab cycling the signal
/// (`handle_confirm_signal_key`) updates the displayed text on the
/// very next render with no extra plumbing.
fn draw_confirm_signal(f: &mut Frame, app: &App, signal: &crate::tui::SignalConfirm) {
    let area = centered_rect(60, 25, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let title = " Send signal to process ";
    let message = format!(
        "Send {} to pid {} ({})?",
        signal.signal.label(),
        signal.pid,
        signal.name,
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::error())
        .border_style(Theme::error());

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
            Span::raw(" to cancel, "),
            Span::styled("Tab", Theme::highlight()),
            Span::raw(" to cycle the signal."),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

/// The create-note dialog's "save or drop?" confirmation overlay
/// (`dialog.confirm_discard == true`), shown by `Esc`/`Ctrl-C` when
/// either field has unsaved text — see
/// `App::note_create_confirm_discard_if_dirty` and
/// `handle_note_create_confirm_key` for the full flow. Deliberately
/// smaller than `draw_confirm_delete`'s popup (this one has no
/// per-case message to fit, just the fixed prompt) and drawn on top
/// of the create-note dialog rather than replacing it, so the
/// user's typed Title/Content stay visible underneath while they
/// decide.
fn draw_note_create_confirm(f: &mut Frame, app: &App) {
    let area = centered_rect(50, 18, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Unsaved note ")
        .title_style(Theme::accent())
        .border_style(Theme::accent());

    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let cancel_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        cancel_keys
    };
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Save this note before closing?",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("Enter", Theme::highlight()),
            Span::raw(" to save (default), "),
            Span::styled("d", Theme::highlight()),
            Span::raw(" to drop it, or "),
            Span::styled(cancel_hint, Theme::highlight()),
            Span::raw(" to keep editing."),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

/// The `~` (zoxide) mode "save this directory?" prompt
/// (`app.zoxide_save_prompt`), shown after selecting a directory
/// not already saved as a `session.<id>` entry — see
/// `crate::tui::state::ZoxideSavePrompt`'s doc comment for the full
/// flow. Non-destructive (unlike `draw_confirm_delete`): both
/// answers complete the directory jump, so this uses the accent
/// color like `draw_note_create_confirm`, not the alarming error
/// color `draw_confirm_delete` uses for its actually-destructive
/// actions.
fn draw_zoxide_save_prompt(
    f: &mut Frame,
    app: &App,
    prompt: &crate::tui::state::ZoxideSavePrompt,
) {
    let area = centered_rect(60, 22, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Save directory? ")
        .title_style(Theme::accent())
        .border_style(Theme::accent());

    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let cancel_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        cancel_keys
    };
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("Save \"{}\" to your Directories list?", prompt.label),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            prompt.directory.as_str(),
            Theme::dim(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("Enter", Theme::highlight()),
            Span::raw("/"),
            Span::styled("y", Theme::highlight()),
            Span::raw(" to save (default), "),
            Span::styled("n", Theme::highlight()),
            Span::raw(" or "),
            Span::styled(cancel_hint, Theme::highlight()),
            Span::raw(" to skip — either way, you'll still jump there."),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn draw_project_since_prompt(
    f: &mut Frame,
    app: &App,
    prompt: &crate::tui::state::ProjectSincePrompt,
) {
    let area = centered_rect(60, 22, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Switch project ")
        .title_style(Theme::accent())
        .border_style(Theme::accent());

    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let cancel_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        cancel_keys
    };

    // Same reversed-character/reversed-space cursor convention every
    // other single-line input in the TUI uses.
    let chars: Vec<char> = prompt.buffer.chars().collect();
    let mut buffer_spans: Vec<Span> = Vec::new();
    if chars.is_empty() {
        buffer_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
        buffer_spans.push(Span::styled(" (just now)", Theme::dim()));
    } else {
        let pre: String = chars.iter().take(prompt.cursor).collect();
        buffer_spans.push(Span::raw(pre));
        if prompt.cursor < chars.len() {
            buffer_spans.push(Span::styled(
                chars[prompt.cursor].to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            let post: String = chars.iter().skip(prompt.cursor + 1).collect();
            if !post.is_empty() {
                buffer_spans.push(Span::raw(post));
            }
        } else {
            buffer_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
        }
        buffer_spans.push(Span::styled(" minutes ago", Theme::dim()));
    }

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("Switch to \"{}\" — started how many minutes ago?", prompt.slug),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(buffer_spans),
        Line::from(""),
        Line::from(vec![
            Span::raw("Digits only, "),
            Span::styled("Enter", Theme::highlight()),
            Span::raw(" confirms (blank = just now), "),
            Span::styled(cancel_hint, Theme::highlight()),
            Span::raw(" cancels."),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

/// Renders `TemplateNamePrompt` — same shape as `draw_project_since_prompt`,
/// with a free-text buffer (no "(just now)" placeholder — an empty buffer
/// is invalid here, not a valid default) and an inline error line when
/// `prompt.error.is_some()`.
fn draw_template_name_prompt(
    f: &mut Frame,
    app: &App,
    prompt: &crate::tui::state::TemplateNamePrompt,
) {
    let area = centered_rect(60, 22, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Create template from issue ")
        .title_style(Theme::accent())
        .border_style(Theme::accent());

    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let cancel_hint = if cancel_keys.is_empty() {
        "no key bound".to_string()
    } else {
        cancel_keys
    };

    let chars: Vec<char> = prompt.buffer.chars().collect();
    let mut buffer_spans: Vec<Span> = Vec::new();
    if chars.is_empty() {
        buffer_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
    } else {
        let pre: String = chars.iter().take(prompt.cursor).collect();
        buffer_spans.push(Span::raw(pre));
        if prompt.cursor < chars.len() {
            buffer_spans.push(Span::styled(
                chars[prompt.cursor].to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            let post: String = chars.iter().skip(prompt.cursor + 1).collect();
            if !post.is_empty() {
                buffer_spans.push(Span::raw(post));
            }
        } else {
            buffer_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
        }
    }

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("Create a template from {}", prompt.source_key),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::raw("Template name:")),
        Line::from(buffer_spans),
    ];
    if let Some(error) = &prompt.error {
        text.push(Line::from(""));
        text.push(Line::from(Span::styled(error.clone(), Theme::error())));
    }
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::styled("Enter", Theme::highlight()),
        Span::raw(" confirms, "),
        Span::styled(cancel_hint, Theme::highlight()),
        Span::raw(" cancels."),
    ]));

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

/// Draw the multi-line note/todo compose overlay
/// (`Action::ComposeNoteEntry`). The buffer is rendered as
/// literal lines split on `'\n'` — no soft-wrap — so long
/// lines simply extend past the visible width; this keeps the
/// cursor-position math a straightforward line/column count
/// instead of having to account for ratatui's wrap points too.
/// A simple bottom-anchored auto-scroll keeps the cursor's line
/// visible when the buffer grows taller than the box.
fn draw_note_compose(f: &mut Frame, app: &App, dialog: &NoteComposeDialog) {
    let area = centered_rect(70, 60, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let title = if dialog.todo {
        " New todo (multi-line) — Ctrl-S save "
    } else {
        " New note (multi-line) — Ctrl-S save "
    };

    let cursor_byte = char_to_byte_index(&dialog.text, dialog.cursor);
    let before_cursor = &dialog.text[..cursor_byte];
    let cursor_line = before_cursor.matches('\n').count();
    let cursor_col = before_cursor.rsplit('\n').next().unwrap_or("").chars().count();

    let display_lines: Vec<Line> = dialog.text.split('\n').map(Line::from).collect();
    // Reserve 2 rows for the top/bottom border and 1 for the
    // footer hint (which overwrites the bottom border row, same
    // convention as the describe/help overlays' scroll footer).
    let inner_height = area.height.saturating_sub(3).max(1) as usize;
    let scroll_y = cursor_line.saturating_sub(inner_height.saturating_sub(1)) as u16;

    let paragraph = Paragraph::new(display_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(title)
            .title_style(Theme::accent())
            .border_style(Theme::accent())
            .style(Style::default().bg(PALETTE.with(|p| p.borrow().list_bg))),
    );
    f.render_widget(paragraph.scroll((scroll_y, 0)), area);

    if area.height >= 4 {
        let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
        let cancel_hint = if cancel_keys.is_empty() {
            "no key bound".to_string()
        } else {
            cancel_keys
        };
        let footer = format!(
            " Ctrl-S save · {} cancel · Enter newline ",
            cancel_hint
        );
        let footer_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(footer, Theme::dim()))),
            footer_area,
        );
    }

    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let visible_cursor_line = cursor_line.saturating_sub(scroll_y as usize);
    let cursor_x =
        (inner_x + cursor_col as u16).min(area.x + area.width.saturating_sub(2));
    let cursor_y = (inner_y + visible_cursor_line as u16)
        .min(area.y + area.height.saturating_sub(2));
    f.set_cursor_position((cursor_x, cursor_y));
}

/// Draw the
/// two-field
/// `create-note`
/// dialog. Renders
/// the Title
/// (single-line)
/// and Content
/// (multi-line)
/// fields stacked
/// vertically,
/// with a footer
/// hint bar at the
/// bottom showing
/// the save / cancel
/// shortcuts. The
/// active field is
/// highlighted
/// (different border
/// style); the
/// other field
/// shows the same
/// border style as
/// the outer
/// dialog for
/// visual
/// consistency.
///
/// Cursor rendering:
/// the visible
/// cursor is drawn
/// on the active
/// field only
/// (the other field
/// is read-only by
/// visual
/// convention; the
/// user types into
/// the active
/// field as
/// determined by
/// the
/// `note_create_toggle_field`
/// path, which is
/// bound to `Tab`).
/// Word-wrap one logical line (no `\n`, given as its already-split
/// `chars`) into rows of at most `width` characters, breaking at the
/// last whitespace before the width boundary; a single word longer
/// than `width` is hard-broken. Always returns at least one
/// (possibly empty) row, so blank logical lines still occupy a
/// display row. Each returned row carries the CHARACTER offset
/// (within `chars`) where it starts, so callers can map a cursor
/// offset in the logical line to a `(row_index, col_in_row)` pair —
/// see `content_display_position` below.
fn wrap_chars_to_rows(chars: &[char], width: usize) -> Vec<(String, usize)> {
    if chars.is_empty() {
        return vec![(String::new(), 0)];
    }
    // `width == 0` can't wrap at all (and would infinite-loop below,
    // since `end` would never advance past `start`) — treat it as
    // "no wrapping", returning the line whole. In practice the
    // caller always passes `inner_width_content.max(1)`, so this is
    // just a safety net, not a normal code path.
    if width == 0 {
        return vec![(chars.iter().collect(), 0)];
    }
    let mut rows = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        if end < chars.len() {
            if let Some(ws) = (start..end).rev().find(|&i| chars[i].is_whitespace()) {
                rows.push((chars[start..ws].iter().collect(), start));
                start = ws + 1; // drop the whitespace itself — it's the break point
                continue;
            }
        }
        rows.push((chars[start..end].iter().collect(), start));
        start = end;
    }
    rows
}

/// Given a logical line's wrapped rows (from `wrap_chars_to_rows`)
/// and a character offset `col` within that logical line, find
/// which row the cursor sits on and its column within that row.
/// Picks the LAST row whose start offset is `<= col` — so a cursor
/// sitting exactly at a wrap point lands at the START of the row
/// that begins there (col 0) rather than one column past the end of
/// the previous row.
fn content_display_position(rows: &[(String, usize)], col: usize) -> (usize, usize) {
    let row_idx = rows
        .iter()
        .rposition(|(_, start)| *start <= col)
        .unwrap_or(0);
    let row_len = rows[row_idx].0.chars().count();
    (row_idx, (col - rows[row_idx].1).min(row_len))
}

fn draw_note_create(
    f: &mut Frame,
    app: &App,
    dialog: &NoteCreateDialog,
) {
    // Slightly
    // taller than
    // the
    // single-field
    // compose
    // dialog (80%
    // of the
    // viewport
    // height) so
    // the
    // multi-line
    // Content
    // field
    // has
    // enough
    // room
    // for a
    // paragraph
    // or two.
    let area = centered_rect(70, 75, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" New note (Title + Content) — Ctrl-S save, Ctrl-O save+edit ")
        .title_style(Theme::accent())
        .border_style(Theme::accent())
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().list_bg)));
    let inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    // Layout: a
    // 3-line
    // Title
    // field at
    // the top
    // (border
    // + content
    // + spacing),
    // a
    // multi-line
    // Content
    // field in
    // the
    // middle
    // (filling
    // the
    // remaining
    // height),
    // and a
    // 1-line
    // footer
    // at the
    // bottom.
    // We split
    // the inner
    // area
    // into
    // three
    // chunks:
    // title (3
    // lines),
    // content
    // (fill),
    // footer
    // (1
    // line).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3), // Title field
                Constraint::Length(1), // spacer
                Constraint::Fill(1),   // Content field
                Constraint::Length(1), // footer
            ]
            .as_ref(),
        )
        .split(inner);

    // ----- Title field (single-line) -----
    let title_active = dialog.active_field == NoteCreateField::Title;
    let title_selected = title_active && dialog.select_all;
    let title_border_style = if title_selected {
        Theme::warning()
    } else if title_active {
        Theme::accent()
    } else {
        Theme::dim()
    };
    let title_block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(if title_selected {
            " Title (SELECTED — Ctrl-C yank, ⌫ clear) "
        } else if title_active {
            " Title (active, Tab → Content) "
        } else {
            " Title "
        })
        .title_style(title_border_style)
        .border_style(title_border_style);
    // The whole field is shown in reverse video while selected
    // (`Ctrl-A`) — the same visual convention every other selection
    // highlight in the TUI uses (see e.g. the completion menu's
    // highlighted candidate).
    let title_text_style = if title_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let title_paragraph =
        Paragraph::new(Line::from(Span::styled(dialog.title.as_str(), title_text_style)))
            .block(title_block)
            .style(Style::default().bg(PALETTE.with(|p| p.borrow().list_bg)));
    f.render_widget(title_paragraph, chunks[0]);

    // ----- Content field (multi-line) -----
    let content_active = dialog.active_field == NoteCreateField::Content;
    let content_selected = content_active && dialog.select_all;
    let content_border_style = if content_selected {
        Theme::warning()
    } else if content_active {
        Theme::accent()
    } else {
        Theme::dim()
    };
    let cursor_byte = char_to_byte_index(&dialog.content, dialog.content_cursor);
    let before_cursor = &dialog.content[..cursor_byte];
    // Word-wrap each logical (`\n`-separated) line at the field's
    // inner width so long lines wrap visually at the edge of the
    // box instead of running off-screen or getting clipped. This is
    // a soft wrap — it never touches `dialog.content` itself, only
    // how it's displayed and where the cursor is drawn.
    let inner_width_content = chunks[2].width.saturating_sub(2).max(1) as usize;
    let content_line_chars: Vec<Vec<char>> = dialog
        .content
        .split('\n')
        .map(|l| l.chars().collect())
        .collect();
    let wrapped_lines: Vec<Vec<(String, usize)>> = content_line_chars
        .iter()
        .map(|chars| wrap_chars_to_rows(chars, inner_width_content))
        .collect();
    // The whole field is shown in reverse video while selected
    // (`Ctrl-A`) — same convention as the Title field above.
    let content_text_style = if content_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    let display_lines: Vec<Line> = wrapped_lines
        .iter()
        .flat_map(|rows| {
            rows.iter()
                .map(|(text, _)| Line::from(Span::styled(text.as_str(), content_text_style)))
        })
        .collect();
    let content_block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(if content_selected {
            " Content (SELECTED — Ctrl-C yank, ⌫ clear) "
        } else if content_active {
            " Content (active, Tab → Title) "
        } else {
            " Content "
        })
        .title_style(content_border_style)
        .border_style(content_border_style);
    // The cursor's LOGICAL line/column (same as before wrapping was
    // added), plus its position within that line's wrapped rows —
    // used both to scroll (keep the cursor's DISPLAY row near the
    // bottom of the visible area, same convention as
    // `draw_note_compose`) and to place the on-screen cursor below.
    let cursor_line = before_cursor.matches('\n').count();
    let cursor_col = before_cursor.rsplit('\n').next().unwrap_or("").chars().count();
    let (cursor_row_in_line, cursor_col_in_row) =
        content_display_position(&wrapped_lines[cursor_line], cursor_col);
    let cursor_display_row: usize = wrapped_lines[..cursor_line]
        .iter()
        .map(|rows| rows.len())
        .sum::<usize>()
        + cursor_row_in_line;
    let inner_height_content = chunks[2].height.saturating_sub(2).max(1) as usize;
    let scroll_y_content = cursor_display_row
        .saturating_sub(inner_height_content.saturating_sub(1))
        as u16;
    let content_paragraph = Paragraph::new(display_lines)
        .block(content_block)
        .scroll((scroll_y_content, 0))
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().list_bg)));
    f.render_widget(content_paragraph, chunks[2]);

    // ----- Footer hint -----
    if chunks[3].height >= 1 {
        let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
        let cancel_hint = if cancel_keys.is_empty() {
            "no key bound".to_string()
        } else {
            cancel_keys
        };
        let footer = format!(
            " Ctrl-S save · Ctrl-O save+edit · Tab next field · C-d/C-7/C-n notes · {} cancel ",
            cancel_hint
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(footer, Theme::dim()))),
            chunks[3],
        );
    }

    // ----- Cursor -----
    // The
    // cursor
    // only
    // appears
    // on the
    // active
    // field.
    // For the
    // Content
    // field
    // we
    // also
    // account
    // for
    // the
    // scroll-y
    // so the
    // cursor
    // follows
    // the
    // visible
    // text
    // (not
    // the raw
    // line
    // number).
    if title_active {
        let cursor_x = chunks[0].x + 1 + dialog.title_cursor as u16;
        let cursor_y = chunks[0].y + 1;
        f.set_cursor_position((
            cursor_x.min(chunks[0].x + chunks[0].width.saturating_sub(2)),
            cursor_y,
        ));
    } else if content_active {
        let visible_display_row = cursor_display_row.saturating_sub(scroll_y_content as usize);
        let cursor_x = chunks[2].x + 1 + cursor_col_in_row as u16;
        let cursor_y = chunks[2].y + 1 + visible_display_row as u16;
        f.set_cursor_position((
            cursor_x.min(chunks[2].x + chunks[2].width.saturating_sub(2)),
            cursor_y.min(chunks[2].y + chunks[2].height.saturating_sub(2)),
        ));
    }

    // ----- Completion menu overlay -----
    // When the user has
    // pressed `Tab` on a
    // word starting with
    // one of the supported
    // prefixes (`@p:`, etc.)
    // and the candidate list
    // is non-empty, render a
    // small inline menu
    // below the active field
    // showing the candidates
    // with the currently
    // selected one
    // highlighted. The menu
    // is positioned just
    // above the footer hint
    // so it doesn't overlap
    // the Title / Content
    // fields.
    if let Some(ref menu) = dialog.completion
        && !menu.candidates.is_empty()
    {
        let n = menu.candidates.len();
        // Cap the menu height
        // to a reasonable
        // fraction of the
        // dialog so it doesn't
        // overflow the
        // terminal. The user
        // can scroll the
        // candidate list with
        // arrow keys (the
        // menu's `selected`
        // index is unbounded).
        let visible = n.min(8);
        let menu_height = (visible as u16) + 2; // +2 for top/bottom border
        // Try to place the
        // menu just above the
        // footer. If the
        // dialog is too
        // short, fall back to
        // a centered overlay
        // below the dialog.
        let menu_y = if chunks[3].y > menu_height {
            chunks[3].y - menu_height - 1
        } else {
            // No room
            // above
            // the
            // footer
            // —
            // fall
            // back
            // to
            // a
            // centered
            // overlay
            // that
            // covers
            // the
            // middle
            // of
            // the
            // dialog.
            area.y + (area.height.saturating_sub(menu_height)) / 2
        };
        let menu_x = chunks[0].x;
        let menu_w = chunks[0].width;
        let menu_area = Rect {
            x: menu_x,
            y: menu_y,
            width: menu_w,
            height: menu_height.min(area.height.saturating_sub(menu_y - area.y)),
        };
        // The
        // window
        // the
        // user
        // is
        // currently
        // looking
        // at:
        // if
        // `selected
        // >=
        // visible`,
        // the
        // menu
        // scrolls
        // the
        // candidate
        // list
        // so
        // the
        // selected
        // row
        // stays
        // visible.
        // This
        // mirrors
        // the
        // main
        // completion
        // menu's
        // scroll-window
        // behavior.
        let scroll = if menu.selected >= visible {
            menu.selected + 1 - visible
        } else {
            0
        };
        let items: Vec<ListItem> = (0..visible)
            .map(|i| {
                let idx = scroll + i;
                if idx >= n {
                    ListItem::new("")
                } else {
                    let candidate = &menu.candidates[idx];
                    let style = if idx == menu.selected {
                        Style::default()
                            .fg(Theme::accent_color())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Theme::dim_color())
                    };
                    let marker = if idx == menu.selected { "▸ " } else { "  " };
                    ListItem::new(Line::from(Span::styled(
                        format!("{marker}{candidate}"),
                        style,
                    )))
                }
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .title(" Tab completion (Enter to commit, Esc to cancel) ")
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
            .highlight_symbol("▌")
            .repeat_highlight_symbol(true);
        // The
        // menu
        // doesn't
        // need
        // its
        // own
        // stateful
        // widget
        // (we
        // already
        // mark
        // the
        // selected
        // item
        // via
        // a
        // `▸`
        // glyph
        // in
        // the
        // line
        // text).
        let mut state = ratatui::widgets::ListState::default();
        state.select(Some(menu.selected.min(visible - 1)));
        f.render_stateful_widget(list, menu_area, &mut state);
    }
}

/// Draw a centered overlay with a bordered title bar, clearing the
/// area underneath. Returns the inner content area (inside the
/// border) for the caller to render into.
///
/// This helper collapses ~8 copies of the same
/// `centered_rect → Clear → Block::default().borders(ALL)`
/// boilerplate across `draw_command_menu`,
/// `draw_prefix_picker`, `draw_theme_picker`,
/// `draw_completion_menu`, `draw_confirm_delete`,
/// `draw_add_entry_dialog`, `draw_help_view`, and
/// `draw_codegraph_relations_picker`. Each of those call
/// sites now calls this helper and renders only the content.
fn overlay(
    f: &mut Frame,
    title: &str,
    percent_x: u16,
    percent_y: u16,
) -> Rect {
    let area = centered_rect(percent_x, percent_y, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let bg = PALETTE.with(|p| p.borrow().bg);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title.to_string())
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
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

/// Render a single-line `DialogField` as `<name>: <value>` with a
/// cursor glyph, matching `draw_add_entry_dialog`'s per-field
/// rendering exactly (same reversed-character/reversed-space cursor
/// convention) — pulled out as its own function here since this
/// dialog needs it for two fields (Subject, Labels) alongside
/// non-`DialogField` selector rows, unlike `draw_add_entry_dialog`
/// where every field is a `DialogField`.
fn dialog_field_line<'a>(field: &'a crate::tui::state::DialogField, is_focused: bool) -> Line<'a> {
    dialog_field_line_inner(field, is_focused, false)
}

/// `dialog_field_line`, with an optional trailing dim `" (cloned)"`
/// marker — used by `draw_create_jira_issue_dialog`'s extra-field loop
/// for a `ClonedCustomField` so the UI explains WHY typing into it
/// does nothing, rather than a silent, confusing no-op.
fn dialog_field_line_inner<'a>(
    field: &'a crate::tui::state::DialogField,
    is_focused: bool,
    read_only: bool,
) -> Line<'a> {
    let style = if is_focused { Theme::highlight() } else { Style::default() };
    // The field name is always bold — a dominant label the user can
    // scan even when the field isn't focused — while its color still
    // tracks focus like the value text does.
    let label_style = style.add_modifier(Modifier::BOLD);
    let chars: Vec<char> = field.value.chars().collect();
    let mut spans: Vec<Span> = vec![Span::styled(format!("{}: ", field.name), label_style)];
    if field.value.is_empty() && is_focused {
        spans.push(Span::styled(field.placeholder.to_string(), Theme::dim()));
        spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
    } else {
        let pre: String = chars.iter().take(field.cursor).collect();
        spans.push(Span::styled(pre, style));
        if is_focused {
            if field.cursor < chars.len() {
                spans.push(Span::styled(
                    chars[field.cursor].to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            } else {
                spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
            }
        }
        let post: String = chars
            .iter()
            .skip(if is_focused {
                field.cursor + if field.cursor < chars.len() { 1 } else { 0 }
            } else {
                field.cursor
            })
            .collect();
        if !post.is_empty() {
            spans.push(Span::styled(post, style));
        }
    }
    if read_only {
        spans.push(Span::styled(" (cloned)", Theme::dim()));
    }
    Line::from(spans)
}

/// The "create JIRA issue from template" picker — a plain arrow-key list
/// (no search/filter, unlike `draw_theme_picker`; see
/// `JiraTemplatePicker`'s doc comment for why).
fn draw_jira_template_picker(f: &mut Frame, picker: &crate::tui::state::JiraTemplatePicker) {
    use ratatui::widgets::{List, ListItem};

    let inner = overlay(f, " Create JIRA issue from template — ↑/↓ select, Enter open, Esc cancel ", 50, 60);

    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .map(|(name, _)| ListItem::new(name.as_str()))
        .collect();
    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .add_modifier(Modifier::BOLD);
    let mut list_state = ratatui::widgets::ListState::default().with_selected(Some(picker.selected));
    let list = List::new(items)
        .highlight_style(highlight_style)
        .highlight_symbol("▌");
    f.render_stateful_widget(list, inner, &mut list_state);
}

/// Renders the `Action::CreateWorktree` dialog (`WorktreeCreateFlow`).
/// The three list-driven steps (`PickBranch`/`PickBaseBranch`/
/// `PickProject`) share this layout: a title describing the step, a
/// filter-input line (cursor rendered the same reversed-character
/// convention every other single-line input uses), an optional error
/// line, and the filtered option list below. `ConfirmCarryOver` swaps
/// the filter/list for a plain y/n prompt.
fn draw_worktree_create_flow(f: &mut Frame, flow: &crate::tui::state::WorktreeCreateFlow) {
    use crate::tui::state::WorktreeCreateStep;
    use ratatui::widgets::{List, ListItem};

    let title = match flow.step {
        WorktreeCreateStep::PickBranch => " Create worktree — pick or create a branch ",
        WorktreeCreateStep::PickBaseBranch => " Create worktree — pick a base branch ",
        WorktreeCreateStep::ConfirmCarryOver => " Create worktree — carry over uncommitted changes? ",
        WorktreeCreateStep::PickProject => " Create worktree — assign to a project (optional) ",
    };
    let inner = overlay(f, title, 60, 60);

    if flow.step == WorktreeCreateStep::ConfirmCarryOver {
        let text = vec![
            Line::from(""),
            Line::from(Span::styled(
                "The current checkout has uncommitted changes.",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("Stash them and apply the stash in the new worktree?"),
            Line::from(""),
            Line::from(vec![
                Span::styled("y", Theme::highlight()),
                Span::raw(" carry over, "),
                Span::styled("n", Theme::highlight()),
                Span::raw(" leave them, "),
                Span::styled("Esc", Theme::highlight()),
                Span::raw(" cancel."),
            ]),
        ];
        let paragraph = Paragraph::new(text)
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true });
        f.render_widget(paragraph, inner);
        return;
    }

    let has_error = flow.error.is_some();
    let mut constraints = vec![Constraint::Length(1)]; // filter input
    if has_error {
        constraints.push(Constraint::Length(1)); // error line
    }
    constraints.push(Constraint::Fill(1)); // option list
    constraints.push(Constraint::Length(1)); // footer hint
    let chunks = Layout::default().direction(Direction::Vertical).constraints(constraints).split(inner);

    // Same reversed-character/reversed-space cursor convention every
    // other single-line input in the TUI uses.
    let chars: Vec<char> = flow.filter.chars().collect();
    let mut filter_spans: Vec<Span> = vec![Span::styled("> ", Theme::dim())];
    if chars.is_empty() {
        filter_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
    } else {
        let pre: String = chars.iter().take(flow.cursor).collect();
        filter_spans.push(Span::raw(pre));
        if flow.cursor < chars.len() {
            filter_spans.push(Span::styled(
                chars[flow.cursor].to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            let post: String = chars.iter().skip(flow.cursor + 1).collect();
            if !post.is_empty() {
                filter_spans.push(Span::raw(post));
            }
        } else {
            filter_spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(filter_spans)), chunks[0]);

    let mut next_idx = 1;
    if has_error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                flow.error.as_deref().unwrap_or_default(),
                Style::default().fg(Theme::error_color()),
            ))),
            chunks[next_idx],
        );
        next_idx += 1;
    }

    let filtered = crate::tui::state::worktree_create_filtered_options(flow);
    let items: Vec<ListItem> = filtered.iter().map(|o| ListItem::new(o.as_str())).collect();
    let highlight_style = Style::default().bg(Theme::selection_color()).add_modifier(Modifier::BOLD);
    let selected = if filtered.is_empty() { None } else { Some(flow.selected.min(filtered.len() - 1)) };
    let mut list_state = ratatui::widgets::ListState::default().with_selected(selected);
    let list = List::new(items).highlight_style(highlight_style).highlight_symbol("▌");
    f.render_stateful_widget(list, chunks[next_idx], &mut list_state);
    next_idx += 1;

    let footer = match flow.step {
        WorktreeCreateStep::PickBranch => {
            "↑/↓ select · Enter pick/create · Esc cancel"
        }
        WorktreeCreateStep::PickBaseBranch => "↑/↓ select · Enter pick · Esc cancel",
        WorktreeCreateStep::PickProject => {
            "↑/↓ select · Enter pick/create/skip (blank) · Esc cancel"
        }
        WorktreeCreateStep::ConfirmCarryOver => unreachable!("handled above"),
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(footer, Theme::dim()))), chunks[next_idx]);
}

/// Render a Project/Issue Type selector row: `<name>: ◂ value ▸`.
fn selector_line<'a>(name: &'a str, value: &'a str, is_focused: bool) -> Line<'a> {
    let style = if is_focused { Theme::highlight() } else { Style::default() };
    // Bold name label, same dominance rule `dialog_field_line` uses.
    let label_style = style.add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(format!("{name}: "), label_style),
        Span::styled(if is_focused { "◂ " } else { "  " }, Theme::dim()),
        Span::styled(value.to_string(), style),
        Span::styled(if is_focused { " ▸" } else { "  " }, Theme::dim()),
    ])
}

fn draw_create_jira_issue_dialog(f: &mut Frame, dialog: &crate::tui::state::CreateJiraIssueDialog) {
    use crate::tui::state::CreateJiraIssueFocus;

    // Layout: Issue Type (1) + Project (1) + Subject (1) + Labels (1)
    // + Description (fill) + error (1, only when present) + footer
    // (1) + borders (2). Issue Type leads (the field most often
    // changed away from its default) and Labels sits right after
    // Subject so the two short fields are typed back-to-back before
    // the long-form Description. Capped at 80% of the viewport height
    // the same way `draw_add_entry_dialog`/`draw_note_create` are,
    // for the same reason (leave the underlying TUI visible as a cue
    // this is a transient overlay).
    let has_error = dialog.error.is_some();
    let area = centered_rect(70, 75, f.area());
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Create JIRA issue ")
        .title_style(Theme::accent())
        .border_style(Theme::accent())
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().list_bg)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let extra_count = dialog.extra_fields.len();
    let mut constraints = vec![
        Constraint::Length(1), // Issue Type
        Constraint::Length(1), // Project
        Constraint::Length(1), // Subject
        Constraint::Length(1), // Labels
    ];
    for _ in 0..extra_count {
        constraints.push(Constraint::Length(1)); // one row per template extra field
    }
    constraints.push(Constraint::Fill(1)); // Description — always last
    if has_error {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // footer
    let chunks = Layout::default().direction(Direction::Vertical).constraints(constraints).split(inner);
    // Description's chunk index shifts by however many extra fields a
    // template contributed (rendered between Labels and Description).
    let desc_idx = 4 + extra_count;

    f.render_widget(
        Paragraph::new(selector_line(
            "Issue Type",
            &dialog.issue_types[dialog.issue_type_index],
            dialog.focused == CreateJiraIssueFocus::IssueType,
        )),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(selector_line(
            "Project",
            &dialog.projects[dialog.project_index],
            dialog.focused == CreateJiraIssueFocus::Project,
        )),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(dialog_field_line(&dialog.fields[0], dialog.focused == CreateJiraIssueFocus::Subject)),
        chunks[2],
    );
    f.render_widget(
        Paragraph::new(dialog_field_line(&dialog.fields[2], dialog.focused == CreateJiraIssueFocus::Labels)),
        chunks[3],
    );
    for (i, field) in dialog.extra_fields.iter().enumerate() {
        let read_only = matches!(
            dialog.extra_field_kinds.get(i),
            Some(crate::tui::state::ExtraFieldKind::ClonedCustomField(_))
        );
        f.render_widget(
            Paragraph::new(dialog_field_line_inner(
                field,
                dialog.focused == CreateJiraIssueFocus::Extra(i),
                read_only,
            )),
            chunks[4 + i],
        );
    }

    // Description: its own bordered box (unlike the flat single-line
    // fields above), title doubling as the "Description:" label — same
    // border-as-label convention `draw_note_create`'s Content field
    // uses. Multi-line, word-wrapped, with the same bottom-anchored
    // auto-scroll that field uses too
    // (`wrap_chars_to_rows`/`content_display_position`) — so a long
    // pre-filled body (a note's full content, or a JIRA issue's
    // description) can actually be scrolled to and edited throughout,
    // not just typed into invisibly off-screen at whatever the cursor
    // happens to sit at. The cursor's own wrapped row gets a rendered
    // glyph (same reversed-character/reversed-space convention
    // `dialog_field_line` uses for the single-line fields above) —
    // every other row renders as plain text.
    let desc_focused = dialog.focused == CreateJiraIssueFocus::Description;
    let desc_border_style = if desc_focused { Theme::highlight() } else { Theme::dim() };
    let desc_block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(" Description ")
        .title_style(desc_border_style)
        .border_style(desc_border_style);
    let desc_inner = desc_block.inner(chunks[desc_idx]);
    f.render_widget(desc_block, chunks[desc_idx]);

    let desc_field = &dialog.fields[1];
    let mut desc_lines: Vec<Line> = Vec::new();
    let mut scroll_y_desc: u16 = 0;
    if desc_field.value.is_empty() {
        desc_lines.push(Line::from(Span::styled(desc_field.placeholder, Theme::dim())));
    } else {
        let inner_width_desc = desc_inner.width.max(1) as usize;
        let line_chars: Vec<Vec<char>> =
            desc_field.value.split('\n').map(|l| l.chars().collect()).collect();
        let wrapped_lines: Vec<Vec<(String, usize)>> =
            line_chars.iter().map(|chars| wrap_chars_to_rows(chars, inner_width_desc)).collect();

        // Cursor's logical (line, col), plus its position within that
        // line's wrapped rows — computed up front (before building
        // `desc_lines`) so the row-building loop below knows which
        // single row to render with a cursor glyph instead of plain
        // text.
        let cursor_byte = char_to_byte_index(&desc_field.value, desc_field.cursor);
        let before_cursor = &desc_field.value[..cursor_byte];
        let cursor_line = before_cursor.matches('\n').count();
        let cursor_col = before_cursor.rsplit('\n').next().unwrap_or("").chars().count();
        let (cursor_row_in_line, cursor_col_in_row) =
            content_display_position(&wrapped_lines[cursor_line], cursor_col);

        for (line_idx, rows) in wrapped_lines.iter().enumerate() {
            for (row_idx, (text, _)) in rows.iter().enumerate() {
                if desc_focused && line_idx == cursor_line && row_idx == cursor_row_in_line {
                    let chars: Vec<char> = text.chars().collect();
                    let pre: String = chars.iter().take(cursor_col_in_row).collect();
                    let mut spans = vec![Span::raw(pre)];
                    if cursor_col_in_row < chars.len() {
                        spans.push(Span::styled(
                            chars[cursor_col_in_row].to_string(),
                            Style::default().add_modifier(Modifier::REVERSED),
                        ));
                        let post: String = chars.iter().skip(cursor_col_in_row + 1).collect();
                        if !post.is_empty() {
                            spans.push(Span::raw(post));
                        }
                    } else {
                        spans.push(Span::styled(
                            " ",
                            Style::default().add_modifier(Modifier::REVERSED),
                        ));
                    }
                    desc_lines.push(Line::from(spans));
                } else {
                    desc_lines.push(Line::from(text.clone()));
                }
            }
        }

        if desc_focused {
            let cursor_display_row: usize = wrapped_lines[..cursor_line]
                .iter()
                .map(|rows| rows.len())
                .sum::<usize>()
                + cursor_row_in_line;
            let inner_height_desc = desc_inner.height.max(1) as usize;
            scroll_y_desc =
                cursor_display_row.saturating_sub(inner_height_desc.saturating_sub(1)) as u16;
        }
    }
    f.render_widget(
        Paragraph::new(desc_lines).scroll((scroll_y_desc, 0)),
        desc_inner,
    );

    let mut next_idx = desc_idx + 1;
    if let Some(err) = &dialog.error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(err.clone(), Theme::error()))),
            chunks[next_idx],
        );
        next_idx += 1;
    }

    let footer = Line::from(vec![
        Span::styled("Tab", Theme::highlight()),
        Span::raw("/"),
        Span::styled("S-Tab", Theme::highlight()),
        Span::raw(" next/prev field, "),
        Span::styled("←/→", Theme::highlight()),
        Span::raw(" change Issue Type/Project, "),
        Span::styled("↑/↓", Theme::highlight()),
        Span::raw(" move line in Description, "),
        Span::styled("Ctrl-S", Theme::highlight()),
        Span::raw(" create, "),
        Span::styled("Esc", Theme::highlight()),
        Span::raw(" cancel"),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[next_idx]);
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
    // codegraph modes syntax-highlight source context (`syntect`,
    // via `highlight_with_bat`/`highlight_with_bat_auto`), and ag
    // matches carry ANSI from `ag`.
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
/// action (prefixed with `?`).
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

/// Mirrors `draw_help_view` exactly (same bespoke full-screen `Clear`
/// + rounded-border `Block` + theme-tinted scrollable `Paragraph` +
/// scroll-position footer chrome), just backed by
/// `PrefixHelpView`/`prefix_help::lines_for` instead of
/// `HelpView`/`build_help_lines`. Kept as a separate function rather
/// than parameterizing `draw_help_view` — the two overlays' content
/// sources and title text differ enough (mode-specific title here)
/// that sharing would need its own indirection layer for little gain.
fn draw_prefix_help_view(f: &mut Frame, app: &App, view: &PrefixHelpView) {
    // Cover the whole screen so the help is the only thing visible.
    let area = f.area();
    f.render_widget(ratatui::widgets::Clear, area);

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let close_hint = if cancel_keys.is_empty() {
        String::from("(no key bound)")
    } else {
        format!("{} to close", cancel_keys)
    };
    let title = match view.mode {
        Some(mode) => format!(
            " Prefix help — {} ({}) — {} ",
            mode.list_title(),
            mode.prefix(&app.query_prefixes),
            close_hint
        ),
        None => format!(" Prefix help — {} ", close_hint),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(title)
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));

    let inner_h = area.height.saturating_sub(2) as usize;
    let lines = crate::tui::mode::prefix_help::lines_for(view.mode, &app.query_prefixes);
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
    // ----- Row indicators -----
    //
    // A static reference (not scoped to the currently-active mode,
    // same as every other section here) explaining the passive
    // glyph columns in the row list — none of them are
    // self-explanatory, and since each is now only shown in the
    // mode(s) where it carries real information (see `render_row`'s
    // `mark_span`/`capture_span`/`tmux_span`/`show_exit_marker`
    // gates), a column simply not appearing in the current mode is
    // itself something a user might reasonably wonder about.
    lines.push(Line::from(vec![Span::styled(
        "Row indicators",
        Style::default().add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![
        Span::styled("  [x]        ", dim),
        Span::styled(
            "marked for a bulk action (Ctrl-X toggles) — history, output, files, todo, and JIRA mode only",
            accent,
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  o / .      ", dim),
        Span::styled(
            "captured output available (Ctrl-L to view) — history mode only",
            accent,
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  T / .      ", dim),
        Span::styled(
            "a live tmux/herdr pane already exists there — # Directories and ~ Zoxide mode only",
            accent,
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  ✓ / ✗ / ~  ", dim),
        Span::styled(
            "exit status (✓ success / ✗ failure); ✓/✗ mean closed/open in JIRA mode, ~ marks an LLM/Question preview that hasn't run — history, output, llm, question, and JIRA mode only",
            accent,
        ),
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
    // `C-n` / `C-p` are claimed by per-mode
    // query-history recall (PreviousHistory /
    // NextHistory). Theme cycling was removed in
    // favour of a single Light ↔ Dark toggle (see
    // `ToggleColorScheme` below) and the theme
    // picker (open with `T`); users who want the
    // list-cycling behaviour can still rebind via
    // the command palette (`C-q` → "theme").
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
    row(
        &mut lines,
        binding_for(Action::ToggleColorScheme),
        "toggle between light and dark color scheme (re-resolves the active theme from the config file's `theme.light=` / `theme.dark=` slots)",
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
        "context dive: & / $ opens callers/callees; - opens the JIRA issue in the browser (background); ! toggles the selected todo's checkbox; / opens the selected file via the per-extension command from `smart-open.<ext>` in the config; else selects the row",
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
        "panes: show only the `# Directories` block",
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
    row(
        &mut lines,
        binding_for(Action::CreateNote),
        "open the two-field create-note dialog (Title + Content with `@` / `#` completion; Ctrl-S saves to the daily note's Yournal section, Ctrl-O saves then opens the note in $EDITOR)",
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
        "list every pane across all tmux sessions / herdr workspaces (organized as a per-session / per-workspace tree with the panes indented underneath; each pane row carries a [label] badge showing its session / workspace; the filter is group-aware: a match on the workspace label keeps the whole workspace, a match on a pane keeps the pane and its parent header); Enter on the Sessions/Directories/hosts group headers collapses/expands them (▾/▸)",
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
    mode_row(
        &mut lines,
        "paperless",
        qp.paperless.to_string(),
        "search a Paperless-ngx backend by title (bare words), tag (#TAG), or correspondent (@AUTHOR); Tab completes tag/correspondent names; needs paperless.url + paperless.token",
    );
    mode_row(
        &mut lines,
        "browser",
        qp.browser.to_string(),
        "search browser bookmarks + history, merged from every configured/auto-detected Chrome, Firefox, or Safari profile; type `bookmark` or `history` to narrow to one source; Enter opens the URL",
    );
    mode_row(
        &mut lines,
        "zoxide",
        qp.zoxide.to_string(),
        "list directories from the local zoxide database (highest frecency score first); Enter creates a new tmux session / herdr workspace there, or jumps to an already-active pane there (same staging as directories mode)",
    );
    mode_row(
        &mut lines,
        "processes",
        qp.processes.to_string(),
        "list running OS processes (macOS + Linux, all users); the preview shows cwd/exe/environment; Enter opens a confirm dialog to send a signal (defaults to SIGTERM, Tab/Shift-Tab cycles SIGKILL/SIGHUP/SIGINT)",
    );
    mode_row(
        &mut lines,
        "worktree",
        qp.worktree.to_string(),
        "list git worktrees for the repo containing the current directory; Enter stages `cd <path>` (same staging as directories mode)",
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
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let title = if cancel_keys.is_empty() {
        String::from(" Command palette ")
    } else {
        format!(" Command palette — {} to close ", cancel_keys)
    };
    let inner = overlay(f, &title, 70, 70);

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);

    // Split the inner area into:
    //   [0] query input (1 line)
    //   [1] action list  (everything else)
    //   [2] footer (1 line)
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
    // Three columns: key binding, description (display name — wrapped
    // when it doesn't fit), internal action name (config key). Key/name
    // column widths are sized once from the longest value across EVERY
    // action (not just the filtered/visible ones) so they don't jitter
    // narrower/wider as the user types a filter or scrolls; Description
    // gets whatever's left.
    let filtered = menu.filtered_indices();
    let highlight_style = Style::default().bg(Theme::selection_color()).fg(fg).add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(Theme::dim_color());
    let accent_style = Theme::accent();
    let warning_style = Style::default().fg(Theme::warning_color());

    fn key_text(app: &App, action: Action) -> String {
        if app.bindings.is_unbound(action) {
            "unbound".to_string()
        } else {
            let specs = app.bindings.specs(action);
            if specs.is_empty() {
                "?".to_string()
            } else {
                format_key_specs(specs)
            }
        }
    }

    let key_width = ALL_ACTIONS
        .iter()
        .map(|a| key_text(app, *a).chars().count())
        .max()
        .unwrap_or(0)
        .max("key".len());
    let name_width = ALL_ACTIONS
        .iter()
        .map(|a| a.config_key().chars().count())
        .max()
        .unwrap_or(0)
        .max("action".len());
    // 2 gaps (description↔key, key↔name) at 1 column each —
    // `Table`'s default column spacing.
    let desc_width = (chunks[1].width as usize)
        .saturating_sub(key_width + name_width + 2)
        .max(1);

    let mut rows: Vec<Row> = Vec::new();
    for &idx in &filtered {
        let action = menu.actions[idx];
        let key = key_text(app, action);
        let key_style = if app.bindings.is_unbound(action) { warning_style } else { accent_style };

        let desc_chars: Vec<char> = action.display_name().chars().collect();
        let wrapped = wrap_chars_to_rows(&desc_chars, desc_width);
        let row_height = wrapped.len().max(1);

        let mut key_lines: Vec<Line> = vec![Line::styled(key, key_style)];
        let mut name_lines: Vec<Line> = vec![Line::styled(action.config_key(), dim_style)];
        let mut desc_lines: Vec<Line> = Vec::new();
        for (text, _) in &wrapped {
            desc_lines.push(Line::raw(text.clone()));
        }
        while key_lines.len() < row_height {
            key_lines.push(Line::raw(""));
        }
        while name_lines.len() < row_height {
            name_lines.push(Line::raw(""));
        }

        rows.push(
            Row::new(vec![
                Cell::from(Text::from(desc_lines)),
                Cell::from(Text::from(key_lines)),
                Cell::from(Text::from(name_lines)),
            ])
            .height(row_height as u16),
        );
    }
    if rows.is_empty() {
        rows.push(Row::new(vec![Cell::from(Span::styled("(no action matches your query)", dim_style)), Cell::from(""), Cell::from("")]));
    }

    let table = Table::new(
        rows,
        [Constraint::Length(desc_width as u16), Constraint::Length(key_width as u16), Constraint::Length(name_width as u16)],
    )
    .style(Style::default().bg(bg))
    .row_highlight_style(highlight_style);

    let mut table_state = TableState::default();
    if !filtered.is_empty() {
        table_state.select(Some(menu.selected));
    }
    f.render_stateful_widget(table, chunks[1], &mut table_state);

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
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
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
        .highlight_symbol("▌")
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
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
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
        .highlight_symbol("▌")
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
        .title(format!(
            " Theme picker{}  Enter commits / {} ",
            if picker.query.is_empty() {
                String::new()
            } else {
                format!("  [{}/{}]", picker.filtered().len(), picker.themes.len())
            },
            revert_hint
        ))
        .title_style(Theme::accent())
        .border_style(Theme::dim())
        .style(Style::default().bg(bg));
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let inner_horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)].as_ref())
        .split(inner);

    let dim_style = Style::default().fg(Theme::dim_color());
    let _highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD);

    // Split the left column: a 1-line search box on top, then
    // the theme list underneath.
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Fill(1)].as_ref())
        .split(inner_horizontal[0]);

    // ---- Search box (left top) ----
    let search_text = format!("filter: {}", picker.query);
    let search = Paragraph::new(Line::from(vec![
        Span::styled("filter: ", dim_style),
        Span::styled(&picker.query, Style::default().fg(fg)),
    ]))
    .style(Style::default().bg(bg));
    f.render_widget(search, left_chunks[0]);
    // Position the cursor at the end of the query text.
    let cursor_x = left_chunks[0].x + 8 + picker.query.chars().count() as u16;
    let cursor_y = left_chunks[0].y;
    f.set_cursor_position((
        cursor_x.min(left_chunks[0].x + left_chunks[0].width.saturating_sub(1)),
        cursor_y,
    ));

    // ---- Theme list (left bottom) ----
    let _ = search_text; // kept for future debug output

    let highlight_style = Style::default()
        .bg(Theme::selection_color())
        .fg(fg)
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
    let dim_style = Style::default().fg(Theme::dim_color());

    use super::SelectedTheme;
    // The filtered list — the user may have typed a search
    // query that narrows the full `picker.themes` list.
    let filtered: Vec<&SelectedTheme> = picker.filtered();
    let total = filtered.len();

    // Scroll so the selected row stays visible.
    let visible_rows = left_chunks[1].height as usize;
    let start = picker
        .selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(total.saturating_sub(visible_rows));
    let end = (start + visible_rows).min(total);

    let mut items: Vec<ListItem> = Vec::new();
    for (row_pos, theme) in filtered
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        let is_selected = row_pos == picker.selected;
        let is_original = **theme == picker.original;
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
        .highlight_symbol("▌")
        .repeat_highlight_symbol(false);
    let mut list_state = ListState::default();
    if end > start {
        list_state.select(Some(picker.selected.saturating_sub(start)));
    }
    f.render_stateful_widget(list, left_chunks[1], &mut list_state);

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
    f.render_widget(preview, inner_horizontal[1]);
}

/// Render the `Action::KeyBindingsEditor` overlay. Same overall shape
/// as `draw_command_menu` (a 3-column description/key/name table, its
/// column widths computed once over the full `ALL_ACTIONS` list so
/// they don't jitter while filtering) — the difference is the top line
/// switches between the filter box (browsing), a "press a key" banner
/// (capturing), and a conflict-confirmation banner (`pending_conflict`),
/// per `KeyBindingsEditor`'s own doc comment.
fn draw_key_bindings_editor(f: &mut Frame, app: &App, editor: &KeyBindingsEditor) {
    let cancel_keys = format_key_specs(app.bindings.specs(Action::Cancel));
    let title = if cancel_keys.is_empty() {
        String::from(" Key bindings editor ")
    } else {
        format!(" Key bindings editor — {} to close ", cancel_keys)
    };
    let inner = overlay(f, &title, 75, 75);

    let bg = PALETTE.with(|p| p.borrow().bg);
    let fg = PALETTE.with(|p| p.borrow().fg);
    let dim_style = Style::default().fg(Theme::dim_color());
    let warning_style = Style::default().fg(Theme::warning_color());
    let accent_style = Theme::accent();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)].as_ref())
        .split(inner);

    // ---- Top line: filter box, or a capture/conflict banner ----
    if let Some((action, spec, other)) = editor.pending_conflict {
        let line = Line::from(vec![
            Span::styled(
                format!(
                    " {} is also bound to {}. ",
                    other.display_name(),
                    format_key_spec(spec)
                ),
                warning_style,
            ),
            Span::styled("y", accent_style),
            Span::raw("/"),
            Span::styled("Enter", accent_style),
            Span::raw(" bind anyway to "),
            Span::styled(action.display_name(), Style::default().fg(fg)),
            Span::raw(", "),
            Span::styled("n", accent_style),
            Span::raw(format!("/{} try another key", cancel_keys)),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), chunks[0]);
    } else if let Some(action) = editor.capturing {
        let line = Line::from(vec![
            Span::styled(" Press a key to bind to ", dim_style),
            Span::styled(action.display_name(), accent_style),
            Span::styled(format!("… ({} cancels)", cancel_keys), dim_style),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::default().bg(bg)), chunks[0]);
    } else {
        let placeholder = if editor.query.is_empty() {
            Span::styled(
                "Type an action name (e.g. \"cycle\", \"delete\") or a key",
                Style::default().fg(Theme::dim_color()).add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::styled(editor.query.clone(), Style::default().fg(fg))
        };
        let query_line = Line::from(vec![Span::styled("> ", accent_style), placeholder]);
        f.render_widget(
            Paragraph::new(query_line).style(Style::default().bg(bg)).wrap(Wrap { trim: false }),
            chunks[0],
        );
        let prompt_width = "> ".chars().count() as u16;
        let cursor_x = chunks[0].x + prompt_width + editor.query.chars().count() as u16;
        f.set_cursor_position((
            cursor_x.min(chunks[0].x.saturating_add(chunks[0].width).saturating_sub(2)),
            chunks[0].y,
        ));
    }

    // ---- Action list ----
    fn key_text(app: &App, action: Action) -> String {
        if app.bindings.is_unbound(action) {
            "unbound".to_string()
        } else {
            let specs = app.bindings.specs(action);
            if specs.is_empty() {
                "?".to_string()
            } else {
                format_key_specs(specs)
            }
        }
    }

    let filtered = editor.filtered_indices();
    let highlight_style =
        Style::default().bg(Theme::selection_color()).fg(fg).add_modifier(Modifier::BOLD);

    let key_width = ALL_ACTIONS
        .iter()
        .map(|a| key_text(app, *a).chars().count())
        .max()
        .unwrap_or(0)
        .max("key".len());
    let name_width = ALL_ACTIONS
        .iter()
        .map(|a| a.config_key().chars().count())
        .max()
        .unwrap_or(0)
        .max("action".len());
    let desc_width = (chunks[1].width as usize).saturating_sub(key_width + name_width + 2).max(1);

    let mut rows: Vec<Row> = Vec::new();
    for &idx in &filtered {
        let action = ALL_ACTIONS[idx];
        let key = key_text(app, action);
        let key_style = if app.bindings.is_unbound(action) { warning_style } else { accent_style };

        let desc_chars: Vec<char> = action.display_name().chars().collect();
        let wrapped = wrap_chars_to_rows(&desc_chars, desc_width);
        let row_height = wrapped.len().max(1);

        let mut key_lines: Vec<Line> = vec![Line::styled(key, key_style)];
        let mut name_lines: Vec<Line> = vec![Line::styled(action.config_key(), dim_style)];
        let mut desc_lines: Vec<Line> = Vec::new();
        for (text, _) in &wrapped {
            desc_lines.push(Line::raw(text.clone()));
        }
        while key_lines.len() < row_height {
            key_lines.push(Line::raw(""));
        }
        while name_lines.len() < row_height {
            name_lines.push(Line::raw(""));
        }

        rows.push(
            Row::new(vec![
                Cell::from(Text::from(desc_lines)),
                Cell::from(Text::from(key_lines)),
                Cell::from(Text::from(name_lines)),
            ])
            .height(row_height as u16),
        );
    }
    if rows.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(Span::styled("(no action matches your query)", dim_style)),
            Cell::from(""),
            Cell::from(""),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(desc_width as u16),
            Constraint::Length(key_width as u16),
            Constraint::Length(name_width as u16),
        ],
    )
    .style(Style::default().bg(bg))
    .row_highlight_style(highlight_style);

    let mut table_state = TableState::default();
    if !filtered.is_empty() {
        table_state.select(Some(editor.selected));
    }
    f.render_stateful_widget(table, chunks[1], &mut table_state);

    // ---- Footer ----
    let close_hint = if cancel_keys.is_empty() { "no key bound".to_string() } else { format!("{} close", cancel_keys) };
    let footer = Line::from(vec![
        Span::styled(format!(" {}/{} actions", filtered.len(), ALL_ACTIONS.len()), dim_style),
        Span::raw(format!("  up/down move  Enter rebind  Delete unbind  {} ", close_hint)),
    ]);
    f.render_widget(Paragraph::new(footer).style(Style::default().bg(bg)), chunks[2]);
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
        super::CompletionKind::AttrKey => "attribute",
        super::CompletionKind::AttrValue => "attribute value",
        super::CompletionKind::PaperlessTag => "paperless tag",
        super::CompletionKind::PaperlessCorrespondent => "paperless correspondent",
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
        .add_modifier(Modifier::BOLD)
        .add_modifier(Modifier::UNDERLINED);
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
        .highlight_symbol("▌")
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

/// The selected row's 1-based position in natural top-to-bottom
/// reading order, given the raw data index (`0` = newest for
/// every mode except panes, which is already top-to-bottom) and
/// the total row count. Returns `None` when nothing is selected
/// or the list is empty. Used by `draw_list` to build the
/// title's "N/M" suffix and to position the scrollbar thumb —
/// pulled out as a pure function so both agree and so the
/// index-flip math is unit-testable without a live `Frame`.
fn list_display_position(
    selected_data_idx: Option<usize>,
    real_count: usize,
    is_panes: bool,
) -> Option<usize> {
    if real_count == 0 {
        return None;
    }
    selected_data_idx.map(|data_idx| {
        if is_panes {
            data_idx + 1
        } else {
            real_count.saturating_sub(data_idx)
        }
    })
}

/// Given the same `(selected, offset, viewport, total)` inputs
/// ratatui's `List` widget would receive, returns the `[first,
/// last)` half-open range of item indices it will actually paint —
/// i.e. the same window `List::get_items_bounds` computes
/// internally (bottom-anchored at `offset`, then shifted just
/// enough to keep `selected` in view), specialized for the case
/// where every item has a uniform height of 1 line (true here:
/// `render_row` always returns a single `Line`, for every mode).
///
/// `draw_list` used to build a `ListItem` for every row in
/// `merged_rows` before handing them to `List::new` — ratatui's
/// widget only ever *paints* the visible slice, but by then the
/// (expensive: string formatting + styled spans) construction cost
/// for the other thousands of off-screen rows was already paid, on
/// every keystroke. A large notes vault easily indexes tens of
/// thousands of segments (`:` mode), which measured at ~400ms of
/// list-building work per keystroke — the actual dominant cost
/// behind reports of `:` mode typing lag, well past the (already
/// debounced, backgrounded) search itself. Calling this function
/// first and only constructing `ListItem`s for `[first, last)`
/// fixes that without changing what ends up on screen.
///
/// Verified against the real widget in `tests::list_visible_window_matches_real_ratatui_scroll`.
fn list_visible_window(
    selected: Option<usize>,
    offset: usize,
    viewport: usize,
    total: usize,
) -> (usize, usize) {
    if total == 0 || viewport == 0 {
        return (0, 0);
    }
    let offset = offset.min(total - 1);
    let mut first = offset;
    let mut last = (offset + viewport).min(total);
    if let Some(sel) = selected {
        let sel = sel.min(total - 1);
        if sel >= last {
            last = sel + 1;
            first = last.saturating_sub(viewport);
        } else if sel < first {
            first = sel;
            last = (first + viewport).min(total);
        }
    }
    (first, last)
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let merged = app.merged_rows();

    // Rows are stored newest-first; for display we want oldest at
    // the top and newest at the bottom, so the CONCEPTUAL (padding
    // + reversed) list is what "rendered index" coordinates below
    // refer to. We no longer materialize a `ListItem` for every
    // entry in it up front — only for whichever slice of it
    // `list_visible_window` says will actually be painted (see
    // that function's doc comment for why).
    //
    // **Panes mode (`*`) is different.** The rows are produced by
    // `fetch_session_panes_impl` as a tree: `workspace` header row,
    // then its `pane` child rows, then the next workspace header,
    // etc. The data order IS the display order — the tree must read
    // top-to-bottom (header, then the panes it owns). Reversing it
    // would put a workspace's pane rows ABOVE that workspace's header,
    // which destroys the visual grouping. So panes mode skips the
    // reversal and treats the rows as already display-ordered.
    let is_panes = app.is_panes_query();
    let real_count = merged.len();

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
    let pad = if is_panes {
        0
    } else {
        visible_height.saturating_sub(real_count)
    };
    let total_conceptual = pad + real_count;

    // The stored selection is in data coordinates (0 = newest).
    // Map it to the rendered list coordinates where the newest item
    // is the last real item.
    //
    // **Panes mode**: data index IS the rendered index (0 = first
    // row at the top). No flip.
    let rendered_idx = if is_panes {
        app.list_state.selected()
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
    let anchor_offset = if is_panes {
        0
    } else if real_count >= visible_height {
        // Anchor at the bottom: offset = real_count - visible_height.
        // This positions the newest entry at the bottom row and leaves
        // older entries visible above as the user scrolls up.
        real_count.saturating_sub(visible_height)
    } else {
        0
    };

    // The actual window of conceptual indices ratatui's `List`
    // would paint, given the anchor offset above and wherever the
    // selection currently is (which may be scrolled well outside
    // the anchor, e.g. the user pressed `Up` repeatedly through a
    // long list). Building `ListItem`s for just this slice —
    // instead of all `total_conceptual` entries — is the whole
    // point of `list_visible_window`.
    let (first_visible, last_visible) =
        list_visible_window(rendered_idx, anchor_offset, visible_height, total_conceptual);

    // `tui.highlight`: batch-fill `command_highlight_cache` for any
    // not-yet-cached command text in the visible window, BEFORE
    // building `ListItem`s below — one `highlight_bash_commands`
    // call for potentially many rows, not one call per row (see
    // `App::command_highlight_cache`'s doc comment for why that
    // matters: this file redraws unconditionally on every ~100ms
    // tick). Only `mode = "command"` rows are real bash text worth
    // highlighting.
    //
    // `merged`'s borrow must end before `fill_command_highlight_cache`
    // (which needs `&mut app`) runs — scoped in its own block so its
    // last use is the `missing` collection below, then `merged` is
    // re-borrowed fresh afterward for `age_width`/`items`.
    if app.tui_highlight_enabled {
        let missing: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            (first_visible..last_visible)
                .filter(|&i| i >= pad)
                .filter_map(|i| {
                    let data_idx = if is_panes {
                        i - pad
                    } else {
                        real_count.saturating_sub(1) - (i - pad)
                    };
                    merged.get(data_idx)
                })
                .filter(|r| r.mode == "command")
                .map(|r| r.command.replace('\n', "↵").replace('\r', ""))
                .filter(|cmd| {
                    !command_highlight_cached(app, cmd) && seen.insert(cmd.clone())
                })
                .collect()
        };
        if !missing.is_empty() {
            fill_command_highlight_cache(app, &missing);
        }
    }
    let merged = app.merged_rows();

    // `age_width` only needs to line up within what's actually on
    // screen, so derive it from the visible window instead of
    // scanning every row.
    let age_width = (first_visible..last_visible)
        .filter(|&i| i >= pad)
        .filter_map(|i| {
            let data_idx = if is_panes {
                i - pad
            } else {
                real_count.saturating_sub(1) - (i - pad)
            };
            merged.get(data_idx)
        })
        .map(|r| format_diff(r.timestamp).chars().count())
        .max()
        .unwrap_or(3)
        .max(3);

    let items: Vec<ListItem> = (first_visible..last_visible)
        .map(|i| {
            if i < pad {
                ListItem::new("")
            } else {
                let data_idx = if is_panes {
                    i - pad
                } else {
                    real_count.saturating_sub(1) - (i - pad)
                };
                let r = &merged[data_idx];
                let is_selected = app.list_state.selected() == Some(data_idx);
                ListItem::new(render_row(r, app, is_selected, age_width))
            }
        })
        .collect();

    // `items` is already exactly the visible slice, so the state we
    // hand to the widget starts at offset 0 with the selection
    // shifted to be relative to `first_visible`.
    let mut render_state = ListState::default().with_offset(0);
    render_state.select(rendered_idx.map(|ri| ri.saturating_sub(first_visible)));

    // The list title is mode-dependent. The
    // historical "History" label is kept for the
    // no-prefix history mode (users have been
    // reading that for years); every other mode
    // gets a title-case noun from
    // `ModeKind::list_title()` so the user always
    // knows which view they're looking at. The
    // row count is appended after an em-dash so
    // the title is `<Mode> — <count>` for every
    // mode (e.g. "Notes — 42", "JIRA — 5",
    // "Directories — 12"). The user's
    // `pane_visibility` choice (Both / Details /
    // Output) is unrelated to this title — the
    // title reflects the data source, not the
    // pane layout.
    let active_mode = crate::tui::mode::active_mode(app);
    // Selected row's 1-based position in NATURAL top-to-bottom
    // reading order (oldest-at-top for the history list, or
    // tree order for panes mode) — NOT the raw data index, which
    // is newest-first (0 = newest) for every mode except panes.
    // `app.list_state.selected()` still holds the PREVIOUS
    // frame's data index at this point (it's only overwritten
    // at the end of this function), so this reads the position
    // the user is currently looking at.
    let selected_display_pos =
        list_display_position(app.list_state.selected(), real_count, is_panes);
    // A locked `--glob-complete-dir` picker still runs on the files
    // (`/`) prefix internally (see `FilePickerKind`), so
    // `ModeKind::list_title()` would otherwise say "Files" even
    // though only directory rows are shown — override the label to
    // match what's actually on screen.
    let list_title_label = if app.is_directory_picker() {
        "Directories"
    } else {
        active_mode.list_title()
    };
    let title = match selected_display_pos {
        Some(pos) if real_count > 0 => format!(
            " {} — {}/{} ",
            list_title_label,
            pos,
            merged.len()
        ),
        _ => format!(
            " {} — {} ",
            list_title_label,
            merged.len()
        ),
    };
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
            // Selected row: the theme's `selection` color
            // for the background, the theme's own `fg` color
            // (not `highlight_color`) for the foreground so
            // the text always has maximum contrast with the
            // theme-designed selection background. Bold +
            // underline + the `▌` left-edge bar add
            // visual weight without relying on color
            // contrast alone (important on light themes
            // where the selection background is close to
            // the app background).
            Style::default()
                .bg(Theme::selection_color())
                .fg(PALETTE.with(|p| p.borrow().fg))
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

    // Scrollbar, only when there's actually something to scroll —
    // an always-visible scrollbar on a list that fits entirely on
    // screen would just be visual noise. Tracks the SAME
    // natural-order position as the title's "N/M" indicator (not
    // ratatui's internal render offset, which is in padded/
    // reversed coordinates for history mode — see the comment on
    // `selected_display_pos` above), so the thumb position always
    // agrees with the title.
    if real_count > visible_height {
        let mut scrollbar_state = ratatui::widgets::ScrollbarState::new(real_count)
            .position(selected_display_pos.map(|p| p - 1).unwrap_or(0))
            .viewport_content_length(visible_height);
        let scrollbar = ratatui::widgets::Scrollbar::new(
            ratatui::widgets::ScrollbarOrientation::VerticalRight,
        )
        .style(Theme::dim());
        f.render_stateful_widget(
            scrollbar,
            area.inner(ratatui::layout::Margin::new(0, 1)),
            &mut scrollbar_state,
        );
    }

    // Map the (pre-render, full conceptual-coordinate) selection
    // back into app.list_state in data coordinates. `rendered_idx`
    // rather than `render_state.selected()` (which is relative to
    // `first_visible`, not the full conceptual list) since ratatui
    // doesn't itself mutate `.selected()` during render — only
    // `.offset()`, which this function no longer even reads back
    // (see `list_visible_window`'s doc comment).
    let data_idx = rendered_idx.and_then(|ri| {
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

/// The active color scheme's light/dark classification, read from
/// the same thread-local `PALETTE` this app's theme system already
/// tracks — used to pick between `syntect`'s bundled
/// `base16-ocean.light`/`base16-ocean.dark` themes
/// (`crate::highlight::highlight_bash_commands`) so
/// `command_highlight_cache`'s key always matches the colors that
/// were (or will be) rendered.
fn command_highlight_is_light() -> bool {
    crate::tui::theme::palette_storage::PALETTE.with(|p| p.borrow().is_light_theme)
}

/// Whether `cmd` (an already `cmd_display`-transformed, single-line
/// command string) has a cached `tui.highlight` entry for the
/// CURRENT color scheme.
fn command_highlight_cached(app: &App, cmd: &str) -> bool {
    app.command_highlight_cache
        .contains_key(&(command_highlight_is_light(), cmd.to_string()))
}

/// Batch-fill `App::command_highlight_cache` for every entry in
/// `commands` (assumed already deduplicated and not-yet-cached by
/// the caller — see `draw_list`'s call site) via
/// `crate::highlight::highlight_bash_commands`, converting its
/// `HighlightedSpan`s straight into `ratatui::text::Span`s — no
/// subprocess, no ANSI text to parse. Always succeeds (that
/// function has no "external tool missing" failure mode to handle),
/// so unlike the old `bat`-based version there's no fallback branch
/// here.
fn fill_command_highlight_cache(app: &mut App, commands: &[String]) {
    let is_light = command_highlight_is_light();
    let refs: Vec<&str> = commands.iter().map(|s| s.as_str()).collect();
    let highlighted = crate::highlight::highlight_bash_commands(&refs, is_light);
    for (cmd, tokens) in commands.iter().zip(highlighted) {
        let spans: Vec<Span<'static>> = tokens
            .into_iter()
            .map(|t| {
                let mut style = Style::default().fg(ratatui::style::Color::Rgb(
                    t.color.0, t.color.1, t.color.2,
                ));
                if t.bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if t.italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if t.underline {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                Span::styled(t.text, style)
            })
            .collect();
        app.command_highlight_cache
            .insert((is_light, cmd.clone()), spans);
    }
}

/// Render a single history row as a `Line` with optional query
/// highlighting. The layout is a fixed-width columnar form:
///
///   [age] [status]  command  ·  time
///
/// `age_width` is the right-aligned width of the age column so rows
/// line up.
pub(crate) fn render_row<'a>(
    row: &'a HistoryRow,
    app: &App,
    is_selected: bool,
    age_width: usize,
) -> Line<'a> {
    // Which prefix mode is active — used below to hide the two
    // fixed-width indicator columns (capture / tmux-pane) in modes
    // where they never carry any information, rather than always
    // reserving their column width with a dim `.` placeholder no
    // mode but the relevant one ever lights up.
    let active_mode = crate::tui::mode::active_mode(app);

    let age = format_diff(row.timestamp);
    let age_padded = format!("{:>age_width$}", age);
    // Color the age column by recency, brightest for "just happened"
    // fading to dim for old entries — a glanceable freshness gradient
    // on top of the existing text, not a replacement for it (the
    // exact age is still spelled out either way). `format_diff`'s
    // unit ladder (seconds -> minutes -> hours -> days -> months,
    // largest non-zero unit wins) already IS a bucket boundary, so
    // reading the trailing unit letter off the already-computed
    // `age` string is enough — no need to re-derive elapsed time
    // from `row.timestamp` a second time. The "9999M" sentinel
    // (`format_diff`'s placeholder for a zero/invalid/out-of-range
    // timestamp — synthetic rows like a `directory`/`session` entry
    // with `timestamp: 0`) falls into the same 'M' (months) bucket
    // as genuinely old entries, which is the right visual outcome
    // either way: dimmest.
    let age_style = match age.chars().last() {
        Some('s') => Theme::highlight(), // < 1 minute: brightest
        Some('m') => Theme::success(),   // < 1 hour
        Some('h') => Theme::accent(),    // < 1 day (previous flat default)
        Some('d') => Theme::dim(),       // < 1 month
        _ => Theme::dimmer(),            // 1+ months, or the "9999M" sentinel
    };

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
    // The exit-status column (`✓`/`✗`, or `~` for an LLM preview) is
    // only shown in modes whose rows can carry a genuinely varying
    // `exit_code` — the shared history table (`History`, `Output`,
    // and the `Llm`/`Question` modes, which mix a synthetic preview
    // row in alongside any matching real history rows — see
    // `build_merged_rows`'s `preview_part` handling) and `Jira`
    // (which repurposes `exit_code` as a closed/open sentinel, not
    // literally command success — see the mapping in
    // `tui/mode/jira.rs`). Every other mode hardcodes `exit_code: 0`
    // for every row it can ever produce (a directory, a note, a
    // file, a pane, …), so the marker would always be the identical
    // `✓` — zero discriminating information, just visual noise.
    let show_exit_marker = matches!(
        active_mode,
        crate::tui::mode::ModeKind::History
            | crate::tui::mode::ModeKind::Output
            | crate::tui::mode::ModeKind::Llm
            | crate::tui::mode::ModeKind::Question
            | crate::tui::mode::ModeKind::Jira
    );
    let (exit_marker, exit_style) = if row.is_llm_preview() {
        ("~", Theme::accent())
    } else if row.exit_code == 0 {
        ("✓", Theme::success())
    } else {
        ("✗", Theme::error())
    };

    // Capture indicator. A bright `o ` shows the row has captured
    // output available (press ^L to view); a dim `. ` is shown
    // otherwise so columns stay aligned. Only meaningful in plain
    // history mode (`ModeKind::History`) — every other prefix mode
    // either never populates `row.output` with captured command
    // output at all, or repurposes the field for something else
    // entirely (e.g. ag mode's source-context preview), so the
    // column is hidden outright there rather than showing a
    // permanently-dim (or, worse, misleadingly "lit") placeholder.
    let capture_span = if active_mode != crate::tui::mode::ModeKind::History {
        Span::raw("")
    } else if !row.output.is_empty() {
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
    // at any given moment. Directory rows only ever appear in `#`
    // (Directories) and `~` (Zoxide) mode — the column is hidden in
    // every other mode (including `*` Panes, whose own rows already
    // ARE the live panes, so a redundant "does a pane exist here"
    // marker would never have anything useful to say there either).
    let is_directory_flavored_mode = matches!(
        active_mode,
        crate::tui::mode::ModeKind::Directories
            | crate::tui::mode::ModeKind::Zoxide
            | crate::tui::mode::ModeKind::Worktree
    );
    let tmux_span = if !is_directory_flavored_mode {
        Span::raw("")
    } else if row.mode == "directory" && app.directory_tmux_pane_id(&row.directory).is_some() {
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

    // Multi-select checkbox marker. A highlighted `[x]` shows the
    // row is in `app.marked_ids` (toggled via `Action::ToggleMark`,
    // `C-x` by default); a dim `[ ]` otherwise, matching the
    // fixed-width-placeholder convention `capture_span`/`tmux_span`
    // already use so columns stay aligned whether or not anything
    // is marked.
    //
    // Only shown in the modes where marking actually DOES
    // something: `Action::BulkDeleteMarked` deletes by real SQL
    // `history.id`, which only exists for `History`/`Output` rows
    // (every other mode's synthetic negative id matches zero rows —
    // see `delete_marked`'s own doc comment). Everywhere else,
    // marking only has an effect through a mode's own
    // `smart_action_targets()`-aware `SmartOpen` handler — and there
    // are exactly three: `smart_open_for_file` (Files), `mark_todo_done`
    // (Todo), `open_jira_in_background` (Jira). Every other mode's
    // `Action::SmartOpen` arm (see the dispatch in `handle_key`) acts
    // on the single selected row only and never reads `marked_ids` —
    // so in those modes, `[x]` would be a checkbox nothing ever
    // consults. Hidden there entirely rather than shown as inert
    // decoration.
    // A locked `--pid-complete` process picker is the one addition
    // to this list that ISN'T keyed off `active_mode` alone: normal,
    // unlocked `%` (Processes) mode marking still does nothing (its
    // `SmartOpen` wildcard dispatch never reads `marked_ids`), but
    // inside `process_picker_lock`, `Ctrl-A`/Enter (see `handle_key`)
    // make it fully meaningful — `kill` can legitimately target more
    // than one PID at once.
    let marking_has_effect = matches!(
        active_mode,
        crate::tui::mode::ModeKind::History
            | crate::tui::mode::ModeKind::Output
            | crate::tui::mode::ModeKind::Files
            | crate::tui::mode::ModeKind::Todo
            | crate::tui::mode::ModeKind::Jira
    ) || app.process_picker_lock.is_some();
    let mark_span = if !marking_has_effect {
        Span::raw("")
    } else if app.marked_ids.contains(&mark_key(row)) {
        Span::styled(
            "[x]",
            Style::default()
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("[ ]", Theme::dim())
    };

    let mut spans = vec![
        mark_span,
        capture_span,
        tmux_span,
        llm_preview_span,
        Span::styled(format!(" {} ", age_padded), age_style),
    ];
    if show_exit_marker {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!(" {} ", exit_marker), exit_style));
        spans.push(Span::raw(" "));
    }

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
    if row.mode == "pane" {
        // Live panes are two levels deep now: `# Sessions` ->
        // `## <workspace>` -> pane. Deeper indent than
        // `session`/`host` rows below, which sit directly under
        // their `Directories`/`hosts` `# `-header with no
        // intermediate level.
        spans.push(Span::raw("    · "));
    } else if row.mode == "session" || row.mode == "host" {
        spans.push(Span::raw("  · "));
    } else if row.mode == "workspace" && row.source == "workspace" {
        // An individual live tmux/herdr workspace — nested one
        // level under the synthetic `# Sessions` header
        // (`insert_sessions_group_header` in `mode/panes.rs`), so
        // it renders as a `## ` sub-heading rather than a
        // top-level `# ` one.
        spans.push(Span::styled(
            "  ## ",
            Style::default()
                .fg(Theme::info_color())
                .add_modifier(Modifier::BOLD),
        ));
    } else if row.mode == "workspace" {
        // Top-level `# ` header: the synthetic `Sessions` wrapper
        // itself (`source == "workspace-group"`), or the
        // `Directories`/`hosts` sections (`source == "sessions"` /
        // `"hosts"`). These three are collapsible — `Enter` toggles
        // `app.collapsed_pane_groups` (see `stage_pane_selection`
        // and `App::toggle_pane_group_collapsed`) instead of staging
        // a focus command, and a leading `▸`/`▾` disclosure triangle
        // shows the current state. Info color (not accent) —
        // distinct from the running-pane marker below, which uses
        // the highlight color. The two used to share a color in some
        // themes, making a busy pane's `▶` marker blend into its
        // workspace header instead of standing out from it.
        let disclosure = if app.collapsed_pane_groups.contains(&row.command) {
            "▸ # "
        } else {
            "▾ # "
        };
        spans.push(Span::styled(
            disclosure,
            Style::default()
                .fg(Theme::info_color())
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
    // `/` (files) mode: `row.command` is a path RELATIVE to the
    // walked root (see `compute_display` in `src/files.rs`), which
    // can still run long for a deeply-nested file and crowd out the
    // filename. Abbreviate it for DISPLAY ONLY, the same way ag mode
    // shortens its (separate) path field — every directory component
    // down to its first character, filename kept in full.
    //
    // Unlike ag mode, Files mode has no separate path/content split:
    // `row.command` IS both the searched text (`src/files.rs`'s own
    // token filter matches against the real, unabbreviated string)
    // and the text `highlight_matches`/`highlight_regex_matches`
    // below highlight query matches in. Deliberately NOT touching
    // the underlying search — only this local `cmd_display` copy is
    // shortened, after the filtering has already happened. The
    // tradeoff: a query match that falls inside an abbreviated-away
    // directory character won't get highlighted (the row still
    // correctly appears in the filtered list either way) — a minor,
    // acceptable cosmetic miss, not a correctness bug. Don't "fix"
    // this by reverting the abbreviation; it's the intended
    // tradeoff, not an oversight. `"directory"`-mode rows (also part
    // of `/` mode's results) are untouched — they already render via
    // their own path/comment-swap convention above.
    let cmd_display = if row.mode == "file" {
        crate::util::shorten_path_dirs(&cmd_display, &[])
    } else {
        cmd_display
    };
    // `,` (ag) mode: put the matched file's path up front, as
    // compactly as possible, before the match content itself —
    // `fetch`/`src/ag.rs` stores the absolute path in `row.directory`
    // and the matched line's content in `row.command`. Every
    // intermediate directory component is abbreviated to its first
    // character (`shorten_path_dirs`); the filename itself is always
    // shown in full, since that's what the user actually needs to
    // recognize which file a match is in. Not query-highlighted —
    // ag's search terms match CONTENT, not the path — so this is a
    // plain styled span, not routed through `highlight_matches`/
    // `highlight_regex_matches` below (those apply to `cmd_display`,
    // the match content, same as for every other mode).
    if row.mode == "ag" && !row.directory.is_empty() {
        let short_path = crate::util::shorten_path_dirs(&row.directory, &app.home_list);
        spans.push(Span::styled(
            format!("{}: ", short_path),
            Style::default()
                .fg(Theme::info_color())
                .add_modifier(Modifier::BOLD),
        ));
    }
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
        // Info color — see the `# ` marker span above for why this
        // isn't accent (kept in sync with it so the marker + label
        // read as one consistently-colored heading).
        spans.push(Span::styled(
            format!("{} ", cmd_display),
            Style::default()
                .fg(Theme::info_color())
                .add_modifier(Modifier::BOLD),
        ));
    } else if row.mode == "pane" && !row.command.is_empty() {
        // A pane actually running something (`current_command` is
        // non-empty — an agent, a build, an editor, anything other
        // than a bare idle shell prompt) gets a much more dominant
        // treatment than the plain text every other row uses: a
        // leading `▶ ` marker plus bold + the highlight color, so a
        // busy pane jumps out immediately when scanning a long `*`
        // panes-mode list. An idle pane (empty `current_command`,
        // `cmd_display` empty) falls through to the plain path below
        // and stays visually quiet — there's nothing running to draw
        // attention to. Bypasses `highlight_matches`/
        // `highlight_regex_matches` (same as the `workspace` branch
        // above) rather than layering search-match bolding on top —
        // those helpers have no base-style parameter, and the running
        // marker is the more important signal here.
        spans.push(Span::styled(
            format!("▶ {} ", cmd_display),
            Style::default()
                .fg(Theme::highlight_color())
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.is_regex_query() {
        let mut text_spans = highlight_regex_matches(&cmd_display, app.query_regex.as_ref());
        if row.mode == "pane" {
            // A pane's name is always bold, even when it doesn't
            // take the dominant `▶`-marker branch above (an idle
            // pane with an empty `current_command`, so there's
            // nothing here to render anyway — this just guarantees
            // the rule holds regardless of how a pane row got here,
            // rather than being an incidental side effect of the
            // running-marker styling).
            for s in &mut text_spans {
                s.style = s.style.add_modifier(Modifier::BOLD);
            }
        }
        spans.extend(text_spans);
    } else {
        // `tui.highlight`: use the cached syntax-highlighted
        // spans instead of the plain/matched-substring rendering,
        // but ONLY for real command rows and ONLY while there's no
        // active search query to emphasize — the moment the user
        // types a search, `highlight_matches`'s bold/colored
        // matched-substring is the more useful signal (which of
        // these rows actually matched, and where), so search takes
        // priority over syntax color rather than trying to compose
        // both onto the same text. `command_highlight_cache` is
        // guaranteed already filled for every row in the visible
        // window by `draw_list`'s batch pre-pass (see its call
        // site), so this is a cache lookup only — never a
        // highlighter call from inside the per-row render path.
        let mut text_spans = if app.tui_highlight_enabled
            && row.mode == "command"
            && app.query.trim().is_empty()
            && let Some(cached) = app
                .command_highlight_cache
                .get(&(command_highlight_is_light(), cmd_display.clone()))
        {
            cached.clone()
        } else {
            highlight_matches(&cmd_display, &app.query)
        };
        if row.mode == "pane" {
            for s in &mut text_spans {
                s.style = s.style.add_modifier(Modifier::BOLD);
            }
        }
        spans.extend(text_spans);
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
    // Skipped entirely for `ag` rows: `row.comment` there is just the
    // match's basename, already shown (in full, as part of the
    // shortened path) in the primary-text prefix above — repeating
    // it here would be pure noise.
    if row.mode != "ag" && !row.comment.is_empty() {
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
    } else if row.mode != "ag" && is_selected {
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

    let text_chars: Vec<char> = text.chars().collect();
    // Precomputed once, indexed by position below — the previous
    // version did `lower_text.chars().skip(i).take(word_chars.len())`
    // *inside* the position loop, which re-walks the string from its
    // start on every `i` (a `&str`'s `Chars` iterator has no
    // random-access skip): O(n) per position, O(n²) overall. For a
    // short shell command that's invisible; for a segments-mode row
    // (`row.command` can be an entire flattened note section, tens of
    // thousands of characters) it's a multi-second stall on every
    // frame the row is visible — reproduced live via
    // `SMARTHISTORY_DEBUG_PERF`, which pinned an 11.9s `draw_list`
    // stall to exactly this call.
    let lower_chars: Vec<char> = text.to_lowercase().chars().collect();
    // `to_lowercase()` can change the character count for a handful
    // of code points (e.g. Turkish İ folds to two chars), which would
    // desync `highlights` indices from `text_chars` below. Bail to
    // unhighlighted plain text for those rare inputs rather than risk
    // a misaligned highlight or an out-of-bounds index.
    if lower_chars.len() != text_chars.len() {
        return vec![Span::raw(text.to_string())];
    }
    let mut highlights = vec![false; text_chars.len()];

    for word in words {
        let word_chars: Vec<char> = word.chars().collect();
        if word_chars.is_empty() || word_chars.len() > lower_chars.len() {
            continue;
        }
        let mut i = 0;
        while i + word_chars.len() <= lower_chars.len() {
            if lower_chars[i..i + word_chars.len()] == word_chars[..] {
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
            //
            // Use a foreground-only style
            // (no background) so the
            // paragraph's own
            // `.bg(details_bg)` / `.bg(list_bg)`
            // is the final authority on the
            // cell background. Using
            // `Theme::default()` here would
            // override the pane's background
            // with the app's main `bg`, which
            // produces a visually wrong
            // background when the user has
            // a `tuicolor.detailsbg=` setting
            // or a theme with a different
            // details-bg color.
            let base = Style::default().fg(
                PALETTE.with(|c| c.borrow().fg),
            );
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
    // Foreground-only base style (no
    // background) so the paragraph's
    // own `.bg(details_bg)` is the
    // final authority on the cell
    // background. Using
    // `Theme::default()` here would
    // override the pane's background
    // with the app's main `bg`, which
    // produces a visually wrong
    // background when the user has a
    // `tuicolor.detailsbg=` setting or
    // a theme with a different
    // details-bg color.
    let base = Style::default().fg(
        PALETTE.with(|c| c.borrow().fg),
    );
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
                // Unclosed marker. Render the rest of the line
                // (including the literal marker) as plain text —
                // reconstructed from `marker_char`/`marker_len` (the
                // ACTUAL character(s) that opened it), not a
                // hardcoded-per-kind string: `MarkerKind::Italic`
                // covers both `*` and `_`, so a fixed "`*`-means-
                // italic" spelling would silently rewrite an
                // unclosed `_` into a `*`. A plain directory listing
                // or file path containing a bare underscore (e.g.
                // `alpha_sub/`, never intended as markdown at all)
                // would otherwise render with its underscore
                // silently swapped for an asterisk.
                let literal_marker: String = std::iter::repeat_n(marker_char, marker_len).collect();
                push_plain_span(
                    &mut spans,
                    format!("{}{}", literal_marker, after_open),
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

/// Parse a single line containing ANSI escape sequences into
/// ratatui `Span`s with the appropriate foreground / background
/// colors and modifiers.
///
/// Supported SGR (Select Graphic Rendition) parameter sets:
///
/// - **Reset / modifiers**: `0` (reset all), `1` (bold),
///   `2` (dim), `3` (italic), `4` (underline), `5` (blink),
///   `7` (reverse video), `8` (hidden), `9` (strikethrough).
///   `22` / `23` / `24` / `27` / `28` / `29` are the
///   corresponding "disable" codes.
/// - **8-color fg / bg**: `30..37` (basic fg), `40..47`
///   (basic bg), `90..97` (bright fg), `100..107` (bright bg).
///   `39` = default fg, `49` = default bg.
/// - **256-color fg / bg**: `38;5;N` (fg), `48;5;N` (bg).
///   `N` is the 256-color palette index (0..=255), looked up
///   in `xterm256_to_rgb`.
/// - **Truecolor fg / bg**: `38;2;R;G;B` (fg), `48;2;R;G;B`
///   (bg). `R` / `G` / `B` are 0..=255 decimal values.
/// - **Colon separator**: the same `38:2:R:G:B` / `48:2:R:G:B`
///   / `38:5:N` forms with `:` instead of `;`. Some terminals
///   (and herdr) emit this variant.
/// - **Multi-parameter sequences**: `\x1b[1;31m` (bold + red)
///   is applied left-to-right. `\x1b[0m` resets all attributes.
///
/// Anything outside this set is silently dropped (the
/// existing style is preserved) — this is intentional, so
/// unknown codes don't accidentally clear colors set by
/// earlier codes in the same line.
///
/// The parser is shared between the inline output preview
/// (`draw_output_preview`) and the full-screen
/// `draw_output_view` overlay. Both code paths detect ANSI
/// via `preview_text.contains('\x1b')` and route through
/// here when present.
fn parse_ansi_line(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current_text = String::new();
    let mut current_style = Style::default();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            // Walk to the next alphabetic byte (the SGR
            // command character, always `m` for SGR but
            // we tolerate other CSI terminators by just
            // skipping to the next `m`). We also stop on
            // the private-use characters (`<` / `=` / `>`
            // / `?`) so we don't accidentally consume
            // past the end of an unsupported CSI.
            let mut params = String::new();
            let mut cmd = '\0';
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_alphabetic() {
                    cmd = ch;
                    chars.next();
                    break;
                }
                // `:` is a valid separator within a
                // parameter list (used by herdr and
                // some terminals for truecolor). The
                // existing parser only kept `;` /
                // digits; the new one also keeps `:` so
                // `38:2:R:G:B` parses correctly. We
                // normalize `:` to `;` later so the
                // downstream code can treat both forms
                // uniformly.
                if ch == ':' {
                    params.push(';');
                    chars.next();
                    continue;
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

/// Apply one SGR parameter set (e.g. `"0"`, `"1;31"`,
/// `"38;2;131;148;150"`, `"38:2:131:148:150"`) to a base
/// style, returning the updated style. Empty / unset
/// parameter (`\x1b[m`) is treated as a reset (`0`),
/// matching the standard's behavior.
///
/// Parameters are applied LEFT-TO-RIGHT, in order. A
/// `\x1b[0;1;31m` sequence resets, then enables bold, then
/// sets the foreground to red. This is the
/// "cumulative, no implicit reset" interpretation that
/// every modern terminal follows.
///
/// Truecolor / 256-color parameters consume the next
/// 2-3 sub-parameters (so the parser looks ahead in the
/// `parts` slice). An out-of-range value is silently
/// ignored — the existing style is preserved rather
/// than clobbered.
fn apply_ansi_sgr(params: &str, style: Style) -> Style {
    // Normalize the `:` separator form to `;` so the
    // downstream parsing can treat both forms uniformly.
    // `38:2:131:148:150` becomes `38;2;131;148;150`.
    let normalized = params.replace(':', ";");
    let parts: Vec<&str> = normalized.split(';').collect();
    if parts.is_empty() {
        return style;
    }
    let mut style = style;
    let mut i = 0;
    while i < parts.len() {
        let part = parts[i];
        // An empty parameter between separators (e.g.
        // `\x1b[;31m`) is treated as a 0 (reset) by
        // xterm; we match that.
        let code = if part.is_empty() { "0" } else { part };
        let code_int = code.parse::<u16>().unwrap_or(0);
        match code_int {
            // Reset
            0 => style = Style::default(),
            // Modifiers (enable)
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 => {
                // Blink is not representable in
                // ratatui; the closest we can get
                // is BOLD (visually distinct but
                // not "flashing"). Skip silently
                // to avoid misrepresenting the
                // source.
            }
            7 => style = style.add_modifier(Modifier::REVERSED),
            8 => {
                // Hidden: render as DIM so the
                // text is still visible (a
                // truly invisible preview is
                // worse than a dim one).
                style = style.add_modifier(Modifier::DIM);
            }
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            // Modifiers (disable)
            22 => style = style.remove_modifier(Modifier::DIM | Modifier::BOLD),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            28 => {
                // Hidden off: nothing to do
                // (we mapped Hidden to DIM on
                // enable; don't remove DIM
                // here, the user may have
                // set it explicitly).
            }
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            // Basic 8-color foreground
            30 => style = style.fg(ratatui::style::Color::Black),
            31 => style = style.fg(ratatui::style::Color::Red),
            32 => style = style.fg(ratatui::style::Color::Green),
            33 => style = style.fg(ratatui::style::Color::Yellow),
            34 => style = style.fg(ratatui::style::Color::Blue),
            35 => style = style.fg(ratatui::style::Color::Magenta),
            36 => style = style.fg(ratatui::style::Color::Cyan),
            37 => style = style.fg(ratatui::style::Color::White),
            38 => {
                // 38;5;N OR 38;2;R;G;B
                let next = parts.get(i + 1).copied().unwrap_or("");
                if next == "5" {
                    if let Some(n) = parts
                        .get(i + 2)
                        .and_then(|s| s.parse::<u8>().ok())
                    {
                        let (r, g, b) = xterm256_to_rgb(n);
                        style = style.fg(ratatui::style::Color::Rgb(r, g, b));
                        i += 2;
                    }
                } else if next == "2" {
                    let r = parts
                        .get(i + 2)
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(0);
                    let g = parts
                        .get(i + 3)
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(0);
                    let b = parts
                        .get(i + 4)
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(0);
                    style = style.fg(ratatui::style::Color::Rgb(r, g, b));
                    i += 4;
                }
            }
            39 => style = style.fg(Color::Reset),
            // Basic 8-color background
            40 => style = style.bg(ratatui::style::Color::Black),
            41 => style = style.bg(ratatui::style::Color::Red),
            42 => style = style.bg(ratatui::style::Color::Green),
            43 => style = style.bg(ratatui::style::Color::Yellow),
            44 => style = style.bg(ratatui::style::Color::Blue),
            45 => style = style.bg(ratatui::style::Color::Magenta),
            46 => style = style.bg(ratatui::style::Color::Cyan),
            47 => style = style.bg(ratatui::style::Color::White),
            48 => {
                // 48;5;N OR 48;2;R;G;B
                let next = parts.get(i + 1).copied().unwrap_or("");
                if next == "5" {
                    if let Some(n) = parts
                        .get(i + 2)
                        .and_then(|s| s.parse::<u8>().ok())
                    {
                        let (r, g, b) = xterm256_to_rgb(n);
                        style = style.bg(ratatui::style::Color::Rgb(r, g, b));
                        i += 2;
                    }
                } else if next == "2" {
                    let r = parts
                        .get(i + 2)
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(0);
                    let g = parts
                        .get(i + 3)
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(0);
                    let b = parts
                        .get(i + 4)
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(0);
                    style = style.bg(ratatui::style::Color::Rgb(r, g, b));
                    i += 4;
                }
            }
            49 => style = style.bg(Color::Reset),
            // Bright 8-color foreground
            90 => style = style.fg(ratatui::style::Color::DarkGray),
            91 => style = style.fg(ratatui::style::Color::LightRed),
            92 => style = style.fg(ratatui::style::Color::LightGreen),
            93 => style = style.fg(ratatui::style::Color::LightYellow),
            94 => style = style.fg(ratatui::style::Color::LightBlue),
            95 => style = style.fg(ratatui::style::Color::LightMagenta),
            96 => style = style.fg(ratatui::style::Color::LightCyan),
            97 => style = style.fg(ratatui::style::Color::White),
            // Bright 8-color background
            100 => style = style.bg(ratatui::style::Color::DarkGray),
            101 => style = style.bg(ratatui::style::Color::LightRed),
            102 => style = style.bg(ratatui::style::Color::LightGreen),
            103 => style = style.bg(ratatui::style::Color::LightYellow),
            104 => style = style.bg(ratatui::style::Color::LightBlue),
            105 => style = style.bg(ratatui::style::Color::LightMagenta),
            106 => style = style.bg(ratatui::style::Color::LightCyan),
            107 => style = style.bg(ratatui::style::Color::White),
            _ => {
                // Unknown SGR code: ignore
                // silently. The existing
                // style is preserved so we
                // don't accidentally
                // clobber a color set by
                // an earlier code in the
                // same line.
            }
        }
        i += 1;
    }
    style
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

    // The preview text source. Most modes populate
    // `row.output` (which has dual duty: it's also
    // the captured output of history commands and the
    // tab_id of pane rows). The newer `row.preview`
    // field is reserved for lazy-loaded context that
    // DOESN'T fit the dual-duty use of `row.output`:
    //   - herdr pane rows store their `tab_id` in
    //     `row.output` (needed by `focus_pane`), so
    //     the pane's visible content has to live
    //     somewhere else.
    // For rows with a populated `preview`, use it;
    // otherwise fall back to `output` (the historical
    // convention).
    //
    // `preview_only_modes` are the row kinds where
    // `row.output` is NOT a sensible preview fallback
    // (it carries metadata, not content). For those
    // rows we read `row.preview` exclusively and treat
    // an empty preview as "no preview available",
    // showing the standard placeholder instead of
    // falling back to the metadata field. The earlier
    // code naively fell back to `row.output` for
    // every mode, which made a `mode == "pane"` row
    // whose preview IPC failed (or hadn't completed
    // yet) display its `tab_id` (e.g. `wA:t1`) for
    // one frame and then snap to the actual pane
    // content the next time `ensure_selected_context`
    // ran — the "toggling between pane id and
    // content" bug the user reported.
    let preview_only = matches!(
        row.mode.as_str(),
        "pane" | "workspace" | "session" | "process"
    );
    let preview_text: &str = if !row.preview.is_empty() {
        row.preview.as_str()
    } else if preview_only {
        // No preview available, and the fallback
        // `output` field is metadata we'd rather not
        // show as "preview". The empty-string check
        // below turns this into the standard
        // "No output captured" placeholder.
        ""
    } else {
        row.output.as_str()
    };

    if preview_text.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("No output captured", Theme::dim()))
                .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)))
                .block(block),
            area,
        );
        return;
    }

    // Every mode's preview text is already bounded at its own
    // fetch/capture site (windowed source-context modes load at most
    // `SOURCE_CONTEXT_LINES`; plain history rows' captured command
    // output is bounded by `capturelines=`; note/todo/file rows load
    // at most `SOURCE_CONTEXT_LINES` of the referenced file; pane rows
    // by `herdr pane read --lines 50`) — so there's no need for a
    // second, render-time cap here on top of that. We render every
    // line the mode already loaded; the `Paragraph`'s own render area
    // only actually draws `visible_height` of them, and the `scroll`
    // computed below (from the real `area.height`, not a hardcoded
    // constant) is what determines which window of lines is on
    // screen — so a taller terminal genuinely shows more without any
    // mode-specific code, and every prefix mode benefits equally
    // instead of only the modes that happened to be on an
    // allow-list for the historical 50-line cap (plain history rows,
    // and any mode not on that list, used to be hard-capped at 4
    // lines regardless of how much room the pane actually had, and
    // — since `total_lines` was computed from that same truncated
    // slice — couldn't be scrolled into either).
    //
    // `highlight_with_bat`/`highlight_with_bat_auto` (`syntect`,
    // in-process — see `src/highlight.rs`) emit 24-bit-color ANSI
    // escape codes for tags/codegraph rows, and `ag` itself emits
    // ANSI for matched-line previews. The markdown
    // `render_preview_line` path doesn't parse ANSI (it would
    // mangle `\x1b[...m` through the inline parser), so any output
    // containing an escape must go through `parse_ansi_line`
    // instead. This is mode-agnostic: an ag row whose match had no
    // coloring proceeds through the markdown path cleanly (`ag`
    // itself decides whether to emit ANSI, independent of
    // `highlight_with_bat*`).
    //
    // herdr pane content is plain text by default (we don't pass
    // `--format ansi` to `herdr pane read` to keep the IPC
    // payload small), so the markdown path is the right one.
    let has_ansi = preview_text.contains('\x1b');
    let preview_lines: Vec<Line> = if has_ansi {
        preview_text
            .lines()
            .map(parse_ansi_line)
            .map(Line::from)
            .collect()
    } else {
        preview_text
            .lines()
            .map(render_preview_line)
            .collect()
    };

    // Render without `Wrap` so lines wider than the preview
    // area get truncated at the right edge with the beginning
    // still visible. The previous behavior (`.wrap(Wrap { trim:
    // false })`) wrapped long lines to multiple visual rows,
    // which destroyed the source-code alignment for
    // syntax-highlighted previews (e.g. an indented `    foo` line
    // would wrap to a new row with the indentation preserved
    // but the start position no longer matching the source).
    // Source code reads top-to-bottom / left-to-right; users
    // want to see the start of each line, not a wrapped
    // continuation. The horizontal scroll is left at 0
    // (the default) so the leftmost column is always anchored
    // — the beginning of every line is always visible.
    //
    // Vertical scroll: windowed source-context modes
    // (`tags` / `ag` / `codegraph` / `segments` / `similar`) load a
    // `SOURCE_CONTEXT_LINES` (50)-line window centered on
    // the matched line. The matched line sits at position
    // `half = 25` within that window. The preview area is
    // usually only 10–20 lines tall, so without a scroll
    // hint the matched line would be below the fold and
    // the user would have to scroll down inside the
    // preview pane to find the line they searched for.
    // `row.preview_scroll` (set by `ensure_selected_context`
    // in each windowed mode) carries the desired vertical
    // offset; we clamp it so it doesn't push past the
    // bottom of the loaded content (e.g. when the file
    // has fewer than 50 lines, or the matched line is
    // near the end of the file).
    let visible_height = area.height.saturating_sub(2) as usize;
    let total_lines = preview_lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = (row.preview_scroll as usize).min(max_scroll);
    let paragraph = Paragraph::new(preview_lines)
        .block(block)
        .scroll((scroll as u16, 0))
        .style(Style::default().bg(PALETTE.with(|p| p.borrow().details_bg)));
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
    // - question (`?`): info (blue — queries
    //   return information).
    // - todo (`!`): warning (yellow — calls
    //   attention to action items).
    //
    // Where two modes share a colour, the prefix
    // character is the differentiator. The colour
    // is the secondary reinforcement.
    let is_regex = app.is_regex_query();
    let is_fuzzy = app.is_fuzzy_query();
    // The active mode is resolved once via
    // `mode::active_mode` and used for the
    // per-mode prompt, title, border-style, and
    // title-style lookups below. This replaces
    // three identical `is_X_query()` if/else
    // chains with a single match.
    let active_mode = crate::tui::mode::active_mode(app);
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
            let (prompt_str, title_str) =
                crate::tui::mode::input_prompt_title(active_mode, algo, app.jira_last_jql.as_deref());
            (prompt_str, title_str, app.query.as_str())
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
            //
            // The per-mode colour lookup is a
            // single `match` in
            // `mode::input_title_style`; the
            // history / no-prefix mode falls
            // through to the match-algorithm
            // colours (regex = warning, fuzzy =
            // success, plain = accent).
            .title_style(
                crate::tui::mode::input_title_style(active_mode)
                    .unwrap_or_else(|| {
                        if is_regex {
                            Style::default().fg(Theme::warning_color())
                        } else if is_fuzzy {
                            Style::default().fg(Theme::success_color())
                        } else {
                            Theme::accent()
                        }
                    })
            )
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
            } else {
                crate::tui::mode::input_title_style(active_mode).unwrap_or_else(|| {
                    if is_regex {
                        Style::default().fg(Theme::warning_color())
                    } else if is_fuzzy {
                        Style::default().fg(Theme::success_color())
                    } else {
                        Theme::dim()
                    }
                })
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
    let mut count = match n {
        0 => "0 matches".to_string(),
        1 => "1 match".to_string(),
        x => format!("{} matches", x),
    };
    // Fold the marked count into the existing left-anchored
    // segment (rather than adding a 4th span) — the status bar
    // already packs 3 unmanaged segments with no overflow
    // handling, so growing the count string in place is the
    // lower-risk way to surface this.
    if !app.marked_ids.is_empty() {
        count.push_str(&format!(" · {} marked", app.marked_ids.len()));
    }

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
    // The badge shows BOTH the theme name AND the active color
    // scheme (light / dark) so the user always knows which
    // `theme.<scheme>=` config slot is currently active.
    let theme_label = format!(
        " theme: {} · {} ",
        app.theme.display_name(),
        app.detected_scheme.label()
    );

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
    use super::{
        content_display_position, list_display_position, truncate_cmd_for_details_pane,
        wrap_chars_to_rows,
    };

    /// History-mode (non-panes) rows are stored newest-first
    /// (data index 0 = newest), but the list DISPLAYS them
    /// oldest-at-top. So data index 0 (newest) is the LAST
    /// on-screen position (`real_count`), and the last data
    /// index (oldest) is on-screen position 1.
    #[test]
    fn list_display_position_flips_index_for_history_mode() {
        // 5 rows, data index 0 (newest) -> on-screen position 5
        // (bottom-most).
        assert_eq!(list_display_position(Some(0), 5, false), Some(5));
        // data index 4 (oldest) -> on-screen position 1 (top-most).
        assert_eq!(list_display_position(Some(4), 5, false), Some(1));
        // A middle index.
        assert_eq!(list_display_position(Some(2), 5, false), Some(3));
    }

    /// Panes mode is already top-to-bottom (tree order), so the
    /// data index maps directly to the 1-based on-screen position
    /// with no flip.
    #[test]
    fn list_display_position_is_unflipped_for_panes_mode() {
        assert_eq!(list_display_position(Some(0), 5, true), Some(1));
        assert_eq!(list_display_position(Some(4), 5, true), Some(5));
    }

    /// No selection or an empty list both yield `None` — the
    /// title falls back to just the total count, no "N/M" suffix.
    #[test]
    fn list_display_position_none_when_unselected_or_empty() {
        assert_eq!(list_display_position(None, 5, false), None);
        assert_eq!(list_display_position(Some(0), 0, false), None);
    }

    use super::list_visible_window;

    #[test]
    fn list_visible_window_empty_list_or_zero_viewport() {
        assert_eq!(list_visible_window(None, 0, 10, 0), (0, 0));
        assert_eq!(list_visible_window(Some(0), 0, 0, 10), (0, 0));
    }

    /// No selection: the window is just `[offset, offset+viewport)`,
    /// clamped to `total` — the anchor position from `draw_list`,
    /// unmodified.
    #[test]
    fn list_visible_window_no_selection_uses_offset_as_is() {
        assert_eq!(list_visible_window(None, 0, 10, 100), (0, 10));
        assert_eq!(list_visible_window(None, 50, 10, 100), (50, 60));
        // Anchor near the end: window clamps to `total`.
        assert_eq!(list_visible_window(None, 95, 10, 100), (95, 100));
    }

    /// Selection already inside `[offset, offset+viewport)`: window
    /// is unchanged from the anchor.
    #[test]
    fn list_visible_window_selection_inside_anchor_is_a_noop() {
        assert_eq!(list_visible_window(Some(55), 50, 10, 100), (50, 60));
    }

    /// Selection scrolled ABOVE the anchor window (e.g. the user
    /// pressed `Up` repeatedly past a bottom-anchored viewport, the
    /// scenario a long segments-mode result list hits by default):
    /// the window slides up so the selected row is the first line.
    #[test]
    fn list_visible_window_selection_above_anchor_scrolls_up() {
        assert_eq!(list_visible_window(Some(5), 50, 10, 100), (5, 15));
        // Selection at the very top of the data.
        assert_eq!(list_visible_window(Some(0), 50, 10, 100), (0, 10));
    }

    /// Selection scrolled BELOW the anchor window: the window
    /// slides down so the selected row is the last line.
    #[test]
    fn list_visible_window_selection_below_anchor_scrolls_down() {
        assert_eq!(list_visible_window(Some(80), 0, 10, 100), (71, 81));
        // Selection at the very end of the data.
        assert_eq!(list_visible_window(Some(99), 0, 10, 100), (90, 100));
    }

    /// A viewport at least as tall as the data, anchored at offset
    /// 0 (the only anchor `draw_list` ever passes for a short list
    /// — its "pad the top" branch): the window covers everything.
    #[test]
    fn list_visible_window_viewport_covers_entire_short_list() {
        assert_eq!(list_visible_window(Some(2), 0, 10, 5), (0, 5));
        assert_eq!(list_visible_window(None, 0, 10, 5), (0, 5));
        // A nonzero offset is honored literally even when the
        // viewport could fit everything — `draw_list` never passes
        // this combination (its anchor is always 0 whenever total <
        // viewport), but the function doesn't second-guess an
        // offset it's given, matching ratatui's own behavior.
        assert_eq!(list_visible_window(None, 3, 10, 5), (3, 5));
    }

    /// `draw_list` builds a `ListItem` per index in `[first, last)`
    /// — this asserts that range never exceeds the requested
    /// viewport size, for a broad sweep of inputs. A window wider
    /// than the viewport would mean `draw_list` is building more
    /// `ListItem`s than can ever be painted, defeating the whole
    /// point of windowing.
    #[test]
    fn list_visible_window_never_exceeds_viewport_size() {
        for total in [0usize, 1, 5, 10, 50, 137, 20_000] {
            for viewport in [0usize, 1, 3, 10, 40] {
                for offset in [0usize, 1, 7, total / 2, total.saturating_sub(1)] {
                    for selected in [None, Some(0), Some(total / 3), Some(total.saturating_sub(1))]
                    {
                        let (first, last) = list_visible_window(selected, offset, viewport, total);
                        assert!(first <= last, "first {first} > last {last}");
                        assert!(
                            last - first <= viewport,
                            "window ({first}, {last}) wider than viewport {viewport} \
                             (total={total}, offset={offset}, selected={selected:?})"
                        );
                        assert!(last <= total, "window extends past total {total}: {last}");
                        // A zero-height viewport can't show anything
                        // — including the selection — by construction.
                        if let Some(sel) = selected
                            && sel < total
                            && viewport > 0
                        {
                            assert!(
                                sel >= first && sel < last,
                                "selected {sel} not inside window ({first}, {last}) \
                                 (total={total}, offset={offset}, viewport={viewport})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Validates `list_visible_window` against the REAL ratatui
    /// `List` widget (not just a hand-derived formula): renders a
    /// `List` of uniform-height (single-`Line`) items to a
    /// `TestBackend` with the same `(selected, offset, viewport,
    /// total)` inputs, and checks the resulting `state.offset`
    /// (which ratatui's own `List::render` sets to whatever it
    /// actually painted from — see `list::rendering::get_items_bounds`
    /// upstream) matches `list_visible_window`'s `first`. This is
    /// the actual claim `draw_list` now depends on: that skipping
    /// `ListItem` construction for anything outside this window
    /// produces the identical on-screen result ratatui would have
    /// picked out of the full, unwindowed item set itself.
    #[test]
    fn list_visible_window_matches_real_ratatui_scroll() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::text::Line;
        use ratatui::widgets::{List, ListItem, ListState};

        for total in [1usize, 2, 5, 10, 47, 200] {
            let items: Vec<ListItem> = (0..total)
                .map(|i| ListItem::new(Line::from(format!("row {i}"))))
                .collect();
            for viewport in [1usize, 3, 10, 25] {
                for offset in [0usize, 1, total / 2, total.saturating_sub(1)] {
                    for selected in
                        [None, Some(0), Some(total / 3), Some(total.saturating_sub(1))]
                    {
                        // No block/borders on this `List` — the full
                        // backend area IS the list's content area, so
                        // `viewport` maps directly to `list_height`
                        // without needing to account for border rows
                        // (unlike `draw_list`'s real `List`, which has
                        // a bordered `Block` and sizes its backend
                        // area at `viewport + 2` accordingly).
                        let backend = TestBackend::new(20, viewport as u16);
                        let mut terminal = Terminal::new(backend).expect("terminal");
                        // `.with_selected()`, NOT `.select()`: `ListState::select`
                        // has a side effect of resetting `offset` to 0 whenever
                        // the new selection is `None` (see its doc/impl) — which
                        // would corrupt this test's `offset` input before the
                        // widget ever renders. The fluent `with_selected` setter
                        // has no such side effect.
                        let mut state = ListState::default()
                            .with_offset(offset)
                            .with_selected(selected);
                        let list = List::new(items.clone());
                        terminal
                            .draw(|f| {
                                f.render_stateful_widget(list.clone(), f.area(), &mut state);
                            })
                            .expect("draw");
                        let (expected_first, _) =
                            list_visible_window(selected, offset, viewport, total);
                        assert_eq!(
                            state.offset(),
                            expected_first,
                            "total={total} viewport={viewport} offset={offset} \
                             selected={selected:?}: ratatui picked first-visible \
                             index {}, list_visible_window said {expected_first}",
                            state.offset()
                        );
                    }
                }
            }
        }
    }

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

    /// Regression test: an unclosed `_` (the italic marker's OTHER
    /// spelling — `MarkerKind::Italic` covers both `*` and `_`) must
    /// fall through with its ACTUAL character preserved, not get
    /// silently rewritten into `*` (a real bug found via the
    /// glob-completion directory picker's preview — a directory
    /// named `alpha_sub` rendered as `alpha*sub` before this fix,
    /// since the old code used a hardcoded per-*kind* string for the
    /// fallback instead of the character that actually opened it).
    #[test]
    fn preview_line_unclosed_underscore_marker_preserves_underscore_not_asterisk() {
        let line = render_preview_line("alpha_sub/");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "alpha_sub/");
        assert!(!line.spans[0].style.add_modifier.contains(Modifier::ITALIC));
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

    #[test]
    fn wrap_chars_to_rows_short_line_is_one_row() {
        let chars: Vec<char> = "hello".chars().collect();
        let rows = wrap_chars_to_rows(&chars, 10);
        assert_eq!(rows, vec![("hello".to_string(), 0)]);
    }

    #[test]
    fn wrap_chars_to_rows_breaks_at_whitespace() {
        let chars: Vec<char> = "the quick brown fox".chars().collect();
        // width 10: "the quick " is 10 chars including the trailing
        // space, so the break lands on that space (dropped) and
        // "brown fox" (9 chars) fits the second row whole.
        let rows = wrap_chars_to_rows(&chars, 10);
        assert_eq!(
            rows,
            vec![
                ("the quick".to_string(), 0),
                ("brown fox".to_string(), 10),
            ]
        );
    }

    #[test]
    fn wrap_chars_to_rows_hard_breaks_a_word_longer_than_width() {
        let chars: Vec<char> = "supercalifragilistic".chars().collect();
        let rows = wrap_chars_to_rows(&chars, 5);
        assert_eq!(
            rows,
            vec![
                ("super".to_string(), 0),
                ("calif".to_string(), 5),
                ("ragil".to_string(), 10),
                ("istic".to_string(), 15),
            ]
        );
    }

    #[test]
    fn wrap_chars_to_rows_empty_line_is_one_empty_row() {
        let rows = wrap_chars_to_rows(&[], 10);
        assert_eq!(rows, vec![(String::new(), 0)]);
    }

    #[test]
    fn wrap_chars_to_rows_zero_width_returns_line_unwrapped() {
        let chars: Vec<char> = "hello".chars().collect();
        let rows = wrap_chars_to_rows(&chars, 0);
        assert_eq!(rows, vec![("hello".to_string(), 0)]);
    }

    #[test]
    fn content_display_position_mid_row() {
        let rows = vec![("the quick".to_string(), 0), ("brown fox".to_string(), 10)];
        // Cursor at char 4 ("the |quick") is mid-first-row.
        assert_eq!(content_display_position(&rows, 4), (0, 4));
        // Cursor at char 13 ("brown| fox", offset 13 - 10 = 3
        // within the second row) is mid-second-row.
        assert_eq!(content_display_position(&rows, 13), (1, 3));
    }

    #[test]
    fn content_display_position_at_wrap_boundary_lands_on_next_row_start() {
        // Break point is the whitespace at char 9 ("the quick" is
        // chars 0..9, the space is char 9, "brown fox" starts at
        // char 10). A cursor sitting on that space (offset 9) is
        // "last start <= 9", which is still row 0 (start 0), giving
        // col 9 — the right edge of row 0.
        let rows = vec![("the quick".to_string(), 0), ("brown fox".to_string(), 10)];
        assert_eq!(content_display_position(&rows, 9), (0, 9));
        // Offset 10 (right after the dropped whitespace) is exactly
        // where row 1 starts — lands at (1, 0), not (0, 10).
        assert_eq!(content_display_position(&rows, 10), (1, 0));
    }

    #[test]
    fn content_display_position_end_of_last_row() {
        let rows = vec![("hello".to_string(), 0)];
        assert_eq!(content_display_position(&rows, 5), (0, 5));
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

    #[test]
    fn parse_ansi_basic_8_color_foreground() {
        // The 8 standard fg colors. herdr sometimes emits
        // these (e.g. for simpler agents that don't use
        // truecolor) and the old parser silently dropped
        // them — the user saw "colors are different than
        // when I call this manually" because every basic
        // 8-color code in the pane was being ignored.
        for (code, expected) in [
            ("30", ratatui::style::Color::Black),
            ("31", ratatui::style::Color::Red),
            ("32", ratatui::style::Color::Green),
            ("33", ratatui::style::Color::Yellow),
            ("34", ratatui::style::Color::Blue),
            ("35", ratatui::style::Color::Magenta),
            ("36", ratatui::style::Color::Cyan),
            ("37", ratatui::style::Color::White),
        ] {
            let input = format!("\x1b[{}mhi\x1b[0m", code);
            let spans = parse_ansi_line(&input);
            assert_eq!(spans.len(), 1, "code {}: span count", code);
            assert_eq!(spans[0].style.fg, Some(expected), "code {}: fg", code);
        }
    }

    #[test]
    fn parse_ansi_bright_8_color_foreground() {
        for (code, expected) in [
            ("90", ratatui::style::Color::DarkGray),
            ("91", ratatui::style::Color::LightRed),
            ("92", ratatui::style::Color::LightGreen),
            ("93", ratatui::style::Color::LightYellow),
            ("94", ratatui::style::Color::LightBlue),
            ("95", ratatui::style::Color::LightMagenta),
            ("96", ratatui::style::Color::LightCyan),
            ("97", ratatui::style::Color::White),
        ] {
            let input = format!("\x1b[{}mhi\x1b[0m", code);
            let spans = parse_ansi_line(&input);
            assert_eq!(spans.len(), 1, "code {}: span count", code);
            assert_eq!(spans[0].style.fg, Some(expected), "code {}: fg", code);
        }
    }

    #[test]
    fn parse_ansi_basic_8_color_background() {
        // Same set for the bg channel — the old parser
        // only handled 48;2;R;G;B and 48;5;N, dropping
        // every 4x code.
        for (code, expected) in [
            ("40", ratatui::style::Color::Black),
            ("41", ratatui::style::Color::Red),
            ("42", ratatui::style::Color::Green),
            ("43", ratatui::style::Color::Yellow),
            ("44", ratatui::style::Color::Blue),
            ("45", ratatui::style::Color::Magenta),
            ("46", ratatui::style::Color::Cyan),
            ("47", ratatui::style::Color::White),
        ] {
            let input = format!("\x1b[{}mhi\x1b[0m", code);
            let spans = parse_ansi_line(&input);
            assert_eq!(spans.len(), 1, "code {}: span count", code);
            assert_eq!(spans[0].style.bg, Some(expected), "code {}: bg", code);
        }
    }

    #[test]
    fn parse_ansi_default_fg_bg() {
        // 39 = default fg, 49 = default bg. ratatui
        // exposes these as `Color::Reset`.
        let input = "\x1b[39;49mhi\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, Some(ratatui::style::Color::Reset));
        assert_eq!(spans[0].style.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn parse_ansi_multi_parameter_sequence() {
        // `\x1b[1;31m` is "bold red" — both codes apply.
        let input = "\x1b[1;31mboldred\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "boldred");
        assert_eq!(spans[0].style.fg, Some(ratatui::style::Color::Red));
        assert!(
            spans[0].style.add_modifier.contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn parse_ansi_colon_separator_truecolor() {
        // herdr and some terminals emit truecolor
        // with `:` instead of `;` inside the parameter
        // list: `\x1b[38:2:R:G:Bm`. The old parser
        // treated `:` as a non-digit and dropped
        // everything, so the color was lost.
        let input = "\x1b[38:2:102:217:239mfn\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "fn");
        assert_eq!(
            spans[0].style.fg,
            Some(ratatui::style::Color::Rgb(102, 217, 239))
        );
    }

    #[test]
    fn parse_ansi_colon_separator_256_color() {
        // Same colon-separator form for 256-color.
        let input = "\x1b[38:5:196mred\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].style.fg,
            Some(ratatui::style::Color::Rgb(255, 0, 0))
        );
    }

    #[test]
    fn parse_ansi_underline_strikethrough_dim() {
        // All the common modifiers the old parser
        // silently dropped.
        let input = "\x1b[4munder\x1b[0m \x1b[9mstrike\x1b[0m \x1b[2mdim\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 5);
        assert!(spans[0].style.add_modifier.contains(
            ratatui::style::Modifier::UNDERLINED
        ));
        assert!(
            spans[2].style.add_modifier.contains(
                ratatui::style::Modifier::CROSSED_OUT
            )
        );
        assert!(spans[4].style.add_modifier.contains(ratatui::style::Modifier::DIM));
    }

    #[test]
    fn parse_ansi_fg_and_bg_combined() {
        // A real-world herdr emit:
        // `[38;2;108;108;108m[48;2;232;240;232mTook 0.0s[0m`
        // — fg + bg + text in one run.
        let input =
            "\x1b[38;2;108;108;108m\x1b[48;2;232;240;232mTook 0.0s\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "Took 0.0s");
        assert_eq!(
            spans[0].style.fg,
            Some(ratatui::style::Color::Rgb(108, 108, 108))
        );
        assert_eq!(
            spans[0].style.bg,
            Some(ratatui::style::Color::Rgb(232, 240, 232))
        );
    }

    #[test]
    fn parse_ansi_reset_clears_attributes() {
        // `[0m` should clear fg / bg / modifiers. The
        // old code already handled this — keep the
        // regression test.
        let input = "\x1b[1;31mboldred\x1b[0mplain";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].content, "plain");
        assert_eq!(spans[1].style.fg, None);
        assert_eq!(spans[1].style.bg, None);
    }

    #[test]
    fn parse_ansi_empty_parameter_is_reset() {
        // `\x1b[m` and `\x1b[;31m` are equivalent to
        // `\x1b[0m` / `\x1b[0;31m` per the xterm spec.
        let input = "\x1b[mbefore\x1b[31mred\x1b[0m";
        let spans = parse_ansi_line(input);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "before");
        assert_eq!(spans[0].style.fg, None);
        assert_eq!(spans[1].content, "red");
        assert_eq!(spans[1].style.fg, Some(ratatui::style::Color::Red));
    }

    #[test]
    fn parse_ansi_real_herdr_pane_output() {
        // A representative slice of real `herdr pane read
        // w20:p1 --ansi` output: a status line with fg +
        // bg truecolor, surrounded by `[0m` resets. The
        // renderer should reproduce the same colors
        // visible in the terminal when the user runs the
        // command by hand.
        let input = "\x1b[0m\x1b[48;2;232;240;232m \x1b[0m\x1b[38;2;108;108;108m\x1b[48;2;232;240;232mTook 0.0s\x1b[0m";
        let spans = parse_ansi_line(input);
        // The exact span count depends on how many SGR
        // codes emit empty-text segments; what matters is
        // that the visible "Took 0.0s" text carries the
        // right fg + bg.
        let took = spans
            .iter()
            .find(|s| s.content.contains("Took 0.0s"))
            .expect("expected a span containing 'Took 0.0s'");
        assert_eq!(
            took.style.fg,
            Some(ratatui::style::Color::Rgb(108, 108, 108))
        );
        assert_eq!(
            took.style.bg,
            Some(ratatui::style::Color::Rgb(232, 240, 232))
        );
    }
}
