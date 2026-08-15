//! Shared helpers for syntax highlighting and query tokenisation.
//!
//! `ag` and the TUI's symbols (`$`) mode both benefit from:
//! - Bat-based syntax highlighting (per-line context piped through
//!   the `bat` CLI with `--color=always`).
//! - A common "split the query into search terms, globs, and
//!   `@lang` language flags" classifier.
//!
//! These used to live inline in `src/ag.rs`; extracting them here
//! keeps the ag module small and lets other modes (currently the
//! tags view, future content views) reuse the same plumbing
//! without copy-pasting the implementation.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// A simple classifier for a query body.
///
/// - `terms` are plain whitespace-separated search terms.
/// - `globs` are tokens containing `*` (shell-style file globs).
/// - `languages` are tokens with a leading `@` (e.g. `@rust`).
///
/// Used by both `ag` mode and the tags view: ag passes `globs` to
/// `ag -G` and `languages` to `ag --<lang>`; tags mode uses
/// `languages` to filter by file extension and to pipe the
/// preview through `bat --language <lang>`.
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
/// and bat highlighting) should pick the first entry.
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

/// Pipe a source snippet through `bat` for syntax highlighting.
/// Returns `None` if `bat` is not on PATH, the call fails, or
/// `bat` exits non-zero (the caller falls back to the unhighlighted
/// text).
///
/// `lang` is forwarded as `bat --language <lang>`. When `lang` is
/// empty the caller should not invoke this function at all.
///
/// The active TUI theme's light/dark classification is read
/// from the `PALETTE` thread-local (populated by
/// `install_palette`) so `bat` uses the matching `--theme=light`
/// or `--theme=dark` for syntax colors that contrast with the
/// TUI's background.
fn bat_theme_arg() -> &'static str {
    if crate::tui::theme::palette_storage::PALETTE.with(|p| p.borrow().is_light_theme) {
        "light"
    } else {
        "dark"
    }
}

pub fn highlight_with_bat(context: &str, lang: &str) -> Option<String> {
    if lang.is_empty() {
        return None;
    }
    let theme_arg = bat_theme_arg();
    let mut child = Command::new("bat")
        .arg("--language")
        .arg(lang)
        .arg("--plain")
        .arg("--color=always")
        .arg("--theme")
        .arg(theme_arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    {
        let stdin = child.stdin.as_mut()?;
        let _ = stdin.write_all(context.as_bytes());
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
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

/// Like [`highlight_with_bat`], but lets `bat` auto-detect the
/// language from the source file's extension (no `--language`
/// flag). `bat` reads the snippet from stdin so it has no
/// filename to detect from by default; passing `--file-name=`
/// with the real source path restores `bat`'s extension-based
/// detection (`.rs` → Rust, `.java` → Java, …). Used by the
/// `tags` / `codegraph` / `ag` preview paths when the user did
/// not supply an explicit `@lang` token. Returns `None` when
/// `bat` is unavailable, exits non-zero, or emits non-UTF8 —
/// the caller falls back to the unhighlighted text.
pub fn highlight_with_bat_auto(context: &str, filepath: &str) -> Option<String> {
    let theme_arg = bat_theme_arg();
    let mut child = Command::new("bat")
        .arg("--plain")
        .arg("--color=always")
        .arg("--file-name")
        .arg(filepath)
        .arg("--theme")
        .arg(theme_arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    {
        let stdin = child.stdin.as_mut()?;
        let _ = stdin.write_all(context.as_bytes());
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Map a file extension to a `bat` language identifier.
///
/// Returns `None` when the extension is not associated with a
/// known language. The mapping is intentionally small: the
/// languages a `ctags` `tags` file is likely to cover and the
/// languages that are useful for `bat` highlighting in a typical
/// polyglot project. Unknown extensions fall through to `None`
/// (no filter applied when the user-supplied `@lang` is empty,
/// or a no-op for `bat` which will then highlight by extension
/// automatically).
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

/// Return the set of file extensions associated with a `bat`
/// language identifier. Used by the tags view to filter rows by
/// extension when the user supplies `@lang`.
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
}
