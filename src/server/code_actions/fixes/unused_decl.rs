//! Quick fixes for unused-declaration findings (B036), keyed on the
//! published diagnostic.
//!
//! **Why this is a [`DiagnosticFixFamily`] and not a
//! [`crate::server::code_actions::fix::FixFamily`].** Unused findings are
//! not type diagnostics: they come from [`crate::analysis::unused`], which
//! needs an `AnalysisSnapshot`, a `WorkspaceUsage`, the plugin's export
//! paths and the `unused` level — none of which a `FixContext` carries, and
//! all of which the code-action handler would have to re-derive (differently
//! from the publisher, and so possibly answering findings the user cannot
//! see). The diagnostic the client echoes back is both cheaper and more
//! faithful: it is exactly what is squiggled.
//!
//! The diagnostic already names its own fix — its message ends "(suppress
//! with a leading `_` or a trailing `// lsp: ignore unused` comment)" — so
//! all that was missing was the edit.
//!
//! **What is offered, and what is deliberately not.**
//!
//! * Prefixing the name with `_` is always available and never loses
//!   anything. For a **parameter** it is the preferred fix, because the
//!   alternative — deleting the parameter — changes the signature and every
//!   call site. Removal is therefore never offered for a parameter.
//! * Removing the declaration is offered only for a **local**, only when it
//!   is provably lossless, and never as the preferred fix: a name may be
//!   unused today and wanted tomorrow, and the `_` prefix keeps it.
//!
//! **Neither is `auto_applicable`** — see
//! [`no_unused_fix_is_batched_into_fix_all`](tests::no_unused_fix_is_batched_into_fix_all)
//! and [`the_two_fixes_overlap_and_the_batcher_would_keep_the_removal`](tests::the_two_fixes_overlap_and_the_batcher_would_keep_the_removal)
//! for the decision and the evidence behind it.
//!
//! **The side-effecting initializer.** `int x = Foo();` cannot simply be
//! deleted — that silently drops the call. The alternative of "remove the
//! declaration but keep the initializer" only works when the initializer is
//! a valid *statement* on its own, and in AngelScript that is true of a call
//! and of very little else: `int x = a + b;` cannot become `a + b;`. So
//! removal comes in exactly three rungs, and nothing else gets a removal
//! action at all:
//!
//! | initializer | removal offered |
//! |---|---|
//! | none, or a literal | delete the statement |
//! | a call | delete the declaration, keep the call as `Foo();` |
//! | anything else | **no removal action** — `_` prefix only |
//!
//! Two more shapes never offer removal, both because a `VarDecl`'s span
//! includes its own `;`. A statement declaring more than one name
//! (`int a, b;`) would take the other declarators with it. And a
//! declaration that is not an element of a statement LIST — a `for`
//! initializer, where that `;` is the loop's separator, or an unbraced
//! branch body, where the declaration IS the body — would leave
//! `for ( c; s)` or a branch with nothing to run. See [`classify_stmts`].

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::lexer::Span;
use crate::parser::ast::{
    ClassDecl, ClassMember, Expr, ExprKind, FunctionDecl, Item, NamespaceDecl, PropertyDecl, Stmt,
    StmtKind, VarDeclStmt,
};
use crate::server::code_actions::fix::{
    extract_name_from_diagnostic, single_file_edit, DiagnosticFixFamily, FixContext, FixDescriptor,
};
use crate::server::diagnostics::{position_to_offset, span_to_range};

/// The `code` [`crate::server::diagnostics::unused_to_diagnostic`] stamps on
/// every unused finding.
const UNUSED_CODE: &str = "unused-declaration";

/// The tail of the published message, used only as a fallback when the
/// client dropped `code` on the round trip.
const UNUSED_MESSAGE_MARK: &str = "is never used (suppress with a leading `_`";

pub const FAMILY: DiagnosticFixFamily = DiagnosticFixFamily {
    name: "unused-declaration",
    matches: is_unused_declaration,
    describe,
    materialize,
};

/// One offered fix: the menu entry and the edit that applies it.
struct Offer {
    title: String,
    is_preferred: bool,
    edit: TextEdit,
}

fn describe(diagnostic: &Diagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    offers(diagnostic, ctx)
        .into_iter()
        .map(|offer| FixDescriptor {
            title: offer.title,
            kind: CodeActionKind::QUICKFIX,
            is_preferred: offer.is_preferred,
            // Never batched into `source.fixAll.openplanet`: see the module
            // docs. Removal is lossy, and the prefix renames a binding in
            // the one state where a declaration is legitimately unused —
            // code the user is still writing.
            auto_applicable: false,
        })
        .collect()
}

fn materialize(diagnostic: &Diagnostic, ctx: &FixContext<'_>, title: &str) -> WorkspaceEdit {
    offers(diagnostic, ctx)
        .into_iter()
        .find(|offer| offer.title == title)
        .map_or_else(WorkspaceEdit::default, |offer| {
            single_file_edit(ctx.uri, vec![offer.edit])
        })
}

/// Both fixes for one diagnosed declaration, or nothing where the range no
/// longer describes one.
///
/// `describe` and `materialize` share this rather than splitting it: the
/// classification walk is the only cost here, and splitting would mean
/// walking twice for each half anyway. `describe` discards the edits.
fn offers(diagnostic: &Diagnostic, ctx: &FixContext<'_>) -> Vec<Offer> {
    let analysis = ctx.analysis;
    let source = &analysis.source;
    let Some(span) = declared_name_span(analysis, diagnostic.range) else {
        return Vec::new();
    };
    let name = span.text(source);
    // The diagnostic names the declaration it is about, in backticks.
    // A range that has drifted onto a DIFFERENT identifier — the client
    // shifted it after an edit — would otherwise be answered with a fix
    // for whatever now sits there, which for a USED variable is an
    // actively wrong action. This is the one place the message is read,
    // and it is read for a name the server itself wrote, through the
    // extractor the "did you mean" family also uses.
    if extract_name_from_diagnostic(&diagnostic.message) != Some(name) {
        return Vec::new();
    }
    // The `_` prefix IS the documented suppression, so a name that
    // already carries one has nothing to gain and would become `__x`.
    if name.starts_with('_') {
        return Vec::new();
    }
    let binding = classify(&analysis.file, span);
    let mut out = vec![Offer {
        title: format!("Prefix `{name}` with `_` to mark it unused"),
        // Deleting a parameter changes the signature and every call
        // site, so for a parameter the prefix is the only sane fix and
        // leads the menu. For a local it competes with removal, and a
        // family offering two alternatives leads with neither.
        is_preferred: matches!(binding, Binding::Param),
        edit: TextEdit {
            range: span_to_range(source, span),
            new_text: format!("_{name}"),
        },
    }];
    if let Binding::Local { stmt, decl } = binding {
        if let Some((range, new_text, title)) = removal(source, name, stmt, decl) {
            out.push(Offer {
                title,
                is_preferred: false,
                edit: TextEdit { range, new_text },
            });
        }
    }
    out
}

/// Ours, and about an unused declaration. `code` is the reliable signal;
/// the message is only consulted when a client dropped `code` on the round
/// trip, because refusing then would silently disable the feature.
fn is_unused_declaration(diagnostic: &Diagnostic) -> bool {
    match &diagnostic.code {
        Some(NumberOrString::String(code)) => code == UNUSED_CODE,
        Some(NumberOrString::Number(_)) => false,
        None => diagnostic.message.contains(UNUSED_MESSAGE_MARK),
    }
}

/// The declaration's name span, or `None` if the range does not cover
/// exactly one identifier token. Reading the name from the tokens rather
/// than from the message keeps prose out of the edit.
///
/// This declines in two situations, both correctly. A **stale** range — the
/// document moved on since the diagnostic was published — must not rename
/// whatever text now sits there. And a **file-level** finding (or a class
/// field) is anchored on the whole declaration rather than on its name
/// (`void Unused() {}` is reported over all 16 bytes), so it has no name
/// token at the range and gets no action; locals and parameters are
/// anchored on the name, which is the population this producer is for.
fn declared_name_span(analysis: &DocumentAnalysis, range: Range) -> Option<Span> {
    let source = &analysis.source;
    let start = u32::try_from(position_to_offset(source, range.start)).ok()?;
    let end = u32::try_from(position_to_offset(source, range.end)).ok()?;
    analysis
        .tokens
        .iter()
        .find(|token| {
            token.kind == crate::lexer::token::TokenKind::Ident
                && token.span.start == start
                && token.span.end == end
        })
        .map(|token| token.span)
}

/// What kind of thing the diagnosed name declares. `Other` covers file-level
/// declarations, class fields and anything this walk does not reach: the
/// prefix still applies, removal does not.
enum Binding<'a> {
    Param,
    Local {
        stmt: &'a Stmt,
        decl: &'a VarDeclStmt,
    },
    Other,
}

fn classify(file: &crate::parser::ast::SourceFile, span: Span) -> Binding<'_> {
    for item in &file.items {
        if let Some(found) = classify_item(item, span) {
            return found;
        }
    }
    Binding::Other
}

fn classify_item(item: &Item, span: Span) -> Option<Binding<'_>> {
    match item {
        Item::Function(function) => classify_function(function, span),
        Item::Class(class) => classify_class(class, span),
        Item::Property(property) => classify_property(property, span),
        Item::Namespace(NamespaceDecl { items, .. }) => {
            items.iter().find_map(|item| classify_item(item, span))
        }
        _ => None,
    }
}

/// A virtual property's accessors are ordinary function bodies as far as
/// bindings go — `Checker::check_property_accessor_body` pushes the same
/// local-declaration context an ordinary body gets, so locals declared in a
/// `get`/`set` block ARE reported unused and must be removable like any
/// other local. (The setter's `value` is synthetic and never recorded, so
/// it cannot reach here.)
fn classify_property(property: &PropertyDecl, span: Span) -> Option<Binding<'_>> {
    property
        .getter
        .as_ref()
        .and_then(|body| classify_stmts(&body.stmts, span))
        .or_else(|| {
            property
                .setter
                .as_ref()
                .and_then(|(_, body)| classify_stmts(&body.stmts, span))
        })
}

fn classify_class(class: &ClassDecl, span: Span) -> Option<Binding<'_>> {
    class.members.iter().find_map(|member| match member {
        ClassMember::Method(function)
        | ClassMember::Constructor(function)
        | ClassMember::Destructor(function) => classify_function(function, span),
        ClassMember::Property(property) => classify_property(property, span),
        ClassMember::Field(_) => None,
    })
}

fn classify_function(function: &FunctionDecl, span: Span) -> Option<Binding<'_>> {
    if function
        .params
        .iter()
        .any(|param| param.name.as_ref().is_some_and(|name| name.span == span))
    {
        return Some(Binding::Param);
    }
    let body = function.body.as_ref()?;
    classify_stmts(&body.stmts, span)
}

/// A declaration is removable only when it is an element of a statement
/// LIST — a function body, a block, a switch case. That is the whole reason
/// this walk distinguishes list context from single-statement context, and
/// it is load-bearing twice over, because a `VarDecl`'s span includes its
/// own `;`:
///
/// * `for (int i = 0; c; s)` — the init's `;` is the loop's first
///   separator, so deleting the statement leaves `for ( c; s)`.
/// * `if (c) int x = 1;` — the declaration IS the branch body, so deleting
///   it leaves `if (c)` with nothing to run.
///
/// Both still get the `_` prefix; neither gets a removal action.
fn classify_stmts(stmts: &[Stmt], span: Span) -> Option<Binding<'_>> {
    for stmt in stmts {
        if let StmtKind::VarDecl(decl) = &stmt.kind {
            if decl
                .declarators
                .iter()
                .any(|declarator| declarator.name.span == span)
            {
                return Some(Binding::Local { stmt, decl });
            }
        }
        if let Some(found) = classify_stmt(stmt, span) {
            return Some(found);
        }
    }
    None
}

/// As [`classify_stmts`], for one statement in a position where it is NOT a
/// list element. A declaration matched here is deliberately reported as
/// [`Binding::Other`]: the prefix applies, removal does not.
fn classify_stmt(stmt: &Stmt, span: Span) -> Option<Binding<'_>> {
    match &stmt.kind {
        StmtKind::VarDecl(decl) => {
            if decl
                .declarators
                .iter()
                .any(|declarator| declarator.name.span == span)
            {
                return Some(Binding::Other);
            }
            decl.declarators
                .iter()
                .filter_map(|declarator| declarator.init.as_ref())
                .find_map(|init| classify_expr(init, span))
        }
        StmtKind::Expr(expr) | StmtKind::Return(Some(expr)) => classify_expr(expr, span),
        StmtKind::Block(stmts) => classify_stmts(stmts, span),
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => classify_expr(condition, span)
            .or_else(|| classify_stmt(then_branch, span))
            .or_else(|| else_branch.as_ref().and_then(|s| classify_stmt(s, span))),
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => init
            .as_ref()
            .and_then(|s| classify_stmt(s, span))
            .or_else(|| condition.as_ref().and_then(|e| classify_expr(e, span)))
            .or_else(|| step.iter().find_map(|e| classify_expr(e, span)))
            .or_else(|| classify_stmt(body, span)),
        StmtKind::While { condition, body } | StmtKind::DoWhile { body, condition } => {
            classify_expr(condition, span).or_else(|| classify_stmt(body, span))
        }
        StmtKind::Switch { expr, cases } => classify_expr(expr, span).or_else(|| {
            cases
                .iter()
                .find_map(|case| classify_stmts(&case.stmts, span))
        }),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => classify_stmt(try_body, span).or_else(|| classify_stmt(catch_body, span)),
        _ => None,
    }
}

/// Expressions are walked only to reach lambda bodies, which hold locals
/// and parameters of their own.
fn classify_expr(expr: &Expr, span: Span) -> Option<Binding<'_>> {
    match &expr.kind {
        ExprKind::Lambda { params, body } => {
            if params
                .iter()
                .any(|param| param.name.as_ref().is_some_and(|name| name.span == span))
            {
                return Some(Binding::Param);
            }
            classify_stmts(&body.stmts, span)
        }
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assign { lhs, rhs, .. }
        | ExprKind::HandleAssign { lhs, rhs } => {
            classify_expr(lhs, span).or_else(|| classify_expr(rhs, span))
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Is { expr, .. }
        | ExprKind::Member { object: expr, .. } => classify_expr(expr, span),
        ExprKind::Call { callee, args } => classify_expr(callee, span)
            .or_else(|| args.iter().find_map(|arg| classify_expr(&arg.value, span))),
        ExprKind::Index { object, index } => {
            classify_expr(object, span).or_else(|| classify_expr(index, span))
        }
        ExprKind::TypeConstruct { args, .. } | ExprKind::ArrayInit(args) => {
            args.iter().find_map(|arg| classify_expr(arg, span))
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => classify_expr(condition, span)
            .or_else(|| classify_expr(then_expr, span))
            .or_else(|| classify_expr(else_expr, span)),
        _ => None,
    }
}

/// The removal action for one local, or `None` where removal would lose
/// something. See the module docs for the three rungs.
fn removal(
    source: &str,
    name: &str,
    stmt: &Stmt,
    decl: &VarDeclStmt,
) -> Option<(Range, String, String)> {
    // `int a, b;` — deleting the statement would take `b` with it.
    let [declarator] = &decl.declarators[..] else {
        return None;
    };
    match declarator.init.as_ref() {
        None => Some((
            span_to_range(source, delete_span(source, stmt.span)),
            String::new(),
            format!("Remove unused local `{name}`"),
        )),
        Some(init) if is_literal(init) => Some((
            span_to_range(source, delete_span(source, stmt.span)),
            String::new(),
            format!("Remove unused local `{name}`"),
        )),
        // A call is the one initializer shape that is also a statement, so
        // it can be kept instead of silently dropped.
        Some(init) if matches!(init.kind, ExprKind::Call { .. }) => Some((
            span_to_range(source, stmt.span),
            format!("{};", init.span.text(source)),
            format!("Remove unused local `{name}`, keeping the call"),
        )),
        // Constructor-style initialization (`IO::File f(path, mode);`) is
        // the near miss, and it stays out on purpose. The parser gives its
        // `TypeConstruct` a span covering the ARGUMENT LIST only — `(1)`
        // for `C f(1)` — so keeping the call would mean re-synthesizing the
        // type text from `decl.type_expr`, and that text may carry a
        // leading `const` or a reference decoration that makes `T(args)`
        // invalid. And even where the text CAN be reconstructed, the class
        // case changes semantics: `C f(1);` destructs at scope exit, while
        // `C(1);` destructs at the end of the statement. That is a
        // behaviour change hiding inside a "removal", which is exactly what
        // this rung exists to avoid. No removal; the `_` prefix still
        // applies.
        //
        // Anything else: deleting loses the work, and keeping it is not
        // expressible — `a + b;` is not a statement. Offer no removal.
        Some(_) => None,
    }
}

/// Losing this initializer loses nothing. Deliberately narrow: `a++` and a
/// call hide behind expression shapes, so only self-evident constants
/// qualify, and `-1` is treated as a constant because unary minus on a
/// literal cannot do anything else.
fn is_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::StringLit
        | ExprKind::HexLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Null => true,
        ExprKind::Unary { op, expr } => {
            matches!(
                op,
                crate::parser::ast::UnaryOp::Neg
                    | crate::parser::ast::UnaryOp::Not
                    | crate::parser::ast::UnaryOp::BitNot
            ) && is_literal(expr)
        }
        _ => false,
    }
}

/// Widen a statement's span to what should actually be deleted: the whole
/// line when the statement has it to itself (otherwise the edit leaves a
/// blank line behind), and any trailing spaces when it shares one.
fn delete_span(source: &str, stmt: Span) -> Span {
    let start = stmt.start as usize;
    let end = stmt.end as usize;
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let alone_before = source[line_start..start]
        .chars()
        .all(|c| c == ' ' || c == '\t');
    let rest = &source[end..];
    let line_end = rest.find('\n').map_or(source.len(), |i| end + i + 1);
    let alone_after = source[end..line_end].chars().all(|c| c.is_whitespace());
    if alone_before && alone_after {
        return Span {
            start: line_start as u32,
            end: line_end as u32,
        };
    }
    let trailing = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    Span {
        start: stmt.start,
        end: (end + trailing) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::code_actions::fix::{
        Registries, WorkspaceFileResolver, SOURCE_FIX_ALL_OPENPLANET,
    };
    use crate::server::code_actions::fix_all::{
        merged_edits, ranges_overlap, select_non_overlapping, DocumentFix,
    };
    use crate::server::code_actions::test_fixtures::{apply_edits, published_unused_diagnostics};
    use crate::server::code_actions::{code_actions_with, resolve_code_action, CodeActionRequest};
    use crate::symbols::SymbolTable;

    /// This family alone, so a sibling family claiming the same fixture can
    /// never perturb these assertions.
    const ONLY_THIS: Registries<'static> = Registries {
        checker: &[],
        published: &[FAMILY],
    };

    fn uri() -> Url {
        Url::parse("file:///test/a.as").expect("valid test uri")
    }

    /// The unused diagnostics the server really publishes for `source` — so
    /// these tests assert against the real range, message and `code`, not a
    /// synthetic diagnostic that could drift away from what is published.
    fn published(source: &str) -> (DocumentAnalysis, Vec<Diagnostic>) {
        (
            DocumentAnalysis::analyze_plain(source),
            published_unused_diagnostics(source),
        )
    }

    fn whole_doc(source: &str) -> Range {
        Range::new(
            Position::new(0, 0),
            Position::new(source.lines().count().max(1) as u32, 0),
        )
    }

    fn request<'a>(
        uri: &'a Url,
        analysis: &'a DocumentAnalysis,
        context: &'a CodeActionContext,
        files: &'a [(Url, &'a DocumentAnalysis)],
        workspace: &'a SymbolTable,
        lazy_edits: bool,
    ) -> CodeActionRequest<'a> {
        CodeActionRequest {
            uri,
            analysis,
            range: whole_doc(&analysis.source),
            context,
            workspace,
            type_index: None,
            files: WorkspaceFileResolver::new(files),
            lazy_edits,
        }
    }

    /// Every quick fix this family offers for `diagnostics`, driven through
    /// the REAL request handler scoped to this family — so the seam's own
    /// filters (`is_our_diagnostic`, the range test) are exercised here
    /// rather than bypassed.
    fn fixes_for(
        uri: &Url,
        analysis: &DocumentAnalysis,
        diagnostics: &[Diagnostic],
    ) -> Vec<CodeAction> {
        let workspace = SymbolTable::new();
        let context = CodeActionContext {
            diagnostics: diagnostics.to_vec(),
            ..Default::default()
        };
        let files = [(uri.clone(), analysis)];
        code_actions_with(
            &request(uri, analysis, &context, &files, &workspace, false),
            ONLY_THIS,
        )
        .into_iter()
        .filter_map(|action| match action {
            CodeActionOrCommand::CodeAction(action) => Some(action),
            CodeActionOrCommand::Command(_) => None,
        })
        .filter(|action| action.kind == Some(CodeActionKind::QUICKFIX))
        .collect()
    }

    fn actions(source: &str) -> Vec<CodeAction> {
        let (analysis, diagnostics) = published(source);
        fixes_for(&uri(), &analysis, &diagnostics)
    }

    fn titles(source: &str) -> Vec<String> {
        actions(source).into_iter().map(|a| a.title).collect()
    }

    /// The text after applying the action whose title contains `needle`.
    fn applied(source: &str, needle: &str) -> String {
        let action = actions(source)
            .into_iter()
            .find(|a| a.title.contains(needle))
            .unwrap_or_else(|| panic!("no action whose title contains `{needle}` for `{source}`"));
        let edits = action
            .edit
            .expect("the action must carry its edit")
            .changes
            .expect("single-file edit")
            .remove(&uri())
            .expect("edits for the current document");
        apply_edits(source, &edits)
    }

    // ── Test premise ──────────────────────────────────────────────────────

    /// If the unused pass stopped firing here, every "no action offered"
    /// assertion below would pass for the wrong reason.
    #[test]
    fn the_server_publishes_the_unused_findings_these_tests_answer() {
        for (source, want) in [
            ("void Main() { int x = 1; }", "x"),
            (
                "void f(int p) { Print(\"hi\"); }\nvoid Main() { f(1); }",
                "p",
            ),
        ] {
            let (_, diagnostics) = published(source);
            assert!(
                diagnostics.iter().any(|d| d.message.contains(want)),
                "test premise: expected an unused finding for `{want}` in `{source}`, got {diagnostics:?}"
            );
        }
    }

    // ── Parameters: `_` prefix preferred, removal never offered ───────────

    #[test]
    fn an_unused_parameter_is_offered_only_the_underscore_prefix() {
        let source = "void f(int p) { Print(\"hi\"); }\nvoid Main() { f(1); }";
        assert_eq!(
            titles(source),
            vec!["Prefix `p` with `_` to mark it unused"]
        );
        assert_eq!(
            applied(source, "Prefix"),
            "void f(int _p) { Print(\"hi\"); }\nvoid Main() { f(1); }"
        );
    }

    /// Deleting a parameter changes the signature and every call site, so
    /// the `_` prefix is unambiguously the right fix and leads the menu.
    #[test]
    fn the_parameter_prefix_is_preferred() {
        let source = "void f(int p) { Print(\"hi\"); }\nvoid Main() { f(1); }";
        let actions = actions(source);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].is_preferred, Some(true));
        assert_eq!(actions[0].kind, Some(CodeActionKind::QUICKFIX));
    }

    #[test]
    fn unused_parameters_of_methods_and_namespaced_functions_are_covered() {
        for (source, fixed) in [
            (
                "class C { void M(int p) { Print(\"hi\"); } }\nvoid Main() { C c; c.M(1); }",
                "class C { void M(int _p) { Print(\"hi\"); } }\nvoid Main() { C c; c.M(1); }",
            ),
            (
                "namespace N { void f(int p) { Print(\"hi\"); } }\nvoid Main() { N::f(1); }",
                "namespace N { void f(int _p) { Print(\"hi\"); } }\nvoid Main() { N::f(1); }",
            ),
        ] {
            assert_eq!(applied(source, "Prefix"), fixed, "for `{source}`");
        }
    }

    // ── Locals: both offered, neither preferred ───────────────────────────

    #[test]
    fn an_unused_local_offers_both_fixes_and_prefers_neither() {
        let source = "void Main() { int x = 1; }";
        assert_eq!(
            titles(source),
            vec![
                "Prefix `x` with `_` to mark it unused",
                "Remove unused local `x`",
            ]
        );
        for action in actions(source) {
            assert_eq!(
                action.is_preferred, None,
                "removal is lossy and the prefix keeps the name — neither leads: {}",
                action.title
            );
        }
    }

    #[test]
    fn removing_a_local_removes_its_whole_line() {
        assert_eq!(
            applied(
                "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}",
                "Remove"
            ),
            "void Main() {\n    Print(\"hi\");\n}"
        );
    }

    #[test]
    fn removing_a_local_sharing_a_line_removes_only_the_statement() {
        assert_eq!(
            applied("void Main() { int x = 1; Print(\"hi\"); }", "Remove"),
            "void Main() { Print(\"hi\"); }"
        );
    }

    #[test]
    fn prefixing_a_local_touches_only_the_name() {
        assert_eq!(
            applied("void Main() { int x = 1; }", "Prefix"),
            "void Main() { int _x = 1; }"
        );
    }

    #[test]
    fn a_local_with_no_initializer_can_be_removed() {
        let source = "void Main() {\n    int x;\n    Print(\"hi\");\n}";
        assert!(titles(source).iter().any(|t| t.starts_with("Remove")));
        assert_eq!(
            applied(source, "Remove"),
            "void Main() {\n    Print(\"hi\");\n}"
        );
    }

    #[test]
    fn literal_initializers_of_every_shape_can_be_removed() {
        for init in ["1", "1.5", "true", "false", "\"s\"", "0x10", "null"] {
            let source = format!("void Main() {{\n    auto x = {init};\n    Print(\"hi\");\n}}");
            assert!(
                titles(&source).iter().any(|t| t.starts_with("Remove")),
                "a literal initializer loses nothing when deleted: `{init}`"
            );
        }
    }

    // ── The side-effecting initializer ────────────────────────────────────

    /// A call initializer must not simply be deleted — that drops the call.
    /// It is the one initializer shape that is also a valid statement, so
    /// removal is offered with the call retained, and is never preferred.
    #[test]
    fn a_call_initializer_is_retained_rather_than_dropped() {
        let source =
            "int Foo() { return 1; }\nvoid Main() {\n    int x = Foo();\n    Print(\"hi\");\n}";
        assert_eq!(
            titles(source),
            vec![
                "Prefix `x` with `_` to mark it unused",
                "Remove unused local `x`, keeping the call",
            ]
        );
        assert_eq!(
            applied(source, "Remove"),
            "int Foo() { return 1; }\nvoid Main() {\n    Foo();\n    Print(\"hi\");\n}"
        );
        assert!(actions(source).iter().all(|a| a.is_preferred.is_none()));
    }

    /// The keep-call rung is the one that WRITES code rather than deleting
    /// it, so matching the expected text is not enough — the result has to
    /// be valid AngelScript. Re-parse it and require a clean parse plus the
    /// call surviving as a real expression statement in the new AST.
    #[test]
    fn the_keep_call_result_re_parses_as_a_call_statement() {
        for (decl, kept) in [
            ("int x = Foo();", "Foo()"),
            ("int x = Foo(1, 2);", "Foo(1, 2)"),
            ("int x = Ns::Foo();", "Ns::Foo()"),
            ("int x = obj.Method();", "obj.Method()"),
            ("int x = Foo().Chained();", "Foo().Chained()"),
        ] {
            let source = format!(
                "namespace Ns {{ int Foo() {{ return 1; }} }}\n\
                 class C {{ int Method() {{ return 1; }} int Chained() {{ return 1; }} }}\n\
                 C@ obj = C();\n\
                 C@ Foo() {{ return C(); }}\n\
                 int Foo(int a, int b) {{ return a; }}\n\
                 void Main() {{\n    {decl}\n    Print(\"hi\");\n}}"
            );
            // Premise: the fixture itself must parse, or a clean re-parse
            // afterwards would prove nothing.
            assert!(
                DocumentAnalysis::analyze_plain(&source)
                    .parse_errors
                    .is_empty(),
                "test premise: `{decl}` fixture must parse"
            );
            let titles = titles(&source);
            if !titles.iter().any(|t| t.contains("keeping the call")) {
                panic!("expected a keep-call removal for `{decl}`, got {titles:?}");
            }
            let fixed = applied(&source, "keeping the call");
            let reparsed = DocumentAnalysis::analyze_plain(&fixed);
            assert!(
                reparsed.parse_errors.is_empty(),
                "keep-call output must be valid AngelScript for `{decl}`:\n{fixed}\nerrors: {:?}",
                reparsed.parse_errors
            );
            // Stronger than a text match: the call must come back as an
            // expression STATEMENT, which is the whole claim of this rung.
            let mains: Vec<_> = reparsed
                .file
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Function(function) if function.name.text(&fixed) == "Main" => {
                        function.body.as_ref()
                    }
                    _ => None,
                })
                .collect();
            let body = mains.first().expect("Main survives the edit");
            assert!(
                body.stmts.iter().any(|stmt| matches!(
                    &stmt.kind,
                    StmtKind::Expr(expr) if expr.span.text(&fixed) == kept
                )),
                "`{kept}` must survive as an expression statement for `{decl}`:\n{fixed}"
            );
        }
    }

    /// Every other initializer shape: no removal action at all. Deleting
    /// would lose the work, and retaining it is not expressible — `a + b;`
    /// is not a statement.
    #[test]
    fn a_non_call_non_literal_initializer_offers_no_removal() {
        for init in [
            "a + b",
            "a",
            "-a",
            "a++",
            "a > b",
            "a ? b : c",
            "Foo() + 1",
            "{1, 2}",
        ] {
            let source = format!(
                "int a = 1; int b = 2; int c = 3;\nint Foo() {{ return 1; }}\nvoid Main() {{ int x = {init}; }}"
            );
            let titles = titles(&source);
            assert!(
                titles.iter().all(|t| !t.starts_with("Remove")),
                "removal must be withheld for initializer `{init}`, got {titles:?}"
            );
            assert!(
                titles.iter().any(|t| t.starts_with("Prefix")),
                "the `_` prefix stays available for `{init}`"
            );
        }
    }

    /// Deleting the statement would take the other declarators with it.
    #[test]
    fn a_multi_declarator_statement_offers_no_removal() {
        let source = "void Main() { int x = 1, y = 2; Print(y); }";
        let titles = titles(source);
        assert!(
            titles.iter().all(|t| !t.starts_with("Remove")),
            "got {titles:?}"
        );
        assert_eq!(
            applied(source, "Prefix"),
            "void Main() { int _x = 1, y = 2; Print(y); }"
        );
    }

    // ── Where the declaration sits ────────────────────────────────────────

    /// A `VarDecl`'s span includes its own `;`. In a `for` init that `;` is
    /// the loop's first separator, so deleting the statement would leave
    /// `for ( i < 3; i++)`. In an unbraced branch the declaration IS the
    /// body, so deleting it leaves the branch with nothing to run. Both
    /// keep the prefix and must lose the removal.
    #[test]
    fn a_declaration_that_is_not_a_list_element_offers_no_removal() {
        for source in [
            // for-init: the `;` is structural.
            "void Main() { for (int unusedI = 0; Now < 3; ) { Print(\"hi\"); } }",
            // Unbraced branch bodies: the declaration is the whole body.
            "void Main() { if (Now > 1) int unusedI = 0; }",
            "void Main() { if (Now > 1) Print(\"hi\"); else int unusedI = 0; }",
            "void Main() { while (false) int unusedI = 0; }",
        ] {
            let titles = titles(source);
            assert!(
                titles.iter().any(|t| t.starts_with("Prefix")),
                "the prefix stays available for `{source}`, got {titles:?}"
            );
            assert!(
                titles.iter().all(|t| !t.starts_with("Remove")),
                "removal would corrupt `{source}`, got {titles:?}"
            );
        }
    }

    /// Statement LISTS at any depth do allow removal — a block, a loop
    /// body, a switch case, a try or catch body, a lambda body.
    #[test]
    fn a_list_element_at_any_depth_can_be_removed() {
        for body in [
            "{ int x = 1; }",
            "while (false) { int x = 1; }",
            "for (uint i = 0; i < 1; i++) { int x = 1; }",
            "if (Now > 1) { int x = 1; }",
            "switch (1) { case 1: int x = 1; break; }",
            "try { int x = 1; } catch { }",
            "auto@ f = function() { int x = 1; };",
        ] {
            let source = format!("void Main() {{ {body} Print(\"hi\"); }}");
            let titles = titles(&source);
            assert!(
                titles.iter().any(|t| t == "Remove unused local `x`"),
                "a list element is removable in `{body}`, got {titles:?}"
            );
        }
    }

    // ── Edit ranges, awkward files ────────────────────────────────────────

    #[test]
    fn removal_handles_crlf_tabs_and_a_trailing_comment() {
        for (source, fixed) in [
            (
                "void Main() {\r\n    int x = 1;\r\n    Print(\"hi\");\r\n}",
                "void Main() {\r\n    Print(\"hi\");\r\n}",
            ),
            (
                "void Main() {\n\tint x = 1;\n\tPrint(\"hi\");\n}",
                "void Main() {\n\tPrint(\"hi\");\n}",
            ),
            (
                // A comment sharing the line survives — only the statement
                // and the space after it go.
                "void Main() {\n    int x = 1; // why\n    Print(\"hi\");\n}",
                "void Main() {\n    // why\n    Print(\"hi\");\n}",
            ),
            (
                // Last statement of the body, and the file has no trailing
                // newline.
                "void Main() {\n    Print(\"hi\");\n    int x = 1;\n}",
                "void Main() {\n    Print(\"hi\");\n}",
            ),
            (
                // Sole statement of a one-line block: the line is not the
                // statement's to take, so only the statement goes.
                "void Main() { { int x = 1; } Print(\"hi\"); }",
                "void Main() { { } Print(\"hi\"); }",
            ),
            (
                // An inline block comment after the statement survives.
                "void Main() {\n    int x = 1; /* why */ Print(\"hi\");\n}",
                "void Main() {\n    /* why */ Print(\"hi\");\n}",
            ),
        ] {
            assert_eq!(applied(source, "Remove"), fixed, "for {source:?}");
        }
    }

    /// Byte offsets from the lexer are UTF-8 boundaries, but a producer that
    /// slices the source by hand can still land mid-character. A panic here
    /// would take the language server down for every later request.
    #[test]
    fn non_ascii_source_does_not_break_the_edits() {
        let source =
            "// héllo wörld ✨ 日本語\nvoid Main() {\n    int x = 1;\n    Print(\"hi\");\n}";
        assert_eq!(
            applied(source, "Remove"),
            "// héllo wörld ✨ 日本語\nvoid Main() {\n    Print(\"hi\");\n}"
        );
        assert_eq!(
            applied(source, "Prefix"),
            "// héllo wörld ✨ 日本語\nvoid Main() {\n    int _x = 1;\n    Print(\"hi\");\n}"
        );
    }

    /// The harder case: non-ASCII on the SAME line as the edit, including an
    /// astral character (two UTF-16 code units), so the emitted range has to
    /// be right in LSP's UTF-16 columns and not merely in bytes.
    #[test]
    fn non_ascii_on_the_edited_line_produces_correct_utf16_columns() {
        let source =
            "void Main() {\n    /* \u{2728}\u{1D11E}\u{e9} */ int x = 1;\n    Print(\"hi\");\n}";
        let prefix = actions(source)
            .into_iter()
            .find(|action| action.title.starts_with("Prefix"))
            .expect("a prefix action");
        let edits = prefix
            .edit
            .expect("eager edit")
            .changes
            .expect("single-file edit")
            .remove(&uri())
            .expect("edits for this document");
        assert_eq!(edits.len(), 1);
        // Before the name: 4 spaces + "/* " + 1 + 2 + 1 + " */ " + "int "
        // = 4 + 3 + 4 + 4 + 4 = 19 UTF-16 code units.
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(1, 19), Position::new(1, 20)),
            "the edit range must be in UTF-16 code units, not bytes"
        );
        assert_eq!(
            applied(source, "Prefix"),
            "void Main() {\n    /* \u{2728}\u{1D11E}\u{e9} */ int _x = 1;\n    Print(\"hi\");\n}"
        );
        // The comment shares the line, so it survives and only the
        // statement (plus the space after it) is removed.
        assert_eq!(
            applied(source, "Remove"),
            "void Main() {\n    /* ✨𝄞é */ \n    Print(\"hi\");\n}"
        );
    }

    // ── Guards ────────────────────────────────────────────────────────────

    /// The `_` prefix IS the documented suppression, so a name that already
    /// has it must never be offered `__x`.
    #[test]
    fn an_underscore_prefixed_name_is_never_offered_another_underscore() {
        for source in [
            "void Main() { int _x = 1; }",
            "void f(int _p) { Print(\"hi\"); }\nvoid Main() { f(1); }",
            "void Main() { int __x = 1; }",
        ] {
            assert!(
                titles(source).iter().all(|t| !t.contains("Prefix")),
                "no second underscore for `{source}`"
            );
        }
    }

    /// A diagnostic from another language server must never drive our fixes.
    #[test]
    fn a_foreign_diagnostic_is_ignored() {
        let source = "void Main() { int x = 1; }";
        let (analysis, mut diagnostics) = published(source);
        assert!(!diagnostics.is_empty(), "test premise");
        for diagnostic in &mut diagnostics {
            diagnostic.source = Some("some-other-server".to_string());
        }
        assert!(fixes_for(&uri(), &analysis, &diagnostics).is_empty());
    }

    /// Our own diagnostics of other kinds must not be answered here.
    #[test]
    fn a_non_unused_diagnostic_of_ours_is_ignored() {
        let source = "void Main() { int x = 1; }";
        let (analysis, mut diagnostics) = published(source);
        for diagnostic in &mut diagnostics {
            diagnostic.code = Some(NumberOrString::String("something-else".to_string()));
            diagnostic.message = "unrelated".to_string();
        }
        assert!(fixes_for(&uri(), &analysis, &diagnostics).is_empty());
    }

    /// A stale range — the document moved on since the diagnostic was
    /// published — must not rename whatever text now sits there.
    #[test]
    fn a_range_that_is_no_longer_an_identifier_offers_nothing() {
        let source = "void Main() { int x = 1; }";
        let (analysis, mut diagnostics) = published(source);
        for diagnostic in &mut diagnostics {
            diagnostic.range = Range::new(Position::new(0, 0), Position::new(0, 4));
        }
        let titles: Vec<_> = fixes_for(&uri(), &analysis, &diagnostics)
            .into_iter()
            .map(|a| a.title)
            .collect();
        assert!(titles.is_empty(), "got {titles:?}");
    }

    /// File-level declarations and class fields are out of scope, and the
    /// name-span guard is what keeps them out: those findings are anchored
    /// on the WHOLE declaration (`void Unused() {}` is reported over all 16
    /// bytes), not on the name, so there is no identifier token at the range
    /// and no action is offered. That is the right answer as well as the
    /// automatic one — renaming a file-level symbol to `_Unused` is a rename
    /// with call-site consequences, not the local suppression this producer
    /// implements. Locals and parameters, by contrast, ARE anchored on the
    /// name, which is exactly the population that gets fixes.
    #[test]
    fn findings_anchored_on_the_whole_declaration_offer_nothing() {
        for source in [
            "void Unused() {}\nvoid Main() {}",
            "int unusedGlobal = 1;\nvoid Main() {}",
            "class C { int unusedField; }\nvoid Main() { C c; }",
        ] {
            let (analysis, diagnostics) = published(source);
            let whole_declaration: Vec<_> = diagnostics
                .iter()
                .filter(|diagnostic| {
                    let start = position_to_offset(source, diagnostic.range.start);
                    let end = position_to_offset(source, diagnostic.range.end);
                    // More than a bare name — a type, a `;`, a body.
                    source[start..end].contains(' ') || source[start..end].contains(';')
                })
                .collect();
            assert!(
                !whole_declaration.is_empty(),
                "test premise: `{source}` reports at least one whole-declaration finding"
            );
            let offered = fixes_for(&uri(), &analysis, &diagnostics);
            for diagnostic in whole_declaration {
                assert!(
                    offered.iter().all(|action| {
                        action.diagnostics.as_deref() != Some(std::slice::from_ref(diagnostic))
                    }),
                    "no action may answer the whole-declaration finding {:?} in `{source}`",
                    diagnostic.message
                );
            }
        }
    }

    /// A range that drifted onto a DIFFERENT identifier must be refused.
    /// The identifier check alone cannot catch this — `y` is a perfectly
    /// good identifier — so the diagnostic's own name is what settles it.
    /// Offering "Prefix `y`" here would underscore a variable that IS used.
    #[test]
    fn a_range_that_drifted_onto_another_identifier_offers_nothing() {
        let source = "void Main() { int x = 1; int y = 2; Print(y); }";
        let (analysis, mut diagnostics) = published(source);
        assert_eq!(diagnostics.len(), 1, "test premise: only `x` is unused");
        assert!(diagnostics[0].message.contains("`x`"));
        // Point the finding at `y` — an identifier, just the wrong one.
        let at = source.find("int y").expect("test premise") + "int ".len();
        assert_eq!(&source[at..at + 1], "y", "test premise");
        diagnostics[0].range = Range::new(
            Position::new(0, at as u32),
            Position::new(0, (at + 1) as u32),
        );
        let titles: Vec<_> = fixes_for(&uri(), &analysis, &diagnostics)
            .into_iter()
            .map(|action| action.title)
            .collect();
        assert!(titles.is_empty(), "got {titles:?}");
    }

    /// A virtual property's accessors are ordinary bodies for bindings, and
    /// the checker records their locals, so those locals must get the same
    /// two fixes as any other local — not the prefix alone.
    #[test]
    fn locals_inside_property_accessors_are_treated_as_locals() {
        for source in [
            "int Prop { get { int x = 1; return 2; } }\nvoid Main() { Print(\"\" + Prop); }",
            "class C { int Prop { get { int x = 1; return 2; } } }\nvoid Main() { C c; Print(\"\" + c.Prop); }",
            "class C { int Prop { get { return 2; } set { int x = 1; } } }\nvoid Main() { C c; c.Prop = 1; Print(\"\" + c.Prop); }",
        ] {
            let titles = titles(source);
            assert!(
                titles
                    .iter()
                    .any(|t| t == "Prefix `x` with `_` to mark it unused"),
                "accessor local gets the prefix in `{source}`, got {titles:?}"
            );
            assert!(
                titles.iter().any(|t| t == "Remove unused local `x`"),
                "an accessor local is a local and is removable in `{source}`, got {titles:?}"
            );
        }
    }

    /// Constructor-style initialization is the near miss to the keep-call
    /// rung and stays out deliberately: the parser's `TypeConstruct` span
    /// covers the ARGUMENT LIST only (`(1)` for `C f(1)`), so keeping the
    /// call would mean re-synthesizing the type text — which may carry a
    /// `const` or a reference decoration that makes `T(args)` invalid. And
    /// where the text can be reconstructed, `C f(1);` -> `C(1);` moves
    /// destruction from scope exit to the end of the statement — a
    /// behaviour change hiding inside a "removal". Locked in as a decision,
    /// not left as an accident.
    #[test]
    fn a_constructor_style_initializer_offers_no_removal() {
        let source = "class C { C(int n) {} }\nvoid Main() { C f(1); Print(\"hi\"); }";
        assert_eq!(
            titles(source),
            vec!["Prefix `f` with `_` to mark it unused"],
            "the prefix applies; removal does not"
        );
    }

    // ── Wiring ───────────────────────────────────────────────────────────

    /// The registry row this family costs actually reaches it through the
    /// real handler, for a resolving and a non-resolving client alike — and
    /// the edit arrives either way, up front for the client that cannot
    /// resolve and on resolve for the one that can. Before B047 this
    /// producer was wired into the handler by hand and always eager.
    #[test]
    fn the_fixes_reach_the_client_through_the_code_action_handler() {
        let source = "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}";
        let (analysis, diagnostics) = published(source);
        let (_, workspace, uri) = crate::server::code_actions::test_fixtures::ctx_for(source);
        let context = CodeActionContext {
            diagnostics: diagnostics.clone(),
            ..Default::default()
        };
        let files = [(uri.clone(), &analysis)];
        for lazy_edits in [false, true] {
            let actions: Vec<_> = crate::server::code_actions::code_actions(&CodeActionRequest {
                uri: &uri,
                analysis: &analysis,
                range: diagnostics[0].range,
                context: &context,
                workspace: &workspace,
                type_index: None,
                files: WorkspaceFileResolver::new(&files),
                lazy_edits,
            })
            .into_iter()
            .filter_map(|action| match action {
                CodeActionOrCommand::CodeAction(action) => Some(action),
                CodeActionOrCommand::Command(_) => None,
            })
            .filter(|action| action.title.contains('`') && action.title.contains('x'))
            .collect();
            assert_eq!(
                actions.len(),
                2,
                "both unused fixes must survive the handler (lazy_edits={lazy_edits})"
            );
            for action in actions {
                let title = action.title.clone();
                let resolved = resolve_code_action(
                    action,
                    &analysis,
                    &workspace,
                    None,
                    WorkspaceFileResolver::new(&files),
                );
                let edits = resolved
                    .edit
                    .unwrap_or_else(|| {
                        panic!("`{title}` produced no edit (lazy_edits={lazy_edits})")
                    })
                    .changes
                    .expect("single-file edit")
                    .remove(&uri)
                    .expect("edits for the current document");
                assert_ne!(
                    apply_edits(source, &edits),
                    source,
                    "`{title}` resolved to an inert edit (lazy_edits={lazy_edits})"
                );
            }
        }
    }

    /// **The fix-all decision (B047).** Neither fix is `auto_applicable`,
    /// so `source.fixAll.openplanet` never applies one on save.
    ///
    /// Since B047 that is a decision rather than an accident of wiring: a
    /// diagnostic-keyed family CAN be batched now. Three reasons it is not:
    ///
    /// 1. The two edits overlap, and the batcher would keep the LOSSY one —
    ///    see
    ///    [`the_two_fixes_overlap_and_the_batcher_would_keep_the_removal`].
    /// 2. "Declared but not yet referenced" is the normal state of code
    ///    being typed. Every other auto-applicable family fires on code
    ///    that is finished and wrong; this one fires on code that is
    ///    unfinished, so renaming `count` to `_count` on save turns the
    ///    next keystroke into an undefined identifier.
    /// 3. For a parameter the prefix is a rename of an API surface —
    ///    AngelScript has named arguments — which is exactly the class
    ///    `rename_and_const/*` already refuses to auto-apply.
    #[test]
    fn no_unused_fix_is_batched_into_fix_all() {
        let source = "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}";
        let (analysis, diagnostics) = published(source);
        let (_, workspace, uri) = crate::server::code_actions::test_fixtures::ctx_for(source);
        let context = CodeActionContext {
            diagnostics: diagnostics.clone(),
            only: Some(vec![SOURCE_FIX_ALL_OPENPLANET]),
            ..Default::default()
        };
        let files = [(uri.clone(), &analysis)];
        for action in crate::server::code_actions::code_actions(&CodeActionRequest {
            uri: &uri,
            analysis: &analysis,
            range: diagnostics[0].range,
            context: &context,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::new(&files),
            lazy_edits: false,
        }) {
            let CodeActionOrCommand::CodeAction(action) = action else {
                continue;
            };
            assert!(
                !action.title.contains("unused"),
                "an unused fix must never be applied on save, got {:?}",
                action.title
            );
        }
    }

    /// The evidence behind [`no_unused_fix_is_batched_into_fix_all`]: the
    /// two fixes for one declarator do not merely compete, their edits
    /// properly OVERLAP — the name span sits inside the statement span — so
    /// the batcher must drop one of them. Its rule is earliest start first,
    /// and the removal starts at the line start, so the survivor is the
    /// LOSSY fix. Marking the `_` prefix `auto_applicable` would therefore
    /// not add a safe fix on save; it would delete the declaration.
    #[test]
    fn the_two_fixes_overlap_and_the_batcher_would_keep_the_removal() {
        for (source, deleted) in [
            (
                "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}",
                "void Main() {\n    Print(\"hi\");\n}",
            ),
            // The keep-call rung overlaps the prefix just the same, and
            // batching it would still drop the declaration.
            (
                "int Foo() { return 1; }\nvoid Main() {\n    int x = Foo();\n    Print(\"hi\");\n}",
                "int Foo() { return 1; }\nvoid Main() {\n    Foo();\n    Print(\"hi\");\n}",
            ),
        ] {
            let offered = actions(source);
            assert_eq!(offered.len(), 2, "test premise: {:?}", titles(source));
            let fixes: Vec<DocumentFix> = offered
                .iter()
                .map(|action| DocumentFix {
                    title: action.title.clone(),
                    edits: action
                        .edit
                        .clone()
                        .expect("eager edit")
                        .changes
                        .expect("single-file edit")
                        .remove(&uri())
                        .expect("edits for this document"),
                })
                .collect();
            assert!(
                ranges_overlap(&fixes[0].edits[0].range, &fixes[1].edits[0].range),
                "the prefix and the removal must be shown to overlap, not assumed to"
            );
            let kept = select_non_overlapping(&fixes);
            assert_eq!(kept.len(), 1, "the batcher can only keep one of them");
            assert!(
                fixes[kept[0]].title.starts_with("Remove"),
                "the batcher keeps `{}` — the lossy fix wins on start position",
                fixes[kept[0]].title
            );
            assert_eq!(
                apply_edits(source, &merged_edits(&fixes)),
                deleted,
                "batching these would delete the declaration on save"
            );
        }
    }

    /// End-to-end, the way the editor drives it: ask
    /// `textDocument/codeAction` at the DIAGNOSTIC'S OWN RANGE, take the
    /// action that comes back, apply its edit, and assert the resulting
    /// source text. Proving an action is merely "offered" would pass just as
    /// well with an inert or wrongly-spanned edit.
    #[test]
    fn requesting_at_the_diagnostic_range_returns_an_edit_that_produces_the_expected_source() {
        for (source, title, expected) in [
            // Local: prefix.
            (
                "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}",
                "Prefix `x` with `_` to mark it unused",
                "void Main() {\n    int _x = 1;\n    Print(\"hi\");\n}",
            ),
            // Local: removal.
            (
                "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}",
                "Remove unused local `x`",
                "void Main() {\n    Print(\"hi\");\n}",
            ),
            // Local: removal keeping a side-effecting call.
            (
                "int Foo() { return 1; }\nvoid Main() {\n    int x = Foo();\n    Print(\"hi\");\n}",
                "Remove unused local `x`, keeping the call",
                "int Foo() { return 1; }\nvoid Main() {\n    Foo();\n    Print(\"hi\");\n}",
            ),
            // Parameter: prefix (the only fix it gets).
            (
                "void f(int p) { Print(\"hi\"); }\nvoid Main() { f(1); }",
                "Prefix `p` with `_` to mark it unused",
                "void f(int _p) { Print(\"hi\"); }\nvoid Main() { f(1); }",
            ),
        ] {
            let (analysis, diagnostics) = published(source);
            let diagnostic = diagnostics
                .iter()
                .find(|d| d.message.contains('`'))
                .expect("test premise: a finding was published");
            let (_, workspace, uri) = crate::server::code_actions::test_fixtures::ctx_for(source);
            let context = CodeActionContext {
                diagnostics: diagnostics.clone(),
                ..Default::default()
            };
            let files = [(uri.clone(), &analysis)];
            let action = crate::server::code_actions::code_actions(&CodeActionRequest {
                uri: &uri,
                analysis: &analysis,
                // The cursor sits on the squiggle, as it does in an editor.
                range: diagnostic.range,
                context: &context,
                workspace: &workspace,
                type_index: None,
                files: WorkspaceFileResolver::new(&files),
                lazy_edits: false,
            })
            .into_iter()
            .filter_map(|action| match action {
                CodeActionOrCommand::CodeAction(action) => Some(action),
                CodeActionOrCommand::Command(_) => None,
            })
            .find(|action| action.title == title)
            .unwrap_or_else(|| panic!("no action titled `{title}` for `{source}`"));
            let edits = action
                .edit
                .expect("the action must carry its edit")
                .changes
                .expect("single-file edit")
                .remove(&uri)
                .expect("edits for the current document");
            assert_eq!(
                apply_edits(source, &edits),
                expected,
                "applying `{title}` to `{source}`"
            );
        }
    }

    /// B035 contract: `codeAction/resolve` returns an action that already
    /// carries an edit untouched. An eager action must therefore survive a
    /// resolving client byte-identically — not be emptied, not error.
    #[test]
    fn resolving_an_eager_action_returns_it_unchanged() {
        let source = "void Main() { int x = 1; }";
        let (analysis, diagnostics) = published(source);
        let (_, workspace, _) = crate::server::code_actions::test_fixtures::ctx_for(source);
        for action in fixes_for(&uri(), &analysis, &diagnostics) {
            let resolved = crate::server::code_actions::resolve_code_action(
                action.clone(),
                &analysis,
                &workspace,
                None,
                WorkspaceFileResolver::empty(),
            );
            assert_eq!(resolved, action, "resolve must not disturb an eager action");
        }
    }

    #[test]
    fn every_action_carries_the_diagnostic_it_answers() {
        let source = "void Main() { int x = 1; }";
        let (_, diagnostics) = published(source);
        let actions = actions(source);
        assert!(!actions.is_empty(), "test premise: actions were offered");
        for action in actions {
            assert_eq!(
                action.diagnostics.as_deref(),
                Some(&diagnostics[..]),
                "the action must be tied back to the squiggle"
            );
        }
    }
}
