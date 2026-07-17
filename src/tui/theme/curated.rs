//! Curated themes — the 6 hand-written palettes that ship with
//! the binary, distinct from the 15 upstream themes that come
//! from the `ratatui-themes` crate.
//!
//! Each theme is defined as a TOML file in `curated/`. We embed
//! the file contents at compile time with `include_str!` so
//! the parser runs once at startup, the binary is self-contained
//! (no runtime file lookup), and a missing or malformed file
//! is a compile-time error rather than a runtime surprise.
//!
//! The TOML format is minimal — three top-level scalars
//! (`name`, `display_name`) and a `[colors]` table with the 10
//! color slots the rest of the theme system consumes:
//!
//! ```toml
//! name         = "doom-one"
//! display_name = "Doom One"
//!
//! [colors]
//! accent    = "#73bfff"
//! secondary = "#ff79c6"
//! # ... 8 more ...
//! ```
//!
//! Colors are written as `#rrggbb` hex. Named CSS colors and
//! the 16-color terminal palette are deliberately not supported
//! here — that would need a color-name table and the curated
//! themes are all hex by convention.
//!
//! ## Adding a new theme
//!
//! 1. Drop a new `.toml` file in `curated/`.
//! 2. Add the variant to `BuiltinTheme` in `mod.rs`.
//! 3. Add the variant to the `upstream()` / `as_upstream()` /
//!    `slug()` / `display_name()` matches (or, if it's a
//!    curated theme, leave the upstream arm as `None`).
//! 4. Add the file to the `CURATED_FILES` list below.
//! 5. Add a dispatch arm in `palette_for()`.

use ratatui::style::Color;

/// A parsed curated theme, exactly as it appears in the TOML.
#[derive(Debug, Clone, Copy)]
pub struct CuratedTheme {
    /// Slug used in the session file and config key. Lowercase,
    /// hyphen-separated. Must match `BuiltinTheme::slug()`.
    pub name: &'static str,
    /// Human-readable label shown in the theme picker.
    /// Currently the `BuiltinTheme::display_name()` lookup
    /// is the source of truth at runtime; this field is
    /// preserved so the parsed file is self-describing and
    /// could become the source of truth in a future refactor.
    #[allow(dead_code)]
    pub display_name: &'static str,
    /// 10-color palette.
    pub colors: CuratedColors,
}

/// The 10 color slots every curated theme must define. Field
/// order is the same as the `[colors]` table in the TOML file
/// and matches `ratatui_themes::ThemePalette` so the parsed
/// result can be passed straight through.
#[derive(Debug, Clone, Copy)]
pub struct CuratedColors {
    pub accent: Color,
    pub secondary: Color,
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub selection: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    pub info: Color,
}

/// Embedded TOML sources, one per curated theme. Adding a new
/// theme means dropping a file in `curated/` and adding the
/// `include_str!` line here so it gets compiled into the binary.
const CURATED_FILES: &[(&str, &str)] = &[
    ("andromeeda", include_str!("curated/andromeeda.toml")),
    ("aurora-x", include_str!("curated/aurora-x.toml")),
    ("ayu-dark", include_str!("curated/ayu-dark.toml")),
    ("ayu-light", include_str!("curated/ayu-light.toml")),
    ("ayu-mirage", include_str!("curated/ayu-mirage.toml")),
    ("catppuccin-frappe", include_str!("curated/catppuccin-frappe.toml")),
    ("catppuccin-macchiato", include_str!("curated/catppuccin-macchiato.toml")),
    ("dark-plus", include_str!("curated/dark-plus.toml")),
    ("doom-one", include_str!("curated/doom-one.toml")),
    ("doom-solarized-light", include_str!("curated/doom-solarized-light.toml")),
    ("dracula-soft", include_str!("curated/dracula-soft.toml")),
    ("everforest-light", include_str!("curated/everforest-light.toml")),
    ("github-dark", include_str!("curated/github-dark.toml")),
    ("github-dark-default", include_str!("curated/github-dark-default.toml")),
    ("github-dark-dimmed", include_str!("curated/github-dark-dimmed.toml")),
    ("github-dark-high-contrast", include_str!("curated/github-dark-high-contrast.toml")),
    ("github-light", include_str!("curated/github-light.toml")),
    ("github-light-default", include_str!("curated/github-light-default.toml")),
    ("github-light-high-contrast", include_str!("curated/github-light-high-contrast.toml")),
    ("gruvbox-dark-hard", include_str!("curated/gruvbox-dark-hard.toml")),
    ("gruvbox-dark-soft", include_str!("curated/gruvbox-dark-soft.toml")),
    ("gruvbox-light-hard", include_str!("curated/gruvbox-light-hard.toml")),
    ("gruvbox-light-soft", include_str!("curated/gruvbox-light-soft.toml")),
    ("horizon", include_str!("curated/horizon.toml")),
    ("horizon-bright", include_str!("curated/horizon-bright.toml")),
    ("houston", include_str!("curated/houston.toml")),
    ("kanagawa-dragon", include_str!("curated/kanagawa-dragon.toml")),
    ("kanagawa-lotus", include_str!("curated/kanagawa-lotus.toml")),
    ("laserwave", include_str!("curated/laserwave.toml")),
    ("leuven", include_str!("curated/leuven.toml")),
    ("light-plus", include_str!("curated/light-plus.toml")),
    ("material-dark", include_str!("curated/material-dark.toml")),
    ("material-light", include_str!("curated/material-light.toml")),
    ("material-theme", include_str!("curated/material-theme.toml")),
    ("material-theme-darker", include_str!("curated/material-theme-darker.toml")),
    ("material-theme-lighter", include_str!("curated/material-theme-lighter.toml")),
    ("material-theme-ocean", include_str!("curated/material-theme-ocean.toml")),
    ("material-theme-palenight", include_str!("curated/material-theme-palenight.toml")),
    ("min-dark", include_str!("curated/min-dark.toml")),
    ("min-light", include_str!("curated/min-light.toml")),
    ("monokai", include_str!("curated/monokai.toml")),
    ("night-owl", include_str!("curated/night-owl.toml")),
    ("night-owl-light", include_str!("curated/night-owl-light.toml")),
    ("one-light", include_str!("curated/one-light.toml")),
    ("plain", include_str!("curated/plain.toml")),
    ("plastic", include_str!("curated/plastic.toml")),
    ("poimandres", include_str!("curated/poimandres.toml")),
    ("red", include_str!("curated/red.toml")),
    ("rose-pine-dawn", include_str!("curated/rose-pine-dawn.toml")),
    ("rose-pine-moon", include_str!("curated/rose-pine-moon.toml")),
    ("slack-dark", include_str!("curated/slack-dark.toml")),
    ("slack-ochin", include_str!("curated/slack-ochin.toml")),
    ("snazzy-light", include_str!("curated/snazzy-light.toml")),
    ("synthwave-84", include_str!("curated/synthwave-84.toml")),
    ("vesper", include_str!("curated/vesper.toml")),
    ("vitesse-black", include_str!("curated/vitesse-black.toml")),
    ("vitesse-dark", include_str!("curated/vitesse-dark.toml")),
    ("vitesse-light", include_str!("curated/vitesse-light.toml")),
];



/// All parsed curated themes, keyed by slug. Built once at
/// startup via `once_cell`-style static initialization. The
/// iterator order matches the order in `CURATED_FILES` so
/// `BuiltinTheme::all()` can rely on it.
pub fn all() -> &'static [CuratedTheme] {
    static CACHE: std::sync::OnceLock<Vec<CuratedTheme>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        CURATED_FILES
            .iter()
            .map(|(slug, src)| {
                parse(src).unwrap_or_else(|e| {
                    // A malformed curated file is a build-time
                    // bug, not a user error. We panic during
                    // startup rather than silently fall back
                    // to a wrong palette, which would mislead
                    // the user about which theme is active.
                    panic!("curated theme {:?} failed to parse: {}", slug, e);
                })
            })
            .collect()
    })
}

/// Look up a curated palette by slug. Returns `None` for
/// upstream themes — the caller should fall back to
/// `ratatui_themes::ThemeName::palette()` in that case.
pub fn palette_for(slug: &str) -> Option<CuratedColors> {
    all().iter().find(|t| t.name == slug).map(|t| t.colors)
}

// ---------------------------------------------------------------------------
// Hand-rolled TOML parser
// ---------------------------------------------------------------------------
//
// We only need to handle the format our own files use:
//
//   - Top-level scalars:   name = "..."   display_name = "..."
//   - A single [colors] section with hex-color values
//   - Comments starting with `#`
//   - Blank lines
//
// That's it. The parser is ~80 lines, easy to audit, and
// avoids pulling in the `toml` + `serde` ecosystem for what
// amounts to 11 fields per file.

/// Parse a curated-theme TOML document. The source must have
/// `'static` lifetime because the parsed `name` and
/// `display_name` borrow from it. In practice this is always
/// `include_str!("curated/<theme>.toml")`, so the lifetime
/// is a no-op.
fn parse(src: &'static str) -> Result<CuratedTheme, String> {
    let mut name: Option<&'static str> = None;
    let mut display_name: Option<&'static str> = None;

    let mut in_colors = false;
    let mut color_fields: [Option<Color>; 10] = [None; 10];
    // Field order matches the indices used below.
    const COLOR_KEYS: &[&str] = &[
        "accent",
        "secondary",
        "bg",
        "fg",
        "muted",
        "selection",
        "error",
        "warning",
        "success",
        "info",
    ];

    for (lineno, raw) in src.lines().enumerate() {
        let line = raw.trim();
        // Skip blanks and comments.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Section header.
        if let Some(header) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if header == "colors" {
                in_colors = true;
            } else {
                return Err(format!("line {}: unknown section [{}]", lineno + 1, header));
            }
            continue;
        }
        // key = value pair.
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "line {}: expected `key = value`, got {:?}",
                lineno + 1,
                line
            ));
        };
        let key = key.trim();
        let value = value.trim();
        if in_colors {
            let idx = COLOR_KEYS
                .iter()
                .position(|k| *k == key)
                .ok_or_else(|| format!("line {}: unknown color key {:?}", lineno + 1, key))?;
            // Strip surrounding quotes from the hex value
            // (`"#73bfff"`) before parsing.
            let raw = strip_quotes(value).unwrap_or(value);
            let color = parse_hex(raw)
                .ok_or_else(|| format!("line {}: invalid color {:?}", lineno + 1, value))?;
            color_fields[idx] = Some(color);
        } else {
            let value = strip_quotes(value).ok_or_else(|| {
                format!(
                    "line {}: expected string literal, got {:?}",
                    lineno + 1,
                    value
                )
            })?;
            match key {
                "name" => name = Some(value),
                "display_name" => display_name = Some(value),
                _ => return Err(format!("line {}: unknown key {:?}", lineno + 1, key)),
            }
        }
    }

    // Finalize: build the CuratedColors from the 10 slots, error
    // if any are missing. Each slot is itself an `Option<Color>`
    // (parsed or not), so we flatten with `.and_then(|c| c)`.
    let mut iter = color_fields.into_iter();
    let colors = CuratedColors {
        accent: require(iter.next().and_then(|c| c), "accent")?,
        secondary: require(iter.next().and_then(|c| c), "secondary")?,
        bg: require(iter.next().and_then(|c| c), "bg")?,
        fg: require(iter.next().and_then(|c| c), "fg")?,
        muted: require(iter.next().and_then(|c| c), "muted")?,
        selection: require(iter.next().and_then(|c| c), "selection")?,
        error: require(iter.next().and_then(|c| c), "error")?,
        warning: require(iter.next().and_then(|c| c), "warning")?,
        success: require(iter.next().and_then(|c| c), "success")?,
        info: require(iter.next().and_then(|c| c), "info")?,
    };

    Ok(CuratedTheme {
        name: name.ok_or_else(|| "missing `name` field".to_string())?,
        display_name: display_name.ok_or_else(|| "missing `display_name` field".to_string())?,
        colors,
    })
}

/// Turn an `Option<Color>` from the parser into a `Color`,
/// producing a clear error if the slot wasn't filled.
fn require(slot: Option<Color>, key: &str) -> Result<Color, String> {
    slot.ok_or_else(|| format!("missing color `{}` in [colors]", key))
}

/// Strip surrounding ASCII double quotes from a string literal.
/// Returns `None` if the value isn't wrapped in quotes.
fn strip_quotes(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

/// Parse a `#rrggbb` hex color. Returns `None` for anything else.
fn parse_hex(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doom_one_round_trip() {
        let t = parse(include_str!("curated/doom-one.toml")).expect("doom-one.toml parses");
        assert_eq!(t.name, "doom-one");
        assert_eq!(t.display_name, "Doom One");
        // #73bfff = (115, 191, 255)
        assert_eq!(t.colors.accent, Color::Rgb(115, 191, 255));
        // #282c34 = (40, 44, 52)
        assert_eq!(t.colors.bg, Color::Rgb(40, 44, 52));
    }

    #[test]
    fn all_themes_parse() {
        // Catches typos in any of the 6 files at `cargo test`
        // time rather than at first-run time.
        for t in all() {
            assert!(!t.name.is_empty());
            assert!(!t.display_name.is_empty());
        }
    }

    #[test]
    fn palette_for_finds_known_slug() {
        assert!(palette_for("doom-one").is_some());
        assert!(palette_for("plain").is_some());
    }

    #[test]
    fn palette_for_returns_none_for_unknown() {
        // Upstream slugs aren't curated.
        assert!(palette_for("dracula").is_none());
        assert!(palette_for("nonexistent").is_none());
    }

    #[test]
    fn parse_hex_rejects_bad_input() {
        assert!(parse_hex("fff").is_none()); // no `#`
        assert!(parse_hex("#ff").is_none()); // too short
        assert!(parse_hex("#zzzzzz").is_none()); // not hex
        assert!(parse_hex("rgb(0,0,0)").is_none()); // wrong format
        assert!(parse_hex("").is_none());
    }

    #[test]
    fn parse_hex_accepts_standard_form() {
        assert_eq!(parse_hex("#000000"), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(parse_hex("#ffffff"), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(parse_hex("#73bfff"), Some(Color::Rgb(115, 191, 255)));
    }

    #[test]
    fn parse_rejects_missing_color() {
        // Use `r##"..."##` (two hashes) so the `"#` inside the
        // hex color strings doesn't terminate the raw string.
        let bad = r##"
            name = "broken"
            display_name = "Broken"
            [colors]
            accent = "#000000"
        "##;
        assert!(parse(bad).is_err());
    }

    #[test]
    fn parse_rejects_missing_top_level_field() {
        let bad = r##"
            name = "broken"
            [colors]
            accent = "#000000"
        "##;
        assert!(parse(bad).is_err());
    }

    #[test]
    fn parse_rejects_unknown_section() {
        let bad = r##"
            name = "x"
            display_name = "X"
            [bogus]
            accent = "#000000"
        "##;
        assert!(parse(bad).is_err());
    }
}
