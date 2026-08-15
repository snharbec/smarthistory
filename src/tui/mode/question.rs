//! `?` (general LLM question) prefix mode.
//!
//! Like the LLM mode, requires non-whitespace text after the
//! prefix.
use crate::tui::{App, LlmRequestType};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rusqlite::params;

/// True if the current query is a general question
/// request (prefixed with the configured question prefix).
/// Only returns true if there's actual question text after
/// the prefix (not just the prefix alone or with only whitespace).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.question;
    app.query.starts_with(p) && !app.query[p.len_utf8()..].trim().is_empty()
}

/// The question body, i.e. everything after the leading
/// `?` prefix. Empty string when not in question mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.question;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

impl App {
    /// True if the current query is a general question
    /// request (prefixed with configured question prefix).
    /// Only returns true if there's actual question text after
    /// the prefix (not just the prefix alone or with only whitespace).
    pub(crate) fn is_question_query(&self) -> bool {
        crate::tui::mode::question::matches(self)
    }

    /// Handle a `%...` query by sending the natural-language
    /// question to the configured ollama instance and displaying
    /// the answer in an overlay. The question is also saved to
    /// history with the answer stored as output (but not as a
    /// comment).
    pub(crate) fn run_question_query(&mut self) {
        // Extract the question (everything after the leading question prefix).
        let prefix = self.query_prefixes.question;
        let question = self.query[prefix.len_utf8()..].trim();
        if question.is_empty() {
            self.set_status_message(
                "Question: provide a question after the question prefix".to_string(),
            );
            return;
        }
        // Bail out cleanly if the LLM isn't configured.
        if self.llm.is_none() {
            self.set_status_message(crate::llm::LlmError::NotConfigured.to_string());
            return;
        }

        let question_owned = question.to_string();
        let context = self.last_command_context();
        let prompt = crate::llm::build_question_prompt(&question_owned, context.as_ref());
        self.spawn_llm_request(
            LlmRequestType::Question {
                question: question_owned,
                context,
            },
            prompt,
        );
    }

    /// The most recently run shell command in the current
    /// session, with its exit code and captured output if
    /// any — used to seed the `?` question prompt so questions
    /// like "what does that command do" or "why did this fail"
    /// can resolve "that"/"this" without the user retyping the
    /// command. Thin wrapper around `crate::llm::last_command_context`,
    /// which has no TUI dependency and is shared with the
    /// `smarthistory ask` CLI subcommand.
    pub(crate) fn last_command_context(&self) -> Option<crate::llm::LastCommandContext> {
        let session_id = std::env::var("SMART_HISTORY_SESSION").unwrap_or_default();
        crate::llm::last_command_context(&self.conn, &session_id)
    }

    /// Persist a general question to the history table with
    /// `question` (prefixed with `?`) as the command and
    /// `answer` stored as the output (but not as a comment).
    pub(crate) fn stage_question(&mut self, question: String, answer: String) {
        // Same canonicalization as
        // the LLM staging site —
        // keeps the dedup index
        // consistent.
        let directory =
            crate::util::canonicalize_directory(&std::env::var("PWD").unwrap_or_default());
        let session_id = std::env::var("SMART_HISTORY_SESSION").unwrap_or_default();
        let query_command = format!("{}{}", self.query_prefixes.question, question);

        let insert_result: anyhow::Result<i64> = (|| {
            self.conn.execute(
                "INSERT INTO history (command, directory, session_id, exit_code, mode) \
                 VALUES (?1, ?2, ?3, -1, 'question') \
                 ON CONFLICT (command, directory, session_id) DO UPDATE \
                 SET timestamp = (strftime('%s', 'now')), mode = 'question'",
                params![&query_command, &directory, &session_id],
            )?;
            let id: i64 = self.conn.query_row(
                "SELECT id FROM history WHERE command = ?1 AND directory = ?2 AND session_id = ?3",
                params![&query_command, &directory, &session_id],
                |row| row.get(0),
            )?;
            Ok(id)
        })();

        let history_id = match insert_result {
            Ok(id) => id,
            Err(e) => {
                self.set_status_message(format!("Question: history insert failed: {}", e));
                return;
            }
        };

        // Store the answer as output (not as comment).
        let output_result: anyhow::Result<()> = (|| {
            self.conn.execute(
                "INSERT INTO history_output (history_id, output) VALUES (?1, ?2) \
                 ON CONFLICT (history_id) DO UPDATE SET output = excluded.output, captured_at = (strftime('%s', 'now'))",
                params![history_id, &answer],
            )?;
            Ok(())
        })();

        if let Err(e) = output_result {
            self.set_status_message(format!("Question: output store failed: {}", e));
        }
    }

    pub(crate) fn close_question(&mut self) {
        self.question_view = None;
    }

    pub(crate) fn is_question_viewing(&self) -> bool {
        self.question_view.is_some()
    }
}

/// Key handler for the general question overlay (prefixed with `?`).
///
/// Mirrors the describe overlay in shape (a piece of text + a scroll
/// offset) but is driven by the user's question rather than by the
/// captured stdout of a history row.
///
/// Close keys: `Esc`, `Enter`, `q`, `Ctrl-C`. Scroll keys: `Up` /
/// `Down` for one line, `PageUp` / `PageDown` for a page, `Home` /
/// `End` for the extremes.
///
/// Returns `true` only when the user aborts the whole TUI with `Ctrl-C`.
pub(crate) fn handle_question_view_key(app: &mut App, key: KeyEvent, page_size: usize) -> bool {
    let max_scroll = |text: &str| -> usize {
        let total = text.lines().count();
        total.saturating_sub(page_size.max(1))
    };
    let is_close = matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'));
    if is_close {
        app.close_question();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_question();
        return true;
    }
    match key.code {
        KeyCode::Up => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.question_view {
                let max = view.text.lines().count().saturating_sub(page_size.max(1));
                view.scroll = (view.scroll + 1).min(max);
            }
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = view.scroll.saturating_sub(page_size.max(1));
            }
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.question_view {
                let max = view.text.lines().count().saturating_sub(page_size.max(1));
                view.scroll = (view.scroll + page_size.max(1)).min(max);
            }
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = 0;
            }
        }
        KeyCode::End => {
            if let Some(ref mut view) = app.question_view {
                view.scroll = max_scroll(&view.text);
            }
        }
        _ => {}
    }
    false
}
