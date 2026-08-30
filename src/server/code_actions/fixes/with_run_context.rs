//! Fix family: the `awaitable.WithRunContext` migration, as a CALL
//! RESTRUCTURE (B048).
//!
//! Sibling of [`super::removed_api`], which claims the same
//! [`TypeDiagnosticKind::RemovedApiUsage`] diagnostics but only ever writes
//! a member RENAME. This migration is not a rename: the successor is a free
//! function and the receiver has to move into its argument list.
//!
//! ```text
//! startnew(Worker).WithRunContext(ctx)      // before
//! Meta::StartWithRunContext(ctx, Worker)    // after
//! ```
//!
//! # The signature, and where it comes from
//!
//! Argument ORDER is load-bearing and is NOT inferred from any example. From
//! the Openplanet type DB shipped in this repo
//! (`tests/fixtures/typedb/OpenplanetCore.json`, Openplanet 1.29.5,
//! namespace `Meta` — all 8 overloads; `OpenplanetNext.json` declares
//! neither function, so there is no game-specific override):
//!
//! ```text
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFunc@ func)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdata@ func, ref userdata)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdataInt64@ func, const int userdata)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdataInt64@ func, const int64 userdata)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdataUint64@ func, const uint userdata)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdataUint64@ func, const uint64 userdata)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdataDouble@ func, const double userdata)
//! awaitable@ StartWithRunContext(RunContext runContext, CoroutineFuncUserdataString@ func, const string&in userdata)
//! ```
//!
//! against
//!
//! ```text
//! awaitable@ startnew(CoroutineFunc@ func)
//! awaitable@ startnew(CoroutineFuncUserdata@ func, ref userdata)
//! …
//! awaitable@ startnew(CoroutineFuncUserdataBool@ func, const bool userdata)
//! ```
//!
//! So the run context goes FIRST and every `startnew` argument follows in
//! its original order. Two consequences are read straight off those lists:
//!
//! * **`startnew` has a `bool` userdata overload and `StartWithRunContext`
//!   does not** — 9 overloads against 8, and the one with no counterpart is
//!   exactly `CoroutineFuncUserdataBool@ func, const bool userdata`. A
//!   rewrite of `startnew(F, true).WithRunContext(c)` would therefore not
//!   compile, so a boolean-shaped userdata argument withholds the fix.
//!   This is a syntactic test, so a `bool` VARIABLE still slips through —
//!   that produces a loud compile error, not the silent loss this family
//!   exists to prevent, and closing it properly would mean running overload
//!   resolution inside a code-action producer.
//! * **Both return `awaitable@`,** so a further-chained call
//!   (`startnew(W).WithRunContext(c).IsRunning()`) rewrites correctly and IS
//!   offered: only the inner call's span is replaced and the chain is
//!   unaffected.
//!
//! # What is withheld, and why
//!
//! A wrong rewrite here is silent data loss — dropping the coroutine — so
//! everything this family cannot read off the AST withholds the fix rather
//! than guessing:
//!
//! | receiver / call shape | offered |
//! |---|---|
//! | `startnew(F)` / `startnew(F, u)` | yes |
//! | receiver is not a `startnew(...)` call (a stored handle, a function result) | **no** |
//! | `WithRunContext` is not called, or called with other than one argument | **no** |
//! | a NAMED argument anywhere in either call | **no** |
//! | `startnew()` with no coroutine | **no** |
//! | a boolean-shaped `startnew` userdata | **no** |
//!
//! The diagnostic is published either way: withholding the fix never
//! suppresses the warning.
//!
//! **What the rewrite does not preserve.** The call is rebuilt from its
//! argument expressions, so anything inside its span that is not one of
//! them — a comment between the arguments, the line breaks of a call spread
//! over several lines — is gone. Each ARGUMENT keeps its own text
//! verbatim, so a multi-line lambda passed to `startnew` survives intact,
//! and nothing outside the call's span is touched. That is a visible,
//! undoable edit rather than a silent one, which is the bar this family is
//! held to.
//!
//! Never `auto_applicable` and never `is_preferred`. This rewrites a call
//! rather than renaming a member, and `source.fixAll.openplanet` must not
//! restructure calls on save. There is a second, structural reason: one
//! diagnosed call can sit inside another's argument list
//! (`startnew(F(startnew(W).WithRunContext(a))).WithRunContext(b)`), and
//! the two edits then overlap by construction — the outer call's span
//! contains the inner one's. Each fix is correct on its own, which is how a
//! client applies a quick fix, but a batcher would have to drop one of
//! them. See `nested_diagnosed_calls_are_each_correct_on_their_own`.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::parser::ast::{
    BinOp, CallArg, ClassDecl, ClassMember, Expr, ExprKind, FunctionDecl, Item, NamespaceDecl,
    PropertyDecl, SourceFile, Stmt, StmtKind, UnaryOp, VarDeclStmt,
};
use crate::server::code_actions::fix::{single_file_edit, FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

/// The deprecated member this family migrates.
const MEMBER: &str = "WithRunContext";
/// The free function it migrates to, fully qualified.
const SUCCESSOR: &str = "Meta::StartWithRunContext";
/// The only receiver shape whose coroutine can be recovered.
const STARTNEW: &str = "startnew";

pub const FAMILY: FixFamily = FixFamily {
    name: "with-run-context-migration",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(
        kind,
        TypeDiagnosticKind::RemovedApiUsage { member, .. } if member == MEMBER
    )
}

const TITLE: &str = "Migrate to `Meta::StartWithRunContext`";

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    // `materialize` must be total for everything described, and it is the
    // same function that decides — so a shape this family cannot rewrite
    // costs a missing menu entry here rather than an empty edit later.
    rewrite(diag, ctx)
        .map(|_| FixDescriptor {
            title: TITLE.to_string(),
            kind: CodeActionKind::QUICKFIX,
            // Two calls collapse into one and the argument list is
            // rebuilt: correct, but not the kind of change to lead the
            // menu with or to apply on save.
            is_preferred: false,
            auto_applicable: false,
        })
        .into_iter()
        .collect()
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, _title: &str) -> WorkspaceEdit {
    let Some((span, new_text)) = rewrite(diag, ctx) else {
        debug_assert!(false, "described a WithRunContext fix that cannot be built");
        return WorkspaceEdit::default();
    };
    single_file_edit(
        ctx.uri,
        vec![TextEdit {
            range: span_to_range(&ctx.analysis.source, span),
            new_text,
        }],
    )
}

/// The span to replace and the text to put there, or `None` for every shape
/// the table in the module docs withholds.
fn rewrite(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<(Span, String)> {
    if !matches(&diag.kind) {
        return None;
    }
    let masked = ctx.analysis.masked_source();
    // The diagnostic is anchored on the member EXPRESSION
    // (`startnew(W).WithRunContext`), so the call to rewrite is the one that
    // has it as its callee. A bare member access — the other producer of
    // this diagnostic — has no such call and correctly finds nothing.
    let call = find_call_with_callee(&ctx.analysis.file, diag.span)?;
    let ExprKind::Call {
        callee,
        args: context_args,
    } = &call.kind
    else {
        return None;
    };
    let ExprKind::Member { object, member } = &callee.kind else {
        return None;
    };
    if member.text(masked) != MEMBER {
        return None;
    }
    // `awaitable@ WithRunContext(int)` takes exactly one argument. Anything
    // else is a shape this family was not written for.
    let [context] = &context_args[..] else {
        return None;
    };
    let ExprKind::Call {
        callee: receiver_callee,
        args: coroutine_args,
    } = &object.kind
    else {
        return None;
    };
    // Only a plain `startnew(...)`: a stored handle or a function returning
    // an awaitable does not expose the coroutine to move.
    let ExprKind::Ident(name) = &receiver_callee.kind else {
        return None;
    };
    if name.text(masked) != STARTNEW || coroutine_args.is_empty() {
        return None;
    }
    // A named argument binds by PARAMETER name, and the two functions'
    // parameter names do not correspond — `WithRunContext`'s parameter has
    // no name at all in the type DB. Carrying a label across would silently
    // rebind it, so any named argument withholds the fix.
    if context_args.iter().chain(coroutine_args).any(is_named) {
        return None;
    }
    // `startnew` has a bool-userdata overload; `Meta::StartWithRunContext`
    // has none. See the module docs.
    if coroutine_args.len() > 1 && coroutine_args[1..].iter().any(is_boolean_shaped) {
        return None;
    }
    // Text comes from the REAL source, not the masked copy the parse ran
    // on: the edit goes into the user's buffer.
    let source = &ctx.analysis.source;
    let mut rendered = String::from(SUCCESSOR);
    rendered.push('(');
    rendered.push_str(context.value.span.text(source));
    for arg in coroutine_args {
        rendered.push_str(", ");
        rendered.push_str(arg.value.span.text(source));
    }
    rendered.push(')');
    Some((call.span, rendered))
}

fn is_named(arg: &CallArg) -> bool {
    arg.name.is_some()
}

/// Syntactically boolean-valued: a literal, a `!`, or a comparison or
/// logical operator. Deliberately syntax-only — see the module docs for
/// what this cannot catch.
fn is_boolean_shaped(arg: &CallArg) -> bool {
    match &arg.value.kind {
        ExprKind::BoolLit(_) => true,
        ExprKind::Unary {
            op: UnaryOp::Not, ..
        } => true,
        ExprKind::Binary { op, .. } => matches!(
            op,
            BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::LtEq
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or
                | BinOp::Xor
        ),
        _ => false,
    }
}

/// The call expression whose CALLEE occupies `callee_span`, anywhere in the
/// file.
///
/// Written here rather than reusing `server::call_hierarchy`'s walk: that one
/// is private, visits every expression through a `FnMut` with no way to stop,
/// and does not reach lambda bodies, property accessors or file-level
/// initializers — all of which can hold a `startnew(...)` chain. This one
/// returns on the first hit, which matters because it runs once per
/// `describe` and once per `materialize`.
fn find_call_with_callee(file: &SourceFile, callee_span: Span) -> Option<&Expr> {
    file.items
        .iter()
        .find_map(|item| find_in_item(item, callee_span))
}

fn find_in_item(item: &Item, callee_span: Span) -> Option<&Expr> {
    match item {
        Item::Function(function) => find_in_function(function, callee_span),
        Item::Class(class) => find_in_class(class, callee_span),
        Item::Property(property) => find_in_property(property, callee_span),
        Item::Namespace(NamespaceDecl { items, .. }) => items
            .iter()
            .find_map(|item| find_in_item(item, callee_span)),
        Item::VarDecl(decl) => find_in_declarators(decl, callee_span),
        _ => None,
    }
}

fn find_in_class(class: &ClassDecl, callee_span: Span) -> Option<&Expr> {
    class.members.iter().find_map(|member| match member {
        ClassMember::Method(function)
        | ClassMember::Constructor(function)
        | ClassMember::Destructor(function) => find_in_function(function, callee_span),
        ClassMember::Property(property) => find_in_property(property, callee_span),
        ClassMember::Field(decl) => find_in_declarators(decl, callee_span),
    })
}

fn find_in_property(property: &PropertyDecl, callee_span: Span) -> Option<&Expr> {
    property
        .getter
        .as_ref()
        .and_then(|body| find_in_stmts(&body.stmts, callee_span))
        .or_else(|| {
            property
                .setter
                .as_ref()
                .and_then(|(_, body)| find_in_stmts(&body.stmts, callee_span))
        })
}

fn find_in_function(function: &FunctionDecl, callee_span: Span) -> Option<&Expr> {
    find_in_stmts(&function.body.as_ref()?.stmts, callee_span)
}

fn find_in_declarators(decl: &VarDeclStmt, callee_span: Span) -> Option<&Expr> {
    decl.declarators
        .iter()
        .filter_map(|declarator| declarator.init.as_ref())
        .find_map(|init| find_in_expr(init, callee_span))
}

fn find_in_stmts(stmts: &[Stmt], callee_span: Span) -> Option<&Expr> {
    stmts
        .iter()
        .find_map(|stmt| find_in_stmt(stmt, callee_span))
}

fn find_in_stmt(stmt: &Stmt, callee_span: Span) -> Option<&Expr> {
    match &stmt.kind {
        StmtKind::Expr(expr) | StmtKind::Return(Some(expr)) => find_in_expr(expr, callee_span),
        StmtKind::VarDecl(decl) => find_in_declarators(decl, callee_span),
        StmtKind::Block(stmts) => find_in_stmts(stmts, callee_span),
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => find_in_expr(condition, callee_span)
            .or_else(|| find_in_stmt(then_branch, callee_span))
            .or_else(|| {
                else_branch
                    .as_ref()
                    .and_then(|branch| find_in_stmt(branch, callee_span))
            }),
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => init
            .as_ref()
            .and_then(|init| find_in_stmt(init, callee_span))
            .or_else(|| {
                condition
                    .as_ref()
                    .and_then(|condition| find_in_expr(condition, callee_span))
            })
            .or_else(|| step.iter().find_map(|step| find_in_expr(step, callee_span)))
            .or_else(|| find_in_stmt(body, callee_span)),
        StmtKind::While { condition, body } | StmtKind::DoWhile { body, condition } => {
            find_in_expr(condition, callee_span).or_else(|| find_in_stmt(body, callee_span))
        }
        StmtKind::Switch { expr, cases } => find_in_expr(expr, callee_span).or_else(|| {
            cases
                .iter()
                .find_map(|case| find_in_stmts(&case.stmts, callee_span))
        }),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => find_in_stmt(try_body, callee_span).or_else(|| find_in_stmt(catch_body, callee_span)),
        _ => None,
    }
}

fn find_in_expr(expr: &Expr, callee_span: Span) -> Option<&Expr> {
    if let ExprKind::Call { callee, .. } = &expr.kind {
        if callee.span == callee_span {
            return Some(expr);
        }
    }
    match &expr.kind {
        ExprKind::Call { callee, args } => find_in_expr(callee, callee_span).or_else(|| {
            args.iter()
                .find_map(|arg| find_in_expr(&arg.value, callee_span))
        }),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assign { lhs, rhs, .. }
        | ExprKind::HandleAssign { lhs, rhs } => {
            find_in_expr(lhs, callee_span).or_else(|| find_in_expr(rhs, callee_span))
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Is { expr, .. }
        | ExprKind::Member { object: expr, .. } => find_in_expr(expr, callee_span),
        ExprKind::Index { object, index } => {
            find_in_expr(object, callee_span).or_else(|| find_in_expr(index, callee_span))
        }
        ExprKind::TypeConstruct { args, .. } | ExprKind::ArrayInit(args) => {
            args.iter().find_map(|arg| find_in_expr(arg, callee_span))
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => find_in_expr(condition, callee_span)
            .or_else(|| find_in_expr(then_expr, callee_span))
            .or_else(|| find_in_expr(else_expr, callee_span)),
        ExprKind::Lambda { body, .. } => find_in_stmts(&body.stmts, callee_span),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::DocumentAnalysis;
    use crate::server::code_actions::fix::{type_diagnostics, WorkspaceFileResolver};
    use crate::server::code_actions::fix_all::{merged_edits, ranges_overlap, DocumentFix};
    use crate::server::code_actions::test_fixtures::{apply_edits, ctx_for};
    use crate::typedb::TypeIndex;

    /// The engine typedb, loaded once: `awaitable` only resolves as an
    /// external type with it, and without that the checker never produces a
    /// `RemovedApiUsage` at all.
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

    /// The real `RemovedApiUsage` findings the checker publishes here.
    fn findings(ctx: &FixContext<'_>) -> Vec<TypeDiagnostic> {
        type_diagnostics(ctx)
            .into_iter()
            .filter(|diag| matches(&diag.kind))
            .collect()
    }

    /// Titles this family offers for `source`.
    fn titles(source: &str) -> Vec<String> {
        with_ctx(source, |ctx| {
            findings(ctx)
                .iter()
                .flat_map(|diag| describe(diag, ctx))
                .map(|descriptor| descriptor.title)
                .collect()
        })
    }

    /// Every edit this family offers for `source`, one `DocumentFix` per
    /// offered action.
    fn offered_edits(source: &str) -> Vec<DocumentFix> {
        with_ctx(source, |ctx| {
            findings(ctx)
                .iter()
                .flat_map(|diag| {
                    describe(diag, ctx)
                        .into_iter()
                        .map(|descriptor| DocumentFix {
                            title: descriptor.title.clone(),
                            edits: materialize(diag, ctx, &descriptor.title)
                                .changes
                                .expect("single-file edit")
                                .remove(ctx.uri)
                                .expect("edits for the current document"),
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        })
    }

    /// `source` after applying every edit this family offers.
    ///
    /// Routed through the fix-all batcher's own selection so a pair of
    /// edits that CONFLICT cannot be stacked on top of each other and
    /// produce nonsense that reads like a product bug. Two diagnosed calls
    /// nested one inside the other do exactly that — see
    /// `nested_diagnosed_calls_are_each_correct_on_their_own`. For every
    /// other fixture here the offers are disjoint and all of them apply.
    fn fixed(source: &str) -> String {
        apply_edits(source, &merged_edits(&offered_edits(source)))
    }

    /// The document after applying ONE offered fix on its own, which is how
    /// a client applies a quick fix.
    fn applied_alone(source: &str, index: usize) -> String {
        apply_edits(source, &offered_edits(source)[index].edits)
    }

    /// A fixture whose only interesting statement is `body`.
    fn program(body: &str) -> String {
        format!("void Worker() {{}}\nvoid Other(int) {{}}\nvoid Main() {{ {body} }}")
    }

    // ── Test premise ──────────────────────────────────────────────────────

    /// Every "no fix offered" assertion below would pass vacuously if the
    /// checker stopped flagging these calls.
    #[test]
    fn the_checker_publishes_the_finding_for_every_shape_under_test() {
        for body in [
            "startnew(Worker).WithRunContext(1);",
            "Make().WithRunContext(1);",
            "startnew(Worker, true).WithRunContext(1);",
            "startnew(Worker).WithRunContext(1).IsRunning();",
        ] {
            let source = format!(
                "void Worker() {{}}\nawaitable@ Make() {{ return startnew(Worker); }}\nvoid Main() {{ {body} }}"
            );
            with_ctx(&source, |ctx| {
                assert_eq!(
                    findings(ctx).len(),
                    1,
                    "test premise: one finding for `{body}`"
                );
            });
        }
    }

    // ── The migration ─────────────────────────────────────────────────────

    /// The headline case, end to end: the receiver moves into the call and
    /// the run context leads, per the signature in the module docs.
    #[test]
    fn the_receiver_moves_into_the_call_behind_the_run_context() {
        assert_eq!(
            titles(&program("startnew(Worker).WithRunContext(ctx);")),
            vec![TITLE]
        );
        assert_eq!(
            fixed(&program("startnew(Worker).WithRunContext(ctx);")),
            program("Meta::StartWithRunContext(ctx, Worker);")
        );
    }

    /// The coroutine argument is copied verbatim, whatever shape it has —
    /// a bare identifier, a member reference, a namespaced function, a
    /// funcdef-typed local, or a constructed `CoroutineFunc`.
    #[test]
    fn every_coroutine_shape_survives_the_move() {
        for (coroutine, prelude) in [
            ("Worker", ""),
            ("this.Worker", "class C { void Worker() {} }\n"),
            ("N::Worker", "namespace N { void Worker() {} }\n"),
            ("handle", "CoroutineFunc@ handle = null;\n"),
            ("CoroutineFunc(Worker)", ""),
        ] {
            let source = format!(
                "void Worker() {{}}\n{prelude}void Main() {{ startnew({coroutine}).WithRunContext(ctx); }}"
            );
            let want = format!(
                "void Worker() {{}}\n{prelude}void Main() {{ Meta::StartWithRunContext(ctx, {coroutine}); }}"
            );
            assert!(
                DocumentAnalysis::analyze_plain(&source)
                    .parse_errors
                    .is_empty(),
                "test premise: `{coroutine}` fixture must parse"
            );
            assert_eq!(fixed(&source), want, "for coroutine `{coroutine}`");
        }
    }

    /// `startnew`'s userdata argument carries through AFTER the coroutine,
    /// which is where `Meta::StartWithRunContext`'s own userdata overloads
    /// put it.
    #[test]
    fn extra_startnew_arguments_carry_through_in_order() {
        for userdata in ["42", "\"tag\"", "1.5", "userdata", "Other(1)", "-7"] {
            let body = format!("startnew(Worker, {userdata}).WithRunContext(ctx);");
            let want = format!("Meta::StartWithRunContext(ctx, Worker, {userdata});");
            assert_eq!(fixed(&program(&body)), program(&want), "for `{userdata}`");
        }
    }

    /// The run-context argument is copied verbatim too.
    #[test]
    fn every_run_context_shape_survives() {
        for context in [
            "ctx",
            "Meta::RunContext::AfterMainLoop",
            "GetContext()",
            "flag ? a : b",
        ] {
            let body = format!("startnew(Worker).WithRunContext({context});");
            let want = format!("Meta::StartWithRunContext({context}, Worker);");
            assert_eq!(fixed(&program(&body)), program(&want), "for `{context}`");
        }
    }

    /// The rewrite must be valid AngelScript, not merely the expected text.
    #[test]
    fn the_rewrite_reparses_cleanly() {
        for body in [
            "startnew(Worker).WithRunContext(ctx);",
            "startnew(Worker, 42).WithRunContext(ctx);",
            "auto@ a = startnew(Worker).WithRunContext(ctx);",
            "startnew(Worker).WithRunContext(ctx).IsRunning();",
        ] {
            let source = program(body);
            assert!(
                DocumentAnalysis::analyze_plain(&source)
                    .parse_errors
                    .is_empty(),
                "test premise: `{body}` must parse"
            );
            let out = fixed(&source);
            assert_ne!(out, source, "no rewrite happened for `{body}`");
            let reparsed = DocumentAnalysis::analyze_plain(&out);
            assert!(
                reparsed.parse_errors.is_empty(),
                "rewrite of `{body}` must re-parse:\n{out}\nerrors: {:?}",
                reparsed.parse_errors
            );
        }
    }

    /// Both functions return `awaitable@` (module docs), so a chained call
    /// keeps working: only the inner call's span is replaced.
    #[test]
    fn a_further_chained_call_is_rewritten_and_keeps_its_chain() {
        assert_eq!(
            fixed(&program(
                "startnew(Worker).WithRunContext(ctx).IsRunning();"
            )),
            program("Meta::StartWithRunContext(ctx, Worker).IsRunning();")
        );
    }

    // ── Withheld shapes ───────────────────────────────────────────────────

    /// The load-bearing negative: without a `startnew(...)` receiver there
    /// is no coroutine to move, and guessing would drop the user's work.
    #[test]
    fn a_receiver_that_is_not_a_startnew_call_offers_nothing() {
        for (prelude, body) in [
            // A function returning an awaitable.
            (
                "awaitable@ Make() { return startnew(Worker); }\n",
                "Make().WithRunContext(ctx);",
            ),
            // A stored handle.
            (
                "",
                "awaitable@ h = startnew(Worker); h.WithRunContext(ctx);",
            ),
            // A different call that happens to return one.
            (
                "awaitable@ Make() { return startnew(Worker); }\n",
                "Make().WithRunContext(ctx).IsRunning();",
            ),
        ] {
            let source = format!("void Worker() {{}}\n{prelude}void Main() {{ {body} }}");
            with_ctx(&source, |ctx| {
                assert_eq!(
                    findings(ctx).len(),
                    1,
                    "test premise: the warning still fires for `{body}`"
                );
            });
            assert!(
                titles(&source).is_empty(),
                "no fix may be offered for `{body}`, got {:?}",
                titles(&source)
            );
        }
    }

    /// A bare member access is the other producer of this diagnostic. There
    /// is no call to restructure, so there is no fix — and the warning still
    /// fires.
    #[test]
    fn a_bare_member_access_offers_nothing() {
        let source = program("auto@ f = startnew(Worker).WithRunContext;");
        with_ctx(&source, |ctx| {
            assert_eq!(findings(ctx).len(), 1, "test premise: the warning fires");
        });
        assert!(titles(&source).is_empty(), "got {:?}", titles(&source));
    }

    /// A named argument binds by parameter NAME, and the two functions'
    /// parameter names do not correspond — carrying a label across would
    /// silently rebind it.
    #[test]
    fn a_named_argument_anywhere_offers_nothing() {
        for body in [
            "startnew(func: Worker).WithRunContext(ctx);",
            "startnew(Worker, userdata: 42).WithRunContext(ctx);",
            "startnew(Worker).WithRunContext(runContext: ctx);",
        ] {
            let source = program(body);
            assert!(
                titles(&source).is_empty(),
                "named arguments must withhold the fix for `{body}`, got {:?}",
                titles(&source)
            );
        }
    }

    /// `startnew` has a bool-userdata overload and `Meta::StartWithRunContext`
    /// has none (module docs), so a boolean-shaped userdata would rewrite to
    /// a call that does not exist.
    #[test]
    fn a_boolean_shaped_userdata_offers_nothing() {
        for userdata in [
            "true", "false", "!flag", "a == b", "a < b", "a && b", "a || b",
        ] {
            let body = format!("startnew(Worker, {userdata}).WithRunContext(ctx);");
            let source = program(&body);
            assert!(
                titles(&source).is_empty(),
                "a bool userdata has no successor overload: `{userdata}`, got {:?}",
                titles(&source)
            );
        }
    }

    /// A shape this family was not written for — `WithRunContext` with the
    /// wrong number of arguments, or `startnew` with none — withholds
    /// rather than inventing an argument list.
    #[test]
    fn an_unexpected_argument_count_offers_nothing() {
        for body in [
            "startnew(Worker).WithRunContext();",
            "startnew(Worker).WithRunContext(a, b);",
            "startnew().WithRunContext(ctx);",
        ] {
            let source = program(body);
            assert!(
                titles(&source).is_empty(),
                "got {:?} for `{body}`",
                titles(&source)
            );
        }
    }

    // ── Layout and neighbours ─────────────────────────────────────────────

    /// The edit replaces the call's own span and nothing around it. The
    /// call itself collapses onto one line — its arguments are copied, its
    /// internal whitespace is not — but everything around it is untouched,
    /// including the indentation of the line it starts on.
    #[test]
    fn a_multi_line_call_keeps_the_layout_around_it() {
        let source = "void Worker() {}\nvoid Main() {\n    // before\n    startnew(\n        Worker\n    ).WithRunContext(\n        ctx\n    );\n    // after\n}";
        assert_eq!(
            fixed(source),
            "void Worker() {}\nvoid Main() {\n    // before\n    Meta::StartWithRunContext(ctx, Worker);\n    // after\n}"
        );
        assert!(DocumentAnalysis::analyze_plain(&fixed(source))
            .parse_errors
            .is_empty());
    }

    /// An argument that itself spans lines keeps its own text verbatim, so
    /// a lambda passed to `startnew` survives the move intact.
    #[test]
    fn an_argument_spanning_lines_is_copied_verbatim() {
        let source = "void Main() {\n    startnew(function() {\n        Print(\"hi\");\n    }).WithRunContext(ctx);\n}";
        assert_eq!(
            fixed(source),
            "void Main() {\n    Meta::StartWithRunContext(ctx, function() {\n        Print(\"hi\");\n    });\n}"
        );
        assert!(DocumentAnalysis::analyze_plain(&fixed(source))
            .parse_errors
            .is_empty());
    }

    /// LSP ranges are UTF-16 code units. An astral character earlier on the
    /// edited line shifts every column after it, and an edit measured in
    /// bytes would land four columns late.
    #[test]
    fn the_edit_range_is_measured_in_utf16_code_units() {
        // `\u{1D11E}` is 1 char, 4 bytes, 2 UTF-16 code units.
        let source = "void Worker() {}\nvoid Main() { Print(\"\u{1D11E}\u{e9}\"); startnew(Worker).WithRunContext(ctx); }";
        with_ctx(source, |ctx| {
            let diag = findings(ctx).pop().expect("one finding");
            let edits = materialize(&diag, ctx, TITLE)
                .changes
                .expect("single-file edit")
                .remove(ctx.uri)
                .expect("edits for this document");
            assert_eq!(edits.len(), 1);
            let before = source
                .split_once("startnew(Worker)")
                .expect("the call")
                .0
                .rsplit_once('\n')
                .expect("the second line")
                .1;
            assert_eq!(
                edits[0].range.start,
                tower_lsp::lsp_types::Position::new(1, before.encode_utf16().count() as u32),
                "the edit must start at the call, counted in UTF-16 units"
            );
        });
        assert_eq!(
            fixed(source),
            "void Worker() {}\nvoid Main() { Print(\"\u{1D11E}\u{e9}\"); Meta::StartWithRunContext(ctx, Worker); }"
        );
    }

    /// The call is rebuilt from its arguments, so a comment sitting between
    /// them is inside the replaced span and does not survive. Documented as
    /// a decision, not left as a surprise — everything OUTSIDE the call is
    /// untouched, including a comment on the same line.
    #[test]
    fn a_comment_inside_the_call_does_not_survive_but_one_outside_does() {
        let source = "void Worker() {}\nvoid Main() {\n    startnew(/* which */ Worker).WithRunContext(ctx); // kept\n}";
        assert_eq!(
            fixed(source),
            "void Worker() {}\nvoid Main() {\n    Meta::StartWithRunContext(ctx, Worker); // kept\n}"
        );
        assert!(DocumentAnalysis::analyze_plain(&fixed(source))
            .parse_errors
            .is_empty());
    }

    /// Two such calls on one line: each fix edits only the call it answers.
    #[test]
    fn two_calls_on_one_line_are_edited_independently() {
        let source =
            program("startnew(Worker).WithRunContext(a); startnew(Other).WithRunContext(b);");
        assert_eq!(
            titles(&source).len(),
            2,
            "test premise: two findings, two fixes"
        );
        assert_eq!(
            fixed(&source),
            program("Meta::StartWithRunContext(a, Worker); Meta::StartWithRunContext(b, Other);")
        );
    }

    /// Only the diagnosed call is edited when a sibling is withheld.
    #[test]
    fn a_withheld_neighbour_does_not_stop_the_other_fix() {
        let source = "void Worker() {}\nawaitable@ Make() { return startnew(Worker); }\nvoid Main() { Make().WithRunContext(a); startnew(Worker).WithRunContext(b); }";
        let want = "void Worker() {}\nawaitable@ Make() { return startnew(Worker); }\nvoid Main() { Make().WithRunContext(a); Meta::StartWithRunContext(b, Worker); }";
        assert_eq!(fixed(source), want);
    }

    /// Calls reachable only through a lambda body, a property accessor or a
    /// file-level initializer are found too — the walk this family owns
    /// exists for exactly those.
    #[test]
    fn calls_in_every_declaration_position_are_found() {
        for (prelude, body) in [
            (
                "",
                "auto@ f = function() { startnew(Worker).WithRunContext(ctx); };",
            ),
            ("", "{ startnew(Worker).WithRunContext(ctx); }"),
            (
                "",
                "for (uint i = 0; i < 1; i++) { startnew(Worker).WithRunContext(ctx); }",
            ),
            ("", "if (flag) { startnew(Worker).WithRunContext(ctx); }"),
            (
                "",
                "try { startnew(Worker).WithRunContext(ctx); } catch { }",
            ),
        ] {
            let source = format!("void Worker() {{}}\n{prelude}void Main() {{ {body} }}");
            assert!(
                DocumentAnalysis::analyze_plain(&source)
                    .parse_errors
                    .is_empty(),
                "test premise: `{body}` must parse"
            );
            assert!(
                fixed(&source).contains("Meta::StartWithRunContext(ctx, Worker)"),
                "no rewrite reached `{body}`:\n{}",
                fixed(&source)
            );
        }
    }

    /// The diagnosed call nested inside ANOTHER call's argument list: the
    /// edit must replace the inner call's span and leave the outer call
    /// alone.
    #[test]
    fn a_call_nested_in_another_calls_arguments_is_edited_in_place() {
        for (body, want) in [
            (
                "await(startnew(Worker).WithRunContext(ctx));",
                "await(Meta::StartWithRunContext(ctx, Worker));",
            ),
            (
                "Other(startnew(Worker).WithRunContext(ctx).IsRunning() ? 1 : 2);",
                "Other(Meta::StartWithRunContext(ctx, Worker).IsRunning() ? 1 : 2);",
            ),
            (
                "array<awaitable@> xs = { startnew(Worker).WithRunContext(ctx) };",
                "array<awaitable@> xs = { Meta::StartWithRunContext(ctx, Worker) };",
            ),
        ] {
            let source = program(body);
            assert!(
                DocumentAnalysis::analyze_plain(&source)
                    .parse_errors
                    .is_empty(),
                "test premise: `{body}` must parse"
            );
            assert_eq!(fixed(&source), program(want), "for `{body}`");
            assert!(DocumentAnalysis::analyze_plain(&fixed(&source))
                .parse_errors
                .is_empty());
        }
    }

    /// A `startnew` whose own argument holds a second diagnosed call. Both
    /// findings are answered, and each fix is correct ON ITS OWN — which is
    /// how a client applies a quick fix.
    ///
    /// Their edits necessarily OVERLAP, because the outer call's span
    /// contains the inner one's, so the two can never be applied together.
    /// That costs nothing today: a client applies one action at a time, and
    /// this family is not `auto_applicable`, so the batcher never sees the
    /// pair. It is asserted rather than assumed because making this family
    /// batchable would put that conflict in front of
    /// `select_non_overlapping`, and one of the two fixes would silently
    /// stop being applied.
    #[test]
    fn nested_diagnosed_calls_are_each_correct_on_their_own() {
        let source = program("startnew(F(startnew(Worker).WithRunContext(a))).WithRunContext(b);");
        with_ctx(&source, |ctx| {
            assert_eq!(findings(ctx).len(), 2, "test premise: two findings");
        });
        let offers = offered_edits(&source);
        assert_eq!(offers.len(), 2, "test premise: two fixes offered");
        assert!(
            ranges_overlap(&offers[0].edits[0].range, &offers[1].edits[0].range),
            "the outer call's span contains the inner one's, so these conflict"
        );

        let outcomes: Vec<String> = (0..2).map(|i| applied_alone(&source, i)).collect();
        assert!(
            outcomes.contains(&program(
                "Meta::StartWithRunContext(b, F(startnew(Worker).WithRunContext(a)));"
            )),
            "the outer fix alone must move the receiver and leave the inner call be: {outcomes:?}"
        );
        assert!(
            outcomes.contains(&program(
                "startnew(F(Meta::StartWithRunContext(a, Worker))).WithRunContext(b);"
            )),
            "the inner fix alone must leave the outer call be: {outcomes:?}"
        );
        for outcome in &outcomes {
            assert!(
                DocumentAnalysis::analyze_plain(outcome)
                    .parse_errors
                    .is_empty(),
                "{outcome}"
            );
        }
    }

    /// A file-level initializer and a property accessor, which the narrower
    /// walks elsewhere in the server do not reach.
    #[test]
    fn file_level_and_accessor_positions_are_found() {
        for source in [
            "void Worker() {}\nawaitable@ g = startnew(Worker).WithRunContext(ctx);",
            "void Worker() {}\nint Prop { get { startnew(Worker).WithRunContext(ctx); return 1; } }",
            "void Worker() {}\nclass C { void M() { startnew(Worker).WithRunContext(ctx); } }",
            "void Worker() {}\nnamespace N { void M() { startnew(Worker).WithRunContext(ctx); } }",
        ] {
            assert!(
                fixed(source).contains("Meta::StartWithRunContext(ctx, Worker)"),
                "no rewrite reached:\n{source}\ngot:\n{}",
                fixed(source)
            );
        }
    }

    // ── Through the real handler ─────────────────────────────────────────

    /// End-to-end, the way the editor drives it: ask
    /// `textDocument/codeAction` at the DIAGNOSTIC'S OWN RANGE, resolve the
    /// action, apply its edit, and assert the resulting source. Asserting
    /// that an action came back would pass just as well with an inert or
    /// wrongly-spanned edit.
    #[test]
    fn the_fix_reaches_the_client_through_the_code_action_handler() {
        let source = program("startnew(Worker).WithRunContext(ctx);");
        let (analysis, workspace, uri) = ctx_for(&source);
        let files = [(uri.clone(), &analysis)];
        let range = with_ctx(&source, |ctx| {
            span_to_range(&ctx.analysis.source, findings(ctx)[0].span)
        });
        let context = tower_lsp::lsp_types::CodeActionContext::default();
        for lazy_edits in [false, true] {
            let action = crate::server::code_actions::code_actions(
                &crate::server::code_actions::CodeActionRequest {
                    uri: &uri,
                    analysis: &analysis,
                    // The cursor sits on the squiggle, as in an editor.
                    range,
                    context: &context,
                    workspace: &workspace,
                    type_index: Some(typedb()),
                    files: WorkspaceFileResolver::new(&files),
                    lazy_edits,
                },
            )
            .into_iter()
            .filter_map(|action| match action {
                tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(action) => Some(action),
                tower_lsp::lsp_types::CodeActionOrCommand::Command(_) => None,
            })
            .find(|action| action.title == TITLE)
            .unwrap_or_else(|| panic!("no `{TITLE}` action (lazy_edits={lazy_edits})"));
            let resolved = crate::server::code_actions::resolve_code_action(
                action,
                &analysis,
                &workspace,
                Some(typedb()),
                WorkspaceFileResolver::new(&files),
            );
            let edits = resolved
                .edit
                .expect("an edit must arrive")
                .changes
                .expect("single-file edit")
                .remove(&uri)
                .expect("edits for this document");
            assert_eq!(
                apply_edits(&source, &edits),
                program("Meta::StartWithRunContext(ctx, Worker);"),
                "lazy_edits={lazy_edits}"
            );
        }
    }

    /// The sibling family claiming the same diagnostic must stay quiet
    /// here: `awaitable.WithRunContext` carries no structured
    /// `replacement`, so a member-span swap is not on offer and the client
    /// sees exactly one action.
    #[test]
    fn the_rename_family_offers_nothing_for_this_diagnostic() {
        let source = program("startnew(Worker).WithRunContext(ctx);");
        let (analysis, workspace, uri) = ctx_for(&source);
        let files = [(uri.clone(), &analysis)];
        let range = with_ctx(&source, |ctx| {
            span_to_range(&ctx.analysis.source, findings(ctx)[0].span)
        });
        let titles: Vec<String> = crate::server::code_actions::code_actions(
            &crate::server::code_actions::CodeActionRequest {
                uri: &uri,
                analysis: &analysis,
                range,
                context: &tower_lsp::lsp_types::CodeActionContext::default(),
                workspace: &workspace,
                type_index: Some(typedb()),
                files: WorkspaceFileResolver::new(&files),
                lazy_edits: true,
            },
        )
        .into_iter()
        .filter_map(|action| match action {
            tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(action) => Some(action),
            tower_lsp::lsp_types::CodeActionOrCommand::Command(_) => None,
        })
        .filter(|action| action.kind == Some(CodeActionKind::QUICKFIX))
        .map(|action| action.title)
        .collect();
        assert_eq!(titles, vec![TITLE], "got {titles:?}");
    }

    // ── Metadata ──────────────────────────────────────────────────────────

    /// A call restructure is not the kind of change to lead the menu with,
    /// and `source.fixAll.openplanet` must not restructure calls on save.
    #[test]
    fn the_fix_is_neither_preferred_nor_auto_applicable() {
        with_ctx(&program("startnew(Worker).WithRunContext(ctx);"), |ctx| {
            let descriptors: Vec<_> = findings(ctx)
                .iter()
                .flat_map(|diag| describe(diag, ctx))
                .collect();
            assert_eq!(descriptors.len(), 1);
            assert!(!descriptors[0].is_preferred);
            assert!(!descriptors[0].auto_applicable);
        });
    }
}
