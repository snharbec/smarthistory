// Style helpers (Theme::bg(), Theme::error(), etc.).

use ratatui::style::{Color, Style};

use super::palette_storage::PALETTE;

pub struct Theme;

impl Theme {
    pub fn default() -> Style {
        let p = PALETTE.with(|c| *c.borrow());
        Style::default().fg(p.fg).bg(p.bg)
    }

    pub fn accent() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().accent))
    }

    pub fn success() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().success))
    }

    pub fn error() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().error))
    }

    pub fn dim() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().dim))
    }

    #[allow(dead_code)]
    pub fn dimmer() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().dimmer))
    }

    pub fn highlight() -> Style {
        Style::default().fg(PALETTE.with(|c| c.borrow().highlight))
    }

    #[allow(dead_code)]
    pub fn warning() -> Style {
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
    pub fn accent_color() -> Color {
        PALETTE.with(|c| c.borrow().accent)
    }
    pub fn success_color() -> Color {
        PALETTE.with(|c| c.borrow().success)
    }
    pub fn error_color() -> Color {
        PALETTE.with(|c| c.borrow().error)
    }
    pub fn warning_color() -> Color {
        PALETTE.with(|c| c.borrow().warning)
    }
    pub fn highlight_color() -> Color {
        PALETTE.with(|c| c.borrow().highlight)
    }
    /// Foreground color for the "output search" mode tint
    /// (the `+...` query prefix). Sourced from the active
    /// theme's `info` slot — blue by default — so it
    /// tracks the rest of the palette through theme
    /// changes.
    pub fn info_color() -> Color {
        PALETTE.with(|c| c.borrow().info)
    }
    #[allow(dead_code)]
    pub fn dim_color() -> Color {
        PALETTE.with(|c| c.borrow().dim)
    }

    /// Background color used to highlight the currently-selected
    /// row in the history list. Always comes from the active
    /// theme / palette so it follows theme changes.
    pub fn selection_color() -> Color {
        PALETTE.with(|c| c.borrow().selection)
    }

    /// Foreground color for badge text (inside the bright
    /// mode/scope/dedup chips). Defaults to the global background
    /// so the text always contrasts with the bright background.
    pub fn badge_fg_color() -> Color {
        PALETTE.with(|c| c.borrow().badge_fg)
    }
}
