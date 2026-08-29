//! B037: quick fixes for the conversion and structural diagnostics.
//!
//! Five families that share one problem — the diagnostic span is not the
//! span the fix edits — and therefore one helper module ([`support`]) that
//! re-finds the AST node behind a diagnostic.
//!
//! | family | diagnostic | fixes |
//! |---|---|---|
//! | [`float_truncation`] | `FloatTruncation` | explicit cast; rewrite the literal |
//! | [`sign_change`] | `SignChange` | signed declared type; explicit cast |
//! | [`signed_unsigned`] | `SignedUnsignedMismatch` | cast either operand |
//! | [`unreachable_code`] | `UnreachableCode` | delete the dead run |
//! | [`missing_return`] | `MissingReturn` | append `return <default>;` |
//!
//! **What `source.fixAll.openplanet` takes.** Only the `FloatTruncation`
//! cast. Everything else here changes program behaviour rather than tidying
//! it, and a save-time action must not do that:
//!
//! * rewriting a float literal changes the *value*;
//! * `SignChange`'s two fixes disagree about what the author meant;
//! * `SignedUnsignedMismatch` offers a cast on *each* side, and the two
//!   edits do not overlap — so a batcher would apply BOTH and produce
//!   `uint(i) < int(n)`, which is nobody's intent (see
//!   [`signed_unsigned`]'s module docs);
//! * deleting unreachable code on save is not a tidy-up;
//! * a plausible-looking wrong `return` is worse than a compile error.

pub mod float_truncation;
pub mod missing_return;
pub mod sign_change;
pub mod signed_unsigned;
pub mod support;
pub mod unreachable_code;

#[cfg(test)]
pub(crate) mod test_support {
    //! Text-level assertions for the five families.
    //!
    //! Everything is scoped to ONE family name: `collect_fixes` runs the
    //! whole registry, and a sibling family that happens to claim the same
    //! fixture must not perturb these tests.

    use crate::analysis::DocumentAnalysis;
    use crate::server::code_actions::fix::{
        collect_fixes, FixCandidate, FixContext, WorkspaceFileResolver,
    };
    use crate::server::code_actions::fix_all::{fix_all_action, DocumentFix};
    use crate::server::code_actions::test_fixtures::{apply_edits, ctx_for};

    fn candidates(source: &str) -> (Vec<FixCandidate>, tower_lsp::lsp_types::Url) {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::empty(),
        };
        (collect_fixes(&ctx), uri)
    }

    fn of_family(source: &str, family: &str) -> Vec<FixCandidate> {
        candidates(source)
            .0
            .into_iter()
            .filter(|candidate| candidate.family == family)
            .collect()
    }

    /// Every action title `family` offers for `source`, in offer order.
    pub fn titles(source: &str, family: &str) -> Vec<String> {
        of_family(source, family)
            .into_iter()
            .map(|candidate| candidate.fix.title)
            .collect()
    }

    /// How many actions `family` offers for `source`.
    pub fn count(source: &str, family: &str) -> usize {
        of_family(source, family).len()
    }

    /// `source` after applying the single action of `family` whose title
    /// contains `title_fragment`. Panics when that is not exactly one
    /// action — an ambiguous test is a broken test.
    pub fn apply(source: &str, family: &str, title_fragment: &str) -> String {
        let matched: Vec<FixCandidate> = of_family(source, family)
            .into_iter()
            .filter(|candidate| candidate.fix.title.contains(title_fragment))
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "expected exactly one `{family}` action matching `{title_fragment}` for `{source}`, got {:?}",
            matched.iter().map(|c| &c.fix.title).collect::<Vec<_>>()
        );
        let edits = matched[0]
            .fix
            .edit
            .changes
            .as_ref()
            .expect("document-local edit")
            .values()
            .next()
            .expect("one file")
            .clone();
        apply_edits(source, &edits)
    }

    /// `source` after applying every action `family` offers, batched
    /// through the real overlap rules. For a diagnostic the checker reports
    /// more than once in a document.
    pub fn apply_all(source: &str, family: &str) -> String {
        let uri = candidates(source).1;
        let fixes: Vec<DocumentFix> = of_family(source, family)
            .iter()
            .filter_map(|candidate| {
                let edits = candidate.fix.edit.changes.as_ref()?.get(&uri)?.clone();
                Some(DocumentFix {
                    title: candidate.fix.title.clone(),
                    edits,
                })
            })
            .collect();
        apply_edits(
            source,
            &crate::server::code_actions::fix_all::merged_edits(&fixes),
        )
    }

    /// `(is_preferred, auto_applicable)` for the action whose title
    /// contains `title_fragment`.
    pub fn flags(source: &str, family: &str, title_fragment: &str) -> (bool, bool) {
        let candidate = of_family(source, family)
            .into_iter()
            .find(|candidate| candidate.fix.title.contains(title_fragment))
            .unwrap_or_else(|| panic!("no `{family}` action matching `{title_fragment}`"));
        (candidate.fix.is_preferred, candidate.fix.auto_applicable)
    }

    /// `source` after `source.fixAll.openplanet` — the whole registry, not
    /// one family, because that is what the user gets on save.
    pub fn apply_fix_all(source: &str) -> String {
        let (candidates, uri) = candidates(source);
        let Some(action) = fix_all_action(&uri, &candidates) else {
            return source.to_string();
        };
        let edits = action
            .edit
            .and_then(|edit| edit.changes)
            .and_then(|mut changes| changes.remove(&uri))
            .expect("fix-all edits");
        apply_edits(source, &edits)
    }

    /// Every title `source.fixAll.openplanet` would batch.
    pub fn fix_all_titles(source: &str) -> Vec<String> {
        let (candidates, uri) = candidates(source);
        let mut titles: Vec<String> = candidates
            .iter()
            .filter(|candidate| candidate.fix.auto_applicable)
            .filter_map(|candidate| {
                let changes = candidate.fix.edit.changes.as_ref()?;
                changes.get(&uri).map(|edits| DocumentFix {
                    title: candidate.fix.title.clone(),
                    edits: edits.clone(),
                })
            })
            .map(|fix| fix.title)
            .collect();
        titles.sort();
        titles
    }

    /// Does `source` parse with no errors? The bar for a deletion or an
    /// insertion: asserting on text alone would pass on output the parser
    /// rejects.
    pub fn reparses_clean(source: &str) -> bool {
        DocumentAnalysis::analyze_plain(source)
            .parse_errors
            .is_empty()
    }
}

#[cfg(test)]
mod fix_all_tests {
    use super::test_support::{apply_fix_all, fix_all_titles, reparses_clean};

    /// One document carrying all five B037 diagnostics at once.
    const ALL_FIVE: &str = "\
void dead() {
    return;
    int unreachable = 1;
}
int missing() {
}
void conversions(int i, uint n) {
    int truncated = 3.7;
    uint negative = -1;
    if (i < n) {
    }
}
";

    #[test]
    fn fix_all_takes_only_the_behaviour_preserving_cast() {
        // Everything else here either changes a value, deletes code, or
        // asks the author which of two readings they meant. See the module
        // docs for the per-family reasoning.
        assert_eq!(
            fix_all_titles(ALL_FIVE),
            vec!["Cast the initializer to `int`".to_string()]
        );
    }

    #[test]
    fn fix_all_leaves_every_other_family_untouched() {
        let fixed = apply_fix_all(ALL_FIVE);
        assert_eq!(
            fixed,
            "\
void dead() {
    return;
    int unreachable = 1;
}
int missing() {
}
void conversions(int i, uint n) {
    int truncated = int(3.7);
    uint negative = -1;
    if (i < n) {
    }
}
"
        );
        assert!(reparses_clean(&fixed));
    }

    #[test]
    fn fix_all_is_idempotent() {
        // The cast it applies removes the diagnostic that produced it, so a
        // second pass must find nothing left to do.
        let once = apply_fix_all(ALL_FIVE);
        assert_eq!(apply_fix_all(&once), once);
    }
}
