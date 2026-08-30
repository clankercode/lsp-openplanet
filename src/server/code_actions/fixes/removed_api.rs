//! Fix family: a removed API member whose migration is a plain rename.
//!
//! Drives [`TypeDiagnosticKind::RemovedApiUsage`] (GH #27/#28), the family
//! of members that still compile but are a no-op at runtime. The checker
//! already knows the migration; it publishes it twice, in two shapes:
//! `hint` is the human sentence in the message, and `replacement` is the
//! same migration as data. **Only `replacement` drives the edit** — parsing
//! prose to recover an identifier is how a quick fix ends up rewriting the
//! wrong thing.
//!
//! `replacement` is `None` wherever the migration is not a member rename,
//! and today that is every producer in the tree: `awaitable.WithRunContext`
//! migrates to `Meta::StartWithRunContext`, a free function that takes the
//! coroutine as an argument, so the receiver has to move into the call.
//! Swapping the member span there would emit
//! `Meta::StartWithRunContext(ctx)` and silently drop the coroutine, so this
//! family offers nothing for it. Offering a fix we cannot make is worse than
//! offering none.
//!
//! That migration is not unfixable, only un-renameable: since B048 it is
//! offered as a call restructure by [`super::with_run_context`], a separate
//! family claiming the same diagnostic. The division is by SHAPE of edit —
//! this family writes member renames and nothing else, which is what keeps
//! it a span swap plus a validity check.
//!
//! The edited span is recovered from the token stream rather than by
//! scanning text: both producers anchor the diagnostic on the whole member
//! *expression* (`startnew(W).WithRunContext`), not on the member name, so
//! the name is the expression's last token. Taking it from the tokens keeps
//! the edit off any other mention of the same name — in the receiver, or
//! elsewhere on the line.

use tower_lsp::lsp_types::CodeActionKind;

use crate::lexer::token::{keyword_lookup, TokenKind};
use crate::lexer::{Span, Token};
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

pub const FAMILY: FixFamily = FixFamily {
    name: "removed-api-usage",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::RemovedApiUsage { .. })
}

/// The rename this diagnostic asks for, or `None` when it does not ask for
/// one. `None` covers both "the checker recorded no structured
/// replacement" and "the replacement is not something a member-span swap
/// can write" — see [`is_member_name`].
fn rename(diag: &TypeDiagnostic) -> Option<(&str, &str)> {
    let TypeDiagnosticKind::RemovedApiUsage {
        member,
        replacement,
        ..
    } = &diag.kind
    else {
        return None;
    };
    let replacement = replacement.as_deref()?;
    is_member_name(replacement).then_some((member.as_str(), replacement))
}

/// The edit replaces the member name and nothing else, so the replacement
/// has to BE a member name. A qualified name (`Meta::Start…`), an empty
/// string, a reserved word or anything else would produce text that does
/// not parse, and the family declines rather than writing it. The checker's
/// field docs say `Some` means a plain rename; this is that contract
/// enforced at the point where breaking it would reach the user's buffer.
fn is_member_name(text: &str) -> bool {
    let mut chars = text.chars();
    let shaped_like_an_identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    // Identifier shape is not enough: `class` and `void` are shaped like
    // identifiers but lex as keywords, so `obj.class(…)` would not parse.
    // Ask the lexer rather than keeping a second list of reserved words.
    shaped_like_an_identifier && keyword_lookup(text) == TokenKind::Ident
}

fn title(member: &str, replacement: &str) -> String {
    format!("Replace `{member}` with `{replacement}`")
}

/// Span of the member NAME, given the span the diagnostic is anchored on.
///
/// Both producers anchor on the whole member expression, so the name is its
/// last non-trivia token. Requiring that token to be an identifier spelled
/// exactly `member` is what makes a stale diagnostic — or a shape this
/// function was not written for — decline instead of renaming whatever
/// happens to sit there.
fn member_name_span(
    tokens: &[Token],
    masked_source: &str,
    diag_span: Span,
    member: &str,
) -> Option<Span> {
    let last = tokens.iter().rfind(|token| {
        !token.kind.is_trivia()
            && token.span.start >= diag_span.start
            && token.span.end <= diag_span.end
    })?;
    (last.kind == TokenKind::Ident && last.span.text(masked_source) == member).then_some(last.span)
}

/// The span this fix would edit, or `None` if there is no fix to offer.
/// `describe` and `materialize` share it so the menu entry and the edit can
/// never disagree.
fn edit_span(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<Span> {
    let (member, _) = rename(diag)?;
    member_name_span(
        &ctx.analysis.tokens,
        ctx.analysis.masked_source(),
        diag.span,
        member,
    )
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    let Some((member, replacement)) = rename(diag) else {
        return Vec::new();
    };
    // `materialize` is total, so describing a fix whose member span cannot
    // be recovered would be a panic in the server rather than a missing menu
    // entry. Establish the invariant here, where failing is free.
    if edit_span(diag, ctx).is_none() {
        return Vec::new();
    }
    vec![FixDescriptor {
        title: title(member, replacement),
        kind: CodeActionKind::QUICKFIX,
        // The checker names the exact successor member and the call keeps
        // its shape, so this is unambiguous and lossless — the B035 bar for
        // leading the menu and for joining `source.fixAll.openplanet`.
        is_preferred: true,
        auto_applicable: true,
    }]
}

fn materialize(
    diag: &TypeDiagnostic,
    ctx: &FixContext<'_>,
    _selected_title: &str,
) -> tower_lsp::lsp_types::WorkspaceEdit {
    // `describe` guarantees both of these resolve. If a future change breaks
    // that, fail loudly in development but hand the editor an empty edit
    // rather than taking the language server down mid-session.
    let Some(((_, replacement), span)) = rename(diag).zip(edit_span(diag, ctx)) else {
        debug_assert!(
            false,
            "described a removed-API fix with no recoverable member span"
        );
        return tower_lsp::lsp_types::WorkspaceEdit::default();
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        ctx.uri.clone(),
        vec![tower_lsp::lsp_types::TextEdit {
            range: span_to_range(&ctx.analysis.source, span),
            new_text: replacement.to_string(),
        }],
    );
    tower_lsp::lsp_types::WorkspaceEdit::new(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::code_actions::fix::{type_diagnostics, WorkspaceFileResolver};
    use crate::server::code_actions::test_fixtures::{apply_edits, ctx_for};
    use crate::typedb::TypeIndex;
    use tower_lsp::lsp_types::TextEdit;

    /// The engine typedb, loaded once: `awaitable` only resolves as an
    /// external type with it, and without that the checker never produces
    /// a `RemovedApiUsage` at all.
    fn typedb() -> &'static TypeIndex {
        static DB: std::sync::OnceLock<TypeIndex> = std::sync::OnceLock::new();
        DB.get_or_init(|| {
            let dir =
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
            TypeIndex::load(
                &dir.join("OpenplanetCore.json"),
                &dir.join("OpenplanetNext.json"),
            )
            .expect("typedb fixtures must load")
        })
    }

    /// Run the family over `source` and hand the closure the real analysis,
    /// the real context (typedb loaded) and the real diagnostics.
    fn with_ctx<R>(source: &str, f: impl FnOnce(&FixContext<'_>) -> R) -> R {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: Some(typedb()),
            files: WorkspaceFileResolver::empty(),
        };
        f(&ctx)
    }

    /// The `RemovedApiUsage` findings the checker really produces for
    /// `source`, with `replacement` substituted in.
    ///
    /// Every producer in the tree sets `replacement: None` (see the module
    /// docs), so this is how the `Some` branch is exercised without
    /// inventing a span: the span, member and hint are the checker's own.
    fn findings(ctx: &FixContext<'_>, replacement: Option<&str>) -> Vec<TypeDiagnostic> {
        type_diagnostics(ctx)
            .into_iter()
            .filter_map(|diag| match diag.kind {
                TypeDiagnosticKind::RemovedApiUsage { member, hint, .. } => Some(TypeDiagnostic {
                    span: diag.span,
                    kind: TypeDiagnosticKind::RemovedApiUsage {
                        member,
                        hint,
                        replacement: replacement.map(str::to_string),
                    },
                }),
                _ => None,
            })
            .collect()
    }

    /// `source` after applying every edit the family offers for its
    /// `RemovedApiUsage` findings, rewritten to carry `replacement`.
    fn fixed(source: &str, replacement: &str) -> String {
        with_ctx(source, |ctx| {
            let mut edits: Vec<TextEdit> = Vec::new();
            for diag in findings(ctx, Some(replacement)) {
                for descriptor in describe(&diag, ctx) {
                    let edit = materialize(&diag, ctx, &descriptor.title);
                    edits.extend(
                        edit.changes
                            .expect("single-file edit")
                            .remove(ctx.uri)
                            .expect("edits for the current document"),
                    );
                }
            }
            // Later edits first: applying in reverse document order keeps
            // earlier offsets valid without a merge pass.
            edits.sort_by_key(|e| std::cmp::Reverse(e.range.start));
            apply_edits(&ctx.analysis.source, &edits)
        })
    }

    /// How many fixes the family offers for `source` once its findings
    /// carry `replacement`.
    fn offer_count(source: &str, replacement: Option<&str>) -> usize {
        with_ctx(source, |ctx| {
            findings(ctx, replacement)
                .iter()
                .map(|diag| describe(diag, ctx).len())
                .sum()
        })
    }

    const AWAITABLE: &str = "void Worker() {}\nvoid Main() { startnew(Worker).WithRunContext(1); }";

    // ── Test premise ──────────────────────────────────────────────────────

    /// Everything below leans on the checker really firing here; if it
    /// stopped, the "no action offered" tests would pass vacuously.
    #[test]
    fn the_checker_publishes_the_removed_api_finding() {
        with_ctx(AWAITABLE, |ctx| {
            let found = findings(ctx, None);
            assert_eq!(found.len(), 1, "test premise: one finding, got {found:?}");
            assert!(found[0].message().contains("no longer valid"));
        });
    }

    // ── No mechanical replacement → no action ─────────────────────────────

    /// The ticket's hard rule: a diagnostic whose migration is not a span
    /// swap still gets published, but must offer NO action. This is the
    /// real `awaitable.WithRunContext`, straight from the checker.
    #[test]
    fn a_finding_with_no_structured_replacement_offers_no_fix() {
        assert_eq!(offer_count(AWAITABLE, None), 0);
    }

    /// A replacement that is not a plain member name cannot be applied by
    /// swapping the member span — `obj.Meta::New(...)` does not parse — so
    /// the family refuses it rather than writing broken code.
    #[test]
    fn a_replacement_that_is_not_a_member_name_offers_no_fix() {
        for replacement in [
            "Meta::StartWithRunContext",
            "",
            "New Name",
            "New(",
            "1New",
            "New-Name",
            "New.Name",
            // Identifier-shaped, but reserved: `obj.class(…)` does not parse.
            "class",
            "void",
            "if",
            "const",
            "null",
        ] {
            assert_eq!(
                offer_count(AWAITABLE, Some(replacement)),
                0,
                "expected no fix for replacement `{replacement}`"
            );
        }
    }

    /// A family must never describe a fix it cannot materialize: the seam
    /// declares `materialize` total, so an unmaterializable descriptor is a
    /// panic in the language server, not a missing menu entry.
    #[test]
    fn describe_offers_nothing_when_the_member_span_cannot_be_recovered() {
        with_ctx(AWAITABLE, |ctx| {
            let orphan = TypeDiagnostic {
                span: Span {
                    start: 9_999,
                    end: 10_000,
                },
                kind: TypeDiagnosticKind::RemovedApiUsage {
                    member: "WithRunContext".to_string(),
                    hint: "Meta::StartWithRunContext".to_string(),
                    replacement: Some("Restarted".to_string()),
                },
            };
            assert!(
                describe(&orphan, ctx).is_empty(),
                "describe must not offer a fix materialize would panic on"
            );
        });
    }

    /// The guard that actually does the work: the member name is the LAST
    /// non-trivia token of the diagnosed span. A span that reaches past it —
    /// here, stretched over the call's `(1)` — ends on `)`, not on an
    /// identifier, and must decline rather than rename the nearest thing it
    /// can find. `describe_offers_nothing_when_the_member_span_cannot_be_recovered`
    /// only covers a span with NO tokens in it; this one is populated.
    #[test]
    fn a_span_whose_last_token_is_not_the_member_offers_no_fix() {
        with_ctx(AWAITABLE, |ctx| {
            let mut stretched = findings(ctx, Some("Restarted")).remove(0);
            let source = &ctx.analysis.source;
            // Through the closing paren of `WithRunContext(1)`.
            let end = source.find(");").expect("test premise: the call is there") + 1;
            assert!(
                u32::try_from(end).unwrap() > stretched.span.end,
                "test premise: the stretched span must reach past the member"
            );
            stretched.span = Span {
                start: stretched.span.start,
                end: u32::try_from(end).unwrap(),
            };
            assert!(
                describe(&stretched, ctx).is_empty(),
                "a span ending on `)` must not be renamed"
            );
        });
    }

    /// The same guard from the other side: a span that ends on an
    /// identifier which is not the diagnosed member (the argument here)
    /// must decline too, or the fix would rename the argument.
    #[test]
    fn a_span_ending_on_a_different_identifier_offers_no_fix() {
        let src =
            "void Worker() {}\nvoid Main() { int arg = 1; startnew(Worker).WithRunContext(arg); }";
        with_ctx(src, |ctx| {
            let mut stretched = findings(ctx, Some("Restarted")).remove(0);
            let source = &ctx.analysis.source;
            let end = source.find("arg);").expect("test premise") + "arg".len();
            stretched.span = Span {
                start: stretched.span.start,
                end: u32::try_from(end).unwrap(),
            };
            assert!(
                describe(&stretched, ctx).is_empty(),
                "a span ending on the argument must not rename it"
            );
        });
    }

    /// A diagnostic whose recorded member disagrees with the source is
    /// stale (the document moved on). Refuse rather than rename whatever
    /// identifier happens to sit at that span.
    #[test]
    fn a_member_name_that_does_not_match_the_source_offers_no_fix() {
        with_ctx(AWAITABLE, |ctx| {
            let mut stale = findings(ctx, Some("Restarted")).remove(0);
            let TypeDiagnosticKind::RemovedApiUsage { member, .. } = &mut stale.kind else {
                unreachable!()
            };
            *member = "SomethingElse".to_string();
            assert!(describe(&stale, ctx).is_empty());
        });
    }

    // ── The rename, applied ───────────────────────────────────────────────

    #[test]
    fn a_structured_replacement_renames_the_member() {
        assert_eq!(
            fixed(AWAITABLE, "Restarted"),
            "void Worker() {}\nvoid Main() { startnew(Worker).Restarted(1); }"
        );
    }

    #[test]
    fn the_receiver_and_the_arguments_are_untouched() {
        let src =
            "void Worker() {}\nvoid Main() { startnew(Worker).WithRunContext(WithRunContext); }";
        assert_eq!(
            fixed(src, "Restarted"),
            "void Worker() {}\nvoid Main() { startnew(Worker).Restarted(WithRunContext); }"
        );
    }

    /// Only the diagnosed span is edited: a same-named method on a user
    /// class is a different symbol, produces no finding, and must survive.
    #[test]
    fn a_same_named_member_on_another_type_is_not_edited() {
        let src = "class C { void WithRunContext(int x) {} }\n\
                   void Worker() {}\n\
                   void Main() { C c; c.WithRunContext(1); startnew(Worker).WithRunContext(1); }";
        assert_eq!(
            fixed(src, "Restarted"),
            "class C { void WithRunContext(int x) {} }\n\
             void Worker() {}\n\
             void Main() { C c; c.WithRunContext(1); startnew(Worker).Restarted(1); }"
        );
    }

    /// Two diagnosed uses on one line: one edit each, and applying them
    /// together must not corrupt the second's offsets.
    #[test]
    fn two_uses_on_one_line_are_both_fixed_without_corrupting_offsets() {
        let src = "void Worker() {}\n\
                   void Main() { startnew(Worker).WithRunContext(1); startnew(Worker).WithRunContext(2); }";
        assert_eq!(offer_count(src, Some("Restarted")), 2);
        assert_eq!(
            fixed(src, "Restarted"),
            "void Worker() {}\n\
             void Main() { startnew(Worker).Restarted(1); startnew(Worker).Restarted(2); }"
        );
    }

    /// Comments and line breaks inside the member expression are trivia,
    /// so the token walk still finds the name.
    #[test]
    fn a_comment_inside_the_member_expression_is_handled() {
        let src =
            "void Worker() {}\nvoid Main() { startnew(Worker) /* why */ . WithRunContext(1); }";
        assert_eq!(
            fixed(src, "Restarted"),
            "void Worker() {}\nvoid Main() { startnew(Worker) /* why */ . Restarted(1); }"
        );
    }

    #[test]
    fn a_multiline_member_expression_is_handled() {
        let src =
            "void Worker() {}\nvoid Main() {\n    startnew(Worker)\n        .WithRunContext(1);\n}";
        assert_eq!(
            fixed(src, "Restarted"),
            "void Worker() {}\nvoid Main() {\n    startnew(Worker)\n        .Restarted(1);\n}"
        );
    }

    // ── Metadata ──────────────────────────────────────────────────────────

    /// A documented, exact rename is unambiguous and lossless, so it leads
    /// the menu and joins `source.fixAll.openplanet`.
    #[test]
    fn the_fix_is_preferred_auto_applicable_and_names_both_members() {
        with_ctx(AWAITABLE, |ctx| {
            let diag = findings(ctx, Some("Restarted")).remove(0);
            let described = describe(&diag, ctx);
            assert_eq!(described.len(), 1);
            let descriptor = &described[0];
            assert_eq!(descriptor.kind, CodeActionKind::QUICKFIX);
            assert!(descriptor.is_preferred);
            assert!(descriptor.auto_applicable);
            assert!(
                descriptor.title.contains("WithRunContext")
                    && descriptor.title.contains("Restarted"),
                "title should name both members, got {}",
                descriptor.title
            );
        });
    }

    /// `materialize` edits the current document and nothing else.
    #[test]
    fn the_edit_is_local_to_the_current_document() {
        with_ctx(AWAITABLE, |ctx| {
            let diag = findings(ctx, Some("Restarted")).remove(0);
            let descriptor = describe(&diag, ctx).remove(0);
            let changes = materialize(&diag, ctx, &descriptor.title)
                .changes
                .expect("single-file edit");
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[ctx.uri].len(), 1);
            assert_eq!(changes[ctx.uri][0].new_text, "Restarted");
        });
    }

    /// The family claims its own kind and nothing else.
    #[test]
    fn the_family_claims_only_removed_api_usage() {
        assert!(matches(&TypeDiagnosticKind::RemovedApiUsage {
            member: "M".to_string(),
            hint: "H".to_string(),
            replacement: None,
        }));
        assert!(!matches(&TypeDiagnosticKind::StringByValueParam {
            param_name: "s".to_string(),
        }));
        assert!(!matches(&TypeDiagnosticKind::UndefinedMember {
            object_type: "T".to_string(),
            member: "M".to_string(),
        }));
    }

    /// End-to-end, the way the editor drives it: ask
    /// `textDocument/codeAction` at the DIAGNOSTIC'S OWN RANGE for the real
    /// `awaitable.WithRunContext` finding, and assert that no RENAME comes
    /// back. Every producer in the tree sets `replacement: None`, and
    /// asserting it through the handler is what proves the "never offer a
    /// fix you cannot make" rule holds where the user meets it, not just
    /// inside `describe`.
    ///
    /// Since B048 the client is not left with nothing here — the migration
    /// is offered as a call restructure by `super::with_run_context`, whose
    /// own `the_rename_family_offers_nothing_for_this_diagnostic` asserts
    /// the same division from the other side.
    #[test]
    fn requesting_at_the_diagnostic_range_offers_no_rename_for_the_real_finding() {
        use crate::server::code_actions::{code_actions, CodeActionRequest};
        use crate::server::diagnostics::span_to_range;
        use tower_lsp::lsp_types::{CodeActionContext, CodeActionOrCommand};

        let (analysis, workspace, uri) = ctx_for(AWAITABLE);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: Some(typedb()),
            files: WorkspaceFileResolver::empty(),
        };
        let found = findings(&ctx, None);
        assert_eq!(found.len(), 1, "test premise: the finding is published");
        let range = span_to_range(&analysis.source, found[0].span);

        let files = [(uri.clone(), &analysis)];
        let titles: Vec<_> = code_actions(&CodeActionRequest {
            uri: &uri,
            analysis: &analysis,
            range,
            context: &CodeActionContext::default(),
            workspace: &workspace,
            type_index: Some(typedb()),
            files: WorkspaceFileResolver::new(&files),
            lazy_edits: false,
        })
        .into_iter()
        .filter_map(|action| match action {
            CodeActionOrCommand::CodeAction(action) => Some(action.title),
            CodeActionOrCommand::Command(_) => None,
        })
        .collect();
        assert!(
            !titles.iter().any(|title| title.starts_with("Replace `")),
            "the migration is not a span swap, so no rename may be offered; got {titles:?}"
        );
        // Not vacuous: the handler really did reach this diagnostic.
        assert!(
            titles
                .iter()
                .any(|title| title.contains("Meta::StartWithRunContext")),
            "test premise: the restructure family answers this finding; got {titles:?}"
        );
    }

    /// The seam re-enters a family by name on `codeAction/resolve`; a
    /// renamed family silently stops resolving.
    #[test]
    fn the_family_name_is_stable() {
        assert_eq!(FAMILY.name, "removed-api-usage");
    }
}
