// Theme picker overlay.

use super::SelectedTheme;
use crate::tui::BuiltinTheme;

pub struct ThemePicker {
    /// Theme in effect when the picker opened. Used by Esc.
    pub(crate) original: SelectedTheme,
    /// Snapshot of the full list, in stable order. The
    /// first entry is always `None` ("no theme"), then the
    /// canonical `BuiltinTheme::all()` list.
    pub(crate) themes: Vec<SelectedTheme>,
    /// Index into the **filtered** list (not `themes`).
    /// Always a valid index (or 0 when the filtered list is
    /// empty).
    pub(crate) selected: usize,
    /// Search query for narrowing the list. When non-empty,
    /// only themes whose `slug()` or `display_name()`
    /// contain the query (case-insensitive substring) are
    /// shown. Typing characters extends the query;
    /// Backspace removes the last char; the list filters
    /// live, matching the command-palette UX.
    pub(crate) query: String,
}

impl ThemePicker {
    pub fn new(current: SelectedTheme) -> Self {
        let mut themes = Vec::with_capacity(BuiltinTheme::all().len() + 1);
        themes.push(SelectedTheme::None);
        for t in BuiltinTheme::all() {
            themes.push(SelectedTheme::Builtin(t));
        }
        // Land on the user's current theme so the picker
        // initially highlights the row that matches the visible
        // palette.
        let selected = themes.iter().position(|t| *t == current).unwrap_or(0);
        ThemePicker {
            original: current,
            themes,
            selected,
            query: String::new(),
        }
    }

    /// The filtered list of themes the user is currently seeing.
    /// When the query is empty, every theme is shown.
    pub(crate) fn filtered(&self) -> Vec<&SelectedTheme> {
        if self.query.is_empty() {
            return self.themes.iter().collect();
        }
        let q = self.query.to_lowercase();
        self.themes
            .iter()
            .filter(|t| {
                t.slug().to_lowercase().contains(&q) || t.display_name().to_lowercase().contains(&q)
            })
            .collect()
    }

    /// The currently-selected theme (from the filtered list).
    pub fn current(&self) -> SelectedTheme {
        self.filtered()
            .get(self.selected)
            .copied()
            .copied()
            .unwrap_or(SelectedTheme::None)
    }

    /// Move the selection by `delta` within the filtered list.
    pub fn move_by(&mut self, delta: isize) {
        let n = self.filtered().len() as isize;
        if n == 0 {
            self.selected = 0;
            return;
        }
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

    /// Append a character to the search query. The filtered list
    /// narrows accordingly; the selection is clamped to remain
    /// a valid index.
    pub(crate) fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.clamp_selection();
    }

    /// Remove the last character from the search query. The
    /// filtered list widens accordingly.
    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.clamp_selection();
    }

    /// Clamp `self.selected` so it remains a valid index into the
    /// filtered list (which may shrink or grow as the user types).
    fn clamp_selection(&mut self) {
        let n = self.filtered().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }
}
