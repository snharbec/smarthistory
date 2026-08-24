//! Shared "negative" tag/link/attribute query syntax: `#tag!`,
//! `[[link]]!`, `[attr:value]!`, and `[attr]!` all match entries
//! that do NOT have the given tag / link / attribute-value /
//! attribute-key. Used by every mode that shares `note_search`'s
//! Obsidian-like query DSL (`@` notes, `!` todo, `:` segments) —
//! plus `"` (similar) mode's phrase search, which doesn't parse a
//! DSL at all (the whole non-token remainder is a literal embedding
//! input) but still strips these tokens out first, the same as the
//! other three.
//!
//! Also handles a `type`-specific shorthand on top of the generic
//! `[type:value]!` / `[type:value]` forms: `!!value` excludes
//! `type:value` (a pure alias for `[type:value]!`, see
//! [`split_negations`]'s doc comment), and `value` restricts results
//! to ONLY the given type(s) — repeatable, `!jira !meeting` means
//! "jira OR meeting". Order between the two matters at the regex
//! level (`!!` must be tried first) but not conceptually; see
//! [`negation_regex`]'s doc comment.
//!
//! `note_search::parse_query` has no negation primitive, so exclusion
//! is implemented entirely on smarthistory's side: [`split_negations`]
//! extracts `!`-suffixed tokens (and the two `!`-prefixed type-shorthand
//! forms) from the typed pattern BEFORE the remainder is handed to
//! `parse_query` (which would otherwise choke on the trailing `!`, an
//! unrecognized character in its grammar). Each caller then runs one
//! extra lookup query per [`NegatedTerm`] — the ordinary POSITIVE
//! `#tag` / `[[link]]` / `[attr:value]` / `[attr]` query via
//! [`NegatedTerm::positive_query_expr`] — to get the set of entries
//! that DO match it, and excludes those from the main result set. This
//! costs one extra (fast, local SQLite) round-trip per negated term;
//! there's no way to express "AND NOT" in a single `QueryExpr` tree
//! since `note_search` doesn't expose one.
//!
//! Type-restriction (the plain `value` form) is different: `note_search`
//! DOES expose `QueryExpr::Or`, so callers that already build a
//! `QueryExpr` (notes/todo/segments) can express "type is one of these"
//! natively in the same query — no extra round-trip. `similar` mode has
//! no `QueryExpr` hook on its embedding search at all, so it runs one
//! extra lookup there too (still just one, covering every restricted
//! type via `Or`, not one per type).

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
///
/// The two `type`-shorthand alternatives, `!!(\S+)` (exclude) and
/// `!(\S+)` (restrict-to), are appended after those four — and, unlike
/// them, ARE order-sensitive relative to EACH OTHER (though still
/// order-independent relative to the four bracket/tag/link forms,
/// none of which can start with `!`). This crate's `regex` engine uses
/// leftmost-FIRST semantics (Perl-style), not POSIX leftmost-longest:
/// at a given position it takes the first alternative (in source
/// order) that matches at all, not the longest one. `!!` is listed
/// before bare `!` so `!!jira` is fully consumed by the exclude
/// alternative — its literal `!!` matches both bang characters — before
/// the engine ever gets a chance to try the restrict-to alternative,
/// which would otherwise greedily swallow the leading `!` too (as
/// `\S+` matching `"!jira"`) if it ran first. Verified directly by a
/// dedicated test (`split_negations_double_bang_is_never_misparsed_as_restrict_to`)
/// rather than relying on this reasoning alone.
fn negation_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"#(?P<tag>\S+)!|\[\[(?P<link>[^\]]+)\]\]!|\[(?P<attr_kv_key>[^:\]]+):(?P<attr_kv_val>[^\]]+)\]!|\[(?P<attr_key>[^:\]]+)\]!|!!(?P<restrict_excl>\S+)|!(?P<restrict_incl>\S+)",
        )
        .expect("static regex is valid")
    })
}

/// Split `pattern` into (remaining positive-query text, negated
/// terms, type-restriction values). Every `#tag!` / `[[link]]!` /
/// `[key:value]!` / `[key]!` / `!!value` token is removed from the
/// returned string and collected as a [`NegatedTerm`] instead — so
/// `note_search::parse_query` never sees the trailing `!` (or the
/// leading `!!`) — and every bare `!value` token is removed and
/// collected into the third return value instead (raw type values,
/// not `NegatedTerm`s — restriction is a different operation from
/// negation, see this module's doc comment). Order of the remaining
/// text is preserved (modulo the removed tokens); duplicates in
/// either output are kept as-is, since callers naturally de-dupe when
/// they union results into a `HashSet` (negations) or `Or` them
/// together (restrictions, where a repeat is just a harmless no-op
/// extra clause).
///
/// An input with none of these tokens returns unchanged (trimmed)
/// with two empty `Vec`s — the common case (most queries use neither)
/// and costs one regex scan with no matches.
pub fn split_negations(pattern: &str) -> (String, Vec<NegatedTerm>, Vec<String>) {
    let re = negation_regex();
    let mut negations = Vec::new();
    let mut type_restrictions = Vec::new();
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
        } else if let Some(excl) = caps.name("restrict_excl") {
            // `!!value` — a pure alias for `[type:value]!`: produces
            // the identical `NegatedTerm` that form already produces,
            // so every caller's existing exclusion-application code
            // handles it with zero changes.
            negations.push(NegatedTerm {
                kind: NegationKind::Attribute,
                value: format!("type:{}", excl.as_str()),
            });
        } else if let Some(incl) = caps.name("restrict_incl") {
            type_restrictions.push(incl.as_str().to_string());
        }
    }
    if negations.is_empty() && type_restrictions.is_empty() {
        return (pattern.trim().to_string(), negations, type_restrictions);
    }
    // Collapse the whitespace left behind by each removed token so
    // `parse_query` doesn't see spurious empty AND-terms (e.g.
    // `foo #bar! baz` → `foo  baz` → `foo baz`).
    let remaining = re
        .replace_all(pattern, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (remaining, negations, type_restrictions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_negations_no_bang_is_untouched() {
        let (remaining, negs, restrictions) = split_negations("#kramfors [[Foo]]");
        assert_eq!(remaining, "#kramfors [[Foo]]");
        assert!(negs.is_empty());
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_extracts_negated_tag() {
        let (remaining, negs, restrictions) = split_negations("#kramfors!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Tag,
                value: "kramfors".to_string(),
            }]
        );
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_extracts_negated_link() {
        let (remaining, negs, restrictions) = split_negations("[[kramfors]]!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Link,
                value: "kramfors".to_string(),
            }]
        );
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_link_value_can_contain_spaces() {
        let (remaining, negs, _restrictions) = split_negations("[[multi word link]]!");
        assert_eq!(remaining, "");
        assert_eq!(negs[0].value, "multi word link");
    }

    #[test]
    fn split_negations_extracts_negated_attribute_key_value() {
        let (remaining, negs, restrictions) = split_negations("[type:project]!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Attribute,
                value: "type:project".to_string(),
            }]
        );
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_extracts_negated_attribute_key_only() {
        let (remaining, negs, restrictions) = split_negations("[assignee]!");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Attribute,
                value: "assignee".to_string(),
            }]
        );
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_attribute_value_can_contain_spaces() {
        let (remaining, negs, _restrictions) = split_negations("[status:in progress]!");
        assert_eq!(remaining, "");
        assert_eq!(negs[0].value, "status:in progress");
    }

    #[test]
    fn split_negations_attribute_value_preserves_extra_colons() {
        // Only the FIRST `:` splits key from value — a value that
        // is itself a URL keeps its colons intact.
        let (remaining, negs, _restrictions) = split_negations("[url:http://example.com]!");
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
        let (remaining, negs, _restrictions) = split_negations("[[kramfors]]! [type:project]!");
        assert_eq!(remaining, "");
        assert_eq!(negs.len(), 2);
        assert_eq!(negs[0].kind, NegationKind::Link);
        assert_eq!(negs[0].value, "kramfors");
        assert_eq!(negs[1].kind, NegationKind::Attribute);
        assert_eq!(negs[1].value, "type:project");
    }

    #[test]
    fn split_negations_double_bang_excludes_type_same_as_bracket_form() {
        let (remaining, negs, restrictions) = split_negations("!!jira");
        assert_eq!(remaining, "");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Attribute,
                value: "type:jira".to_string(),
            }],
            "!!jira must produce the identical NegatedTerm [type:jira]! does"
        );
        assert!(restrictions.is_empty());
        // Equivalence with the existing bracket form, not just a
        // matching literal value.
        let (_, bracket_negs, _) = split_negations("[type:jira]!");
        assert_eq!(negs, bracket_negs);
    }

    #[test]
    fn split_negations_single_bang_extracts_type_restriction() {
        let (remaining, negs, restrictions) = split_negations("!jira");
        assert_eq!(remaining, "");
        assert!(negs.is_empty(), "a restriction is not a negation");
        assert_eq!(restrictions, vec!["jira".to_string()]);
    }

    #[test]
    fn split_negations_multiple_double_bangs_collect_multiple_exclusions() {
        let (remaining, negs, restrictions) = split_negations("!!jira !!meeting");
        assert_eq!(remaining, "");
        assert_eq!(negs.len(), 2);
        assert_eq!(negs[0].value, "type:jira");
        assert_eq!(negs[1].value, "type:meeting");
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_multiple_single_bangs_collect_multiple_restrictions() {
        let (remaining, negs, restrictions) = split_negations("!jira !meeting");
        assert_eq!(remaining, "");
        assert!(negs.is_empty());
        assert_eq!(restrictions, vec!["jira".to_string(), "meeting".to_string()]);
    }

    #[test]
    fn split_negations_mixes_text_exclusion_and_restriction() {
        let (remaining, negs, restrictions) = split_negations("text !!jira !meeting");
        assert_eq!(remaining, "text");
        assert_eq!(negs.len(), 1);
        assert_eq!(negs[0].value, "type:jira");
        assert_eq!(restrictions, vec!["meeting".to_string()]);
    }

    /// The critical ordering guarantee `negation_regex`'s doc comment
    /// describes: `!!jira` must never be misparsed as a bare
    /// restriction whose value happens to start with `!` (i.e.
    /// `restrictions == ["!jira"]`), regardless of where it sits in
    /// the input or what surrounds it.
    #[test]
    fn split_negations_double_bang_is_never_misparsed_as_restrict_to() {
        for input in ["!!jira", "text !!jira", "!!jira text", "!!jira !!other", "!!jira !other"] {
            let (_, negs, restrictions) = split_negations(input);
            assert!(
                !restrictions.iter().any(|r| r.starts_with('!')),
                "input {:?} produced a restriction value starting with '!': {:?}",
                input,
                restrictions
            );
            assert!(
                negs.iter().any(|n| n.value == "type:jira"),
                "input {:?} must still extract the type:jira exclusion: negs={:?}",
                input,
                negs
            );
        }
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
        let (remaining, negs, restrictions) = split_negations("project #urgent! [[Foo]]");
        assert_eq!(remaining, "project [[Foo]]");
        assert_eq!(
            negs,
            vec![NegatedTerm {
                kind: NegationKind::Tag,
                value: "urgent".to_string(),
            }]
        );
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_multiple_negated_terms() {
        let (remaining, negs, restrictions) = split_negations("#urgent! [[Foo]]! plain");
        assert_eq!(remaining, "plain");
        assert_eq!(negs.len(), 2);
        assert_eq!(negs[0].kind, NegationKind::Tag);
        assert_eq!(negs[0].value, "urgent");
        assert_eq!(negs[1].kind, NegationKind::Link);
        assert_eq!(negs[1].value, "Foo");
        assert!(restrictions.is_empty());
    }

    #[test]
    fn split_negations_empty_input() {
        let (remaining, negs, restrictions) = split_negations("");
        assert_eq!(remaining, "");
        assert!(negs.is_empty());
        assert!(restrictions.is_empty());
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
