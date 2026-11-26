// The glyph table for the TUI's box-drawing and status markers.
//
// Same shape as `palette_storage`'s `PALETTE`: a thread-local read by
// every render helper, installed once at TUI startup from
// `tui.glyphs=unicode|ascii`. A thread-local rather than a parameter
// threaded through `render_row` and the 30-odd `draw_*` functions,
// which would otherwise all need a new argument for what is a single
// process-wide choice.
//
// Why it exists: the Unicode glyphs below are all East-Asian Width =
// Ambiguous (`▌ ├ └ ● ▸ ▾ ▶ ≡ ✓ ✗ ┃ │`). Terminals honouring
// "ambiguous characters are double-width" (iA/Iterm2's default under a
// CJK locale) render them two columns wide while the layout counts one,
// so every rule, connector and badge shifts by a cell. The zsh dropdown
// already solves this per-widget with `dropdown.boxchars=ascii|unicode`
// (see `main.rs`'s `dropdown_boxchars` doc comment); this is the TUI's
// equivalent.
use std::cell::Cell;

/// Every non-ASCII glyph the TUI draws, in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    /// Left-edge bar repeated across the selected row's symbol area.
    pub selection_bar: &'static str,
    /// "This row has captured output" marker.
    pub capture: &'static str,
    /// Tree branch connector, not last in its group.
    pub tree_mid: &'static str,
    /// Tree branch connector for the last child of a group.
    pub tree_last: &'static str,
    /// Vertical rule used as the scrollbar track.
    pub scrollbar_track: &'static str,
    /// Scrollbar thumb.
    pub scrollbar_thumb: &'static str,
    /// Collapsed disclosure marker (group closed).
    pub disclosure_closed: &'static str,
    /// Expanded disclosure marker (group open).
    pub disclosure_open: &'static str,
    /// Live session marker.
    pub live: &'static str,
    /// Busy pane marker.
    pub running: &'static str,
    /// Agent-waiting marker.
    pub waiting: &'static str,
    /// Successful exit status.
    pub ok: &'static str,
    /// Failed exit status.
    pub failed: &'static str,
    /// Truncation marker for an overflowing row.
    pub ellipsis: &'static str,
    /// Horizontal rule filling a pane's top border.
    pub rule: &'static str,
}

impl Glyphs {
    pub const fn unicode() -> Self {
        Glyphs {
            selection_bar: "▌",
            capture: "≡",
            tree_mid: "├",
            tree_last: "└",
            scrollbar_track: "│",
            scrollbar_thumb: "┃",
            disclosure_closed: "▸",
            disclosure_open: "▾",
            live: "●",
            running: "▶",
            waiting: "⏸",
            ok: "✓",
            failed: "✗",
            ellipsis: "…",
            rule: "─",
        }
    }

    /// Every glyph a single-column ASCII character. `…` becomes `..`
    /// because a lone `.` reads as punctuation rather than
    /// truncation; the call sites add their own spacing.
    pub const fn ascii() -> Self {
        Glyphs {
            selection_bar: ">",
            capture: "o",
            tree_mid: "+",
            tree_last: "`",
            scrollbar_track: "|",
            scrollbar_thumb: "#",
            disclosure_closed: ">",
            disclosure_open: "v",
            live: "*",
            running: ">",
            waiting: "!",
            ok: "+",
            failed: "x",
            ellipsis: ".",
            rule: "-",
        }
    }
}

thread_local! {
    static GLYPHS: Cell<Glyphs> = const { Cell::new(Glyphs::unicode()) };
}

/// The glyphs the current session draws with.
pub fn glyphs() -> Glyphs {
    GLYPHS.with(|g| g.get())
}

/// Install the glyph set for this session. Called once from
/// `run_tui_to_stdout` alongside `install_palette`.
pub fn install_glyphs(ascii: bool) {
    GLYPHS.with(|g| g.set(if ascii { Glyphs::ascii() } else { Glyphs::unicode() }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ASCII-fallback glyph must be exactly one terminal cell
    /// wide and pure ASCII — that is the entire point of the set.
    #[test]
    fn ascii_glyphs_are_single_column_ascii() {
        let g = Glyphs::ascii();
        for (name, value) in [
            ("selection_bar", g.selection_bar),
            ("capture", g.capture),
            ("tree_mid", g.tree_mid),
            ("tree_last", g.tree_last),
            ("scrollbar_track", g.scrollbar_track),
            ("scrollbar_thumb", g.scrollbar_thumb),
            ("disclosure_closed", g.disclosure_closed),
            ("disclosure_open", g.disclosure_open),
            ("live", g.live),
            ("running", g.running),
            ("waiting", g.waiting),
            ("ok", g.ok),
            ("failed", g.failed),
            ("ellipsis", g.ellipsis),
            ("rule", g.rule),
        ] {
            assert!(value.is_ascii(), "{name} must be ASCII, got {value:?}");
            assert_eq!(value.chars().count(), 1, "{name} must be one cell");
        }
    }

    /// The Unicode set is what every existing test asserts against, so
    /// it must keep its exact current glyphs.
    #[test]
    fn unicode_glyphs_are_unchanged() {
        let g = Glyphs::unicode();
        assert_eq!(g.selection_bar, "▌");
        assert_eq!(g.capture, "≡");
        assert_eq!(g.tree_mid, "├");
        assert_eq!(g.tree_last, "└");
        assert_eq!(g.waiting, "⏸");
        assert_eq!(g.running, "▶");
    }
}
