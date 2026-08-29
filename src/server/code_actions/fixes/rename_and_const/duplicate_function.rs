//! Fix family: rename the duplicate of two identically-signed functions.
//!
//! Answers [`TypeDiagnosticKind::DuplicateFunction`] — the game's
//! `A function with the same name and parameters already exists`, an ERROR
//! that aborts the compile. The checker anchors it on the *second* (and
//! later) declaration, and the fix is offered only there: which of the two
//! should keep the name is the user's call, so the action renames the one
//! the squiggle is on and never reaches for the other.
//!
//! **Only the declaration is renamed — never a call site.** Two
//! declarations with the same qualified name and the same parameter types
//! are indistinguishable at every call site, so no call can be attributed
//! to one of them. Leaving the calls alone is also what the game does
//! after the user fixes this by hand: existing calls keep resolving to the
//! first declaration, and the renamed one starts out uncalled. Renaming
//! call sites would be a guess, and a guess that silently redirects
//! behaviour.
//!
//! Never `is_preferred`, never `auto_applicable` (B038 decision): this
//! changes an API surface, and the equally valid alternative — delete the
//! duplicate, or rename the *other* one — cannot be distinguished here.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::parser::ast::{Item, SourceFile};
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{GlobalScope, TypeDiagnostic, TypeDiagnosticKind};

use super::scope;

pub const FAMILY: FixFamily = FixFamily {
    name: "rename-duplicate-function",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::DuplicateFunction { .. })
}

struct RenamePlan {
    name: String,
    new_name: String,
    name_span: Span,
}

fn title(plan: &RenamePlan) -> String {
    format!(
        "Rename this duplicate `{}` to `{}`",
        plan.name, plan.new_name
    )
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    plan(diag, ctx)
        .map(|plan| FixDescriptor {
            title: title(&plan),
            kind: CodeActionKind::QUICKFIX,
            is_preferred: false,
            auto_applicable: false,
        })
        .into_iter()
        .collect()
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, _title: &str) -> WorkspaceEdit {
    let Some(plan) = plan(diag, ctx) else {
        debug_assert!(
            false,
            "described a duplicate-function rename with no recoverable plan"
        );
        return WorkspaceEdit::default();
    };
    WorkspaceEdit::new(std::collections::HashMap::from([(
        ctx.uri.clone(),
        vec![TextEdit {
            range: span_to_range(&ctx.analysis.source, plan.name_span),
            new_text: plan.new_name,
        }],
    )]))
}

fn plan(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<RenamePlan> {
    let TypeDiagnosticKind::DuplicateFunction { function_name } = &diag.kind else {
        return None;
    };
    let analysis = ctx.analysis;
    // The diagnostic is anchored on the whole declaration, so the name is
    // recovered from the AST rather than by scanning text — a function may
    // legitimately share its name with a type mentioned in its signature.
    let name_span = function_name_span(&analysis.file, diag.span)?;
    if name_span.text(analysis.masked_source()) != function_name {
        return None;
    }
    let global_scope = GlobalScope::new(ctx.workspace, ctx.type_index);
    let new_name = scope::fresh_name(function_name, &|candidate| {
        scope::name_occurs_in_file(&analysis.tokens, analysis.masked_source(), candidate)
            || global_scope.has_global_ident(candidate)
    })?;
    Some(RenamePlan {
        name: function_name.clone(),
        new_name,
        name_span,
    })
}

/// Name span of the top-level function declared over exactly `decl_span`.
fn function_name_span(file: &SourceFile, decl_span: Span) -> Option<Span> {
    fn walk(items: &[Item], decl_span: Span) -> Option<Span> {
        for item in items {
            match item {
                Item::Function(func) if func.span == decl_span => return Some(func.name.span),
                Item::Namespace(ns) => {
                    if let Some(found) = walk(&ns.items, decl_span) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(&file.items, decl_span)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{applied, edits_for, fixes_in, Workspace};
    use super::*;
    use crate::server::code_actions::fix::FixCandidate;

    fn duplicate_fixes(workspace: &Workspace) -> Vec<FixCandidate> {
        fixes_in(workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .collect()
    }

    fn fix_count(source: &str) -> usize {
        duplicate_fixes(&Workspace::single(source)).len()
    }

    fn fixed(source: &str) -> String {
        let workspace = Workspace::single(source);
        let candidates = duplicate_fixes(&workspace);
        assert_eq!(candidates.len(), 1, "expected exactly one offer");
        applied(source, &candidates[0].fix.edit, workspace.uri(0))
    }

    // ── Diagnosed shapes ─────────────────────────────────────────────────

    #[test]
    fn the_second_of_two_identical_declarations_is_renamed() {
        assert_eq!(
            fixed("void Foo() {} void Foo() {}"),
            "void Foo() {} void Foo2() {}"
        );
    }

    #[test]
    fn identical_parameter_lists_collide_whatever_the_parameters_are_called() {
        assert_eq!(
            fixed("void Foo(int a) {} void Foo(int b) {}"),
            "void Foo(int a) {} void Foo2(int b) {}"
        );
    }

    #[test]
    fn a_return_type_difference_does_not_make_them_distinct() {
        // AngelScript overloads on parameters only, so these still collide.
        assert_eq!(
            fixed("void Foo() {} int Foo() { return 1; }"),
            "void Foo() {} int Foo2() { return 1; }"
        );
    }

    #[test]
    fn namespaced_duplicates_in_the_same_namespace_are_fixable() {
        assert_eq!(
            fixed("namespace N { void Foo() {} void Foo() {} }"),
            "namespace N { void Foo() {} void Foo2() {} }"
        );
    }

    #[test]
    fn three_duplicates_offer_one_fix_each_after_the_first() {
        assert_eq!(fix_count("void Foo() {} void Foo() {} void Foo() {}"), 2);
    }

    // ── Regression guards: legal code must stay silent ───────────────────

    #[test]
    fn overloads_with_different_parameter_types_are_legal() {
        assert_eq!(fix_count("void Foo(int a) {} void Foo(float a) {}"), 0);
    }

    #[test]
    fn overloads_with_different_parameter_counts_are_legal() {
        assert_eq!(fix_count("void Foo() {} void Foo(int a) {}"), 0);
    }

    #[test]
    fn the_same_name_in_different_namespaces_is_legal() {
        assert_eq!(
            fix_count("namespace A { void Foo() {} } namespace B { void Foo() {} }"),
            0
        );
    }

    #[test]
    fn a_namespaced_function_does_not_collide_with_a_global_one() {
        assert_eq!(fix_count("void Foo() {} namespace N { void Foo() {} }"), 0);
    }

    #[test]
    fn class_methods_are_a_separate_world_and_not_diagnosed() {
        assert_eq!(fix_count("class C { void M() {} void M() {} }"), 0);
    }

    #[test]
    fn a_single_declaration_offers_nothing() {
        assert_eq!(fix_count("void Foo() {}"), 0);
    }

    // ── Call sites ───────────────────────────────────────────────────────

    #[test]
    fn only_the_declaration_is_renamed_never_a_call_site() {
        // With two identical signatures no call can be attributed to
        // either declaration, and the surviving calls keep resolving to
        // the first — which is what the game does once the duplicate is
        // renamed by hand.
        let src = "void Foo() {} void Foo() {} void Bar() { Foo(); Foo(); }";
        let workspace = Workspace::single(src);
        let candidates = duplicate_fixes(&workspace);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            edits_for(&candidates[0].fix.edit, workspace.uri(0)).len(),
            1
        );
        assert_eq!(
            applied(src, &candidates[0].fix.edit, workspace.uri(0)),
            "void Foo() {} void Foo2() {} void Bar() { Foo(); Foo(); }"
        );
    }

    #[test]
    fn the_first_declaration_is_never_touched() {
        let src = "void Foo() {} void Foo() {}";
        let workspace = Workspace::single(src);
        let candidates = duplicate_fixes(&workspace);
        let edits = edits_for(&candidates[0].fix.edit, workspace.uri(0));
        assert_eq!(edits.len(), 1);
        assert!(edits[0].range.start.character > 14, "{:?}", edits[0].range);
    }

    // ── Name generation ──────────────────────────────────────────────────

    #[test]
    fn the_suggested_name_avoids_an_existing_symbol() {
        assert_eq!(
            fixed("void Foo2() {} void Foo() {} void Foo() {}"),
            "void Foo2() {} void Foo() {} void Foo3() {}"
        );
    }

    #[test]
    fn the_suggested_name_avoids_a_symbol_from_another_workspace_file() {
        let workspace = Workspace::new(&[
            ("file:///test/a.as", "void Foo() {} void Foo() {}"),
            ("file:///test/b.as", "void Foo2() {}"),
        ]);
        let candidates = duplicate_fixes(&workspace);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            applied(
                workspace.source(0),
                &candidates[0].fix.edit,
                workspace.uri(0)
            ),
            "void Foo() {} void Foo3() {}"
        );
    }

    #[test]
    fn a_name_that_only_looks_like_the_type_is_not_edited() {
        // The diagnostic is anchored on the whole declaration, so a
        // producer scanning text could hit the return type instead.
        assert_eq!(
            fixed("class Foo {} void f(Foo a) {} void f(Foo a) {}"),
            "class Foo {} void f(Foo a) {} void f2(Foo a) {}"
        );
    }

    // ── Action metadata ──────────────────────────────────────────────────

    #[test]
    fn the_fix_is_neither_preferred_nor_auto_applicable() {
        // Which of the two keeps the name is the user's call, and this
        // changes an API surface — never applied on save.
        let workspace = Workspace::single("void Foo() {} void Foo() {}");
        let candidates = duplicate_fixes(&workspace);
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].fix.is_preferred);
        assert!(!candidates[0].fix.auto_applicable);
        assert_eq!(candidates[0].fix.kind, CodeActionKind::QUICKFIX);
        assert_eq!(
            candidates[0].fix.title,
            "Rename this duplicate `Foo` to `Foo2`"
        );
    }

    #[test]
    fn describe_offers_nothing_for_a_declaration_it_cannot_find() {
        let workspace = Workspace::single("void Foo() {} void Foo() {}");
        let pairs = workspace.pairs();
        let ctx = super::super::test_support::context(&workspace, &pairs, 0);
        let orphan = TypeDiagnostic {
            span: Span {
                start: 9_000,
                end: 9_100,
            },
            kind: TypeDiagnosticKind::DuplicateFunction {
                function_name: "Foo".to_string(),
            },
        };
        assert!(describe(&orphan, &ctx).is_empty());
    }
}
