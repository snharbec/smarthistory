//! Rex backend: drives the Rex terminal through its `rex` CLI.
//!
//! Rex's model maps onto the trait like this:
//!
//! | trait            | tmux    | Rex     |
//! |------------------|---------|---------|
//! | session          | session | session |
//! | tab / window     | window  | window  |
//! | pane             | pane    | block   |
//!
//! Every read goes through one `rex do` Lua script ([`SNAPSHOT_LUA`])
//! that walks every session, window and terminal block and asks each
//! block for its `process` (cwd + foreground program). That is one
//! subprocess for the whole snapshot instead of one per block.
//!
//! Switching what the Rex app shows is a *client* action
//! (`session.select`), which needs real session and window ids. The
//! staged commands therefore go through small `smarthistory rex ...`
//! helpers ([`rex_focus_pane`], [`rex_focus_session`], [`rex_new`])
//! that resolve the ids first, the same way herdr's pane focus goes
//! through `smarthistory herdr focus-pane`.

use super::{ActiveContext, CurrentPaneInfo, MultiplexerBackend, MultiplexerKind, MuxSession};
use std::path::Path;

/// Walks every session -> window -> terminal block and prints one JSON
/// object per block, newline-separated. Rows are encoded one by one so
/// an empty result is just an empty string: Lua can't tell an empty
/// array from an empty object, and a whole-table encode would make
/// "no blocks" ambiguous.
///
/// Sessions other than the caller's must be attached before
/// `session.view` answers for them; the attach lasts only as long as
/// this short-lived `rex do` connection.
const SNAPSHOT_LUA: &str = r#"
local rows = {}
local listed = rex.call("session.list", {}) or {}
for _, s in ipairs(listed.sessions or {}) do
  pcall(rex.call, "session.attach", {session_id = s.session_id})
  local view = rex.call("session.view", {session_id = s.session_id})
  if view then
    for _, w in ipairs(view.windows or {}) do
      for _, layer in ipairs(w.layers or {}) do
        for _, b in ipairs(layer.blocks or {}) do
          local ok, p = pcall(rex.call, "block.method",
            {session_id = s.session_id, block_id = b.block_id, method = "process"})
          if ok and p then
            local fg = p.foreground or {}
            local cwd = fg.cwd or ""
            if cwd == "" then
              for _, a in ipairs(p.ancestors or {}) do
                if a.cwd and a.cwd ~= "" then cwd = a.cwd break end
              end
            end
            table.insert(rows, rex.json.encode({
              session_id = s.session_id,
              session_label = s.label or "",
              session_active_window = (view.active_window_id == w.window_id),
              window_id = w.window_id,
              block_id = b.block_id,
              focused = (w.focused_block_id == b.block_id),
              cwd = cwd,
              command = fg.argv0 or fg.name or "",
            }))
          end
        end
      end
    end
  end
end
return table.concat(rows, "\n")
"#;

/// One terminal block, as [`SNAPSHOT_LUA`] reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct RexBlock {
    pub session_id: String,
    #[serde(default)]
    pub session_label: String,
    #[serde(default)]
    pub session_active_window: bool,
    pub window_id: String,
    pub block_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub command: String,
}

/// Parse `rex do`'s output for [`SNAPSHOT_LUA`]: the script returns a
/// string, which `rex do` prints JSON-encoded, so decode that string
/// first, then one JSON object per line. Malformed lines are skipped.
pub(crate) fn parse_snapshot(stdout: &str) -> Vec<RexBlock> {
    let Ok(text) = serde_json::from_str::<String>(stdout.trim()) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<RexBlock>(l).ok())
        .map(|mut b| {
            // A login shell's argv0 is `-zsh`; show it as `zsh`.
            if let Some(rest) = b.command.strip_prefix('-') {
                b.command = rest.to_string();
            }
            b
        })
        .collect()
}

/// Run `rex` with a bounded wait. `None` on spawn failure, timeout or
/// a non-zero exit.
fn rex_run(args: &[&str]) -> Option<String> {
    let timeout_ms: u64 = std::env::var("REX_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let out = super::run_bounded("rex", args, timeout_ms)?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn snapshot_blocks() -> Vec<RexBlock> {
    rex_run(&["do", "-e", SNAPSHOT_LUA])
        .map(|s| parse_snapshot(&s))
        .unwrap_or_default()
}

fn block_to_pane(b: RexBlock) -> CurrentPaneInfo {
    CurrentPaneInfo {
        pane_id: b.block_id,
        // The workspace-row focus target (see `focus_session`): the
        // session id, which `session.select` needs.
        window_id: b.session_id,
        tab_id: b.window_id,
        session_label: b.session_label,
        path: b.cwd,
        current_command: b.command,
        is_last: false,
    }
}

fn block_to_context(b: RexBlock) -> ActiveContext {
    ActiveContext {
        pane_id: b.block_id,
        window_id: b.window_id,
        path: b.cwd,
        current_command: b.command,
        workspace_label: b.session_label,
    }
}

/// Group blocks into sessions; each session's roots are its blocks'
/// cwds (Rex sessions carry no directory of their own, like herdr
/// workspaces).
pub(crate) fn sessions_from_blocks(blocks: &[RexBlock]) -> Vec<MuxSession> {
    let mut out: Vec<MuxSession> = Vec::new();
    for b in blocks {
        let idx = match out.iter().position(|s| s.id == b.session_id) {
            Some(i) => i,
            None => {
                out.push(MuxSession {
                    id: b.session_id.clone(),
                    name: b.session_label.clone(),
                    roots: Vec::new(),
                });
                out.len() - 1
            }
        };
        if !b.cwd.is_empty() && !out[idx].roots.contains(&b.cwd) {
            out[idx].roots.push(b.cwd.clone());
        }
    }
    out
}

pub(crate) struct RexBackend;

impl MultiplexerBackend for RexBackend {
    fn kind(&self) -> Option<MultiplexerKind> {
        Some(MultiplexerKind::Rex)
    }

    fn snapshot(&self) -> Vec<ActiveContext> {
        snapshot_blocks().into_iter().map(block_to_context).collect()
    }

    fn snapshot_current_panes(&self, current_pane: &str) -> Vec<CurrentPaneInfo> {
        super::panes_for_caller(MultiplexerKind::Rex, self.snapshot_all_panes(), current_pane)
    }

    fn snapshot_all_panes(&self) -> Vec<CurrentPaneInfo> {
        snapshot_blocks().into_iter().map(block_to_pane).collect()
    }

    fn focus_command(&self, pane_id: &str) -> Option<String> {
        self.focus_pane(pane_id, "")
    }

    fn focus_session(&self, session: &str) -> Option<String> {
        if session.is_empty() {
            return None;
        }
        Some(format!(
            "smarthistory rex focus-session {}",
            crate::util::shell_quote(session)
        ))
    }

    fn focus_pane(&self, pane_id: &str, _tab_id: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        Some(format!(
            "smarthistory rex focus-pane {}",
            crate::util::shell_quote(pane_id)
        ))
    }

    fn create_command(&self, dir: &Path, label: &str) -> Option<String> {
        let path = crate::util::expand_home_to_absolute(
            &dir.to_string_lossy(),
            &[std::env::var("HOME").unwrap_or_default()],
        )
        .into_owned();
        Some(format!(
            "smarthistory rex new {} {}",
            crate::util::shell_quote(label),
            crate::util::shell_quote(&path)
        ))
    }

    fn list_sessions(&self) -> Option<Vec<MuxSession>> {
        let out = rex_run(&["do", "-e", SNAPSHOT_LUA])?;
        Some(sessions_from_blocks(&parse_snapshot(&out)))
    }

    fn send_in_pane_command(&self, pane_id: &str, body: &str) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        let block = crate::util::shell_quote(pane_id);
        Some(format!(
            "rex send -b {block} {} && rex send-key -b {block} enter",
            crate::util::shell_quote(body)
        ))
    }

    fn read_pane(&self, pane_id: &str, lines: usize) -> Option<String> {
        if pane_id.is_empty() {
            return None;
        }
        let text = rex_run(&["capture", "-b", pane_id, "--trim"])?;
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        Some(all[start..].join("\n"))
    }

    fn name(&self) -> &'static str {
        "rex"
    }
}

/// Run `rex`, returning stdout or an error naming the failed command.
fn rex_checked(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("rex")
        .args(args)
        .output()
        .map_err(|e| format!("could not run rex: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("rex {} failed: {}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Make the Rex app show `session_id` (optionally at `window_id`).
fn select_session(session_id: &str, window_id: Option<&str>) -> Result<(), String> {
    let sid = format!("session_id={session_id}");
    let mut args = vec!["do", "session.select", sid.as_str()];
    let wid;
    if let Some(w) = window_id.filter(|w| !w.is_empty()) {
        wid = format!("window_id={w}");
        args.push(&wid);
    }
    rex_checked(&args).map(|_| ())
}

/// `smarthistory rex focus-pane <block>`: focus the block inside its
/// session, then make the app show that session and window.
pub fn rex_focus_pane(block_id: &str) -> Result<(), String> {
    if block_id.is_empty() {
        return Err("no block id given".to_string());
    }
    let blocks = snapshot_blocks();
    let block = blocks
        .iter()
        .find(|b| b.block_id == block_id)
        .ok_or_else(|| format!("rex block {block_id} not found"))?;
    rex_checked(&["focus", "-s", &block.session_id, block_id])?;
    select_session(&block.session_id, Some(&block.window_id))
}

/// `smarthistory rex focus-session <session>`: show a session, given
/// by id or label.
pub fn rex_focus_session(session: &str) -> Result<(), String> {
    if session.is_empty() {
        return Err("no session given".to_string());
    }
    let id = if session.starts_with("session:") {
        session.to_string()
    } else {
        let listed: serde_json::Value = serde_json::from_str(&rex_checked(&["ls", "--json"])?)
            .map_err(|e| format!("could not parse `rex ls --json`: {e}"))?;
        listed
            .get("sessions")
            .and_then(|s| s.as_array())
            .and_then(|all| {
                all.iter().find(|s| {
                    s.get("label").and_then(|l| l.as_str()) == Some(session)
                })
            })
            .and_then(|s| s.get("session_id")?.as_str().map(str::to_string))
            .ok_or_else(|| format!("rex session {session:?} not found"))?
    };
    select_session(&id, None)
}

/// `smarthistory rex new <label> <dir>`: create a session rooted at
/// `dir` and show it.
pub fn rex_new(label: &str, dir: &str) -> Result<(), String> {
    let out = rex_checked(&["new", label, "-c", dir, "--json"])?;
    let created: serde_json::Value = serde_json::from_str(&out)
        .map_err(|e| format!("could not parse `rex new --json`: {e}"))?;
    let sid = created
        .get("session_id")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "`rex new --json` returned no session_id".to_string())?;
    select_session(sid, None)
}

/// The session holding `$REX_BLOCK` (the terminal this runs in), if
/// any.
fn caller_session() -> Option<String> {
    let block = std::env::var("REX_BLOCK").ok().filter(|b| !b.is_empty())?;
    let out = rex_checked(&["block", "inspect", &block, "--json"]).ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    v.get("session_id")?.as_str().map(str::to_string)
}

/// What `focus_history.reverse` answered, when it moved focus.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reversed {
    pub session_id: String,
    pub window_id: String,
}

/// Parse a `focus_history.reverse` reply. `None` when it didn't move
/// focus (`reversed: false`: nothing left to go back to).
pub(crate) fn parse_reversed(reply: &str) -> Option<Reversed> {
    let v: serde_json::Value = serde_json::from_str(reply.trim()).ok()?;
    if v.get("reversed").and_then(|r| r.as_bool()) != Some(true) {
        return None;
    }
    Some(Reversed {
        session_id: v.get("session_id")?.as_str()?.to_string(),
        window_id: v
            .get("window_id")
            .and_then(|w| w.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// `smarthistory last-session` for Rex: show the session focus was in
/// before the current one. Rex's server keeps a focus history itself
/// (`focus_history.reverse`), so nothing needs recording, and asking
/// it to skip the current session makes a second run toggle back.
///
/// `--print` only names the session, but the server has no way to
/// peek at the history: the call still moves that session's own focus
/// to its last block. The app keeps showing the current session.
pub(crate) fn rex_last_session(print: bool) -> Result<(), String> {
    let args = match caller_session() {
        Some(current) => serde_json::json!({ "not_session_id": current }).to_string(),
        None => "{}".to_string(),
    };
    let reply = rex_checked(&["api", "call", "focus_history.reverse", &args])?;
    let target =
        parse_reversed(&reply).ok_or_else(|| "no previous rex session recorded yet".to_string())?;
    if print {
        let listed: serde_json::Value =
            serde_json::from_str(&rex_checked(&["ls", "--json"])?).unwrap_or_default();
        let label = listed
            .get("sessions")
            .and_then(|s| s.as_array())
            .and_then(|all| {
                all.iter()
                    .find(|s| s.get("session_id").and_then(|i| i.as_str()) == Some(&target.session_id))
            })
            .and_then(|s| s.get("label")?.as_str())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| target.session_id.clone());
        println!("{label}");
        return Ok(());
    }
    select_session(&target.session_id, Some(&target.window_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(session: &str, label: &str, window: &str, id: &str, cwd: &str) -> RexBlock {
        RexBlock {
            session_id: session.into(),
            session_label: label.into(),
            session_active_window: false,
            window_id: window.into(),
            block_id: id.into(),
            focused: false,
            cwd: cwd.into(),
            command: "zsh".into(),
        }
    }

    #[test]
    fn parse_snapshot_decodes_rows() {
        let row = r#"{"session_id":"session:a","session_label":"work","window_id":"window:1","block_id":"block:x","focused":true,"cwd":"/tmp","command":"nvim"}"#;
        let login = r#"{"session_id":"session:a","window_id":"window:1","block_id":"block:y","command":"-zsh"}"#;
        let rows = parse_snapshot(&serde_json::to_string(login).unwrap());
        assert_eq!(rows[0].command, "zsh");
        let stdout = serde_json::to_string(&format!("{row}\n{row}")).unwrap();
        let rows = parse_snapshot(&stdout);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].block_id, "block:x");
        assert_eq!(rows[0].cwd, "/tmp");
        assert_eq!(rows[0].command, "nvim");
        assert!(rows[0].focused);
    }

    #[test]
    fn parse_reversed_reads_target_or_nothing() {
        let moved = r#"{"reversed":true,"cursor":26,"session_id":"session:a","window_id":"window:1","block_id":"block:x"}"#;
        assert_eq!(
            parse_reversed(moved),
            Some(Reversed { session_id: "session:a".into(), window_id: "window:1".into() })
        );
        assert_eq!(parse_reversed(r#"{"reversed":false}"#), None);
        assert_eq!(parse_reversed("garbage"), None);
    }

    #[test]
    fn config_value_rex_selects_the_rex_backend() {
        assert_eq!(MultiplexerKind::parse(" Rex "), Some(MultiplexerKind::Rex));
        assert_eq!(MultiplexerKind::Rex.as_str(), "rex");
        assert_eq!(super::super::backend_for(MultiplexerKind::Rex).name(), "rex");
    }

    #[test]
    fn parse_snapshot_tolerates_empty_and_garbage() {
        assert!(parse_snapshot("\"\"").is_empty());
        assert!(parse_snapshot("").is_empty());
        assert!(parse_snapshot("not json").is_empty());
        let stdout = serde_json::to_string("garbage\n{\"nope\":1}").unwrap();
        assert!(parse_snapshot(&stdout).is_empty());
    }

    #[test]
    fn block_maps_to_pane_with_session_as_focus_target() {
        let p = block_to_pane(block("session:a", "work", "window:1", "block:x", "/tmp"));
        assert_eq!(p.pane_id, "block:x");
        assert_eq!(p.window_id, "session:a");
        assert_eq!(p.tab_id, "window:1");
        assert_eq!(p.session_label, "work");
        assert_eq!(p.path, "/tmp");
    }

    #[test]
    fn sessions_group_blocks_and_dedupe_roots() {
        let blocks = vec![
            block("session:a", "work", "window:1", "block:1", "/src"),
            block("session:b", "play", "window:2", "block:2", "/tmp"),
            block("session:a", "work", "window:1", "block:3", "/src"),
            block("session:a", "work", "window:3", "block:4", "/etc"),
        ];
        let s = sessions_from_blocks(&blocks);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].id, "session:a");
        assert_eq!(s[0].name, "work");
        assert_eq!(s[0].roots, ["/src", "/etc"]);
        assert_eq!(s[1].roots, ["/tmp"]);
    }

    #[test]
    fn staged_commands_quote_their_arguments() {
        let b = RexBackend;
        assert_eq!(
            b.focus_pane("block:x", "").unwrap(),
            "smarthistory rex focus-pane block:x"
        );
        assert!(b.focus_pane("", "").is_none());
        assert_eq!(
            b.focus_session("session:a").unwrap(),
            "smarthistory rex focus-session session:a"
        );
        assert!(b.focus_session("").is_none());
        let create = b
            .create_command(Path::new("/var/tmp/My Work"), "work")
            .unwrap();
        assert_eq!(create, "smarthistory rex new work '/var/tmp/My Work'");
        let send = b.send_in_pane_command("block:x", "sh .command /tmp").unwrap();
        assert_eq!(
            send,
            "rex send -b block:x 'sh .command /tmp' && rex send-key -b block:x enter"
        );
    }
}
