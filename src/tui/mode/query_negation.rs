//! Shared "negative" tag/link/attribute query syntax: `#tag!`,
//! `[[link]]!`, `[attr:value]!`, and `[attr]!` all match entries
//! that do NOT have the given tag / link / attribute-value /
//! attribute-key. Used by every mode that shares `note_search`'s
//! Obsidian-like query DSL (`@` notes, `!` todo, `:` segments) —
//! `"` (similar) mode's phrase search doesn't parse a DSL at all
//! (the whole typed body is a literal embedding input), so it's
//! unaffected by this module.
//!
//! `note_search::parse_query` has no negation primitive, so this is
//! implemented entirely on smarthistory's side: [`split_negations`]
//! extracts `!`-suffixed tokens from the typed pattern BEFORE the
//! remainder is handed to `parse_query` (which would otherwise choke
//! on the trailing `!`, an unrecognized character in its grammar).
//! Each caller then runs one extra lookup query per [`NegatedTerm`]
//! — the ordinary POSITIVE `#tag` / `[[link]]` / `[attr:value]` /
//! `[attr]` query via [`NegatedTerm::positive_query_expr`] — to get
//! the set of entries that DO match it, and excludes those from the
//! main result set. This costs one extra (fast, local SQLite)
//! round-trip per negated term; there's no way to express "AND NOT"
//! in a single `QueryExpr` tree since `note_search` doesn't expose
//! one.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegationKind {
    Tag,
    Link,
    /// `[attr:value]!` / `[attr]!`. The attribute's key (and, for
    /// the `[attr:value]!` form, its value) is encoded into
    /// [`NegatedTerm::value`] as `"key:value"` or bare `"key"` —
    /// see [`NegatedTerm::attribute_key_value`] — rather than
    /// adding a second field, since only this one variant needs
    /// the extra piece of data.
    Attribute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegatedTerm {
    pub kind: NegationKind,
    pub value: String,
}

impl NegatedTerm {
    /// For `NegationKind::Attribute`, split the encoded `value`
    /// (`"key:value"` or bare `"key"`) back into its parts. Only
    /// the FIRST `:` is significant — an attribute value containing
    /// its own `:` (e.g. a URL) stays intact.
    fn attribute_key_value(&self) -> (String, Option<String>) {
        match self.value.split_once(':') {
            Some((key, value)) => (key.to_string(), Some(value.to_string())),
            None => (self.value.clone(), None),
        }
    }

    /// The ordinary (non-negated) `QueryExpr` for this term — used
    /// to run the "does this entry have it" lookup query the caller
    /// excludes matches from.
    pub fn positive_query_expr(&self) -> note_search::QueryExpr {
        match self.kind {
            NegationKind::Tag => note_search::QueryExpr::Tag(self.value.clone()),
            NegationKind::Link => note_search::QueryExpr::Link(self.value.clone()),
            NegationKind::Attribute => {
                let (key, value) = self.attribute_key_value();
                note_search::QueryExpr::Attribute { key, value }
            }
        }
    }

    /// The equivalent positive query DSL string for this term
    /// (`#value` / `[[value]]` / `[key:value]` / `[key]`) — for
    /// callers (like `notes::fetch`) that go through
    /// `DatabaseService::search_notes_by_query(&str)` rather than
    /// building a `QueryExpr` directly.
    pub fn positive_query_string(&self) -> String {
        match self.kind {
            NegationKind::Tag => format!("#{}", self.value),
            NegationKind::Link => format!("[[{}]]", self.value),
            NegationKind::Attribute => match self.attribute_key_value() {
                (key, Some(value)) => format!("[{}:{}]", key, value),
                (key, None) => format!("[{}]", key),
            },
        }
    }
}

/// `#(\S+)!`, `[[(...)]]!`, `[key:value]!`, or `[key]!` anywhere in
/// the pattern. Tags have no spaces (bare `\S+`, backtracking
/// naturally leaves the trailing `!` for the literal); links and
/// attribute values are bracket-delimited so they CAN contain
/// spaces (`[^\]]+`, stopping at the first `]`). The attribute
/// key's character class excludes `:` (so the `key:value` split is
/// unambiguous) and `]`. The alternatives are order-independent —
/// the required-immediately-adjacent `]!` / `]]!` literals mean a
/// `[[link]]!` (two closing brackets before `!`) can never satisfy
/// the single-bracket attribute alternatives, and `[key:value]!`
/// requires a literal `:` that plain `[key]!` and `[[link]]!` don't
/// have — so the four forms never collide regardless of scan order.
fn negation_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"#(?P<tag>\S+)!|\[\[(?P<link>[^\]]+)\]\]!|\[(?P<attr_kv_key>[^:\]]+):(?P<attr_kv_val>[^\]]+)\]!|\[(?P<attr_key>[^:\]]+)\]!",
        )
        .expect("static regex is valid")
    })
}

/// Split `pattern` into (remaining positive-query text, negated
/// terms). Every `#tag!` / `[[link]]!` token is removed from the
/// returned string — so `note_search::parse_query` never sees the
/// trailing `!` — and collected as a [`NegatedTerm`] instead. Order
/// of the remaining text is preserved (modulo the removed tokens);
/// duplicate negated terms are kept as-is, since callers naturally
/// de-dupe when they union the lookup results into a `HashSet`.
///
/// An input with no `!`-suffixed tokens returns unchanged (trimmed)
/// with an empty `Vec` — this is the common case (most queries don't
/// use negation) and costs one regex scan with no matches.
pub fn split_negations(pattern: &str) -> (String, Vec<NegatedTerm>) {
    let re = negation_regex();
    let mut negations = Vec::new();
    for caps in re.captures_iter(pattern) {
        if let Some(tag) = caps.name("tag") {
            negations.push(NegatedTerm {
                kind: NegationKind::Tag,
                value: tag.as_str().to_string(),
            });
        } else if let Some(link) = caps.name("link") {
            negations.push(NegatedTerm {
                kind: NegationKind::Link,
                value: link.as_str().to_string(),
            });
        } else if let (Some(key), Some(val)) =
            (caps.name("attr_kv_key"), caps.name("attr_kv_val"))
        {
            negations.push(NegatedTerm {
                kind: NegationKind::Attribute,
                value: format!("{}:{}", key.as_str(), val.as_str()),
            });
        } else if let Some(key) = caps.name("attr_key") {
            negations.push(NegatedTerm {
                kind: NegationKind::Attribute,
                value: key.as_str().to_string(),
            });
        }
    }
    if negations.is_empty() {
        return (pattern.trim().to_string(), negations);
    }
    // Collapse the whitespace left behind by each removed token so
    // `parse_query` doesn't see spurious empty AND-terms (e.g.
    // `foo #bar! baz` → `foo  baz` → `foo baz`).
    let remaining = re
        .replace_all(pattern, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (remaining, negations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_negations_no_bang_is_untouched() {
        let (remaining, negs) = split_negations("#kramfors [[Foo]]");
        assert_eq!(remaining, "#kramfors [[Foo]]");
        assert!(negs.is_empty());
    }

    #[test]
    fn split_negations_extracts_negated_tag() {
        let (remaining, negs) = split_negations("#kramfors!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Tag,
                value: "kramfors".to_string(),
            }]
        );
    }

    #[test]
    fn split_negations_extracts_negated_link() {
        let (remaining, negs) = split_negations("[[kramfors]]!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Link,
                value: "kramfors".to_string(),
            }]
        );
    }

    #[test]
    fn split_negations_link_value_can_contain_spaces() {
        let (remaining, negs) = split_negations("[[multi word link]]!");
        assert_eq!(remaining, "");
        assert_eq!(negs[0].value, "multi word link");
    }

    #[test]
    fn split_negations_extracts_negated_attribute_key_value() {
        let (remaining, negs) = split_negations("[type:project]!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Attribute,
                value: "type:project".to_string(),
            }]
        );
    }

    #[test]
    fn split_negations_extracts_negated_attribute_key_only() {
        let (remaining, negs) = split_negations("[assignee]!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Attribute,
                value: "assignee".to_string(),
            }]
        );
    }

    #[test]
    fn split_negations_attribute_value_can_contain_spaces() {
        let (remaining, negs) = split_negations("[status:in progress]!");
        assert_eq!(remaining, "");
        assert_eq!(negs[0].value, "status:in progress");
    }

    #[test]
    fn split_negations_attribute_value_preserves_extra_colons() {
        // Only the FIRST `:` splits key from value — a value that
        // is itself a URL keeps its colons intact.
        let (remaining, negs) = split_negations("[url:http://example.com]!");
        assert_eq!(remaining, "");
        assert_eq!(negs[0].value, "url:http://example.com");
        assert_eq!(
            negs[0].positive_query_expr(),
            note_search::QueryExpr::Attribute {
                key: "url".to_string(),
                value: Some("http://example.com".to_string()),
            }
        );
    }

    #[test]
    fn split_negations_attribute_negation_does_not_collide_with_link_negation() {
        let (remaining, negs) = split_negations("[[kramfors]]! [type:project]!");
        assert_eq!(remaining, "");
        assert_eq!(negs.len(), 2);
        assert_eq!(negs[0].kind, NegationKind::Link);
        assert_eq!(negs[0].value, "kramfors");
        assert_eq!(negs[1].kind, NegationKind::Attribute);
        assert_eq!(negs[1].value, "type:project");
    }

    #[test]
    fn attribute_positive_query_expr_key_value() {
        let term = NegatedTerm {
            kind: NegationKind::Attribute,
            value: "type:project".to_string(),
        };
        assert_eq!(
            term.positive_query_expr(),
            note_search::QueryExpr::Attribute {
                key: "type".to_string(),
                value: Some("project".to_string()),
            }
        );
        assert_eq!(term.positive_query_string(), "[type:project]");
    }

    #[test]
    fn attribute_positive_query_expr_key_only() {
        let term = NegatedTerm {
            kind: NegationKind::Attribute,
            value: "assignee".to_string(),
        };
        assert_eq!(
            term.positive_query_expr(),
            note_search::QueryExpr::Attribute {
                key: "assignee".to_string(),
                value: None,
            }
        );
        assert_eq!(term.positive_query_string(), "[assignee]");
    }

    #[test]
    fn split_negations_mixes_positive_and_negative_terms() {
        let (remaining, negs) = split_negations("project #urgent! [[Foo]]");
        assert_eq!(remaining, "project [[Foo]]");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Tag,
                value: "urgent".to_string(),
            }]
        );
    }

    #[test]
    fn split_negations_multiple_negated_terms() {
        let (remaining, negs) = split_negations("#urgent! [[Foo]]! plain");
        assert_eq!(remaining, "plain");
        assert_eq!(negs.len(), 2);
        assert_eq!(negs[0].kind, NegationKind::Tag);
        assert_eq!(negs[0].value, "urgent");
        assert_eq!(negs[1].kind, NegationKind::Link);
        assert_eq!(negs[1].value, "Foo");
    }

    #[test]
    fn split_negations_empty_input() {
        let (remaining, negs) = split_negations("");
        assert_eq!(remaining, "");
        assert!(negs.is_empty());
    }

    #[test]
    fn positive_query_string_round_trips() {
        let tag = NegatedTerm {
            kind: NegationKind::Tag,
            value: "urgent".to_string(),
        };
        assert_eq!(tag.positive_query_string(), "#urgent");
        let link = NegatedTerm {
            kind: NegationKind::Link,
            value: "Project X".to_string(),
        };
        assert_eq!(link.positive_query_string(), "[[Project X]]");
    }
}
