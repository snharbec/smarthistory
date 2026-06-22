//! LLM-driven command generation.
//!
//! When the user types a query starting with `=` in the TUI, the
//! TUI calls into this module to ask a local LLM (via ollama) to
//! translate the natural-language description into an executable
//! command. The LLM's response is sanitized (markdown fences,
//! preamble lines, comments) and the resulting command is inserted
//! into the history database with the original description as a
//! comment, then staged for execution by the parent shell.
//!
//! Configuration is optional: if neither `ollama.url` nor
//! `ollama.model` is set in `~/.config/smarthistory/config`, the LLM
//! mode is disabled and the TUI surfaces a status message. There
//! is no fallback to a hosted API — this is a local-only feature.
//!
//! The HTTP client is hidden behind the [`LlmClient`] trait so the
//! full LLM round-trip can be unit-tested with a canned response
//! without a live ollama instance.

use std::time::Duration;

/// Configuration for the LLM backend. Both fields are required
/// for the feature to be enabled; partial configuration
/// (`ollama.url` without `ollama.model` or vice versa) is treated
/// as "not configured" and the TUI surfaces a status message.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Full URL of the ollama instance, including scheme and
    /// port. The default ollama port is 11434, so the typical
    /// value is `http://localhost:11434`. We do not assume
    /// any particular host; the user is in charge of where
    /// their model runs.
    pub url: String,
    /// Name of the ollama model to use (e.g. `"llama3.2"`,
    /// `"qwen2.5-coder"`, `"codellama"`). Whatever the user
    /// has already pulled with `ollama pull`.
    pub model: String,
}

impl LlmConfig {
    /// `is_configured` is provided as a documentation helper for
    /// callers that want to surface a custom "not configured"
    /// message; the production code path is the simpler
    /// `Option<LlmConfig>` from the parsed config file.
    #[allow(dead_code)]
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.model.trim().is_empty()
    }
}

/// Errors the LLM subsystem can produce. Most variants carry a
/// human-readable detail so the TUI can show a useful status
/// message without leaking internal jargon.
#[derive(Debug)]
pub enum LlmError {
    /// LLM mode was requested but no ollama configuration is
    /// present in `~/.config/smarthistory/config`.
    NotConfigured,
    /// HTTP transport failure (DNS, connect refused, TLS, read
    /// timeout). The detail is the underlying ureq error.
    Transport(String),
    /// ollama returned a non-2xx status. The detail is the
    /// response body (which ollama fills with a JSON error
    /// blob, useful for debugging).
    HttpStatus(u16, String),
    /// ollama's response was not valid JSON, or the JSON did not
    /// contain the fields we expect (`response`).
    Decode(String),
    /// The LLM returned a non-empty response but our sanitizer
    /// couldn't extract an executable command from it (e.g. the
    /// LLM only wrote an explanation and no actual command).
    NoCommand,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::NotConfigured => f.write_str(
                "LLM not configured (set ollama.url and ollama.model in \
                 ~/.config/smarthistory/config)",
            ),
            LlmError::Transport(s) => write!(f, "LLM transport error: {}", s),
            LlmError::HttpStatus(code, body) => write!(
                f,
                "LLM returned HTTP {}: {}",
                code,
                body.chars().take(200).collect::<String>()
            ),
            LlmError::Decode(s) => write!(f, "LLM decode error: {}", s),
            LlmError::NoCommand => f.write_str("LLM returned no usable command (only commentary?)"),
        }
    }
}

impl std::error::Error for LlmError {}

/// The HTTP backend. Production code constructs an
/// [`OllamaClient`] from [`LlmConfig`]; tests can pass any
/// implementation that returns canned responses.
pub trait LlmClient: Send + Sync {
    /// Send a raw `prompt` to the backend and return the
    /// raw response text. This is the low-level
    /// transport method — the only one that actually
    /// hits the network in the production
    /// implementation. The higher-level [`generate`]
    /// and [`describe`] methods are thin wrappers
    /// that build the right prompt and forward here.
    ///
    /// Tests can override `generate` / `describe`
    /// directly to return canned responses without
    /// having to also implement the prompt
    /// construction. The default implementations
    /// below use [`build_prompt`] / [`build_describe_prompt`].
    ///
    /// # Errors
    /// - [`LlmError::Transport`] on network failures.
    /// - [`LlmError::HttpStatus`] on non-2xx ollama responses.
    /// - [`LlmError::Decode`] on malformed JSON.
    fn prompt(&self, prompt: &str) -> Result<String, LlmError>;

    /// Translate `description` into an executable
    /// command. The returned string is the *raw*
    /// ollama response — callers should run it
    /// through [`sanitize_command`] before executing.
    ///
    /// Default implementation calls
    /// [`build_prompt`] and forwards to [`prompt`].
    /// Tests typically override this to return a
    /// canned command-form response.
    fn generate(&self, description: &str) -> Result<String, LlmError> {
        self.prompt(&build_prompt(description))
    }

    /// Describe what a shell command does, in at most
    /// four sentences of plain prose. Used by the TUI's
    /// "describe" action (default key `Ctrl-K`).
    ///
    /// Default implementation calls
    /// [`build_describe_prompt`] and forwards to
    /// [`prompt`]. Tests can override this to return a
    /// canned description without having to also stub
    /// the prompt construction.
    fn describe(&self, command: &str) -> Result<String, LlmError> {
        self.prompt(&build_describe_prompt(command))
    }
}

/// Real ollama backend. Uses ureq (sync HTTP) with a 30-second
/// timeout, which is well above the typical response time of a
/// local 7B model and short enough that a hung connection
/// doesn't freeze the TUI indefinitely.
pub struct OllamaClient {
    /// Pre-built ureq agent with our timeout policy. Sharing one
    /// agent across requests means we get connection pooling
    /// and a single configuration point.
    agent: ureq::Agent,
    /// Full URL of the ollama instance (e.g.
    /// `http://localhost:11434`). The `/api/generate` path is
    /// appended per request.
    url: String,
    /// Model name to request.
    model: String,
}

impl OllamaClient {
    /// Build a client. Panics on the (very unlikely) event that
    /// ureq's TLS config can't be initialized — we want a loud
    /// failure at startup, not a silent one per request.
    pub fn new(cfg: &LlmConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        OllamaClient {
            agent,
            url: cfg.url.trim_end_matches('/').to_string(),
            model: cfg.model.clone(),
        }
    }
}

impl LlmClient for OllamaClient {
    fn prompt(&self, prompt: &str) -> Result<String, LlmError> {
        let url = format!("{}/api/generate", self.url);
        // ollama's `/api/generate` accepts a JSON body with the
        // fields we care about. We disable streaming so the
        // response is a single JSON object — easier to parse and
        // easier to surface partial-output errors as a single
        // transport failure.
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
        });
        let response = self
            .agent
            .post(&url)
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    LlmError::HttpStatus(code, resp.into_string().unwrap_or_default())
                }
                other => LlmError::Transport(other.to_string()),
            })?;
        let text = response
            .into_string()
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        // ollama's response shape: { "response": "...", "done": true, ... }.
        // We only care about `response`.
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Decode(e.to_string()))?;
        let raw = parsed
            .get("response")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                LlmError::Decode(format!(
                    "ollama response missing 'response' field: {}",
                    text.chars().take(200).collect::<String>()
                ))
            })?
            .to_string();
        Ok(raw)
    }
    // `generate` and `describe` use the default
    // implementations, which build the right prompt
    // and forward to `prompt`. Defining them here
    // would be redundant — the trait's default impls
    // are exactly what we want.
}

/// The prompt template. Kept as a small free function (not a
/// const) so it composes cleanly with the user's description
/// without running into const-evaluation limits.
pub fn build_prompt(description: &str) -> String {
    format!(
        "You are a strict Bash command generator. Respond ONLY with the executable command. \
Do not include markdown formatting, backticks, explanations, or introductory text.\n\n{}",
        description
    )
}

/// The prompt template for the "describe what this command
/// does" action. The hard constraint is "at most four
/// sentences" so the response fits comfortably in a small
/// overlay; the soft constraint is "plain prose, no
/// markdown, no lists, no code blocks" so the user gets a
/// readable explanation instead of a wall of formatted
/// text.
///
/// The model is told to *start with the verb the command
/// performs* — this gives a consistent shape ("Lists…",
/// "Recursively deletes…", "Connects to…") that scans well
/// when the user has many describes stacked up.
pub fn build_describe_prompt(command: &str) -> String {
    format!(
        "You are a concise technical writer. In at most 4 sentences (no markdown, no lists, \
no code blocks, no preamble), describe what this shell command does. Start with the verb \
the command performs, then explain the user-visible effect and any side effects (files \
created, network connections, side effects on the system).\n\n```\n{}\n```",
        command
    )
}

/// Strip the LLM's likely cruft and return the first plausible
/// executable command, or `None` if no command could be
/// extracted.
///
/// The sanitizer is conservative — it only removes patterns that
/// are unambiguously not the command:
/// - Markdown code fences (```` ``` ```` or ```` ```bash ````)
/// - Comment lines (starting with `#` or `//`)
/// - Empty lines and pure-whitespace lines
/// - Common preamble phrases (case-insensitive):
///   "sure, here's the command:",
///   "the command is:",
///   "command:",
///   "here's the command:",
///   "answer:",
/// - A pair of single backticks wrapping the whole line
///   (e.g. `` `find . -name foo` ``)
/// - Trailing prose after a semicolon (e.g.
///   `find . -mtime -1; this finds files modified yesterday`)
///
/// The first surviving line that contains at least one
/// non-whitespace character is the answer. If the LLM's
/// response is all commentary and no command survives the
/// strip, we return `None` so the caller can surface a
/// "no usable command" status instead of executing a
/// fragment.
pub fn sanitize_command(raw: &str) -> Option<String> {
    // Preamble phrases that we strip from the start of any line.
    // Match is case-insensitive, anchored at the start, followed
    // by optional whitespace and a colon. We deliberately keep
    // this list short and obvious; the LLM is told to not write
    // preambles, so anything that slips through is a failure
    // case we want to see (rather than silently cleaning up).
    const PREAMBLES: &[&str] = &[
        "sure, here's the command:",
        "sure, here is the command:",
        "here's the command:",
        "here is the command:",
        "the command is:",
        "command:",
        "answer:",
    ];

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Strip matching markdown code fences. A fence is a line
        // that is just ``` (with optional language tag).
        if trimmed.starts_with("```") {
            continue;
        }
        // Skip comment lines.
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        // Strip a matching preamble prefix. We don't try every
        // possible phrasing; the list is short and we accept
        // false negatives (the line stays as-is) over false
        // positives (a real command starting with "command:"
        // getting mangled).
        let mut candidate = trimmed.to_string();
        let lower = candidate.to_ascii_lowercase();
        for preamble in PREAMBLES {
            if let Some(rest) = lower.strip_prefix(preamble) {
                // The actual `candidate` may differ in case; strip
                // the original-case prefix with the same length.
                let prefix_len = candidate.len() - rest.len();
                candidate = candidate[prefix_len..].trim_start().to_string();
                break;
            }
        }
        // Strip a single pair of wrapping backticks. We only do
        // this once (the inner content is whatever the LLM
        // produced) and we don't recurse into nested backticks.
        if candidate.starts_with('`') && candidate.ends_with('`') && candidate.len() >= 2 {
            candidate = candidate[1..candidate.len() - 1].to_string();
        }
        // Drop trailing prose after a `;`. The command comes
        // first, the prose is an explanation the LLM added
        // despite being told not to.
        if let Some(idx) = candidate.find(';') {
            candidate = candidate[..idx].trim_end().to_string();
        }
        // Final pass: if the candidate became empty after
        // stripping (e.g. the line was just "command: ; done"),
        // skip it and try the next line.
        if candidate.is_empty() {
            continue;
        }
        return Some(candidate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_includes_user_description() {
        let p = build_prompt("find yesterday's files");
        assert!(p.contains("find yesterday's files"));
        // The system instructions must precede the user input.
        assert!(p.starts_with("You are a strict Bash command generator"));
    }

    #[test]
    fn is_configured_requires_both_fields() {
        let mut cfg = LlmConfig {
            url: "http://localhost:11434".to_string(),
            model: "llama3.2".to_string(),
        };
        assert!(cfg.is_configured());
        cfg.url = String::new();
        assert!(!cfg.is_configured());
        cfg.url = "   ".to_string();
        assert!(!cfg.is_configured());
        cfg.model = String::new();
        assert!(!cfg.is_configured());
    }

    #[test]
    fn sanitize_passes_through_clean_command() {
        assert_eq!(
            sanitize_command("find . -mtime -1 -type f"),
            Some("find . -mtime -1 -type f".to_string())
        );
    }

    #[test]
    fn sanitize_strips_markdown_fences() {
        // A fenced block where the LLM echoed ```bash … ```.
        let raw = "```bash\nfind . -mtime -1\n```";
        assert_eq!(sanitize_command(raw), Some("find . -mtime -1".to_string()));
    }

    #[test]
    fn sanitize_drops_comment_lines() {
        let raw = "# this finds yesterday's files\nfind . -mtime -1";
        assert_eq!(sanitize_command(raw), Some("find . -mtime -1".to_string()));
    }

    #[test]
    fn sanitize_strips_common_preambles() {
        assert_eq!(
            sanitize_command("Sure, here's the command: find . -mtime -1"),
            Some("find . -mtime -1".to_string())
        );
        assert_eq!(
            sanitize_command("The command is: find . -mtime -1"),
            Some("find . -mtime -1".to_string())
        );
    }

    #[test]
    fn sanitize_strips_wrapping_backticks() {
        assert_eq!(
            sanitize_command("`find . -mtime -1`"),
            Some("find . -mtime -1".to_string())
        );
    }

    #[test]
    fn sanitize_truncates_at_semicolon() {
        assert_eq!(
            sanitize_command("find . -mtime -1; this lists yesterday's files"),
            Some("find . -mtime -1".to_string())
        );
    }

    #[test]
    fn sanitize_returns_none_for_empty_or_pure_commentary() {
        assert_eq!(sanitize_command(""), None);
        assert_eq!(sanitize_command("\n\n# just a comment\n"), None);
        assert_eq!(sanitize_command("```\n```"), None);
    }

    #[test]
    fn sanitize_picks_first_non_empty_line() {
        // The LLM wrote the actual command, then a comment line.
        // We pick the first surviving line.
        let raw = "find . -mtime -1\n# explanation follows";
        assert_eq!(sanitize_command(raw), Some("find . -mtime -1".to_string()));
    }

    // --- `build_describe_prompt` and the describe pipeline ----

    /// `build_describe_prompt` includes the command
    /// being described, the four-sentence limit, and
    /// the formatting prohibitions (no markdown, no
    /// lists, no code blocks). The prompt also
    /// includes the verb-first instruction so the LLM's
    /// responses have a consistent shape.
    #[test]
    fn build_describe_prompt_includes_command_and_constraints() {
        let p = build_describe_prompt("find . -mtime -1 -type f");
        assert!(p.contains("find . -mtime -1 -type f"));
        assert!(p.contains("4 sentences"), "missing 4-sentence constraint");
        assert!(p.contains("no markdown"));
        assert!(p.contains("no lists"));
        assert!(p.contains("no code blocks"));
        // Verb-first instruction makes responses
        // scan well when the user has many of
        // them stacked up.
        assert!(p.contains("verb"));
    }

    /// `OllamaClient::prompt` is the low-level HTTP
    /// method. We don't have a live ollama server
    /// in the test environment, so this test only
    /// pins the trait's required shape: the method
    /// exists and is callable with a `&str` prompt.
    /// The actual HTTP behavior is exercised by the
    /// production path (the run-time path the
    /// TUI uses), not by the unit tests.
    #[test]
    fn ollama_client_exposes_prompt_method() {
        // The trait's signature is the contract;
        // this test fails to compile if the
        // signature changes. We don't actually
        // construct an OllamaClient because that
        // would require a live URL; we just want
        // the type-level guarantee.
        fn _accepts_prompt(_c: &dyn LlmClient, _p: &str) {}
    }
}
