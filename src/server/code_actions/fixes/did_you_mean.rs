//! "Did you mean X?" quick fixes for a misspelled name (diagnostic-keyed).
//!
//! Driven by the `unknown type` / `undefined identifier` diagnostics the
//! client echoes back. Candidates come from the workspace symbol table and
//! the external type index (short-name map). Similarity is scored with
//! Levenshtein distance (≤ 2), plus two cheap fallbacks: case-insensitive
//! substring containment and a common prefix of at least 3 characters.
//!
//! **Why this is a [`DiagnosticFixFamily`] and not a `FixFamily`.** It reads
//! the name out of the diagnostic MESSAGE, not out of a
//! `TypeDiagnosticKind`: several unrelated checker kinds render to
//! ``unknown type `X` ``, and the fix cares about the rendered form rather
//! than about which kind produced it. That is also why this is the one
//! family whose `matches` looks at the message rather than at
//! [`Diagnostic::code`] — these diagnostics carry no code.
//!
//! Never `auto_applicable`: choosing between candidate spellings is the
//! user's judgement, and `source.fixAll.openplanet` has no way to make it.

use tower_lsp::lsp_types::*;

use crate::server::code_actions::fix::{
    extract_name_from_diagnostic, single_file_edit, DiagnosticFixDescriptor, DiagnosticFixFamily,
    FixContext, FixDescriptor,
};
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub const FAMILY: DiagnosticFixFamily = DiagnosticFixFamily {
    name: "did-you-mean",
    matches,
    describe,
    materialize,
};

fn matches(diagnostic: &Diagnostic) -> bool {
    is_did_you_mean_candidate(&diagnostic.message)
}

fn describe(diagnostic: &Diagnostic, ctx: &FixContext<'_>) -> Vec<DiagnosticFixDescriptor> {
    candidates(diagnostic, ctx)
        .into_iter()
        .enumerate()
        .map(|(i, name)| DiagnosticFixDescriptor {
            descriptor: FixDescriptor {
                title: title_for(&name),
                kind: CodeActionKind::QUICKFIX,
                // The closest candidate leads the menu; the rest are guesses
                // the user is choosing between.
                is_preferred: Some(i == 0),
                auto_applicable: false,
            },
            materialization_id: Some(name),
        })
        .collect()
}

/// Apply the stable spelling recorded when the action was offered. Resolve
/// deliberately does not repeat the workspace candidate search: unrelated
/// workspace files may change while this document remains identical.
fn materialize(diagnostic: &Diagnostic, ctx: &FixContext<'_>, spelling: &str) -> WorkspaceEdit {
    single_file_edit(
        ctx.uri,
        vec![TextEdit {
            range: diagnostic.range,
            new_text: spelling.to_string(),
        }],
    )
}

fn title_for(name: &str) -> String {
    format!("Did you mean '{}'?", name)
}

fn candidates(diagnostic: &Diagnostic, ctx: &FixContext<'_>) -> Vec<String> {
    let Some(needle) = extract_name_from_diagnostic(&diagnostic.message) else {
        return Vec::new();
    };
    if needle.is_empty() {
        return Vec::new();
    }
    find_candidates(needle, ctx.workspace, ctx.type_index)
        .into_iter()
        .map(|(name, _dist)| name)
        .collect()
}

fn is_did_you_mean_candidate(message: &str) -> bool {
    message.starts_with("unknown type `") || message.starts_with("undefined identifier `")
}

/// Bounded Levenshtein distance. Returns a large sentinel for over-long
/// inputs so the caller naturally skips them.
fn levenshtein(a: &str, b: &str) -> usize {
    if a.len() > 30 || b.len() > 30 {
        return 999;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 0..m {
        curr[0] = i + 1;
        for j in 0..n {
            let cost = if a_chars[i] == b_chars[j] { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn is_similar(needle: &str, candidate: &str) -> Option<usize> {
    let needle_lower = needle.to_lowercase();
    let cand_lower = candidate.to_lowercase();
    let dist = levenshtein(&needle_lower, &cand_lower);
    if dist <= 2 {
        return Some(dist);
    }
    if needle.len() >= 3 && candidate.len() >= 3 && cand_lower.contains(&needle_lower) {
        return Some(dist);
    }
    if common_prefix_len(&needle_lower, &cand_lower) >= 3 {
        return Some(dist);
    }
    None
}

fn find_candidates(
    needle: &str,
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
) -> Vec<(String, usize)> {
    let mut candidates: Vec<(String, usize)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Workspace symbols: use the tail segment ("Foo" from "Ns::Foo").
    for sym in workspace.all_symbols() {
        let bare = sym.name.rsplit("::").next().unwrap_or(&sym.name);
        if bare == needle {
            continue;
        }
        if let Some(dist) = is_similar(needle, bare) {
            if seen.insert(bare.to_string()) {
                candidates.push((bare.to_string(), dist));
            }
        }
    }

    // External type index short names (types + enums).
    if let Some(idx) = type_index {
        for short in idx.all_short_names() {
            if short == needle {
                continue;
            }
            if let Some(dist) = is_similar(needle, &short) {
                if seen.insert(short.clone()) {
                    candidates.push((short, dist));
                }
            }
        }
    }

    candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    candidates.truncate(3);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_name_works() {
        assert_eq!(
            extract_name_from_diagnostic("unknown type `Foo`"),
            Some("Foo")
        );
        assert_eq!(
            extract_name_from_diagnostic("undefined identifier `bar`"),
            Some("bar")
        );
        assert_eq!(extract_name_from_diagnostic("no backticks"), None);
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("foo", "foo"), 0);
        assert_eq!(levenshtein("foo", "fooo"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn is_did_you_mean_candidate_detects_messages() {
        assert!(is_did_you_mean_candidate("unknown type `X`"));
        assert!(is_did_you_mean_candidate("undefined identifier `y`"));
        assert!(!is_did_you_mean_candidate("some other error"));
    }

    #[test]
    fn common_prefix_matches_long_prefix() {
        assert!(is_similar("GetFoo", "GetFooBar").is_some());
    }
}
