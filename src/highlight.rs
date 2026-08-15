//! Shared helpers for syntax highlighting and query tokenisation.
//!
//! `ag` and the TUI's symbols (`$`) mode both benefit from:
//! - `syntect`-based syntax highlighting (the same Rust engine `bat`
//!   itself is built on, run in-process — no external `bat` binary
//!   required).
//! - A common "split the query into search terms, globs, and
//!   `@lang` language flags" classifier.
//!
//! These used to live inline in `src/ag.rs`; extracting them here
//! keeps the ag module small and lets other modes (currently the
//! tags view, future content views) reuse the same plumbing
//! without copy-pasting the implementation.

use std::path::Path;

/// A simple classifier for a query body.
///
/// - `terms` are plain whitespace-separated search terms.
/// - `globs` are tokens containing `*` (shell-style file globs).
/// - `languages` are tokens with a leading `@` (e.g. `@rust`).
///
/// Used by both `ag` mode and the tags view: ag passes `globs` to
/// `ag -G` and `languages` to `ag --<lang>`; tags mode uses
/// `languages` to filter by file extension and as the `lang` token
/// for [`highlight_with_bat`]'s preview highlighting.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueryTokens {
    pub terms: Vec<String>,
    pub globs: Vec<String>,
    pub languages: Vec<String>,
}

/// Split a query body into terms / globs / `@lang` tokens.
///
/// The classifier mirrors the ag-mode behaviour:
/// - tokens containing `*` go to `globs`,
/// - tokens with a leading `@` go to `languages`,
/// - everything else goes to `terms`.
///
/// An empty `@lang` token (`@`) is silently dropped. Multiple
/// languages may be supplied; callers that only support one
/// (e.g. tags mode, which uses the first for extension filtering
/// and preview highlighting) should pick the first entry.
pub fn parse_query_tokens(pattern: &str) -> QueryTokens {
    let mut out = QueryTokens::default();
    for tok in pattern.split_whitespace() {
        if tok.is_empty() {
            continue;
        }
        if tok.contains('*') {
            out.globs.push(tok.to_string());
        } else if let Some(lang) = tok.strip_prefix('@') {
            if !lang.is_empty() {
                out.languages.push(lang.to_string());
            }
        } else {
            out.terms.push(tok.to_string());
        }
    }
    out
}

/// Whether the active TUI theme (light vs dark) — read from the
/// `PALETTE` thread-local (populated by `install_palette`) — should
/// resolve to `syntect`'s light or dark bundled theme variant, so
/// syntax colors contrast correctly with the TUI's background.
fn is_light_theme() -> bool {
    crate::tui::theme::palette_storage::PALETTE.with(|p| p.borrow().is_light_theme)
}

/// Resolve the `syntect` theme matching the active color scheme —
/// the same `base16-ocean.light`/`base16-ocean.dark` pair
/// [`highlight_bash_commands`] uses, so every highlighted surface in
/// this app (the TUI's history list AND its preview panes) reads as
/// one consistent palette.
fn resolve_theme(is_light: bool) -> &'static syntect::highlighting::Theme {
    let ts = theme_set();
    let theme_name = if is_light {
        "base16-ocean.light"
    } else {
        "base16-ocean.dark"
    };
    ts.themes
        .get(theme_name)
        .or_else(|| ts.themes.values().next())
        .expect("syntect::highlighting::ThemeSet::load_defaults() always bundles at least one theme")
}

/// Highlight `text` (which may be multi-line, unlike
/// [`highlight_bash_commands`]'s single-line commands) against
/// `syntax`/`theme` and return it as a single 24-bit-ANSI-escaped
/// string, ready for the same `parse_ansi_line`-based rendering
/// path every caller of `highlight_with_bat`/`highlight_with_bat_auto`
/// already uses. `syntect::util::as_24_bit_terminal_escaped` doesn't
/// itself emit a trailing reset code, so one (`\x1b[0m`) is appended
/// after each line to prevent color bleed into whatever the caller
/// concatenates or renders next to it.
fn highlight_as_ansi(
    text: &str,
    syntax: &syntect::parsing::SyntaxReference,
    theme: &syntect::highlighting::Theme,
) -> String {
    let ss = syntax_set();
    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
    let mut out = String::with_capacity(text.len() * 2);
    for line in syntect::util::LinesWithEndings::from(text) {
        match highlighter.highlight_line(line, ss) {
            Ok(ranges) => {
                out.push_str(&syntect::util::as_24_bit_terminal_escaped(&ranges, false));
                out.push_str("\x1b[0m");
            }
            // `highlight_line` only errors on malformed syntax
            // definitions, never on the input text itself — not
            // expected with the bundled default syntaxes, but fall
            // back to the unstyled line rather than dropping it.
            Err(_) => out.push_str(line),
        }
    }
    out
}

/// Syntax-highlight `context` for the given `lang` token (e.g. an
/// `@lang` search-token from [`parse_query_tokens`]) using
/// `syntect` — the same engine `bat` itself is built on, run
/// in-process rather than shelled out to. `lang` is matched against
/// `syntect`'s bundled syntax definitions by name/token first (its
/// own `bash`/`rust`/`python`/… identifiers), then by treating it as
/// a file extension (covers short forms like `py`/`rs` that happen
/// to equal the extension).
///
/// Returns `None` when `lang` doesn't match any known syntax (the
/// caller falls back to the unhighlighted text) — mirrors the
/// original `bat --language <lang>` behavior, which exits non-zero
/// for a language name it doesn't recognize, unlike the auto-detect
/// path ([`highlight_with_bat_auto`]) below, which always succeeds.
/// When `lang` is empty the caller should not invoke this function
/// at all.
pub fn highlight_with_bat(context: &str, lang: &str) -> Option<String> {
    if lang.is_empty() {
        return None;
    }
    let ss = syntax_set();
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))?;
    let theme = resolve_theme(is_light_theme());
    Some(highlight_as_ansi(context, syntax, theme))
}

/// One highlighted token from [`highlight_bash_commands`]: a run of
/// text sharing a single color/style, in the order it appears in
/// the source command. `color` is resolved RGB (from whichever
/// `syntect` theme was selected — see that function's doc comment),
/// ready to hand straight to `ratatui::style::Color::Rgb` without
/// any further theme lookup or ANSI parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedSpan {
    pub text: String,
    pub color: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// The bundled `syntect` syntax/theme data, parsed once and reused
/// for the lifetime of the process — `SyntaxSet::load_defaults_newlines`
/// and `ThemeSet::load_defaults` both parse a non-trivial amount of
/// bundled definition data, so paying that cost on every highlight
/// call (this runs from the TUI's render path) would defeat the
/// point of moving off a `bat` subprocess for speed.
fn syntax_set() -> &'static syntect::parsing::SyntaxSet {
    static SYNTAX_SET: std::sync::OnceLock<syntect::parsing::SyntaxSet> = std::sync::OnceLock::new();
    SYNTAX_SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static syntect::highlighting::ThemeSet {
    static THEME_SET: std::sync::OnceLock<syntect::highlighting::ThemeSet> = std::sync::OnceLock::new();
    THEME_SET.get_or_init(syntect::highlighting::ThemeSet::load_defaults)
}

/// Syntax-highlight multiple single-line bash command strings using
/// `syntect` — the same Rust highlighting engine `bat` itself is
/// built on — entirely in-process. No subprocess, no external `bat`
/// binary requirement (unlike the `highlight_with_bat*` functions
/// above), and no ANSI-text intermediate to parse back out: this
/// returns already-resolved `HighlightedSpan`s ready to become
/// `ratatui::text::Span`s directly.
///
/// Each element of `commands` should already be a single logical
/// line (no embedded `\n`/`\r` — the TUI's `cmd_display` already
/// replaces those with a visible `↵` marker before this is called);
/// `syntect`'s line-oriented highlighter is given one call per
/// command; unlike the old `bat`-subprocess design there's no
/// external-process cost to batch away, so this is a plain loop, not
/// a single joined call.
///
/// `is_light` selects the `base16-ocean.light`/`base16-ocean.dark`
/// bundled theme — the same theme FAMILY for both, just the
/// light/dark variant, so the two read as a matched pair rather than
/// two visually unrelated palettes. Always succeeds: an unrecognized
/// command (falls back to `syntect`'s built-in plain-text syntax) or
/// a highlighter error yields a single unstyled span for that
/// command rather than a `None`/error the caller has to branch on —
/// there's no external tool that can be "missing" here, so the
/// caller doesn't need a fallback path.
pub fn highlight_bash_commands(commands: &[&str], is_light: bool) -> Vec<Vec<HighlightedSpan>> {
    let ss = syntax_set();
    let ts = theme_set();
    let syntax = ss
        .find_syntax_by_extension("sh")
        .or_else(|| ss.find_syntax_by_token("bash"))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme_name = if is_light {
        "base16-ocean.light"
    } else {
        "base16-ocean.dark"
    };
    let theme = ts
        .themes
        .get(theme_name)
        .or_else(|| ts.themes.values().next())
        .expect("syntect::highlighting::ThemeSet::load_defaults() always bundles at least one theme");

    commands
        .iter()
        .map(|cmd| {
            let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
            // `load_defaults_newlines`-loaded syntaxes expect each
            // line to include its trailing newline for correct
            // internal state tracking; commands here are single
            // lines with none, so append one and trim it back off
            // each returned token below.
            let line_with_newline = format!("{cmd}\n");
            match highlighter.highlight_line(&line_with_newline, ss) {
                Ok(ranges) => ranges
                    .into_iter()
                    .filter_map(|(style, text)| {
                        let text = text.trim_end_matches(['\n', '\r']);
                        if text.is_empty() {
                            return None;
                        }
                        Some(HighlightedSpan {
                            text: text.to_string(),
                            color: (
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                            ),
                            bold: style
                                .font_style
                                .contains(syntect::highlighting::FontStyle::BOLD),
                            italic: style
                                .font_style
                                .contains(syntect::highlighting::FontStyle::ITALIC),
                            underline: style
                                .font_style
                                .contains(syntect::highlighting::FontStyle::UNDERLINE),
                        })
                    })
                    .collect(),
                // `highlight_line` only errors on malformed syntax
                // definitions, never on the input text itself — not
                // expected to happen with the bundled default
                // syntaxes, but fall back to a single plain span
                // rather than panicking or losing the row.
                Err(_) => vec![HighlightedSpan {
                    text: (*cmd).to_string(),
                    color: (255, 255, 255),
                    bold: false,
                    italic: false,
                    underline: false,
                }],
            }
        })
        .collect()
}

/// Like [`highlight_with_bat`], but auto-detects the language from
/// the source file's extension (or exact basename, for
/// extension-less files like `Makefile`) instead of taking an
/// explicit `lang` token. `filepath` only needs to LOOK like the
/// real path — nothing is read from disk; `syntect`'s syntax lookup
/// works purely off the string, the same way the old `bat
/// --file-name <path>` flag did (`bat` itself never opened the file
/// either, since `context` was piped in via stdin). Used by the
/// `tags` / `codegraph` / `ag` / notes / todo / segments / similar /
/// files preview paths when the user did not supply an explicit
/// `@lang` token.
///
/// Unlike [`highlight_with_bat`], this always returns `Some` — an
/// unrecognized extension falls back to `syntect`'s plain-text
/// syntax (no color, but still succeeds), mirroring how `bat
/// --file-name` itself never errors out for an extension it doesn't
/// know, only for things like invalid input encoding.
pub fn highlight_with_bat_auto(context: &str, filepath: &str) -> Option<String> {
    let ss = syntax_set();
    let path = Path::new(filepath);
    let ext = path.extension().and_then(|e| e.to_str());
    let basename = path.file_name().and_then(|n| n.to_str());
    let syntax = ext
        .and_then(|e| ss.find_syntax_by_extension(e))
        .or_else(|| basename.and_then(|b| ss.find_syntax_by_extension(b)))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = resolve_theme(is_light_theme());
    Some(highlight_as_ansi(context, syntax, theme))
}

/// Map a file extension to a language identifier (also usable as
/// [`highlight_with_bat`]'s `lang` argument, since these names
/// overlap with `syntect`'s own syntax tokens for every language
/// listed here).
///
/// Returns `None` when the extension is not associated with a
/// known language. The mapping is intentionally small: the
/// languages a `ctags` `tags` file is likely to cover and the
/// languages that are useful for preview highlighting in a typical
/// polyglot project. Unknown extensions fall through to `None` (no
/// filter applied when the user-supplied `@lang` is empty, or a
/// no-op for preview highlighting, which will then fall back to
/// extension-based auto-detection — see [`highlight_with_bat_auto`]).
///
/// Currently unused by the rest of the crate but kept for
/// future call sites (e.g. an automatic per-file language hint
/// when the user does NOT supply `@lang`). The unit test pins
/// the mapping so an accidental edit is caught.
#[allow(dead_code)]
pub fn language_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "sh" | "bash" | "zsh" => "bash",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "lua" => "lua",
        "vim" => "vim",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "hs" => "haskell",
        "ml" | "mli" => "ocaml",
        "scala" | "sbt" => "scala",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "dart" => "dart",
        "php" => "php",
        "pl" | "pm" => "perl",
        "r" => "r",
        "jl" => "julia",
        "sql" => "sql",
        _ => return None,
    })
}

/// Return the set of file extensions associated with a language
/// identifier. Used by the tags view to filter rows by extension
/// when the user supplies `@lang`.
///
/// The table mirrors `language_for_path`; `None` is returned
/// when the language is unknown so the caller can either skip
/// the filter or surface a status message.
pub fn extensions_for_language(lang: &str) -> Option<&'static [&'static str]> {
    Some(match lang {
        "rust" => &["rs"],
        "python" => &["py"],
        "javascript" => &["js", "mjs", "cjs", "jsx"],
        "typescript" => &["ts", "mts", "cts", "tsx"],
        "tsx" => &["tsx"],
        "go" => &["go"],
        "c" => &["c", "h"],
        "cpp" => &["cc", "cpp", "cxx", "hpp", "hxx"],
        "java" => &["java"],
        "ruby" => &["rb"],
        "bash" => &["sh", "bash", "zsh"],
        "markdown" => &["md", "markdown"],
        "toml" => &["toml"],
        "yaml" => &["yaml", "yml"],
        "json" => &["json"],
        "html" => &["html", "htm"],
        "css" => &["css"],
        "scss" => &["scss", "sass"],
        "lua" => &["lua"],
        "vim" => &["vim"],
        "elixir" => &["ex", "exs"],
        "erlang" => &["erl", "hrl"],
        "haskell" => &["hs"],
        "ocaml" => &["ml", "mli"],
        "scala" => &["scala", "sbt"],
        "swift" => &["swift"],
        "kotlin" => &["kt", "kts"],
        "dart" => &["dart"],
        "php" => &["php"],
        "perl" => &["pl", "pm"],
        "r" => &["r"],
        "julia" => &["jl"],
        "sql" => &["sql"],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_tokens_splits_three_classes() {
        let q = parse_query_tokens("result @rust *.rs extra");
        assert_eq!(q.terms, vec!["result", "extra"]);
        assert_eq!(q.languages, vec!["rust"]);
        assert_eq!(q.globs, vec!["*.rs"]);
    }

    #[test]
    fn parse_query_tokens_drops_empty_at() {
        // A bare `@` (with no language suffix) is silently
        // dropped by the classifier: the leading-`@` lookup
        // finds an empty language, so the token doesn't go to
        // `languages` and the `else` arm that would push it
        // to `terms` is skipped. Only `rust` survives as a
        // plain search term. This matches the ag-mode
        // behaviour where `@` alone is a no-op.
        let q = parse_query_tokens("@ rust");
        assert!(q.languages.is_empty());
        assert_eq!(q.terms, vec!["rust"]);
    }

    #[test]
    fn parse_query_tokens_empty_input() {
        let q = parse_query_tokens("");
        assert!(q.terms.is_empty() && q.globs.is_empty() && q.languages.is_empty());
    }

    #[test]
    fn parse_query_tokens_handles_multiple_languages() {
        let q = parse_query_tokens("@rust @python");
        assert_eq!(q.languages, vec!["rust", "python"]);
    }

    #[test]
    fn language_for_path_known_extensions() {
        assert_eq!(language_for_path(Path::new("foo.rs")), Some("rust"));
        assert_eq!(language_for_path(Path::new("FOO.PY")), Some("python"));
        assert_eq!(language_for_path(Path::new("bar.tsx")), Some("tsx"));
    }

    #[test]
    fn language_for_path_unknown_extension_is_none() {
        assert_eq!(language_for_path(Path::new("foo.xyz")), None);
    }

    #[test]
    fn extensions_for_language_round_trip() {
        let exts = extensions_for_language("rust").unwrap();
        assert!(exts.contains(&"rs"));
        assert!(extensions_for_language("nope").is_none());
    }

    /// Crude CSI-sequence stripper for test verification only —
    /// good enough to prove `highlight_with_bat*` never drops or
    /// reorders the original text, only wraps it in color codes.
    fn strip_ansi_for_test(s: &str) -> String {
        let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(s, "").to_string()
    }

    // `highlight_with_bat`/`highlight_with_bat_auto` are pure Rust
    // (`syntect`, not a `bat` subprocess) as of the syntect swap, so
    // — unlike the old bat-based versions, which had no tests since
    // they depended on an external tool that might not be present
    // in CI — these can exercise real behavior directly.

    #[test]
    fn highlight_with_bat_empty_lang_returns_none() {
        assert_eq!(highlight_with_bat("fn main() {}", ""), None);
    }

    #[test]
    fn highlight_with_bat_unknown_lang_returns_none() {
        // Mirrors the original `bat --language <lang>` behavior:
        // an explicit but unrecognized language name is a hard
        // failure, not a silent fall-through to plain text (that's
        // what `highlight_with_bat_auto` is for).
        assert_eq!(
            highlight_with_bat("hello", "totally-not-a-real-language-xyz"),
            None
        );
    }

    #[test]
    fn highlight_with_bat_known_lang_adds_ansi_without_altering_text() {
        let src = "fn main() {\n    println!(\"hi\");\n}\n";
        let out = highlight_with_bat(src, "rust").expect("rust is a bundled syntect syntax");
        assert!(
            out.contains('\x1b'),
            "expected ANSI escape codes in highlighted output"
        );
        assert_eq!(
            strip_ansi_for_test(&out),
            src,
            "stripping the added color codes must reproduce the original text exactly"
        );
    }

    #[test]
    fn highlight_with_bat_auto_known_extension_highlights() {
        let src = "fn main() {}\n";
        let out =
            highlight_with_bat_auto(src, "src/main.rs").expect("highlight_with_bat_auto always succeeds");
        assert!(out.contains('\x1b'));
        assert_eq!(strip_ansi_for_test(&out), src);
    }

    #[test]
    fn highlight_with_bat_auto_unknown_extension_still_succeeds() {
        // Mirrors the real `bat --file-name <path>` behavior: an
        // extension it doesn't recognize still succeeds (falls back
        // to plain-text highlighting), it doesn't error out the way
        // an explicit unrecognized `--language` does.
        let src = "some random content\n";
        let out = highlight_with_bat_auto(src, "file.totally-unknown-ext-xyz")
            .expect("highlight_with_bat_auto always succeeds, even for an unknown extension");
        assert_eq!(strip_ansi_for_test(&out), src);
    }

    #[test]
    fn highlight_with_bat_auto_matches_extensionless_file_by_basename() {
        // `Makefile` has no extension; the basename-based fallback
        // lookup should still find the Makefile syntax (or at worst
        // fall back to plain text) without erroring.
        let src = "all:\n\techo hi\n";
        let out = highlight_with_bat_auto(src, "Makefile")
            .expect("highlight_with_bat_auto always succeeds");
        assert_eq!(strip_ansi_for_test(&out), src);
    }
}
