// Staging actions for the TUI: what happens when the user presses
// Enter/Left/Right on a selected row. This was extracted from
// src/tui.rs to reduce the size of the main TUI module.

use super::*;

/// Build a `$EDITOR +<line> <file>` command for staging, validating
/// `line` as a plain positive integer before splicing it in
/// unquoted. `+<line>` is the vim/most-editors line-jump syntax and
/// can't be shell-quoted as a unit without breaking that syntax, so
/// the numeric check is the only guard — an unvalidated `line`
/// coming from row data (tags/ag/codegraph rows all carry it in
/// `session_id`, sourced from ctags fields, `ag` output splitting,
/// or a DB column) would otherwise be a command-injection primitive
/// the moment it's `eval`'d by the parent shell. A non-numeric line
/// still opens the file, just without jumping to a line.
pub(crate) fn stage_editor_open_at_line(editor: &str, filepath: &str, line: &str) -> String {
    let quoted_path = crate::util::shell_quote(filepath);
    match line.parse::<u64>() {
        Ok(n) => format!("{} +{} {}", editor, n, quoted_path),
        Err(_) => format!("{} {}", editor, quoted_path),
    }
}

impl App {
    pub(crate) fn select_for_run_impl(&mut self) {
        // The active prefix mode drives a flat `match`
        // dispatch. Each arm is specialised for its
        // mode's staging behaviour (LLM generates a
        // command, todo opens the editor at a line,
        // files / tags / ag / codegraph all open an
        // editor at a path+line, jira opens the
        // browser, etc.). The fall-through arm is the
        // history / no-prefix row selection.
        match crate::tui::mode::active_mode(self) {
            crate::tui::mode::ModeKind::Llm => {
                // `=...` queries are an LLM
                // command-generation request, not a row
                // selection. Short-circuit before any row
                // lookup: there *is* no meaningful
                // selected row when the user is
                // composing a natural-language description.
                self.run_llm_query();
            }
            crate::tui::mode::ModeKind::Question => {
                // `%...` queries are general question
                // requests. Open an overlay with the
                // answer instead of running a command.
                self.run_question_query();
            }
            crate::tui::mode::ModeKind::Todo => {
                // `!...` queries are todo search requests.
                // Selecting a todo line opens the editor at
                // the exact line number so the user lands
                // on the todo. The `id` of a todo row is
                // `-(line_number)` (synthetic negative id),
                // so we recover the line number with
                // `i64::abs() as usize`. The body lives in
                // `stage_todo_selection` (the todo mode has
                // two sub-paths: `!@new <text>` to create a
                // new TODO entry, and the default to open
                // the selected todo in `$EDITOR` at the
                // line number).
                self.stage_todo_selection();
            }
            crate::tui::mode::ModeKind::Notes => {
                // `@...` queries are note search requests.
                // Selecting a note opens it in the editor.
                // The body lives in `stage_note_selection`
                // (two sub-paths: `@new <text>` to create
                // a new daily-note entry, and the default
                // to open the selected note in `$EDITOR`).
                self.stage_note_selection();
            }
            crate::tui::mode::ModeKind::Files => {
                if let Some(row) = self.selected_row() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    // The absolute path is in
                    // `row.directory` for files,
                    // set during `fetch_files`.
                    let filepath = &row.directory;
                    let quoted = crate::util::shell_quote(filepath);
                    self.selection = Some(format!("{} {}", editor, quoted));
                    self.pick_mode = Some(PickMode::Run);
                }
            }
            crate::tui::mode::ModeKind::Tags => {
                if let Some(row) = self.selected_row() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    // The absolute path is in
                    // `row.directory`, the line
                    // number is in `row.session_id`.
                    self.selection = Some(stage_editor_open_at_line(
                        &editor,
                        &row.directory,
                        &row.session_id,
                    ));
                    self.pick_mode = Some(PickMode::Run);
                }
            }
            crate::tui::mode::ModeKind::Segments => {
                // `:` queries are segment-search requests.
                // Same "open the file at the matching line"
                // convention as Tags/Ag/Codegraph — the
                // absolute path is in `row.directory`, the
                // segment's start line is in `row.session_id`.
                if let Some(row) = self.selected_row() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    self.selection = Some(stage_editor_open_at_line(
                        &editor,
                        &row.directory,
                        &row.session_id,
                    ));
                    self.pick_mode = Some(PickMode::Run);
                }
            }
            crate::tui::mode::ModeKind::Similar => {
                // `"` queries are similar-phrase search requests
                // over the same `segments` table — identical
                // staging to `ModeKind::Segments` above.
                if let Some(row) = self.selected_row() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    self.selection = Some(stage_editor_open_at_line(
                        &editor,
                        &row.directory,
                        &row.session_id,
                    ));
                    self.pick_mode = Some(PickMode::Run);
                }
            }
            crate::tui::mode::ModeKind::Ag => {
                // `,` queries are ag content-search
                // requests. Selecting a match opens
                // the file in $EDITOR at the
                // matching line number.
                if let Some(row) = self.selected_row() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    self.selection = Some(stage_editor_open_at_line(
                        &editor,
                        &row.directory,
                        &row.session_id,
                    ));
                    self.pick_mode = Some(PickMode::Run);
                }
            }
            crate::tui::mode::ModeKind::Codegraph => {
                // `&` queries are CodeGraph
                // symbol-search requests. Selecting a
                // symbol opens the source file in
                // $EDITOR at the symbol's
                // `start_line`, exactly like tags
                // mode (the row's `directory` and
                // `session_id` carry the absolute path
                // and line).
                if let Some(row) = self.selected_row() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    self.selection = Some(stage_editor_open_at_line(
                        &editor,
                        &row.directory,
                        &row.session_id,
                    ));
                    self.pick_mode = Some(PickMode::Run);
                }
            }
            crate::tui::mode::ModeKind::Directories => {
                // `#...` queries are directories-view
                // requests. Selecting a directory
                // stages `cd <abs-path>` (or
                // `tmux select-pane && switch-client`
                // for `T`-marked rows where the
                // directory is the cwd of an active
                // tmux pane). The complex
                // tmux/herdr-backend logic lives
                // in `stage_directory_selection`.
                self.stage_directory_selection();
            }
            crate::tui::mode::ModeKind::Panes => {
                // `*...` queries are multiplexer
                // panes / windows / sessions /
                // hosts. The complex
                // tmux/herdr-backend logic lives
                // in `stage_pane_selection`.
                self.stage_pane_selection();
            }
            crate::tui::mode::ModeKind::Jira => {
                // `-...` queries are JIRA
                // issue-search requests. The
                // open-in-browser flow lives
                // in `stage_jira_selection`.
                self.stage_jira_selection();
            }
            crate::tui::mode::ModeKind::Paperless => {
                // `<...` queries are paperless
                // document-search requests. The
                // open-in-browser flow lives
                // in `stage_paperless_selection`.
                self.stage_paperless_selection();
            }
            crate::tui::mode::ModeKind::Browser => {
                // `^...` queries are browser
                // bookmarks/history requests. The
                // open-in-browser flow lives
                // in `stage_browser_selection`.
                self.stage_browser_selection();
            }
            crate::tui::mode::ModeKind::Zoxide => {
                // `~...` queries are zoxide directory
                // rows. `stage_zoxide_selection` may
                // defer the actual staging behind a
                // "save this directory?" prompt first
                // — see its own doc comment.
                self.stage_zoxide_selection();
            }
            crate::tui::mode::ModeKind::Processes => {
                // `%...` queries are running-process rows. Enter
                // must NOT stage/run the process name as a shell
                // command — it opens a signal-confirmation dialog
                // instead. See `stage_process_signal_prompt`.
                self.stage_process_signal_prompt();
            }
            crate::tui::mode::ModeKind::Pass => {
                // `)...` queries are pass password-store entries.
                // Selecting a row stages `pass show --clip <entry>`
                // so the parent shell copies the password to the
                // clipboard via pass's built-in support.
                self.stage_pass_selection();
            }
            crate::tui::mode::ModeKind::ProjectPick => {
                // `.`-prefixed queries are `type: project` notes.
                // Selecting one stages `smarthistory project select
                // <slug>`, setting the explicit "current project"
                // fallback. See `stage_project_selection`.
                self.stage_project_selection();
            }
            crate::tui::mode::ModeKind::Worktree => {
                // `;...` queries are git-worktree rows (tagged
                // `mode == "directory"`, same as Directories/Zoxide).
                // Selecting one stages `cd <abs-path>` directly — no
                // extra "save this directory?" detour, unlike
                // `stage_zoxide_selection`; that's zoxide-specific UX
                // this mode doesn't need.
                self.stage_directory_selection();
            }
            // The history / no-prefix mode
            // is the default — it stages
            // the selected history row for
            // the parent shell to run.
            _ => {
                self.stage_history_selection();
            }
        }
    }

    /// Stage the pass (`)`) mode selection.
    ///
    /// Reads the entry name from the selected row's `directory`
    /// field (which holds the raw entry path relative to the store
    /// root, without the `.gpg` extension) and stages
    /// `pass show --clip <entry>` for the parent shell to execute.
    /// `pass` will copy the first line of the entry (the password)
    /// to the clipboard and clear it after 45 seconds.
    fn stage_pass_selection(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.mode != "pass" {
            return;
        }
        let entry = row.directory.clone();
        if entry.is_empty() {
            return;
        }
        self.selection = Some(format!(
            "pass show --clip {}",
            crate::util::shell_quote(&entry)
        ));
        self.pick_mode = Some(PickMode::Run);
    }

    /// Stage the project-picker (`.`) mode selection.
    ///
    /// Slugs the selected note's filename stem the same way
    /// `project.<slug>.dir` config keys are matched (see
    /// `crate::util::slugify`), then opens `project_since_prompt`
    /// ("started how many minutes ago?") instead of staging directly
    /// — the actual `smarthistory project select <slug>` (optionally
    /// with `--since <N>m`) is staged once that prompt is answered,
    /// by `answer_project_since_prompt`. This is the same "defer the
    /// real staging behind a small prompt" shape
    /// `stage_zoxide_selection`/`zoxide_save_prompt` already use,
    /// just to collect a backdate offset instead of a yes/no.
    fn stage_project_selection(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.mode != "project" {
            return;
        }
        let stem = std::path::Path::new(&row.command)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&row.command);
        let slug = crate::util::slugify(stem, "project");
        self.project_since_prompt = Some(crate::tui::state::ProjectSincePrompt {
            slug,
            buffer: String::new(),
            cursor: 0,
        });
    }

    /// Answer the `project_since_prompt` opened by
    /// `stage_project_selection`. `buffer` is either empty or a plain
    /// digit string (enforced at insertion time by
    /// `handle_project_since_prompt_key`, so no parse error is
    /// possible here) — empty or `"0"` stages `smarthistory project
    /// select <slug>` exactly as `stage_project_selection` did before
    /// this prompt existed; a positive number `N` appends `--since
    /// {N}m`. Either way sets the same `selection`/`pick_mode` fields
    /// staging always has, so the TUI exits and the parent shell runs
    /// the staged command.
    pub(crate) fn answer_project_since_prompt(&mut self) {
        let Some(prompt) = self.project_since_prompt.take() else {
            return;
        };
        let minutes: u64 = prompt.buffer.parse().unwrap_or(0);
        let mut command = format!(
            "smarthistory project select {}",
            crate::util::shell_quote(&prompt.slug)
        );
        if minutes > 0 {
            command.push_str(&format!(" --since {minutes}m"));
        }
        self.selection = Some(command);
        self.pick_mode = Some(PickMode::Run);
    }

    /// Open the `Action::CreateWorktree` dialog (`;` mode): resolve
    /// the repo root for the current directory, populate the
    /// `PickBranch` step's candidate list, and open at that step. A
    /// status message (no dialog) when the cwd isn't inside a git
    /// repo — same degrade-to-message convention `DownloadJiraIssue`'s
    /// mode gate uses.
    pub(crate) fn open_worktree_create_flow(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let Some(repo_root) = crate::tui::mode::worktree::find_repo_root(&cwd) else {
            self.set_status_message("not inside a git repository".to_string());
            return;
        };
        let options = crate::tui::mode::worktree::list_branches(&repo_root);
        self.worktree_create_flow = Some(crate::tui::state::WorktreeCreateFlow {
            repo_root,
            step: crate::tui::state::WorktreeCreateStep::PickBranch,
            branch: String::new(),
            is_new_branch: false,
            base_branch: String::new(),
            carry_over: false,
            project_slug: None,
            options,
            filter: String::new(),
            cursor: 0,
            selected: 0,
            error: None,
        });
    }

    /// `Enter` pressed inside the `Action::CreateWorktree` dialog.
    /// Advances through `WorktreeCreateStep`s per the flow described on
    /// `WorktreeCreateFlow`'s doc comment, executing everything on the
    /// final `PickProject` step. Returns `true` when a `cd` command
    /// was staged (i.e. the TUI should exit), the same convention
    /// `handle_project_since_prompt_key` uses around
    /// `answer_project_since_prompt`.
    pub(crate) fn advance_worktree_create_flow(&mut self) -> bool {
        use crate::tui::state::WorktreeCreateStep;
        let Some(flow) = self.worktree_create_flow.as_ref() else {
            return false;
        };
        let step = flow.step;
        let repo_root = flow.repo_root.clone();
        let typed = flow.filter.trim().to_string();
        let filtered = crate::tui::state::worktree_create_filtered_options(flow);
        let chosen = filtered.get(flow.selected).cloned();
        match step {
            WorktreeCreateStep::PickBranch => {
                if let Some(existing) = chosen {
                    if let Some(f) = self.worktree_create_flow.as_mut() {
                        f.branch = existing;
                        f.is_new_branch = false;
                        f.error = None;
                    }
                    self.worktree_create_after_branch_chosen(&repo_root);
                } else if !typed.is_empty() {
                    // No existing branch matches what's typed — create
                    // a new one. Preselect the base branch: an
                    // explicit `worktree.defaultbranch` config value
                    // wins, otherwise auto-detect (remote HEAD /
                    // main / master / current branch).
                    let default_base = self.worktree_default_branch.clone().unwrap_or_else(|| {
                        crate::tui::mode::worktree::default_base_branch(&repo_root)
                    });
                    let base_options = crate::tui::mode::worktree::list_branches(&repo_root);
                    let selected_idx =
                        base_options.iter().position(|b| *b == default_base).unwrap_or(0);
                    if let Some(f) = self.worktree_create_flow.as_mut() {
                        f.branch = typed;
                        f.is_new_branch = true;
                        f.step = WorktreeCreateStep::PickBaseBranch;
                        f.base_branch = default_base;
                        f.options = base_options;
                        f.filter.clear();
                        f.cursor = 0;
                        f.selected = selected_idx;
                        f.error = None;
                    }
                } else if let Some(f) = self.worktree_create_flow.as_mut() {
                    f.error = Some("pick a branch or type a new name".to_string());
                }
                false
            }
            WorktreeCreateStep::PickBaseBranch => {
                if let Some(existing) = chosen {
                    if let Some(f) = self.worktree_create_flow.as_mut() {
                        f.base_branch = existing;
                        f.error = None;
                    }
                    self.worktree_create_after_branch_chosen(&repo_root);
                } else if let Some(f) = self.worktree_create_flow.as_mut() {
                    // Unlike `PickBranch`, there's no "create new"
                    // concept for a base branch — it must already exist.
                    f.error = Some("pick an existing branch as the base".to_string());
                }
                false
            }
            // `y`/`n` (not `Enter`) drive this step — see
            // `worktree_create_confirm_carry_over`, called directly
            // from `handle_worktree_create_flow_key`.
            WorktreeCreateStep::ConfirmCarryOver => false,
            WorktreeCreateStep::PickProject => {
                // A blank filter always means "skip assignment",
                // regardless of whether the (untouched) candidate list
                // happens to be non-empty — the one step where an
                // empty `Enter` is a deliberate choice, not "browse
                // the full list and pick index 0".
                let slug = if typed.is_empty() {
                    None
                } else if let Some(existing) = chosen {
                    Some(existing)
                } else {
                    Some(crate::util::slugify(&typed, "project"))
                };
                if let Some(f) = self.worktree_create_flow.as_mut() {
                    f.project_slug = slug;
                }
                self.worktree_create_execute()
            }
        }
    }

    /// The dirty-check that runs right after `branch`/`base_branch`
    /// are settled (from either `PickBranch` choosing an existing
    /// branch, or `PickBaseBranch` completing a new one): clean →
    /// skip straight to `PickProject`; dirty → open `ConfirmCarryOver`
    /// first so the user can choose whether to bring their
    /// uncommitted changes along.
    fn worktree_create_after_branch_chosen(&mut self, repo_root: &std::path::Path) {
        use crate::tui::state::WorktreeCreateStep;
        let dirty = crate::tui::mode::worktree::repo_is_dirty(repo_root);
        if dirty {
            if let Some(f) = self.worktree_create_flow.as_mut() {
                f.step = WorktreeCreateStep::ConfirmCarryOver;
                f.options.clear();
                f.filter.clear();
                f.cursor = 0;
                f.selected = 0;
                f.error = None;
            }
        } else {
            let projects = crate::tui::mode::worktree::list_project_slugs(self);
            if let Some(f) = self.worktree_create_flow.as_mut() {
                f.step = WorktreeCreateStep::PickProject;
                f.options = projects;
                f.filter.clear();
                f.cursor = 0;
                f.selected = 0;
                f.error = None;
            }
        }
    }

    /// `y`/`n` pressed on the `ConfirmCarryOver` step: records the
    /// choice and advances to `PickProject`, the same as the dirty-check
    /// branch in `worktree_create_after_branch_chosen` above (a clean
    /// repo skips straight past this step, so both paths converge on
    /// the same `PickProject` setup).
    pub(crate) fn worktree_create_confirm_carry_over(&mut self, carry_over: bool) {
        use crate::tui::state::WorktreeCreateStep;
        if self.worktree_create_flow.is_none() {
            return;
        }
        let projects = crate::tui::mode::worktree::list_project_slugs(self);
        if let Some(f) = self.worktree_create_flow.as_mut() {
            f.carry_over = carry_over;
            f.step = WorktreeCreateStep::PickProject;
            f.options = projects;
            f.filter.clear();
            f.cursor = 0;
            f.selected = 0;
            f.error = None;
        }
    }

    /// The final step: create the worktree, optionally carry over
    /// uncommitted changes and bind a project, then stage the `cd`
    /// into it via `stage_cd_to_directory` — the same staging a
    /// Phase-1 row selection uses. A git failure sets `flow.error`
    /// and leaves the dialog open rather than staging anything, so
    /// the user sees what went wrong.
    fn worktree_create_execute(&mut self) -> bool {
        let Some(flow) = self.worktree_create_flow.clone() else {
            return false;
        };
        let path = self.worktree_create_target_path(&flow.repo_root, &flow.branch);
        match crate::tui::mode::worktree::create_worktree(
            &flow.repo_root,
            &path,
            &flow.branch,
            flow.is_new_branch,
            &flow.base_branch,
        ) {
            Ok(()) => {
                if flow.carry_over {
                    // Best-effort past this point: a stash-apply
                    // conflict surfaces as a status message but
                    // doesn't undo the already-created worktree, and
                    // the stash entry itself is never dropped either
                    // way, so nothing is lost even on failure.
                    let _ = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&flow.repo_root)
                        .args(["stash", "push"])
                        .output();
                    let apply = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&path)
                        .args(["stash", "apply"])
                        .output();
                    if let Ok(o) = apply
                        && !o.status.success()
                    {
                        self.set_status_message(format!(
                            "worktree created, but stash apply failed: {}",
                            String::from_utf8_lossy(&o.stderr).trim()
                        ));
                    }
                }
                if let Some(slug) = flow.project_slug.as_ref()
                    && let Err(e) = crate::tui::mode::worktree::write_project_dir_binding(slug, &path)
                {
                    self.set_status_message(format!(
                        "worktree created, but failed to bind project {:?}: {}",
                        slug, e
                    ));
                }
                self.worktree_create_flow = None;
                let dir_str = path.display().to_string();
                self.stage_cd_to_directory(&dir_str);
                self.selection.is_some()
            }
            Err(e) => {
                if let Some(f) = self.worktree_create_flow.as_mut() {
                    f.error = Some(e);
                }
                false
            }
        }
    }

    /// Where a new worktree for `branch` is created: under the
    /// configured `worktree.basedir` when set, otherwise sibling to
    /// the repo (`<repo-parent>/<repo-name>-worktrees/<branch>`).
    fn worktree_create_target_path(
        &self,
        repo_root: &std::path::Path,
        branch: &str,
    ) -> std::path::PathBuf {
        if let Some(base) = self.worktree_basedir.as_ref() {
            base.join(branch)
        } else {
            let parent = repo_root.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| repo_root.to_path_buf());
            let repo_name = repo_root.file_name().and_then(|n| n.to_str()).unwrap_or("repo");
            parent.join(format!("{}-worktrees", repo_name)).join(branch)
        }
    }

    /// Stage the todo (`!`) mode selection.
    ///
    /// Extracted from the legacy monolithic
    /// `select_for_run_legacy_dispatch` and called by
    /// `select_for_run_impl`'s `ModeKind::Todo` arm.
    ///
    /// The body is unchanged from the original — the
    /// todo mode has two sub-paths: the `!@new <text>`
    /// alias (creates a new TODO entry in today's
    /// daily note) and the default (open the selected
    /// todo in `$EDITOR` at the exact line number).
    fn stage_todo_selection(&mut self) {
        // Special case: `!@new <text>` creates a
        // new TODO entry in the daily note by calling
        // `note_search create-note <text>
        // --type daily --timestamp --todo --database <db>`.
        // The `--todo` flag makes `create-note` add the
        // text as a `- [ ] TEXT` todo entry instead of
        // a plain line.
        let pattern = self.todo_pattern();
        if pattern.trim().to_lowercase().starts_with("@new ") {
            let text = pattern.trim()[5..].trim();
            if !text.is_empty() {
                if let Some(ref db_path) = self.notes_database {
                    self.selection = Some(format!(
                        "note_search create-note {} --type daily --timestamp --todo --database {}",
                        crate::util::shell_quote(text),
                        crate::util::shell_quote(&db_path.display().to_string())
                    ));
                    self.pick_mode = Some(PickMode::Run);
                } else {
                    self.set_status_message(
                        "notes.database not configured; set it to use @new".to_string(),
                    );
                }
            }
            return;
        }
        // Default: open the selected todo in $EDITOR at
        // the exact line number.
        if let Some(row) = self.selected_row() {
            let editor = std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "vi".to_string());
            // Recover the 1-based line number
            // from the synthetic id. The id is
            // negative (e.g. -42 means line 42);
            // `i64::MIN` would be its own
            // absolute value, but that's not a
            // valid line number anyway and the
            // mapping is informational, so the
            // overflow edge case doesn't matter.
            let line_number: usize = (row.id.unsigned_abs() as usize).max(1);
            let line_option = self
                .todo_line_option
                .replace("$LINE", &line_number.to_string());
            let filepath = match self.notes_dir.as_ref() {
                Some(dir) => dir.join(&row.comment).to_string_lossy().to_string(),
                None => row.comment.clone(),
            };
            // Quote the path for the shell using POSIX single-quote
            // escaping so inner quotes, backslashes, and other
            // metacharacters cannot break the staged command.
            let quoted = crate::util::shell_quote(&filepath);
            self.selection = Some(format!("{} {} {}", editor, quoted, line_option));
            self.pick_mode = Some(PickMode::Run);
        }
    }

    /// Stage the notes (`@`) mode selection.
    ///
    /// Extracted from the legacy monolithic
    /// `select_for_run_legacy_dispatch` and called by
    /// `select_for_run_impl`'s `ModeKind::Notes` arm.
    ///
    /// Two sub-paths: the `@new <text>` alias (creates
    /// a new daily-note entry) and the default (open
    /// the selected note in `$EDITOR`).
    fn stage_note_selection(&mut self) {
        // Special case: `@new <text>` creates a
        // new daily note entry by calling
        // `note_search create-note <text>
        // --type daily --timestamp --database <db>`.
        // This is the user's "quick add a note
        // from the TUI" path — they type `@new
        // remember to buy milk` and press Enter;
        // the staged command appends a timestamped
        // line to today's daily note.
        let pattern = self.notes_pattern();
        if pattern.trim().to_lowercase().starts_with("new ") {
            let text = pattern.trim()[4..].trim();
            if !text.is_empty() {
                if let Some(ref db_path) = self.notes_database {
                    self.selection = Some(format!(
                        "note_search create-note {} --type daily --timestamp --database {}",
                        crate::util::shell_quote(text),
                        crate::util::shell_quote(&db_path.display().to_string())
                    ));
                    self.pick_mode = Some(PickMode::Run);
                } else {
                    self.set_status_message(
                        "notes.database not configured; set it to use @new".to_string(),
                    );
                }
            }
            return;
        }
        // Default: open the selected note in $EDITOR.
        if let Some(row) = self.selected_row() {
            let editor = std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "vi".to_string());
            // Build the full path to the note file
            let filepath = match self.notes_dir.as_ref() {
                Some(dir) => dir.join(&row.command).to_string_lossy().to_string(),
                None => row.command.clone(),
            };
            // Quote the path for the shell using POSIX single-quote escaping.
            let quoted = crate::util::shell_quote(&filepath);
            self.selection = Some(format!("{} {}", editor, quoted));
            self.pick_mode = Some(PickMode::Run);
        }
    }

    /// Stage the directories (`#`) mode selection.
    ///
    /// Extracted from the legacy monolithic
    /// `select_for_run_legacy_dispatch` and called by
    /// `select_for_run_impl`'s
    /// `ModeKind::Directories` arm.
    ///
    /// Complex tmux / herdr backend logic: `T`-marked
    /// rows (rows with an active tmux pane as cwd)
    /// stage a `select-pane && switch-client` (or
    /// herdr `workspace focus`) command; unmarked rows
    /// stage a `new-session -d -s <basename> -c <dir>`
    /// command. See the original
    /// `select_for_run_legacy_dispatch` doc-comment for
    /// the full rationale on basename collisions and
    /// the `;` shell-safe sequencing.
    fn stage_directory_selection(&mut self) {
        let Some(directory) = self.selected_row().map(|r| r.directory.clone()) else {
            return;
        };
        self.stage_cd_to_directory(&directory);
    }

    /// The tmux/herdr `cd`-staging core `stage_directory_selection`
    /// uses for a selected `#`/`~`/`;` row, extracted so
    /// `Action::CreateWorktree`'s completion step can reuse it for a
    /// freshly-created worktree directory that never had a row to
    /// select in the first place. Builds either a "focus the existing
    /// pane" command (when `directory` already has an active
    /// tmux/herdr context — see `directory_tmux_pane_id`) or a "create
    /// a new session/workspace rooted here" command, then chains in
    /// the directory's `.command` bootstrap script, if any.
    fn stage_cd_to_directory(&mut self, directory: &str) {
        let pane_id = self.directory_tmux_pane_id(directory);
        // Two action paths for
        // directory rows, branched
        // on whether the row has
        // an active tmux window
        // attached (the `T` mark
        // the user sees in the
        // capture column):
        //
        // 1. `T`-marked row: a
        //    tmux window with this
        //    directory as cwd
        //    exists. The user
        //    wants to *jump to* it
        //    — they're in some
        //    other directory, this
        //    is "I had a session
        //    running here earlier".
        //    We stage
        //    `tmux select-pane -t <id> && tmux switch-client -t <id>`
        //    so the parent shell
        //    (which is itself
        //    running in a tmux
        //    client) re-attaches
        //    to the target pane.
        //
        // 2. Unmarked row: no
        //    active tmux window
        //    for this directory.
        //    The user wants a
        //    fresh session rooted
        //    here. We stage
        //    `tmux new-session -d -s <basename> -c <dir>; tmux switch-client -t <basename>`
        //    (the `;` is
        //    shell-safe: the
        //    parent shell eval's
        //    the staged line and
        //    the `new-session` must
        //    finish before
        //    `switch-client` runs).
        //
        // The basename is
        // `std::path::Path::file_name`
        // which returns the
        // trailing path
        // component (e.g.
        // `/Users/har/work` →
        // `work`). If two
        // directories share the
        // same basename (e.g.
        // `/Users/har/x/work`
        // and
        // `/Users/har/y/work`),
        // the second
        // `new-session -s work`
        // will fail with
        // "duplicate session";
        // the parent shell
        // surfaces the error and
        // the user can pick a
        // different action
        // (rename, or `cd
        // manually` first).
        // We don't try to be
        // clever about
        // disambiguation — the
        // error path is rare
        // enough that an
        // explicit user action
        // is preferable.
        if let Some(pane_id) = pane_id.clone() {
            // `T`-marked path:
            // the directory is
            // already the cwd
            // of an active
            // context (a tmux
            // pane or a herdr
            // workspace pane),
            // so we *jump to*
            // that context
            // rather than
            // creating a new
            // one. The exact
            // staged command is
            // backend-specific
            // — tmux wants
            // `select-pane && switch-client`,
            // herdr wants
            // `workspace focus` —
            // and the backend's
            // `focus_command`
            // method returns
            // the right shape
            // (and `None` when
            // the id is stale
            // or the backend
            // can't build a
            // focus command).
            if let Some(cmd) = self.multiplexer.focus_command(&pane_id) {
                self.selection = Some(cmd);
            } else {
                self.set_status_message(format!(
                    "{} context {} is no longer available; cannot focus",
                    self.multiplexer.name(),
                    pane_id
                ));
                return;
            }
        } else {
            // Unmarked path: open
            // a fresh context
            // rooted at the
            // directory. The
            // basename of the
            // directory is used
            // as a human-readable
            // label (tmux session
            // name, herdr
            // workspace label);
            // collisions are
            // surfaced by the
            // backend (tmux fails
            // with "duplicate
            // session", herdr
            // auto-suffixes the
            // positional id) and
            // the parent shell
            // surfaces the error.
            //
            // Path quoting /
            // `~` expansion /
            // `--focus` are
            // handled inside the
            // backend's
            // `create_command`;
            // the staging layer
            // just hands it the
            // directory and the
            // label and trusts
            // the backend to
            // produce a
            // shell-safe string.
            let path = crate::util::expand_home(directory).into_owned();
            let label = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("smarthistory")
                .to_string();
            if let Some(cmd) = self
                .multiplexer
                .create_command(std::path::Path::new(&path), &label)
            {
                self.selection = Some(cmd);
            } else {
                self.set_status_message(format!(
                    "could not build a create command for {}",
                    self.multiplexer.name()
                ));
                return;
            }
        }
        // `.command` chain. If
        // the directory (or an
        // ancestor) has a
        // `.command` file, run
        // it with the
        // directory as the
        // first argument. The
        // lookup walks up the
        // parent tree, so a
        // `project/.command`
        // fires for any
        // selection under
        // `project/`. The
        // `.command` is run
        // *inside* the new
        // session (so it
        // affects the new
        // session's
        // environment) via
        // `tmux send-keys`.
        // For the `T`-marked
        // branch (jumping to
        // an existing pane)
        // we still run the
        // command, since the
        // user explicitly
        // picked the row and
        // we shouldn't second-
        // guess their intent.
        //
        // Form:
        //   tmux send-keys -t <pane> "sh <command-file> <dir>" Enter
        //
        // The `sh` wrapper
        // means the file
        // doesn't need to be
        // executable. The
        // first argument is
        // always the selected
        // directory; the
        // .command script can
        // use `$1` (or `$@`
        // for the full arg
        // list) to read it.
        //
        // The chain uses `;`
        // (not `&&`) for the
        // `T`-marked branch:
        // the user wants the
        // jump to happen
        // even if the
        // .command script
        // fails. A `.command`
        // author who needs
        // the jump to fail
        // on script failure
        // can `exit 1` from
        // the script and the
        // user will see the
        // non-zero exit in
        // the parent shell.
        //
        // For the unmarked
        // branch (new
        // session) we *wait*
        // for the .command
        // to finish before
        // switch-client, so
        // the user lands in
        // a session that
        // already has the
        // project set up.
        // This is `&&`
        // between the
        // command and the
        // switch-client.
        if let Some(cmd_path) = crate::util::find_command_file(std::path::Path::new(directory)) {
            let path_for_arg = crate::util::expand_home(directory).into_owned();
            let quoted_arg = crate::util::shell_quote(&path_for_arg);
            let quoted_cmd = crate::util::shell_quote(&cmd_path.display().to_string());
            // The script body:
            // `sh <file> <dir>`.
            // The first argument
            // is always the
            // selected directory
            // (the user said so).
            let command_run = format!("sh {} {}", quoted_cmd, quoted_arg);
            if let Some(pane_id_inner) = pane_id.as_ref() {
                // T-marked
                // branch: chain
                // the bootstrap
                // via
                // `self.multiplexer.send_in_pane_command`
                // (tmux
                // `send-keys`,
                // herdr
                // `pane send-text`).
                // The
                // existing
                // `selection`
                // (the focus
                // command
                // staged
                // above) is
                // preserved;
                // the
                // bootstrap
                // script
                // appends
                // after a `;`
                // so the
                // jump still
                // happens
                // even on
                // script
                // failure.
                // If the
                // backend
                // can't build
                // a
                // send-in-pane
                // command
                // (the id is
                // stale,
                // etc.), we
                // silently
                // keep the
                // bare focus
                // command
                // already
                // staged; the
                // user gets
                // their jump
                // even if the
                // bootstrap
                // script
                // doesn't
                // run.
                if let Some(send_cmd) = self
                    .multiplexer
                    .send_in_pane_command(pane_id_inner, &command_run)
                {
                    let existing = self.selection.take().unwrap_or_default();
                    self.selection = Some(format!("{} ; {}", existing, send_cmd));
                }
            } else {
                // Unmarked
                // branch.
                // For tmux:
                // the
                // bootstrap
                // script
                // runs
                // *inside*
                // the new
                // session's
                // first
                // command
                // position
                // (the
                // session is
                // created
                // with the
                // project
                // already
                // set up
                // when
                // `switch-client`
                // takes
                // effect).
                // The shape:
                //   tmux new-session -d -s NAME -c DIR ; sh FILE DIR ; tmux switch-client -t NAME
                // For herdr:
                // `workspace create`
                // doesn't
                // currently
                // accept a
                // startup
                // command,
                // so we
                // degrade
                // to the
                // bare
                // create
                // command
                // already
                // staged
                // (the
                // bootstrap
                // script
                // would
                // need to
                // be
                // re-run
                // after
                // the
                // workspace
                // is up).
                // The
                // user can
                // re-select
                // the row
                // to
                // retry
                // the
                // bootstrap
                // once the
                // workspace
                // is
                // open —
                // smarthistory
                // has no
                // way to
                // chain a
                // send-text
                // to a
                // workspace
                // it
                // doesn't
                // yet
                // know
                // the id
                // of.
                if self.multiplexer.name() == "tmux" {
                    let path = crate::util::expand_home(directory).into_owned();
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("smarthistory")
                        .to_string();
                    let quoted_path = crate::util::shell_quote(&path);
                    let quoted_name = crate::util::shell_quote(&name);
                    self.selection = Some(format!(
                        "tmux new-session -d -s {} -c {}; \
                                 sh {} {}; \
                                 tmux switch-client -t {}",
                        quoted_name, quoted_path, quoted_cmd, quoted_arg, quoted_name
                    ));
                }
                // For herdr
                // (or any
                // other
                // backend
                // without
                // a
                // create-with-command
                // flag),
                // the bare
                // `create_command`
                // is
                // already
                // staged.
                // No-op
                // here.
            }
        }
        self.pick_mode = Some(PickMode::Run);
    }

    /// Stage the zoxide (`~`) mode selection. Zoxide rows are
    /// tagged `mode == "directory"`, the same tag `#` Directories
    /// mode's rows carry, so the underlying create/focus staging
    /// (`stage_directory_selection`) is identical — this wrapper
    /// only adds a one-time prompt on top: if the directory isn't
    /// already a configured `session.<id>` entry, defer the actual
    /// staging and open `zoxide_save_prompt` instead, asking
    /// whether to save it (see that field's doc comment in
    /// `src/tui/state.rs` and `App::answer_zoxide_save_prompt` for
    /// the rest of the flow). Directories mode itself is untouched
    /// — this dispatch only runs for `ModeKind::Zoxide`.
    ///
    /// A directory with no path (defensive — shouldn't happen for a
    /// real zoxide row) skips the prompt and stages directly, same
    /// as an already-saved directory.
    fn stage_zoxide_selection(&mut self) {
        let Some(directory) = self.selected_row().map(|r| r.directory.clone()) else {
            return;
        };
        if directory.is_empty() {
            self.stage_directory_selection();
            return;
        }
        let canonical = crate::util::canonicalize_directory(&directory);
        let already_saved = self
            .sessions
            .iter()
            .any(|s| crate::util::canonicalize_directory(&s.directory) == canonical);
        if already_saved {
            self.stage_directory_selection();
            return;
        }
        let label = std::path::Path::new(&directory)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("smarthistory")
            .to_string();
        self.zoxide_save_prompt = Some(crate::tui::state::ZoxideSavePrompt { label, directory });
    }

    /// Answer the `zoxide_save_prompt` opened by
    /// `stage_zoxide_selection`. `save = true` writes a new
    /// `session.<id>` entry (name + `.dir` only — deliberately no
    /// `.exec`, per how the user described the feature: "create
    /// this entry automatically without any specific execution
    /// flags") by reusing the exact same `AddEntryDialog` /
    /// `write_new_entry_to_config` machinery the `F5` "add session"
    /// dialog uses, just built and submitted programmatically
    /// instead of interactively — a real `AddEntryDialog` is
    /// constructed (pre-filled the same way `F5` would pre-fill it
    /// from a selected row) and immediately committed, so it's
    /// never actually shown to the user.
    ///
    /// Either way — saved or not — the ORIGINAL directory selection
    /// (create/focus the tmux/herdr session) always runs afterward:
    /// the prompt only decides whether to ALSO save the directory,
    /// it never blocks the jump the user actually pressed `Enter`
    /// for.
    pub(crate) fn answer_zoxide_save_prompt(&mut self, save: bool) {
        let Some(prompt) = self.zoxide_save_prompt.take() else {
            return;
        };
        // Stage the original directory jump FIRST, while the current
        // row selection is still intact. `write_new_entry_to_config`
        // (below) calls `self.refresh()`, which can re-fetch
        // `merged_rows` (e.g. re-running the real zoxide query) and
        // reset `list_state` — if staging ran AFTER that,
        // `stage_directory_selection`'s `self.selected_row()` could
        // read a different row or find none at all, staging the
        // wrong directory (or nothing). Staging first makes the jump
        // immune to any side effect of the save step.
        self.stage_directory_selection();
        if save {
            let mut dialog = crate::tui::state::AddEntryDialog::new(
                crate::tui::state::AddEntryKind::Session,
                prompt.directory.clone(),
                String::new(),
            );
            // `AddEntryDialog::new` leaves the required `Name`
            // field blank (it's normally typed by hand) — fill it
            // with the directory's basename so the programmatic
            // commit below has a valid entry to write.
            if let Some(name_field) = dialog.fields.first_mut() {
                name_field.value = prompt.label.clone();
            }
            self.add_entry_dialog = Some(dialog);
            if let Err(e) = self.write_new_entry_to_config() {
                self.set_status_message(format!("could not save directory: {}", e));
            }
        }
    }

    /// Open the signal-confirmation dialog for the selected process
    /// (`%` mode) row. Deliberately does NOT set `self.selection` /
    /// `self.pick_mode` — unlike every other mode's staging, Enter
    /// here must not close the TUI and hand a command to the parent
    /// shell; it opens `confirm_signal` and waits for `y`/`n`/Tab,
    /// same defer-don't-stage pattern as `stage_zoxide_selection`
    /// above deferring behind `zoxide_save_prompt`.
    fn stage_process_signal_prompt(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.mode != "process" {
            return;
        }
        self.confirm_signal = Some(crate::tui::SignalConfirm {
            pid: row.id,
            name: row.command.clone(),
            signal: crate::tui::ProcessSignal::Term,
        });
    }

    /// Stage the panes (`*`) mode selection.
    ///
    /// Extracted from the legacy monolithic
    /// `select_for_run_legacy_dispatch` and called by
    /// `select_for_run_impl`'s `ModeKind::Panes` arm.
    ///
    /// Switches to the selected pane / window /
    /// session in the configured multiplexer backend
    /// (tmux or herdr). The complex backend dispatch
    /// lives in `MultiplexerBackend::focus_command`.
    fn stage_pane_selection(&mut self) {
        // Populate the tmux-windows
        // snapshot used by the
        // session-row matcher below.
        // `App::refresh` only calls
        // `fetch_tmux_windows` for
        // directories mode, so the
        // `*` view's `tmux_windows`
        // is otherwise empty when
        // the user opens the picker
        // with `*` as the first
        // character — and the
        // matcher below would always
        // fall into the "create"
        // branch, duplicating an
        // existing herdr/tmux
        // workspace on every Enter.
        // The fetch is idempotent
        // (returns immediately when
        // the cache is populated) so
        // re-Enter doesn't re-spawn
        // the multiplexer.
        crate::tui::mode::directories::ensure_multiplexer_snapshot(self);
        // The `*` mode now shows
        // a **tree**:
        //   workspace_header
        //     · pane_row
        //     · pane_row
        //   workspace_header
        //     · pane_row
        // Selecting a workspace
        // header stages
        // `self.multiplexer.focus_session(session_label)`;
        // selecting a pane row
        // stages
        // `self.multiplexer.focus_pane(pane_id, tab_id)`.
        // The dispatch happens
        // based on the row's
        // `mode` field —
        // `"workspace"` for
        // header rows, `"pane"`
        // for pane rows.
        let row = match self.selected_row() {
            Some(r) => r,
            None => return,
        };
        match row.mode.as_str() {
            "workspace" => {
                // Group headers (`Sessions`, `Directories`, `hosts`
                // — the synthetic wrapper + the two configured
                // sections, as opposed to an individual live
                // workspace's own `## <label>` sub-heading, whose
                // `source` is plain `"workspace"`) collapse/expand
                // on `Enter` instead of staging a focus command —
                // their `session_id` is either empty (`Sessions`) or
                // the literal string `"sessions"`/`"hosts"`, neither
                // of which was ever a real focusable target, so this
                // replaces a staged command that always silently
                // failed with something actually useful.
                if matches!(row.source.as_str(), "workspace-group" | "sessions" | "hosts") {
                    self.toggle_pane_group_collapsed(&row.command.clone());
                    return;
                }
                let label = row.session_id.clone();
                if label.is_empty() {
                    return;
                }
                if let Some(cmd) = self.multiplexer.focus_session(&label) {
                    self.selection = Some(cmd);
                    self.pick_mode = Some(PickMode::Run);
                    // Record `last_touched`
                    // for every pane in
                    // this workspace so
                    // the workspace header
                    // bubbles to the top
                    // of the panes list on
                    // the next refresh
                    // (the within-group
                    // max-touched sort
                    // in
                    // `refresh_session_panes_impl`
                    // is what floats
                    // it). We don't know
                    // which specific pane
                    // the multiplexer
                    // landed the user on
                    // (herdr's
                    // `workspace focus`
                    // is workspace-scoped,
                    // not pane-scoped), so
                    // bumping all panes
                    // in the workspace
                    // is the cleanest
                    // approximation —
                    // the user navigated
                    // to this workspace,
                    // so the whole
                    // workspace is
                    // "new".
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    for pane in self
                        .session_panes
                        .iter()
                        .filter(|r| r.mode == "pane" && r.workspace_label == label)
                    {
                        self.pane_last_touched.insert(pane.session_id.clone(), now);
                    }
                } else {
                    self.set_status_message(format!(
                        "{} workspace {} is no longer available",
                        self.multiplexer.name(),
                        label
                    ));
                }
            }
            "pane" => {
                let pane_id = row.session_id.clone();
                // The pane's tab_id is
                // stashed in `row.output`
                // (for backward-compat with
                // older pane rows that
                // didn't carry it, the
                // backend's `focus_pane`
                // degrades to a
                // workspace-level focus).
                let tab_id = row.output.clone();
                if pane_id.is_empty() {
                    return;
                }
                if let Some(cmd) = self.multiplexer.focus_pane(&pane_id, &tab_id) {
                    self.selection = Some(cmd);
                    self.pick_mode = Some(PickMode::Run);
                    // Record this pane's
                    // `last_touched` so it
                    // bubbles to the top
                    // of its workspace
                    // group on the next
                    // refresh. The
                    // within-group sort
                    // in
                    // `refresh_session_panes_impl`
                    // picks the
                    // `last_touched`
                    // column, so the
                    // just-focused pane
                    // becomes the
                    // topmost row of
                    // its workspace
                    // (and the
                    // workspace's max
                    // also gets bumped,
                    // so it bubbles to
                    // the top of the
                    // outer list too).
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    self.pane_last_touched.insert(pane_id, now);
                } else {
                    self.set_status_message(format!(
                        "{} pane {} is no longer available",
                        self.multiplexer.name(),
                        pane_id
                    ));
                }
            }
            "session" => {
                let name = row.command.clone().trim().to_string();
                let dir = row.directory.clone();
                let exec = row.comment.clone();
                let quoted_exec = crate::util::shell_quote(&exec);
                let home_list: Vec<String> =
                    std::iter::once(std::env::var("HOME").unwrap_or_default())
                        .filter(|s| !s.is_empty())
                        .collect();
                let abs = crate::util::expand_home_to_absolute(&dir, &home_list).into_owned();
                let quoted_dir = if abs
                    .chars()
                    .any(|c| c.is_whitespace() || "<>|&;\"'$`\\".contains(c))
                {
                    format!("\"{}\"", abs)
                } else {
                    abs.clone()
                };
                let quoted_label = crate::util::shell_quote(&name);
                // Check if a workspace with a matching LABEL already
                // exists. The session's display name (e.g.
                // `Proxmox`, `Downloads`) is matched against the
                // workspace's `workspace_label` (the human-readable
                // name from `herdr workspace list`'s `label` field).
                // This is different from the host matcher (which
                // matches by label too) and from the old directory-
                // based matcher (which checked if any pane's cwd
                // matched the session's `dir` — that was too
                // broad: a pane running in the same directory but
                // under a different workspace label would falsely
                // match, preventing the user from creating a new
                // dedicated workspace).
                let existing = self
                    .tmux_windows
                    .iter()
                    .find(|w| w.workspace_label == name)
                    .map(|w| w.pane_id.clone());
                let cmd = if let Some(ref pane_id) = existing {
                    // Workspace exists — focus it (+ optionally exec).
                    if self.multiplexer.name() == "herdr" {
                        let ws_id = pane_id.split(':').next().unwrap_or(pane_id);
                        if exec.is_empty() {
                            format!("herdr workspace focus {} 2>/dev/null", ws_id)
                        } else {
                            format!(
                                "herdr workspace focus {} 2>/dev/null && herdr pane run \"{}\" {}",
                                ws_id, pane_id, quoted_exec
                            )
                        }
                    } else {
                        format!(
                            "tmux select-pane -t {} && tmux switch-client -t {}",
                            pane_id, pane_id
                        )
                    }
                } else {
                    // No existing workspace — create one.
                    if self.multiplexer.name() == "herdr" {
                        if exec.is_empty() {
                            format!(
                                "herdr workspace create --cwd {} --label {} --focus 2>/dev/null",
                                quoted_dir, quoted_label
                            )
                        } else {
                            format!(
                                    "WS=$(herdr workspace create --cwd {} --label {} 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)[\"result\"][\"workspace\"][\"workspace_id\"])' 2>/dev/null) && herdr pane run \"$WS:p1\" {} && herdr workspace focus \"$WS\"",
                                    quoted_dir, quoted_label, quoted_exec
                                )
                        }
                    } else {
                        let base = self
                            .multiplexer
                            .create_command(std::path::Path::new(&abs), &name)
                            .unwrap_or_default();
                        if exec.is_empty() {
                            base
                        } else {
                            format!("{} ; {}", base, quoted_exec)
                        }
                    }
                };
                self.selection = Some(cmd);
                self.pick_mode = Some(PickMode::Run);
            }
            "host" => {
                // The `# hosts` block.
                // Each host row has a
                // display name in
                // `command` and a
                // `user@host:port`
                // connection string in
                // `directory`. The full
                // `HostDef` is looked
                // up by row position
                // (the row's synthetic
                // id maps to the
                // `host_defs` index
                // directly).
                let display_name = row.command.clone();
                let connection_string = row.directory.clone();
                // The synthetic id
                // scheme is
                // `-25_000 - <position>` (set by
                // `fetch_session_panes_impl`),
                // so the
                // position in
                // `self.hosts` /
                // `self.host_defs` is
                // `-row.id - 25_000 - 1`
                // (0-indexed).
                let host_pos = (-row.id - 25_000 - 1) as usize;
                let host_def = self.host_defs.get(host_pos).cloned();
                let host_def = match host_def {
                    Some(d) => d,
                    None => {
                        // The id
                        // scheme
                        // is
                        // out-of-sync
                        // with
                        // `self.hosts`
                        // (shouldn't
                        // happen,
                        // but
                        // surface
                        // a
                        // status
                        // message
                        // rather
                        // than
                        // panicking).
                        self.set_status_message("host definition not found".to_string());
                        return;
                    }
                };
                // Build the `ssh` argv from the full `HostDef` — only
                // the flags that are actually set. Shared with
                // `smarthistory pane-exec` via `HostDef::ssh_command`.
                let ssh_body = host_def.ssh_command();
                let target = if host_def.hostname.is_empty() {
                    host_def.host.clone()
                } else {
                    host_def.hostname.clone()
                };
                let quoted_body = crate::util::shell_quote(&ssh_body);
                let exec = host_def.exec.clone();
                // Match against
                // existing
                // workspaces. tmux:
                // any pane whose
                // `current_command`
                // starts with
                // `ssh` and contains
                // the connection
                // string. herdr:
                // any workspace
                // whose
                // `workspace_label`
                // matches the host's
                // display name
                // (herdr's
                // foreground-command
                // field is empty).
                let existing_pane_id: Option<String> = if self.multiplexer.name() == "tmux" {
                    self.tmux_windows
                        .iter()
                        .find(|w| {
                            w.current_command.starts_with("ssh")
                                && (w.current_command.contains(&connection_string)
                                    || w.current_command.contains(&target))
                        })
                        .map(|w| w.pane_id.clone())
                } else {
                    // herdr: match by
                    // workspace
                    // label. We
                    // accept the
                    // host's display
                    // name OR a
                    // `host:<name>`
                    // label (the
                    // user might
                    // have manually
                    // renamed the
                    // workspace).
                    self.tmux_windows
                        .iter()
                        .find(|w| {
                            w.workspace_label == display_name
                                || w.workspace_label == format!("host:{}", display_name)
                        })
                        .map(|w| w.pane_id.clone())
                };
                let cmd = if let Some(ref pane_id) = existing_pane_id {
                    // Workspace
                    // already
                    // exists —
                    // focus it
                    // (and
                    // optionally
                    // run the
                    // post-connect
                    // command).
                    if self.multiplexer.name() == "herdr" {
                        let ws_id = pane_id.split(':').next().unwrap_or(pane_id);
                        if exec.is_empty() {
                            format!("herdr workspace focus {} 2>/dev/null", ws_id,)
                        } else {
                            // Use `pane run` (same as
                            // the named-session
                            // technique) — it executes
                            // the command directly in
                            // the pane without needing
                            // a separate
                            // `pane send-keys Enter`
                            // to submit it.
                            format!(
                                    "herdr workspace focus {} 2>/dev/null && herdr pane run {} {} 2>/dev/null",
                                    ws_id,
                                    pane_id,
                                    crate::util::shell_quote(&exec),
                                )
                        }
                    } else {
                        // tmux:
                        // focus the
                        // pane
                        // (the
                        // `ssh`
                        // body is
                        // already
                        // running
                        // there).
                        if exec.is_empty() {
                            format!(
                                "tmux select-pane -t {} && tmux switch-client -t {}",
                                pane_id, pane_id,
                            )
                        } else {
                            format!(
                                    "tmux select-pane -t {} && tmux switch-client -t {} && tmux send-keys -t {} {} Enter",
                                    pane_id,
                                    pane_id,
                                    pane_id,
                                    crate::util::shell_quote(&exec),
                                )
                        }
                    }
                } else {
                    // No
                    // existing
                    // workspace
                    // — create
                    // one and
                    // bootstrap
                    // the `ssh`
                    // connection
                    // inside.
                    if self.multiplexer.name() == "herdr" {
                        // herdr
                        // doesn't
                        // accept a
                        // startup
                        // command
                        // on
                        // `workspace
                        // create`,
                        // so we
                        // create
                        // first
                        // and
                        // send the
                        // `ssh`
                        // body
                        // into the
                        // first
                        // pane
                        // via
                        // `pane
                        // send-text`.
                        let quoted_label = crate::util::shell_quote(&display_name);
                        if exec.is_empty() {
                            // Use `pane run` (same as
                            // the named-session
                            // technique) — it executes
                            // the `ssh` body directly
                            // in the new workspace's
                            // first pane. No need for
                            // `pane send-text` +
                            // `pane send-keys Enter`.
                            format!(
                                    "WS=$(herdr workspace create --label {} 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)[\"result\"][\"workspace\"][\"workspace_id\"])' 2>/dev/null) && herdr pane run \"$WS:p1\" {} && herdr workspace focus \"$WS\"",
                                    quoted_label, quoted_body,
                                )
                        } else {
                            // Same technique: `pane run`
                            // for the exec, then focus
                            // the workspace. The exec
                            // runs inside the SSH
                            // session's PTY (sent
                            // after the SSH body lands
                            // in the remote shell's
                            // stdin).
                            format!(
                                    "WS=$(herdr workspace create --label {} 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)[\"result\"][\"workspace\"][\"workspace_id\"])' 2>/dev/null) && herdr pane run \"$WS:p1\" {} && herdr pane run \"$WS:p1\" {} && herdr workspace focus \"$WS\"",
                                    quoted_label,
                                    quoted_body,
                                    crate::util::shell_quote(&exec),
                                )
                        }
                    } else {
                        // tmux:
                        // create a
                        // new
                        // session
                        // (no cwd
                        // — the
                        // user
                        // wants
                        // the SSH
                        // connection,
                        // not a
                        // local
                        // dir) and
                        // send
                        // the
                        // `ssh`
                        // body
                        // into the
                        // new
                        // pane.
                        let quoted_label = crate::util::shell_quote(&display_name);
                        if exec.is_empty() {
                            format!(
                                    "tmux new-session -d -s {}; tmux switch-client -t {}; tmux send-keys {} Enter",
                                    quoted_label, quoted_label, quoted_body,
                                )
                        } else {
                            format!(
                                    "tmux new-session -d -s {}; tmux switch-client -t {}; tmux send-keys {} Enter; tmux send-keys {} Enter",
                                    quoted_label,
                                    quoted_label,
                                    quoted_body,
                                    crate::util::shell_quote(&exec),
                                )
                        }
                    }
                };
                self.selection = Some(cmd);
                self.pick_mode = Some(PickMode::Run);
            }
            _ => {
                // Unknown row mode in
                // the `*` view —
                // silently ignore
                // (shouldn't happen
                // but no status
                // message so the user
                // doesn't get a
                // confusing hint).
            }
        }
    }

    /// Stage the JIRA (`-`) mode selection.
    ///
    /// Extracted from the legacy monolithic
    /// `select_for_run_legacy_dispatch` and called by
    /// `select_for_run_impl`'s `ModeKind::Jira` arm.
    ///
    /// Stages a `open <browse_url>` (macOS) or
    /// `xdg-open <browse_url>` (Linux) command for
    /// the selected issue's browse URL. When JIRA is
    /// not configured, surfaces a status message via
    /// `set_status_message` instead of staging a
    /// malformed command.
    fn stage_jira_selection(&mut self) {
        let key: String = match self.selected_row() {
            Some(r) => r.command.clone(),
            None => return,
        };
        if key.is_empty() {
            return;
        }
        match crate::jira::JiraConfig::from_env() {
            Some(cfg) => {
                let url = cfg.browse_url(&key);
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                self.selection = Some(format!("{} \"{}\"", opener, url));
                self.pick_mode = Some(PickMode::Run);
            }
            None => {
                self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            }
        }
    }

    /// Stage the paperless (`<`) mode selection: open the
    /// selected document's details page in the system browser.
    /// Mirrors `stage_jira_selection` — the document id is
    /// recovered from the row's synthetic negative `id` (see
    /// `paperless::document_to_row`), and the browse URL is
    /// rebuilt from the configured `paperless.url` rather than
    /// stored on the row.
    fn stage_paperless_selection(&mut self) {
        let id: i64 = match self.selected_row() {
            Some(r) => r.id.unsigned_abs() as i64,
            None => return,
        };
        match self.paperless_config.as_ref() {
            Some(cfg) => {
                let url = cfg.document_url(id);
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                self.selection = Some(format!("{} \"{}\"", opener, url));
                self.pick_mode = Some(PickMode::Run);
            }
            None => {
                self.set_status_message(
                    crate::paperless::PaperlessError::NotConfigured.to_string(),
                );
            }
        }
    }

    /// Stage the browser (`^`) mode selection: open the selected
    /// bookmark/history row's URL in the system browser. Unlike
    /// `stage_jira_selection` / `stage_paperless_selection`, the
    /// URL doesn't need to be reconstructed from a config + id —
    /// the row already carries it verbatim in `comment` (see
    /// `browser::browser_entry_to_row`), since the row was read
    /// straight from the browser's own bookmarks/history file.
    ///
    /// Unlike those two modes (whose URL is either an API-returned
    /// numeric id or a JIRA key, both effectively ASCII
    /// identifiers), a browser history/bookmark URL is arbitrary
    /// web content the user merely *visited* — a page designed to
    /// plant a shell-metacharacter-laden URL in the user's history
    /// is a realistic threat model a naive `"<url>"` double-quoted
    /// splice wouldn't be safe against (double quotes still expand
    /// `$(...)` / backticks). `shell_quote` applies the same
    /// POSIX single-quote escaping used everywhere else in this
    /// codebase that splices untrusted text into a staged command.
    fn stage_browser_selection(&mut self) {
        let url: String = match self.selected_row() {
            Some(r) => r.comment.clone(),
            None => return,
        };
        if url.is_empty() {
            return;
        }
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        self.selection = Some(format!("{} {}", opener, crate::util::shell_quote(&url)));
        self.pick_mode = Some(PickMode::Run);
    }

    /// Stage the history (no-prefix) mode selection.
    ///
    /// Extracted from the legacy monolithic
    /// `select_for_run_legacy_dispatch` and called by
    /// `select_for_run_impl`'s `ModeKind::History`
    /// fall-through arm.
    ///
    /// The default row-staging behaviour: the selected
    /// row's `command` text is staged (and the TUI
    /// exits) so the parent shell runs it. Special
    /// cases for old LLM / question rows (where the
    /// generated command is in `row.output`) re-route
    /// to the same staging logic the Enter key used
    /// to perform.
    fn stage_history_selection(&mut self) {
        if let Some(row) = self.selected_row() {
            // Check the mode field to determine the type of entry.
            if row.mode == "llm" && !row.output.is_empty() {
                // Old LLM query: execute the output (the generated command).
                self.selection = Some(row.output.clone());
                self.pick_mode = Some(PickMode::Run);
            } else if row.mode == "question" && !row.output.is_empty() {
                // Old question: show the answer in the overlay.
                self.question_view = Some(QuestionView {
                    question: row.command.clone(),
                    text: row.output.clone(),
                    scroll: 0,
                });
            } else {
                self.selection = Some(row.command.clone());
                self.pick_mode = Some(PickMode::Run);
            }
        }
    }
}
