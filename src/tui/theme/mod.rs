// Theme subsystem: registry of 73 built-in palettes plus the
// runtime palette plumbing (resolve_color, Palette, install_palette).

use crate::Config;
use crate::tui::theme::palette_storage::PALETTE;
use ratatui::style::Color;
use std::io::IsTerminal;

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, Hash)]
pub enum SelectedTheme {
    /// Manually-configured palette (built-in defaults if no config).
    #[default]
    None,
    /// One of the built-in themes.
    Builtin(BuiltinTheme),
}

/// The terminal's current color scheme. Used by
/// `detect_color_scheme()` (a best-effort auto-detection that
/// reads `$COLORFGBG`, `$TERM_PROGRAM`, and `$WT_SESSION`)
/// and by the `theme.light=` / `theme.dark=` config keys
/// (which let the user pick a separate built-in theme for
/// each scheme). The active scheme at startup selects
/// which of the two `theme.<scheme>=...` values applies.
///
/// The default is `Dark`: the historical smarthistory
/// look, the default of every built-in theme, and the
/// scheme of the most common modern terminal (wezterm /
/// kitty / alacritty / gnome-terminal / iTerm2 all
/// default to dark). Users with a light terminal can
/// either set `theme.light=<slug>` in the config file
/// (the loader then picks it up) or the in-TUI theme
/// picker will write the active scheme's slot on
/// commit.
///
/// `Unknown` is the "we have no idea" case — the
/// detection function couldn't read any signal, the
/// user hasn't set either `theme.light` or `theme.dark`,
/// and there's no `theme=` legacy key. In that case the
/// loader falls back to the dark default (the same
/// behaviour `Dark` would produce) and the user can
/// pick a real theme once the TUI is running.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, Hash)]
pub enum ColorScheme {
    Light,
    #[default]
    Dark,
    /// Detection failed AND the user hasn't set either
    /// `theme.light` or `theme.dark`. Treated as `Dark`
    /// at the call site; the variant exists so
    /// `detect_color_scheme()` can communicate the
    /// "I don't know" state without conflating it with
    /// "I know and it's dark".
    Unknown,
}

impl ColorScheme {
    /// The lowercased ASCII label used in the config
    /// file (`theme.light=...` / `theme.dark=...`) and in
    /// status messages / theme-picker hints.
    pub fn label(self) -> &'static str {
        match self {
            ColorScheme::Light => "light",
            ColorScheme::Dark => "dark",
            ColorScheme::Unknown => "unknown",
        }
    }

    /// The opposite scheme. Used by the theme picker to
    /// compute the "this is the OTHER slot's value"
    /// when the user commits a new theme — we want to
    /// know the current state of the dark slot while
    /// the user is editing the light slot, so the
    /// picker can show "you're about to change
    /// the LIGHT theme; the DARK theme is currently
    /// gruvbox-light".
    pub fn other(self) -> ColorScheme {
        match self {
            ColorScheme::Light => ColorScheme::Dark,
            ColorScheme::Dark => ColorScheme::Light,
            ColorScheme::Unknown => ColorScheme::Unknown,
        }
    }
}

/// Best-effort detection of the terminal's color
/// scheme. Returns the most informative answer we can
/// give. The detection is intentionally conservative:
/// if we can't tell, return `ColorScheme::Unknown` (the
/// call site treats that as `Dark` and the user can
/// pick a real theme from inside the TUI).
///
/// Detection order (first non-`Unknown` answer wins):
///
/// 1. **`$COLORFGBG`** — the most reliable signal on
///    Linux / BSD. Set by xterm, rxvt, gnome-terminal,
///    konsole, and most other X11 / Wayland terminals
///    to the default fg and bg ANSI indices
///    (`COLORFGBG="15;0"` = white-on-black = dark;
///    `COLORFGBG="0;15"` = black-on-white = light;
///    `COLORFGBG="default;default"` is what newer
///    terminals write when they can't be classified,
///    and we treat that as `Unknown`). The indices are
///    ANSI palette positions, not RGB, so a high bg
///    index (>= 7) on a light theme typically
///    corresponds to a bright color. We use the
///    "is the bg index in the bright half" heuristic
///    which works for the standard 16-color palette
///    (bg 0-6 = dark theme, bg 7+ = light theme).
///
/// 2. **`$TERM_PROGRAM`** — heuristic for terminals
///    that don't set `COLORFGBG`. `iTerm.app`,
///    `Apple_Terminal`, and `WezTerm` are
///    user-configurable; we don't try to peek at the
///    user's preference, but the absence of any
///    signal leaves us at `Unknown` (which is then
///    treated as `Dark` by the loader). macOS
///    `Terminal.app` sets `TERM_PROGRAM=Apple_Terminal`
///    and its default is light, but most users on macOS
///    running smarthistory have switched to iTerm2 or
///    WezTerm — the default of `Unknown` is the safe
///    answer here.
///
/// 3. **`$WT_SESSION`** — Windows Terminal sets this
///    and defaults to dark; we treat its presence as
///    `Dark` rather than `Unknown` because the vast
///    majority of Windows Terminal users run with the
///    default dark theme.
///
/// 4. **OSC 10 / 11 query** via the
///    `terminal-colorsaurus` crate — the cross-platform
///    standard for "what's the terminal's default
///    bg / fg color?". We send `\x1b]11;?\x07` (query
///    bg), read the terminal's reply with a short
///    timeout (~300ms), and parse the RGB. Supports
///    xterm, iTerm2, kitty, alacritty, gnome-terminal,
///    wezterm, and every other modern terminal. This
///    is the answer for the env-var-blind case (e.g. a
///    bare `xterm-256color` shell that sets none of
///    the above). Skipped if stdout isn't a TTY (so
///    piped invocations don't get their output
///    corrupted by the OSC escape sequence).
///
/// 5. Fallback: `Unknown` (treated as `Dark` by the
///    loader).
///
/// The OSC query is the load-bearing step for users
/// with a "clean" shell environment (no
/// `$COLORFGBG`, no `$TERM_PROGRAM`). The env-var
/// steps run first as a fast first pass — they're
/// effectively free and answer the question in the
/// common case (the user runs iTerm2, xterm,
/// gnome-terminal, or any other terminal that sets
/// `COLORFGBG` on its own). The OSC query is only
/// reached when every env-var step returns `None`,
/// and only when stdout is a TTY.
///
/// Note: the OSC query must run BEFORE the terminal
/// enters raw mode (otherwise the answer would
/// interfere with the TUI's own input handling). The
/// call site in `run_tui_to_stdout` honors this: the
/// detection happens before
/// `crossterm::terminal::enable_raw_mode`. The query
/// also adds ~5-300ms of startup latency depending on
/// the terminal's reply speed; we accept this as a
/// one-time cost because (a) the user only pays it
/// once per TUI launch, (b) the OSC query is the only
/// way to know the actual scheme on a bare
/// `xterm-256color` shell, and (c) the status-bar
/// display of the active scheme is worth the wait.
pub fn detect_color_scheme() -> ColorScheme {
    // Step 1: COLORFGBG.
    if let Some(c) = std::env::var("COLORFGBG").ok().and_then(|s| {
        // The format is "fg;bg" with optional ";"
        // followed by a terminal-default indicator
        // we ignore. Split on ';' and take the LAST
        // numeric token as the bg (some terminals put
        // extra metadata after).
        let bg = s
            .split(';').rfind(|t| !t.is_empty())
            .and_then(|t| t.parse::<u8>().ok());
        bg.map(|b| {
            // Standard 16-color palette: indices
            // 0-7 are the dark / standard half,
            // 8-15 are the bright half. Most
            // terminals on a light background use a
            // bright (>=7) bg. We pick 7 as the
            // threshold (the white-on-black case
            // uses index 0, the black-on-white case
            // uses index 7 or 15).
            if b >= 7 {
                ColorScheme::Light
            } else {
                ColorScheme::Dark
            }
        })
    }) {
        return c;
    }
    // Step 2: $TERM_PROGRAM. We treat the well-known
    // values as informative defaults — iTerm2 is dark
    // by default, Apple_Terminal is light by default,
    // WezTerm is dark by default. Users who run these
    // with non-default colors will end up with the
    // wrong scheme unless they set `theme.light=...` /
    // `theme.dark=...` explicitly. That's an
    // acceptable trade-off — the env-var signal is
    // already a hint, not a guarantee.
    if let Ok(p) = std::env::var("TERM_PROGRAM") {
        match p.as_str() {
            "iTerm.app" | "WezTerm" => return ColorScheme::Dark,
            "Apple_Terminal" => return ColorScheme::Light,
            _ => {}
        }
    }
    // Step 3: Windows Terminal.
    if std::env::var("WT_SESSION").is_ok() {
        return ColorScheme::Dark;
    }
    // Step 4: OSC 10 / 11 query via
    // `terminal-colorsaurus`. This is the cross-platform
    // standard: the terminal replies with the actual
    // RGB of the default background, and the crate
    // converts that to a `Light` / `Dark` classification
    // using a perceived-lightness threshold.
    //
    // Skip if stdout isn't a TTY. Two reasons:
    //
    // 1. The OSC escape sequence (`\x1b]11;?\x07`)
    //    would be written to the pipe, corrupting
    //    whatever downstream consumer is reading
    //    (a pager, a redirect to a file, etc.). The
    //    env-var fallbacks already answer the
    //    detection question for the rare case where
    //    the user pipes the TUI binary — the TUI
    //    itself wouldn't render in that case anyway.
    // 2. The reply comes back over the TTY's input
    //    side. If stdout is a pipe, the input side
    //    is whatever the parent shell provides, and
    //    reading a "reply" from it would deadlock
    //    (or, if the parent is non-interactive,
    //    return EOF immediately).
    //
    // We also wrap the call in a defensive
    // `std::panic::catch_unwind` so a buggy terminal
    // that emits a malformed OSC reply (which has
    // been known to crash naive parsers) doesn't
    // take the whole TUI down before it even starts.
    // A panic is treated like any other failure: we
    // fall through to `Unknown` and the user can
    // pick a theme from inside the TUI.
    if std::io::stdout().is_terminal() {
        // The `use` is at the function scope (not
        // inside the `catch_unwind` closure) so
        // `ThemeMode` is visible to the
        // pattern-matches below the call. The
        // closure captures by value, so there's
        // no lifetime concern.
        use terminal_colorsaurus::{
            theme_mode, QueryOptions, ThemeMode,
        };
        let result = std::panic::catch_unwind(|| {
            // 300ms is enough for any reasonable
            // terminal (most reply in <50ms) and short
            // enough that the user doesn't notice the
            // extra startup latency. SSH connections
            // over high-latency links might need more,
            // but those users are already paying
            // hundreds of ms of startup cost; the
            // 300ms cap is a UX budget, not a
            // correctness limit.
            //
            // `QueryOptions` is `#[non_exhaustive]`
            // upstream (so future versions can add
            // new options without a SemVer break), so
            // we MUST use the `Default` impl and patch
            // the one field we care about. We can't
            // construct it with a struct literal.
            let mut options = QueryOptions::default();
            options.timeout = std::time::Duration::from_millis(300);
            theme_mode(options).ok()
        });
        if let Ok(Some(ThemeMode::Light)) = result {
            return ColorScheme::Light;
        }
        if let Ok(Some(ThemeMode::Dark)) = result {
            return ColorScheme::Dark;
        }
        // `Ok(None)` (terminal-colorsaurus couldn't
        // classify) and `Err(_)` (timeout / parse
        // failure) both fall through to `Unknown`.
        // `Err` and panics are not fatal — the OSC
        // query is best-effort.
    }
    ColorScheme::Unknown
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

    // --- additional curated themes (user-requested) ---
    CatppuccinFrappe,
    CatppuccinMacchiato,
    DraculaSoft,
    EverforestLight,
    GithubDark,
    GithubDarkDefault,
    GithubDarkDimmed,
    GithubDarkHighContrast,
    GithubLight,
    GithubLightDefault,
    GithubLightHighContrast,
    GruvboxDarkHard,
    GruvboxDarkSoft,
    GruvboxLightHard,
    GruvboxLightSoft,
    AyuDark,
    AyuLight,
    AyuMirage,
    RosePineDawn,
    RosePineMoon,
    NightOwl,
    NightOwlLight,
    Synthwave84,
    MaterialTheme,
    MaterialThemeDarker,
    MaterialThemeLighter,
    MaterialThemeOcean,
    MaterialThemePalenight,
    VitesseBlack,
    VitesseDark,
    VitesseLight,
    Monokai,
    OneLight,
    DarkPlus,
    LightPlus,
    Horizon,
    HorizonBright,
    Laserwave,
    Houston,
    Andromeeda,
    AuroraX,
    KanagawaDragon,
    KanagawaLotus,
    Plastic,
    Poimandres,
    Red,
    SlackDark,
    SlackOchin,
    SnazzyLight,
    Vesper,
    MinDark,
    MinLight,
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
            BuiltinTheme::Andromeeda,
            BuiltinTheme::AuroraX,
            BuiltinTheme::AyuDark,
            BuiltinTheme::AyuLight,
            BuiltinTheme::AyuMirage,
            BuiltinTheme::CatppuccinFrappe,
            BuiltinTheme::CatppuccinMacchiato,
            BuiltinTheme::DarkPlus,
            BuiltinTheme::DoomOne,
            BuiltinTheme::DoomSolarizedLight,
            BuiltinTheme::DraculaSoft,
            BuiltinTheme::EverforestLight,
            BuiltinTheme::GithubDark,
            BuiltinTheme::GithubDarkDefault,
            BuiltinTheme::GithubDarkDimmed,
            BuiltinTheme::GithubDarkHighContrast,
            BuiltinTheme::GithubLight,
            BuiltinTheme::GithubLightDefault,
            BuiltinTheme::GithubLightHighContrast,
            BuiltinTheme::GruvboxDarkHard,
            BuiltinTheme::GruvboxDarkSoft,
            BuiltinTheme::GruvboxLightHard,
            BuiltinTheme::GruvboxLightSoft,
            BuiltinTheme::Horizon,
            BuiltinTheme::HorizonBright,
            BuiltinTheme::Houston,
            BuiltinTheme::KanagawaDragon,
            BuiltinTheme::KanagawaLotus,
            BuiltinTheme::Laserwave,
            BuiltinTheme::Leuven,
            BuiltinTheme::LightPlus,
            BuiltinTheme::MaterialDark,
            BuiltinTheme::MaterialLight,
            BuiltinTheme::MaterialTheme,
            BuiltinTheme::MaterialThemeDarker,
            BuiltinTheme::MaterialThemeLighter,
            BuiltinTheme::MaterialThemeOcean,
            BuiltinTheme::MaterialThemePalenight,
            BuiltinTheme::MinDark,
            BuiltinTheme::MinLight,
            BuiltinTheme::Monokai,
            BuiltinTheme::NightOwl,
            BuiltinTheme::NightOwlLight,
            BuiltinTheme::OneLight,
            BuiltinTheme::Plain,
            BuiltinTheme::Plastic,
            BuiltinTheme::Poimandres,
            BuiltinTheme::Red,
            BuiltinTheme::RosePineDawn,
            BuiltinTheme::RosePineMoon,
            BuiltinTheme::SlackDark,
            BuiltinTheme::SlackOchin,
            BuiltinTheme::SnazzyLight,
            BuiltinTheme::Synthwave84,
            BuiltinTheme::Vesper,
            BuiltinTheme::VitesseBlack,
            BuiltinTheme::VitesseDark,
            BuiltinTheme::VitesseLight,
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
            | BuiltinTheme::MaterialLight
            | BuiltinTheme::CatppuccinFrappe
            | BuiltinTheme::CatppuccinMacchiato
            | BuiltinTheme::DraculaSoft
            | BuiltinTheme::EverforestLight
            | BuiltinTheme::GithubDark
            | BuiltinTheme::GithubDarkDefault
            | BuiltinTheme::GithubDarkDimmed
            | BuiltinTheme::GithubDarkHighContrast
            | BuiltinTheme::GithubLight
            | BuiltinTheme::GithubLightDefault
            | BuiltinTheme::GithubLightHighContrast
            | BuiltinTheme::GruvboxDarkHard
            | BuiltinTheme::GruvboxDarkSoft
            | BuiltinTheme::GruvboxLightHard
            | BuiltinTheme::GruvboxLightSoft
            | BuiltinTheme::AyuDark
            | BuiltinTheme::AyuLight
            | BuiltinTheme::AyuMirage
            | BuiltinTheme::RosePineDawn
            | BuiltinTheme::RosePineMoon
            | BuiltinTheme::NightOwl
            | BuiltinTheme::NightOwlLight
            | BuiltinTheme::Synthwave84
            | BuiltinTheme::MaterialTheme
            | BuiltinTheme::MaterialThemeDarker
            | BuiltinTheme::MaterialThemeLighter
            | BuiltinTheme::MaterialThemeOcean
            | BuiltinTheme::MaterialThemePalenight
            | BuiltinTheme::VitesseBlack
            | BuiltinTheme::VitesseDark
            | BuiltinTheme::VitesseLight
            | BuiltinTheme::Monokai
            | BuiltinTheme::OneLight
            | BuiltinTheme::DarkPlus
            | BuiltinTheme::LightPlus
            | BuiltinTheme::Horizon
            | BuiltinTheme::HorizonBright
            | BuiltinTheme::Laserwave
            | BuiltinTheme::Houston
            | BuiltinTheme::Andromeeda
            | BuiltinTheme::AuroraX
            | BuiltinTheme::KanagawaDragon
            | BuiltinTheme::KanagawaLotus
            | BuiltinTheme::Plastic
            | BuiltinTheme::Poimandres
            | BuiltinTheme::Red
            | BuiltinTheme::SlackDark
            | BuiltinTheme::SlackOchin
            | BuiltinTheme::SnazzyLight
            | BuiltinTheme::Vesper
            | BuiltinTheme::MinDark
            | BuiltinTheme::MinLight => None,
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
            BuiltinTheme::CatppuccinFrappe => "catppuccin-frappe",
            BuiltinTheme::CatppuccinMacchiato => "catppuccin-macchiato",
            BuiltinTheme::DraculaSoft => "dracula-soft",
            BuiltinTheme::EverforestLight => "everforest-light",
            BuiltinTheme::GithubDark => "github-dark",
            BuiltinTheme::GithubDarkDefault => "github-dark-default",
            BuiltinTheme::GithubDarkDimmed => "github-dark-dimmed",
            BuiltinTheme::GithubDarkHighContrast => "github-dark-high-contrast",
            BuiltinTheme::GithubLight => "github-light",
            BuiltinTheme::GithubLightDefault => "github-light-default",
            BuiltinTheme::GithubLightHighContrast => "github-light-high-contrast",
            BuiltinTheme::GruvboxDarkHard => "gruvbox-dark-hard",
            BuiltinTheme::GruvboxDarkSoft => "gruvbox-dark-soft",
            BuiltinTheme::GruvboxLightHard => "gruvbox-light-hard",
            BuiltinTheme::GruvboxLightSoft => "gruvbox-light-soft",
            BuiltinTheme::AyuDark => "ayu-dark",
            BuiltinTheme::AyuLight => "ayu-light",
            BuiltinTheme::AyuMirage => "ayu-mirage",
            BuiltinTheme::RosePineDawn => "rose-pine-dawn",
            BuiltinTheme::RosePineMoon => "rose-pine-moon",
            BuiltinTheme::NightOwl => "night-owl",
            BuiltinTheme::NightOwlLight => "night-owl-light",
            BuiltinTheme::Synthwave84 => "synthwave-84",
            BuiltinTheme::MaterialTheme => "material-theme",
            BuiltinTheme::MaterialThemeDarker => "material-theme-darker",
            BuiltinTheme::MaterialThemeLighter => "material-theme-lighter",
            BuiltinTheme::MaterialThemeOcean => "material-theme-ocean",
            BuiltinTheme::MaterialThemePalenight => "material-theme-palenight",
            BuiltinTheme::VitesseBlack => "vitesse-black",
            BuiltinTheme::VitesseDark => "vitesse-dark",
            BuiltinTheme::VitesseLight => "vitesse-light",
            BuiltinTheme::Monokai => "monokai",
            BuiltinTheme::OneLight => "one-light",
            BuiltinTheme::DarkPlus => "dark-plus",
            BuiltinTheme::LightPlus => "light-plus",
            BuiltinTheme::Horizon => "horizon",
            BuiltinTheme::HorizonBright => "horizon-bright",
            BuiltinTheme::Laserwave => "laserwave",
            BuiltinTheme::Houston => "houston",
            BuiltinTheme::Andromeeda => "andromeeda",
            BuiltinTheme::AuroraX => "aurora-x",
            BuiltinTheme::KanagawaDragon => "kanagawa-dragon",
            BuiltinTheme::KanagawaLotus => "kanagawa-lotus",
            BuiltinTheme::Plastic => "plastic",
            BuiltinTheme::Poimandres => "poimandres",
            BuiltinTheme::Red => "red",
            BuiltinTheme::SlackDark => "slack-dark",
            BuiltinTheme::SlackOchin => "slack-ochin",
            BuiltinTheme::SnazzyLight => "snazzy-light",
            BuiltinTheme::Vesper => "vesper",
            BuiltinTheme::MinDark => "min-dark",
            BuiltinTheme::MinLight => "min-light",
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
            BuiltinTheme::CatppuccinFrappe => "Catppuccin Frappe",
            BuiltinTheme::CatppuccinMacchiato => "Catppuccin Macchiato",
            BuiltinTheme::DraculaSoft => "Dracula Soft",
            BuiltinTheme::EverforestLight => "Everforest Light",
            BuiltinTheme::GithubDark => "GitHub Dark",
            BuiltinTheme::GithubDarkDefault => "GitHub Dark Default",
            BuiltinTheme::GithubDarkDimmed => "GitHub Dark Dimmed",
            BuiltinTheme::GithubDarkHighContrast => "GitHub Dark High Contrast",
            BuiltinTheme::GithubLight => "GitHub Light",
            BuiltinTheme::GithubLightDefault => "GitHub Light Default",
            BuiltinTheme::GithubLightHighContrast => "GitHub Light High Contrast",
            BuiltinTheme::GruvboxDarkHard => "Gruvbox Dark Hard",
            BuiltinTheme::GruvboxDarkSoft => "Gruvbox Dark Soft",
            BuiltinTheme::GruvboxLightHard => "Gruvbox Light Hard",
            BuiltinTheme::GruvboxLightSoft => "Gruvbox Light Soft",
            BuiltinTheme::AyuDark => "Ayu Dark",
            BuiltinTheme::AyuLight => "Ayu Light",
            BuiltinTheme::AyuMirage => "Ayu Mirage",
            BuiltinTheme::RosePineDawn => "Rosé Pine Dawn",
            BuiltinTheme::RosePineMoon => "Rosé Pine Moon",
            BuiltinTheme::NightOwl => "Night Owl",
            BuiltinTheme::NightOwlLight => "Night Owl Light",
            BuiltinTheme::Synthwave84 => "Synthwave 84",
            BuiltinTheme::MaterialTheme => "Material Theme",
            BuiltinTheme::MaterialThemeDarker => "Material Theme Darker",
            BuiltinTheme::MaterialThemeLighter => "Material Theme Lighter",
            BuiltinTheme::MaterialThemeOcean => "Material Theme Ocean",
            BuiltinTheme::MaterialThemePalenight => "Material Theme Palenight",
            BuiltinTheme::VitesseBlack => "Vitesse Black",
            BuiltinTheme::VitesseDark => "Vitesse Dark",
            BuiltinTheme::VitesseLight => "Vitesse Light",
            BuiltinTheme::Monokai => "Monokai",
            BuiltinTheme::OneLight => "One Light",
            BuiltinTheme::DarkPlus => "Dark Plus",
            BuiltinTheme::LightPlus => "Light Plus",
            BuiltinTheme::Horizon => "Horizon",
            BuiltinTheme::HorizonBright => "Horizon Bright",
            BuiltinTheme::Laserwave => "Laserwave",
            BuiltinTheme::Houston => "Houston",
            BuiltinTheme::Andromeeda => "Andromeeda",
            BuiltinTheme::AuroraX => "Aurora X",
            BuiltinTheme::KanagawaDragon => "Kanagawa Dragon",
            BuiltinTheme::KanagawaLotus => "Kanagawa Lotus",
            BuiltinTheme::Plastic => "Plastic",
            BuiltinTheme::Poimandres => "Poimandres",
            BuiltinTheme::Red => "Red",
            BuiltinTheme::SlackDark => "Slack Dark",
            BuiltinTheme::SlackOchin => "Slack Ochin",
            BuiltinTheme::SnazzyLight => "Snazzy Light",
            BuiltinTheme::Vesper => "Vesper",
            BuiltinTheme::MinDark => "Min Dark",
            BuiltinTheme::MinLight => "Min Light",
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

    /// The full ordered list: `None` first, then every entry in
    /// `BuiltinTheme::all()` in canonical order.
    #[allow(dead_code)] // convention API; the theme picker rolls its own filter
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

/// Determine if a `Color` is "light" using the ITU-R BT.601
/// perceived-brightness formula (the same one
/// `ratatui_themes::ThemePalette::is_light()` uses). This is
/// used by the `bat` color-highlighting paths to pick
/// `--theme=light` (for light themes) or `--theme=dark`
/// (for dark themes) so syntax-highlighted source previews
/// match the active theme's background.
fn is_color_light(color: Color) -> bool {
    if let Color::Rgb(r, g, b) = color {
        let brightness = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
        brightness > 127
    } else {
        matches!(
            color,
            Color::White | Color::LightRed | Color::LightGreen | Color::LightYellow
                | Color::LightBlue | Color::LightMagenta | Color::LightCyan | Color::Gray
        )
    }
}

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
    /// Whether the active theme is a light theme (for
    /// `bat --theme=light>` / `bat --theme=dark`
    /// selection in the color-highlighting paths).
    /// Computed from the theme's `bg` color brightness
    /// via the ITU-R BT.601 perceived-brightness
    /// formula, matching the `ratatui-themes`
    /// `ThemePalette::is_light()` method. Light themes
    /// (Leuven, Catppuccin Latte, GitHub Light, etc.)
    /// set this to `true`; dark themes set `false`.
    pub(crate) is_light_theme: bool,
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
            is_light_theme: false,
        }
    }

    /// Construct the resolved palette for the manually-configured
    /// "no theme" case. All fallbacks come from the user's own
    /// `tuicolor.*` settings (so the manual-config defaults are
    /// self-consistent even without any user override).
    fn from_manual(theme: &crate::TuiTheme, _cfg: &Config) -> Self {
        let fallback = Palette::builtin();
        Palette {
            bg: if theme.bg.is_empty() { fallback.bg } else { resolve_color(&theme.bg) },
            fg: if theme.fg.is_empty() { fallback.fg } else { resolve_color(&theme.fg) },
            accent: if theme.accent.is_empty() { fallback.accent } else { resolve_color(&theme.accent) },
            success: if theme.success.is_empty() { fallback.success } else { resolve_color(&theme.success) },
            error: if theme.error.is_empty() { fallback.error } else { resolve_color(&theme.error) },
            warning: if theme.warning.is_empty() { fallback.warning } else { resolve_color(&theme.warning) },
            dim: if theme.dim.is_empty() { fallback.dim } else { resolve_color(&theme.dim) },
            dimmer: Color::DarkGray,
            highlight: if theme.highlight.is_empty() { fallback.highlight } else { resolve_color(&theme.highlight) },
            info: if theme.info.is_empty() { fallback.info } else { resolve_color(&theme.info) },
            selection: if theme.selection.is_empty() { fallback.selection } else { resolve_color(&theme.selection) },
            badge_fg: if theme.badge_fg.is_empty() { fallback.badge_fg } else { resolve_color(&theme.badge_fg) },
            list_bg: if theme.list_bg.is_empty() { fallback.list_bg } else { resolve_color(&theme.list_bg) },
            details_bg: if theme.details_bg.is_empty() { fallback.details_bg } else { resolve_color(&theme.details_bg) },
            input_bg: if theme.input_bg.is_empty() { fallback.input_bg } else { resolve_color(&theme.input_bg) },
            status_bg: if theme.status_bg.is_empty() { fallback.status_bg } else { resolve_color(&theme.status_bg) },
            is_light_theme: is_color_light(
                if theme.bg.is_empty() { fallback.bg } else { resolve_color(&theme.bg) }
            ),
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
                bg: if cfg_theme.bg.is_empty() {
                    p.bg
                } else {
                    resolve_color(&cfg_theme.bg)
                },
                fg: if cfg_theme.fg.is_empty() {
                    p.fg
                } else {
                    resolve_color(&cfg_theme.fg)
                },
                accent: if cfg_theme.accent.is_empty() {
                    p.accent
                } else {
                    resolve_color(&cfg_theme.accent)
                },
                success: if cfg_theme.success.is_empty() {
                    p.success
                } else {
                    resolve_color(&cfg_theme.success)
                },
                error: if cfg_theme.error.is_empty() {
                    p.error
                } else {
                    resolve_color(&cfg_theme.error)
                },
                warning: if cfg_theme.warning.is_empty() {
                    p.warning
                } else {
                    resolve_color(&cfg_theme.warning)
                },
                dim: if cfg_theme.dim.is_empty() {
                    p.muted
                } else {
                    resolve_color(&cfg_theme.dim)
                },
                dimmer: Color::DarkGray,
                highlight: if cfg_theme.highlight.is_empty() {
                    // No `tuicolor.highlight=` override.
                    // Use the theme's own `accent` (there's
                    // no separate `highlight` slot in
                    // `ThemePalette`) so highlighted matches
                    // and the selected-row markers pick up
                    // the theme's primary accent.
                    p.accent
                } else {
                    resolve_color(&cfg_theme.highlight)
                },
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
                // Use `ThemePalette::is_light()` for the built-in
                // theme (the crate supplies the classification).
                // When the user overrides `bg` via `tuicolor.bg=`,
                // recompute from the resolved `bg` Color
                // instead.
                is_light_theme: if cfg_theme.bg.is_empty() {
                    p.is_light()
                } else {
                    is_color_light(resolve_color(&cfg_theme.bg))
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

#[cfg(test)]
mod scheme_tests {
    use super::{ColorScheme, detect_color_scheme};
    use std::sync::Mutex;

    /// Process-wide mutex that serialises every
    /// `detect_color_scheme()` test that mutates
    /// env vars. The function reads several
    /// process-level env vars (`$COLORFGBG`,
    /// `$TERM_PROGRAM`, `$WT_SESSION`) and the OSC
    /// step (in real terminals) reads from the TTY;
    /// the parallel test runner would otherwise let
    /// two tests stomp on each other's env.
    /// `std::sync::Mutex` keeps the test dependency
    /// footprint at zero.
    static DETECTION_LOCK: Mutex<()> = Mutex::new(());

    /// `ColorScheme::other()` returns the opposite
    /// scheme. Used by the theme picker to compute
    /// "the OTHER slot's value" when displaying the
    /// active scheme's slot — so the picker can show
    /// "you're about to change the LIGHT theme; the
    /// DARK theme is currently gruvbox-light".
    #[test]
    fn other_returns_dark_for_light_and_vice_versa() {
        assert_eq!(ColorScheme::Light.other(), ColorScheme::Dark);
        assert_eq!(ColorScheme::Dark.other(), ColorScheme::Light);
    }

    /// `Unknown.other()` is itself — there is no
    /// "other" scheme when we couldn't detect either
    /// one. The caller falls back to `Dark` upstream.
    #[test]
    fn unknown_other_is_unknown() {
        assert_eq!(ColorScheme::Unknown.other(), ColorScheme::Unknown);
    }

    /// `ColorScheme::label()` returns the lowercased
    /// ASCII label used in config-file keys
    /// (`theme.light` / `theme.dark`), status
    /// messages, and the theme picker. This is the
    /// single source of truth for the wire format.
    #[test]
    fn label_matches_config_key() {
        assert_eq!(ColorScheme::Light.label(), "light");
        assert_eq!(ColorScheme::Dark.label(), "dark");
        assert_eq!(ColorScheme::Unknown.label(), "unknown");
    }

    /// The default scheme is `Dark` (the historical
    /// smarthistory look, the most common modern
    /// terminal default). The test pins the default so
    /// a future refactor doesn't accidentally flip it
    /// to `Light` (which would surprise every dark-
    /// terminal user on first run).
    #[test]
    fn default_is_dark() {
        assert_eq!(ColorScheme::default(), ColorScheme::Dark);
    }

    /// `detect_color_scheme()` returns `Dark` when
    /// `$COLORFGBG` has a small bg index (standard
    /// 16-color palette: indices 0-6 = dark half,
    /// 7+ = bright half). This is the historical
    /// dark-terminal case (`COLORFGBG="15;0"` =
    /// white-on-black). The test is hermetic — it
    /// sets `$COLORFGBG` to a known value and
    /// expects the documented behaviour, regardless
    /// of the OSC step's outcome (the env-var step
    /// runs first and short-circuits before the OSC
    /// call, so we don't need a real terminal).
    #[test]
    fn colorfgbg_dark_index_returns_dark() {
        // Serialise with the other env-mutating
        // tests in this module (see DETECTION_LOCK
        // doc comment). The lock is released on
        // drop.
        let _guard = DETECTION_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The env var is process-wide, so we set it
        // for the duration of the test.
        // SAFETY: process-level env mutation,
        // serialised by DETECTION_LOCK above.
        let prev = std::env::var("COLORFGBG").ok();
        unsafe {
            std::env::set_var("COLORFGBG", "15;0");
        }
        let scheme = detect_color_scheme();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("COLORFGBG", v),
                None => std::env::remove_var("COLORFGBG"),
            }
        }
        assert_eq!(
            scheme,
            ColorScheme::Dark,
            "expected Dark for `COLORFGBG=15;0`"
        );
    }

    /// The bright-half heuristic: a `COLORFGBG`
    /// value with bg index >= 7 (the bright half
    /// of the standard 16-color palette) returns
    /// `Light`. The black-on-white case is
    /// `COLORFGBG="0;15"`; the white-on-white case
    /// is `COLORFGBG="15;15"`. Both should classify
    /// as `Light`.
    #[test]
    fn colorfgbg_bright_index_returns_light() {
        let _guard = DETECTION_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("COLORFGBG").ok();
        unsafe {
            std::env::set_var("COLORFGBG", "0;15");
        }
        let scheme = detect_color_scheme();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("COLORFGBG", v),
                None => std::env::remove_var("COLORFGBG"),
            }
        }
        assert_eq!(
            scheme,
            ColorScheme::Light,
            "expected Light for `COLORFGBG=0;15`"
        );
    }

    /// `COLORFGBG="default;default"` is what newer
    /// terminals write when they can't be
    /// classified. We treat the non-numeric
    /// tokens as `Unknown` (the env-var step
    /// returns `None` and the function falls
    /// through to the OSC step). In a non-TTY test
    /// environment the OSC step is skipped, so the
    /// overall result is `Unknown`. This test
    /// pins that behaviour so a future refactor
    /// doesn't accidentally classify "default" as
    /// a numeric index.
    #[test]
    fn colorfgbg_default_default_returns_unknown_or_dark() {
        let _guard = DETECTION_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("COLORFGBG").ok();
        unsafe {
            std::env::set_var("COLORFGBG", "default;default");
        }
        let scheme = detect_color_scheme();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("COLORFGBG", v),
                None => std::env::remove_var("COLORFGBG"),
            }
        }
        // The env-var step returns `None` (the
        // "default" token doesn't parse as a u8),
        // the OSC step is skipped in non-TTY test
        // environments, so the result is
        // `Unknown`. In a real TTY the OSC step
        // would answer the question, but we
        // can't test that path here.
        assert_eq!(
            scheme,
            ColorScheme::Unknown,
            "expected Unknown for `COLORFGBG=default;default` in a non-TTY env"
        );
    }
}
