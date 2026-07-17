// Theme subsystem: registry of 73 built-in palettes plus the
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
