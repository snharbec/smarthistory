// Theme picker overlay.

use super::SelectedTheme;
use crate::tui::BuiltinTheme;

pub struct ThemePicker {
    /// Theme in effect when the picker opened. Used by Esc.
    pub(crate) original: SelectedTheme,
    /// Snapshot of the list to display, in stable order. The
    /// first entry is always `None` ("no theme"), then the
    /// canonical `ratatui-themes::ThemeName::all()` list.
    pub(crate) themes: Vec<SelectedTheme>,
    /// Index into `themes`. Always a valid index.
    pub(crate) selected: usize,
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
        }
    }

    pub fn current(&self) -> SelectedTheme {
        self.themes[self.selected]
    }

    pub fn move_by(&mut self, delta: isize) {
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
