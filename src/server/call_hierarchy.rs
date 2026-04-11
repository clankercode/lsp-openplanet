//! LSP call hierarchy provider.
//!
//! Implements `textDocument/prepareCallHierarchy`,
//! `callHierarchy/incomingCalls`, and `callHierarchy/outgoingCalls`. The
//! provider walks each function/method body in the workspace, collects
//! `ExprKind::Call` sites, resolves each callee to a bare name, and matches
//! names against the prepared item. State survives the prepare → incoming /
//! outgoing round-trip via the `data` field (a JSON object carrying the
//! fully qualified name of the target function).
//!
//! # Simplifications
//!
//! - Callee resolution matches on the **bare** tail name only (same pragmatic
//!   shortcut used by `highlights.rs` and `find_references`). This means an
//!   overloaded or shadowed name can produce false matches. Method
//!   cross-file resolution is bare-name, unaware of receiver type.
//! - Only `ExprKind::Call` is considered. Constructor-as-call
//!   (`ExprKind::TypeConstruct`) and funcdef invocations are ignored.
//! - Only free functions and class methods in the open workspace are
//!   reachable. External type-index functions are not listed.

use std::collections::HashMap;

use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Position, Range,
    SymbolKind as LspSymbolKind, Url,
};

use crate::lexer::Span;
use crate::parser::Parser;
use crate::parser::ast::{
    ClassDecl, ClassMember, Expr, ExprKind, FunctionBody, FunctionDecl, Item, NamespaceDecl,
    SourceFile, Stmt, StmtKind, SwitchLabel, VarDeclStmt,
};
use crate::server::diagnostics::{position_to_offset, span_to_range};
use crate::server::navigation::WorkspaceFiles;
use crate::symbols::SymbolTable;
use crate::symbols::scope::SymbolKind as InternalSymbolKind;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Prepare a call hierarchy item for the identifier at `position`.
///
/// Resolution order:
/// 1. If the identifier matches a function or method in the workspace, that
///    declaration is returned.
/// 2. Otherwise, if the cursor is inside a call expression whose callee's
///    bare name matches a workspace function/method, the **target** of the
///    call is returned.
/// 3. Otherwise an empty vec (e.g. cursor on a local variable).
pub fn prepare(
    source: &str,
    _uri: &Url,
    position: Position,
    workspace: &SymbolTable,
    files: &WorkspaceFiles<'_>,
) -> Vec<CallHierarchyItem> {
    let Some(name) = identifier_at(source, position) else {
        return Vec::new();
    };

    // First: direct symbol lookup (qualified or bare tail).
    if let Some(item) = lookup_function_item(&name, workspace, files) {
        return vec![item];
    }
    let bare = bare_tail(&name);
    if bare != name {
        if let Some(item) = lookup_function_item(bare, workspace, files) {
            return vec![item];
        }
    }

    // Fall back to the enclosing-call shortcut: if the cursor is on a call
    // site, resolve to the callee target.
    let cursor = position_to_offset(source, position);
    let tokens = crate::lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file = parser.parse_file();
    if let Some(call_name) = find_enclosing_call_name(&file, source, cursor as u32) {
        if let Some(item) = lookup_function_item(&call_name, workspace, files) {
            return vec![item];
        }
        let call_bare = bare_tail(&call_name);
        if call_bare != call_name {
            if let Some(item) = lookup_function_item(call_bare, workspace, files) {
                return vec![item];
            }
        }
    }

    Vec::new()
}

/// Find every function / method in the workspace that calls `item`, grouped
/// by caller.
pub fn incoming(
    item: &CallHierarchyItem,
    _workspace: &SymbolTable,
    files: &WorkspaceFiles<'_>,
) -> Vec<CallHierarchyIncomingCall> {
    let target_bare = bare_tail(data_name(item).unwrap_or(item.name.as_str())).to_string();

    // Keyed by (caller qualified name, caller file_id).
    let mut buckets: HashMap<(String, usize), (CallHierarchyItem, Vec<Range>)> = HashMap::new();

    for (&fid, (file_uri, src)) in files.files.iter() {
        let tokens = crate::lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();

        visit_functions_src(&file, src, None, &mut |qname, decl| {
            let Some(body) = decl.body.as_ref() else {
                return;
            };
            let mut call_spans: Vec<Span> = Vec::new();
            collect_calls_matching(body, src, &target_bare, &mut call_spans);
            if call_spans.is_empty() {
                return;
            }
            let caller_item = build_item_for_decl(qname, decl, file_uri, src);
            let key = (qname.to_string(), fid);
            let ranges: Vec<Range> = call_spans
                .into_iter()
                .map(|s| span_to_range(src, s))
                .collect();
            buckets
                .entry(key)
                .and_modify(|e| e.1.extend(ranges.iter().cloned()))
                .or_insert((caller_item, ranges));
        });
    }

    buckets
        .into_values()
        .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
        .collect()
}

/// Find every call expression inside `item`'s body, grouped by callee.
pub fn outgoing(
    item: &CallHierarchyItem,
    workspace: &SymbolTable,
    files: &WorkspaceFiles<'_>,
) -> Vec<CallHierarchyOutgoingCall> {
    let target_qname = data_name(item).unwrap_or(item.name.as_str()).to_string();
    let target_bare = bare_tail(&target_qname).to_string();

    // Find the function body in the workspace. We iterate files and match by
    // qualified name; if multiple files declare the same name, the first hit
    // wins. Iteration order is the HashMap's — non-deterministic across runs
    // but duplicate-qname collisions are rare enough in practice.
    for (_fid, (_file_uri, src)) in files.files.iter() {
        let tokens = crate::lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();

        let mut found_body: Option<FunctionBody> = None;
        visit_functions_src(&file, src, None, &mut |qname, decl| {
            if found_body.is_some() {
                return;
            }
            let matches = qname == target_qname
                || bare_tail(qname) == target_bare;
            if matches {
                if let Some(b) = decl.body.as_ref() {
                    found_body = Some(b.clone());
                }
            }
        });
        if let Some(body) = found_body {
            return collect_outgoing(&body, src, workspace, files);
        }
    }

    Vec::new()
}

// ---------------------------------------------------------------------------
// Outgoing call collection
// ---------------------------------------------------------------------------

fn collect_outgoing(
    body: &FunctionBody,
    caller_src: &str,
    workspace: &SymbolTable,
    files: &WorkspaceFiles<'_>,
) -> Vec<CallHierarchyOutgoingCall> {
    let mut raw: Vec<(String, Span)> = Vec::new();
    for s in &body.stmts {
        collect_calls(s, caller_src, &mut raw);
    }
    // Group by resolved target qname (if any); dedupe duplicates.
    let mut buckets: HashMap<String, (CallHierarchyItem, Vec<Range>)> = HashMap::new();
    for (name, span) in raw {
        // Ranges are always from the CALLER's source.
        let range = span_to_range(caller_src, span);
        let resolved = lookup_function_item(&name, workspace, files).or_else(|| {
            let bare = bare_tail(&name);
            if bare == name {
                None
            } else {
                lookup_function_item(bare, workspace, files)
            }
        });
        let Some(item) = resolved else {
            continue;
        };
        let key = data_name(&item).unwrap_or(item.name.as_str()).to_string();
        buckets
            .entry(key)
            .and_modify(|e| e.1.push(range))
            .or_insert((item, vec![range]));
    }
    buckets
        .into_values()
        .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
        .collect()
}

// ---------------------------------------------------------------------------
// Lookup helpers
// ---------------------------------------------------------------------------

/// Walk `source` around `position` to pull out a qualified identifier name.
fn identifier_at(source: &str, position: Position) -> Option<String> {
    // Reuse navigation's token-based approach.
    crate::server::navigation::name_at_position(source, position)
}

fn bare_tail(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn data_name(item: &CallHierarchyItem) -> Option<&str> {
    item.data
        .as_ref()
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
}

/// Walk the workspace `SymbolTable` for a function/method whose name matches
/// `query` (exact qualified name, or bare tail). Returns a `CallHierarchyItem`
/// for the first match whose file is present in `files`.
fn lookup_function_item(
    query: &str,
    workspace: &SymbolTable,
    files: &WorkspaceFiles<'_>,
) -> Option<CallHierarchyItem> {
    // Exact-qualified first.
    let hits = workspace.lookup(query);
    let candidate = hits
        .iter()
        .find(|s| matches!(s.kind, InternalSymbolKind::Function { .. }))
        .copied();
    let candidate = candidate.or_else(|| {
        // Search all symbols with matching bare tail.
        workspace.all_symbols().find(|s| {
            matches!(s.kind, InternalSymbolKind::Function { .. })
                && bare_tail(&s.name) == query
        })
    })?;
    let (uri, src) = files.get(candidate.file_id)?;
    let range = span_to_range(src, candidate.span);
    let selection_range = select_name_range(src, candidate.span, bare_tail(&candidate.name));
    let kind = if candidate.name.contains("::") {
        LspSymbolKind::METHOD
    } else {
        LspSymbolKind::FUNCTION
    };
    Some(CallHierarchyItem {
        name: candidate.name.clone(),
        kind,
        tags: None,
        detail: None,
        uri: uri.clone(),
        range,
        selection_range,
        data: Some(serde_json::json!({ "name": candidate.name })),
    })
}

fn build_item_for_decl(
    qname: &str,
    decl: &FunctionDecl,
    uri: &Url,
    src: &str,
) -> CallHierarchyItem {
    let range = span_to_range(src, decl.span);
    let selection_range = span_to_range(src, decl.name.span);
    let kind = if qname.contains("::") {
        LspSymbolKind::METHOD
    } else {
        LspSymbolKind::FUNCTION
    };
    CallHierarchyItem {
        name: qname.to_string(),
        kind,
        tags: None,
        detail: None,
        uri: uri.clone(),
        range,
        selection_range,
        data: Some(serde_json::json!({ "name": qname })),
    }
}

/// Best-effort: scan the function's span for its declared name, returning a
/// narrower range suitable for the `selection_range` field.
fn select_name_range(src: &str, span: Span, name: &str) -> Range {
    let start = span.start as usize;
    let end = (span.end as usize).min(src.len());
    if start < end {
        let haystack = &src[start..end];
        if let Some(off) = haystack.find(name) {
            let nstart = (start + off) as u32;
            let nend = nstart + name.len() as u32;
            return span_to_range(src, Span::new(nstart, nend));
        }
    }
    span_to_range(src, span)
}

// ---------------------------------------------------------------------------
// AST visitors
// ---------------------------------------------------------------------------

/// Call `f` for every top-level function and class method in `file`, with a
/// constructed qualified name (including namespace / class prefix). The
/// `source` argument is used to read identifier text.
fn visit_functions_src<'a, F>(
    file: &'a SourceFile,
    source: &str,
    namespace: Option<&str>,
    f: &mut F,
) where
    F: FnMut(&str, &'a FunctionDecl),
{
    for item in &file.items {
        visit_item_src(item, source, namespace, f);
    }
}

fn visit_item_src<'a, F>(
    item: &'a Item,
    source: &str,
    namespace: Option<&str>,
    f: &mut F,
) where
    F: FnMut(&str, &'a FunctionDecl),
{
    match item {
        Item::Function(func) => {
            let name = func.name.text(source);
            let qname = qualify(namespace, name);
            f(&qname, func);
        }
        Item::Namespace(NamespaceDecl { name, items, .. }) => {
            let ns_name = qualify(namespace, name.text(source));
            for sub in items {
                visit_item_src(sub, source, Some(&ns_name), f);
            }
        }
        Item::Class(ClassDecl { name, members, .. }) => {
            let class_name = qualify(namespace, name.text(source));
            for m in members {
                match m {
                    ClassMember::Method(func)
                    | ClassMember::Constructor(func)
                    | ClassMember::Destructor(func) => {
                        let mname = func.name.text(source);
                        let qname = format!("{}::{}", class_name, mname);
                        f(&qname, func);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn qualify(namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(ns) => format!("{}::{}", ns, name),
        None => name.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Call-site walkers
// ---------------------------------------------------------------------------

/// Walk `body` and record `(callee_name, callee_span)` for every call site
/// whose callee resolves to a bare identifier, member expression, or
/// namespace access.
fn collect_calls(stmt: &Stmt, source: &str, out: &mut Vec<(String, Span)>) {
    match &stmt.kind {
        StmtKind::Expr(e) => collect_calls_in_expr(e, source, out),
        StmtKind::VarDecl(vd) => collect_calls_in_vardecl(vd, source, out),
        StmtKind::Block(stmts) => {
            for s in stmts {
                collect_calls(s, source, out);
            }
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_calls_in_expr(condition, source, out);
            collect_calls(then_branch, source, out);
            if let Some(e) = else_branch {
                collect_calls(e, source, out);
            }
        }
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => {
            if let Some(i) = init {
                collect_calls(i, source, out);
            }
            if let Some(c) = condition {
                collect_calls_in_expr(c, source, out);
            }
            for s in step {
                collect_calls_in_expr(s, source, out);
            }
            collect_calls(body, source, out);
        }
        StmtKind::While { condition, body } => {
            collect_calls_in_expr(condition, source, out);
            collect_calls(body, source, out);
        }
        StmtKind::DoWhile { body, condition } => {
            collect_calls(body, source, out);
            collect_calls_in_expr(condition, source, out);
        }
        StmtKind::Switch { expr, cases } => {
            collect_calls_in_expr(expr, source, out);
            for case in cases {
                if let SwitchLabel::Case(e) = &case.label {
                    collect_calls_in_expr(e, source, out);
                }
                for s in &case.stmts {
                    collect_calls(s, source, out);
                }
            }
        }
        StmtKind::Return(Some(e)) => collect_calls_in_expr(e, source, out),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            collect_calls(try_body, source, out);
            collect_calls(catch_body, source, out);
        }
        _ => {}
    }
}

fn collect_calls_in_vardecl(vd: &VarDeclStmt, source: &str, out: &mut Vec<(String, Span)>) {
    for d in &vd.declarators {
        if let Some(init) = &d.init {
            collect_calls_in_expr(init, source, out);
        }
    }
}

fn collect_calls_in_expr(expr: &Expr, source: &str, out: &mut Vec<(String, Span)>) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let Some((name, span)) = callee_name_span(callee, source) {
                out.push((name, span));
            }
            // Recurse into callee (for nested calls on the receiver) and args.
            collect_calls_in_expr(callee, source, out);
            for a in args {
                collect_calls_in_expr(a, source, out);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_calls_in_expr(lhs, source, out);
            collect_calls_in_expr(rhs, source, out);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Postfix { expr: inner, .. } => {
            collect_calls_in_expr(inner, source, out);
        }
        ExprKind::Member { object, .. } => collect_calls_in_expr(object, source, out),
        ExprKind::Index { object, index } => {
            collect_calls_in_expr(object, source, out);
            collect_calls_in_expr(index, source, out);
        }
        ExprKind::Cast { expr: inner, .. } => collect_calls_in_expr(inner, source, out),
        ExprKind::TypeConstruct { args, .. } => {
            for a in args {
                collect_calls_in_expr(a, source, out);
            }
        }
        ExprKind::Is { expr: inner, target, .. } => {
            collect_calls_in_expr(inner, source, out);
            if let crate::parser::ast::IsTarget::Expr(e) = target {
                collect_calls_in_expr(e, source, out);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_calls_in_expr(condition, source, out);
            collect_calls_in_expr(then_expr, source, out);
            collect_calls_in_expr(else_expr, source, out);
        }
        ExprKind::Assign { lhs, rhs, .. } | ExprKind::HandleAssign { lhs, rhs } => {
            collect_calls_in_expr(lhs, source, out);
            collect_calls_in_expr(rhs, source, out);
        }
        ExprKind::ArrayInit(items) => {
            for i in items {
                collect_calls_in_expr(i, source, out);
            }
        }
        ExprKind::Lambda { body, .. } => {
            for s in &body.stmts {
                collect_calls(s, source, out);
            }
        }
        _ => {}
    }
}

/// Pull a `(name, span)` pair from a call's callee expression. Supports
/// bare identifiers, namespace accesses, and member expressions. Returns
/// `None` for anything else.
fn callee_name_span(callee: &Expr, source: &str) -> Option<(String, Span)> {
    match &callee.kind {
        ExprKind::Ident(id) => Some((id.text(source).to_string(), id.span)),
        ExprKind::NamespaceAccess { path } => {
            let last = path.segments.last()?;
            Some((path.to_string(source), last.span))
        }
        ExprKind::Member { member, .. } => {
            Some((member.text(source).to_string(), member.span))
        }
        _ => None,
    }
}

fn collect_calls_matching(
    body: &FunctionBody,
    source: &str,
    target_bare: &str,
    out: &mut Vec<Span>,
) {
    let mut raw: Vec<(String, Span)> = Vec::new();
    for s in &body.stmts {
        collect_calls(s, source, &mut raw);
    }
    for (name, span) in raw {
        if bare_tail(&name) == target_bare {
            out.push(span);
        }
    }
}

// ---------------------------------------------------------------------------
// Enclosing-call discovery for prepare()
// ---------------------------------------------------------------------------

/// If `cursor` sits inside a `Call` expression (either on the callee or in
/// the arguments), return the bare callee name.
fn find_enclosing_call_name(file: &SourceFile, source: &str, cursor: u32) -> Option<String> {
    let mut result: Option<(String, u32)> = None;
    let mut visit = |expr: &Expr| {
        if expr.span.start > cursor || expr.span.end < cursor {
            return;
        }
        if let ExprKind::Call { callee, .. } = &expr.kind {
            if let Some((name, _)) = callee_name_span(callee, source) {
                // Track the innermost (smallest span) match.
                let width = expr.span.end - expr.span.start;
                if result.as_ref().is_none_or(|(_, w)| width < *w) {
                    result = Some((name, width));
                }
            }
        }
    };
    walk_file_exprs(file, &mut visit);
    result.map(|(n, _)| n)
}

fn walk_file_exprs<F: FnMut(&Expr)>(file: &SourceFile, f: &mut F) {
    for item in &file.items {
        walk_item_exprs(item, f);
    }
}

fn walk_item_exprs<F: FnMut(&Expr)>(item: &Item, f: &mut F) {
    match item {
        Item::Function(func) => {
            if let Some(body) = &func.body {
                for s in &body.stmts {
                    walk_stmt_exprs(s, f);
                }
            }
        }
        Item::Namespace(ns) => {
            for i in &ns.items {
                walk_item_exprs(i, f);
            }
        }
        Item::Class(cls) => {
            for m in &cls.members {
                if let ClassMember::Method(func)
                | ClassMember::Constructor(func)
                | ClassMember::Destructor(func) = m
                {
                    if let Some(body) = &func.body {
                        for s in &body.stmts {
                            walk_stmt_exprs(s, f);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn walk_stmt_exprs<F: FnMut(&Expr)>(stmt: &Stmt, f: &mut F) {
    match &stmt.kind {
        StmtKind::Expr(e) => walk_expr(e, f),
        StmtKind::VarDecl(vd) => {
            for d in &vd.declarators {
                if let Some(e) = &d.init {
                    walk_expr(e, f);
                }
            }
        }
        StmtKind::Block(stmts) => {
            for s in stmts {
                walk_stmt_exprs(s, f);
            }
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            walk_expr(condition, f);
            walk_stmt_exprs(then_branch, f);
            if let Some(e) = else_branch {
                walk_stmt_exprs(e, f);
            }
        }
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => {
            if let Some(i) = init {
                walk_stmt_exprs(i, f);
            }
            if let Some(c) = condition {
                walk_expr(c, f);
            }
            for s in step {
                walk_expr(s, f);
            }
            walk_stmt_exprs(body, f);
        }
        StmtKind::While { condition, body } | StmtKind::DoWhile { body, condition } => {
            walk_expr(condition, f);
            walk_stmt_exprs(body, f);
        }
        StmtKind::Switch { expr, cases } => {
            walk_expr(expr, f);
            for c in cases {
                if let SwitchLabel::Case(e) = &c.label {
                    walk_expr(e, f);
                }
                for s in &c.stmts {
                    walk_stmt_exprs(s, f);
                }
            }
        }
        StmtKind::Return(Some(e)) => walk_expr(e, f),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            walk_stmt_exprs(try_body, f);
            walk_stmt_exprs(catch_body, f);
        }
        _ => {}
    }
}

fn walk_expr<F: FnMut(&Expr)>(expr: &Expr, f: &mut F) {
    f(expr);
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assign { lhs, rhs, .. }
        | ExprKind::HandleAssign { lhs, rhs } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Postfix { expr: inner, .. } => {
            walk_expr(inner, f);
        }
        ExprKind::Member { object, .. } => walk_expr(object, f),
        ExprKind::Index { object, index } => {
            walk_expr(object, f);
            walk_expr(index, f);
        }
        ExprKind::Cast { expr: inner, .. } => walk_expr(inner, f),
        ExprKind::TypeConstruct { args, .. } => {
            for a in args {
                walk_expr(a, f);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, f);
            walk_expr(then_expr, f);
            walk_expr(else_expr, f);
        }
        ExprKind::ArrayInit(items) => {
            for i in items {
                walk_expr(i, f);
            }
        }
        ExprKind::Is { expr: inner, target, .. } => {
            walk_expr(inner, f);
            if let crate::parser::ast::IsTarget::Expr(e) = target {
                walk_expr(e, f);
            }
        }
        ExprKind::Lambda { body, .. } => {
            for s in &body.stmts {
                walk_stmt_exprs(s, f);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Url;

    fn build_workspace(
        uri_str: &str,
        source: &str,
    ) -> (SymbolTable, HashMap<usize, (Url, String)>) {
        let mut table = SymbolTable::new();
        let tokens = crate::lexer::tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let symbols = SymbolTable::extract_symbols(fid, source, &file);
        table.set_file_symbols(fid, symbols);
        let mut files = HashMap::new();
        files.insert(fid, (Url::parse(uri_str).unwrap(), source.to_string()));
        (table, files)
    }

    #[test]
    fn test_prepare_on_function_declaration() {
        let src = "void foo() {}\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        // Cursor on `foo` (col 5-8)
        let items = prepare(src, &uri, Position::new(0, 6), &table, &ws);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "foo");
    }

    #[test]
    fn test_prepare_on_call_site() {
        let src = "void foo() {}\nvoid bar() { foo(); }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        // Cursor on `foo` call, line 1 col 14
        let items = prepare(src, &uri, Position::new(1, 14), &table, &ws);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "foo");
    }

    #[test]
    fn test_prepare_on_non_function_returns_empty() {
        let src = "void foo() { int x = 0; }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        // Cursor on `x` (col 17)
        let items = prepare(src, &uri, Position::new(0, 17), &table, &ws);
        assert!(items.is_empty());
    }

    #[test]
    fn test_incoming_calls_single_caller() {
        let src = "void a() {}\nvoid b() { a(); }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        // Prepare on `a` declaration
        let items = prepare(src, &uri, Position::new(0, 5), &table, &ws);
        assert_eq!(items.len(), 1);
        let incomings = incoming(&items[0], &table, &ws);
        assert_eq!(incomings.len(), 1);
        assert_eq!(incomings[0].from.name, "b");
        assert_eq!(incomings[0].from_ranges.len(), 1);
    }

    #[test]
    fn test_incoming_calls_multiple_callers() {
        let src = "void a() {}\nvoid b() { a(); }\nvoid c() { a(); }\nvoid d() { a(); }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        let items = prepare(src, &uri, Position::new(0, 5), &table, &ws);
        let incomings = incoming(&items[0], &table, &ws);
        let mut names: Vec<_> = incomings.iter().map(|c| c.from.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["b", "c", "d"]);
    }

    #[test]
    fn test_incoming_calls_multiple_sites_same_caller() {
        let src = "void a() {}\nvoid b() { a(); a(); }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        let items = prepare(src, &uri, Position::new(0, 5), &table, &ws);
        let incomings = incoming(&items[0], &table, &ws);
        assert_eq!(incomings.len(), 1);
        assert_eq!(incomings[0].from_ranges.len(), 2);
    }

    #[test]
    fn test_outgoing_calls_lists_callees() {
        let src = "void g() {}\nvoid h() {}\nvoid f() { g(); h(); }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        let items = prepare(src, &uri, Position::new(2, 5), &table, &ws);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "f");
        let outs = outgoing(&items[0], &table, &ws);
        let mut names: Vec<_> = outs.iter().map(|c| c.to.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["g", "h"]);
    }

    #[test]
    fn test_outgoing_calls_dedupes() {
        let src = "void g() {}\nvoid f() { g(); g(); }\n";
        let (table, files) = build_workspace("file:///t/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let uri = Url::parse("file:///t/a.as").unwrap();
        let items = prepare(src, &uri, Position::new(1, 5), &table, &ws);
        assert_eq!(items.len(), 1);
        let outs = outgoing(&items[0], &table, &ws);
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].to.name, "g");
        assert_eq!(outs[0].from_ranges.len(), 2);
    }
}
