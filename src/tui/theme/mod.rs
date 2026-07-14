// Theme subsystem: registry of 21 built-in palettes plus the
// runtime palette plumbing (resolve_color, Palette, install_palette).

use crate::Config;
use crate::tui::theme::palette_storage::PALETTE;
use ratatui::style::Color;

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, Hash)]
pub enum SelectedTheme {
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
            BuiltinTheme::CatppuccinMocha => Some(ratatui_themes::ThemeName::CatppuccinMocha),
            BuiltinTheme::CatppuccinLatte => Some(ratatui_themes::ThemeName::CatppuccinLatte),
            BuiltinTheme::GruvboxDark => Some(ratatui_themes::ThemeName::GruvboxDark),
            BuiltinTheme::GruvboxLight => Some(ratatui_themes::ThemeName::GruvboxLight),
            BuiltinTheme::TokyoNight => Some(ratatui_themes::ThemeName::TokyoNight),
            BuiltinTheme::SolarizedDark => Some(ratatui_themes::ThemeName::SolarizedDark),
            BuiltinTheme::SolarizedLight => Some(ratatui_themes::ThemeName::SolarizedLight),
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
    /// curated themes load their palette from the TOML files in
    /// `theme/curated/` (see the `curated` module for the
    /// format and how to add a new one).
    pub fn palette(self) -> ratatui_themes::ThemePalette {
        if let Some(name) = self.as_upstream() {
            return name.palette();
        }
        self.curated_palette()
    }

    /// Look up the hand-written palette for one of the curated
    /// themes. Dispatches to the `curated` module, which reads
    /// from the `theme/curated/<slug>.toml` files. Panics at
    /// startup if a curated theme's TOML is missing or
    /// malformed — that's a build-time bug, not a user error.
    fn curated_palette(self) -> ratatui_themes::ThemePalette {
        let c = curated::palette_for(self.slug()).unwrap_or_else(|| {
            panic!(
                "curated theme {:?} not found in theme/curated/",
                self.slug()
            )
        });
        ratatui_themes::ThemePalette {
            accent: c.accent,
            secondary: c.secondary,
            bg: c.bg,
            fg: c.fg,
            muted: c.muted,
            selection: c.selection,
            error: c.error,
            warning: c.warning,
            success: c.success,
            info: c.info,
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
    pub fn slug(&self) -> &'static str {
        match self {
            SelectedTheme::None => "none",
            SelectedTheme::Builtin(t) => t.slug(),
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            SelectedTheme::None => "no theme",
            SelectedTheme::Builtin(t) => t.display_name(),
        }
    }

    /// Cycle to the next theme in the list, wrapping around. The
    /// order is `None` (manual) followed by every theme in
    /// `BuiltinTheme::all()` (upstream first, then curated).
    pub fn next(self) -> Self {
        let themes = Self::ordered_list();
        let pos = themes.iter().position(|t| *t == self).unwrap_or(0);
        themes[(pos + 1) % themes.len()]
    }

    /// Cycle to the previous theme.
    pub fn prev(self) -> Self {
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
    pub fn from_slug(s: &str) -> Self {
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

// --- Palette runtime ---

fn resolve_color(s: &str) -> Color {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#').or_else(|| s.strip_prefix("0x"))
        && hex.len() == 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        )
    {
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
    pub fn next(self) -> Self {
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
pub struct Palette {
    pub(crate) bg: Color,
    pub(crate) fg: Color,
    pub(crate) accent: Color,
    pub(crate) success: Color,
    pub(crate) error: Color,
    pub(crate) warning: Color,
    pub(crate) dim: Color,
    #[allow(dead_code)]
    pub(crate) dimmer: Color,
    pub(crate) highlight: Color,
    /// Foreground color used for the "output search" mode
    /// tint (the `+...` query prefix). Distinct from
    /// `accent` (LLM), `success` (fuzzy), and `warning`
    /// (regex). Sourced from the theme's `info` slot in
    /// built-in palettes and from `tuicolor.info=` in the
    /// manual-config case.
    pub(crate) info: Color,
    /// Background color for the currently-selected row in the list.
    pub(crate) selection: Color,
    /// Foreground color used for badge text. Defaults to `bg` so
    /// the text always contrasts with the bright badge background.
    pub(crate) badge_fg: Color,
    /// Background color for the history list pane. Defaults to
    /// `bg` when the user does not set `tuicolor.listbg=`.
    pub(crate) list_bg: Color,
    /// Background color for the details pane.
    pub(crate) details_bg: Color,
    /// Background color for the search/comment input pane.
    pub(crate) input_bg: Color,
    /// Background color for the status bar.
    pub(crate) status_bg: Color,
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
            info: Color::Blue,
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
            info: resolve_color(&theme.info),
            selection: resolve_color(&cfg.selection(&theme.bg)),
            badge_fg: resolve_color(&cfg.badge_fg(&theme.bg)),
            list_bg: resolve_color(&cfg.list_bg(&theme.bg)),
            details_bg: resolve_color(&cfg.details_bg(&theme.bg)),
            input_bg: resolve_color(&cfg.input_bg(&theme.bg)),
            status_bg: resolve_color(&cfg.status_bg(&theme.bg)),
        }
    }
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
pub fn install_palette(theme: SelectedTheme) {
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
                // `info` is sourced from the theme's own
                // `info` slot when the theme is built-in.
                // The user's `tuicolor.info=` override wins
                // when set. (We don't gate on a "has info
                // override" check the way bg/fg do, because
                // `info` is a pure-foreground accent and the
                // manual config sets it to "blue" by default
                // — there's no meaningful fallback case
                // where the user *should* inherit the
                // theme's slot.)
                info: if cfg_theme.info.is_empty() {
                    p.info
                } else {
                    resolve_color(&cfg_theme.info)
                },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end check that a curated theme loads the same
    /// colors the inline code used to. Catches mistakes in
    /// either the TOML files or the dispatch glue that maps
    /// `BuiltinTheme` to a curated palette.
    #[test]
    fn doom_one_palette_matches_inline_baseline() {
        let p = BuiltinTheme::DoomOne.palette();
        // #73bfff = (115, 191, 255)
        assert_eq!(p.accent, ratatui_themes::Color::Rgb(115, 191, 255));
        // #282c34 = (40, 44, 52)
        assert_eq!(p.bg, ratatui_themes::Color::Rgb(40, 44, 52));
    }
}

mod curated;
pub mod palette_storage;
mod picker;
mod styles;

pub use picker::ThemePicker;
pub use styles::Theme;
