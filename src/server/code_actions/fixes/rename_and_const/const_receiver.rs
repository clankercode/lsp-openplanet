//! Fix family: a non-const method called on a const receiver.
//!
//! Answers [`TypeDiagnosticKind::NonConstMethodOnConstReceiver`] — the game
//! aborts with `No matching signatures to 'T::M() const'`. Two edits fix
//! it, and they mean opposite things:
//!
//! * **(a) mark the METHOD `const`** at its declaration. Correct when the
//!   method genuinely does not mutate `this`. Edits the *declaring* file,
//!   which need not be the file the user right-clicked in, so the fix is
//!   produced as a `WorkspaceEdit` keyed by the declaration's own URI.
//! * **(b) drop `const` from the RECEIVER's declaration**. Correct when
//!   the caller should not have promised constness. Local to the call
//!   site's file.
//!
//! Both are offered, **neither is preferred** and neither is
//! `auto_applicable` (B038 decision): each changes an API surface, and
//! nothing in the source says which intent the author had.
//!
//! ## Scope of fix (a)
//!
//! The diagnostic itself is deliberately scoped to workspace classes,
//! because external typedb methods carry no const flag (see the variant's
//! doc comment in `typecheck::checker`). The fix inherits that scope by
//! construction: it offers (a) only when a class declaration for
//! `object_type` with a non-const method named `method` is actually
//! present in one of the workspace files the request carries. An external
//! or typedb-only method has no such declaration and gets no offer.
//!
//! Note that the *checker* is currently narrower still: it fires this
//! diagnostic only for a class indexed in the same file as the call (see
//! `cross_file_method_const_receiver_silent` in `typecheck::checker`'s
//! tests), so in practice today the declaration (a) edits lives in the
//! call's own file. The lookup here is written against the workspace
//! rather than the file so the fix stays correct when that widens; the
//! multi-file path is exercised directly in this module's tests.

use std::collections::HashMap;

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, Url, WorkspaceEdit};

use crate::analysis::DocumentAnalysis;
use crate::lexer::{Span, TokenKind};
use crate::parser::ast::{ClassDecl, ClassMember, FunctionBody, Item, SourceFile, Stmt, StmtKind};
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::server::scope_query::find_enclosing_function;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

use super::scope;

pub const FAMILY: FixFamily = FixFamily {
    name: "const-receiver-method",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(
        kind,
        TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
    )
}

/// One of the two alternatives, already located.
enum Alternative {
    /// Insert ` const` after the method's parameter list, in `uri`.
    MarkMethodConst { uri: Url, at: Span },
    /// Delete the receiver declaration's `const` qualifier, in this file.
    DropReceiverConst { at: Span, receiver: String },
}

fn matched_kind(diag: &TypeDiagnostic) -> Option<(&str, &str)> {
    match &diag.kind {
        TypeDiagnosticKind::NonConstMethodOnConstReceiver {
            method,
            object_type,
        } => Some((method.as_str(), object_type.as_str())),
        _ => None,
    }
}

fn title(diag: &TypeDiagnostic, alternative: &Alternative) -> Option<String> {
    let (method, object_type) = matched_kind(diag)?;
    Some(match alternative {
        Alternative::MarkMethodConst { .. } => {
            format!("Mark `{object_type}::{method}()` as a const method")
        }
        Alternative::DropReceiverConst { receiver, .. } => {
            format!("Remove `const` from the declaration of `{receiver}`")
        }
    })
}

/// Both alternatives that can actually be applied, in offer order.
fn alternatives(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<Alternative> {
    let Some((method, object_type)) = matched_kind(diag) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some((uri, at)) = locate_method_const_insertion(ctx, object_type, method) {
        out.push(Alternative::MarkMethodConst { uri, at });
    }
    if let Some((at, receiver)) = locate_receiver_const_deletion(ctx, diag.span) {
        out.push(Alternative::DropReceiverConst { at, receiver });
    }
    out
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    alternatives(diag, ctx)
        .iter()
        .filter_map(|alternative| {
            Some(FixDescriptor {
                title: title(diag, alternative)?,
                kind: CodeActionKind::QUICKFIX,
                // Two legitimate intents the tool cannot choose between:
                // neither may be marked preferred, and neither may be
                // applied by `source.fixAll.openplanet` on save.
                is_preferred: false,
                auto_applicable: false,
            })
        })
        .collect()
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, selected: &str) -> WorkspaceEdit {
    let chosen = alternatives(diag, ctx)
        .into_iter()
        .find(|alternative| title(diag, alternative).as_deref() == Some(selected));
    let Some(chosen) = chosen else {
        debug_assert!(false, "described a const-receiver fix that no longer plans");
        return WorkspaceEdit::default();
    };
    match chosen {
        Alternative::MarkMethodConst { uri, at } => {
            // The declaring file is not necessarily this document, so the
            // edit is addressed by the declaration's own URI and the range
            // is computed against that file's text.
            let Some(source) = source_of(ctx, &uri) else {
                debug_assert!(false, "planned an edit in a file that is no longer open");
                return WorkspaceEdit::default();
            };
            let insertion = span_to_range(
                source,
                Span {
                    start: at.end,
                    end: at.end,
                },
            );
            WorkspaceEdit::new(HashMap::from([(
                uri,
                vec![TextEdit {
                    range: insertion,
                    new_text: " const".to_string(),
                }],
            )]))
        }
        Alternative::DropReceiverConst { at, .. } => WorkspaceEdit::new(HashMap::from([(
            ctx.uri.clone(),
            vec![TextEdit {
                range: span_to_range(&ctx.analysis.source, at),
                new_text: String::new(),
            }],
        )])),
    }
}

// ── Fix (a): mark the method const ──────────────────────────────────────────

/// Every file the request can see, current document first so an unsaved
/// buffer wins over the snapshot's copy of the same URI.
fn workspace_files<'a>(ctx: &'a FixContext<'a>) -> Vec<(&'a Url, &'a DocumentAnalysis)> {
    let mut out = vec![(ctx.uri, ctx.analysis)];
    out.extend(ctx.files.iter().filter(|(uri, _)| *uri != ctx.uri));
    out
}

fn source_of<'a>(ctx: &'a FixContext<'a>, uri: &Url) -> Option<&'a str> {
    workspace_files(ctx)
        .into_iter()
        .find(|(candidate, _)| *candidate == uri)
        .map(|(_, analysis)| analysis.source.as_str())
}

/// `(declaring uri, span of the `)` closing the method's parameter list)`
/// for the unique non-const workspace declaration of
/// `object_type::method`.
///
/// The diagnostic carries no argument types or overload identity. Selecting
/// the first same-named declaration would therefore change an arbitrary
/// overload, so ambiguity means no method alternative.
fn locate_method_const_insertion(
    ctx: &FixContext<'_>,
    object_type: &str,
    method: &str,
) -> Option<(Url, Span)> {
    let mut found = None;
    for (uri, analysis) in workspace_files(ctx) {
        let source = analysis.masked_source();
        for class in matching_classes(&analysis.file, source, object_type) {
            for decl in class.members.iter().filter_map(|member| match member {
                ClassMember::Method(func) if func.name.text(source) == method => Some(func),
                _ => None,
            }) {
                if found.is_some() || decl.is_const {
                    return None;
                }
                let close = scope::params_close_paren(&analysis.tokens, decl.name.span.end)?;
                found = Some((uri.clone(), close));
            }
        }
    }
    found
}

/// Every class declared in `file` under the fully qualified `wanted` name.
/// Duplicate class declarations are invalid source, but are still treated
/// conservatively because a code action must never pick one arbitrarily.
fn matching_classes<'a>(file: &'a SourceFile, source: &str, wanted: &str) -> Vec<&'a ClassDecl> {
    fn walk<'a>(
        items: &'a [Item],
        source: &str,
        prefix: Option<String>,
        wanted: &str,
        out: &mut Vec<&'a ClassDecl>,
    ) {
        for item in items {
            match item {
                Item::Class(class) => {
                    let simple = class.name.text(source);
                    let qualified = match &prefix {
                        Some(ns) => format!("{ns}::{simple}"),
                        None => simple.to_string(),
                    };
                    if qualified == wanted {
                        out.push(class);
                    }
                }
                Item::Namespace(ns) => {
                    let simple = ns.name.text(source);
                    let nested = match &prefix {
                        Some(outer) => format!("{outer}::{simple}"),
                        None => simple.to_string(),
                    };
                    walk(&ns.items, source, Some(nested), wanted, out);
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&file.items, source, None, wanted, &mut out);
    out
}

// ── Fix (b): drop const from the receiver ───────────────────────────────────

/// `(span to delete, receiver name)` for the `const` qualifier on the
/// declaration of the receiver in `callee_span` (`receiver.method`).
///
/// Only a plain `receiver.method` callee qualifies: `a.b.M()` names no
/// single declaration to edit, and `this.M()` has no declaration at all.
fn locate_receiver_const_deletion(
    ctx: &FixContext<'_>,
    callee_span: Span,
) -> Option<(Span, String)> {
    let analysis = ctx.analysis;
    // Anonymous-function parameters are not represented by the enclosing
    // `FunctionDecl`. Until they have their own scope-aware lookup, refusing
    // this alternative is safer than deleting a same-named outer parameter.
    if is_in_anonymous_function_body(&analysis.tokens, callee_span.start) {
        return None;
    }
    let source = analysis.masked_source();
    let receiver = simple_receiver_name(&analysis.tokens, source, callee_span)?;
    let type_span = receiver_type_expr_span(
        &analysis.file,
        &analysis.tokens,
        source,
        &receiver,
        callee_span.start,
    )?;
    let deletion = sole_const_qualifier(&analysis.tokens, type_span)?;
    Some((deletion, receiver))
}

/// Whether `offset` lies within an anonymous `function (...) { ... }` body.
///
/// The parser models function declarations but not lambda parameters in the
/// enclosing-function query used below. This token-level guard deliberately
/// errs on the conservative side: an unmatched lambda simply yields no
/// receiver edit rather than risking an edit to an outer binding.
fn is_in_anonymous_function_body(tokens: &[crate::lexer::Token], offset: u32) -> bool {
    for (function_idx, function) in tokens.iter().enumerate() {
        if function.kind != TokenKind::KwFunction || function.span.start >= offset {
            continue;
        }
        let Some(open_idx) = tokens[function_idx + 1..]
            .iter()
            .position(|token| token.kind == TokenKind::LBrace)
            .map(|relative| function_idx + 1 + relative)
        else {
            continue;
        };
        let mut depth = 0usize;
        for token in &tokens[open_idx..] {
            match token.kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    let Some(next_depth) = depth.checked_sub(1) else {
                        break;
                    };
                    depth = next_depth;
                    if depth == 0 {
                        if tokens[open_idx].span.start < offset && offset < token.span.start {
                            return true;
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

/// Receiver name when the callee is exactly `Ident . Ident`.
fn simple_receiver_name(
    tokens: &[crate::lexer::Token],
    source: &str,
    callee_span: Span,
) -> Option<String> {
    let inside: Vec<_> = tokens
        .iter()
        .filter(|tok| tok.span.start >= callee_span.start && tok.span.end <= callee_span.end)
        .collect();
    match inside.as_slice() {
        [receiver, dot, _member]
            if receiver.kind == TokenKind::Ident && dot.kind == TokenKind::Dot =>
        {
            Some(receiver.span.text(source).to_string())
        }
        _ => None,
    }
}

/// Span of the declared type of the local or parameter named `receiver`
/// that is in effect at `use_offset`.
///
/// "In effect" is load-bearing, not decorative: taking the nearest
/// preceding declaration alone would pick a same-named local out of a
/// sibling block that has already closed —
/// `void f(const C &in h) { { const C h = C(); } h.M(); }` would then
/// delete the `const` from the inner declaration, which is not the
/// receiver of the diagnosed call at all. Locals therefore have to still
/// be in scope at the use site; parameters always are.
fn receiver_type_expr_span(
    file: &SourceFile,
    tokens: &[crate::lexer::Token],
    source: &str,
    receiver: &str,
    use_offset: u32,
) -> Option<Span> {
    let func = find_enclosing_function(file, use_offset)?;
    let mut best: Option<(u32, Span)> = None;
    let mut consider = |decl_offset: u32, type_span: Span| {
        if decl_offset < use_offset && best.is_none_or(|(prev, _)| decl_offset >= prev) {
            best = Some((decl_offset, type_span));
        }
    };
    for param in &func.params {
        if let Some(name) = &param.name {
            if name.text(source) == receiver {
                consider(name.span.start, param.type_expr.span);
            }
        }
    }
    if let Some(body) = &func.body {
        visit_var_decls(body, &mut |var| {
            for declarator in &var.declarators {
                if declarator.name.text(source) != receiver {
                    continue;
                }
                let in_scope = scope::innermost_scope_end(file, tokens, declarator.name.span.start)
                    .is_some_and(|end| use_offset < end);
                if in_scope {
                    consider(declarator.name.span.start, var.type_expr.span);
                }
            }
        });
    }
    best.map(|(_, type_span)| type_span)
}

/// Deletion range for the one and only `const` in `type_span`, extending
/// to the next token so the separating whitespace goes with it. Both
/// spellings work: `const T &in` and the trailing `T@ const`.
///
/// Refuses when the type has zero or several `const` keywords — with two
/// of them (`const T@ const`) it is not clear which one the diagnostic is
/// about, and a wrong deletion silently changes a signature.
fn sole_const_qualifier(tokens: &[crate::lexer::Token], type_span: Span) -> Option<Span> {
    let consts: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, tok)| {
            tok.kind == TokenKind::KwConst
                && tok.span.start >= type_span.start
                && tok.span.end <= type_span.end
        })
        .map(|(idx, _)| idx)
        .collect();
    let [idx] = consts.as_slice() else {
        return None;
    };
    let next = tokens.get(idx + 1)?;
    Some(Span {
        start: tokens[*idx].span.start,
        end: next.span.start,
    })
}

fn visit_var_decls(body: &FunctionBody, f: &mut impl FnMut(&crate::parser::ast::VarDeclStmt)) {
    for stmt in &body.stmts {
        visit_stmt(stmt, f);
    }
}

fn visit_stmt(stmt: &Stmt, f: &mut impl FnMut(&crate::parser::ast::VarDeclStmt)) {
    match &stmt.kind {
        StmtKind::VarDecl(var) => f(var),
        StmtKind::Block(stmts) => {
            for nested in stmts {
                visit_stmt(nested, f);
            }
        }
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            visit_stmt(then_branch, f);
            if let Some(branch) = else_branch {
                visit_stmt(branch, f);
            }
        }
        StmtKind::For { init, body, .. } => {
            if let Some(init) = init {
                visit_stmt(init, f);
            }
            visit_stmt(body, f);
        }
        StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => visit_stmt(body, f),
        StmtKind::Switch { cases, .. } => {
            for case in cases {
                for nested in &case.stmts {
                    visit_stmt(nested, f);
                }
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            visit_stmt(try_body, f);
            visit_stmt(catch_body, f);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        applied, context, edits_for, fixes_in, touched_uris, Workspace,
    };
    use super::*;
    use crate::server::code_actions::fix::FixCandidate;

    const METHOD_TITLE: &str = "Mark `C::M()` as a const method";
    const RECEIVER_TITLE: &str = "Remove `const` from the declaration of `h`";

    fn const_fixes(workspace: &Workspace) -> Vec<FixCandidate> {
        fixes_in(workspace, 0)
            .into_iter()
            .filter(|candidate| candidate.family == FAMILY.name)
            .collect()
    }

    fn fix_count(source: &str) -> usize {
        const_fixes(&Workspace::single(source)).len()
    }

    /// Apply the offer whose title is `title` and return the new text of
    /// document 0.
    fn apply_titled(source: &str, title: &str) -> String {
        let workspace = Workspace::single(source);
        let chosen = const_fixes(&workspace)
            .into_iter()
            .find(|candidate| candidate.fix.title == title)
            .unwrap_or_else(|| panic!("no offer titled `{title}`"));
        applied(source, &chosen.fix.edit, workspace.uri(0))
    }

    const CALL: &str = "class C { bool M() { return true; } } void f(const C &in h) { h.M(); }";

    // ── Both alternatives, neither preferred ─────────────────────────────

    #[test]
    fn both_alternatives_are_offered_with_distinct_titles() {
        let workspace = Workspace::single(CALL);
        let candidates = const_fixes(&workspace);
        assert_eq!(candidates.len(), 2, "{:?}", candidates);
        let titles: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.fix.title.as_str())
            .collect();
        assert_eq!(titles, vec![METHOD_TITLE, RECEIVER_TITLE]);
    }

    #[test]
    fn neither_alternative_is_preferred_or_auto_applicable() {
        // Two legitimate intents the tool cannot distinguish: marking one
        // preferred would make the editor apply it on a bare "quick fix",
        // and either would change an API surface on save.
        for candidate in const_fixes(&Workspace::single(CALL)) {
            assert!(!candidate.fix.is_preferred, "{}", candidate.fix.title);
            assert!(!candidate.fix.auto_applicable, "{}", candidate.fix.title);
            assert_eq!(candidate.fix.kind, CodeActionKind::QUICKFIX);
        }
    }

    // ── Fix (a): mark the method const ───────────────────────────────────

    #[test]
    fn marking_the_method_const_edits_its_declaration() {
        assert_eq!(
            apply_titled(CALL, METHOD_TITLE),
            "class C { bool M() const { return true; } } void f(const C &in h) { h.M(); }"
        );
    }

    #[test]
    fn the_const_lands_after_a_parameter_list_with_defaults_and_nested_parens() {
        let src = "class C { bool M(int a = f(1, g(2))) { return true; } } \
                   int f(int a, int b) { return a; } int g(int a) { return a; } \
                   void h(const C &in r) { r.M(); }";
        let out = apply_titled(src, METHOD_TITLE);
        assert!(out.contains("bool M(int a = f(1, g(2))) const {"), "{out}");
    }

    #[test]
    fn the_method_fix_edits_a_declaration_in_another_file_as_a_multi_file_edit() {
        // The checker only fires this diagnostic for a class indexed in
        // the call's own file today, so the cross-file path is exercised
        // through the family's own entry points with the diagnostic the
        // checker would produce if that widened. The point of the test is
        // the EDIT: it must be addressed at the declaring file's URI, not
        // at the document the action was requested on.
        let workspace = Workspace::new(&[
            ("file:///test/call.as", "void f(const C &in h) { h.M(); }"),
            (
                "file:///test/decl.as",
                "class C { bool M() { return true; } }",
            ),
        ]);
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let call = workspace.source(0);
        let start = call.find("h.M").expect("call site") as u32;
        let diag = TypeDiagnostic {
            span: Span {
                start,
                end: start + 3,
            },
            kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                method: "M".to_string(),
                object_type: "C".to_string(),
            },
        };
        let described = describe(&diag, &ctx);
        assert_eq!(described.len(), 2, "{described:?}");
        assert_eq!(described[0].title, METHOD_TITLE);

        let edit = materialize(&diag, &ctx, METHOD_TITLE);
        assert_eq!(
            touched_uris(&edit),
            vec!["file:///test/decl.as".to_string()]
        );
        assert!(
            edits_for(&edit, workspace.uri(0)).is_empty(),
            "the call's own file must not be edited"
        );
        assert_eq!(
            applied(workspace.source(1), &edit, workspace.uri(1)),
            "class C { bool M() const { return true; } }"
        );
    }

    #[test]
    fn the_method_fix_is_not_offered_for_an_external_type() {
        // Scope guard: an external/typedb method carries no const flag and
        // has no workspace declaration to edit. Only the receiver fix is
        // available.
        let workspace = Workspace::single("void f(const CMwNod &in h) { h.M(); }");
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let source = workspace.source(0);
        let start = source.find("h.M").expect("call site") as u32;
        let diag = TypeDiagnostic {
            span: Span {
                start,
                end: start + 3,
            },
            kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                method: "M".to_string(),
                object_type: "CMwNod".to_string(),
            },
        };
        let titles: Vec<_> = describe(&diag, &ctx)
            .into_iter()
            .map(|descriptor| descriptor.title)
            .collect();
        assert_eq!(
            titles,
            vec!["Remove `const` from the declaration of `h`".to_string()]
        );
    }

    #[test]
    fn an_already_const_method_gets_no_offer_at_all() {
        let workspace = Workspace::single("void f(const C &in h) { h.M(); }");
        let pairs = workspace.pairs();
        let _ = context(&workspace, &pairs, 0);
        // Guarded through the checker: a const method on a const receiver
        // is legal, so nothing is diagnosed and nothing is offered.
        assert_eq!(
            fix_count(
                "class C { bool M() const { return true; } } void f(const C &in h) { h.M(); }"
            ),
            0
        );
    }

    #[test]
    fn an_inherited_method_fix_targets_the_declaring_class() {
        // The receiver is typed as the base class, which is where the
        // method is declared — the edit must land on `B`, not on `D`.
        let src = "class B { bool M() { return true; } } class D : B {} \
                   void f(const B &in h) { h.M(); }";
        let out = apply_titled(src, "Mark `B::M()` as a const method");
        assert!(out.contains("class B { bool M() const {"), "{out}");
        assert!(out.contains("class D : B {}"), "{out}");
    }

    #[test]
    fn a_derived_receiver_is_not_diagnosed_today_so_nothing_is_offered() {
        // Documented limitation of the checker, not of the fix: an
        // inherited method reached through a DERIVED const receiver does
        // not fire the rule (the member is not a direct member of `D`).
        assert_eq!(
            fix_count(
                "class B { bool M() { return true; } } class D : B {} \
                 void f(const D &in h) { h.M(); }"
            ),
            0
        );
    }

    #[test]
    fn a_namespaced_class_declaration_is_found_by_its_qualified_name() {
        let src = "namespace N { class C { bool M() { return true; } } \
                   void f(const C &in h) { h.M(); } }";
        let out = apply_titled(src, "Mark `N::C::M()` as a const method");
        assert!(out.contains("bool M() const {"), "{out}");
    }

    // ── Fix (b): drop const from the receiver ────────────────────────────

    #[test]
    fn dropping_the_receiver_const_edits_only_the_receiver_declaration() {
        assert_eq!(
            apply_titled(CALL, RECEIVER_TITLE),
            "class C { bool M() { return true; } } void f(C &in h) { h.M(); }"
        );
    }

    #[test]
    fn dropping_the_receiver_const_works_for_a_const_handle_parameter() {
        assert_eq!(
            apply_titled(
                "class C { bool M() { return true; } } void f(const C@ &in h) { h.M(); }",
                RECEIVER_TITLE
            ),
            "class C { bool M() { return true; } } void f(C@ &in h) { h.M(); }"
        );
    }

    #[test]
    fn dropping_the_receiver_const_works_for_a_const_local() {
        assert_eq!(
            apply_titled(
                "class C { bool M() { return true; } } void f() { const C h = C(); h.M(); }",
                RECEIVER_TITLE
            ),
            "class C { bool M() { return true; } } void f() { C h = C(); h.M(); }"
        );
    }

    #[test]
    fn a_chained_receiver_offers_only_the_method_fix() {
        // `o.i` names no single declaration whose `const` could be
        // dropped, so only the method alternative is available.
        let src = "class Inner { bool M() { return true; } } class Outer { Inner i; } \
                   void f(const Outer &in o) { o.i.M(); }";
        let workspace = Workspace::single(src);
        let titles: Vec<_> = const_fixes(&workspace)
            .into_iter()
            .map(|candidate| candidate.fix.title)
            .collect();
        assert_eq!(
            titles,
            vec!["Mark `Inner::M()` as a const method".to_string()]
        );
    }

    #[test]
    fn a_closed_sibling_blocks_same_named_local_is_not_mistaken_for_the_receiver() {
        // The nearest PRECEDING declaration of `h` is the one inside the
        // block, but it is out of scope by the time the outer `h.M()`
        // runs. Deleting its `const` would edit a declaration that is not
        // the receiver at all.
        let src = "class C { bool M() { return true; } } \
                   void f(const C &in h) { { const C h = C(); h.M(); } h.M(); }";
        let workspace = Workspace::single(src);
        let candidates = const_fixes(&workspace);
        let receiver_fixes: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.fix.title == RECEIVER_TITLE)
            .collect();
        assert_eq!(receiver_fixes.len(), 2, "one per diagnosed call");
        // The inner call edits the inner declaration; the outer call edits
        // the parameter. Two different offsets, never the same one twice.
        let mut starts: Vec<_> = receiver_fixes
            .iter()
            .map(|candidate| {
                edits_for(&candidate.fix.edit, workspace.uri(0))[0]
                    .range
                    .start
            })
            .collect();
        starts.sort_by_key(|position| (position.line, position.character));
        starts.dedup();
        assert_eq!(starts.len(), 2, "both fixes edited the same declaration");
        // The outer one must be the parameter, which is the first `const`
        // in the function signature.
        let param_const = src.find("const C &in h").expect("parameter") as u32;
        assert!(
            starts
                .iter()
                .any(|position| position.character == param_const),
            "no fix edits the parameter: {starts:?}"
        );
    }

    #[test]
    fn a_closed_inner_receiver_does_not_steal_the_outer_calls_fix() {
        let src = "class C { bool M() { return true; } } \
                   void f(const C &in h) { { const C h = C(); } h.M(); }";
        assert_eq!(
            apply_titled(src, RECEIVER_TITLE),
            "class C { bool M() { return true; } } \
             void f(C &in h) { { const C h = C(); } h.M(); }"
        );
    }

    #[test]
    fn a_live_inner_receiver_is_preferred_over_the_outer_parameter() {
        let src = "class C { bool M() { return true; } } \
                   void f(const C &in h) { { const C h = C(); h.M(); } }";
        assert_eq!(
            apply_titled(src, RECEIVER_TITLE),
            "class C { bool M() { return true; } } \
             void f(const C &in h) { { C h = C(); h.M(); } }"
        );
    }

    #[test]
    fn a_lambda_parameter_never_edits_the_enclosing_functions_receiver() {
        let src = "class C { bool M() { return true; } } \
                   void f(const C &in h) { auto cb = function(const C &in h) { h.M(); }; }";
        let workspace = Workspace::single(src);
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let start = src.rfind("h.M").expect("lambda call site") as u32;
        let diag = TypeDiagnostic {
            span: Span {
                start,
                end: start + 3,
            },
            kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                method: "M".to_string(),
                object_type: "C".to_string(),
            },
        };
        let described = describe(&diag, &ctx);
        assert_eq!(
            described
                .iter()
                .map(|descriptor| descriptor.title.as_str())
                .collect::<Vec<_>>(),
            vec![METHOD_TITLE]
        );
        assert!(
            !edits_for(&materialize(&diag, &ctx, METHOD_TITLE), workspace.uri(0)).is_empty(),
            "the safe method alternative remains available"
        );
    }

    #[test]
    fn overloaded_methods_do_not_offer_an_ambiguous_method_edit() {
        for src in [
            "class C { bool M(int n) { return true; } bool M() { return true; } } void f(const C &in h) { h.M(); }",
            "class C { bool M() { return true; } bool M(int n) { return true; } } void f(const C &in h) { h.M(); }",
        ] {
            let workspace = Workspace::single(src);
            let pairs = workspace.pairs();
            let ctx = context(&workspace, &pairs, 0);
            let start = src.find("h.M").expect("call site") as u32;
            let diag = TypeDiagnostic {
                span: Span {
                    start,
                    end: start + 3,
                },
                kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                    method: "M".to_string(),
                    object_type: "C".to_string(),
                },
            };
            let described = describe(&diag, &ctx);
            assert_eq!(
                described
                    .iter()
                    .map(|descriptor| descriptor.title.as_str())
                    .collect::<Vec<_>>(),
                vec![RECEIVER_TITLE],
                "must not choose an overload in `{src}`"
            );
            assert!(described.iter().all(|descriptor| {
                !descriptor.is_preferred && !descriptor.auto_applicable
            }));
        }
    }

    #[test]
    fn a_unique_method_still_offers_both_non_auto_applicable_alternatives() {
        let workspace = Workspace::single(CALL);
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let start = CALL.find("h.M").expect("call site") as u32;
        let diag = TypeDiagnostic {
            span: Span {
                start,
                end: start + 3,
            },
            kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                method: "M".to_string(),
                object_type: "C".to_string(),
            },
        };
        let described = describe(&diag, &ctx);
        assert_eq!(described.len(), 2);
        assert!(described
            .iter()
            .all(|descriptor| { !descriptor.is_preferred && !descriptor.auto_applicable }));
        for descriptor in described {
            assert!(
                !edits_for(
                    &materialize(&diag, &ctx, &descriptor.title),
                    workspace.uri(0)
                )
                .is_empty(),
                "described title must materialize: {}",
                descriptor.title
            );
        }
    }

    #[test]
    fn a_global_receiver_offers_only_the_method_fix() {
        // The receiver is not a local or a parameter, so there is no
        // declaration in the enclosing function whose `const` we could
        // drop. Documented rather than guessed at.
        let src = "class C { bool M() { return true; } } const C g; void f() { g.M(); }";
        let titles: Vec<_> = const_fixes(&Workspace::single(src))
            .into_iter()
            .map(|candidate| candidate.fix.title)
            .collect();
        assert!(
            !titles
                .iter()
                .any(|title| title.starts_with("Remove `const`")),
            "{titles:?}"
        );
    }

    // ── Regression guards: legal receivers stay silent ───────────────────

    #[test]
    fn a_const_method_on_a_const_receiver_is_legal() {
        assert_eq!(
            fix_count(
                "class C { bool M() const { return true; } } void f(const C &in h) { h.M(); }"
            ),
            0
        );
    }

    #[test]
    fn a_non_const_method_on_a_non_const_receiver_is_legal() {
        for src in [
            "class C { bool M() { return true; } } void f(C &in h) { h.M(); }",
            "class C { bool M() { return true; } } void f(C h) { h.M(); }",
            "class C { bool M() { return true; } } void f(C@ h) { h.M(); }",
            "class C { bool M() { return true; } } void f(C@ &in h) { h.M(); }",
            "class C { bool M() { return true; } } void f() { C h; h.M(); }",
        ] {
            assert_eq!(fix_count(src), 0, "expected no fix for `{src}`");
        }
    }

    #[test]
    fn a_field_read_on_a_const_receiver_is_legal() {
        assert_eq!(
            fix_count("class C { int x; } void f(const C &in h) { int y = h.x; }"),
            0
        );
    }

    // ── `describe`/`materialize` totality ────────────────────────────────

    #[test]
    fn describe_offers_nothing_when_neither_alternative_can_be_located() {
        let workspace = Workspace::single("void f() {}");
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let orphan = TypeDiagnostic {
            span: Span {
                start: 9_000,
                end: 9_003,
            },
            kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                method: "M".to_string(),
                object_type: "Nowhere".to_string(),
            },
        };
        assert!(describe(&orphan, &ctx).is_empty());
    }

    #[test]
    fn every_described_title_materializes_a_non_empty_edit() {
        let workspace = Workspace::single(CALL);
        for candidate in const_fixes(&workspace) {
            assert!(
                !edits_for(&candidate.fix.edit, workspace.uri(0)).is_empty(),
                "empty edit for `{}`",
                candidate.fix.title
            );
        }
    }
}
