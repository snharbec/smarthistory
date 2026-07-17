// Staging actions for the TUI: what happens when the user presses
// Enter/Left/Right on a selected row. This was extracted from
// src/tui.rs to reduce the size of the main TUI module.

use super::*;

impl App {
    pub(crate) fn select_for_run_impl(&mut self) {
        // `=...` queries are an LLM command-generation request,
        // not a row selection. Short-circuit before any row
        // lookup: there *is* no meaningful selected row when
        // the user is composing a natural-language description.
        if self.is_llm_query() {
            self.run_llm_query();
            return;
        }
        // `%...` queries are general question requests.
        // Open an overlay with the answer instead of running
        // a command.
        if self.is_question_query() {
            self.run_question_query();
            return;
        }
        // `!...` queries are todo search requests.
        // Selecting a todo line opens the editor at
        // the exact line number so the user lands
        // on the todo. The `id` of a todo row is
        // `-(line_number)` (synthetic negative id),
        // so we recover the line number with
        // `i64::abs() as usize`.
        if self.is_todo_query() {
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
            return;
        }
        // `@...` queries are note search requests.
        // Selecting a note opens it in the editor.
        if self.is_notes_query() {
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
            return;
        }

        // `~...` queries are file-search requests.
        // Selecting a file opens it in the editor.
        if self.is_files_query() {
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
            return;
        }
        // `$...` queries are tags-search requests.
        // Selecting a symbol opens the file in the
        // editor at the correct line, using
        // `+LINE_NUMBER` (the convention nvim, vim,
        // and most CLI editors understand).
        if self.is_tags_query() {
            if let Some(row) = self.selected_row() {
                let editor = std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vi".to_string());
                // The absolute path is in
                // `row.directory`, the line
                // number is in `row.session_id`.
                let filepath = &row.directory;
                let line = &row.session_id;
                let quoted = crate::util::shell_quote(filepath);
                self.selection = Some(format!("{} +{} {}", editor, line, quoted,));
                self.pick_mode = Some(PickMode::Run);
            }
            return;
        }
        // `,` queries are ag content-search requests.
        // Selecting a match opens the file in $EDITOR
        // at the matching line number.
        if self.is_ag_query() {
            if let Some(row) = self.selected_row() {
                let editor = std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vi".to_string());
                let filepath = &row.directory;
                let line = &row.session_id;
                let quoted = crate::util::shell_quote(filepath);
                self.selection = Some(format!("{} +{} {}", editor, line, quoted,));
                self.pick_mode = Some(PickMode::Run);
            }
            return;
        }
        // `&` queries are CodeGraph symbol-search
        // requests. Selecting a symbol opens the
        // source file in $EDITOR at the symbol's
        // `start_line`, exactly like tags mode
        // (the row's `directory` and `session_id`
        // carry the absolute path and line).
        if self.is_codegraph_query() {
            if let Some(row) = self.selected_row() {
                let editor = std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "vi".to_string());
                let filepath = &row.directory;
                let line = &row.session_id;
                let quoted = crate::util::shell_quote(filepath);
                self.selection = Some(format!("{} +{} {}", editor, line, quoted,));
                self.pick_mode = Some(PickMode::Run);
            }
            return;
        }

        // `#...` queries are directories-view
        // requests. Selecting a
        // directory stages `cd
        // <abs-path>` so the
        // parent shell changes
        // cwd to that directory.
        // The selection routes
        // through the TUI's normal
        // exit-and-run path
        // (`PickMode::Run` with
        // `selection` set), so
        // any path with spaces is
        // quoted by the parent
        // shell as a single arg
        // (the path is the only
        // argument to `cd`).
        if self.is_directories_query() {
            // Clone the row's
            // `directory` (and
            // the resolved tmux
            // pane id) up front
            // so the rest of the
            // block can mutate
            // `self.selection`
            // without fighting
            // the borrow
            // checker. We can't
            // hold the
            // `selected_row()`
            // borrow across
            // `self.selection =`
            // assignments.
            let (directory, pane_id): (String, Option<String>) = match self.selected_row() {
                Some(r) => (
                    r.directory.clone(),
                    self.directory_tmux_pane_id(&r.directory),
                ),
                None => return,
            };
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
                let path = crate::util::expand_home(&directory).into_owned();
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
            if let Some(cmd_path) = crate::util::find_command_file(std::path::Path::new(&directory))
            {
                let path_for_arg = crate::util::expand_home(&directory).into_owned();
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
                        let path = crate::util::expand_home(&directory).into_owned();
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
            return;
        }
        // `*...` queries are the
        // panes-in-the-current-context
        // view. Selecting a
        // pane stages a
        // command that jumps
        // the user's client to
        // that pane. The exact
        // shape is
        // backend-specific: tmux
        // needs
        // `select-window -t <window_id> && select-pane -t <pane_id>`
        // (plain
        // `select-pane` does
        // NOT switch windows,
        // so a pane in another
        // window would be
        // unreachable); herdr
        // has a single
        // `workspace focus`
        // command. The current
        // pane is excluded from
        // the list at fetch
        // time so the user
        // never stages a no-op
        // jump to themselves.
        // The
        // `self.multiplexer.focus_command`
        // method owns the
        // exact shape per
        // backend; the staging
        // layer just hands it
        // the pane id.
        if self.is_panes_query() {
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
            self.fetch_tmux_windows();
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
                    let label = row.session_id.clone();
                    if label.is_empty() {
                        return;
                    }
                    if let Some(cmd) = self.multiplexer.focus_session(&label) {
                        self.selection = Some(cmd);
                        self.pick_mode = Some(PickMode::Run);
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
                    // Build the `ssh`
                    // argv from the
                    // full `HostDef`.
                    // Only include
                    // flags that are
                    // actually set.
                    let effective_user = if host_def.user.is_empty() {
                        std::env::var("USER").unwrap_or_default()
                    } else {
                        host_def.user.clone()
                    };
                    let target = if host_def.hostname.is_empty() {
                        host_def.host.clone()
                    } else {
                        host_def.hostname.clone()
                    };
                    let mut ssh_body = String::from("ssh");
                    if host_def.port != 0 && host_def.port != 22 {
                        ssh_body.push_str(&format!(" -p {}", host_def.port));
                    }
                    if !host_def.identity.is_empty() {
                        let home_list: Vec<String> =
                            std::iter::once(std::env::var("HOME").unwrap_or_default())
                                .filter(|s| !s.is_empty())
                                .collect();
                        let id_path =
                            crate::util::expand_home_to_absolute(&host_def.identity, &home_list);
                        ssh_body.push_str(&format!(" -i {}", crate::util::shell_quote(&id_path),));
                    }
                    if !effective_user.is_empty() {
                        ssh_body.push_str(&format!(" {}@{}", effective_user, target,));
                    } else {
                        ssh_body.push_str(&format!(" {}", target));
                    }
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
            return;
        }
        // `-...` queries are JIRA issue-search
        // requests. Selecting an issue opens its
        // browse URL (`JIRA_URL/<key>`) in the
        // system browser: `open` on macOS,
        // `xdg-open` on other Unixes. The key is
        // the row's `command` field. If JIRA isn't
        // configured (no `JIRA_URL`), surface a
        // status message instead of staging a
        // malformed command.
        if self.is_jira_query() {
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
            return;
        }
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
