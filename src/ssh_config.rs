//! `~/.ssh/config` parser.
//!
//! Minimal OpenSSH config parser tailored to the
//! subset smarthistory's `# hosts` view needs.
//! Recognised keywords:
//!
//! - `Host` — the alias that keys the block.
//!   Wildcard patterns (`*`, `?`) are ignored
//!   for the purpose of building the host list
//!   because the user-visible UI wants explicit
//!   aliases; a `Host *` defaults block is still
//!   applied as defaults to subsequent explicit
//!   blocks (matching OpenSSH's own behaviour).
//! - `HostName` — the real hostname to connect
//!   to. Overrides the alias as the SSH target.
//! - `User` — the login user.
//! - `Port` — TCP port (parsed as `u16`).
//! - `IdentityFile` — path to the private key
//!   (the first one wins; OpenSSH accepts
//!   multiple).
//!
//! Unrecognised keywords are silently ignored.
//! `Match` blocks are not supported (smarthistory
//! is single-user and the `~/.ssh/config` files
//! it sees in practice don't use them).
//!
//! The parser is permissive about indentation and
//! case (`Host` and `host` are the same keyword),
//! matching OpenSSH's own behaviour. Comments
//! starting with `#` and blank lines are skipped.
//!
//! On any I/O error (the file doesn't exist, no
//! permission, etc.) the parser returns an empty
//! `Vec` rather than an error: a missing
//! `~/.ssh/config` is the common case on a fresh
//! machine and shouldn't break startup.
//!
//! ## Example
//!
//! ```text
//! Host proxmox
//!     HostName pve-1.example.com
//!     User root
//!     IdentityFile ~/.ssh/id_ed25519
//! ```
//!
//! parses to a single
//! [`SshHostBlock`] with `alias = "proxmox"`,
//! `hostname = "pve-1.example.com"`, `user =
//! "root"`, `identity = "~/.ssh/id_ed25519"`.

use std::path::Path;

/// One parsed `Host` block from `~/.ssh/config`.
///
/// The `alias` is the key (the value on the
/// `Host` line). For wildcard patterns
/// (`Host *`) the alias is `"*"` and the block
/// is treated as a defaults block — its values
/// are inherited by subsequent explicit blocks
/// unless they override them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshHostBlock {
    /// The `Host` alias (e.g. `proxmox`, `bmlv`,
    /// `*`). Empty when the block was constructed
    /// as a defaults-only block.
    pub alias: String,
    /// The real hostname (from `HostName`).
    /// Empty when the alias is to be used as
    /// the connection target verbatim.
    pub hostname: String,
    /// The login user (from `User`).
    pub user: String,
    /// The TCP port (from `Port`). 0 means
    /// "unspecified" (the SSH default 22 should
    /// be applied at connect time).
    pub port: u16,
    /// The first `IdentityFile` (from
    /// `IdentityFile`). Empty when unset.
    pub identity: String,
}

/// Read and parse `~/.ssh/config`.
///
/// Returns an empty `Vec` on any I/O error
/// (missing file, permission denied, etc.) so a
/// machine without an SSH config still gets a
/// working (empty) hosts list. Parse errors
/// mid-file are tolerated: the lines that did
/// parse are returned and the rest is skipped.
/// Strict mode is not useful here because the
/// SSH config syntax is loose (OpenSSH itself
/// ignores unknown keywords).
pub fn load_ssh_config(home: &Path) -> Vec<SshHostBlock> {
    let path = home.join(".ssh").join("config");
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    parse(&contents)
}

/// Parse a `~/.ssh/config`-formatted string.
///
/// Public for testing — production callers should
/// use [`load_ssh_config`] which handles the
/// `~/.ssh/config` path resolution and I/O
/// error swallowing.
pub fn parse(contents: &str) -> Vec<SshHostBlock> {
    let mut out: Vec<SshHostBlock> = Vec::new();
    let mut current: Option<SshHostBlock> = None;
    // Defaults from the most recent
    // `Host *` block (and any other
    // wildcard pattern that the SSH
    // grammar allows, but for our
    // purposes the only useful
    // wildcard is `*`).
    let mut defaults: SshHostBlock = SshHostBlock::default();

    for raw_line in contents.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        // Tokens are whitespace-separated;
        // values containing whitespace are
        // not supported by our subset (we
        // don't need to handle them — every
        // field we read is either an alias,
        // a hostname, a user, a port number,
        // or a path, none of which contain
        // spaces in practice).
        let mut tokens = line.split_whitespace();
        let keyword = match tokens.next() {
            Some(k) => k,
            None => continue,
        };
        // Case-insensitive keyword
        // matching. `Host` and `host`
        // are the same in OpenSSH's
        // grammar.
        let keyword_lower = keyword.to_ascii_lowercase();
        match keyword_lower.as_str() {
            "host" => {
                // `Host` may take multiple
                // aliases (space-separated);
                // we emit one block per
                // alias. Wildcard aliases
                // become a defaults block
                // rather than a host
                // entry.
                if let Some(block) = current.take() {
                    if is_wildcard_alias(&block.alias) {
                        merge_into(&mut defaults, &block);
                    } else {
                        merge_into_defaults(&mut out, block, &defaults);
                    }
                }
                let aliases: Vec<String> = tokens.map(|s| s.to_string()).collect();
                if aliases.is_empty() {
                    // No alias on this Host
                    // line — OpenSSH would
                    // treat it as an error,
                    // but we just skip it.
                    continue;
                }
                for alias in aliases {
                    if is_wildcard_alias(&alias) {
                        // `Host *` etc. is
                        // a defaults block
                        // for subsequent
                        // explicit blocks.
                        // We allocate a
                        // temporary block
                        // with just the
                        // alias set; the
                        // rest of the
                        // keywords will
                        // populate it as
                        // we see them.
                        current = Some(SshHostBlock {
                            alias,
                            ..SshHostBlock::default()
                        });
                    } else {
                        // First explicit
                        // alias in a
                        // `Host a b c`
                        // line opens the
                        // current block;
                        // subsequent
                        // aliases open
                        // duplicate
                        // blocks (we
                        // emit one per
                        // alias at the
                        // end of the
                        // block by
                        // re-running the
                        // emit logic).
                        // For simplicity
                        // (and because
                        // `Host a b`
                        // with shared
                        // options is
                        // rare in
                        // practice), we
                        // only emit the
                        // first alias
                        // here and
                        // ignore trailing
                        // aliases — the
                        // user can split
                        // them into
                        // separate `Host`
                        // blocks.
                        current = Some(SshHostBlock {
                            alias,
                            ..SshHostBlock::default()
                        });
                        break;
                    }
                }
            }
            "hostname" => {
                if let Some(ref mut block) = current
                    && let Some(v) = tokens.next()
                {
                    block.hostname = v.to_string();
                }
            }
            "user" => {
                if let Some(ref mut block) = current
                    && let Some(v) = tokens.next()
                {
                    block.user = v.to_string();
                }
            }
            "port" => {
                if let Some(ref mut block) = current
                    && let Some(v) = tokens.next()
                    && let Ok(n) = v.parse::<u16>()
                {
                    block.port = n;
                }
            }
            "identityfile" => {
                if let Some(ref mut block) = current
                    && block.identity.is_empty()
                {
                    // The first
                    // `IdentityFile`
                    // wins; OpenSSH
                    // accepts
                    // multiple and
                    // tries them in
                    // order. We
                    // mirror that
                    // by keeping the
                    // first one
                    // only.
                    if let Some(v) = tokens.next() {
                        block.identity = v.to_string();
                    }
                }
            }
            _ => {
                // Unrecognised keyword
                // (or `Match`, which
                // we don't support) —
                // silently skip.
            }
        }
    }
    // Flush the last block, if any.
    if let Some(block) = current.take() {
        if is_wildcard_alias(&block.alias) {
            // Stray wildcard block at
            // EOF — merge into defaults
            // for completeness.
            merge_into(&mut defaults, &block);
        } else {
            merge_into_defaults(&mut out, block, &defaults);
        }
    }
    out
}

/// Apply the `defaults` block's set
/// fields to `block`'s unset fields,
/// then push the block.
///
/// The `defaults` block is the most
/// recent `Host *` block (or another
/// wildcard pattern). SSH applies it
/// before the explicit block, so any
/// field that the explicit block
/// leaves empty inherits from the
/// defaults.
fn merge_into_defaults(
    out: &mut Vec<SshHostBlock>,
    mut block: SshHostBlock,
    defaults: &SshHostBlock,
) {
    if block.hostname.is_empty() {
        block.hostname = defaults.hostname.clone();
    }
    if block.user.is_empty() {
        block.user = defaults.user.clone();
    }
    if block.port == 0 {
        block.port = defaults.port;
    }
    if block.identity.is_empty() {
        block.identity = defaults.identity.clone();
    }
    out.push(block);
}

/// Merge `src` into `dst` keeping
/// the set fields. Used for
/// `Host *` blocks themselves —
/// the most recent wildcard block
/// wins for any field it sets.
fn merge_into(dst: &mut SshHostBlock, src: &SshHostBlock) {
    if !src.hostname.is_empty() {
        dst.hostname = src.hostname.clone();
    }
    if !src.user.is_empty() {
        dst.user = src.user.clone();
    }
    if src.port != 0 {
        dst.port = src.port;
    }
    if !src.identity.is_empty() {
        dst.identity = src.identity.clone();
    }
}

fn strip_comment(line: &str) -> &str {
    // OpenSSH treats `#` as a
    // comment delimiter even mid-line
    // (when preceded by whitespace or
    // at the start of a token). We
    // match that by looking for the
    // first `#` that's either at the
    // start of the line or preceded
    // by whitespace. This means an
    // `IdentityFile` path containing
    // `#` is safe.
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return &line[..i];
        }
    }
    line
}

fn is_wildcard_alias(alias: &str) -> bool {
    // OpenSSH accepts `*` and `?`
    // as wildcards. We treat any
    // alias containing `*` or `?`
    // as a pattern.
    alias.contains('*') || alias.contains('?')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_vec() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn blank_lines_and_comments_are_skipped() {
        let input = "\n# a comment\n\n   \n# another\n";
        assert!(parse(input).is_empty());
    }

    #[test]
    fn single_host_with_all_fields() {
        let input = "\
Host proxmox
    HostName pve-1.example.com
    User root
    Port 2222
    IdentityFile ~/.ssh/id_ed25519
";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].alias, "proxmox");
        assert_eq!(blocks[0].hostname, "pve-1.example.com");
        assert_eq!(blocks[0].user, "root");
        assert_eq!(blocks[0].port, 2222);
        assert_eq!(blocks[0].identity, "~/.ssh/id_ed25519");
    }

    #[test]
    fn host_with_only_alias_keeps_empty_optional_fields() {
        let input = "Host bmlv\n";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].alias, "bmlv");
        assert_eq!(blocks[0].hostname, "");
        assert_eq!(blocks[0].user, "");
        assert_eq!(blocks[0].port, 0);
        assert_eq!(blocks[0].identity, "");
    }

    #[test]
    fn case_insensitive_keywords() {
        let input = "\
host proxmox
    hostname pve-1
    USER root
    port 2222
    identityfile ~/.ssh/k
";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].alias, "proxmox");
        assert_eq!(blocks[0].hostname, "pve-1");
        assert_eq!(blocks[0].user, "root");
        assert_eq!(blocks[0].port, 2222);
        assert_eq!(blocks[0].identity, "~/.ssh/k");
    }

    #[test]
    fn host_star_block_supplies_defaults() {
        let input = "\
Host *
    User har
    IdentityFile ~/.ssh/id_ed25519
Host proxmox
    HostName pve-1
";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].alias, "proxmox");
        assert_eq!(blocks[0].hostname, "pve-1");
        // Inherited from `Host *`:
        assert_eq!(blocks[0].user, "har");
        assert_eq!(blocks[0].identity, "~/.ssh/id_ed25519");
    }

    #[test]
    fn explicit_block_overrides_defaults() {
        let input = "\
Host *
    User har
Host proxmox
    User root
";
        let blocks = parse(input);
        assert_eq!(
            blocks[0].user, "root",
            "explicit block must win over defaults"
        );
    }

    #[test]
    fn first_identityfile_wins() {
        let input = "\
Host proxmox
    IdentityFile ~/.ssh/first
    IdentityFile ~/.ssh/second
";
        let blocks = parse(input);
        assert_eq!(blocks[0].identity, "~/.ssh/first");
    }

    #[test]
    fn unknown_keywords_are_skipped() {
        let input = "\
Host proxmox
    HostName pve-1
    ForwardAgent yes
    User root
";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].user, "root");
    }

    #[test]
    fn multiple_hosts_in_sequence() {
        let input = "\
Host a
    HostName a.example
    User alice
Host b
    HostName b.example
    User bob
";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].alias, "a");
        assert_eq!(blocks[0].user, "alice");
        assert_eq!(blocks[1].alias, "b");
        assert_eq!(blocks[1].user, "bob");
    }

    #[test]
    fn port_invalid_value_is_ignored() {
        let input = "\
Host proxmox
    Port notanumber
";
        let blocks = parse(input);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].port, 0);
    }

    #[test]
    fn load_ssh_config_missing_file_is_empty() {
        // A non-existent home dir should
        // produce an empty vec, not an
        // error.
        let blocks = load_ssh_config(Path::new("/nonexistent/home/dir"));
        assert!(blocks.is_empty());
    }
}
