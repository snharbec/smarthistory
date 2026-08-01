//! `-` (JIRA issue search) prefix mode.
//!
//! Lists JIRA issues from a self-hosted instance matching
//! the typed query (issue keys, `field=value` constraints,
//! or free text matched against description / summary).
//! Selecting an issue opens its browse URL in the system
//! browser. Credentials / config come from the
//! `JIRA_SERVER`, `JIRA_API_TOKEN`, `JIRA_URL`, and
//! `JIRA_PROJECT` environment variables.
use crate::tui::mode::CheckReport;
use crate::tui::state::HistoryRow;
use crate::tui::{
    App, CompletionKind, JIRA_DEBOUNCE, JIRA_IDLE_TIMEOUT, OutputView, PickMode,
    char_to_byte_index,
};
use anyhow::Result;
use crate::jira::JiraClient;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// Whether the query is a JIRA issue-search request:
/// the query starts with the jira prefix (`-` by
/// default). The body is parsed into a JQL query by
/// `crate::jira::build_jql` (issue keys,
/// `field=value` constraints, free text).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.jira;
    !app.query.is_empty() && app.query.starts_with(p)
}

/// The JIRA search body, i.e. everything after the
/// leading `-` prefix. Empty string when not in jira
/// mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.jira;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}

/// Health check for the JIRA (`-`) issue-search
/// mode. The mode talks to a self-hosted JIRA
/// instance via REST for every search, so the
/// check verifies:
///
/// 1. The required env vars are set
///    (`JIRA_SERVER` + `JIRA_API_TOKEN`).
/// 2. The optional `JIRA_PROJECT` (if set) is
///    a non-empty project key.
/// 3. The JIRA server is reachable
///    (`GET {server}/rest/api/3/myself` with
///    Bearer auth returns HTTP 200).
/// 4. The configured project exists
///    (`GET {server}/rest/api/3/project/{key}`
///    returns HTTP 200, not 404).
///
/// Stops at the first failure.
pub(crate) fn check(_app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Jira;

    // 1. Required env vars.
    let server = match std::env::var("JIRA_SERVER") {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) | Err(_) => {
            return CheckReport::err(
                mode,
                "JIRA_SERVER env var is not set or is empty (set it to your JIRA base URL, e.g. https://jira.example.com)",
            );
        }
    };
    let token = match std::env::var("JIRA_API_TOKEN") {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) | Err(_) => {
            return CheckReport::err(
                mode,
                "JIRA_API_TOKEN env var is not set or is empty (create an API token at https://id.atlassian.com/manage-profile/security/api-tokens and set this env var)",
            );
        }
    };
    let project = std::env::var("JIRA_PROJECT")
        .ok()
        .filter(|s| !s.trim().is_empty());

    // 2. Optional `JIRA_URL` (browse URL base; defaults
    //    to `JIRA_SERVER`).
    let browse_url = std::env::var("JIRA_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| server.trim_end_matches('/').to_string());

    // 3. Server reachability + auth. The
    //    `/myself` endpoint is the lightest
    //    authenticated request: it returns
    //    200 with the user payload, 401 if
    //    the token is wrong, 403 if the user
    //    is disabled.
    let myself_url = format!("{}/rest/api/3/myself", server.trim_end_matches('/'));
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let resp = client
        .get(&myself_url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .call();
    let status = match resp {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(ureq::Error::Transport(t)) => {
            return CheckReport::err(mode, format!("could not reach JIRA at {server}: {t}"));
        }
    };
    if status == 401 {
        return CheckReport::err(
            mode,
            format!("JIRA at {server} returned 401 Unauthorized (the API token is invalid or expired; create a new one at https://id.atlassian.com/manage-profile/security/api-tokens)"),
        );
    }
    if status == 403 {
        return CheckReport::err(
            mode,
            format!("JIRA at {server} returned 403 Forbidden (the user this token belongs to is disabled or lacks the `Browse Projects` permission)"),
        );
    }
    if !(200..300).contains(&status) {
        return CheckReport::err(
            mode,
            format!("JIRA at {server} returned HTTP {status} on /myself"),
        );
    }

    // 4. Project existence (only when
    //    `JIRA_PROJECT` is set — otherwise
    //    the runtime uses a server-wide
    //    search).
    if let Some(key) = project.as_deref() {
        let proj_url = format!(
            "{}/rest/api/3/project/{}",
            server.trim_end_matches('/'),
            key
        );
        let proj_resp = client
            .get(&proj_url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .call();
        let proj_status = match proj_resp {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(ureq::Error::Transport(t)) => {
                return CheckReport::err(mode, format!("could not probe JIRA project {key}: {t}"));
            }
        };
        if proj_status == 404 {
            return CheckReport::err(
                mode,
                format!("JIRA project `{key}` does not exist on {server} (or you don't have permission to view it)"),
            );
        }
        if !(200..300).contains(&proj_status) {
            return CheckReport::err(
                mode,
                format!("JIRA project probe returned HTTP {proj_status}"),
            );
        }
        CheckReport::ok(
            mode,
            format!("JIRA reachable at {browse_url}, project `{key}` is visible"),
        )
    } else {
        CheckReport::ok(
            mode,
            format!("JIRA reachable at {browse_url} (no JIRA_PROJECT set; the runtime uses a server-wide search)"),
        )
    }
}

/// Fetch the JIRA-mode result set. The JIRA search
/// runs on a background thread (spawned by
/// `App::jira_touch` → `crate::jira::spawn_jira_search`),
/// so this just clones the cached rows from
/// `App::jira_rows`. A future pass can move the
/// JIRA background-thread orchestration here.
pub(crate) fn fetch(app: &mut App) -> Result<Vec<HistoryRow>> {
    Ok(app.jira_rows.clone())
}

/// An in-flight JIRA search request. Same shape as
/// `LlmRequest`: a background thread sends the result over
/// the channel, the run loop polls it, and the cancelled
/// flag lets the user abort a slow search with `Esc`.
pub(crate) struct JiraRequest {
    pub(crate) receiver: std::sync::mpsc::Receiver<Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError>>,
    pub(crate) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// An in-flight JIRA comments fetch request.
/// Mirrors `JiraRequest` (a background thread
/// sends the result over the channel, the run
/// loop polls it, the cancelled flag lets the
/// user abort a slow fetch with `Esc`).
///
/// The result is `Vec<JiraComment>`, sorted
/// newest-first by the TUI after the fetch
/// completes (JIRA's REST v2 returns comments in
/// `created` ascending order, so the TUI
/// reverses them on the way in).
pub(crate) struct JiraCommentsRequest {
    pub(crate) receiver: std::sync::mpsc::Receiver<Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError>>,
    pub(crate) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The issue key the comments were fetched
    /// for. Kept on the request struct so the
    /// run-loop poll can match the result back
    /// to the row that initiated the fetch
    /// (the user may have navigated away from
    /// the row in the meantime; we still want
    /// the overlay to be tied to the right
    /// issue's comments).
    pub(crate) key: String,
}

/// An in-flight JIRA add-comment POST.
/// Mirrors `JiraCommentsRequest`: a
/// background thread sends the result
/// (success or error) over the channel,
/// the run loop polls it, and the
/// cancelled flag lets the user abort a
/// slow POST with `Esc`.
///
/// The result is `Result<(), JiraError>`.
/// `Ok(())` means JIRA returned 201
/// Created; the buffer is cleared and
/// the user sees a "Comment posted"
/// status. `Err(...)` is surfaced as a
/// status message; the buffer stays so
/// the user can fix any issue and retry
/// without retyping.
///
/// The `key` and `body` are kept on the
/// struct so the run-loop poll can
/// attach the result to the right issue
/// and so cancellation can show
/// "JIRA add-comment cancelled" with
/// the right issue context.
#[allow(dead_code)]
pub(crate) struct JiraAddCommentRequest {
    pub(crate) receiver: std::sync::mpsc::Receiver<Result<(), crate::jira::JiraError>>,
    pub(crate) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The issue key the comment is being
    /// posted to. Stashed on the request
    /// struct so the result-processing
    /// step can surface the issue
    /// context in status messages
    /// ("Comment posted to PROJ-1"
    /// rather than just "Comment posted").
    pub(crate) key: String,
    /// The body of the comment being
    /// posted. Kept on the struct so
    /// cancellation messages can
    /// reference what was being
    /// cancelled ("JIRA add-comment
    /// cancelled (was posting to PROJ-1)"),
    /// and so future code that wants to
    /// retry on transient failures
    /// (network blip, JIRA 503) can
    /// re-fire the same body without
    /// re-reading the buffer.
    pub(crate) body: String,
}

/// Test-injection seam for [`App::open_jira_in_background`]'s real
/// child-process spawn. Production code has no implementor:
/// `App::url_opener` stays `None`, and the function falls through
/// to spawning the real `open` / `xdg-open` binary exactly as
/// before this seam existed. Tests inject a recording stub via
/// `set_url_opener` (`#[cfg(test)]`, see `src/tui/tests.rs`) so
/// `cargo test` never actually launches a browser — mirrors
/// `crate::jira::JiraClient`'s "injectable fake, `None` by default"
/// convention.
pub(crate) trait UrlOpener: Send + Sync {
    /// `opener_cmd` is the binary name (`"open"` on macOS,
    /// `"xdg-open"` elsewhere); `url` is the browse URL. A real
    /// implementor would spawn `opener_cmd url`; the test stub just
    /// records the call.
    fn open(&self, opener_cmd: &str, url: &str);
}
/// Sort a JIRA comments list newest-first by
/// the `created` timestamp. JIRA's REST v2
/// `comment` endpoint returns comments in
/// `created` *ascending* order, so we reverse
/// them here. The `id` field is the
/// tie-breaker when two comments share the
/// same `created` timestamp (rare but
/// possible for batch-imported comments);
/// JIRA's comment IDs are roughly
/// monotonically increasing, so the higher
/// ID wins (newer insertion).
///
/// Kept as a free function (not a method) so
/// the test can drive it directly with a
/// canned `Vec<JiraComment>` without
/// standing up the full `App` and the
/// SQLite DB.
pub(crate) fn sort_comments_newest_first(comments: &mut [crate::jira::JiraComment]) {
    comments.sort_by(|a, b| {
        // `parse_rfc3339`-style parse via
        // `updated_to_epoch` (the same
        // helper the JQL-built-in test
        // epoch uses). On parse failure
        // (empty / malformed), the
        // function returns 0; an `Ord`
        // tie on 0 falls through to the
        // id-based tie-breaker.
        let ea = crate::jira::updated_to_epoch(&a.created);
        let eb = crate::jira::updated_to_epoch(&b.created);
        // Reverse: newer (`eb > ea`)
        // comes first.
        eb.cmp(&ea).then_with(|| {
            // Tie-breaker: compare the
            // comment IDs as strings.
            // JIRA's comment IDs are
            // numeric, so this is
            // effectively a numeric
            // comparison; using
            // `Ord` on `String` is
            // correct enough for
            // the rare batch-import
            // case.
            b.id.cmp(&a.id)
        })
    });
}

/// Build the markdown-like overlay text for
/// a JIRA row + its comments. The format
/// is what the user spec calls out:
///
/// ```text
/// ## Header
/// <3-line preview: Status/Priority,
///                 Due/Assignee,
///                 Description label>
/// <description body lines>
///
/// ## Description
/// <full description text>
///
/// ## Comments
/// ## <author> · <date>
/// <comment text>
/// ## <author> · <date>
/// <comment text>
/// ...
/// ```
///
/// Each section is preceded by an `## `
/// heading marker (rendered as a
/// heading-styled span by the preview
/// renderer). Per-comment sub-headings
/// follow the same convention with the
/// author and date in the heading text.
/// Empty sections (no description, no
/// comments) are still emitted with a
/// `(none)` placeholder so the user
/// always sees the section structure.
pub(crate) fn build_jira_overlay_text(
    row: &crate::tui::state::HistoryRow,
    comments: &[crate::jira::JiraComment],
) -> String {
    let mut out = String::new();

    // The preview pane's `row.output`
    // is the 3-line metadata header
    // (Status/Priority, Due/Assignee,
    // Description label) followed by
    // the description body (which can
    // be multiple lines for
    // multi-paragraph descriptions).
    //
    // Split it into the two parts up
    // front so we can route them to
    // separate sections without
    // duplicating the description.
    // The first 3 lines are the
    // metadata block; everything
    // from line 4 onwards is the
    // description body.
    let mut all_lines = row.output.lines();
    let header_block: Vec<&str> = all_lines.by_ref().take(3).collect();
    let description_body: String = all_lines.collect::<Vec<_>>().join("\n");

    // ---- ## Header ----
    out.push_str("## Header\n");
    // The `# Header` section shows
    // the metadata block — the
    // same 3 lines the preview
    // pane shows. The
    // description body is
    // NOT included here (it
    // lives in its own
    // `## Description`
    // section below, so the
    // description appears
    // exactly once in the
    // overlay). The
    // original implementation
    // copied the full
    // `row.output` here,
    // which caused the
    // description to
    // appear twice (once in
    // `# Header`, once in
    // `# Description`); the
    // user reported this and
    // asked for the
    // description to be
    // visible only once.
    for line in &header_block {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');

    // ---- ## Description ----
    out.push_str("## Description\n");
    if description_body.is_empty() || description_body == "<none>" {
        out.push_str("(no description)\n");
    } else {
        out.push_str(&description_body);
        out.push('\n');
    }
    out.push('\n');

    // ---- ## Comments ----
    out.push_str("## Comments\n");
    if comments.is_empty() {
        out.push_str("(no comments)\n");
        return out;
    }
    for comment in comments {
        // `## <author> · <date>` sub-heading.
        // The `·` (U+00B7 MIDDLE DOT) is
        // a clean separator that renders
        // consistently across terminals
        // and doesn't conflict with any
        // markdown convention.
        let author = if comment.author.is_empty() {
            "(unknown)"
        } else {
            comment.author.as_str()
        };
        let date = format_jira_date(&comment.created);
        out.push_str(&format!("## {} \u{00b7} {}\n", author, date));
        // The comment body. Empty
        // bodies get a placeholder so
        // the user knows the comment
        // exists (vs. the section
        // being incomplete).
        if comment.body.is_empty() {
            out.push_str("(empty comment)\n");
        } else {
            out.push_str(&comment.body);
            out.push('\n');
        }
        // A blank line between comments
        // for visual separation. The
        // renderer's `lines()` iteration
        // strips the trailing blank
        // line in the last comment.
        out.push('\n');
    }
    out
}

/// Format a JIRA ISO-8601 `created` timestamp
/// (e.g. `2024-06-30T19:14:39.000+0000`)
/// into a short, human-readable date
/// (`2024-06-30 19:14 UTC`). The JQL comment
/// sub-headings list date + author, so the
/// format is opinionated: we drop the
/// milliseconds and the offset (just `UTC`
/// as a timezone hint) to keep the heading
/// compact. JIRA's `created` is always UTC
/// in practice, so this is a safe
/// presentation.
pub(crate) fn format_jira_date(iso: &str) -> String {
    let s = iso.trim();
    if s.is_empty() {
        return String::new();
    }
    // Parse the `YYYY-MM-DDTHH:MM:SS`
    // prefix by slicing. We don't need
    // the full `chrono` machinery for
    // this short format — JIRA's
    // timestamps are stable and the
    // substring is the same width.
    //
    // Minimum length for a
    // `YYYY-MM-DDTHH:MM` extract is
    // 16 chars. Anything shorter is
    // malformed and we degrade to
    // the raw string.
    if s.len() < 16 {
        return s.to_string();
    }
    let date = &s[..10]; // YYYY-MM-DD
    let time = &s[11..16]; // HH:MM
    format!("{} {} UTC", date, time)
}


impl App {
    /// Whether the query is a JIRA issue-search request:
    /// the query starts with the jira prefix (`-` by
    /// default). The body is parsed into a JQL query by
    /// `crate::jira::build_jql` (issue keys,
    /// `field=value` constraints, free text).
    pub(crate) fn is_jira_query(&self) -> bool {
        matches(self)
    }

    /// The JIRA search body, i.e. everything after the
    /// leading `-` prefix. Empty string when not in jira
    /// mode.
    pub(crate) fn jira_pattern(&self) -> &str {
        pattern(self)
    }

    /// Arm the JIRA search debounce. Called from every
    /// keystroke path when in `-`-mode (push_char,
    /// backspace, set_search_mode_prefix, etc.) — mirrors
    /// `llm_touch`. The run loop's tick then fires the
    /// actual search after `JIRA_DEBOUNCE` of quiet.
    /// Outside `-`-mode this clears any pending state so a
    /// stray timer doesn't fire after the user leaves the
    /// mode.
    pub(crate) fn jira_touch(&mut self) {
        if self.is_jira_query() {
            self.jira_debounce_started = Some(std::time::Instant::now());
            // The idle timer fires in
            // lock-step with the
            // fast debounce. Both
            // are armed on every
            // keystroke; the run
            // loop tick fires
            // whichever is due
            // first (400ms for the
            // fast debounce, 3s
            // for the idle timer).
            // See [`JIRA_IDLE_TIMEOUT`]
            // for the safety-net
            // rationale.
            self.jira_idle_started = Some(std::time::Instant::now());
        } else {
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
            self.jira_in_flight = false;
        }
    }

    /// Build the JQL string for the current query body,
    /// using the configured `JIRA_PROJECT` as the default
    /// when the body is empty. The project is read from
    /// `JIRA_PROJECT` directly (not the full `JiraConfig`)
    /// so this works in tests where only a fake client is
    /// injected and no `JIRA_SERVER`/`JIRA_API_TOKEN` is
    /// set.
    ///
    /// The user-defined JQL fragments (from the
    /// `jira.search.<name>=` config keys) are spliced
    /// into the JQL when the body contains `@<name>`
    /// tokens. Any unresolved fragment names are stashed
    /// on `self.jira_undefined_fragments`; the autocall
    /// reads that to decide whether to skip the search
    /// and surface a diagnostic.
    pub(crate) fn jira_build_query(&mut self) -> String {
        let project = std::env::var("JIRA_PROJECT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        // `now_epoch()` is the wall clock the JQL
        // builder uses to compute the date-cutoff for
        // the `@today` / `@week` / `@month` aliases
        // (e.g. `@week` becomes
        // `updated >= "<today - 7d>"`). It's the same
        // helper the rest of the TUI uses for "now",
        // so all date-bearing features see a consistent
        // view of time.
        let (jql, undefined) = crate::jira::build_jql(
            self.jira_pattern(),
            project.as_deref(),
            self.now_epoch(),
            &self.jira_fragments,
        );
        self.jira_undefined_fragments = undefined;
        jql
    }

    /// Drive the JIRA search debounce. Called from the
    /// run-loop tick on the no-input path (mirrors
    /// `llm_maybe_autocall`). Fires a single background
    /// search when: in `-`-mode, debounce elapsed, no
    /// search in flight, JIRA is configured (env vars OR
    /// an injected test client), and the live JQL differs
    /// from the last-fired one.
    ///
    /// Two timers can trigger a fire:
    /// 1. The 400ms fast debounce
    ///    ([`JIRA_DEBOUNCE`]) — handles the
    ///    fast-typo case (user
    ///    pauses briefly after
    ///    typing).
    /// 2. The 3-second idle
    ///    timer
    ///    ([`JIRA_IDLE_TIMEOUT`])
    ///    — safety-net trigger
    ///    that guarantees the
    ///    query fires within 3
    ///    seconds of the last
    ///    keystroke regardless
    ///    of whether the fast
    ///    debounce ever elapses
    ///    (e.g. the user keeps
    ///    typing slowly, or
    ///    the run loop is
    ///    temporarily blocked).
    /// The space key
    /// (`' '`) has its own
    /// explicit-fire path in
    /// `push_char` that fires
    /// immediately, before
    /// either timer.
    pub(crate) fn jira_maybe_autocall(&mut self) {
        if !self.is_jira_query() {
            return;
        }
        if self.jira_in_flight {
            return;
        }
        // The fast debounce and
        // the idle timer are
        // armed in lock-step by
        // `jira_touch`. Either
        // can fire the search
        // when its respective
        // window elapses.
        let debounce_elapsed = self
            .jira_debounce_started
            .map(|started| started.elapsed() >= JIRA_DEBOUNCE)
            .unwrap_or(false);
        let idle_elapsed = self
            .jira_idle_started
            .map(|started| started.elapsed() >= JIRA_IDLE_TIMEOUT)
            .unwrap_or(false);
        if !debounce_elapsed && !idle_elapsed {
            return;
        }
        // "Configured" means either real env config OR an
        // injected test client. If neither, surface a
        // one-shot status message and disarm.
        let configured =
            self.jira_client.is_some() || crate::jira::JiraConfig::from_env().is_some();
        if !configured {
            if self.jira_last_jql.is_some() || self.jira_rows.is_empty() {
                self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            }
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
            return;
        }
        let jql = self.jira_build_query();
        // If the user typed `@somefrag` and `somefrag`
        // isn't in the configured fragments map, the
        // JQL is still valid (the unknown token falls
        // through to free text) but the search would
        // return wrong results. Refuse to fire and
        // surface a diagnostic instead. We only emit
        // the status message when the undefined list
        // CHANGES, so the user doesn't get a stale
        // message re-surfacing on every keystroke
        // while they correct a typo.
        if !self.jira_undefined_fragments.is_empty() {
            if self.jira_last_undefined_message.as_ref() != Some(&self.jira_undefined_fragments) {
                self.set_status_message(format!(
                    "JIRA fragment{} not configured: {}. \
                     Define via jira.search.<name>=... in \
                     ~/.config/smarthistory/config.",
                    if self.jira_undefined_fragments.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    self.jira_undefined_fragments
                        .iter()
                        .map(|n| format!("@{}", n))
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
                self.jira_last_undefined_message = Some(self.jira_undefined_fragments.clone());
            }
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
            return;
        }
        // No undefined fragments on this build — clear
        // the debounce on the "fragment not configured"
        // message so a previously-flagged typo doesn't
        // keep a stale status visible forever. (The
        // message is replaced by the next status set
        // anywhere in the app; this just keeps the
        // bookkeeping consistent.)
        self.jira_last_undefined_message = None;
        // Skip if we already have results for this exact JQL.
        if self.jira_last_jql.as_deref() == Some(&jql) {
            return;
        }
        self.spawn_jira_request(jql);
    }

    /// Spawn the search. When an injected test client is
    /// present (`set_jira_client`), run synchronously on
    /// the calling thread (deterministic for tests).
    /// Otherwise spawn a real `reqwest` background thread
    /// against the env-configured JIRA server.
    pub(crate) fn spawn_jira_request(&mut self, jql: String) {
        if let Some(client) = self.jira_client.clone() {
            let result = client.search(&jql);
            let request = JiraRequest {
                receiver: mpsc::channel().1,
                cancelled: Arc::new(AtomicBool::new(false)),
            };
            self.jira_in_flight = true;
            self.jira_last_jql = Some(jql.clone());
            // Clear both timers now that
            // the search is in flight.
            // `jira_maybe_autocall`
            // early-returns on
            // `jira_in_flight` so the
            // timers are not strictly
            // required to be `None`,
            // but clearing them keeps
            // the bookkeeping
            // consistent and avoids a
            // stale timer firing on
            // the next iteration if
            // the in-flight guard is
            // ever bypassed (e.g. by
            // a future fast-path).
            self.jira_debounce_started = None;
            self.jira_idle_started = None;
            self.process_jira_result(request, result);
            return;
        }
        let Some(config) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            return;
        };
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        let jql_for_thread = jql.clone();
        std::thread::spawn(move || {
            let client = crate::jira::RestJiraClient::new(config);
            let result = client.search(&jql_for_thread);
            if !cancelled_clone.load(Ordering::Relaxed) {
                let _ = tx.send(result);
            }
        });
        self.jira_in_flight = true;
        self.jira_request = Some(JiraRequest {
            receiver: rx,
            cancelled,
        });
        self.jira_last_jql = Some(jql);
        // Same bookkeeping
        // clear as the test-client
        // path above.
        self.jira_debounce_started = None;
        self.jira_idle_started = None;
        self.set_status_message("JIRA searching…".to_string());
    }

    /// Process a JIRA search result that arrived from the
    /// background thread. Converts issues to `HistoryRow`s,
    /// caches them, and refreshes so the list repaints.
    /// Errors surface as a status message (the list keeps
    /// the previous result).
    pub(crate) fn process_jira_result(
        &mut self,
        request: JiraRequest,
        result: Result<Vec<crate::jira::JiraIssue>, crate::jira::JiraError>,
    ) {
        self.jira_in_flight = false;
        self.jira_request = None;
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message("JIRA search cancelled".to_string());
            return;
        }
        match result {
            Ok(issues) => {
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let mut next_id: i64 = -1;
                let rows = issues
                    .into_iter()
                    .map(|issue| {
                        // Build the details-pane text from
                        // the user-spec'd attribute set:
                        // Status, Priority, Due, Assignee,
                        // Description. The new layout is:
                        //
                        //   line 1: **Status**: X   **Priority**: Y
                        //   line 2: **Due**: X       **Assignee**: Y
                        //   line 3: **Description**
                        //   lines 4+: the full description text
                        //
                        // Labels are wrapped in `**...**`
                        // markers that the details-pane
                        // renderer turns into bold spans.
                        // Each field uses `<none>` as a
                        // placeholder when empty, so the
                        // 3-line header layout stays
                        // consistent regardless of which
                        // fields are populated.
                        //
                        // The 4-line preview budget in the
                        // details pane shows the 3 header
                        // breaks as `\n`), so a
                        // multi-paragraph body produces a
                        // multi-line tail in the output.
                        let none_placeholder = "<none>";
                        let status = if issue.status.is_empty() {
                            none_placeholder
                        } else {
                            issue.status.as_str()
                        };
                        let priority = if issue.priority.is_empty() {
                            none_placeholder
                        } else {
                            issue.priority.as_str()
                        };
                        let due = if issue.due.is_empty() {
                            none_placeholder
                        } else {
                            issue.due.as_str()
                        };
                        let assignee = if issue.assignee.is_empty() {
                            none_placeholder
                        } else {
                            issue.assignee.as_str()
                        };
                        // The header block is three
                        // lines, joined by `\n`. Two
                        // spaces between the (label,
                        // value) pairs on lines 1 and 2
                        // give the bold spans a little
                        // breathing room without forcing
                        // a hard column alignment.
                        let mut details: Vec<String> = Vec::new();
                        details.push(format!(
                            "**Status**: {}  **Priority**: {}",
                            status, priority
                        ));
                        details.push(format!("**Due**: {}  **Assignee**: {}", due, assignee));
                        details.push("**Description**".to_string());
                        // The description body
                        // follows on the next line(s).
                        // Empty descriptions get a
                        // single `<none>` placeholder
                        // so the line is always
                        // present and the layout
                        // stays consistent.
                        if !issue.description.is_empty() {
                            // `description` may
                            // contain newlines (the
                            // extractor preserves
                            // paragraph breaks as
                            // `\n`); each one
                            // becomes its own
                            // line in the output.
                            for line in issue.description.lines() {
                                details.push(line.to_string());
                            }
                            // Trailing empty
                            // lines from the
                            // description's
                            // trailing `\n`
                            // are dropped
                            // silently by
                            // `lines()` —
                            // no extra
                            // placeholder
                            // needed.
                        } else {
                            details.push(none_placeholder.to_string());
                        }
                        let ts = crate::jira::updated_to_epoch(&issue.updated);
                        let id = next_id;
                        next_id -= 1;
                        // Map the JIRA workflow status to an exit-code
                        // sentinel so the row's `✓`/`✗` marker reflects
                        // whether the issue is closed or still open.
                        // `Closed` and `To be Reviewed` are treated as
                        // "done" (exit_code = 0 → green ✓); every other
                        // status is "still open" (exit_code = 1 → red ✗).
                        // The comparison is case-insensitive so a JIRA
                        // instance that lowercases its workflow names
                        // (e.g. `closed`) still matches.
                        let status_lower = issue.status.to_ascii_lowercase();
                        let jira_exit_code =
                            if status_lower == "closed" || status_lower == "to be reviewed" {
                                0
                            } else {
                                1
                            };
                        crate::tui::state::HistoryRow {
                            id,
                            command: issue.key,
                            directory: String::new(),
                            session_id: String::new(),
                            exit_code: jira_exit_code,
                            timestamp: if ts > 0 { ts } else { now_epoch },
                            comment: issue.summary,
                            // Newlines between
                            // attributes are the
                            // natural rendering
                            // boundary for the
                            // preview pane.
                            output: details.join("\n"),
                            mode: "jira".to_string(),
                            source: "jira".to_string(),

                            ..Default::default()
                        }
                    })
                    .collect();
                self.jira_rows = rows;
                self.status_message = None;
                self.refresh();
            }
            Err(e) => {
                self.set_status_message(e.to_string());
            }
        }
    }

    /// Install a JIRA client for tests (a fake). When set,
    /// searches run synchronously on the calling thread via
    /// this client instead of spawning a background HTTP
    /// thread, so the search-render path is deterministic.
    #[cfg(test)]
    pub(crate) fn set_jira_client(&mut self, client: std::sync::Arc<dyn crate::jira::JiraClient>) {
        self.jira_client = Some(client);
    }

    /// Install a URL opener for tests (a fake). When set,
    /// `open_jira_in_background` records the would-be `open` /
    /// `xdg-open` invocation through this instead of spawning the
    /// real binary — see `UrlOpener`'s doc comment.
    #[cfg(test)]
    pub(crate) fn set_url_opener(&mut self, opener: std::sync::Arc<dyn UrlOpener>) {
        self.url_opener = Some(opener);
    }

    /// Delete one word backward from the cursor
    /// position. Matches the readline / bash / zsh
    /// `Ctrl-W` semantics:
    ///
    /// 1. **Trailing whitespace first**: if the
    ///    character(s) immediately before the cursor
    ///    are whitespace, eat them.
    /// 2. **Then the preceding word**: walk left
    ///    through the run of non-whitespace
    ///    characters immediately before the cursor
    ///    and eat them too.
    /// 3. **Stop at the start of the buffer** (or at
    ///    cursor position 0, whichever comes first).
    ///
    /// If the cursor is in the middle of a word
    /// (e.g. `git |status` with the cursor between
    /// the space and `status`), only the characters
    /// to the left of the cursor are deleted — the
    /// cursor's position is respected. This matches
    /// the existing `Backspace` behaviour, which
    /// already supports mid-buffer cursor position
    /// via `query_cursor`.
    ///
    /// The function operates on the query field by
    /// default, but routes to the comment-edit
    /// buffer when one is open (so the same shortcut
    /// works in the `EditComment` overlay).
    ///
    /// UTF-8 handling: `query_cursor` and the
    /// buffer's `.chars()` are in characters; we
    /// only convert to a byte index once, at the
    /// moment of the `String::remove` call, via
    /// `char_to_byte_index`. We delete one character
    /// at a time (rather than slicing) so the buffer
    /// stays valid UTF-8 throughout — a multi-byte
    /// character is removed as a single unit.
    /// Tab-completion of a JQL field name at the
    /// current cursor position. The user's example:
    /// typing `lab<TAB>` inside the JIRA query
    /// expands to `labels=` (cursor right after the
    /// `=`). Behaviour:
    ///
    /// 1. Find the field-name token immediately
    ///    before the cursor — the run of
    ///    word-characters (`[A-Za-z0-9_]`) that
    ///    starts at the previous whitespace
    ///    boundary and ends at the cursor.
    /// 2. Call `jira::jira_field_complete_with_value`
    ///    on the prefix.
    /// 3. If the prefix has no matches, do
    ///    nothing (and surface a soft status
    ///    message so the user knows Tab did
    ///    not silently fail).
    /// 4. If the prefix is a complete field name
    ///    (e.g. the user typed `labels<TAB>` and
    ///    the field is already fully typed), append
    ///    a `=` and move the cursor past it. This
    ///    means a second Tab always advances the
    ///    user from "I typed the field" to "I'm
    ///    ready to type the value".
    /// 5. If the prefix is the start of multiple
    ///    fields (e.g. `lab` matches both `label`
    ///    and `labels`), the prefix is extended
    ///    to the LONGEST COMMON PREFIX and the
    ///    cursor lands after it. The user keeps
    ///    typing to disambiguate.
    /// 6. If the prefix matches exactly one
    ///    field, the token is replaced with
    ///    the full field name + `=` and the
    ///    cursor lands right after the `=`.
    ///
    /// The function also handles the edge cases:
    /// - Cursor at the start of the query
    ///   (no prefix): no-op.
    /// - Cursor mid-field (e.g. `lab|els` with
    ///   the cursor between `b` and `e`): the
    ///   prefix is everything from the
    ///   previous whitespace to the cursor
    ///   (`lab`), and the function replaces
    ///   just that prefix. The `els` after the
    ///   cursor is preserved.
    /// - The cursor is in a value position
    ///   (e.g. after `=`, e.g.
    ///   `labels=|foo`): the prefix-to-complete
    ///   is the empty string immediately after
    ///   `=`, which is a no-match case, so the
    ///   function does nothing. (The completion
    ///   is intentionally not bidirectional:
    ///   the field name lives to the LEFT of
    ///   the `=`; the value lives to the RIGHT
    ///   and is left alone. If the user wants
    ///   to edit a value, they can backspace
    ///   and re-type, which is the expected
    ///   readline convention.)
    pub(crate) fn jira_field_complete_at_cursor(&mut self) {
        // The JIRA prefix is `-` by
        // default (a single `char`).
        // The completion operates
        // on the body (the text
        // after the prefix), not
        // on the prefix itself.
        // We also re-check
        // `is_jira_query` here so
        // the function is safe to
        // call from tests and from
        // any future caller — the
        // dispatch site already
        // checks, but defence in
        // depth.
        if !self.is_jira_query() {
            return;
        }
        // `QueryPrefixes::jira` is a
        // single `char`, so the
        // prefix length is always
        // 1.
        let prefix_len: usize = 1;
        // `query_cursor` is in
        // characters and points to
        // the position where the
        // next character would be
        // inserted (i.e. one past
        // the last character if the
        // cursor is at the end).
        if self.query_cursor < prefix_len {
            // Cursor is on or before
            // the JIRA prefix itself
            // (e.g. the user is in
            // the middle of the `-`
            // prefix). There's no
            // field name to
            // complete.
            return;
        }
        // Walk left from the cursor
        // (in character indices),
        // stopping at the first
        // character that is NOT a
        // JQL field-name character
        // (alphanumeric or
        // underscore). The
        // completion target is
        // everything in
        // `self.query[prefix_len..cursor]`
        // back to the start of the
        // current word.
        let mut start_char = self.query_cursor;
        while start_char > prefix_len {
            let prev = start_char - 1;
            let ch = self.query[char_to_byte_index(&self.query, prev)
                ..char_to_byte_index(&self.query, start_char)]
                .chars()
                .next()
                .expect("non-empty slice between char indices");
            if ch == '_' || ch.is_ascii_alphanumeric() {
                start_char = prev;
            } else {
                break;
            }
        }
        let start_byte = char_to_byte_index(&self.query, start_char);
        let cursor_byte = char_to_byte_index(&self.query, self.query_cursor);
        let prefix_str = &self.query[start_byte..cursor_byte];
        if prefix_str.is_empty() {
            // No field-name characters
            // before the cursor —
            // the user pressed Tab
            // right after whitespace
            // or at the start of a
            // value. Surface a
            // status message and
            // bail.
            self.set_status_message("jira-field-complete: no field name to expand".to_string());
            return;
        }
        // Detect `@` alias / fragment
        // completion. If the
        // character immediately
        // before the alphanumeric
        // word is `@`, the user is
        // typing an alias like
        // `@me` or `@today`. We
        // include the `@` in the
        // replacement range and
        // route to the alias
        // completion path (built-in
        // aliases + user-defined
        // fragments from
        // `jira.search.<name>=...`
        // config entries).
        let is_alias = start_char > prefix_len
            && self.query.as_bytes()[char_to_byte_index(&self.query, start_char - 1)] == b'@';
        // For alias completions, the `@`
        // is part of the replacement range.
        let (replace_start_byte, replace_start_char) = if is_alias {
            let at_char = start_char - 1;
            let at_byte = char_to_byte_index(&self.query, at_char);
            (at_byte, at_char)
        } else {
            (start_byte, start_char)
        };
        let (completion, _kind) = if is_alias {
            let alias_prefix = prefix_str; // alphanumeric part after @
            // Check for multiple
            // matches first. If the
            // alias completion is
            // ambiguous, open the
            // completion menu so
            // the user can pick
            // from the candidates
            // rather than just
            // extending to the
            // LCP.
            let matches = crate::jira::jira_alias_matches(alias_prefix, &self.jira_fragments);
            if matches.len() >= 2 {
                // Open the
                // completion
                // menu. The
                // user picks
                // a candidate
                // and the
                // menu
                // applies it
                // (prepending
                // `@` and
                // adding a
                // trailing
                // space).
                self.open_completion_menu(
                    matches,
                    replace_start_byte,
                    cursor_byte,
                    replace_start_char,
                    CompletionKind::JiraAlias,
                );
                return;
            }
            let result = match crate::jira::jira_alias_complete_with_space(
                alias_prefix,
                &self.jira_fragments,
            ) {
                Some(c) => c,
                None => {
                    self.set_status_message(format!(
                        "jira-alias-complete: no alias starts with `{}`",
                        alias_prefix
                    ));
                    return;
                }
            };
            // Include the `@` in the
            // replacement: the result
            // is just the alias name
            // (e.g. `"me "`), so we
            // prepend `@`.
            let mut full = String::from("@");
            full.push_str(&result);
            (full, "alias")
        } else {
            // Check for multiple
            // matches first. If the
            // field completion is
            // ambiguous, open the
            // completion menu so
            // the user can pick
            // from the candidates
            // rather than just
            // extending to the
            // LCP.
            let matches = crate::jira::jira_field_matches(prefix_str);
            if matches.len() >= 2 {
                // Open the
                // completion
                // menu. The
                // user picks
                // a candidate
                // and the
                // menu
                // applies it
                // with a
                // trailing
                // `=`.
                self.open_completion_menu(
                    matches,
                    replace_start_byte,
                    cursor_byte,
                    replace_start_char,
                    CompletionKind::JiraField,
                );
                return;
            }
            let result = match crate::jira::jira_field_complete_with_value(prefix_str) {
                Some(c) => c,
                None => {
                    // No field starts with
                    // the prefix. Don't
                    // silently destroy
                    // text; surface a
                    // status message.
                    self.set_status_message(format!(
                        "jira-field-complete: no JIRA field starts with `{}`",
                        prefix_str
                    ));
                    return;
                }
            };
            (result, "field")
        };
        // Replace the prefix with the
        // completion string and move
        // the cursor to the end.
        self.query
            .replace_range(replace_start_byte..cursor_byte, &completion);
        let completion_chars = completion.chars().count();
        self.query_cursor = replace_start_char + completion_chars;
        // Re-arm the debounce/idle
        // timers so the JIRA
        // search fires on the
        // expanded query.
        // (Same effect as a
        // normal keystroke.)
        self.llm_touch();
        // Recompile the regex (if
        // any) for the new
        // query body.
        self.recompile_regex();
        // Refresh the row set so
        // the search-as-you-type
        // shows the new result
        // count immediately
        // (without waiting for
        // the debounce).
        self.refresh();
        // If the expansion has a
        // trailing `=`, the user
        // is now ready to type
        // the value. A status
        // message like
        // "expanded labels=" is
        // a useful confirmation
        // (and is the same
        // verbosity as the rest
        // of the TUI's status
        // messages). If the
        // expansion is the
        // longest-common-prefix
        // (no trailing `=`), we
        // don't surface a
        // status message
        // because it would
        // flash too often
        // during disambiguation
        // and the user can see
        // the change in the
        // query line anyway.
        if completion.ends_with('=') {
            self.set_status_message(format!("expanded {}", completion));
        }
    }

    /// Process a JIRA add-comment result
    /// that arrived from the background
    /// thread. Mirrors
    /// `process_jira_comments_result`
    /// (the JIRA-comments-fetch
    /// equivalent): on success, clear
    /// the buffer and the JIRA target;
    /// on failure, surface a status
    /// message and preserve the buffer
    /// so the user can retry.
    pub(crate) fn process_jira_add_comment_result(
        &mut self,
        request: JiraAddCommentRequest,
        key: String,
        result: Result<(), crate::jira::JiraError>,
    ) {
        self.jira_add_comment_in_flight = false;
        self.jira_add_comment_request = None;
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message(format!("JIRA add-comment to {} cancelled", key));
            return;
        }
        match result {
            Ok(()) => {
                // Success: clear the
                // buffer and the
                // target. The
                // `comment_edit`
                // and
                // `jira_add_comment_target`
                // fields are
                // reset together
                // so the next
                // Ctrl-E on a
                // non-JIRA row
                // doesn't
                // accidentally
                // re-enter JIRA
                // add-comment
                // mode.
                self.comment_edit = None;
                self.jira_add_comment_target = None;
                self.set_status_message(format!("Comment posted to {}", key));
            }
            Err(e) => {
                // Failure: keep
                // the buffer
                // and the
                // target so
                // the user
                // can
                // retry
                // without
                // retyping.
                self.set_status_message(format!("JIRA add-comment to {} failed: {}", key, e));
            }
        }
    }

    /// Fire a background thread to fetch the
    /// comments for a JIRA issue. Mirrors
    /// `spawn_jira_request` (the search-time
    /// version): a thread runs the HTTP call
    /// against the configured `JiraClient`,
    /// the result flows over an `mpsc`
    /// channel, and the run loop polls it.
    ///
    /// If a comments fetch is already in flight
    /// (`jira_comments_in_flight` is true),
    /// this is a no-op — the user might
    /// press Ctrl+L again on the same row
    /// while a fetch is pending, and we'd
    /// rather silently drop the second
    /// request than spawn a duplicate thread.
    /// The status message is also left
    /// alone so the user doesn't see a
    /// "Loading comments..." flash.
    ///
    /// The fetch is run synchronously against
    /// the fake client in tests, just like
    /// `spawn_jira_request`. The test
    /// seam is `self.jira_client.clone()` —
    /// when set, the search / comments
    /// fetch runs in-line and the result is
    /// processed before this method
    /// returns.
    pub(crate) fn start_jira_comments_fetch(&mut self, key: &str) {
        if self.jira_comments_in_flight {
            return;
        }
        // Snapshot the issue's preview
        // output. The full overlay
        // synthesises its text from this
        // preview + the comments, so we
        // keep a copy on `self` for the
        // processing step. The preview
        // contains the 3-line header
        // (Status/Priority, Due/Assignee,
        // Description label) + the full
        // description body, which is
        // exactly the `# Header` content
        // the user spec calls out.
        let Some(row) = self.jira_rows.iter().find(|r| r.command == key).cloned() else {
            // The row was selected when
            // the user pressed Ctrl+L
            // but it's no longer in
            // `jira_rows` (the user
            // must have navigated and
            // the search cache is now
            // for a different query).
            // Silently do nothing —
            // the row is gone.
            return;
        };
        // The fake-client path runs
        // synchronously and processes
        // the result inline. We still
        // set `jira_comments_in_flight`
        // so a second Ctrl+L on the
        // same row doesn't queue a
        // duplicate.
        if let Some(client) = self.jira_client.clone() {
            let result = client.fetch_comments(key);
            let request = JiraCommentsRequest {
                receiver: mpsc::channel().1,
                cancelled: Arc::new(AtomicBool::new(false)),
                key: key.to_string(),
            };
            self.jira_comments_in_flight = true;
            self.process_jira_comments_result(request, row, result);
            return;
        }
        // Production path: spawn a
        // background thread.
        let Some(config) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            return;
        };
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        let key_for_thread = key.to_string();
        std::thread::spawn(move || {
            let client = crate::jira::RestJiraClient::new(config);
            let result = client.fetch_comments(&key_for_thread);
            if !cancelled_clone.load(Ordering::Relaxed) {
                let _ = tx.send(result);
            }
        });
        self.jira_comments_in_flight = true;
        self.jira_comments_request = Some(JiraCommentsRequest {
            receiver: rx,
            cancelled,
            key: key.to_string(),
        });
        self.set_status_message("JIRA loading comments…".to_string());
    }

    /// Process a JIRA comments-fetch result that
    /// arrived from the background thread.
    /// Builds the markdown-like overlay text
    /// (header, description, comments) and
    /// opens the `OutputView`.
    ///
    /// Mirrors `process_jira_result` (the
    /// search-time equivalent): on success,
    /// cache the comments and refresh; on
    /// error, surface a status message; on
    /// cancellation, surface a different
    /// status message ("JIRA comments
    /// cancelled").
    pub(crate) fn process_jira_comments_result(
        &mut self,
        request: JiraCommentsRequest,
        row: crate::tui::state::HistoryRow,
        result: Result<Vec<crate::jira::JiraComment>, crate::jira::JiraError>,
    ) {
        self.jira_comments_in_flight = false;
        self.jira_comments_request = None;
        if request.cancelled.load(Ordering::Relaxed) {
            self.set_status_message("JIRA comments cancelled".to_string());
            return;
        }
        match result {
            Ok(comments) => {
                // Build the markdown-like
                // overlay text. The
                // structure follows the user
                // spec:
                //
                //   ## Header
                //   <3-line preview: Status/Priority,
                //                   Due/Assignee,
                //                   Description label>
                //   <description body lines>
                //
                //   ## Description
                //   <full description text>
                //
                //   ## Comments
                //   ## <author> · <date>
                //   <comment text>
                //   ## <author> · <date>
                //   <comment text>
                //   ...
                //
                // Comments are sorted
                // newest-first by the
                // `created` timestamp.
                let mut comments = comments;
                sort_comments_newest_first(&mut comments);
                let text = build_jira_overlay_text(&row, &comments);
                self.output_view = Some(OutputView { text, scroll: 0 });
                self.status_message = None;
            }
            Err(e) => {
                self.set_status_message(e.to_string());
            }
        }
    }

    /// Download the currently-selected JIRA issue as a
    /// local markdown note via
    /// `note_search jira-issue <KEY>`.
    ///
    /// The action is intended to be
    /// invoked only from the JIRA
    /// search mode (`-...`) where the
    /// selected row's `command` field
    /// carries the issue key (e.g.
    /// `PROJ-42`). The dispatcher
    /// already gates on
    /// `is_jira_query`; the helper
    /// itself also re-checks the mode
    /// so a stray test or future caller
    /// can't stage the wrong command
    /// from outside JIRA mode.
    ///
    /// The staged command is the bare
    /// `note_search jira-issue <KEY>`
    /// shell line — no path, no flags.
    /// `note_search` writes the
    /// markdown into the `notes.dir`
    /// configured in the same config
    /// file, picking its own filename
    /// from the issue summary. The
    /// TUI exits so the parent shell
    /// runs the command, which in
    /// turn shells out to the
    /// `note_search` binary on `PATH`.
    ///
    /// On any no-op (no row selected,
    /// empty key, not in JIRA mode)
    /// a status message is surfaced
    /// and `selection` stays `None`,
    /// so the TUI remains open and
    /// the user can react to the
    /// feedback.
    pub(crate) fn download_jira_issue(&mut self) {
        // Re-gate here so a stray
        // caller can't stage the
        // wrong command from outside
        // JIRA mode. The dispatcher
        // already gates on this, but
        // the helper defends against
        // future refactors that might
        // call it from a different
        // code path (e.g. a future
        // command-palette entry that
        // calls into the helper
        // directly).
        if !self.is_jira_query() {
            self.set_status_message(
                "Download-JIRA-issue is only available in JIRA search (type `-`)".to_string(),
            );
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            self.set_status_message("No JIRA issue selected".to_string());
            return;
        };
        // The issue key lives in the
        // row's `command` field (the
        // JIRA search stores it
        // there so the column is
        // visible in the row's
        // primary slot). Empty
        // command is a no-op — a
        // freshly-inserted row that
        // hasn't been populated yet
        // could in theory have one,
        // and staging `note_search
        // jira-issue ""` would be a
        // confusing surprise.
        let key = row.command.trim().to_string();
        if key.is_empty() {
            self.set_status_message("Selected JIRA row has no issue key".to_string());
            return;
        }
        // The bare shell line.
        // `shell_quote` keeps the
        // staged command safe even if
        // the key happens to contain
        // shell metacharacters (it
        // shouldn't — JIRA keys are
        // `^[A-Z]+-\d+$` — but
        // defence in depth costs
        // nothing here).
        let staged = format!("note_search jira-issue {}", crate::util::shell_quote(&key));
        self.selection = Some(staged);
        self.pick_mode = Some(PickMode::Run);
    }

    /// Download EVERY JIRA issue matching the current query (not
    /// just the selected row) as local markdown notes, via
    /// `note_search jira <JQL>`. Unlike `download_jira_issue`,
    /// this does NOT loop over `app.jira_rows` — that list is
    /// capped by `JIRA_MAX_RESULTS` (default 5, see
    /// `src/jira.rs`) to keep the live search responsive.
    /// Instead it reuses `jira_build_query()` (the same JQL the
    /// TUI already built for the on-screen results) and lets
    /// `note_search`'s own bulk `jira` subcommand paginate the
    /// JIRA API itself, so the download is complete regardless
    /// of the in-TUI result cap.
    pub(crate) fn download_jira_matching(&mut self) {
        // Re-gate here so a stray caller can't stage the wrong
        // command from outside JIRA mode — same defence-in-depth
        // rationale as `download_jira_issue`.
        if !self.is_jira_query() {
            self.set_status_message(
                "Download-JIRA-matching is only available in JIRA search (type `-`)".to_string(),
            );
            return;
        }
        let jql = self.jira_build_query();
        // Same gate as `jira_maybe_autocall`: an undefined
        // `@fragment` falls through to free text, which would
        // silently download the wrong (much broader) set of
        // issues instead of failing loudly.
        if !self.jira_undefined_fragments.is_empty() {
            self.set_status_message(format!(
                "JIRA fragment{} not configured: {}. \
                 Define via jira.search.<name>=... in \
                 ~/.config/smarthistory/config.",
                if self.jira_undefined_fragments.len() == 1 {
                    ""
                } else {
                    "s"
                },
                self.jira_undefined_fragments
                    .iter()
                    .map(|n| format!("@{}", n))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
            return;
        }
        let staged = format!("note_search jira {}", crate::util::shell_quote(&jql));
        self.selection = Some(staged);
        self.pick_mode = Some(PickMode::Run);
    }

    /// Open the selected JIRA issue's browse URL in the system
    /// browser **in the background** — the same action as pressing
    /// `Enter` on the row (`select_for_run_impl`), but spawned as a
    /// detached child process so the TUI stays open. Used by the
    /// [`Action::SmartOpen`] dive key in `-` (JIRA) mode.
    ///
    /// [`UrlOpener`]: the test-injection seam for this function's
    /// real-process spawn. `App::url_opener` defaults to `None` in
    /// production, in which case the real `open` / `xdg-open` binary
    /// is spawned exactly as before; tests inject a recording stub
    /// via `set_url_opener` (see `src/tui/tests.rs`) so `cargo test`
    /// never actually launches a browser.
    ///
    /// The opener is `open` on macOS and `xdg-open` on other
    /// Unixes (matching `select_for_run_impl`). The process is
    /// spawned on a short-lived thread that calls `.status()`
    /// (blocking wait + reap) so the child doesn't become a
    /// zombie; the TUI never blocks on the browser-launch call.
    pub(crate) fn open_jira_in_background(&mut self) {
        // Same gates as `select_for_run_impl`'s JIRA branch.
        if !self.is_jira_query() {
            self.set_status_message(
                "JIRA open is only available in JIRA search (type `-`)".to_string(),
            );
            return;
        }
        // `smart_action_targets()` returns every marked row when
        // at least one is marked, otherwise just the selected
        // row — this is what lets `Ctrl-]` open ALL marked JIRA
        // issues at once instead of only the highlighted one.
        let targets = self.smart_action_targets();
        if targets.is_empty() {
            self.set_status_message("No JIRA issue selected".to_string());
            return;
        }
        let Some(cfg) = crate::jira::JiraConfig::from_env() else {
            self.set_status_message(crate::jira::JiraError::NotConfigured.to_string());
            return;
        };
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        // One opener process per issue (spawned individually
        // rather than passed as multiple args to a single
        // invocation): `xdg-open` on Linux only reliably opens
        // one URL per call, so looping is the portable choice.
        let injected_opener = self.url_opener.clone();
        let mut opened_keys: Vec<String> = Vec::new();
        for row in &targets {
            let key = row.command.trim().to_string();
            if key.is_empty() {
                continue;
            }
            let url = cfg.browse_url(&key);
            match injected_opener.as_ref() {
                // Test seam: record the call synchronously instead
                // of touching the real process table. See
                // `UrlOpener`'s doc comment.
                Some(injected) => injected.open(opener, &url),
                // Production: spawn a thread that runs the real
                // opener and reaps the child. The TUI thread never
                // blocks on the browser launch.
                None => {
                    let opener = opener.to_string();
                    std::thread::spawn(move || {
                        let _ = std::process::Command::new(&opener).arg(&url).status();
                    });
                }
            }
            opened_keys.push(key);
        }
        if opened_keys.is_empty() {
            self.set_status_message("Selected JIRA row has no issue key".to_string());
            return;
        }
        self.set_status_message(if opened_keys.len() == 1 {
            format!("Opened {} in browser", opened_keys[0])
        } else {
            format!(
                "Opened {} JIRA issues in browser: {}",
                opened_keys.len(),
                opened_keys.join(", ")
            )
        });
    }
}
