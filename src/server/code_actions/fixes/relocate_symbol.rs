//! Fix family: rewrite a `No matching symbol 'Ns::Fn'` callee to the name the
//! diagnostic already hints at (B093).
//!
//! The checker publishes the hint as DATA — a fully qualified name in
//! [`SymbolHint`] — and until now it reached the user only as prose glued
//! onto the message (`… — use \`Text::Join\` instead`). Everything needed to
//! make it an edit was already there; nothing claimed the diagnostic.
//!
//! # Two producers, two confidence classes, one edit shape
//!
//! [`SymbolHint`] is an enum precisely so this file can tell them apart
//! without reading the rendered sentence:
//!
//! * [`SymbolHint::Relocation`] — the curated `removed_api_hint` table
//!   (`Draw::GetWidth` → `Display::GetWidth`, `string::Join` → `Text::Join`).
//!   A verified successor with the call shape unchanged, so it leads the menu
//!   and joins `source.fixAll.openplanet`, exactly like
//!   [`super::removed_api`]'s member rename.
//! * [`SymbolHint::NearName`] — the nearest member by edit distance (≤ 2),
//!   from all three near-name producers (typedb namespace, workspace
//!   namespace, enum member). A GUESS. It is offered, because a typo is the
//!   common case and the user can read the name; it is neither preferred nor
//!   auto-applicable, because `source.fixAll` must never silently retarget a
//!   call to a symbol the user did not choose.
//!
//! The edit itself is identical for both: replace the diagnosed span with the
//! hinted name. Both hints are absolute qualified names, so this is a
//! substitution, never a re-qualification.
//!
//! # The relocation table is CURATED, and cannot be otherwise
//!
//! Stated where a reader will look for it, because a table that looks
//! computed but is hand-written rots silently. The full argument is on
//! [`SymbolHint::Relocation`]; the short form: the `Draw::*` rows name a
//! namespace missing from every snapshot in the repo, so no dump diff can
//! reach them; the rows a diff *could* reach are still ambiguous by simple
//! name (`Join` lives in `Path` and `string` on 1.29.5); and the CORE dump
//! carries no usable deprecation metadata — a removed symbol just vanishes,
//! and its enum-value migration prose is unusable for a measured, structural
//! reason (B134, tabulated on [`SymbolHint::Relocation`]).
//!
//! # Why the span text is checked before it is replaced
//!
//! `materialize` must be total for anything `describe` offered, and the
//! diagnostic may be stale by the time the user picks the action. The guard
//! is that the diagnosed span must still spell the qualified name the
//! diagnostic recorded (whitespace-insensitively, so `Draw :: GetWidth`
//! passes). A span that says anything else is not this call any more, and the
//! family declines rather than overwriting whatever moved into that range.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::server::code_actions::fix::{single_file_edit, FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{SymbolHint, TypeDiagnostic, TypeDiagnosticKind};

pub const FAMILY: FixFamily = FixFamily {
    name: "relocate-symbol",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::NoMatchingSymbol { .. })
}

/// The `(qualified, hint)` pair this diagnostic asks to rewrite, or `None`
/// when there is nothing writable to offer.
fn rewrite(diag: &TypeDiagnostic) -> Option<(&str, &SymbolHint)> {
    let TypeDiagnosticKind::NoMatchingSymbol { qualified, hint } = &diag.kind else {
        return None;
    };
    let hint = hint.as_ref()?;
    // A hint equal to what is already written would offer a no-op edit.
    // Unreachable from today's producers (a name that resolved would not
    // have been diagnosed), asserted so it stays that way.
    if hint.name() == qualified {
        return None;
    }
    is_qualified_name(hint.name()).then_some((qualified.as_str(), hint))
}

/// The hint has to BE a qualified name, because the edit writes it verbatim
/// into an expression position.
///
/// Segments are checked for identifier SHAPE and nothing more. Deliberately
/// no keyword check, unlike [`super::removed_api::is_member_name`]: the
/// namespace `string` is a lexer keyword and appears in real qualified names
/// on the left of exactly these diagnostics, so refusing keyword-shaped
/// segments would refuse names the game accepts.
fn is_qualified_name(text: &str) -> bool {
    let segments: Vec<&str> = text.split("::").collect();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The span to overwrite, or `None` when the document no longer spells the
/// diagnosed name there.
///
/// Compared with whitespace removed on both sides: the parser anchors these
/// diagnostics on the whole qualified-name expression, and `Draw :: GetWidth`
/// is one such expression whose text is not byte-equal to `Draw::GetWidth`.
fn edit_span(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<Span> {
    let (qualified, _) = rewrite(diag)?;
    let source = ctx.analysis.masked_source();
    let text = source.get(diag.span.start as usize..diag.span.end as usize)?;
    let written: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    (written == qualified).then_some(diag.span)
}

fn title(qualified: &str, hint: &SymbolHint) -> String {
    if hint.is_exact() {
        // Same wording as the removed-API member rename: the user is being
        // told what the successor IS, not asked to consider a candidate.
        format!("Replace `{qualified}` with `{}`", hint.name())
    } else {
        // Same wording as the near-name family for unqualified identifiers,
        // so a guess reads as a guess wherever it is offered.
        format!("Did you mean '{}'?", hint.name())
    }
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    let Some((qualified, hint)) = rewrite(diag) else {
        return Vec::new();
    };
    // `materialize` is declared total for everything described, so the span
    // is established here, where refusing is free.
    if edit_span(diag, ctx).is_none() {
        return Vec::new();
    }
    let exact = hint.is_exact();
    vec![FixDescriptor {
        title: title(qualified, hint),
        kind: CodeActionKind::QUICKFIX,
        // Only the curated table is unambiguous and lossless (the B035 bar).
        // A near-name guess may name the wrong symbol, so it must not lead
        // the menu and must never be applied by `source.fixAll`.
        is_preferred: exact,
        auto_applicable: exact,
    }]
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, _selected: &str) -> WorkspaceEdit {
    let Some(((_, hint), span)) = rewrite(diag).zip(edit_span(diag, ctx)) else {
        debug_assert!(false, "described a relocation fix with no recoverable span");
        return WorkspaceEdit::default();
    };
    single_file_edit(
        ctx.uri,
        vec![TextEdit {
            range: span_to_range(&ctx.analysis.source, span),
            new_text: hint.name().to_string(),
        }],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::code_actions::fix::{type_diagnostics, WorkspaceFileResolver};
    use crate::server::code_actions::test_fixtures::{apply_edits, ctx_for};
    use crate::typedb::TypeIndex;

    /// A recorded snapshot from `tests/fixtures/typedb-versions/`. Both
    /// halves of the relocation table need a version to be real on:
    /// `Draw::*` was removed before 1.29.5, `string::*` in 1.29.14.
    fn typedb(version: &'static str) -> &'static TypeIndex {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static DBS: OnceLock<Mutex<HashMap<&'static str, &'static TypeIndex>>> = OnceLock::new();
        let mut map = DBS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        map.entry(version).or_insert_with(|| {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/typedb-versions")
                .join(version);
            let idx = TypeIndex::load(
                &dir.join("OpenplanetCore.json"),
                &dir.join("OpenplanetNext.json"),
            )
            .expect("versioned typedb snapshot must load");
            Box::leak(Box::new(idx))
        })
    }

    /// The context a family sees, with the workspace symbol table marked
    /// AUTHORITATIVE.
    ///
    /// That flag is the fence the workspace-namespace producer sits behind
    /// (GH #58): without it `Helper::DoThng()` is never diagnosed at all,
    /// because an unread module file could still contribute the member. It
    /// is inert for every typedb case here — engine namespaces are excluded
    /// from that producer by name — so one helper serves both halves.
    fn with_ctx<R>(
        source: &str,
        idx: Option<&TypeIndex>,
        f: impl FnOnce(&FixContext<'_>) -> R,
    ) -> R {
        let (analysis, mut workspace, uri) = ctx_for(source);
        workspace.mark_dependency_closure_complete();
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: idx,
            files: WorkspaceFileResolver::empty(),
        };
        f(&ctx)
    }

    /// The real `NoMatchingSymbol` findings for `source`, as
    /// `(qualified, hint)`.
    fn findings(ctx: &FixContext<'_>) -> Vec<TypeDiagnostic> {
        type_diagnostics(ctx)
            .into_iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSymbol { .. }))
            .collect()
    }

    fn qualified_of(diag: &TypeDiagnostic) -> &str {
        let TypeDiagnosticKind::NoMatchingSymbol { qualified, .. } = &diag.kind else {
            unreachable!()
        };
        qualified
    }

    fn hint_of(diag: &TypeDiagnostic) -> Option<&SymbolHint> {
        let TypeDiagnosticKind::NoMatchingSymbol { hint, .. } = &diag.kind else {
            unreachable!()
        };
        hint.as_ref()
    }

    /// Every descriptor the family offers for `source`, paired with the
    /// qualified name it answers.
    fn offers(source: &str, idx: Option<&TypeIndex>) -> Vec<(String, FixDescriptor)> {
        with_ctx(source, idx, |ctx| {
            findings(ctx)
                .iter()
                .flat_map(|diag| {
                    let name = qualified_of(diag).to_string();
                    describe(diag, ctx)
                        .into_iter()
                        .map(move |d| (name.clone(), d))
                })
                .collect()
        })
    }

    /// `source` after applying the family's fixes ONE AT A TIME, re-deriving
    /// the diagnostics after each — the way an editor actually drives quick
    /// fixes, and the only ordering that proves each edit really removes its
    /// own finding rather than the batch happening to converge.
    fn round_trip(source: &str, idx: Option<&TypeIndex>) -> String {
        let mut text = source.to_string();
        for _ in 0..16 {
            let applied = with_ctx(&text, idx, |ctx| {
                for diag in findings(ctx) {
                    let Some(descriptor) = describe(&diag, ctx).into_iter().next() else {
                        continue;
                    };
                    let edit = materialize(&diag, ctx, &descriptor.title);
                    let edits = edit
                        .changes
                        .expect("single-file edit")
                        .remove(ctx.uri)
                        .expect("edits for the current document");
                    return Some(apply_edits(&ctx.analysis.source, &edits));
                }
                None
            });
            match applied {
                Some(next) => text = next,
                None => return text,
            }
        }
        panic!("relocation fixes did not converge for {source:?}");
    }

    /// The qualified names still flagged in `text`.
    fn still_flagged(text: &str, idx: Option<&TypeIndex>) -> Vec<String> {
        with_ctx(text, idx, |ctx| {
            findings(ctx)
                .iter()
                .map(|d| qualified_of(d).to_string())
                .collect()
        })
    }

    // ── Test premises ─────────────────────────────────────────────────────

    /// Every assertion below is worthless if the checker stopped producing
    /// these findings, so pin them first: the exact hint, and its class.
    #[test]
    fn the_checker_publishes_each_hint_class_this_family_claims() {
        let cases: [(&str, Option<&'static str>, &str, bool); 6] = [
            (
                r#"void F() { string::Join({"a"}, ","); }"#,
                Some("1.29.14"),
                "Text::Join",
                true,
            ),
            (
                r#"void F() { string::Repeat("a", 3); }"#,
                Some("1.29.14"),
                "Text::Repeat",
                true,
            ),
            (
                "void F() { Draw::GetWidth(); }",
                Some("1.29.5"),
                "Display::GetWidth",
                true,
            ),
            (
                "void F() { Display::GetWidht(); }",
                Some("1.29.5"),
                "Display::GetWidth",
                false,
            ),
            (
                "enum Status { Alpha, Beta }\nvoid F() { int x = Status::Alpah; }",
                None,
                "Status::Alpha",
                false,
            ),
            (
                "namespace Helper { void DoThing() {} }\nvoid F() { Helper::DoThng(); }",
                None,
                "Helper::DoThing",
                false,
            ),
        ];
        for (source, version, want_hint, want_exact) in cases {
            let idx = version.map(typedb);
            with_ctx(source, idx, |ctx| {
                let found = findings(ctx);
                assert_eq!(found.len(), 1, "one finding for {source:?}, got {found:?}");
                let hint =
                    hint_of(&found[0]).unwrap_or_else(|| panic!("expected a hint for {source:?}"));
                assert_eq!(hint.name(), want_hint, "for {source:?}");
                assert_eq!(hint.is_exact(), want_exact, "hint class for {source:?}");
            });
        }
    }

    // ── The relocation half: offered, exact, and it fixes the code ────────

    /// The ticket's two named cases plus the rest of the curated table.
    /// Asserts the TITLE names both ends, then round-trips: apply, re-run
    /// the real checker, and require the finding to be gone. "The edit
    /// applies" and "the edit fixes the problem" are different claims.
    #[test]
    fn every_relocation_is_offered_and_round_trips_clean() {
        let cases: [(&str, &str, &str, &str); 5] = [
            (
                r#"void F() { string::Join({"a"}, ","); }"#,
                "1.29.14",
                "string::Join",
                "Text::Join",
            ),
            (
                r#"void F() { string::Repeat("a", 3); }"#,
                "1.29.14",
                "string::Repeat",
                "Text::Repeat",
            ),
            (
                "void F() { Draw::GetWidth(); }",
                "1.29.5",
                "Draw::GetWidth",
                "Display::GetWidth",
            ),
            (
                "void F() { Draw::GetHeight(); }",
                "1.29.5",
                "Draw::GetHeight",
                "Display::GetHeight",
            ),
            (
                r#"void F() { Draw::MeasureString("hi"); }"#,
                "1.29.5",
                "Draw::MeasureString",
                "UI::MeasureString",
            ),
        ];
        for (source, version, from, to) in cases {
            let idx = Some(typedb(version));
            let offered = offers(source, idx);
            assert_eq!(offered.len(), 1, "one action for {source:?}: {offered:?}");
            assert_eq!(offered[0].0, from);
            assert_eq!(
                offered[0].1.title,
                format!("Replace `{from}` with `{to}`"),
                "for {source:?}"
            );

            let fixed = round_trip(source, idx);
            assert!(
                fixed.contains(to) && !fixed.contains(from),
                "expected {from} rewritten to {to}, got {fixed:?}"
            );
            assert_eq!(
                still_flagged(&fixed, idx),
                Vec::<String>::new(),
                "the rewrite must remove the finding, not move it: {fixed:?}"
            );
        }
    }

    /// The round trip is not vacuous. A hint that names a symbol which does
    /// not exist produces an edit that applies just as cleanly and leaves
    /// the finding standing — so the assertion above is really testing the
    /// hint, not the mechanics of applying a `TextEdit`.
    #[test]
    fn a_wrong_hint_applies_but_does_not_clear_the_finding() {
        let source = r#"void F() { string::Join({"a"}, ","); }"#;
        let idx = Some(typedb("1.29.14"));
        let mutated = with_ctx(source, idx, |ctx| {
            let mut diag = findings(ctx).remove(0);
            let TypeDiagnosticKind::NoMatchingSymbol { hint, .. } = &mut diag.kind else {
                unreachable!()
            };
            *hint = Some(SymbolHint::Relocation("Text::Joinn".to_string()));
            let descriptor = describe(&diag, ctx).remove(0);
            let edits = materialize(&diag, ctx, &descriptor.title)
                .changes
                .expect("single-file edit")
                .remove(ctx.uri)
                .expect("edits for the current document");
            apply_edits(&ctx.analysis.source, &edits)
        });
        assert!(mutated.contains("Text::Joinn"), "the edit did apply");
        assert_eq!(
            still_flagged(&mutated, idx),
            vec!["Text::Joinn".to_string()],
            "a bogus successor must still be flagged"
        );
    }

    /// Two relocations in one document, applied together by the batcher.
    /// Their spans are disjoint, so unlike B095's insertion-point collision
    /// this really can be a single batch — asserted rather than assumed.
    #[test]
    fn two_relocations_in_one_document_are_both_rewritten() {
        let source = r#"void F() { string::Join({"a"}, ","); string::Repeat("b", 2); }"#;
        let idx = Some(typedb("1.29.14"));
        assert_eq!(offers(source, idx).len(), 2);
        let fixed = round_trip(source, idx);
        assert_eq!(
            fixed,
            r#"void F() { Text::Join({"a"}, ","); Text::Repeat("b", 2); }"#
        );
    }

    /// Whitespace around `::` is inside the diagnosed span, so the guard
    /// compares with it stripped and the edit replaces the whole run.
    #[test]
    fn a_qualified_name_written_with_spaces_is_still_rewritten() {
        let source = "void F() { Draw :: GetWidth(); }";
        let idx = Some(typedb("1.29.5"));
        assert_eq!(offers(source, idx).len(), 1);
        assert_eq!(round_trip(source, idx), "void F() { Display::GetWidth(); }");
    }

    /// Only the diagnosed span moves: a same-named local, an argument and a
    /// string literal spelling the old name all survive.
    #[test]
    fn nothing_outside_the_diagnosed_span_is_edited() {
        let source = r#"void F() { string Join = "string::Join"; print(Join); Draw::GetWidth(); }"#;
        let idx = Some(typedb("1.29.5"));
        let fixed = round_trip(source, idx);
        assert_eq!(
            fixed,
            r#"void F() { string Join = "string::Join"; print(Join); Display::GetWidth(); }"#
        );
    }

    // ── The near-name half: offered, but a guess ──────────────────────────

    /// All three near-name producers reach this family by different routes
    /// (typedb namespace, enum member, workspace namespace), and each must
    /// round-trip clean too — a typo fix that does not compile is not a fix.
    #[test]
    fn every_near_name_producer_is_offered_and_round_trips_clean() {
        let cases: [(&str, Option<&'static str>, &str, &str); 3] = [
            (
                "void F() { Display::GetWidht(); }",
                Some("1.29.5"),
                "Display::GetWidth",
                "void F() { Display::GetWidth(); }",
            ),
            (
                "enum Status { Alpha, Beta }\nvoid F() { int x = Status::Alpah; }",
                None,
                "Status::Alpha",
                "enum Status { Alpha, Beta }\nvoid F() { int x = Status::Alpha; }",
            ),
            (
                "namespace Helper { void DoThing() {} }\nvoid F() { Helper::DoThng(); }",
                None,
                "Helper::DoThing",
                "namespace Helper { void DoThing() {} }\nvoid F() { Helper::DoThing(); }",
            ),
        ];
        for (source, version, hint, want) in cases {
            let idx = version.map(typedb);
            let offered = offers(source, idx);
            assert_eq!(offered.len(), 1, "one action for {source:?}: {offered:?}");
            assert_eq!(
                offered[0].1.title,
                format!("Did you mean '{hint}'?"),
                "for {source:?}"
            );
            assert_eq!(round_trip(source, idx), want);
            assert_eq!(
                still_flagged(want, idx),
                Vec::<String>::new(),
                "the typo fix must clear the finding for {source:?}"
            );
        }
    }

    /// The confidence split, which is the whole reason [`SymbolHint`] is an
    /// enum: a curated successor leads the menu and joins the batcher; a
    /// Levenshtein guess does neither, so `source.fixAll` can never
    /// silently retarget a call.
    #[test]
    fn only_the_curated_relocation_is_preferred_and_auto_applicable() {
        let relocation = offers("void F() { Draw::GetWidth(); }", Some(typedb("1.29.5")));
        assert_eq!(relocation.len(), 1);
        assert_eq!(relocation[0].1.kind, CodeActionKind::QUICKFIX);
        assert!(relocation[0].1.is_preferred);
        assert!(relocation[0].1.auto_applicable);

        let guess = offers("void F() { Display::GetWidht(); }", Some(typedb("1.29.5")));
        assert_eq!(guess.len(), 1);
        assert_eq!(guess[0].1.kind, CodeActionKind::QUICKFIX);
        assert!(!guess[0].1.is_preferred, "a guess must not lead the menu");
        assert!(
            !guess[0].1.auto_applicable,
            "a guess must never be applied by source.fixAll"
        );
    }

    // ── Refusals, each with a positive control ────────────────────────────

    /// A finding with no hint carries no target, so no action. The control
    /// in the same document proves the family was reached at all.
    #[test]
    fn a_finding_with_no_hint_offers_nothing_while_a_hinted_one_does() {
        let source = "void F() { UI::ZzzNotARealApiName(); Display::GetWidht(); }";
        let idx = Some(typedb("1.29.5"));
        with_ctx(source, idx, |ctx| {
            let found = findings(ctx);
            let unhinted: Vec<_> = found.iter().filter(|d| hint_of(d).is_none()).collect();
            assert_eq!(
                unhinted.len(),
                1,
                "test premise: one unhinted finding, got {found:?}"
            );
            assert!(describe(unhinted[0], ctx).is_empty());
        });
        let offered = offers(source, idx);
        assert_eq!(
            offered.iter().map(|(q, _)| q.as_str()).collect::<Vec<_>>(),
            vec!["Display::GetWidht"],
            "positive control: the hinted finding in the same document is offered"
        );
    }

    /// A stale diagnostic — the document moved on and the span no longer
    /// spells the recorded name — must decline rather than overwrite
    /// whatever now sits there.
    #[test]
    fn a_span_that_no_longer_spells_the_qualified_name_offers_nothing() {
        let idx = Some(typedb("1.29.5"));
        with_ctx("void F() { Draw::GetWidth(); }", idx, |ctx| {
            let real = findings(ctx).remove(0);
            assert!(!describe(&real, ctx).is_empty(), "positive control");

            // Same shape, span moved onto `void F`.
            let moved = TypeDiagnostic {
                span: Span { start: 0, end: 6 },
                kind: real.kind.clone(),
            };
            assert!(describe(&moved, ctx).is_empty(), "span text disagrees");

            // Span past the end of the document.
            let orphan = TypeDiagnostic {
                span: Span {
                    start: 9_999,
                    end: 10_000,
                },
                kind: real.kind.clone(),
            };
            assert!(describe(&orphan, ctx).is_empty(), "span out of bounds");
        });
    }

    /// A hint that is not a qualified name cannot be written into an
    /// expression position, so the family declines rather than emitting
    /// text that does not parse.
    #[test]
    fn a_hint_that_is_not_a_qualified_name_offers_nothing() {
        let idx = Some(typedb("1.29.5"));
        with_ctx("void F() { Draw::GetWidth(); }", idx, |ctx| {
            let real = findings(ctx).remove(0);
            assert!(!describe(&real, ctx).is_empty(), "positive control");
            for bad in [
                "",
                "GetWidth",         // unqualified: a bare rename, not this family
                "Display::",        // empty trailing segment
                "::GetWidth",       // empty leading segment
                "Display::1Width",  // segment starts with a digit
                "Display::Get-W",   // punctuation
                "Display::Get W",   // space inside a segment
                "Display GetWidth", // no separator at all
                "Draw::GetWidth",   // equal to what is written: a no-op edit
            ] {
                let mut diag = real.clone();
                let TypeDiagnosticKind::NoMatchingSymbol { hint, .. } = &mut diag.kind else {
                    unreachable!()
                };
                *hint = Some(SymbolHint::Relocation(bad.to_string()));
                assert!(
                    describe(&diag, ctx).is_empty(),
                    "expected no action for hint {bad:?}"
                );
            }
        });
    }

    /// Both classes are accepted by the same shape guard — the refusal
    /// above is about the hint's TEXT, not about which producer made it.
    #[test]
    fn the_shape_guard_accepts_deep_and_underscored_names() {
        for good in ["A::B", "A::B::C", "_a::_b", "Ns::Get_Width2"] {
            assert!(is_qualified_name(good), "{good} should be accepted");
        }
    }

    // ── Registry contract ─────────────────────────────────────────────────

    #[test]
    fn the_family_claims_only_no_matching_symbol() {
        assert!(matches(&TypeDiagnosticKind::NoMatchingSymbol {
            qualified: "A::B".to_string(),
            hint: None,
        }));
        assert!(!matches(&TypeDiagnosticKind::RemovedApiUsage {
            member: "M".to_string(),
            hint: "H".to_string(),
            replacement: None,
        }));
        assert!(!matches(&TypeDiagnosticKind::UndefinedMember {
            object_type: "T".to_string(),
            member: "M".to_string(),
        }));
    }

    /// The seam re-enters a family by name on `codeAction/resolve`.
    #[test]
    fn the_family_name_is_stable() {
        assert_eq!(FAMILY.name, "relocate-symbol");
    }

    /// End to end, through the handler the editor calls: ask at the
    /// diagnostic's own range and require the relocation action to come
    /// back, with the edit a client that cannot resolve would receive.
    #[test]
    fn the_handler_offers_the_relocation_at_the_diagnostic_range() {
        use crate::server::code_actions::{code_actions, CodeActionRequest};
        use tower_lsp::lsp_types::{CodeActionContext, CodeActionOrCommand};

        let source = r#"void F() { string::Join({"a"}, ","); }"#;
        let idx = typedb("1.29.14");
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: Some(idx),
            files: WorkspaceFileResolver::empty(),
        };
        let found = findings(&ctx);
        assert_eq!(found.len(), 1, "test premise: the finding is published");
        let range = span_to_range(&analysis.source, found[0].span);

        let files = [(uri.clone(), &analysis)];
        let actions = code_actions(&CodeActionRequest {
            uri: &uri,
            analysis: &analysis,
            defines: crate::server::code_actions::no_defines(),
            range,
            context: &CodeActionContext::default(),
            workspace: &workspace,
            type_index: Some(idx),
            files: WorkspaceFileResolver::new(&files),
            lazy_edits: false,
        });
        let action = actions
            .into_iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => Some(a),
                CodeActionOrCommand::Command(_) => None,
            })
            .find(|a| a.title == "Replace `string::Join` with `Text::Join`")
            .expect("the relocation action reaches the client");
        let edits = action
            .edit
            .and_then(|e| e.changes)
            .and_then(|mut c| c.remove(&uri))
            .expect("a non-resolving client gets the edit up front");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "Text::Join");
        assert_eq!(
            apply_edits(source, &edits),
            r#"void F() { Text::Join({"a"}, ","); }"#
        );
    }
}
