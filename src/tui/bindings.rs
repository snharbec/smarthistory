// Bindings subsystem: Action enum, KeySpec parser, KeyBindings
// table, and the action_for_key lookup.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Close the TUI / cancel an ongoing operation.
    Cancel,
    /// Cycle the search scope (SESS → DIR → GLOBAL → STATS → SESS).
    CycleMode,
    /// Toggle the duplicate filter.
    ToggleDuplicateFilter,
    /// Cycle to the next theme.
    CycleThemeNext,
    /// Cycle to the previous theme.
    CycleThemePrev,
    /// Start editing the comment of the selected entry.
    EditComment,
    /// Open the captured-output view.
    ShowOutput,
    /// Copy the current selection to the system clipboard.
    ///
    /// "Selection" picks the most useful thing to copy at the
    /// moment: if the captured-output view is open, the output
    /// text is copied; otherwise the selected history row's
    /// command is copied. When nothing is selected the action
    /// is a no-op (with a status message so the user knows).
    ///
    /// The default key (`Ctrl-Y`) is the canonical readline/vim
    /// "yank" shortcut, so the muscle memory transfers.
    YankSelection,
    /// Find a filename referenced in the selected history row
    /// and stage `$EDITOR <filename>` as the next selection. The
    /// TUI exits so the parent shell runs the command, which
    /// launches the editor on the file.
    ///
    /// The pick algorithm tokenizes the row's command,
    /// discards tokens containing shell metacharacters
    /// (globs, redirects, subshells, …), and scores the rest by
    /// how "path-like" each looks (starts with `/`, `~`, `./`,
    /// `../`; contains a `/`; has a file extension). The
    /// highest-scoring token wins. A no-op with a status
    /// message is surfaced when no row is selected or no
    /// filename-shaped token is found.
    ///
    /// The default key (`Ctrl-O`) is mnemonic for "Open" in
    /// editor. `$EDITOR` falls back to `vi` (POSIX-mandated)
    /// when unset.
    EditFileReference,
    /// Open the help overlay.
    OpenHelp,
    /// Delete the selected entry (with confirmation).
    DeleteSelected,
    /// Delete all matching entries (with confirmation).
    DeleteMatching,
    /// Clear the search query.
    ClearQuery,
    /// Cycle the exit-code filter.
    CycleExitFilter,
    /// Run the selected command (Enter).
    Run,
    /// Prefill the line for editing, cursor at the start (Left).
    EditStart,
    /// Prefill the line for editing, cursor at the end (Right).
    EditEnd,
    /// Move the cursor up in the list (Up).
    Up,
    /// Move the cursor down in the list (Down).
    Down,
    /// Jump 10 rows up (PageUp).
    PageUp,
    /// Jump 10 rows down (PageDown).
    PageDown,
    /// Jump to the oldest entry (Home).
    Home,
    /// Jump to the newest entry (End).
    End,
    /// Delete one character from the query (Backspace).
    Backspace,
    /// Open the command palette: a menu where the user can pick
    /// any action by name, with its current binding displayed.
    /// Useful when the user has forgotten (or rebound) a shortcut.
    CommandAction,
    /// Open the theme picker: a list of every available theme
    /// (manual + built-in) where navigating the list applies the
    /// theme live, Enter commits, Esc reverts to the original.
    ThemePicker,
    /// Toggle between plain, regex, and fuzzy search modes.
    ToggleSearchMode,
}

impl Action {
    /// Stable kebab-case identifier used in the config file and the
    /// session file (so users see "key.cycle-theme-next=" in their
    /// editor instead of an opaque enum variant name).
    pub fn config_key(self) -> &'static str {
        match self {
            Action::Cancel => "cancel",
            Action::CycleMode => "cycle-mode",
            Action::ToggleDuplicateFilter => "toggle-duplicate-filter",
            Action::CycleThemeNext => "cycle-theme-next",
            Action::CycleThemePrev => "cycle-theme-prev",
            Action::EditComment => "edit-comment",
            Action::ShowOutput => "show-output",
            Action::YankSelection => "yank-selection",
            Action::EditFileReference => "edit-file-reference",
            Action::OpenHelp => "open-help",
            Action::DeleteSelected => "delete-selected",
            Action::DeleteMatching => "delete-matching",
            Action::ClearQuery => "clear-query",
            Action::CycleExitFilter => "cycle-exit-filter",
            Action::Run => "run",
            Action::EditStart => "edit-start",
            Action::EditEnd => "edit-end",
            Action::Up => "up",
            Action::Down => "down",
            Action::PageUp => "page-up",
            Action::PageDown => "page-down",
            Action::Home => "home",
            Action::End => "end",
            Action::Backspace => "backspace",
            Action::CommandAction => "command-action",
            Action::ThemePicker => "theme-picker",
            Action::ToggleSearchMode => "toggle-search-mode",
        }
    }

    /// Human-readable name for help / status displays.
    pub fn display_name(self) -> &'static str {
        match self {
            Action::Cancel => "Cancel",
            Action::CycleMode => "Cycle scope",
            Action::ToggleDuplicateFilter => "Toggle dedup",
            Action::CycleThemeNext => "Next theme",
            Action::CycleThemePrev => "Previous theme",
            Action::EditComment => "Edit comment",
            Action::ShowOutput => "Show output",
            Action::YankSelection => "Yank selection",
            Action::EditFileReference => "Edit referenced file",
            Action::OpenHelp => "Open help",
            Action::DeleteSelected => "Delete entry",
            Action::DeleteMatching => "Delete matches",
            Action::ClearQuery => "Clear query",
            Action::CycleExitFilter => "Cycle exit filter",
            Action::Run => "Run",
            Action::EditStart => "Edit (cursor at start)",
            Action::EditEnd => "Edit (cursor at end)",
            Action::Up => "Up",
            Action::Down => "Down",
            Action::PageUp => "Page up",
            Action::PageDown => "Page down",
            Action::Home => "Home",
            Action::End => "End",
            Action::Backspace => "Backspace",
            Action::CommandAction => "Command palette",
            Action::ThemePicker => "Theme picker",
            Action::ToggleSearchMode => "Toggle search mode",
        }
    }

    /// Category used to group actions in the command palette.
    /// Stable across builds so the menu ordering is predictable.
    #[allow(dead_code)]
    pub fn category(self) -> &'static str {
        match self {
            Action::Cancel
            | Action::Run
            | Action::EditStart
            | Action::EditEnd
            | Action::Up
            | Action::Down
            | Action::PageUp
            | Action::PageDown
            | Action::Home
            | Action::End
            | Action::Backspace => "navigation",
            Action::CycleMode
            | Action::ToggleDuplicateFilter
            | Action::CycleExitFilter
            | Action::ClearQuery
            | Action::ToggleSearchMode => "search",
            Action::CycleThemeNext | Action::CycleThemePrev => "theme",
            Action::EditComment
            | Action::ShowOutput
            | Action::OpenHelp
            | Action::CommandAction
            | Action::ThemePicker
            | Action::YankSelection
            | Action::EditFileReference => "tools",
            Action::DeleteSelected | Action::DeleteMatching => "delete",
        }
    }

    /// The default key binding (as a string in the same format the
    /// config file uses, e.g. `"C-h"`, `"Up"`, `"Esc"`).
    pub fn default_key(self) -> &'static str {
        match self {
            Action::Cancel => "Esc",
            Action::CycleMode => "C-g",
            Action::ToggleDuplicateFilter => "C-s",
            Action::CycleThemeNext => "C-n",
            Action::CycleThemePrev => "C-p",
            Action::EditComment => "C-e",
            Action::ShowOutput => "C-l",
            Action::YankSelection => "C-y",
            Action::EditFileReference => "C-o",
            Action::OpenHelp => "C-h",
            Action::DeleteSelected => "C-d",
            Action::DeleteMatching => "C-x",
            Action::ClearQuery => "C-u",
            Action::CycleExitFilter => "C-j",
            Action::Run => "Enter",
            Action::EditStart => "Left",
            Action::EditEnd => "Right",
            Action::Up => "Up",
            Action::Down => "Down",
            Action::PageUp => "PageUp",
            Action::PageDown => "PageDown",
            Action::Home => "Home",
            Action::End => "End",
            Action::Backspace => "Backspace",
            Action::CommandAction => ":",
            Action::ThemePicker => "T",
            Action::ToggleSearchMode => "F3",
        }
    }
}

/// A parsed key binding. `None` means "any key with these
/// modifiers"; otherwise the binding matches only when the
/// keycode and modifiers both match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

/// Parse a `key.<action>=<spec>` value into a `KeySpec`. Accepts:
///
/// - Plain keys: `a`, `B`, `5`, `/`, `?`, `:`…
/// - Prefixed modifiers: `C-<x>` (Ctrl), `M-<x>` (Alt/Meta),
///   `S-<x>` (Shift). Multiple modifiers can be chained:
///   `C-M-h` = Ctrl+Alt+h.
/// - Named keys: `Esc`, `Enter`, `Tab`, `Backspace`, `Up`,
///   `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`,
///   `Space`, `BackTab`. `C-Esc`, `S-Tab`, etc. are also accepted.
///
/// Returns `Err` for unrecognized input; the caller logs a warning
/// and keeps the previous binding.
pub(crate) fn parse_key_spec(s: &str) -> Result<KeySpec, String> {
    parse_key_spec_opt(s)?.ok_or_else(|| {
        // The spec parsed as a valid unbind sentinel ("none").
        // Surface a friendly message if anyone calls the
        // non-Optional variant with that input by mistake.
        "this function does not accept the `none` sentinel; use parse_key_spec_opt".to_string()
    })
}

/// Like `parse_key_spec`, but additionally recognises an "unbind"
/// sentinel (`none`, `off`, `disable`, `-`, or empty). Returns
/// `Ok(Some(spec))` for a normal binding, `Ok(None)` for an
/// explicit unbind, and `Err` for any malformed input.
///
/// The unbind sentinel lets users disable a default binding by
/// writing `key.<action>=none` in the config file. The action
/// will then simply never fire when its key is pressed.
pub(crate) fn parse_key_spec_opt(s: &str) -> Result<Option<KeySpec>, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "none" | "off" | "disable" | "-" | "disabled"
    ) {
        return Ok(None);
    }
    if s.is_empty() {
        return Err("empty key spec".into());
    }
    let mut modifiers = KeyModifiers::empty();
    let mut rest = s;
    // Walk modifier prefixes. Allow C-, M-, S- in any order.
    loop {
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("c-") && rest.len() > 2 {
            modifiers |= KeyModifiers::CONTROL;
            rest = &rest[2..];
        } else if lower.starts_with("m-") && rest.len() > 2 {
            modifiers |= KeyModifiers::ALT;
            rest = &rest[2..];
        } else if lower.starts_with("s-") && rest.len() > 2 {
            modifiers |= KeyModifiers::SHIFT;
            rest = &rest[2..];
        } else {
            break;
        }
    }
    if rest.is_empty() {
        return Err(format!("key spec {:?} has no key after modifiers", s));
    }
    // Try to interpret `rest` as a named key first (case-insensitive).
    let lower = rest.to_ascii_lowercase();
    let code = match lower.as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" | "shift-tab" | "shifttab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" | "page-up" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "page-down" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        "delete" | "del" => KeyCode::Delete,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        _ => {
            // Plain character. For multi-character strings, only
            // accept the single-character form; otherwise emit a
            // clear error so the user notices the typo.
            let mut chars = rest.chars();
            let first = chars.next().unwrap();
            if chars.next().is_some() {
                return Err(format!(
                    "unknown key spec {:?}: expected a single character or a named key (Up, Esc, …)",
                    s
                ));
            }
            KeyCode::Char(first)
        }
    };
    Ok(Some(KeySpec { code, modifiers }))
}

/// Format a `KeySpec` back to its canonical display form so it can
/// be shown in the help overlay, status bar, and `smarthistory
/// config check` reports.
pub fn format_key_spec(spec: KeySpec) -> String {
    let mut out = String::new();
    if spec.modifiers.contains(KeyModifiers::CONTROL) {
        out.push_str("C-");
    }
    if spec.modifiers.contains(KeyModifiers::ALT) {
        out.push_str("M-");
    }
    if spec.modifiers.contains(KeyModifiers::SHIFT) {
        out.push_str("S-");
    }
    out.push_str(&format_key_code(spec.code));
    out
}

fn format_key_code(code: KeyCode) -> String {
    match code {
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "BackTab".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Insert => "Ins".to_string(),
        KeyCode::Delete => "Del".to_string(),
        KeyCode::F(n) => format!("F{}", n),
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        _ => format!("{:?}", code),
    }
}

/// User-customizable key bindings. Populated once at TUI startup
/// from the config file; defaults match the original hard-coded
/// `Ctrl-*` bindings so the TUI still behaves the same when no
/// `key.*` entries are configured.
///
/// Each action is associated with a `Vec<KeySpec>` (possibly
/// empty) so a single action can fire on several keys at once.
/// The empty `Vec` means the action is unbound — the user wrote
/// `key.<action>=none` to disable it, or the unbind sentinel
/// `none` appeared in a multi-key value like
/// `key.cancel=none,Esc`. The action still appears in `iter()`
/// (so the help overlay can render it as "unbound") but
/// `action_for_key` will never produce it.
#[derive(Debug, Clone)]
pub struct KeyBindings {
    by_action: HashMap<Action, Vec<KeySpec>>,
}

impl KeyBindings {
    /// Build a fresh binding table with every action wired to its
    /// default key (one spec per action).
    pub fn defaults() -> Self {
        let mut by_action = HashMap::new();
        for a in ALL_ACTIONS {
            let spec =
                parse_key_spec(a.default_key()).expect("default key bindings must always parse");
            by_action.insert(*a, vec![spec]);
        }
        KeyBindings { by_action }
    }

    /// Replace the binding list for `action` with the given specs.
    /// An empty vec unbinds the action; a non-empty vec replaces
    /// any previous bindings for that action. Used by the config
    /// parser when the user writes `key.<action>=<spec>,…`.
    pub fn set(&mut self, action: Action, specs: Vec<KeySpec>) {
        self.by_action.insert(action, specs);
    }

    /// Unbind `action` so it never fires when its key is pressed.
    /// The action is still in the table (so the help overlay can
    /// report it as "unbound") but `action_for_key` and `specs`
    /// will return nothing for it.
    pub fn unbind(&mut self, action: Action) {
        self.by_action.insert(action, Vec::new());
    }

    /// All key specs currently bound to `action`. Empty slice when
    /// the action is unbound.
    pub fn specs(&self, action: Action) -> &[KeySpec] {
        self.by_action
            .get(&action)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// True when `action` is currently unbound (zero specs).
    pub fn is_unbound(&self, action: Action) -> bool {
        self.specs(action).is_empty()
    }

    /// `(action, specs)` for every action, in the stable
    /// `ALL_ACTIONS` order. Used by the help overlay, the command
    /// palette, and the `smarthistory config check` tool.
    pub fn iter(&self) -> impl Iterator<Item = (Action, &[KeySpec])> + '_ {
        ALL_ACTIONS.iter().map(move |a| (*a, self.specs(*a)))
    }
}

/// Every action the user can remap, in display order. Kept as a
/// const slice so the iteration order in `KeyBindings::iter` is
/// deterministic (helpful for the help overlay and tests).
pub const ALL_ACTIONS: &[Action] = &[
    Action::Cancel,
    Action::CycleMode,
    Action::ToggleDuplicateFilter,
    Action::CycleThemeNext,
    Action::CycleThemePrev,
    Action::EditComment,
    Action::ShowOutput,
    Action::YankSelection,
    Action::EditFileReference,
    Action::OpenHelp,
    Action::DeleteSelected,
    Action::DeleteMatching,
    Action::ClearQuery,
    Action::CycleExitFilter,
    Action::Run,
    Action::EditStart,
    Action::EditEnd,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Home,
    Action::End,
    Action::Backspace,
    Action::CommandAction,
    Action::ThemePicker,
    Action::ToggleSearchMode,
];

/// Build a `KeyBindings` table from a parsed config map of
/// `key.<action>` → `<spec-list>` strings. Each spec-list is a
/// comma-separated list of key specs (e.g. `"C-h,F1"` or
/// `"C-h, F1"`); every spec in the list is bound to the action
/// in the order given. Whitespace around the commas is ignored.
///
/// Unknown actions are reported on stderr and dropped. Unbind
/// sentinels (`none`, `off`, `disable`, `-`, `disabled`,
/// case-insensitive) anywhere in the list mean the whole action
/// is unbound — there's no meaningful interpretation of
/// `key.cancel=none,Esc` since the user clearly wanted to
/// disable the action, so we honor that. Any other parse error
/// drops the whole binding with a warning rather than
/// half-applying a broken config.
pub fn key_bindings_from_config(entries: &HashMap<String, String>) -> KeyBindings {
    let mut bindings = KeyBindings::defaults();
    // Build a quick lookup so we can detect `key.<unknown>` typos
    // (e.g. `key.toggle-duplication-filter` with the extra "ation")
    // and warn the user about them.
    //
    // The `entries` map is keyed by the bare action name (without
    // the `key.` prefix) — see `Config::parse` — so we compare
    // against the action's `config_key()` directly.
    let known_keys: std::collections::HashSet<&'static str> =
        ALL_ACTIONS.iter().map(|a| a.config_key()).collect();
    for (k, v) in entries {
        if !known_keys.contains(k.as_str()) {
            eprintln!(
                "warning: ignoring unknown key action {:?}={:?} (valid actions: {})",
                k,
                v,
                ALL_ACTIONS
                    .iter()
                    .map(|a| a.config_key())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            continue;
        }
    }
    for a in ALL_ACTIONS {
        let Some(value) = entries.get(a.config_key()) else {
            continue;
        };
        // Split on commas, trim each piece, drop empties. The
        // outer trim handles a leading/trailing comma.
        let parts: Vec<&str> = value
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            eprintln!(
                "warning: ignoring key.{}={:?}: no key specs after splitting on ','",
                a.config_key(),
                value,
            );
            continue;
        }
        let mut specs: Vec<KeySpec> = Vec::with_capacity(parts.len());
        let mut unbind_requested = false;
        let mut bad_piece: Option<String> = None;
        for part in &parts {
            match parse_key_spec_opt(part) {
                Ok(Some(spec)) => specs.push(spec),
                Ok(None) => unbind_requested = unbind_requested || specs.is_empty(),
                Err(e) => {
                    bad_piece = Some(format!("{:?}: {}", part, e));
                    break;
                }
            }
        }
        if let Some(msg) = bad_piece {
            eprintln!(
                "warning: ignoring key.{}={:?}: bad spec {}",
                a.config_key(),
                value,
                msg,
            );
            continue;
        }
        if unbind_requested {
            // An unbind sentinel anywhere in the list means the
            // user wants this action disabled. The other keys in
            // the list are silently discarded so that
            // `key.cancel=none,Esc` (a likely accidental mix-up)
            // doesn't bind Esc to cancel after the user thought
            // they'd disabled it.
            bindings.unbind(*a);
            continue;
        }
        bindings.set(*a, specs);
    }
    bindings
}

/// Try to match a `KeyEvent` against the binding table, returning
/// the first action whose spec matches. Iteration order is the
/// `ALL_ACTIONS` order, so earlier entries win on collisions. (We
/// don't currently try to detect collisions; the help overlay lists
/// every binding so the user can spot duplicates themselves.)
///
/// An action with several bound specs is matched if the event
/// matches *any* of them — pressing F1 or C-h both fire
/// `Action::OpenHelp` if the user wrote `key.open-help=C-h,F1`.
pub fn action_for_key(bindings: &KeyBindings, key: &KeyEvent) -> Option<Action> {
    for a in ALL_ACTIONS {
        for spec in bindings.specs(*a) {
            if spec.code == key.code && spec.modifiers == key.modifiers {
                return Some(*a);
            }
        }
    }
    None
}

/// Join a slice of `KeySpec` into the canonical display form
/// (`"C-h, F1, M-x"`) for the help overlay and the command
/// palette. Empty slice returns the empty string; use
/// `KeyBindings::is_unbound` to render the "unbound" label
/// separately.
pub fn format_key_specs(specs: &[KeySpec]) -> String {
    let mut out = String::new();
    for (i, spec) in specs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format_key_spec(*spec));
    }
    out
}
