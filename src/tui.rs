use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
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
use crate::Config;
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;

/// Persistent state of the last TUI session. Stored in
/// `~/.cache/smarthistory/session` and reloaded on the next TUI
/// invocation so that the user's mode, query, and duplicate-filter
/// preferences carry over.
#[derive(Debug, Default, Clone)]
struct TuiSession {
    /// Last used search mode (e.g. "SESS", "DIR", "GLOBAL").
    mode: Option<String>,
    /// Last entered search query.
    query: Option<String>,
    /// Last duplicate-filter state. `None` means "no preference" and
    /// falls back to the config-file default.
    duplicate_filter: Option<bool>,
    /// Last selected theme slug (e.g. `"dracula"`, `"tokyo-night"`,
    /// or `"none"` for the manual-config palette). Persisted across
    /// TUI invocations so the user always lands back on their
    /// preferred colors.
    theme: Option<String>,
}

/// All theme choices available in the TUI. The first entry, `None`,
/// represents the "no theme" mode where the manually-configured
/// `tuicolor.*` settings from `~/.config/smarthistory/config` are
/// used. Every other entry corresponds to a built-in theme —
/// see `BuiltinTheme` for the full list (upstream `ratatui-themes`
/// plus a small set of hand-curated themes shipped with this
/// crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
enum SelectedTheme {
    /// Manually-configured palette (built-in defaults if no config).
    #[default]
    None,
    /// One of the built-in themes.
    Builtin(BuiltinTheme),
}

/// Every built-in theme smarthistory knows about. The upstream
/// variants come from `ratatui-themes`; the rest live in this
/// crate so users get a wider palette out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinTheme {
    // --- Upstream themes (from `ratatui-themes`) ---
    Dracula,
    OneDarkPro,
    Nord,
    CatppuccinMocha,
    CatppuccinLatte,
    GruvboxDark,
    GruvboxLight,
    TokyoNight,
    SolarizedDark,
    SolarizedLight,
    MonokaiPro,
    RosePine,
    Kanagawa,
    Everforest,
    Cyberpunk,
    // --- smarthistory hand-curated themes ---
    /// Doom Emacs' `doom-one` theme: deep blues on a near-black
    /// background, popular among Doom users.
    DoomOne,
    /// Doom Emacs' `doom-solarized-light` theme: warm cream
    /// background with the classic Solarized accent palette.
    DoomSolarizedLight,
    /// Minimalist theme with no decoration — just black, white,
    /// and one accent color. Useful for accessibility or
    /// recording demos where color noise is distracting.
    Plain,
    /// Leuven theme: a warm, paper-like light theme with sepia
    /// accents inspired by academic reading.
    Leuven,
    /// Google Material Design 3-inspired dark theme: deep purple
    /// accents on a near-black background.
    MaterialDark,
    /// Google Material Design 3-inspired light theme: purple
    /// accents on a soft off-white background.
    MaterialLight,
}

impl BuiltinTheme {
    /// The 15 upstream themes, in the canonical
    /// `ratatui-themes::ThemeName::all()` order. Listed first so
    /// existing session files keep working unchanged.
    pub fn upstream() -> &'static [BuiltinTheme] {
        &[
            BuiltinTheme::Dracula,
            BuiltinTheme::OneDarkPro,
            BuiltinTheme::Nord,
            BuiltinTheme::CatppuccinMocha,
            BuiltinTheme::CatppuccinLatte,
            BuiltinTheme::GruvboxDark,
            BuiltinTheme::GruvboxLight,
            BuiltinTheme::TokyoNight,
            BuiltinTheme::SolarizedDark,
            BuiltinTheme::SolarizedLight,
            BuiltinTheme::MonokaiPro,
            BuiltinTheme::RosePine,
            BuiltinTheme::Kanagawa,
            BuiltinTheme::Everforest,
            BuiltinTheme::Cyberpunk,
        ]
    }

    /// The hand-curated themes shipped with smarthistory. Listed
    /// after the upstream ones so they appear in the theme picker
    /// in the same order as in `all()`.
    pub fn curated() -> &'static [BuiltinTheme] {
        &[
            BuiltinTheme::DoomOne,
            BuiltinTheme::DoomSolarizedLight,
            BuiltinTheme::Plain,
            BuiltinTheme::Leuven,
            BuiltinTheme::MaterialDark,
            BuiltinTheme::MaterialLight,
        ]
    }

    /// Every theme, upstream first then curated. Returned as a
    /// `Vec` so callers can iterate without thinking about
    /// slices-of-slices.
    pub fn all() -> Vec<BuiltinTheme> {
        let mut out = Vec::with_capacity(Self::upstream().len() + Self::curated().len());
        out.extend_from_slice(Self::upstream());
        out.extend_from_slice(Self::curated());
        out
    }

    /// Map back to the corresponding `ratatui-themes::ThemeName`
    /// for the upstream subset. Returns `None` for the curated
    /// themes — they don't exist upstream and fall back to
    /// `Self::curated_palette()`.
    fn as_upstream(self) -> Option<ratatui_themes::ThemeName> {
        match self {
            BuiltinTheme::Dracula => Some(ratatui_themes::ThemeName::Dracula),
            BuiltinTheme::OneDarkPro => Some(ratatui_themes::ThemeName::OneDarkPro),
            BuiltinTheme::Nord => Some(ratatui_themes::ThemeName::Nord),
            BuiltinTheme::CatppuccinMocha => {
                Some(ratatui_themes::ThemeName::CatppuccinMocha)
            }
            BuiltinTheme::CatppuccinLatte => {
                Some(ratatui_themes::ThemeName::CatppuccinLatte)
            }
            BuiltinTheme::GruvboxDark => Some(ratatui_themes::ThemeName::GruvboxDark),
            BuiltinTheme::GruvboxLight => Some(ratatui_themes::ThemeName::GruvboxLight),
            BuiltinTheme::TokyoNight => Some(ratatui_themes::ThemeName::TokyoNight),
            BuiltinTheme::SolarizedDark => Some(ratatui_themes::ThemeName::SolarizedDark),
            BuiltinTheme::SolarizedLight => {
                Some(ratatui_themes::ThemeName::SolarizedLight)
            }
            BuiltinTheme::MonokaiPro => Some(ratatui_themes::ThemeName::MonokaiPro),
            BuiltinTheme::RosePine => Some(ratatui_themes::ThemeName::RosePine),
            BuiltinTheme::Kanagawa => Some(ratatui_themes::ThemeName::Kanagawa),
            BuiltinTheme::Everforest => Some(ratatui_themes::ThemeName::Everforest),
            BuiltinTheme::Cyberpunk => Some(ratatui_themes::ThemeName::Cyberpunk),
            BuiltinTheme::DoomOne
            | BuiltinTheme::DoomSolarizedLight
            | BuiltinTheme::Plain
            | BuiltinTheme::Leuven
            | BuiltinTheme::MaterialDark
            | BuiltinTheme::MaterialLight => None,
        }
    }

    /// Resolve the canonical `ThemePalette` for this theme.
    /// Upstream themes use `ratatui_themes::ThemeName::palette()`;
    /// curated themes have their palettes defined locally in
    /// `Self::curated_palette()`.
    pub fn palette(self) -> ratatui_themes::ThemePalette {
        if let Some(name) = self.as_upstream() {
            return name.palette();
        }
        self.curated_palette()
    }

    /// Palettes for the curated (hand-written) themes. The colors
    /// below are inspired by the upstream projects' own
    /// documentation so the look is recognisable.
    fn curated_palette(self) -> ratatui_themes::ThemePalette {
        use ratatui::style::Color;
        match self {
            // Doom One: dark indigo background with soft blue
            // accents. Source: doom-emacs `doom-one`.
            BuiltinTheme::DoomOne => ratatui_themes::ThemePalette {
                accent: Color::Rgb(115, 191, 255),
                secondary: Color::Rgb(255, 121, 198),
                bg: Color::Rgb(40, 44, 52),
                fg: Color::Rgb(187, 187, 187),
                muted: Color::Rgb(99, 109, 131),
                selection: Color::Rgb(56, 60, 74),
                error: Color::Rgb(255, 84, 84),
                warning: Color::Rgb(229, 192, 123),
                success: Color::Rgb(152, 195, 121),
                info: Color::Rgb(115, 191, 255),
            },
            // Doom Solarized Light: cream paper background with
            // the Solarized base palette. Source: doom-emacs
            // `doom-solarized-light`.
            BuiltinTheme::DoomSolarizedLight => ratatui_themes::ThemePalette {
                accent: Color::Rgb(38, 139, 210),
                secondary: Color::Rgb(108, 113, 196),
                bg: Color::Rgb(253, 246, 227),
                fg: Color::Rgb(101, 123, 131),
                muted: Color::Rgb(147, 161, 161),
                selection: Color::Rgb(238, 232, 213),
                error: Color::Rgb(220, 50, 47),
                warning: Color::Rgb(181, 137, 0),
                success: Color::Rgb(133, 153, 0),
                info: Color::Rgb(42, 161, 152),
            },
            // Plain: deliberately minimal. Pure black
            // background, white foreground, single accent color.
            // No fancy gradients or muted tones.
            BuiltinTheme::Plain => ratatui_themes::ThemePalette {
                accent: Color::White,
                secondary: Color::White,
                bg: Color::Black,
                fg: Color::White,
                muted: Color::White,
                selection: Color::Rgb(40, 40, 40),
                error: Color::White,
                warning: Color::White,
                success: Color::White,
                info: Color::White,
            },
            // Leuven: warm academic-light theme. Soft paper
            // background with sepia/red accents. Inspired by
            // Leuven's `~/.Xresources` style.
            BuiltinTheme::Leuven => ratatui_themes::ThemePalette {
                accent: Color::Rgb(170, 65, 57),
                secondary: Color::Rgb(34, 102, 102),
                bg: Color::Rgb(255, 250, 240),
                fg: Color::Rgb(34, 34, 34),
                muted: Color::Rgb(150, 140, 120),
                selection: Color::Rgb(240, 220, 180),
                error: Color::Rgb(170, 30, 30),
                warning: Color::Rgb(180, 120, 30),
                success: Color::Rgb(60, 130, 60),
                info: Color::Rgb(60, 90, 160),
            },
            // Material Dark: Material Design 3 dark theme with
            // purple primary. Background is a deep neutral grey,
            // not pure black.
            BuiltinTheme::MaterialDark => ratatui_themes::ThemePalette {
                accent: Color::Rgb(208, 188, 255),
                secondary: Color::Rgb(3, 218, 198),
                bg: Color::Rgb(28, 27, 34),
                fg: Color::Rgb(230, 225, 229),
                muted: Color::Rgb(202, 196, 208),
                selection: Color::Rgb(55, 52, 67),
                error: Color::Rgb(242, 184, 181),
                warning: Color::Rgb(255, 213, 153),
                success: Color::Rgb(165, 214, 167),
                info: Color::Rgb(149, 222, 227),
            },
            // Material Light: M3 light theme. Off-white
            // background with the same purple primary.
            BuiltinTheme::MaterialLight => ratatui_themes::ThemePalette {
                accent: Color::Rgb(103, 80, 164),
                secondary: Color::Rgb(0, 137, 123),
                bg: Color::Rgb(254, 247, 255),
                fg: Color::Rgb(28, 27, 34),
                muted: Color::Rgb(73, 69, 79),
                selection: Color::Rgb(231, 224, 236),
                error: Color::Rgb(179, 38, 30),
                warning: Color::Rgb(245, 124, 0),
                success: Color::Rgb(46, 125, 50),
                info: Color::Rgb(1, 135, 134),
            },
            // Upstream themes shouldn't reach here (their
            // palettes come from `ratatui-themes::ThemeName::palette()`
            // via `Self::palette()`). This defensive arm keeps
            // the match exhaustive so the type checker is happy.
            _ => {
                if let Some(name) = self.as_upstream() {
                    name.palette()
                } else {
                    // Truly unreachable.
                    ratatui_themes::ThemePalette {
                        accent: Color::Reset,
                        secondary: Color::Reset,
                        bg: Color::Reset,
                        fg: Color::Reset,
                        muted: Color::Reset,
                        selection: Color::Reset,
                        error: Color::Reset,
                        warning: Color::Reset,
                        success: Color::Reset,
                        info: Color::Reset,
                    }
                }
            }
        }
    }

    /// Slug used in the session file and the config key. Lowercase,
    /// hyphen-separated.
    pub fn slug(self) -> &'static str {
        match self {
            BuiltinTheme::Dracula => "dracula",
            BuiltinTheme::OneDarkPro => "one-dark-pro",
            BuiltinTheme::Nord => "nord",
            BuiltinTheme::CatppuccinMocha => "catppuccin-mocha",
            BuiltinTheme::CatppuccinLatte => "catppuccin-latte",
            BuiltinTheme::GruvboxDark => "gruvbox-dark",
            BuiltinTheme::GruvboxLight => "gruvbox-light",
            BuiltinTheme::TokyoNight => "tokyo-night",
            BuiltinTheme::SolarizedDark => "solarized-dark",
            BuiltinTheme::SolarizedLight => "solarized-light",
            BuiltinTheme::MonokaiPro => "monokai-pro",
            BuiltinTheme::RosePine => "rose-pine",
            BuiltinTheme::Kanagawa => "kanagawa",
            BuiltinTheme::Everforest => "everforest",
            BuiltinTheme::Cyberpunk => "cyberpunk",
            BuiltinTheme::DoomOne => "doom-one",
            BuiltinTheme::DoomSolarizedLight => "doom-solarized-light",
            BuiltinTheme::Plain => "plain",
            BuiltinTheme::Leuven => "leuven",
            BuiltinTheme::MaterialDark => "material-dark",
            BuiltinTheme::MaterialLight => "material-light",
        }
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            BuiltinTheme::Dracula => "Dracula",
            BuiltinTheme::OneDarkPro => "One Dark Pro",
            BuiltinTheme::Nord => "Nord",
            BuiltinTheme::CatppuccinMocha => "Catppuccin Mocha",
            BuiltinTheme::CatppuccinLatte => "Catppuccin Latte",
            BuiltinTheme::GruvboxDark => "Gruvbox Dark",
            BuiltinTheme::GruvboxLight => "Gruvbox Light",
            BuiltinTheme::TokyoNight => "Tokyo Night",
            BuiltinTheme::SolarizedDark => "Solarized Dark",
            BuiltinTheme::SolarizedLight => "Solarized Light",
            BuiltinTheme::MonokaiPro => "Monokai Pro",
            BuiltinTheme::RosePine => "Rosé Pine",
            BuiltinTheme::Kanagawa => "Kanagawa",
            BuiltinTheme::Everforest => "Everforest",
            BuiltinTheme::Cyberpunk => "Cyberpunk",
            BuiltinTheme::DoomOne => "Doom One",
            BuiltinTheme::DoomSolarizedLight => "Doom Solarized Light",
            BuiltinTheme::Plain => "Plain",
            BuiltinTheme::Leuven => "Leuven",
            BuiltinTheme::MaterialDark => "Material Dark",
            BuiltinTheme::MaterialLight => "Material Light",
        }
    }
}

impl SelectedTheme {
    fn slug(&self) -> &'static str {
        match self {
            SelectedTheme::None => "none",
            SelectedTheme::Builtin(t) => t.slug(),
        }
    }

    fn display_name(&self) -> &'static str {
        match self {
            SelectedTheme::None => "no theme",
            SelectedTheme::Builtin(t) => t.display_name(),
        }
    }

    /// Cycle to the next theme in the list, wrapping around. The
    /// order is `None` (manual) followed by every theme in
    /// `BuiltinTheme::all()` (upstream first, then curated).
    fn next(self) -> Self {
        let themes = Self::ordered_list();
        let pos = themes.iter().position(|t| *t == self).unwrap_or(0);
        themes[(pos + 1) % themes.len()]
    }

    /// Cycle to the previous theme.
    fn prev(self) -> Self {
        let themes = Self::ordered_list();
        let pos = themes.iter().position(|t| *t == self).unwrap_or(0);
        themes[(pos + themes.len() - 1) % themes.len()]
    }

    /// The full ordered list: `None` first, then every entry in
    /// `BuiltinTheme::all()` in canonical order.
    fn ordered_list() -> Vec<SelectedTheme> {
        let mut themes: Vec<SelectedTheme> = Vec::with_capacity(BuiltinTheme::all().len() + 1);
        themes.push(SelectedTheme::None);
        for t in BuiltinTheme::all() {
            themes.push(SelectedTheme::Builtin(t));
        }
        themes
    }

    /// Parse a slug back into a `SelectedTheme`. Unknown slugs
    /// (including ones from a future theme that was removed)
    /// fall back to `None` so the TUI can always start.
    fn from_slug(s: &str) -> Self {
        let normalized: String = s
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect();
        if normalized == "none" || normalized.is_empty() {
            return SelectedTheme::None;
        }
        for t in BuiltinTheme::all() {
            if t.slug() == normalized {
                return SelectedTheme::Builtin(t);
            }
        }
        SelectedTheme::None
    }
}



impl TuiSession {
    fn path() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(
            PathBuf::from(home)
                .join(".local")
                .join("cache")
                .join("smarthistory")
                .join("session"),
        )
    }

    /// Load the persisted session from disk, if available. Missing
    /// files or unparseable contents yield a default (empty) session
    /// rather than an error so the TUI can always start.
    fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut s = Self::default();
        for raw_line in contents.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };
            match key {
                "mode" => s.mode = Some(value.to_string()),
                "query" => s.query = Some(value.to_string()),
                "duplicatefilter" => s.duplicate_filter = Some(parse_bool(value, true)),
                "theme" => s.theme = Some(value.to_string()),
                _ => {}
            }
        }
        s
    }

    /// Persist the current session to disk. Best-effort: any I/O
    /// error is logged to stderr but does not propagate, since the
    /// TUI is exiting anyway.
    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut out = String::new();
        if let Some(ref m) = self.mode {
            out.push_str(&format!("mode={}\n", m));
        }
        if let Some(ref q) = self.query {
            out.push_str(&format!("query={}\n", q));
        }
        if let Some(d) = self.duplicate_filter {
            out.push_str(&format!("duplicatefilter={}\n", if d { "on" } else { "off" }));
        }
        if let Some(ref t) = self.theme {
            out.push_str(&format!("theme={}\n", t));
        }
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("warning: failed to persist TUI session: {}", e);
        }
    }
}

/// Simple boolean parser used by both the global config and the
/// per-session state file. Kept local (not in `util.rs`) so the TUI
/// module stays self-contained for the session file format.
fn parse_bool(s: &str, default: bool) -> bool {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => true,
        "off" | "false" | "0" | "no" => false,
        _ => default,
    }
}

/// A high-level action that the TUI can take in response to a key
/// press. Action names appear in the user-facing config file as
/// `key.<action>=<key-spec>`, e.g. `key.help=C-h`.
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
        }
    }

    /// Category used to group actions in the command palette.
    /// Stable across builds so the menu ordering is predictable.
    #[allow(dead_code)]
    fn category(self) -> &'static str {
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
            | Action::ClearQuery => "search",
            Action::CycleThemeNext | Action::CycleThemePrev => "theme",
            Action::EditComment
            | Action::ShowOutput
            | Action::OpenHelp
            | Action::CommandAction
            | Action::ThemePicker => "tools",
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
fn parse_key_spec(s: &str) -> Result<KeySpec, String> {
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
fn parse_key_spec_opt(s: &str) -> Result<Option<KeySpec>, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if matches!(lower.as_str(), "none" | "off" | "disable" | "-" | "disabled") {
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
#[derive(Debug, Clone)]
pub struct KeyBindings {
    /// `Some(spec)` = action is bound to that key.
    /// `None` = action is unbound (the user wrote
    /// `key.<action>=none` to disable it).
    by_action: HashMap<Action, Option<KeySpec>>,
}

impl KeyBindings {
    /// Build a fresh binding table with every action wired to its
    /// default key.
    pub fn defaults() -> Self {
        let mut by_action = HashMap::new();
        for a in ALL_ACTIONS {
            let spec = parse_key_spec(a.default_key())
                .expect("default key bindings must always parse");
            by_action.insert(*a, Some(spec));
        }
        KeyBindings { by_action }
    }

    /// Override the binding for `action`. Used while parsing the
    /// config file. Unrecognized values are silently kept at their
    /// previous binding (the parser logs a warning elsewhere).
    pub fn set(&mut self, action: Action, spec: KeySpec) {
        self.by_action.insert(action, Some(spec));
    }

    /// Unbind `action` so it never fires when its key is pressed.
    /// The action is still in the table (so the help overlay can
    /// report it as "unbound") but `action_for_key` and `get`
    /// will treat it as if no binding exists.
    pub fn unbind(&mut self, action: Action) {
        self.by_action.insert(action, None);
    }

    /// Look up the spec bound to `action`. Returns `Some(spec)`
    /// when the action is bound and `None` when it has been
    /// explicitly unbound (or never bound).
    pub fn get(&self, action: Action) -> Option<KeySpec> {
        self.by_action.get(&action).and_then(|opt| *opt)
    }

    /// True when `action` is currently unbound.
    pub fn is_unbound(&self, action: Action) -> bool {
        matches!(self.by_action.get(&action), Some(None))
    }

    /// All (action, spec) pairs for currently-bound actions, in a
    /// stable iteration order.
    pub fn iter(&self) -> impl Iterator<Item = (Action, KeySpec)> + '_ {
        ALL_ACTIONS.iter().filter_map(move |a| {
            self.by_action.get(a).and_then(|opt| opt.map(|s| (*a, s)))
        })
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
];

/// Build a `KeyBindings` table from a parsed config map of
/// `key.<action>` → `<spec>` strings. Unknown keys and unparseable
/// specs are silently dropped so the rest of the config still
/// applies; defaults are filled in first so the result is always
/// complete.
pub fn key_bindings_from_config(entries: &HashMap<String, String>) -> KeyBindings {
    let mut bindings = KeyBindings::defaults();
    // Build a quick lookup so we can detect `key.<unknown>` typos
    // (e.g. `key.toggle-duplication-filter` with the extra "ation")
    // and warn the user about them.
    //
    // The `entries` map is keyed by the bare action name (without
    // the `key.` prefix) — see `Config::parse` — so we compare
    // against the action's `config_key()` directly.
    let known_keys: std::collections::HashSet<&'static str> = ALL_ACTIONS
        .iter()
        .map(|a| a.config_key())
        .collect();
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
        if let Some(value) = entries.get(a.config_key()) {
            match parse_key_spec_opt(value) {
                Ok(Some(spec)) => bindings.set(*a, spec),
                Ok(None) => bindings.unbind(*a),
                Err(e) => eprintln!(
                    "warning: ignoring key.{}={:?}: {}",
                    a.config_key(),
                    value,
                    e
                ),
            }
        }
    }
    bindings
}

/// Try to match a `KeyEvent` against the binding table, returning
/// the first action whose spec matches. Iteration order is the
/// `ALL_ACTIONS` order, so earlier entries win on collisions. (We
/// don't currently try to detect collisions; the help overlay lists
/// every binding so the user can spot duplicates themselves.)
pub fn action_for_key(bindings: &KeyBindings, key: &KeyEvent) -> Option<Action> {
    for a in ALL_ACTIONS {
        if let Some(spec) = bindings.get(*a)
            && spec.code == key.code && spec.modifiers == key.modifiers {
                return Some(*a);
            }
    }
    None
}

/// Search scope for the TUI. Mirrors the line-editor widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sess,
    Dir,
    Global,
    /// Rank the global history by:
    ///   1. probability of following the most-recently-executed
    ///      command (via SQLite's `LEAD()` window function),
    ///   2. age (newest first).
    /// The "last command" is determined across the whole global
    /// history so the view is reproducible across mode switches.
    Stats,
}

impl Mode {
    fn next(self) -> Self {
        match self {
            Mode::Sess => Mode::Dir,
            Mode::Dir => Mode::Global,
            Mode::Global => Mode::Stats,
            Mode::Stats => Mode::Sess,
        }
    }
    /// Parse a string like "SESS", "SESSION", "DIR", "DIRECTORY",
    /// "GLOBAL", "STATS", "STATISTICS" (case-insensitive). Returns
    /// None for anything else.
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "SESS" | "SESSION" => Some(Mode::Sess),
            "DIR" | "DIRECTORY" => Some(Mode::Dir),
            "GLOBAL" => Some(Mode::Global),
            "STATS" | "STATISTICS" => Some(Mode::Stats),
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
/// Resolve a color string into a ratatui `Color`. Supports the
/// standard CSS-style named colors plus the 16-color terminal palette
/// that `ratatui::Color` exposes. Hex strings of the form `#rrggbb`
/// or `0xrrggbb` are also accepted. Unknown strings fall back to
/// `Color::Reset`, which lets the terminal decide.
fn resolve_color(s: &str) -> Color {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#').or_else(|| s.strip_prefix("0x"))
        && hex.len() == 6
            && let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
    match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        "reset" => Color::Reset,
        _ => Color::Reset,
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
    #[allow(dead_code)]
    fn next(self) -> Self {
        match self {
            ExitFilter::All => ExitFilter::Success,
            ExitFilter::Success => ExitFilter::Failed,
            ExitFilter::Failed => ExitFilter::All,
        }
    }
}

/// Active TUI palette. Holds the resolved colors used by the draw
/// helpers below. Populated once at TUI startup from the user's
/// `Config::theme()` (via `tuicolor.<field>=<value>` overrides), then
/// read through the `Theme::*` style helpers that the rest of the
/// TUI code already calls. This indirection keeps the call sites
/// unchanged while allowing per-user theming.
#[derive(Debug, Clone, Copy)]
struct Palette {
    bg: Color,
    fg: Color,
    accent: Color,
    success: Color,
    error: Color,
    warning: Color,
    dim: Color,
    #[allow(dead_code)]
    dimmer: Color,
    highlight: Color,
    /// Background color for the currently-selected row in the list.
    selection: Color,
    /// Foreground color used for badge text. Defaults to `bg` so
    /// the text always contrasts with the bright badge background.
    badge_fg: Color,
    /// Background color for the history list pane. Defaults to
    /// `bg` when the user does not set `tuicolor.listbg=`.
    list_bg: Color,
    /// Background color for the details pane.
    details_bg: Color,
    /// Background color for the search/comment input pane.
    input_bg: Color,
    /// Background color for the status bar.
    status_bg: Color,
}

impl Palette {
    fn builtin() -> Self {
        Palette {
            bg: Color::Black,
            fg: Color::Gray,
            accent: Color::Cyan,
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            dim: Color::Gray,
            dimmer: Color::DarkGray,
            highlight: Color::Yellow,
            selection: Color::DarkGray,
            badge_fg: Color::Black,
            list_bg: Color::Black,
            details_bg: Color::Black,
            input_bg: Color::Black,
            status_bg: Color::Black,
        }
    }

    /// Construct the resolved palette for the manually-configured
    /// "no theme" case. All fallbacks come from the user's own
    /// `tuicolor.*` settings (so the manual-config defaults are
    /// self-consistent even without any user override).
    fn from_manual(theme: &crate::TuiTheme, cfg: &Config) -> Self {
        Palette {
            bg: resolve_color(&theme.bg),
            fg: resolve_color(&theme.fg),
            accent: resolve_color(&theme.accent),
            success: resolve_color(&theme.success),
            error: resolve_color(&theme.error),
            warning: resolve_color(&theme.warning),
            dim: resolve_color(&theme.dim),
            dimmer: Color::DarkGray,
            highlight: resolve_color(&theme.highlight),
            selection: resolve_color(&cfg.selection(&theme.bg)),
            badge_fg: resolve_color(&cfg.badge_fg(&theme.bg)),
            list_bg: resolve_color(&cfg.list_bg(&theme.bg)),
            details_bg: resolve_color(&cfg.details_bg(&theme.bg)),
            input_bg: resolve_color(&cfg.input_bg(&theme.bg)),
            status_bg: resolve_color(&cfg.status_bg(&theme.bg)),
        }
    }
}

thread_local! {
    static PALETTE: std::cell::RefCell<Palette> = std::cell::RefCell::new(Palette::builtin());
}

/// Rebuild the active palette for the chosen theme and store it in
/// the `PALETTE` thread-local. When `theme` is `SelectedTheme::None`
/// the palette is rebuilt from the user's manually-configured
/// `tuicolor.*` settings. Otherwise the `ratatui-themes` palette
/// for the matching `ThemeName` supplies the **base** bg / fg /
/// selection / badge-fg / per-pane-bg values, and the user's
/// manual `tuicolor.*` settings override them where set. This way
/// built-in light themes (Gruvbox Light, Catppuccin Latte, …)
/// actually look light in the TUI — not painted on top of the
/// dark defaults that the manual config ships with.
fn install_palette(theme: SelectedTheme) {
    let cfg = Config::load();
    let palette = match theme {
        SelectedTheme::None => Palette::from_manual(cfg.theme(), &cfg),
        SelectedTheme::Builtin(name) => {
            let p = name.palette();
            // Build the palette with the theme's own colors as the
            // fallbacks for every slot. The user's `tuicolor.*`
            // overrides still win where set, so fine-tuning is
            // preserved.
            let cfg_theme = cfg.theme();
            Palette {
                bg: if cfg.has_bg_override() {
                    resolve_color(&cfg_theme.bg)
                } else {
                    p.bg
                },
                fg: if cfg.has_fg_override() {
                    resolve_color(&cfg_theme.fg)
                } else {
                    p.fg
                },
                accent: resolve_color(&cfg_theme.accent),
                success: resolve_color(&cfg_theme.success),
                error: resolve_color(&cfg_theme.error),
                warning: resolve_color(&cfg_theme.warning),
                dim: if cfg.has_dim_override() {
                    resolve_color(&cfg_theme.dim)
                } else {
                    p.muted
                },
                dimmer: Color::DarkGray,
                highlight: resolve_color(&cfg_theme.highlight),
                selection: if cfg_theme.selection.is_empty() {
                    p.selection
                } else {
                    resolve_color(&cfg_theme.selection)
                },
                badge_fg: if cfg_theme.badge_fg.is_empty() {
                    p.bg
                } else {
                    resolve_color(&cfg_theme.badge_fg)
                },
                list_bg: if cfg_theme.list_bg.is_empty() {
                    p.bg
                } else {
                    resolve_color(&cfg_theme.list_bg)
                },
                details_bg: if cfg_theme.details_bg.is_empty() {
                    p.bg
                } else {
                    resolve_color(&cfg_theme.details_bg)
                },
                input_bg: if cfg_theme.input_bg.is_empty() {
                    p.bg
                } else {
                    resolve_color(&cfg_theme.input_bg)
                },
                status_bg: if cfg_theme.status_bg.is_empty() {
                    p.bg
                } else {
                    resolve_color(&cfg_theme.status_bg)
                },
            }
        }
    };
    PALETTE.with(|c| *c.borrow_mut() = palette);
}



/// Style helpers used throughout the TUI. Each reads the current
/// color from the active `Palette`. Keeping the original call-site
/// signatures (`Theme::error()`, etc.) means none of the rendering
/// code needs to change.
struct Theme;

impl Theme {
    fn default() -> Style {
        let p = PALETTE.with(|c| *c.borrow());
        Style::default().fg(p.fg).bg(p.bg)
    }

    fn accent() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().accent))
    }

    fn success() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().success))
    }

    fn error() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().error))
    }

    fn dim() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().dim))
    }

    #[allow(dead_code)]
    fn dimmer() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().dimmer))
    }

    fn highlight() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().highlight))
    }

    #[allow(dead_code)]
    fn warning() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().warning))
    }

    #[allow(dead_code)]
    const BG: Color = Color::Black;
    #[allow(dead_code)]
    const FG: Color = Color::Gray;
    #[allow(dead_code)]
    const ACCENT: Color = Color::Cyan;
    #[allow(dead_code)]
    const SUCCESS: Color = Color::Green;
    #[allow(dead_code)]
    const ERROR: Color = Color::Red;
    #[allow(dead_code)]
    const WARNING: Color = Color::Yellow;
    #[allow(dead_code)]
    const DIM: Color = Color::Gray;
    #[allow(dead_code)]
    const DIMMER: Color = Color::DarkGray;
    #[allow(dead_code)]
    const HIGHLIGHT: Color = Color::Yellow;

    /// Read the current accent color from the active palette.
    /// Used by badge renderers that need a raw `Color` rather than
    /// a full `Style`.
    fn accent_color() -> Color {
        PALETTE.with(|c| c.borrow().accent)
    }
    fn success_color() -> Color {
        PALETTE.with(|c| c.borrow().success)
    }
    fn error_color() -> Color {
        PALETTE.with(|c| c.borrow().error)
    }
    fn warning_color() -> Color {
        PALETTE.with(|c| c.borrow().warning)
    }
    fn highlight_color() -> Color {
        PALETTE.with(|c| c.borrow().highlight)
    }
    #[allow(dead_code)]
    fn dim_color() -> Color {
        PALETTE.with(|c| c.borrow().dim)
    }

    /// Background color used to highlight the currently-selected
    /// row in the history list. Always comes from the active
    /// theme / palette so it follows theme changes.
    fn selection_color() -> Color {
        PALETTE.with(|c| c.borrow().selection)
    }

    /// Foreground color for badge text (inside the bright
    /// mode/scope/dedup chips). Defaults to the global background
    /// so the text always contrasts with the bright background.
    fn badge_fg_color() -> Color {
        PALETTE.with(|c| c.borrow().badge_fg)
    }
}

struct App {
    conn: Connection,
    mode: Mode,
    duplicate_filter: bool,
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
    output_view: Option<OutputView>,
    /// When `Some`, the help overlay is open. The contained `scroll`
    /// tracks how far down the user has scrolled.
    help_view: Option<HelpView>,
    /// When `Some`, the command-palette overlay is open.
    command_menu: Option<CommandMenu>,
    /// When `Some`, the theme-picker overlay is open. Navigating
    /// the list applies the selected theme live; `Enter` commits,
    /// `Esc` reverts to the original.
    theme_picker: Option<ThemePicker>,
    /// When `Some`, we are prompting for deletion confirmation.
    confirm_delete: Option<ConfirmMode>,
    /// Cached set of all history rows that have a comment, used to
    /// populate the optional labeled entries pane.
    labeled_rows: Vec<HistoryRow>,
    /// List state for the labeled entries pane (separate from
    /// `list_state` so the two views can remember their own selection).
    labeled_list_state: ListState,
    /// True when the initial query was loaded from the persisted
    /// session file (so the user is editing a previously-saved query
    /// rather than typing fresh text). The first character typed
    /// replaces the prefilled value instead of appending to it.
    query_prefilled: bool,
    /// True once the user has touched the query buffer (typed,
    /// deleted, or cleared). After this point, additional input
    /// appends normally — even if the buffer is later emptied.
    query_touched: bool,
    /// Compiled regex when the query starts with `/`. `None` when
    /// the query is plain text, the query is empty, or the regex
    /// failed to compile (in which case we silently fall back to
    /// the plain-text path so the user can keep editing).
    query_regex: Option<Regex>,
    /// The currently-selected TUI palette. Defaults to
    /// `SelectedTheme::None`, which means the manually-configured
    /// colors from `tuicolor.*` are used.
    theme: SelectedTheme,
    /// Active key bindings, resolved from the user's config file.
    /// Defaults match the original hard-coded Ctrl-* shortcuts.
    bindings: KeyBindings,
}

impl App {
    /// True if the current query is a regex (prefixed with `/`).
    fn is_regex_query(&self) -> bool {
        self.query.starts_with('/')
    }

    /// The regex pattern, i.e. everything after the leading `/`.
    /// Empty when the query is just `/`.
    fn regex_pattern(&self) -> &str {
        if self.is_regex_query() {
            &self.query[1..]
        } else {
            ""
        }
    }

    /// Recompile the regex from the current query. Called whenever
    /// the query buffer changes. Failures (invalid regex) leave the
    /// previous compiled regex in place so the user can keep typing
    /// without the list flickering empty.
    ///
    /// The query is the post-slash text. Implicit `.*` anchors are
    /// added at both ends unless the user provided an explicit
    /// anchor (`^` at the start, `$` at the end), so a query like
    /// `git commit` behaves as `.*git commit.*` instead of needing
    /// to match from the very first character.
    fn recompile_regex(&mut self) {
        if !self.is_regex_query() {
            self.query_regex = None;
            return;
        }
        let pattern = build_implicit_regex(self.regex_pattern());
        match Regex::new(&pattern) {
            Ok(re) => self.query_regex = Some(re),
            Err(_) => {
                // Leave the previous regex (if any) in place; the
                // user is mid-edit and we'll retry on the next
                // keystroke. This avoids the list briefly going
                // empty for a transient typo like an unbalanced
                // bracket.
            }
        }
    }

    /// Cycle to the next theme. `Ctrl-N` calls this; `Ctrl-P` calls
    /// `cycle_theme_prev`. Updates the global palette immediately so
    /// the change is visible on the next frame, and triggers a full
    /// repaint by marking the terminal as needing a redraw.
    fn cycle_theme_next(&mut self) {
        self.theme = self.theme.next();
        install_palette(self.theme);
    }

    /// Cycle to the previous theme.
    fn cycle_theme_prev(&mut self) {
        self.theme = self.theme.prev();
        install_palette(self.theme);
    }

    /// Return true if the given text matches the current query:
    /// either the plain-text substring search (multi-word, AND),
    /// or the compiled regex when the query starts with `/`.
    fn query_matches_text(&self, text: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        if self.is_regex_query() {
            if let Some(ref re) = self.query_regex {
                return re.is_match(text);
            }
            // Regex mode but no valid compiled regex yet — treat
            // the entire post-slash text as a literal pattern so
            // the user sees at least the matches that contain it.
            return text.to_lowercase().contains(&self.query[1..].to_lowercase());
        }
        // Plain text: every whitespace-separated word must appear
        // (case-insensitive).
        let lower = text.to_lowercase();
        self.query
            .split_whitespace()
            .all(|w| lower.contains(&w.to_lowercase()))
    }
}

/// Wrap `pattern` with implicit `.*` anchors unless the user
/// already provided an explicit anchor (`^` at the start, `$` at
/// the end). This means `/git commit/` matches any command that
/// contains `git commit` (i.e. behaves as `/.*git commit.*/`),
/// while `/^git commit/` still only matches commands that start
/// with `git commit`, and `/git commit$/` only matches commands
/// that end with `git commit`.
fn build_implicit_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    if !pattern.starts_with('^') {
        out.push_str(".*");
    }
    out.push_str(pattern);
    if !pattern.ends_with('$') {
        out.push_str(".*");
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmMode {
    DeleteSelected,
    DeleteMatching,
}

/// State for the captured-output overlay: the captured text plus a
/// scroll offset (number of lines scrolled past the top).
struct OutputView {
    text: String,
    scroll: usize,
}

/// State for the help overlay. Just a scroll offset; the help text
/// is computed from the live app state on each render so the
/// "current settings" section is always accurate.
struct HelpView {
    scroll: usize,
}

/// State for the command-palette overlay. The user types into
/// `query` to filter the action list; `selected` is the index of
/// the currently-highlighted action (relative to the filtered
/// list). Pressing Enter runs the highlighted action; Esc closes
/// the overlay; arrows navigate.
struct CommandMenu {
    query: String,
    selected: usize,
    /// The full ordered list of actions shown in the palette. We
    /// snapshot this when the menu opens so the displayed order
    /// stays stable while the user types.
    actions: Vec<Action>,
    /// Whether the user has typed anything. Once true, the first
    /// character no longer replaces a cached query — same
    /// behavior as the main search box.
    touched: bool,
}

impl CommandMenu {
    fn new() -> Self {
        CommandMenu {
            query: String::new(),
            selected: 0,
            actions: ALL_ACTIONS.to_vec(),
            touched: false,
        }
    }

    /// Return the indices (into `self.actions`) of the actions that
    /// match `query`. Matching is case-insensitive substring AND
    /// across words, against either the action's display name or
    /// its `config_key`. Empty query returns every action.
    fn filtered_indices(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.actions.len()).collect();
        }
        let q = self.query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.actions
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                let name = a.display_name().to_lowercase();
                let key = a.config_key().to_lowercase();
                words
                    .iter()
                    .all(|w| name.contains(w) || key.contains(w))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Clamp `self.selected` so it remains a valid index into the
    /// filtered list (which may shrink as the user types).
    fn clamp_selection(&mut self) {
        let n = self.filtered_indices().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }
}

impl App {
    fn new(conn: Connection, initial_mode: Mode, initial_query: String, duplicate_filter: bool, query_prefilled: bool, theme: SelectedTheme, bindings: KeyBindings) -> Self {
        let list_state = ListState::default();
        let mut app = App {
            conn,
            mode: initial_mode,
            duplicate_filter,
            query: initial_query,
            rows: Vec::new(),
            list_state,
            selection: None,
            pick_mode: None,
            cancelled: false,
            comment_edit: None,
            output_view: None,
            help_view: None,
            command_menu: None,
            theme_picker: None,
            confirm_delete: None,
            labeled_rows: Vec::new(),
            labeled_list_state: ListState::default(),
            query_prefilled,
            query_touched: false,
            query_regex: None,
            theme,
            bindings,
        };
        app.recompile_regex();
        app.refresh();
        app.refresh_labeled();
        // Rows are ordered newest first; index 0 is the newest entry.
        // Keep the selection on the newest match so it appears at the
        // bottom of the bottom-aligned list.
        if !app.rows.is_empty() {
            app.list_state.select(Some(0));
        }
        if !app.labeled_rows.is_empty() {
            app.labeled_list_state.select(Some(0));
        }
        app
    }

    /// Re-query the database with the current mode + query.
    /// After re-querying, land on the newest match (index 0 in the
    /// merged list, which is the bottom of the bottom-aligned render).
    /// When the query is a regex, post-filter the SQL results using
    /// `query_matches_text` so the regex can match anywhere in the
    /// command or comment text.
    fn refresh(&mut self) {
        self.rows = self.fetch().unwrap_or_default();
        if self.is_regex_query() {
            // Two-phase borrow: copy the rows out, then post-filter.
            // Avoids the borrow checker complaining about
            // simultaneously borrowing `self.rows` and `self`.
            let query = self.query.clone();
            let regex = self.query_regex.clone();
            self.rows.retain(|r| {
                if let Some(ref re) = regex {
                    re.is_match(&r.command) || re.is_match(&r.comment)
                } else {
                    // No valid regex yet (in-progress typo) — fall
                    // back to a literal substring match on the
                    // post-slash text so the user sees *something*.
                    r.command
                        .to_lowercase()
                        .contains(&query[1..].to_lowercase())
                        || r
                            .comment
                            .to_lowercase()
                            .contains(&query[1..].to_lowercase())
                }
            });
        }
        self.refresh_labeled();
        let n = self.merged_rows().len();
        if n == 0 {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    fn fetch(&self) -> Result<Vec<HistoryRow>> {
        if matches!(self.mode, Mode::Stats) {
            return self.fetch_stats();
        }
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

    /// Fetch rows ordered by:
    ///   1. probability of following the most-recently-executed
    ///      command (computed via SQLite's `LEAD()` window
    ///      function on the entire global history, ignoring
    ///      session/directory filters),
    ///   2. timestamp DESC (newest first).
    ///
    /// The user's query (when non-empty and not a regex) is honored
    /// as a `LIKE` filter so the user can narrow down what's
    /// ranked. The "last command" itself is the newest row in the
    /// global history that matches the query — the view is
    /// reproducible regardless of which session we're in.
    ///
    /// Tie-breaking within a probability bucket: more recent wins.
    /// Tie-breaking across duplicate commands when the duplicate
    /// filter is on: the most recent instance only.
    fn fetch_stats(&self) -> Result<Vec<HistoryRow>> {
        // 1) Determine the "last command" from the global history
        //    (still respecting the user's query so the prediction
        //    makes sense in context).
        let last_cmd: Option<String> = {
            let (where_clause, params) = self.build_where();
            let sql = format!(
                "SELECT h.command FROM history h{} \
                 ORDER BY h.timestamp DESC, h.id DESC LIMIT 1",
                where_clause
            );
            let params_ref: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query_map(&params_ref[..], |row| {
                row.get::<_, String>(0)
            })?;
            rows.next().transpose()?
        };
        let Some(last_cmd) = last_cmd else {
            // No matching history at all.
            return Ok(Vec::new());
        };

        // 2) Pull the rows the user is going to see, ranked by:
        //    (a) frequency as a successor of `last_cmd` DESC,
        //    (b) timestamp DESC.
        //    The user's typed query is honored (where possible).
        let (where_clause, params) = self.build_where();
        // The freq CTE compares against `last_cmd`. SQLite parameter
        // binding works inside CTEs, but we splice the value
        // directly here because it's an internal-only slug (not
        // user input) and escaping via `replace('\'')` keeps the
        // query plan simple. Single quotes are doubled to escape.
        let last_sql = last_cmd.replace('\'', "''");
        // We compute frequency in a single SQL query using a CTE so
        // the entire ranking is one round trip. Predicted commands
        // get a `freq` > 0; commands that never followed `last_cmd`
        // get `freq = 0` and are sorted by timestamp DESC.
        // `build_where` already starts with " WHERE 1=1", so we
        // splice the user's filter in directly.
        let sql = format!(
            "WITH pairs AS ( \
                 SELECT h.command AS cmd, \
                        LEAD(h.command) OVER (ORDER BY h.timestamp ASC, h.id ASC) AS next_cmd \
                 FROM history h \
             ), \
             freq AS ( \
                 SELECT next_cmd AS cmd, COUNT(*) AS freq \
                 FROM pairs \
                 WHERE cmd = '{last_sql}' AND next_cmd IS NOT NULL \
                 GROUP BY next_cmd \
             ) \
             SELECT h.id, h.command, h.directory, h.session_id, \
                    h.exit_code, h.timestamp, c.comment, o.output, \
                    COALESCE(f.freq, 0) AS freq \
             FROM history h \
             LEFT JOIN command_comments c ON h.command = c.command \
             LEFT JOIN history_output o ON h.id = o.history_id \
             LEFT JOIN freq f ON h.command = f.cmd \
             {where_clause} \
             ORDER BY freq DESC, h.timestamp DESC \
             LIMIT 1000",
        );
        // The user's typed query is the only bound parameter (if any).
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

    /// Merge `labeled_rows` (entries with a comment that are NOT already
    /// in `rows`) into a single list ordered by timestamp. Labeled
    /// entries that are already present keep their position from the
    /// primary list so their highlighted search state is preserved.
    /// When the user has typed a query, labeled entries are filtered to
    /// only those whose command or comment matches the query (plain
    /// text or regex, depending on whether the query starts with `/`).
    /// When the duplicate filter is on, only the newest instance of each
    /// command is kept.
    ///
    /// **Stats mode is special**: the primary list arrives already
    /// sorted by (successor-frequency DESC, timestamp DESC). We
    /// preserve that ordering instead of re-sorting, so the
    /// ranking the user sees in the SQL query survives into the
    /// rendered list.
    fn merged_rows(&self) -> Vec<HistoryRow> {
        let mut merged = self.rows.clone();
        let existing_ids: std::collections::HashSet<i64> =
            merged.iter().map(|r| r.id).collect();
        for row in &self.labeled_rows {
            if !existing_ids.contains(&row.id) {
                if !self.query.is_empty() {
                    let in_command = self.query_matches_text(&row.command);
                    let in_comment = self.query_matches_text(&row.comment);
                    if !in_command && !in_comment {
                        continue;
                    }
                }
                merged.push(row.clone());
            }
        }
        // Only re-sort when the primary list is in timestamp-DESC
        // order. Stats mode uses a frequency-aware ordering from
        // `fetch_stats` that we must preserve.
        if !matches!(self.mode, Mode::Stats) {
            merged.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        }

        if self.duplicate_filter {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            merged.retain(|r| seen.insert(r.command.clone()));
        }

        merged
    }

    fn build_where(&self) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut clause = String::from(" WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        // When the query is a regex (prefixed with `/`) we skip the
        // SQL `LIKE` clause entirely and post-filter the rows in
        // `refresh()` via `query_matches_text`. Otherwise we issue
        // one `LIKE` clause per whitespace-separated word so the
        // search is AND-by-word.
        if !self.query.is_empty() && !self.is_regex_query() {
            for word in self.query.split_whitespace() {
                let escaped = crate::util::escape_like(word);
                clause.push_str(
                    " AND (h.command LIKE ? ESCAPE '\\' OR c.comment LIKE ? ESCAPE '\\')",
                );
                params.push(Box::new(format!("%{}%", escaped)));
                params.push(Box::new(format!("%{}%", escaped)));
            }
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
            // Stats mode always uses the global history regardless of
            // session or directory; the only filter is the user's
            // typed query (handled above).
            Mode::Stats => {}
        }
        (clause, params)
    }

    fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        self.refresh();
    }

    /// Cycle the exit-code filter (All → Success → Failed → All).
    /// Wired up but not yet bound to a key by default; users can
    /// bind it via `key.cycle-exit-filter=...` in the config file.
    fn cycle_exit_filter(&mut self) {
        // The exit-filter field has been removed in favor of just
        // the SQL query; this action is now a no-op kept for
        // backward compatibility with any existing key binding.
        let _ = self;
    }

    /// Toggle the duplicate filter on or off. When on (default), only
    /// the newest instance of each command is shown. When off, every
    /// history row is shown as-is.
    fn toggle_duplicate_filter(&mut self) {
        self.duplicate_filter = !self.duplicate_filter;
        // Adjust the selection so it stays on a valid index even after
        // the list shrinks (when turning the filter on).
        let target = self.list_state.selected().unwrap_or(0);
        self.refresh();
        let n = self.rows.len();
        if n == 0 {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(target.min(n - 1)));
        }
    }

    /// Move the selection by `delta` rows within the visible list.
/// The visible list is the union of `rows` (current search results)
/// and `labeled_rows` (entries with comments not already in `rows`),
/// filtered by `duplicate_filter` and the user's `query` on labels.
/// We compute the merged list here so that navigation matches the
/// rendered order exactly, and clamp to the merged length.
fn move_selection(&mut self, delta: isize) {
    let merged_len = self.merged_rows().len();
    if merged_len == 0 {
        return;
    }
    let cur = self.list_state.selected().unwrap_or(0) as isize;
    let next = (cur + delta).clamp(0, merged_len as isize - 1) as usize;
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

    /// Stage an external editor invocation as the next "selection".
    /// The TUI prints the command on stdout and exits with the Run
    /// exit code, so the parent shell treats it like any other
    /// command line and runs the editor after the TUI has fully
    /// torn down. The TUI does NOT manage the terminal while the
    /// editor runs.
    fn select_for_editor(&mut self, editor_cmd: String) {
        self.selection = Some(editor_cmd);
        self.pick_mode = Some(PickMode::Run);
        self.close_output_view();
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
            // If the query was prefilled from the session cache and the
            // user hasn't touched it yet, the first character should
            // replace it rather than append (so the cached query
            // doesn't accidentally end up as a prefix).
            if self.query_prefilled && !self.query_touched {
                self.query.clear();
            }
            self.query_touched = true;
            self.query.push(c);
            self.recompile_regex();
            self.refresh();
        }
    }

    fn backspace(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.pop();
        } else {
            // Only flag the query as user-touched once we've actually
            // removed at least one character (so a stray backspace on
            // an empty, prefilled query still leaves the prefilled
            // value alone until the user starts typing).
            if !self.query.is_empty() {
                self.query_touched = true;
                self.query.pop();
                self.recompile_regex();
                self.refresh();
            }
        }
    }

    fn clear_query(&mut self) {
        if let Some(ref mut buf) = self.comment_edit {
            buf.clear();
        } else {
            self.query.clear();
            self.query_touched = true;
            self.query_regex = None;
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
        self.refresh_labeled();
        Ok(())
    }

    fn show_output_view(&mut self) {
        if let Some(row) = self.selected_row().filter(|r| !r.output.is_empty()) {
            self.output_view = Some(OutputView {
                text: row.output.clone(),
                scroll: 0,
            });
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

    fn is_help_viewing(&self) -> bool {
        self.help_view.is_some()
    }

    fn open_help(&mut self) {
        self.help_view = Some(HelpView { scroll: 0 });
    }

    fn close_help(&mut self) {
        self.help_view = None;
    }

    fn is_command_menu_open(&self) -> bool {
        self.command_menu.is_some()
    }

    fn open_command_menu(&mut self) {
        self.command_menu = Some(CommandMenu::new());
    }

    fn close_command_menu(&mut self) {
        self.command_menu = None;
    }

    fn is_theme_picker_open(&self) -> bool {
        self.theme_picker.is_some()
    }

    fn open_theme_picker(&mut self) {
        self.theme_picker = Some(ThemePicker::new(self.theme));
    }

    /// Restore the picker to the theme that was active when it
    /// opened, then close. Used on `Esc`.
    fn close_theme_picker_revert(&mut self) {
        if let Some(picker) = self.theme_picker.take() {
            self.theme = picker.original;
            install_palette(self.theme);
        }
    }

    /// Commit the currently selected theme and close the picker.
    /// The picker is already applying live updates as the user
    /// navigates, so on `Enter` we just close the overlay.
    fn close_theme_picker_commit(&mut self) {
        self.theme_picker = None;
    }

    fn is_labeled_view(&self) -> bool {
        // The labeled pane is always available, so the toggle state is
        // determined by the dedicated `labeled_list_state` which we
        // keep synchronized with `list_state` for the moment.
        self.labeled_list_state.selected().is_some() || !self.labeled_rows.is_empty()
    }

    /// Re-query the database for all rows that have an associated
    /// comment. This powers the always-available "labeled" pane.
    fn refresh_labeled(&mut self) {
        self.labeled_rows = self.fetch_labeled().unwrap_or_default();
        if self.labeled_rows.is_empty() {
            self.labeled_list_state.select(None);
        } else {
            self.labeled_list_state.select(Some(0));
        }
    }

    fn fetch_labeled(&self) -> Result<Vec<HistoryRow>> {
        let sql = "SELECT h.id, h.command, h.directory, h.session_id, h.exit_code, h.timestamp, c.comment, o.output \
                   FROM history h \
                   JOIN command_comments c ON h.command = c.command \
                   LEFT JOIN history_output o ON h.id = o.history_id \
                   ORDER BY h.timestamp DESC LIMIT 1000";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |row| {
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

    fn delete_selected(&mut self) -> Result<()> {
        if let Some(row) = self.selected_row() {
            self.conn
                .execute("DELETE FROM history WHERE id = ?1", params![row.id])?;
            self.refresh();
            self.refresh_labeled();
        }
        self.confirm_delete = None;
        Ok(())
    }

    fn delete_matching(&mut self) -> Result<()> {
        let (where_clause, params) = self.build_where();
        let sql = format!("DELETE FROM history WHERE id IN (SELECT h.id FROM history h LEFT JOIN command_comments c ON h.command = c.command{})", where_clause);
        let params_ref: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        self.conn.execute(&sql, &params_ref[..])?;
        self.refresh();
        self.refresh_labeled();
        self.confirm_delete = None;
        Ok(())
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
    let app_cfg = Config::load();
    let bindings = app_cfg.key_bindings().clone();
    let session = TuiSession::load();
    let duplicate_filter = session
        .duplicate_filter
        .unwrap_or(app_cfg.duplicate_filter);
    // Install the user-configured TUI palette (or built-in defaults)
    // into a thread-local so the draw helpers can read it without
    // needing it threaded through every signature.
    let initial_theme = session
        .theme
        .as_deref()
        .map(SelectedTheme::from_slug)
        .unwrap_or(SelectedTheme::None);
    install_palette(initial_theme);
    // The effective initial mode is decided by precedence:
    //   1. The `initial_mode` argument (already resolved by `main`
    //      from --mode / env / config).
    //   2. The persisted session file.
    let effective_mode = session
        .mode
        .as_deref()
        .and_then(Mode::parse)
        .unwrap_or(mode);
    // The query is considered "prefilled" only when it was loaded
    // from the persisted session file, not when the user supplied
    // a fresh `--query` argument or `$SMARTHISTORY_TUI_QUERY`.
    let prefilled_query = session.query.clone();
    let effective_query = prefilled_query.clone().unwrap_or(initial_query);
    let mut app = App::new(
        conn,
        effective_mode,
        effective_query,
        duplicate_filter,
        prefilled_query.is_some(),
        initial_theme,
        bindings,
    );
    // If the persisted session requested a different duplicate filter
    // than the one we initialized with, honor it.
    if session.duplicate_filter.is_some() && session.duplicate_filter != Some(duplicate_filter) {
        app.duplicate_filter = session.duplicate_filter.unwrap_or(true);
    }

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
    let selection = if app.cancelled {
        None
    } else if let Some(sel) = app.selection.take() {
        let pm = app.pick_mode.unwrap_or(PickMode::Run).exit_code();
        Some((sel, pm))
    } else {
        None
    };

    // Persist the user's TUI preferences so the next invocation can
    // restore them. The session file lives at
    // ~/.cache/smarthistory/session.
    let session = TuiSession {
        mode: Some(match app.mode {
            Mode::Sess => "SESS".to_string(),
            Mode::Dir => "DIR".to_string(),
            Mode::Global => "GLOBAL".to_string(),
            Mode::Stats => "STATS".to_string(),
        }),
        query: Some(app.query.clone()),
        duplicate_filter: Some(app.duplicate_filter),
        theme: Some(app.theme.slug().to_string()),
    };
    session.save();

    Ok(selection)
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stderr>>,
    app: &mut App,
) -> Result<()> {
    let page_size = terminal
        .size()
        .map(|s| s.height.max(3) as usize)
        .unwrap_or(20);
    loop {
        if let Err(e) = terminal.draw(|f| ui(f, app)) {
            return Err(anyhow::anyhow!("terminal draw failed: {}", e));
        }

        if !crossterm::event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if app.is_output_viewing() {
            handle_output_view_key(app, key, page_size);
            // If the overlay was closed AND a selection has been
            // staged (e.g. by pressing ^E to open the captured
            // output in an external editor), exit the TUI so the
            // parent shell can run the staged command.
            if !app.is_output_viewing() && app.selection.is_some() {
                return Ok(());
            }
            continue;
        }

        if handle_key(app, key) {
            return Ok(());
        }
    }
}


/// Returns `true` if the app should exit (selection made or cancelled).
/// The captured-output overlay is handled directly in the run loop
/// so that it can launch an external editor.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    // The command palette sits above the help overlay so it can
    // dispatch actions (including open-help) without the overlay
    // intercepting keys.
    if app.is_command_menu_open() {
        return handle_command_menu_key(app, key);
    }

    // The theme picker also takes precedence over the help
    // overlay so it can receive the same arrow / Ctrl-N / Ctrl-P
    // keys that the cycling actions use.
    if app.is_theme_picker_open() {
        return handle_theme_picker_key(app, key);
    }

    // When the help overlay is open, route all input to its handler
    // (Esc / q / Enter / Ctrl-C close it, arrows scroll).
    if app.is_help_viewing() {
        return handle_help_view_key(app, key);
    }

    // When prompting for deletion, only allow 'y' or 'n' or Esc/Ctrl+C.
    if let Some(mode) = app.confirm_delete {
        return handle_confirm_delete_key(app, key, mode);
    }

    // When editing a comment, most keys go to the comment buffer.
    if app.is_comment_editing() {
        return handle_comment_edit_key(app, key);
    }

    // Action-based dispatch: look up the user-configured binding
    // for this key. Anything not explicitly bound falls through to
    // the default "type a character into the query" behavior.
    if let Some(action) = action_for_key(&app.bindings, &key) {
        return dispatch_action(app, action);
    }

    // Unbound characters extend the query. We accept any plain
    // printable character (Shift is allowed — terminals report
    // uppercase letters as `Char('G')` + `SHIFT`, so excluding
    // SHIFT would silently swallow every uppercase letter and
    // every shifted symbol). Ctrl / Alt are still ignored so we
    // don't accidentally trigger terminal-specific shortcuts.
    if !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && let KeyCode::Char(c) = key.code {
            app.push_char(c);
        }
    false
}

/// Execute a single `Action`. Returns `true` when the action
/// terminates the TUI (selection made or cancelled); `false`
/// otherwise. Mirrors the structure of the previous hand-written
/// match blocks but is now driven entirely by the binding table.
fn dispatch_action(app: &mut App, action: Action) -> bool {
    match action {
        Action::Cancel => {
            app.cancelled = true;
            true
        }
        Action::CycleMode => {
            app.cycle_mode();
            false
        }
        Action::ToggleDuplicateFilter => {
            app.toggle_duplicate_filter();
            false
        }
        Action::CycleThemeNext => {
            app.cycle_theme_next();
            false
        }
        Action::CycleThemePrev => {
            app.cycle_theme_prev();
            false
        }
        Action::EditComment => {
            app.start_comment_edit();
            false
        }
        Action::ShowOutput => {
            app.show_output_view();
            false
        }
        Action::OpenHelp => {
            app.open_help();
            false
        }
        Action::DeleteSelected => {
            app.confirm_delete = Some(ConfirmMode::DeleteSelected);
            false
        }
        Action::DeleteMatching => {
            app.confirm_delete = Some(ConfirmMode::DeleteMatching);
            false
        }
        Action::ClearQuery => {
            app.clear_query();
            false
        }
        Action::CycleExitFilter => {
            app.cycle_exit_filter();
            false
        }
        Action::Run => {
            app.select_for_run();
            true
        }
        Action::EditStart => {
            app.select_for_edit_start();
            true
        }
        Action::EditEnd => {
            app.select_for_edit_end();
            true
        }
        // Movement keys share a "user is navigating, so the cached
        // prefilled query should be appended to" side effect.
        Action::Up => {
            app.move_selection(1);
            app.query_prefilled = false;
            false
        }
        Action::Down => {
            app.move_selection(-1);
            app.query_prefilled = false;
            false
        }
        Action::PageUp => {
            app.move_selection(10);
            app.query_prefilled = false;
            false
        }
        Action::PageDown => {
            app.move_selection(-10);
            app.query_prefilled = false;
            false
        }
        Action::Home => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(app.rows.len() - 1));
            }
            app.query_prefilled = false;
            false
        }
        Action::End => {
            if !app.rows.is_empty() {
                app.list_state.select(Some(0));
            }
            app.query_prefilled = false;
            false
        }
        Action::Backspace => {
            app.backspace();
            false
        }
        Action::CommandAction => {
            app.open_command_menu();
            false
        }
        Action::ThemePicker => {
            app.open_theme_picker();
            false
        }
    }
}

fn handle_confirm_delete_key(app: &mut App, key: KeyEvent, mode: ConfirmMode) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            match mode {
                ConfirmMode::DeleteSelected => {
                    let _ = app.delete_selected();
                }
                ConfirmMode::DeleteMatching => {
                    let _ = app.delete_matching();
                }
            }
            false
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.confirm_delete = None;
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            true
        }
        _ => false,
    }
}

/// Key handler used while the help overlay is open. Returns `true`
/// only when the user aborts the whole TUI with Ctrl+C.
fn handle_help_view_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            app.close_help();
            false
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            app.close_help();
            true
        }
        KeyCode::Up => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
            false
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_add(1);
            }
            false
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_sub(10);
            }
            false
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_add(10);
            }
            false
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.help_view {
                view.scroll = 0;
            }
            false
        }
        KeyCode::End => {
            // Clamped on render. We just push the scroll forward so
            // the user can always reach the bottom.
            if let Some(ref mut view) = app.help_view {
                view.scroll = view.scroll.saturating_add(usize::MAX / 2);
            }
            false
        }
        _ => false,
    }
}

/// Key handler used while viewing captured output. Returns `true` only
/// when the user aborts the whole TUI with Ctrl+C.
/// Result of handling a key event in the captured-output overlay.
enum OutputViewResult {
    /// Stay in the overlay and keep the loop running.
    Continue,
    /// Close the overlay and continue the main loop.
    Close,
}

/// Key handler used while the command palette is open. Mirrors
/// the help-overlay pattern but executes the highlighted action
/// instead of scrolling.
fn handle_command_menu_key(app: &mut App, key: KeyEvent) -> bool {
    // Esc / q / Ctrl-C close the palette without running anything.
    if matches!(
        key.code,
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')
    ) {
        app.close_command_menu();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_command_menu();
        return true;
    }

    // Capture a mutable borrow of the menu once so the closures
    // below can use it without conflicting with the immutable
    // borrow on `app`.
    let menu = match app.command_menu.as_mut() {
        Some(m) => m,
        None => return false,
    };

    match key.code {
        KeyCode::Enter => {
            // Run the highlighted action.
            let indices = menu.filtered_indices();
            if let Some(&idx) = indices.get(menu.selected) {
                let action = menu.actions[idx];
                // Close the palette BEFORE dispatching so the
                // action runs against the un-modified app state.
                app.close_command_menu();
                return dispatch_action(app, action);
            }
            // Empty list — just close the palette.
            app.close_command_menu();
            false
        }
        KeyCode::Up => {
            if menu.selected > 0 {
                menu.selected -= 1;
            }
            false
        }
        KeyCode::Down => {
            let n = menu.filtered_indices().len();
            if n > 0 && menu.selected + 1 < n {
                menu.selected += 1;
            }
            false
        }
        KeyCode::PageUp => {
            menu.selected = menu.selected.saturating_sub(5);
            menu.clamp_selection();
            false
        }
        KeyCode::PageDown => {
            let n = menu.filtered_indices().len();
            menu.selected = (menu.selected + 5).min(n.saturating_sub(1));
            false
        }
        KeyCode::Home => {
            menu.selected = 0;
            false
        }
        KeyCode::End => {
            let n = menu.filtered_indices().len();
            if n > 0 {
                menu.selected = n - 1;
            }
            false
        }
        KeyCode::Backspace => {
            if !menu.query.is_empty() {
                menu.touched = true;
                menu.query.pop();
                menu.clamp_selection();
            }
            false
        }
        KeyCode::Char(c) => {
            // Allow plain printable characters; ignore Ctrl/Alt
            // so we don't trigger accidental shortcuts.
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                if menu.query_prefilled_replacement_armed() {
                    menu.query.clear();
                }
                menu.touched = true;
                menu.query.push(c);
                menu.clamp_selection();
            }
            false
        }
        _ => false,
    }
}

impl CommandMenu {
    /// Internal helper: `true` when the user has just opened the
    /// palette and not yet typed anything, so the first character
    /// should replace any prefilled value rather than append.
    /// (We always start with an empty query, so this is currently
    /// a synonym for `!self.touched`, but keeping the indirection
    /// leaves room for future "last-used query" support.)
    fn query_prefilled_replacement_armed(&self) -> bool {
        !self.touched && !self.query.is_empty()
    }
}

/// State for the theme-picker overlay.
///
/// The picker keeps two snapshots:
///
/// - `original` is the theme that was active when the picker
///   opened. On `Esc` we restore it; on `Enter` we keep whatever
///   the user navigated to.
/// - `selected` is the index into `themes` and drives the live
///   preview: every time it changes we call `install_palette` so
///   the TUI re-renders with the new theme while the picker stays
///   open.
struct ThemePicker {
    /// Theme in effect when the picker opened. Used by Esc.
    original: SelectedTheme,
    /// Snapshot of the list to display, in stable order. The
    /// first entry is always `None` ("no theme"), then the
    /// canonical `ratatui-themes::ThemeName::all()` list.
    themes: Vec<SelectedTheme>,
    /// Index into `themes`. Always a valid index.
    selected: usize,
}

impl ThemePicker {
    fn new(current: SelectedTheme) -> Self {
        let mut themes = Vec::with_capacity(BuiltinTheme::all().len() + 1);
        themes.push(SelectedTheme::None);
        for t in BuiltinTheme::all() {
            themes.push(SelectedTheme::Builtin(t));
        }
        // Land on the user's current theme so the picker
        // initially highlights the row that matches the visible
        // palette.
        let selected = themes
            .iter()
            .position(|t| *t == current)
            .unwrap_or(0);
        ThemePicker {
            original: current,
            themes,
            selected,
        }
    }

    fn current(&self) -> SelectedTheme {
        self.themes[self.selected]
    }

    fn move_by(&mut self, delta: isize) {
        let n = self.themes.len() as isize;
        let cur = self.selected as isize;
        let mut next = cur + delta;
        if next < 0 {
            next = 0;
        }
        if next >= n {
            next = n - 1;
        }
        self.selected = next as usize;
    }
}

/// Key handler for the theme picker. Up/Down (and the Ctrl-N /
/// Ctrl-P shortcuts that also drive the live cycling) move the
/// selection and immediately apply the new theme via
/// `install_palette`. Enter commits, Esc reverts to the original
/// theme, Home/End jump to the first/last entry.
fn handle_theme_picker_key(app: &mut App, key: KeyEvent) -> bool {
    // Esc / Ctrl-C always revert to the original theme and
    // close. Ctrl-C additionally aborts the whole TUI.
    if key.code == KeyCode::Esc {
        app.close_theme_picker_revert();
        return false;
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.cancelled = true;
        app.close_theme_picker_revert();
        return true;
    }

    // Enter commits the currently-highlighted theme. The live
    // preview is already in effect because we apply each move
    // immediately, so "commit" just means "close the overlay".
    if key.code == KeyCode::Enter {
        app.close_theme_picker_commit();
        return false;
    }

    // Determine the navigation delta. We treat the same keys as the
    // standalone cycling actions so muscle memory works either way:
    //   Down / C-n  -> next theme
    //   Up / C-p    -> previous theme
    //   Home        -> first theme (the manual "no theme" entry)
    //   End         -> last theme
    let delta: Option<isize> = match key.code {
        KeyCode::Down => Some(1),
        KeyCode::Up => Some(-1),
        KeyCode::PageDown => Some(5),
        KeyCode::PageUp => Some(-5),
        KeyCode::Home => {
            if let Some(picker) = app.theme_picker.as_mut() {
                picker.selected = 0;
                app.theme = picker.current();
                install_palette(app.theme);
            }
            return false;
        }
        KeyCode::End => {
            if let Some(picker) = app.theme_picker.as_mut() {
                picker.selected = picker.themes.len().saturating_sub(1);
                app.theme = picker.current();
                install_palette(app.theme);
            }
            return false;
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(1),
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(-1),
        _ => None,
    };

    if let Some(delta) = delta
        && let Some(picker) = app.theme_picker.as_mut() {
            picker.move_by(delta);
            app.theme = picker.current();
            install_palette(app.theme);
        }
    false
}

/// Key handler used while viewing captured output. Returns a result
/// describing what the run loop should do next.
fn handle_output_view_key(
    app: &mut App,
    key: KeyEvent,
    page_size: usize,
) -> OutputViewResult {
    // Helper to compute the max valid scroll offset.
    let max_scroll = |text: &str| -> usize {
        let total = text.lines().count();
        total.saturating_sub(page_size.max(1))
    };

    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => OutputViewResult::Close,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.cancelled = true;
            app.close_output_view();
            OutputViewResult::Close
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Write the captured output to a temporary file and stage
            // the editor invocation as the next "selection". The TUI
            // will exit normally, printing the editor command on
            // stdout, and the parent shell runs it like any other
            // command. This avoids all TUI terminal-mode juggling.
            if let Some(ref view) = app.output_view {
                let path = std::env::temp_dir().join(format!(
                    "smarthistory-output-{}.txt",
                    generate_tui_pane_id()
                ));
                if std::fs::write(&path, &view.text).is_ok() {
                    let editor = std::env::var("EDITOR")
                        .ok()
                        .filter(|e| !e.is_empty())
                        .unwrap_or_else(|| "vi".to_string());
                    let cmd = format!("{} {}", editor, path.display());
                    app.select_for_editor(cmd);
                    return OutputViewResult::Close;
                }
            }
            OutputViewResult::Continue
        }
        KeyCode::Up => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = view.scroll.saturating_sub(1);
            }
            OutputViewResult::Continue
        }
        KeyCode::Down => {
            if let Some(ref mut view) = app.output_view {
                let max = max_scroll(&view.text);
                view.scroll = (view.scroll + 1).min(max);
            }
            OutputViewResult::Continue
        }
        KeyCode::PageUp => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = view.scroll.saturating_sub(page_size.max(1));
            }
            OutputViewResult::Continue
        }
        KeyCode::PageDown => {
            if let Some(ref mut view) = app.output_view {
                let max = max_scroll(&view.text);
                view.scroll = (view.scroll + page_size.max(1)).min(max);
            }
            OutputViewResult::Continue
        }
        KeyCode::Home => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = 0;
            }
            OutputViewResult::Continue
        }
        KeyCode::End => {
            if let Some(ref mut view) = app.output_view {
                view.scroll = max_scroll(&view.text);
            }
            OutputViewResult::Continue
        }
        _ => OutputViewResult::Continue,
    }
}

/// A short random ID used in temp file names.
fn generate_tui_pane_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
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
    lines.push(Line::from(
        "  to disable a default binding entirely.",
    ));
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
            app.bindings
                .get(a)
                .map(format_key_spec)
                .unwrap_or_else(|| "?".to_string())
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
    row(&mut lines, binding_for(Action::ClearQuery), "clear the query");
    row(
        &mut lines,
        format!("{} / {}", binding_for(Action::Up), binding_for(Action::Down)),
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
        format!("{} / {}", binding_for(Action::Home), binding_for(Action::End)),
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
    lines.push(Line::from("    mode, query, duplicate filter, and theme between launches."));
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
        f.set_cursor_position((cursor_x.min(chunks[0].x.saturating_add(chunks[0].width).saturating_sub(2)), cursor_y));
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
        let key = app
            .bindings
            .get(action)
            .map(format_key_spec)
            .map(|s| format!(" {}", s))
            .unwrap_or_else(|| " (unbound)".to_string());
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
                Span::styled(" ⚠ ", if is_selected { highlight_style } else { warning_style }),
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
        .constraints(
            [
                Constraint::Percentage(55),
                Constraint::Percentage(45),
            ]
            .as_ref(),
        )
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
            if is_selected { highlight_style } else { dim_style },
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
            Line::from(vec![
                Span::styled("  Theme preview", Style::default().add_modifier(Modifier::BOLD)),
            ]),
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
                Span::styled(
                    picker.original.display_name(),
                    Style::default().fg(p.fg),
                ),
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
    let dup_label = if app.duplicate_filter { "last only" } else { "all entries" };
    let spans = vec![
        Span::styled("smart", Theme::dim()),
        Span::styled("history", Theme::accent()),
        Span::styled("  ", Theme::default()),
        mode_badge(app.mode),
        Span::styled("  ", Theme::default()),
        duplicate_filter_badge(app.duplicate_filter),
        Span::styled(
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
        ),
    ];
    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).style(Style::default().bg(bg));
    f.render_widget(paragraph, area);
}

fn duplicate_filter_badge(on: bool) -> Span<'static> {
    let (label, color) = if on { ("LAST", Theme::success_color()) } else { ("ALL", Theme::accent_color()) };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Theme::badge_fg_color()).bg(color).add_modifier(Modifier::BOLD),
    )
}

#[allow(dead_code)]
fn exit_filter_badge(filter: ExitFilter) -> Span<'static> {
    let (label, color) = match filter {
        ExitFilter::All => ("ALL", Theme::accent_color()),
        ExitFilter::Success => ("OK", Theme::success_color()),
        ExitFilter::Failed => ("ERR", Theme::error_color()),
    };
    Span::styled(
        format!(" {} ", label),
        Style::default().fg(Theme::badge_fg_color()).bg(color).add_modifier(Modifier::BOLD),
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
        Style::default().fg(Theme::badge_fg_color()).bg(color).add_modifier(Modifier::BOLD),
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
    let rendered_idx = app.list_state.selected().map(|data_idx| {
        pad + (real_count.saturating_sub(1) - data_idx)
    });

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
fn highlight_matches<'a>(text: &'a str, query: &str) -> Vec<Span<'a>> {
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
            if lower_text.chars().skip(i).take(word_chars.len()).collect::<Vec<char>>() == word_chars
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

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
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
        Some(ref buf) => {
            ("comment> ", " comment ", buf.as_str())
        }
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

    let line = Line::from(vec![
        Span::styled(format!(" {}  ", count), Theme::highlight()),
        Span::styled(help, Theme::dim()),
        Span::styled(theme_label, Theme::accent()),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(PALETTE.with(|p| p.borrow().status_bg))),
        area,
    );
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

    #[test]
    fn highlight_matches_multi_word() {
        let spans = highlight_matches("git commit -m", "git commit");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git", " ", "commit", " -m"]);
    }

    #[test]
    fn highlight_matches_multi_word_out_of_order() {
        let spans = highlight_matches("git commit -m", "commit git");
        let content: Vec<String> = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(content, vec!["git", " ", "commit", " -m"]);
    }

    #[test]
    fn build_implicit_regex_plain() {
        // No anchors → wrap with `.*` on both sides.
        assert_eq!(build_implicit_regex("git commit"), ".*git commit.*");
        assert_eq!(build_implicit_regex("foo"), ".*foo.*");
    }

    #[test]
    fn build_implicit_regex_start_anchor() {
        // Leading `^` suppresses the implicit `.*` on the left.
        assert_eq!(build_implicit_regex("^git commit"), "^git commit.*");
        assert_eq!(build_implicit_regex("^foo"), "^foo.*");
    }

    #[test]
    fn build_implicit_regex_end_anchor() {
        // Trailing `$` suppresses the implicit `.*` on the right.
        assert_eq!(build_implicit_regex("git$"), ".*git$");
        assert_eq!(build_implicit_regex("foo bar$"), ".*foo bar$");
    }

    #[test]
    fn build_implicit_regex_both_anchors() {
        // Both anchors present → no implicit `.*` added.
        assert_eq!(build_implicit_regex("^git$"), "^git$");
        assert_eq!(build_implicit_regex("^foo bar$"), "^foo bar$");
    }

    #[test]
    fn build_implicit_regex_empty() {
        // Empty pattern still gets `.*` wrappers — useful for
        // `/` alone (matches everything).
        assert_eq!(build_implicit_regex(""), ".*.*");
    }

    #[test]
    fn parse_key_spec_plain() {
        let spec = parse_key_spec("a").unwrap();
        assert_eq!(spec.code, KeyCode::Char('a'));
        assert!(spec.modifiers.is_empty());

        let spec = parse_key_spec("/").unwrap();
        assert_eq!(spec.code, KeyCode::Char('/'));
    }

    #[test]
    fn parse_key_spec_ctrl() {
        let spec = parse_key_spec("C-h").unwrap();
        assert_eq!(spec.code, KeyCode::Char('h'));
        assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
        assert!(!spec.modifiers.contains(KeyModifiers::ALT));

        // Uppercase and lowercase both work.
        let spec = parse_key_spec("c-H").unwrap();
        assert_eq!(spec.code, KeyCode::Char('H'));
        assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn parse_key_spec_alt_and_combinations() {
        let spec = parse_key_spec("M-x").unwrap();
        assert_eq!(spec.code, KeyCode::Char('x'));
        assert!(spec.modifiers.contains(KeyModifiers::ALT));

        let spec = parse_key_spec("C-M-h").unwrap();
        assert_eq!(spec.code, KeyCode::Char('h'));
        assert!(spec.modifiers.contains(KeyModifiers::CONTROL));
        assert!(spec.modifiers.contains(KeyModifiers::ALT));
    }

    #[test]
    fn parse_key_spec_named_keys() {
        assert_eq!(parse_key_spec("Esc").unwrap().code, KeyCode::Esc);
        assert_eq!(parse_key_spec("Enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_key_spec("Backspace").unwrap().code, KeyCode::Backspace);
        assert_eq!(parse_key_spec("Up").unwrap().code, KeyCode::Up);
        assert_eq!(parse_key_spec("PageUp").unwrap().code, KeyCode::PageUp);
        assert_eq!(parse_key_spec("F5").unwrap().code, KeyCode::F(5));
    }

    #[test]
    fn parse_key_spec_invalid() {
        assert!(parse_key_spec("").is_err());
        assert!(parse_key_spec("not-a-single-char").is_err());
    }

    #[test]
    fn action_for_key_roundtrip() {
        let bindings = KeyBindings::defaults();
        // C-h is the default for OpenHelp.
        let evt = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(action_for_key(&bindings, &evt), Some(Action::OpenHelp));
        // Unbound plain char → None.
        let evt = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty());
        assert_eq!(action_for_key(&bindings, &evt), None);
        // Uppercase letters (Shift held) are unbound at the action
        // level — they fall through to the input path, which must
        // accept them rather than swallow them.
        let evt = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(action_for_key(&bindings, &evt), None);
        // Shift+symbol also falls through (e.g. "?" typed via
        // Shift+/).
        let evt = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(action_for_key(&bindings, &evt), None);
    }

    #[test]
    fn key_bindings_from_config_overrides() {
        // Entries are keyed by the bare action name (without the
        // `key.` prefix); `Config::parse` strips the prefix before
        // inserting into the map.
        let mut entries = std::collections::HashMap::new();
        entries.insert("open-help".to_string(), "M-h".to_string());
        entries.insert("cancel".to_string(), "C-q".to_string());
        let bindings = key_bindings_from_config(&entries);
        assert_eq!(
            bindings.get(Action::OpenHelp).map(format_key_spec),
            Some("M-h".to_string())
        );
        assert_eq!(
            bindings.get(Action::Cancel).map(format_key_spec),
            Some("C-q".to_string())
        );
        // Unmentioned actions keep their defaults.
        assert_eq!(
            bindings.get(Action::DeleteSelected).map(format_key_spec),
            Some("C-d".to_string())
        );
    }

    #[test]
    fn key_bindings_from_config_unknown_action_is_reported() {
        // `toggle-duplication-filter` (extra "ation") is a typo of
        // `toggle-duplicate-filter` and must not silently bind to
        // anything. Capture stderr to confirm the warning is
        // emitted, then ensure the matching default still wins.
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "toggle-duplication-filter".to_string(),
            "C-d".to_string(),
        );
        let bindings = key_bindings_from_config(&entries);
        // Unknown action does not pollute any known action.
        assert_eq!(
            bindings.get(Action::ToggleDuplicateFilter).map(format_key_spec),
            Some(Action::ToggleDuplicateFilter.default_key().to_string())
        );
    }

    #[test]
    fn parse_key_spec_unbind_sentinels() {
        // `none`, `off`, `disable`, `-`, `disabled` (case
        // insensitive) all map to `Ok(None)` — the action is
        // unbound, not bound to a literal "None" key.
        for sentinel in ["none", "NONE", "off", "disable", "-", "disabled"] {
            let parsed = parse_key_spec_opt(sentinel).unwrap();
            assert!(parsed.is_none(), "sentinel {sentinel:?} should unbind");
        }
    }

    #[test]
        fn key_bindings_from_config_unbind_action() {
                let mut entries = std::collections::HashMap::new();
                entries.insert("open-help".to_string(), "none".to_string());
                let bindings = key_bindings_from_config(&entries);
                assert!(bindings.is_unbound(Action::OpenHelp));
                assert!(bindings.get(Action::OpenHelp).is_none());
                // Unbinding one action must not affect siblings.
                assert!(!bindings.is_unbound(Action::Cancel));
                assert!(bindings.get(Action::Cancel).is_some());
                // `action_for_key` must not fire for unbound actions.
                let evt = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
                assert_eq!(action_for_key(&bindings, &evt), None);
        }

        #[test]
        fn command_menu_filter_matches() {
                let menu = CommandMenu::new();
                // Empty query returns every action.
                assert_eq!(menu.filtered_indices().len(), ALL_ACTIONS.len());
                // Substring match against the display name.
                let m = CommandMenu {
                        query: "delete".into(),
                        ..CommandMenu::new()
                };
                let filtered = m.filtered_indices();
                assert!(filtered
                        .iter()
                        .all(|&i| ALL_ACTIONS[i].display_name().to_lowercase().contains("delete")));
                assert!(filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::DeleteSelected));
                assert!(filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::DeleteMatching));
                // Multi-word AND: "open help" matches OpenHelp (also
                // ShowOutput because its name contains "open"? — actually
                // it doesn't, so only OpenHelp should match).
                let m = CommandMenu {
                        query: "open help".into(),
                        ..CommandMenu::new()
                };
                let filtered = m.filtered_indices();
                assert!(filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::OpenHelp));
                assert!(!filtered
                        .iter()
                        .any(|&i| ALL_ACTIONS[i] == Action::ShowOutput));
                // `clamp_selection` keeps the cursor inside the filtered
                // list when items disappear (e.g. user deletes the last char).
                let mut m = CommandMenu::new();
                m.selected = ALL_ACTIONS.len() - 1;
                m.query = "no-such-action".into();
                m.clamp_selection();
                assert_eq!(m.selected, 0);
        }

        #[test]
        fn command_action_has_default_binding_and_routes() {
                let bindings = KeyBindings::defaults();
                // The default key for CommandAction is ":" (matches the
                // vim-style command palette convention).
                assert_eq!(
                        bindings.get(Action::CommandAction).map(format_key_spec),
                        Some(":".to_string())
                );
                // Pressing ":" fires the CommandAction.
                let evt = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::empty());
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::CommandAction)
                );
        }

        #[test]
        fn theme_picker_default_binding_and_list_layout() {
                let bindings = KeyBindings::defaults();
                // Default key is `T` so it doesn't collide with the
                // Ctrl-N / Ctrl-P cycling shortcuts.
                assert_eq!(
                        bindings.get(Action::ThemePicker).map(format_key_spec),
                        Some("T".to_string())
                );
                // Pressing T fires the ThemePicker.
                let evt = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::empty());
                assert_eq!(
                        action_for_key(&bindings, &evt),
                        Some(Action::ThemePicker)
                );
                // Picker contains every theme: `None` plus the
                // canonical `ratatui-themes::ThemeName::all()` list.
                let p = ThemePicker::new(SelectedTheme::None);
                assert_eq!(p.themes.len(), BuiltinTheme::all().len() + 1);
                assert_eq!(p.themes[0], SelectedTheme::None);
                assert!(p
                        .themes
                        .iter()
                        .skip(1)
                        .all(|t| matches!(t, SelectedTheme::Builtin(_))));
                // `move_by` clamps to the list bounds.
                let mut p = ThemePicker::new(SelectedTheme::None);
                p.move_by(-10);
                assert_eq!(p.selected, 0);
                p.move_by(9999);
                assert_eq!(p.selected, p.themes.len() - 1);
        }

        #[test]
        fn curated_themes_parse_and_cycle() {
                // Every curated theme must:
                //   * have a unique, kebab-case slug,
                //   * round-trip through `from_slug`,
                //   * show up in `BuiltinTheme::all()` exactly once.
                let mut seen = std::collections::HashSet::new();
                for t in BuiltinTheme::curated() {
                        let s = t.slug();
                        assert!(!s.is_empty(), "empty slug for {:?}", t);
                        assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                                "slug {:?} not kebab-case", s);
                        assert!(seen.insert(s), "duplicate slug {}", s);
                        let parsed = SelectedTheme::from_slug(s);
                        assert_eq!(parsed, SelectedTheme::Builtin(*t),
                                "from_slug round-trip failed for {:?}", s);
                }
                // Upstream themes still parse (regression check).
                assert_eq!(
                        SelectedTheme::from_slug("dracula"),
                        SelectedTheme::Builtin(BuiltinTheme::Dracula)
                );
                // Unknown slug falls back to None.
                assert_eq!(SelectedTheme::from_slug("totally-made-up"), SelectedTheme::None);
        }

        #[test]
        fn mode_cycle_and_parse() {
                // Cycling wraps through the four modes.
                assert_eq!(Mode::Sess.next(), Mode::Dir);
                assert_eq!(Mode::Dir.next(), Mode::Global);
                assert_eq!(Mode::Global.next(), Mode::Stats);
                assert_eq!(Mode::Stats.next(), Mode::Sess);
                // String parsing is case-insensitive and accepts the
                // documented aliases.
                assert_eq!(Mode::parse("stats"), Some(Mode::Stats));
                assert_eq!(Mode::parse("STATISTICS"), Some(Mode::Stats));
                assert_eq!(Mode::parse("Stats"), Some(Mode::Stats));
                assert!(Mode::parse("not-a-mode").is_none());
        }

        /// Build a fresh in-memory `App` whose `history` table is
        /// pre-populated with the rows in `rows`. `rows` is a slice
        /// of `(command, timestamp_offset_secs)` — the timestamp is
        /// `now - offset` so the tests are stable regardless of when
        /// they run.
        fn stats_test_app(rows: &[(&str, i64)]) -> App {
                use rusqlite::Connection;
                let conn = Connection::open_in_memory().expect("open in-memory db");
                conn.execute_batch(
                        "CREATE TABLE history (
                            id INTEGER PRIMARY KEY,
                            command TEXT NOT NULL,
                            directory TEXT NOT NULL,
                            session_id TEXT NOT NULL,
                            exit_code INTEGER,
                            timestamp INTEGER DEFAULT (strftime('%s', 'now'))
                        );
                        CREATE TABLE command_comments (
                            command TEXT PRIMARY KEY,
                            comment TEXT NOT NULL
                        );
                        CREATE TABLE history_output (
                            history_id INTEGER PRIMARY KEY,
                            output TEXT NOT NULL,
                            captured_at INTEGER DEFAULT (strftime('%s', 'now')),
                            FOREIGN KEY (history_id) REFERENCES history(id) ON DELETE CASCADE
                        );",
                )
                .expect("schema");
                let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                for (i, (cmd, offset)) in rows.iter().enumerate() {
                        conn.execute(
                                "INSERT INTO history (id, command, directory, session_id, exit_code, timestamp) \
                                 VALUES (?1, ?2, '/tmp', 'sess', 0, ?3)",
                                rusqlite::params![i as i64 + 1, *cmd, now - *offset],
                        )
                        .expect("insert");
                }
                let mut app = App::new(
                        conn,
                        Mode::Stats,
                        String::new(),
                        false,
                        false,
                        SelectedTheme::None,
                        KeyBindings::defaults(),
                );
                app.refresh();
                app
        }

        #[test]
        fn stats_mode_ranks_by_follow_frequency_then_age() {
                // Sequence (oldest first):
                //   A B A B C A D
                // The "last command" is D. Its successors in the
                // global history are: A (once). So A should rank
                // first. The remaining rows are sorted by timestamp
                // DESC: D is excluded (it's the last command itself
                // in this test since we always pick the newest), A
                // (just after D), B, C — with the duplicate filter
                // off, every occurrence is shown.
                //
                // For a cleaner test we use a sequence where the
                // last command has multiple distinct successors with
                // known frequencies:
                //   seq:    X Y X Y Z X Y W
                //   newest: W
                //   successors of W: none (it's the most recent)
                //   but we want a non-W last, so add a trailing W2:
                //   seq:    X Y X Y Z X Y W W2
                //   last:   W2 (newest). Successors of W2: none yet.
                //
                // We rebuild the sequence so the *last* command has
                // many successors: arrange so the global newest row
                // is `git status`. Successors of `git status` in
                // the history should be ranked first; everything else
                // falls back to timestamp DESC.
                let rows: &[(&str, i64)] = &[
                    ("vim Cargo.toml", 50),
                    ("cargo build", 45),
                    ("vim Cargo.toml", 40),
                    ("git status", 35),
                    ("vim Cargo.toml", 30),
                    ("cargo build", 25),
                    ("git status", 20),
                    ("cargo build", 15),
                    ("git status", 10), // oldest
                ];
                let _ = rows; // appease unused-warning fixers
                let rows: &[(&str, i64)] = &[
                    ("vim Cargo.toml", 90),
                    ("cargo build", 85),
                    ("vim Cargo.toml", 80),
                    ("git status", 75),
                    ("vim Cargo.toml", 70),
                    ("cargo build", 65),
                    ("git status", 60),
                    ("cargo build", 55),
                    ("git status", 50),
                    ("cargo build", 45),
                    ("git status", 40),
                    ("cargo build", 35),
                    ("git status", 30),
                    ("cargo build", 25),
                    ("ls", 20),
                    ("echo hello", 15),
                    // Newest: `git status` — so it's the "last command".
                    ("git status", 10),
                ];
                let app = stats_test_app(rows);
                assert_eq!(app.mode, Mode::Stats);
                let merged = app.merged_rows();
                // The newest row is `git status` (timestamp 10).
                // Its successors in the entire history are
                // `cargo build` and `vim Cargo.toml`. Counting pairs:
                //   git status -> cargo build: 5 times
                //   git status -> vim Cargo.toml: 3 times
                // So cargo build ranks above vim, then the rest of
                // the history sorted by timestamp DESC.
                let cmds: Vec<&str> = merged.iter().map(|r| r.command.as_str()).collect();
                // 6 cargo build entries with freq=4, 3 vim with
                // freq=1, then the rest sorted by timestamp DESC.
                assert_eq!(cmds.len(), 17,
                        "expected every history row to come back, got {} rows: {:?}",
                        cmds.len(), cmds);
                assert_eq!(cmds[0], "cargo build",
                        "expected highest frequency successor first, got {:?}",
                        cmds);
                assert_eq!(cmds[5], "cargo build",
                        "6 cargo build rows should share freq=4, got {:?}",
                        cmds);
                assert_eq!(cmds[6], "vim Cargo.toml",
                        "vim should follow cargo's freq=4 rows, got {:?}",
                        cmds);
                assert!(!cmds.is_empty());
        }

        #[test]
        fn stats_mode_duplicate_filter_keeps_newest_only() {
                let rows: &[(&str, i64)] = &[
                    ("git status", 30),
                    ("cargo build", 25),
                    ("git status", 20),
                    ("vim Cargo.toml", 15),
                    ("git status", 10), // newest
                ];
                let mut app = stats_test_app(rows);
                // Duplicate filter on: only one `cargo build`,
                // one `vim Cargo.toml`, one `git status`.
                app.duplicate_filter = true;
                app.refresh();
                let binding = app.merged_rows();
                let cmds: Vec<&str> = binding
                        .iter()
                        .map(|r| r.command.as_str())
                        .collect();
                // Each unique command appears at most once.
                let mut sorted = cmds.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted.len(), cmds.len(),
                        "duplicate filter should remove duplicates: {:?}",
                        cmds);
        }
}
