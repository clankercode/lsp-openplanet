//! `source.fixAll.openplanet` — batching many independent fixes into one
//! `WorkspaceEdit`.
//!
//! The whole difficulty is overlap: two fixes may each be individually
//! correct while their edits contradict each other. [`select_non_overlapping`]
//! is the pure, separately tested decision function; everything else in this
//! module is assembly.
//!
//! Two rules make the batch safe:
//!
//! 1. **Atomic per fix.** A fix is accepted or rejected as a whole; a
//!    partially applied multi-edit fix would be worse than no fix.
//! 2. **Descending application order.** The returned edits are sorted by
//!    start position *descending*, so applying them front-to-back never
//!    shifts the offsets of an edit that has not been applied yet. (A
//!    spec-compliant client may apply non-overlapping edits in any order;
//!    ordering them defensively costs nothing and makes the batch safe for
//!    naive sequential appliers too.)

use tower_lsp::lsp_types::*;

use super::fix::{Fix, FixCandidate};

/// Do two ranges conflict?
///
/// Non-empty ranges conflict when they properly intersect. Two *insertions*
/// (empty ranges) conflict only at the identical position, where their
/// relative order would be arbitrary. An insertion strictly inside another
/// range conflicts; an insertion exactly at a range boundary does not.
pub fn ranges_overlap(a: &Range, b: &Range) -> bool {
    let a_empty = a.start == a.end;
    let b_empty = b.start == b.end;
    if a_empty && b_empty {
        return a.start == b.start;
    }
    a.start < b.end && b.start < a.end
}

/// Does this fix's own edit set contradict itself? Such a fix can never be
/// applied safely, individually or in a batch.
fn is_self_consistent(fix: &Fix) -> bool {
    for (i, x) in fix.edits.iter().enumerate() {
        for y in &fix.edits[i + 1..] {
            if ranges_overlap(&x.range, &y.range) {
                return false;
            }
        }
    }
    true
}

/// Sort key that makes the greedy choice below deterministic regardless of
/// the order families were registered in: earliest start, then shortest,
/// then title. Fixes with no edits sort last and are dropped by the caller.
fn order_key(fix: &Fix) -> (Position, Position, &str) {
    let start = fix
        .edits
        .iter()
        .map(|e| e.range.start)
        .min()
        .unwrap_or(Position::new(u32::MAX, u32::MAX));
    let end = fix
        .edits
        .iter()
        .map(|e| e.range.end)
        .max()
        .unwrap_or(Position::new(u32::MAX, u32::MAX));
    (start, end, fix.title.as_str())
}

/// Greedily choose a non-conflicting subset of `fixes`, deterministically.
///
/// Returns indices into `fixes`, in the original order, so a caller can tell
/// which fixes were *dropped* (those stay individually offerable).
pub fn select_non_overlapping(fixes: &[&Fix]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..fixes.len())
        .filter(|&i| !fixes[i].edits.is_empty() && is_self_consistent(fixes[i]))
        .collect();
    order.sort_by(|&a, &b| order_key(fixes[a]).cmp(&order_key(fixes[b])));

    let mut accepted: Vec<usize> = Vec::new();
    let mut taken: Vec<Range> = Vec::new();
    for i in order {
        let conflicts = fixes[i]
            .edits
            .iter()
            .any(|e| taken.iter().any(|t| ranges_overlap(t, &e.range)));
        if conflicts {
            continue;
        }
        taken.extend(fixes[i].edits.iter().map(|e| e.range));
        accepted.push(i);
    }
    accepted.sort_unstable();
    accepted
}

/// The batched edits for the accepted fixes, ordered start-descending.
pub fn merged_edits(fixes: &[&Fix]) -> Vec<TextEdit> {
    let mut edits: Vec<TextEdit> = select_non_overlapping(fixes)
        .into_iter()
        .flat_map(|i| fixes[i].edits.iter().cloned())
        .collect();
    edits.sort_by(|a, b| {
        b.range
            .start
            .cmp(&a.range.start)
            .then_with(|| b.range.end.cmp(&a.range.end))
    });
    edits
}

/// Build the `source.fixAll.openplanet` action, or `None` when nothing in
/// the document is auto-applicable. An empty `WorkspaceEdit` would still show
/// up in the client's menu and do nothing when picked, so we omit the action.
pub fn fix_all_action(uri: &Url, candidates: &[FixCandidate]) -> Option<CodeAction> {
    let auto: Vec<&Fix> = candidates
        .iter()
        .filter(|c| c.fix.auto_applicable)
        .map(|c| &c.fix)
        .collect();
    let edits = merged_edits(&auto);
    if edits.is_empty() {
        return None;
    }
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(CodeAction {
        title: "Fix all auto-fixable Openplanet problems".to_string(),
        kind: Some(super::fix::SOURCE_FIX_ALL_OPENPLANET),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }),
        command: None,
        // Never preferred: a source action must not win the "auto fix"
        // keybinding over the specific quick fix the user is looking at.
        is_preferred: None,
        disabled: None,
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
        Range::new(Position::new(l0, c0), Position::new(l1, c1))
    }

    fn fix(title: &str, edits: &[(Range, &str)]) -> Fix {
        Fix {
            title: title.to_string(),
            kind: CodeActionKind::QUICKFIX,
            edits: edits
                .iter()
                .map(|(r, t)| TextEdit {
                    range: *r,
                    new_text: t.to_string(),
                })
                .collect(),
            is_preferred: false,
            auto_applicable: true,
        }
    }

    /// Apply edits sequentially, exactly as a naive client would.
    fn apply(source: &str, edits: &[TextEdit]) -> String {
        let mut out = source.to_string();
        for e in edits {
            let start = crate::server::diagnostics::position_to_offset(&out, e.range.start);
            let end = crate::server::diagnostics::position_to_offset(&out, e.range.end);
            out.replace_range(start..end, &e.new_text);
        }
        out
    }

    #[test]
    fn overlap_detects_proper_intersection() {
        assert!(ranges_overlap(&rng(0, 0, 0, 5), &rng(0, 3, 0, 8)));
        assert!(ranges_overlap(&rng(0, 3, 0, 8), &rng(0, 0, 0, 5)));
        assert!(ranges_overlap(&rng(0, 0, 0, 9), &rng(0, 2, 0, 3)));
    }

    #[test]
    fn overlap_allows_abutting_ranges() {
        assert!(!ranges_overlap(&rng(0, 0, 0, 5), &rng(0, 5, 0, 9)));
        assert!(!ranges_overlap(&rng(0, 0, 0, 5), &rng(1, 0, 1, 5)));
    }

    #[test]
    fn overlap_rules_for_insertions() {
        // Two insertions at the same point: order would be arbitrary.
        assert!(ranges_overlap(&rng(0, 4, 0, 4), &rng(0, 4, 0, 4)));
        // Different points: independent.
        assert!(!ranges_overlap(&rng(0, 4, 0, 4), &rng(0, 5, 0, 5)));
        // Insertion strictly inside a replacement: conflicts.
        assert!(ranges_overlap(&rng(0, 4, 0, 4), &rng(0, 2, 0, 7)));
        // Insertion at a replacement's boundary: fine.
        assert!(!ranges_overlap(&rng(0, 2, 0, 2), &rng(0, 2, 0, 7)));
        assert!(!ranges_overlap(&rng(0, 7, 0, 7), &rng(0, 2, 0, 7)));
    }

    #[test]
    fn selects_all_disjoint_fixes() {
        let a = fix("a", &[(rng(0, 0, 0, 3), "A")]);
        let b = fix("b", &[(rng(0, 5, 0, 8), "B")]);
        let c = fix("c", &[(rng(2, 0, 2, 1), "C")]);
        let all = [&a, &b, &c];
        assert_eq!(select_non_overlapping(&all), vec![0, 1, 2]);
    }

    #[test]
    fn drops_exactly_one_of_an_overlapping_pair_deterministically() {
        let early = fix("early", &[(rng(0, 0, 0, 6), "E")]);
        let late = fix("late", &[(rng(0, 3, 0, 9), "L")]);
        // Registration order must not change the outcome.
        assert_eq!(select_non_overlapping(&[&early, &late]), vec![0]);
        assert_eq!(select_non_overlapping(&[&late, &early]), vec![1]);
    }

    #[test]
    fn same_line_disjoint_fixes_both_apply() {
        //           0         1         2
        //           0123456789012345678901234567
        let src = "void f(string one, string two) {}";
        let a = fix("a", &[(rng(0, 7, 0, 13), "const string &in")]);
        let b = fix("b", &[(rng(0, 19, 0, 25), "const string &in")]);
        let edits = merged_edits(&[&a, &b]);
        assert_eq!(edits.len(), 2);
        assert_eq!(
            apply(src, &edits),
            "void f(const string &in one, const string &in two) {}"
        );
    }

    #[test]
    fn edits_are_ordered_descending_so_offsets_stay_valid() {
        let a = fix("a", &[(rng(0, 0, 0, 1), "AAAA")]);
        let b = fix("b", &[(rng(0, 4, 0, 5), "B")]);
        let c = fix("c", &[(rng(0, 8, 0, 9), "CC")]);
        let edits = merged_edits(&[&a, &b, &c]);
        let starts: Vec<u32> = edits.iter().map(|e| e.range.start.character).collect();
        assert_eq!(starts, vec![8, 4, 0], "expected descending start order");
        // Growing an early edit must not corrupt the later ones.
        assert_eq!(apply("0123456789", &edits), "AAAA123B567CC9");
    }

    #[test]
    fn multi_edit_fix_is_all_or_nothing() {
        let pair = fix("pair", &[(rng(0, 0, 0, 2), "P"), (rng(0, 10, 0, 12), "Q")]);
        let blocker = fix("blocker", &[(rng(0, 11, 0, 13), "X")]);
        // `pair` starts first, so it wins; `blocker` is dropped whole.
        let sel = select_non_overlapping(&[&pair, &blocker]);
        assert_eq!(sel, vec![0]);
        assert_eq!(merged_edits(&[&pair, &blocker]).len(), 2);
    }

    #[test]
    fn self_contradictory_fix_is_never_selected() {
        let bad = fix("bad", &[(rng(0, 0, 0, 5), "A"), (rng(0, 3, 0, 8), "B")]);
        assert!(select_non_overlapping(&[&bad]).is_empty());
    }

    #[test]
    fn fix_with_no_edits_is_ignored() {
        let empty = fix("empty", &[]);
        let real = fix("real", &[(rng(0, 0, 0, 1), "R")]);
        assert_eq!(select_non_overlapping(&[&empty, &real]), vec![1]);
    }

    #[test]
    fn fix_all_action_is_absent_when_nothing_is_fixable() {
        let uri = Url::parse("file:///tmp/a.as").unwrap();
        assert!(fix_all_action(&uri, &[]).is_none());
    }

    #[test]
    fn fix_all_action_skips_non_auto_applicable_fixes() {
        let uri = Url::parse("file:///tmp/a.as").unwrap();
        let mut manual = fix("manual", &[(rng(0, 0, 0, 1), "M")]);
        manual.auto_applicable = false;
        let candidates = vec![FixCandidate {
            family: "test",
            diagnostic: Diagnostic {
                range: rng(0, 0, 0, 1),
                message: "m".into(),
                ..Default::default()
            },
            fix: manual,
        }];
        assert!(fix_all_action(&uri, &candidates).is_none());
    }
}
