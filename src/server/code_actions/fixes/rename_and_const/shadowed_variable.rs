//! Fix family: rename the inner declaration of a shadowing local.
//!
//! Answers [`TypeDiagnosticKind::VariableShadow`] — the game's
//! `Variable 'x' hides another variable of same name in outer scope`. The
//! fix renames the **inner** binding and every reference that resolves to
//! it, and must not touch a single reference that resolves to the outer
//! one. That asymmetry is the whole risk of the fix, so the occurrence set
//! is computed from a lexical scope model ([`super::scope`]) rather than
//! from the server's existing name-matching rename, which rewrites every
//! matching identifier in the workspace.
//!
//! The family refuses rather than guesses. It offers nothing when:
//!
//! * the declarator's scope cannot be determined;
//! * a nested anonymous function (`function() { … }`) appears inside the
//!   scope — a lambda gets its own checker frame, so an identifier inside
//!   one is not reliably our local;
//! * the name is also a workspace or engine global, where a bare
//!   identifier in the scope may resolve past the local;
//! * the same name is declared a second time in the *same* scope (the
//!   game's `'x' is already declared`), where the later uses' binding is
//!   not ours to move;
//! * no free `name<n>` candidate exists.
//!
//! Never `is_preferred`, never `auto_applicable` (B038 decision): renaming
//! a variable is a judgement call, and the alternative — renaming the
//! *outer* one — is equally valid and cannot be distinguished by the tool.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::{Span, TokenKind};
use crate::parser::ast::{
    ClassMember, FunctionBody, Item, SourceFile, Stmt, StmtKind, VarDeclStmt,
};
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{GlobalScope, TypeDiagnostic, TypeDiagnosticKind};

use super::scope;

pub const FAMILY: FixFamily = FixFamily {
    name: "rename-shadowed-variable",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::VariableShadow { .. })
}

/// The rename this family would perform: every span to rewrite, and what
/// to rewrite them to.
struct RenamePlan {
    name: String,
    new_name: String,
    spans: Vec<Span>,
}

fn title(plan: &RenamePlan) -> String {
    format!(
        "Rename the shadowing `{}` to `{}`",
        plan.name, plan.new_name
    )
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    // `materialize` is declared total over what this returns, so the whole
    // plan is established here — where failing costs a missing menu entry
    // rather than an empty edit in the user's hands.
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
        debug_assert!(false, "described a shadow rename with no recoverable plan");
        return WorkspaceEdit::default();
    };
    let edits = plan
        .spans
        .iter()
        .map(|span| TextEdit {
            range: span_to_range(&ctx.analysis.source, *span),
            new_text: plan.new_name.clone(),
        })
        .collect();
    WorkspaceEdit::new(std::collections::HashMap::from([(ctx.uri.clone(), edits)]))
}

fn plan(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<RenamePlan> {
    let TypeDiagnosticKind::VariableShadow { name } = &diag.kind else {
        return None;
    };
    let analysis = ctx.analysis;
    let source = analysis.masked_source();
    let decl_start = diag.span.start;
    let scope_end = scope::innermost_scope_end(&analysis.file, &analysis.tokens, decl_start)?;

    // A nested lambda opens a new checker frame, so `x` inside it may be
    // the lambda's own binding rather than ours. Refuse.
    if analysis.tokens.iter().any(|tok| {
        tok.kind == TokenKind::KwFunction
            && tok.span.start > decl_start
            && tok.span.end <= scope_end
    }) {
        return None;
    }

    let global_scope = GlobalScope::new(ctx.workspace, ctx.type_index);
    if global_scope.has_global_ident(name) {
        return None;
    }

    let mut excluded = Vec::new();
    for redecl in
        scope::redeclarations_in_range(&analysis.file, source, name, decl_start, scope_end)
    {
        let end = scope::innermost_scope_end(&analysis.file, &analysis.tokens, redecl)?;
        if end >= scope_end {
            // Same scope as ours (`'x' is already declared`): the later
            // uses bind to the FIRST declaration, which may or may not be
            // ours. Not a rename we can get right.
            return None;
        }
        // The checker evaluates the nested declaration's initializer
        // before binding the nested name. Exclude its declarator and all
        // later uses, but leave its initializer available to the parent
        // rename plan because those references still resolve to us.
        if let Some(init) = own_initializer_span(&analysis.file, redecl) {
            excluded.push((redecl, init.start));
            excluded.push((init.end, end));
        } else {
            excluded.push((redecl, end));
        }
    }
    // `int x = x;` initialises from the OUTER binding — the checker types
    // the initialiser before defining the local. Rewriting it would change
    // which variable the initialiser reads.
    if let Some(init) = own_initializer_span(&analysis.file, decl_start) {
        excluded.push((init.start, init.end));
    }

    let new_name = scope::fresh_name(name, &|candidate| {
        scope::name_occurs_in_file(&analysis.tokens, source, candidate)
            || global_scope.has_global_ident(candidate)
    })?;

    let spans = scope::local_occurrences(
        &analysis.tokens,
        source,
        name,
        decl_start,
        scope_end,
        &excluded,
    );
    // Renaming uses without renaming the binding would be worse than the
    // warning. The declarator is always the first span in range.
    if spans.first().map(|span| span.start) != Some(decl_start) {
        return None;
    }
    Some(RenamePlan {
        name: name.clone(),
        new_name,
        spans,
    })
}

/// Span of the initialiser expression belonging to the declarator that
/// starts at `decl_start`, if it has one.
fn own_initializer_span(file: &SourceFile, decl_start: u32) -> Option<Span> {
    let mut found = None;
    for item in &file.items {
        visit_item_var_decls(item, &mut |var| {
            for declarator in &var.declarators {
                if declarator.name.span.start == decl_start {
                    found = declarator.init.as_ref().map(|init| init.span);
                }
            }
        });
    }
    found
}

fn visit_item_var_decls(item: &Item, f: &mut impl FnMut(&VarDeclStmt)) {
    match item {
        Item::Function(func) => {
            if let Some(body) = &func.body {
                visit_body_var_decls(body, f);
            }
        }
        Item::Class(cls) => {
            for member in &cls.members {
                match member {
                    ClassMember::Method(m)
                    | ClassMember::Constructor(m)
                    | ClassMember::Destructor(m) => {
                        if let Some(body) = &m.body {
                            visit_body_var_decls(body, f);
                        }
                    }
                    ClassMember::Property(prop) => {
                        for body in prop
                            .getter
                            .iter()
                            .chain(prop.setter.iter().map(|(_, body)| body))
                        {
                            visit_body_var_decls(body, f);
                        }
                    }
                    ClassMember::Field(_) => {}
                }
            }
        }
        Item::Namespace(ns) => {
            for nested in &ns.items {
                visit_item_var_decls(nested, f);
            }
        }
        Item::Property(prop) => {
            for body in prop
                .getter
                .iter()
                .chain(prop.setter.iter().map(|(_, body)| body))
            {
                visit_body_var_decls(body, f);
            }
        }
        _ => {}
    }
}

fn visit_body_var_decls(body: &FunctionBody, f: &mut impl FnMut(&VarDeclStmt)) {
    for stmt in &body.stmts {
        visit_stmt_var_decls(stmt, f);
    }
}

fn visit_stmt_var_decls(stmt: &Stmt, f: &mut impl FnMut(&VarDeclStmt)) {
    match &stmt.kind {
        StmtKind::VarDecl(var) => f(var),
        StmtKind::Block(stmts) => {
            for nested in stmts {
                visit_stmt_var_decls(nested, f);
            }
        }
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            visit_stmt_var_decls(then_branch, f);
            if let Some(branch) = else_branch {
                visit_stmt_var_decls(branch, f);
            }
        }
        StmtKind::For { init, body, .. } => {
            if let Some(init) = init {
                visit_stmt_var_decls(init, f);
            }
            visit_stmt_var_decls(body, f);
        }
        StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => {
            visit_stmt_var_decls(body, f)
        }
        StmtKind::Switch { cases, .. } => {
            for case in cases {
                for nested in &case.stmts {
                    visit_stmt_var_decls(nested, f);
                }
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            visit_stmt_var_decls(try_body, f);
            visit_stmt_var_decls(catch_body, f);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{applied, edits_for, fixes_for, Workspace};
    use super::*;
    use crate::server::code_actions::fix::FixCandidate;

    fn shadow_fixes(source: &str) -> Vec<FixCandidate> {
        fixes_for(source)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .collect()
    }

    /// The text after applying the family's single offer for `source`.
    fn fixed(source: &str) -> String {
        let workspace = Workspace::single(source);
        let candidates: Vec<_> = super::super::test_support::fixes_in(&workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .collect();
        assert_eq!(candidates.len(), 1, "expected exactly one offer");
        applied(source, &candidates[0].fix.edit, workspace.uri(0))
    }

    fn edit_count(source: &str) -> usize {
        let workspace = Workspace::single(source);
        let candidates: Vec<_> = super::super::test_support::fixes_in(&workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .collect();
        assert_eq!(candidates.len(), 1, "expected exactly one offer");
        edits_for(&candidates[0].fix.edit, workspace.uri(0)).len()
    }

    // ── The shapes the checker actually diagnoses ────────────────────────

    #[test]
    fn local_shadowing_a_local_is_renamed() {
        assert_eq!(
            fixed("void f() { int x = 1; { int x = 2; x = x + 1; } x = 3; }"),
            "void f() { int x = 1; { int x2 = 2; x2 = x2 + 1; } x = 3; }"
        );
    }

    #[test]
    fn local_shadowing_a_parameter_is_renamed() {
        // Parameters live in the function's outermost frame, so a local
        // shadows one only from a nested block.
        assert_eq!(
            fixed("void f(int x) { { int x = 2; x++; } x = 3; }"),
            "void f(int x) { { int x2 = 2; x2++; } x = 3; }"
        );
    }

    #[test]
    fn shadow_inside_a_nested_block_is_renamed() {
        assert_eq!(
            fixed("void f() { int x = 1; { { int x = 2; x++; } } x++; }"),
            "void f() { int x = 1; { { int x2 = 2; x2++; } } x++; }"
        );
    }

    #[test]
    fn shadow_inside_a_loop_body_is_renamed() {
        assert_eq!(
            fixed("void f() { int i = 0; for (int k = 0; k < 2; k++) { int i = 1; i++; } i++; }"),
            "void f() { int i = 0; for (int k = 0; k < 2; k++) { int i2 = 1; i2++; } i++; }"
        );
    }

    #[test]
    fn shadow_inside_a_while_body_is_renamed() {
        assert_eq!(
            fixed("void f(bool c) { int x = 1; while (c) { int x = 2; x++; } x++; }"),
            "void f(bool c) { int x = 1; while (c) { int x2 = 2; x2++; } x++; }"
        );
    }

    #[test]
    fn shadow_inside_a_switch_case_is_renamed_without_reaching_the_next_case() {
        assert_eq!(
            fixed(
                "void f(int v) { int a = 0; switch (v) { case 1: { int a = 1; a++; } break; \
                 case 2: a++; break; } }"
            ),
            "void f(int v) { int a = 0; switch (v) { case 1: { int a2 = 1; a2++; } break; \
             case 2: a++; break; } }"
        );
    }

    #[test]
    fn shadow_inside_an_if_branch_is_renamed() {
        assert_eq!(
            fixed("void f(bool c) { int x = 1; if (c) { int x = 2; x++; } else { x++; } }"),
            "void f(bool c) { int x = 1; if (c) { int x2 = 2; x2++; } else { x++; } }"
        );
    }

    // ── The whole risk of the fix: inner yes, outer never ────────────────

    #[test]
    fn rename_updates_every_inner_reference_and_no_outer_one() {
        let src = "void f() {\n\
                   \x20   int x = 1;\n\
                   \x20   x = x + 1;\n\
                   \x20   {\n\
                   \x20       int x = 2;\n\
                   \x20       x = x * 2;\n\
                   \x20       x++;\n\
                   \x20   }\n\
                   \x20   x = x - 1;\n\
                   }";
        // Declarator, both `x` in `x = x * 2`, and the `x` in `x++` —
        // four edits, and nothing outside the block.
        assert_eq!(edit_count(src), 4);
        let out = fixed(src);
        assert!(out.contains("int x2 = 2;"), "{out}");
        assert!(out.contains("x2 = x2 * 2;"), "{out}");
        assert!(out.contains("x2++;"), "{out}");
        assert!(out.contains("int x = 1;"), "{out}");
        assert!(out.contains("x = x + 1;"), "{out}");
        assert!(out.contains("x = x - 1;"), "{out}");
    }

    #[test]
    fn a_nested_reshadow_keeps_its_own_references() {
        // The middle `x` is renamed; the deepest block redeclares `x`, so
        // its uses belong to that binding and must be left alone.
        let src = "void f() { int x = 1; { int x = 2; x++; { int x = 3; x++; } x++; } }";
        let workspace = Workspace::single(src);
        let middle = super::super::test_support::fixes_in(&workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .min_by_key(|candidate| candidate.diagnostic.range.start.character)
            .expect("a fix on the middle declaration");
        assert_eq!(
            applied(src, &middle.fix.edit, workspace.uri(0)),
            "void f() { int x = 1; { int x2 = 2; x2++; { int x = 3; x++; } x2++; } }"
        );
    }

    #[test]
    fn a_nested_redeclarations_initializer_keeps_the_parent_binding() {
        // The checker evaluates a declaration's initializer before adding
        // that declaration to its frame. The deepest `x` therefore reads
        // the middle binding, even though its later uses are its own.
        let src = "void f() { int x = 0; { int x = 1; { int x = x + 1; x++; } x++; } }";
        let workspace = Workspace::single(src);
        let middle = super::super::test_support::fixes_in(&workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .min_by_key(|candidate| candidate.diagnostic.range.start.character)
            .expect("a fix on the middle declaration");
        assert_eq!(
            applied(src, &middle.fix.edit, workspace.uri(0)),
            "void f() { int x = 0; { int x2 = 1; { int x = x2 + 1; x++; } x2++; } }"
        );
    }

    #[test]
    fn nested_redeclaration_initializers_all_keep_the_parent_binding() {
        let src =
            "void f() { int x = 0; { int x = 1; { int x = x + 1; } { int x = x + 2; } x++; } }";
        let workspace = Workspace::single(src);
        let middle = super::super::test_support::fixes_in(&workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .min_by_key(|candidate| candidate.diagnostic.range.start.character)
            .expect("a fix on the middle declaration");
        assert_eq!(
            applied(src, &middle.fix.edit, workspace.uri(0)),
            "void f() { int x = 0; { int x2 = 1; { int x = x2 + 1; } { int x = x2 + 2; } x2++; } }"
        );
    }

    #[test]
    fn a_member_named_like_the_local_is_not_renamed() {
        assert_eq!(
            fixed("class C { int x; } void f() { int x = 1; { int x = 2; C c; c.x = x; } }"),
            "class C { int x; } void f() { int x = 1; { int x2 = 2; C c; c.x = x2; } }"
        );
    }

    #[test]
    fn the_declarations_own_initializer_reads_the_outer_binding() {
        // `int x = x + 1;` is typed against the OUTER `x` (the checker
        // defines the local only after the initialiser), so rewriting the
        // initialiser would change which variable it reads.
        assert_eq!(
            fixed("void f() { int x = 1; { int x = x + 1; x++; } }"),
            "void f() { int x = 1; { int x2 = x + 1; x2++; } }"
        );
    }

    // ── Name generation ──────────────────────────────────────────────────

    #[test]
    fn the_suggested_name_avoids_one_already_in_the_file() {
        assert_eq!(
            fixed("void f() { int x = 1; int x2 = 2; { int x = 3; x = 4; } }"),
            "void f() { int x = 1; int x2 = 2; { int x3 = 3; x3 = 4; } }"
        );
    }

    #[test]
    fn the_suggested_name_avoids_several_already_in_the_file() {
        assert_eq!(
            fixed("void f() { int x2 = 0; int x3 = 0; int x = 1; { int x = 3; x++; } }"),
            "void f() { int x2 = 0; int x3 = 0; int x = 1; { int x4 = 3; x4++; } }"
        );
    }

    #[test]
    fn the_suggested_name_avoids_a_name_used_anywhere_in_the_file() {
        // Blunt on purpose: `x2` is another function's local, so it cannot
        // collide here — but over-rejecting costs one suffix, while
        // under-rejecting would capture a symbol.
        assert_eq!(
            fixed("void g() { int x2 = 0; } void f() { int x = 1; { int x = 2; x++; } }"),
            "void g() { int x2 = 0; } void f() { int x = 1; { int x3 = 2; x3++; } }"
        );
    }

    #[test]
    fn the_underscore_suppression_prefix_survives_the_rename() {
        // A leading `_` is Openplanet's documented "ignore this" marker; a
        // rename that dropped it would turn on a warning the author had
        // deliberately silenced.
        assert_eq!(
            fixed("void f() { int _x = 1; { int _x = 2; _x++; } }"),
            "void f() { int _x = 1; { int _x2 = 2; _x2++; } }"
        );
    }

    // ── Guards: the checker does not diagnose these at all ───────────────

    #[test]
    fn shadowing_a_class_field_is_not_diagnosed_so_no_fix_is_offered() {
        assert!(shadow_fixes("class C { int x; void M() { { int x = 1; } } }").is_empty());
    }

    #[test]
    fn shadowing_a_global_is_not_diagnosed_so_no_fix_is_offered() {
        assert!(shadow_fixes("int x = 1; void f() { { int x = 2; } }").is_empty());
    }

    #[test]
    fn a_local_that_shadows_nothing_offers_no_fix() {
        assert!(shadow_fixes("void f() { int x = 1; { int y = 2; } }").is_empty());
    }

    #[test]
    fn two_siblings_in_unrelated_blocks_are_not_shadowing() {
        assert!(shadow_fixes("void f() { { int x = 1; } { int x = 2; } }").is_empty());
    }

    // ── Guards: diagnosed, but the fix refuses to guess ──────────────────

    #[test]
    fn a_second_declaration_in_the_same_scope_blocks_the_fix() {
        // `'x' is already declared` — the later uses bind to the first
        // declaration, which may not be the one we would rename.
        assert!(shadow_fixes("void f() { int x = 1; { int x = 2; int x = 3; } }").is_empty());
    }

    #[test]
    fn a_lambda_inside_the_scope_blocks_the_fix() {
        // An anonymous function gets its own checker frame, so an `x`
        // inside it is not reliably the local we are renaming.
        assert!(shadow_fixes(
            "void f() { int x = 1; { int x = 2; auto g = function() { return 1; }; } }"
        )
        .is_empty());
    }

    #[test]
    fn a_lambda_before_the_declaration_does_not_block_the_fix() {
        assert_eq!(
            fixed("void f() { int x = 1; { auto g = function() { return 1; }; int x = 2; x++; } }"),
            "void f() { int x = 1; { auto g = function() { return 1; }; int x2 = 2; x2++; } }"
        );
    }

    #[test]
    fn a_name_that_is_also_a_global_blocks_the_fix() {
        // A bare `x` in the block could resolve past the local, so the
        // occurrence set is not ours to compute.
        assert!(shadow_fixes("int x = 1; void f() { int x = 2; { int x = 3; x++; } }").is_empty());
    }

    #[test]
    fn a_name_that_is_also_a_workspace_function_blocks_the_fix() {
        assert!(
            shadow_fixes("void Run() {} void f() { int Run = 1; { int Run = 2; } }").is_empty()
        );
    }

    // ── Action metadata ──────────────────────────────────────────────────

    #[test]
    fn the_fix_is_neither_preferred_nor_auto_applicable() {
        // Renaming the OUTER declaration is just as valid; nothing in the
        // source says which the author meant. Never applied on save.
        let candidates = shadow_fixes("void f() { int x = 1; { int x = 2; x++; } }");
        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].fix.is_preferred);
        assert!(!candidates[0].fix.auto_applicable);
        assert_eq!(candidates[0].fix.kind, CodeActionKind::QUICKFIX);
        assert_eq!(candidates[0].fix.title, "Rename the shadowing `x` to `x2`");
    }

    // ── Source-text hazards ──────────────────────────────────────────────

    #[test]
    fn a_string_literal_holding_the_name_is_not_renamed() {
        // The analysis masks literals to same-length blanks, so no `Ident`
        // token ever lands inside one — assert it, because the whole
        // occurrence set depends on that.
        assert_eq!(
            fixed(r#"void f() { int x = 1; { int x = 2; print("x = " + x); } }"#),
            r#"void f() { int x = 1; { int x2 = 2; print("x = " + x2); } }"#
        );
    }

    #[test]
    fn a_comment_holding_the_name_is_not_renamed() {
        assert_eq!(
            fixed("void f() { int x = 1; { int x = 2; /* x */ x++; // x\n } }"),
            "void f() { int x = 1; { int x2 = 2; /* x */ x2++; // x\n } }"
        );
    }

    #[test]
    fn multibyte_text_before_the_declaration_does_not_shift_the_edits() {
        // Spans are byte offsets and LSP positions are UTF-16 columns, so
        // a non-ASCII prefix is where an off-by-one would show up.
        assert_eq!(
            fixed("void f() { /* héllo ✓ */ int x = 1; { int x = 2; x++; } }"),
            "void f() { /* héllo ✓ */ int x = 1; { int x2 = 2; x2++; } }"
        );
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        assert_eq!(
            fixed("void f() {\r\n    int x = 1;\r\n    {\r\n        int x = 2;\r\n        x++;\r\n    }\r\n}"),
            "void f() {\r\n    int x = 1;\r\n    {\r\n        int x2 = 2;\r\n        x2++;\r\n    }\r\n}"
        );
    }

    #[test]
    fn a_truncated_or_unbalanced_source_never_panics() {
        // `materialize` runs inside `textDocument/codeAction`; a panic
        // there takes the server down for every later request.
        for src in [
            "void f() { int x = 1; { int x = 2; x++;",
            "void f() { int x = 1; { int x = 2; }",
            "void f() { int x = 1; { { int x = 2; x++; }",
            "void f( { int x = 1; { int x = 2; } }",
            "void f() { int x = 1; { int x = 2; x++; } } }",
        ] {
            let _ = shadow_fixes(src);
        }
    }

    #[test]
    fn describe_and_materialize_agree() {
        // The seam declares `materialize` total over everything `describe`
        // returns; an unrecoverable plan must produce no menu entry rather
        // than an empty edit.
        let workspace = Workspace::single("void f() { int x = 1; { int x = 2; } }");
        let pairs = workspace.pairs();
        let ctx = super::super::test_support::context(&workspace, &pairs, 0);
        let orphan = TypeDiagnostic {
            span: Span {
                start: 9_999,
                end: 10_000,
            },
            kind: TypeDiagnosticKind::VariableShadow {
                name: "x".to_string(),
            },
        };
        assert!(describe(&orphan, &ctx).is_empty());
    }
}
