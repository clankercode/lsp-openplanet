//! Inlay hint provider (AC15).
//!
//! Produces three categories of hints for the visible range:
//!
//! 1. **Type hints** after an `auto` local declarator's name (e.g.
//!    `auto x = 5;` → shows `: int`). Only fires when the declared type is
//!    `auto`; explicit types already tell the reader what's going on.
//! 2. **Parameter-name hints** in front of literal arguments at call sites
//!    (e.g. `g(5, "x")` → shows `count:` and `name:`). The callee must
//!    resolve to a workspace or external function whose parameter at that
//!    position has a known name. Hints are suppressed when the argument
//!    expression is itself an identifier whose text matches the param name.
//! 3. **Return-type hints** after multi-line lambda parameter lists — not
//!    yet implemented because the checker's expression-type inference is
//!    not exposed on private API and lambdas would need a full frame/scope
//!    push to type-check. Tracked as a deferred AC15 sub-goal.
//!
//! Hints that lie outside the requested `Range` are dropped so the editor
//! only renders what it asked for.

use tower_lsp::lsp_types::*;

use crate::lexer;
use crate::parser::Parser;
use crate::parser::ast::{
    ClassMember, Expr, ExprKind, FunctionBody, FunctionDecl, Item, NamespaceDecl,
    SourceFile, Stmt, StmtKind, TypeExpr, TypeExprKind, VarDeclStmt,
};
use crate::server::diagnostics::offset_to_position;
use crate::symbols::SymbolTable;
use crate::symbols::scope::SymbolKind;
use crate::typedb::TypeIndex;

/// Public entry: compute inlay hints for the given source, restricted to
/// `range`. Walks the parsed AST once and collects hints for `auto` locals
/// and literal call arguments.
pub fn inlay_hints(
    source: &str,
    range: Range,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Vec<InlayHint> {
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file: SourceFile = parser.parse_file();

    let mut out: Vec<InlayHint> = Vec::new();
    for item in &file.items {
        collect_from_item(item, source, workspace, type_index, &mut out);
    }

    // Filter by requested range and drop hints with empty labels.
    out.into_iter()
        .filter(|h| position_in_range(h.position, range))
        .collect()
}

// ---------------------------------------------------------------------------
// AST walkers.
// ---------------------------------------------------------------------------

fn collect_from_item(
    item: &Item,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    match item {
        Item::Function(func) => collect_from_function(func, source, workspace, type_index, out),
        Item::Class(cls) => {
            for m in &cls.members {
                match m {
                    ClassMember::Method(f)
                    | ClassMember::Constructor(f)
                    | ClassMember::Destructor(f) => {
                        collect_from_function(f, source, workspace, type_index, out)
                    }
                    _ => {}
                }
            }
        }
        Item::Namespace(NamespaceDecl { items, .. }) => {
            for sub in items {
                collect_from_item(sub, source, workspace, type_index, out);
            }
        }
        _ => {}
    }
}

fn collect_from_function(
    func: &FunctionDecl,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    let Some(body) = &func.body else { return };
    collect_from_body(body, source, workspace, type_index, out);
}

fn collect_from_body(
    body: &FunctionBody,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    for s in &body.stmts {
        collect_from_stmt(s, source, workspace, type_index, out);
    }
}

fn collect_from_stmt(
    stmt: &Stmt,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    match &stmt.kind {
        StmtKind::VarDecl(vd) => {
            collect_var_decl_hint(vd, source, workspace, type_index, out);
            for d in &vd.declarators {
                if let Some(init) = &d.init {
                    collect_from_expr(init, source, workspace, type_index, out);
                }
            }
        }
        StmtKind::Expr(e) => collect_from_expr(e, source, workspace, type_index, out),
        StmtKind::Block(stmts) => {
            for s in stmts {
                collect_from_stmt(s, source, workspace, type_index, out);
            }
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_from_expr(condition, source, workspace, type_index, out);
            collect_from_stmt(then_branch, source, workspace, type_index, out);
            if let Some(e) = else_branch {
                collect_from_stmt(e, source, workspace, type_index, out);
            }
        }
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => {
            if let Some(i) = init {
                collect_from_stmt(i, source, workspace, type_index, out);
            }
            if let Some(c) = condition {
                collect_from_expr(c, source, workspace, type_index, out);
            }
            for s in step {
                collect_from_expr(s, source, workspace, type_index, out);
            }
            collect_from_stmt(body, source, workspace, type_index, out);
        }
        StmtKind::While { condition, body } => {
            collect_from_expr(condition, source, workspace, type_index, out);
            collect_from_stmt(body, source, workspace, type_index, out);
        }
        StmtKind::DoWhile { body, condition } => {
            collect_from_stmt(body, source, workspace, type_index, out);
            collect_from_expr(condition, source, workspace, type_index, out);
        }
        StmtKind::Switch { expr, cases } => {
            collect_from_expr(expr, source, workspace, type_index, out);
            for case in cases {
                for s in &case.stmts {
                    collect_from_stmt(s, source, workspace, type_index, out);
                }
            }
        }
        StmtKind::Return(Some(e)) => collect_from_expr(e, source, workspace, type_index, out),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            collect_from_stmt(try_body, source, workspace, type_index, out);
            collect_from_stmt(catch_body, source, workspace, type_index, out);
        }
        _ => {}
    }
}

fn collect_from_expr(
    expr: &Expr,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            collect_from_expr(callee, source, workspace, type_index, out);
            for a in args {
                collect_from_expr(a, source, workspace, type_index, out);
            }
            collect_param_name_hints(callee, args, source, workspace, type_index, out);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_from_expr(lhs, source, workspace, type_index, out);
            collect_from_expr(rhs, source, workspace, type_index, out);
        }
        ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => {
            collect_from_expr(expr, source, workspace, type_index, out);
        }
        ExprKind::Member { object, .. } => {
            collect_from_expr(object, source, workspace, type_index, out);
        }
        ExprKind::Index { object, index } => {
            collect_from_expr(object, source, workspace, type_index, out);
            collect_from_expr(index, source, workspace, type_index, out);
        }
        ExprKind::Cast { expr: inner, .. } => {
            collect_from_expr(inner, source, workspace, type_index, out);
        }
        ExprKind::TypeConstruct { args, .. } => {
            for a in args {
                collect_from_expr(a, source, workspace, type_index, out);
            }
        }
        ExprKind::Assign { lhs, rhs, .. } | ExprKind::HandleAssign { lhs, rhs } => {
            collect_from_expr(lhs, source, workspace, type_index, out);
            collect_from_expr(rhs, source, workspace, type_index, out);
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_from_expr(condition, source, workspace, type_index, out);
            collect_from_expr(then_expr, source, workspace, type_index, out);
            collect_from_expr(else_expr, source, workspace, type_index, out);
        }
        ExprKind::ArrayInit(items) => {
            for i in items {
                collect_from_expr(i, source, workspace, type_index, out);
            }
        }
        ExprKind::Lambda { body, .. } => {
            collect_from_body(body, source, workspace, type_index, out);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Type hints for `auto` locals.
// ---------------------------------------------------------------------------

fn collect_var_decl_hint(
    vd: &VarDeclStmt,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    // Only fire on `auto` (untyped inference). An explicit type already tells
    // the reader what they need to know.
    if !matches!(vd.type_expr.kind, TypeExprKind::Auto) {
        return;
    }
    for d in &vd.declarators {
        let Some(init) = &d.init else { continue };
        let Some(ty) = infer_init_type(init, source, workspace, type_index) else {
            continue;
        };
        if ty.is_empty() {
            continue;
        }
        // Position the hint right after the declarator name.
        let pos = offset_to_position(source, d.name.span.end as usize);
        out.push(InlayHint {
            position: pos,
            label: InlayHintLabel::String(format!(": {}", ty)),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        });
    }
}

/// Minimal expression type inference for the init of an `auto` local.
/// Returns the *display text* of the inferred type, or `None` when we can't
/// figure it out cheaply. Deliberately narrow — we only need to cover
/// constructs that are common on the right-hand side of a top-level
/// `auto x = ...`.
fn infer_init_type(
    expr: &Expr,
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
) -> Option<String> {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::HexLit(_) => Some("int".to_string()),
        ExprKind::FloatLit(_) => Some("float".to_string()),
        ExprKind::StringLit => Some("string".to_string()),
        ExprKind::BoolLit(_) => Some("bool".to_string()),
        ExprKind::Cast { target_type, .. } => Some(type_expr_text(target_type, source)),
        ExprKind::TypeConstruct { target_type, .. } => {
            Some(type_expr_text(target_type, source))
        }
        ExprKind::Call { callee, .. } => {
            let callee_text = extract_ident_chain(callee, source)?;
            lookup_callee_return_type(&callee_text, workspace, type_index)
        }
        _ => None,
    }
}

fn type_expr_text(ty: &TypeExpr, source: &str) -> String {
    let start = ty.span.start as usize;
    let end = ty.span.end as usize;
    if start <= end && end <= source.len() {
        source[start..end].trim().to_string()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Parameter-name hints at call sites.
// ---------------------------------------------------------------------------

fn collect_param_name_hints(
    callee: &Expr,
    args: &[Expr],
    source: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
    out: &mut Vec<InlayHint>,
) {
    let Some(callee_text) = extract_ident_chain(callee, source) else {
        return;
    };
    let Some(param_names) = lookup_callee_param_names(&callee_text, workspace, type_index) else {
        return;
    };
    for (idx, arg) in args.iter().enumerate() {
        let Some(name) = param_names.get(idx) else {
            break;
        };
        if name.is_empty() {
            continue;
        }
        if !is_literal_or_null(&arg.kind) {
            continue;
        }
        if let ExprKind::Ident(id) = &arg.kind {
            if id.text(source) == name {
                continue;
            }
        }
        let pos = offset_to_position(source, arg.span.start as usize);
        out.push(InlayHint {
            position: pos,
            label: InlayHintLabel::String(format!("{}:", name)),
            kind: Some(InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: Some(true),
            data: None,
        });
    }
}

fn is_literal_or_null(kind: &ExprKind) -> bool {
    matches!(
        kind,
        ExprKind::IntLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::StringLit
            | ExprKind::HexLit(_)
            | ExprKind::BoolLit(_)
            | ExprKind::Null
    )
}

/// Resolve `callee` (a dotted / namespaced identifier chain) to the
/// parameter-name vector of its first overload. Returns `None` when the
/// callee can't be resolved or has no parameter metadata.
fn lookup_callee_param_names(
    callee: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
) -> Option<Vec<String>> {
    // Workspace free functions are keyed by bare name only — the symbol
    // table doesn't track enclosing namespace. Only consult it for
    // unqualified callees; a qualified callee like `Foo::bar` goes straight
    // to the TypeIndex (which is keyed on fully qualified names) to avoid
    // matching an unrelated `Baz::bar` by its bare tail.
    if !callee.contains("::") {
        if let Some(ws) = workspace {
            if let Some(names) = lookup_workspace_function_params(callee, ws) {
                return Some(names);
            }
        }
    }
    if let Some(index) = type_index {
        if let Some(fns) = index.lookup_function(callee) {
            if let Some(f) = fns.first() {
                return Some(
                    f.params
                        .iter()
                        .map(|p| p.name.clone().unwrap_or_default())
                        .collect(),
                );
            }
        }
        if let Some(idx) = callee.rfind("::") {
            let bare = &callee[idx + 2..];
            if let Some(fns) = index.lookup_function(bare) {
                if let Some(f) = fns.first() {
                    return Some(
                        f.params
                            .iter()
                            .map(|p| p.name.clone().unwrap_or_default())
                            .collect(),
                    );
                }
            }
        }
    }
    None
}

fn lookup_workspace_function_params(
    name: &str,
    ws: &SymbolTable,
) -> Option<Vec<String>> {
    for s in ws.all_symbols() {
        if s.name != name {
            continue;
        }
        if let SymbolKind::Function { params, .. } = &s.kind {
            return Some(params.iter().map(|(n, _)| n.clone()).collect());
        }
    }
    None
}

/// Resolve the return type of `callee` for the `auto` hint inference. Same
/// precedence as `lookup_callee_param_names`, but returns the return type
/// text.
fn lookup_callee_return_type(
    callee: &str,
    workspace: Option<&SymbolTable>,
    type_index: Option<&TypeIndex>,
) -> Option<String> {
    if let Some(ws) = workspace {
        for s in ws.all_symbols() {
            if s.name != callee {
                continue;
            }
            if let SymbolKind::Function { return_type, .. } = &s.kind {
                if return_type.is_empty() {
                    continue;
                }
                return Some(return_type.clone());
            }
        }
    }
    if let Some(index) = type_index {
        if let Some(fns) = index.lookup_function(callee) {
            if let Some(f) = fns.first() {
                if !f.return_type.is_empty() {
                    return Some(f.return_type.clone());
                }
            }
        }
    }
    None
}

/// Walk a callee expression and collect a dotted/namespaced identifier
/// chain. Returns `Some("foo")`, `Some("Ns::foo")`, or `Some("recv.method")`
/// depending on the shape; `None` for anything more complex (calls on
/// literals, etc.).
fn extract_ident_chain(expr: &Expr, source: &str) -> Option<String> {
    match &expr.kind {
        ExprKind::Ident(id) => Some(id.text(source).to_string()),
        ExprKind::NamespaceAccess { path } => Some(path.to_string(source)),
        ExprKind::Member { object, member } => {
            let base = extract_ident_chain(object, source)?;
            Some(format!("{}.{}", base, member.text(source)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Range filtering.
// ---------------------------------------------------------------------------

fn position_in_range(p: Position, range: Range) -> bool {
    let after_start = p.line > range.start.line
        || (p.line == range.start.line && p.character >= range.start.character);
    let before_end = p.line < range.end.line
        || (p.line == range.end.line && p.character <= range.end.character);
    after_start && before_end
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use crate::parser::Parser;

    fn ws_from(source: &str) -> SymbolTable {
        let mut table = SymbolTable::new();
        let tokens = lexer::tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        table.set_file_symbols(fid, syms);
        table
    }

    fn full_range() -> Range {
        Range::new(Position::new(0, 0), Position::new(u32::MAX, u32::MAX))
    }

    #[test]
    fn test_auto_local_gets_type_hint() {
        let src = "void f() { auto x = 5; }";
        let hints = inlay_hints(src, full_range(), None, None);
        assert_eq!(hints.len(), 1, "expected one type hint, got {:?}", hints);
        let h = &hints[0];
        assert_eq!(h.kind, Some(InlayHintKind::TYPE));
        match &h.label {
            InlayHintLabel::String(s) => {
                assert_eq!(s, ": int", "expected `: int`, got {:?}", s);
            }
            _ => panic!("expected string label"),
        }
    }

    #[test]
    fn test_explicit_type_no_hint() {
        let src = "void f() { int x = 5; }";
        let hints = inlay_hints(src, full_range(), None, None);
        assert!(
            hints.is_empty(),
            "explicit type should not emit hints, got {:?}",
            hints
        );
    }

    #[test]
    fn test_param_name_hint_on_literal_arg() {
        let src = "void g(int count, string name) {}\nvoid main() { g(5, \"x\"); }";
        let ws = ws_from(src);
        let hints = inlay_hints(src, full_range(), None, Some(&ws));
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .collect();
        assert_eq!(
            param_hints.len(),
            2,
            "expected 2 param hints, got {:?}",
            param_hints
        );
        let labels: Vec<String> = param_hints
            .iter()
            .map(|h| match &h.label {
                InlayHintLabel::String(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(
            labels.iter().any(|l| l == "count:"),
            "expected `count:` in labels, got {:?}",
            labels
        );
        assert!(
            labels.iter().any(|l| l == "name:"),
            "expected `name:` in labels, got {:?}",
            labels
        );
    }

    #[test]
    fn test_param_name_hint_suppressed_when_arg_name_matches() {
        // `count` is an ident arg matching the param name → suppressed.
        // `"x"` is a literal arg → hinted.
        let src = "void g(int count, string name) {}\n\
                   void main() { int count = 3; g(count, \"x\"); }";
        let ws = ws_from(src);
        let hints = inlay_hints(src, full_range(), None, Some(&ws));
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .collect();
        assert_eq!(
            param_hints.len(),
            1,
            "expected exactly one param hint, got {:?}",
            param_hints
        );
        match &param_hints[0].label {
            InlayHintLabel::String(s) => assert_eq!(s, "name:"),
            _ => panic!("expected string label"),
        }
    }

    #[test]
    fn test_out_of_range_hints_skipped() {
        // Hints on line 1 should be excluded by a range covering only line 0.
        let src = "void f() {\n  auto x = 5;\n}";
        let range = Range::new(Position::new(0, 0), Position::new(0, 100));
        let hints = inlay_hints(src, range, None, None);
        assert!(
            hints.is_empty(),
            "expected no hints outside range, got {:?}",
            hints
        );
    }

    #[test]
    fn test_unresolved_callee_emits_no_param_hints() {
        // `mystery` is not declared anywhere — must not panic, must not emit.
        let src = "void main() { mystery(1, 2); }";
        let ws = ws_from(src);
        let hints = inlay_hints(src, full_range(), None, Some(&ws));
        assert!(
            hints
                .iter()
                .all(|h| h.kind != Some(InlayHintKind::PARAMETER)),
            "expected no param hints for unresolved callee, got {:?}",
            hints
        );
    }
}

