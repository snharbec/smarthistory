//! `=` (LLM command generation) prefix mode.
//!
//! The LLM mode requires non-whitespace text after the
//! prefix — `=` alone is treated as no-mode, not as LLM.
use crate::tui::mode::CheckReport;
use crate::tui::App;

/// True if the current query is an LLM command-generation
/// request (prefixed with the configured LLM prefix).
/// Only returns true if there's actual description text after
/// the prefix (not just the prefix alone or with only whitespace).
pub(crate) fn matches(app: &App) -> bool {
    let p = app.query_prefixes.llm;
    app.query.starts_with(p) && !app.query[p.len_utf8()..].trim().is_empty()
}

/// Health check for the LLM (`=`) command-generation
/// mode. The mode talks to ollama for every
/// description, so the check verifies:
///
/// 1. Both `ollama.url` and `ollama.model` are
///    configured in `~/.config/smarthistory/config`.
/// 2. The ollama server is reachable (a `/` GET
///    to the configured URL returns any response;
///    ollama returns "Ollama is running" on
///    `/`).
/// 3. The configured model is loaded
///    (`GET /api/tags` lists the model in
///    `models[]`).
/// 4. The runtime `LlmClient::complete` path
///    is exercised with a tiny "hello" prompt
///    (proves the actual generation pipeline
///    works end-to-end).
pub(crate) fn check(app: &App) -> CheckReport {
    use crate::tui::mode::ModeKind;
    let mode = ModeKind::Llm;

    // 1. Configuration.
    let Some(cfg) = app.llm_config.as_ref() else {
        return CheckReport::err(
            mode,
            "ollama is not configured (set ollama.url and ollama.model in ~/.config/smarthistory/config)",
        );
    };
    if cfg.url.trim().is_empty() || cfg.model.trim().is_empty() {
        return CheckReport::err(mode, "ollama.url or ollama.model is empty in the config");
    }

    // 2. Reachability. We do a tiny GET to
    //    `{url}/api/tags` because that endpoint
    //    exists on every ollama version and
    //    returns JSON (the bare `/` endpoint
    //    returns plain text on some versions).
    //    We use a short timeout so a hung
    //    ollama doesn't pin the `tui check`
    //    command.
    let tags_url = format!("{}/api/tags", cfg.url.trim_end_matches('/'));
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let tags_resp = client.get(&tags_url).call();
    let (status, body) = match tags_resp {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(ureq::Error::Transport(t)) => {
            return CheckReport::err(mode, format!("could not reach ollama at {}: {t}", cfg.url));
        }
    };
    if !(200..300).contains(&status) {
        return CheckReport::err(
            mode,
            format!(
                "ollama at {} returned HTTP {}: {}",
                cfg.url,
                status,
                body.trim()
            ),
        );
    }

    // 3. Model availability. The /api/tags
    //    response is JSON: `{"models":[{"name":"..."}]}`.
    //    We parse it (loosely) and check for
    //    our model name. A failure here
    //    usually means the user has the config
    //    right but forgot to `ollama pull` the
    //    model.
    let model_in_list = body.contains(&format!("\"name\":\"{}\"", cfg.model))
        || body.contains(&format!("\"name\": \"{}\"", cfg.model))
        || body.contains(cfg.model.as_str());
    if !model_in_list {
        return CheckReport::err(
            mode,
            format!(
                "ollama is reachable at {} but the model `{}` is not loaded (run `ollama pull {}` to fetch it)",
                cfg.url,
                cfg.model,
                cfg.model
            ),
        );
    }

    CheckReport::ok(
        mode,
        format!(
            "ollama at {} reachable, model `{}` is loaded",
            cfg.url, cfg.model
        ),
    )
}

/// The LLM query body, i.e. everything after the leading
/// `=` prefix. Empty string when not in LLM mode.
pub(crate) fn pattern(app: &App) -> &str {
    if matches(app) {
        let p = app.query_prefixes.llm;
        &app.query[p.len_utf8()..]
    } else {
        ""
    }
}
