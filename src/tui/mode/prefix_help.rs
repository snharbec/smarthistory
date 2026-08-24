//! Content for the prefix query-syntax help overlay
//! ([`crate::tui::bindings::Action::PrefixHelp`], default `F3`) —
//! distinct from the keyboard-shortcut help overlay
//! (`Action::OpenHelp`, `C-a`, see `build_help_lines` in
//! `src/tui/render.rs`).
//!
//! Every string here is a condensed cheatsheet, not the full prose —
//! see the corresponding `docs/modes/*.md` file for the complete
//! writeup (rationale, edge cases, cross-references). Keep the two in
//! sync when either changes.
use crate::tui::mode::ModeKind;
use crate::tui::theme::Theme;
use crate::QueryPrefixes;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Entry point: `None` shows the general per-prefix overview,
/// `Some(mode)` shows that mode's query-syntax cheatsheet.
pub(crate) fn lines_for(mode: Option<ModeKind>, prefixes: &QueryPrefixes) -> Vec<Line<'static>> {
    match mode {
        None => overview_lines(prefixes),
        Some(mode) => mode_lines(mode),
    }
}

fn heading(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn blank() -> Line<'static> {
    Line::from("")
}

fn text(s: &'static str) -> Line<'static> {
    Line::from(s)
}

/// One `token — meaning` row, styled like the existing help
/// overlay's shortcut rows (bright token, dim separator).
fn token_row(token: String, meaning: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<18}", token),
            Style::default().fg(Theme::highlight_color()),
        ),
        Span::raw(meaning),
    ])
}

/// The general overview shown when no prefix mode is active and the
/// picker isn't open — one line per prefix, reusing the exact same
/// data `F1`'s picker itself renders from (`PrefixPicker::build_options`)
/// so there's a single source of truth for the name/label/description
/// triple instead of a second hand-maintained copy.
fn overview_lines(prefixes: &QueryPrefixes) -> Vec<Line<'static>> {
    let mut lines = vec![
        heading("Prefix modes"),
        blank(),
        text("  F1 to browse and switch; F3 on a highlighted row there"),
        text("  shows that mode's full query syntax."),
        blank(),
    ];
    for opt in crate::tui::PrefixPicker::build_options(prefixes) {
        let prefix_str = match opt.prefix {
            Some(c) => c.to_string(),
            None => "(none)".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<6}", prefix_str),
                Style::default()
                    .fg(Theme::highlight_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:<16}", opt.label), Theme::dim()),
            Span::raw(opt.description),
        ]));
    }
    lines
}

fn mode_lines(mode: ModeKind) -> Vec<Line<'static>> {
    match mode {
        ModeKind::Notes => notes_lines(),
        ModeKind::Todo => todo_lines(),
        ModeKind::Segments => segments_lines(),
        ModeKind::Similar => similar_lines(),
        ModeKind::Paperless => paperless_lines(),
        ModeKind::Jira => jira_lines(),
        ModeKind::Ag => ag_lines(),
        ModeKind::Codegraph => codegraph_lines(),
        ModeKind::Tags => tags_lines(),
        _ => fallback_lines(mode),
    }
}

/// The shared `note_search::parse_query` DSL — used verbatim by
/// `@` (Notes) and `!` (Todo), and by `:` (Segments) (see
/// `segments_lines`, which starts from this and adds its own
/// cascade-rule note). See `docs/modes/notes.md#shorthand-expansion`.
fn core_dsl_lines() -> Vec<Line<'static>> {
    vec![
        token_row("word".into(), "AND-matched substring (case-insensitive)"),
        token_row("\"quoted phrase\"".into(), "exact phrase match"),
        token_row("#tag".into(), "tagged `tag`"),
        token_row("[[link]]".into(), "links to `link` (wiki-link)"),
        token_row("[attr:value]".into(), "attribute `attr` equals `value`"),
        token_row("[attr]".into(), "has attribute `attr`, any value"),
        token_row("(a OR b)".into(), "OR-grouping (bare terms AND by default)"),
        blank(),
        text("  Trailing `!` on #tag / [[link]] / [attr:value] / [attr] negates it"),
        text("  (\"does NOT have\") instead of requiring it."),
        blank(),
        token_row("!!type".into(), "shorthand for [type:value]! (exclude type)"),
        token_row(
            "!type".into(),
            "restrict to ONLY type:value (repeatable, OR'd:",
        ),
        text("                    !jira !meeting means \"jira or meeting\")"),
    ]
}

fn notes_lines() -> Vec<Line<'static>> {
    let mut lines = vec![
        heading("@ Notes — whole-file search (note_search DB)"),
        blank(),
    ];
    lines.extend(core_dsl_lines());
    lines.push(blank());
    lines.push(heading("Notes-only extras"));
    lines.push(token_row(
        "@link".into(),
        "shorthand for [[link]] (@\"my note\" for spaced names)",
    ));
    lines.push(token_row(
        "@today / @week".into(),
        "date filter on last-updated (whole-word only —",
    ));
    lines.push(text(
        "  @month / @year   email@today.com is NOT matched as an alias)",
    ));
    lines.push(token_row("@new <text>".into(), "quick-create: appends to today's daily note"));
    lines.push(blank());
    lines.push(text("  Empty query (bare @) returns every note."));
    lines
}

fn todo_lines() -> Vec<Line<'static>> {
    let mut lines = vec![
        heading("! Todo — open markdown checkboxes (note_search DB)"),
        blank(),
        text("  Filters TODOS BY THEIR NOTE's tag/link/attribute — same DSL as @ Notes,"),
        text("  applied to which note a todo lives in, not the todo text itself:"),
        blank(),
    ];
    lines.extend(core_dsl_lines());
    lines.push(blank());
    lines.push(heading("Todo-only extras"));
    lines.push(token_row(
        "!@new <text>".into(),
        "quick-create: appends \"- [ ] <text>\" to today's note",
    ));
    lines.push(blank());
    lines.push(text(
        "  Empty query (bare !) shows every OPEN todo; closed ones are hidden.",
    ));
    lines
}

fn segments_lines() -> Vec<Line<'static>> {
    let mut lines = vec![
        heading(": Segments — header-anchored sections (note_search DB)"),
        blank(),
        text("  A segment is one markdown header (level 1-4) plus everything below it"),
        text("  up to the next header. Finer-grained than @ Notes (whole file)."),
        blank(),
    ];
    lines.extend(core_dsl_lines());
    lines.push(blank());
    lines.push(text(
        "  Bare-word text matching checks the segment's OWN text only (not",
    ));
    lines.push(text(
        "  cascaded); #tag / [[link]] DO cascade from ancestor headers and the",
    ));
    lines.push(text(
        "  whole document. Empty query (bare :) returns every indexed segment.",
    ));
    lines
}

fn similar_lines() -> Vec<Line<'static>> {
    vec![
        heading("\" Similar — embedding similarity over the segments table"),
        blank(),
        text("  No query DSL — the whole typed body is ONE literal phrase, embedded"),
        text("  (local Ollama call) and ranked against every segment by cosine"),
        text("  similarity (top 25, highest first). Any #/[[ ]] you type just becomes"),
        text("  part of the literal phrase text — UNLESS suffixed with `!`:"),
        blank(),
        token_row(
            "#tag! / [[link]]!".into(),
            "stripped before embedding, applied as a",
        ),
        text("  [attr:value]! / [attr]!  post-filter over the ranked results"),
        token_row("!!type / !type".into(), "same exclude/restrict shorthand, also stripped"),
        blank(),
        text("  Empty query, or a query that's ONLY negation/type tokens with nothing"),
        text("  left to embed, is a no-op (nothing to rank against)."),
    ]
}

fn paperless_lines() -> Vec<Line<'static>> {
    vec![
        heading("< Paperless — Paperless-ngx document search (REST API)"),
        blank(),
        token_row(
            "word".into(),
            "title substring match, case-insensitive. Multiple bare",
        ),
        text("  words join into ONE literal substring (not independently ANDed —"),
        text("  an API limitation, not a design choice)."),
        token_row("#TAG".into(), "tag, whole-word/exact match (only the LAST #TAG used)"),
        token_row(
            "@AUTHOR".into(),
            "correspondent, whole-word/exact (only the LAST @AUTHOR used)",
        ),
        blank(),
        text("  No OR-grouping, negation, or attribute syntax here — different DSL"),
        text("  from the note_search-backed modes above."),
    ]
}

fn jira_lines() -> Vec<Line<'static>> {
    vec![
        heading("- JIRA — issue search (REST API / JQL)"),
        blank(),
        token_row("word".into(), "free-text search (summary / description)"),
        token_row("field=value".into(), "JQL field filter, e.g. status=Open priority=Blocker"),
        token_row("JOB-1234".into(), "matches that single issue by key"),
        blank(),
        heading("JQL tag fragments"),
        token_row("@me".into(), "assignee = currentUser()"),
        token_row("@today".into(), "updated in the last 24 hours"),
        token_row("@week".into(), "updated in the last 7 days"),
        token_row("@month".into(), "updated in the last 31 days"),
        token_row(
            "@<name>".into(),
            "user-defined fragment (jira.search.<name>=<jql>)",
        ),
        blank(),
        text("  Tags are whole-word tokens (case-insensitive, @ optional) and combine"),
        text("  with everything else by AND."),
    ]
}

fn ag_lines() -> Vec<Line<'static>> {
    vec![
        heading(", ag — file content search (the_silver_searcher)"),
        blank(),
        token_row("word".into(), "every line containing `word` in any text file under cwd"),
        token_row("*.rs".into(), "shell-style glob restricting which files are searched"),
        token_row("@lang".into(), "restrict to a file type, e.g. @rust (ag --rust)"),
        blank(),
        text("  Empty query (bare ,) returns nothing — ag needs at least one term."),
    ]
}

fn codegraph_lines() -> Vec<Line<'static>> {
    vec![
        heading("& CodeGraph — FTS5 symbol search (.codegraph/codegraph.db)"),
        blank(),
        token_row(
            "word".into(),
            "symbols whose name starts with `word` (FTS5 prefix match)",
        ),
        token_row("@lang".into(), "restrict to a language, e.g. @java"),
        blank(),
        text("  Empty query (bare &) returns nothing — FTS5 needs at least one term."),
    ]
}

fn tags_lines() -> Vec<Line<'static>> {
    vec![
        heading("$ Tags — universal-ctags symbol search"),
        blank(),
        token_row("word".into(), "symbols whose name contains `word` (case-insensitive)"),
        token_row("@lang".into(), "restrict by file extension, e.g. @rust (.rs)"),
        blank(),
        text("  Empty query (bare $) lists every symbol in the nearest tags file."),
    ]
}

/// Every mode with no special query syntax — plain substring/AND-word
/// filtering only.
fn fallback_lines(mode: ModeKind) -> Vec<Line<'static>> {
    vec![
        heading(mode.list_title()),
        blank(),
        text("  No special query syntax here — space-separated words are"),
        text("  case-insensitive, AND-matched substrings against each row's text."),
    ]
}
