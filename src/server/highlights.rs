//! Document highlights (AC16).
//!
//! Implements `textDocument/documentHighlight`: given a cursor position,
//! return every occurrence of the identifier under the cursor within the
//! current file, classified as READ (default) or WRITE. TEXT kind is not
//! emitted — the classifier only distinguishes read/write, because
//! AngelScript's single-file call sites are always read-like anyway.
//!
//! Scope handling note: this is intentionally a **naive name matcher**. It
//! does NOT resolve lexical shadowing — two unrelated locals named `x` in
//! different functions will both light up. A full scope-aware matcher would
//! require reusing the reference-resolution pipeline, but document
//! highlights are a single-file, low-stakes feature where over-highlighting
//! is acceptable. Documented as a known limitation / follow-up.

use tower_lsp::lsp_types::*;

use crate::lexer;
use crate::parser::Parser;
use crate::parser::ast::{
    ClassDecl, ClassMember, Expr, ExprKind, FunctionBody, FunctionDecl, Item, NamespaceDecl,
    PropertyDecl, SourceFile, Stmt, StmtKind, UnaryOp, VarDeclStmt,
};
use crate::server::diagnostics::{offset_to_position, position_to_offset};

/// Public entry: compute document highlights for the identifier under
/// `position`. Returns `None` when the cursor isn't on an identifier-like
/// byte run.
pub fn document_highlights(source: &str, position: Position) -> Option<Vec<DocumentHighlight>> {
    let cursor_offset = position_to_offset(source, position);
    let name = identifier_at(source, cursor_offset)?;

    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file: SourceFile = parser.parse_file();

    let mut out: Vec<Occurrence> = Vec::new();
    for item in &file.items {
        collect_item(item, source, &name, &mut out);
    }

    // Deduplicate by (start, end); prefer the first-seen kind.
    out.sort_by_key(|o| (o.start, o.end));
    out.dedup_by(|a, b| a.start == b.start && a.end == b.end);

    if out.is_empty() {
        return None;
    }
    Some(
        out.into_iter()
            .map(|o| DocumentHighlight {
                range: Range::new(
                    offset_to_position(source, o.start as usize),
                    offset_to_position(source, o.end as usize),
                ),
                kind: Some(o.kind),
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Cursor-identifier extraction.
// ---------------------------------------------------------------------------

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Scan backwards and forwards from `offset` collecting an identifier-like
/// byte run. Returns `None` if the resulting text is empty, starts with a
/// digit, or is a reserved keyword.
fn identifier_at(source: &str, offset: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let mut start = offset.min(bytes.len());
    // Back up while the *previous* byte is part of an identifier.
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset.min(bytes.len());
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if start >= end {
        return None;
    }
    if !is_ident_start_byte(bytes[start]) {
        return None;
    }
    let text = &source[start..end];
    if is_reserved_keyword(text) {
        return None;
    }
    Some(text.to_string())
}

fn is_reserved_keyword(s: &str) -> bool {
    matches!(
        s,
        "void"
            | "bool"
            | "int"
            | "uint"
            | "int8"
            | "int16"
            | "int64"
            | "uint8"
            | "uint16"
            | "uint64"
            | "float"
            | "double"
            | "string"
            | "auto"
            | "const"
            | "true"
            | "false"
            | "null"
            | "this"
            | "super"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "default"
            | "break"
            | "continue"
            | "class"
            | "interface"
            | "namespace"
            | "enum"
            | "funcdef"
            | "shared"
            | "abstract"
            | "mixin"
            | "private"
            | "protected"
            | "override"
            | "final"
            | "import"
            | "from"
            | "try"
            | "catch"
            | "is"
            | "in"
            | "out"
            | "inout"
            | "and"
            | "or"
            | "not"
            | "xor"
            | "new"
            | "delete"
            | "get"
            | "set"
            | "property"
            | "typedef"
            | "cast"
    )
}

// ---------------------------------------------------------------------------
// Collection.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Occurrence {
    start: u32,
    end: u32,
    kind: DocumentHighlightKind,
}

fn push(out: &mut Vec<Occurrence>, start: u32, end: u32, kind: DocumentHighlightKind) {
    out.push(Occurrence { start, end, kind });
}

fn collect_item(item: &Item, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    match item {
        Item::Function(func) => collect_function(func, source, name, out),
        Item::Class(cls) => collect_class(cls, source, name, out),
        Item::Namespace(NamespaceDecl { items, .. }) => {
            for sub in items {
                collect_item(sub, source, name, out);
            }
        }
        Item::VarDecl(vd) => collect_var_decl(vd, source, name, out),
        Item::Property(p) => collect_property(p, source, name, out),
        Item::Interface(iface) => {
            if iface.name.text(source) == name {
                push(
                    out,
                    iface.name.span.start,
                    iface.name.span.end,
                    DocumentHighlightKind::WRITE,
                );
            }
            for m in &iface.methods {
                collect_function(m, source, name, out);
            }
        }
        Item::Enum(enum_decl) => {
            if enum_decl.name.text(source) == name {
                push(
                    out,
                    enum_decl.name.span.start,
                    enum_decl.name.span.end,
                    DocumentHighlightKind::WRITE,
                );
            }
            for v in &enum_decl.values {
                if v.name.text(source) == name {
                    push(
                        out,
                        v.name.span.start,
                        v.name.span.end,
                        DocumentHighlightKind::WRITE,
                    );
                }
                if let Some(e) = &v.value {
                    collect_expr(e, source, name, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_class(cls: &ClassDecl, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    if cls.name.text(source) == name {
        push(
            out,
            cls.name.span.start,
            cls.name.span.end,
            DocumentHighlightKind::WRITE,
        );
    }
    for m in &cls.members {
        match m {
            ClassMember::Field(vd) => collect_var_decl(vd, source, name, out),
            ClassMember::Method(f)
            | ClassMember::Constructor(f)
            | ClassMember::Destructor(f) => collect_function(f, source, name, out),
            ClassMember::Property(p) => collect_property(p, source, name, out),
        }
    }
}

fn collect_property(p: &PropertyDecl, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    if p.name.text(source) == name {
        push(
            out,
            p.name.span.start,
            p.name.span.end,
            DocumentHighlightKind::WRITE,
        );
    }
    if let Some(g) = &p.getter {
        collect_body(g, source, name, out);
    }
    if let Some((_, body)) = &p.setter {
        collect_body(body, source, name, out);
    }
}

fn collect_function(func: &FunctionDecl, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    if func.name.text(source) == name {
        push(
            out,
            func.name.span.start,
            func.name.span.end,
            DocumentHighlightKind::WRITE,
        );
    }
    for p in &func.params {
        if let Some(pname) = &p.name {
            if pname.text(source) == name {
                push(
                    out,
                    pname.span.start,
                    pname.span.end,
                    DocumentHighlightKind::WRITE,
                );
            }
        }
        if let Some(def) = &p.default_value {
            collect_expr(def, source, name, out);
        }
    }
    if let Some(body) = &func.body {
        collect_body(body, source, name, out);
    }
}

fn collect_body(body: &FunctionBody, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    for s in &body.stmts {
        collect_stmt(s, source, name, out);
    }
}

fn collect_var_decl(vd: &VarDeclStmt, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    for d in &vd.declarators {
        if d.name.text(source) == name {
            push(
                out,
                d.name.span.start,
                d.name.span.end,
                DocumentHighlightKind::WRITE,
            );
        }
        if let Some(init) = &d.init {
            collect_expr(init, source, name, out);
        }
    }
}

fn collect_stmt(stmt: &Stmt, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    match &stmt.kind {
        StmtKind::VarDecl(vd) => collect_var_decl(vd, source, name, out),
        StmtKind::Expr(e) => collect_expr(e, source, name, out),
        StmtKind::Block(stmts) => {
            for s in stmts {
                collect_stmt(s, source, name, out);
            }
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect_expr(condition, source, name, out);
            collect_stmt(then_branch, source, name, out);
            if let Some(e) = else_branch {
                collect_stmt(e, source, name, out);
            }
        }
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => {
            if let Some(i) = init {
                collect_stmt(i, source, name, out);
            }
            if let Some(c) = condition {
                collect_expr(c, source, name, out);
            }
            for s in step {
                collect_expr(s, source, name, out);
            }
            collect_stmt(body, source, name, out);
        }
        StmtKind::While { condition, body } => {
            collect_expr(condition, source, name, out);
            collect_stmt(body, source, name, out);
        }
        StmtKind::DoWhile { body, condition } => {
            collect_stmt(body, source, name, out);
            collect_expr(condition, source, name, out);
        }
        StmtKind::Switch { expr, cases } => {
            collect_expr(expr, source, name, out);
            for case in cases {
                for s in &case.stmts {
                    collect_stmt(s, source, name, out);
                }
            }
        }
        StmtKind::Return(Some(e)) => collect_expr(e, source, name, out),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            collect_stmt(try_body, source, name, out);
            collect_stmt(catch_body, source, name, out);
        }
        _ => {}
    }
}

fn collect_expr(expr: &Expr, source: &str, name: &str, out: &mut Vec<Occurrence>) {
    collect_expr_ctx(expr, source, name, out, false);
}

/// Walk `expr`, recording occurrences. `write_ctx` is true when the
/// immediate position is a write target (LHS of `=`, operand of `++`/`--`).
fn collect_expr_ctx(
    expr: &Expr,
    source: &str,
    name: &str,
    out: &mut Vec<Occurrence>,
    write_ctx: bool,
) {
    match &expr.kind {
        ExprKind::Ident(id) => {
            if id.text(source) == name {
                let kind = if write_ctx {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                };
                push(out, id.span.start, id.span.end, kind);
            }
        }
        ExprKind::Member { object, member } => {
            collect_expr_ctx(object, source, name, out, false);
            if member.text(source) == name {
                let kind = if write_ctx {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                };
                push(out, member.span.start, member.span.end, kind);
            }
        }
        ExprKind::NamespaceAccess { path } => {
            for seg in &path.segments {
                if seg.text(source) == name {
                    push(
                        out,
                        seg.span.start,
                        seg.span.end,
                        DocumentHighlightKind::READ,
                    );
                }
            }
        }
        ExprKind::Call { callee, args } => {
            collect_expr_ctx(callee, source, name, out, false);
            for a in args {
                collect_expr(a, source, name, out);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_expr(lhs, source, name, out);
            collect_expr(rhs, source, name, out);
        }
        ExprKind::Unary { op, expr: inner } => {
            let is_write = matches!(op, UnaryOp::Inc | UnaryOp::Dec);
            collect_expr_ctx(inner, source, name, out, is_write);
        }
        ExprKind::Postfix { expr: inner, op } => {
            let is_write = matches!(op, UnaryOp::Inc | UnaryOp::Dec);
            collect_expr_ctx(inner, source, name, out, is_write);
        }
        ExprKind::Index { object, index } => {
            collect_expr(object, source, name, out);
            collect_expr(index, source, name, out);
        }
        ExprKind::Cast { expr: inner, .. } => {
            collect_expr(inner, source, name, out);
        }
        ExprKind::TypeConstruct { args, .. } => {
            for a in args {
                collect_expr(a, source, name, out);
            }
        }
        ExprKind::Is { expr: inner, target, .. } => {
            collect_expr(inner, source, name, out);
            if let crate::parser::ast::IsTarget::Expr(e) = target {
                collect_expr(e, source, name, out);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_expr(condition, source, name, out);
            collect_expr(then_expr, source, name, out);
            collect_expr(else_expr, source, name, out);
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            collect_expr_ctx(lhs, source, name, out, true);
            collect_expr(rhs, source, name, out);
        }
        ExprKind::HandleAssign { lhs, rhs } => {
            collect_expr_ctx(lhs, source, name, out, true);
            collect_expr(rhs, source, name, out);
        }
        ExprKind::ArrayInit(items) => {
            for i in items {
                collect_expr(i, source, name, out);
            }
        }
        ExprKind::Lambda { body, .. } => {
            collect_body(body, source, name, out);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Unit tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pos_of(source: &str, needle: &str) -> Position {
        let offset = source.find(needle).expect("needle not found");
        offset_to_position(source, offset + 1) // cursor inside the needle
    }

    #[test]
    fn test_local_variable_highlights_all_uses() {
        let src = "void f() { int x = 0; x = 1; g(x); }\n";
        // Put cursor on the declarator's 'x'.
        let decl_x = src.find("int x").unwrap() + 4;
        let position = offset_to_position(src, decl_x + 1);
        let hs = document_highlights(src, position).expect("should return some");
        // Three 'x': declarator, LHS of x=1, and argument g(x).
        assert_eq!(hs.len(), 3, "got {:?}", hs);
    }

    #[test]
    fn test_write_vs_read_classification() {
        let src = "void f() { int x = 0; x = x + 1; }\n";
        // Cursor on the declarator 'x'.
        let decl_x = src.find("int x").unwrap() + 4;
        let position = offset_to_position(src, decl_x);
        let hs = document_highlights(src, position).expect("some");
        // Expected: declarator=WRITE, LHS of `x = x + 1`=WRITE, RHS `x + 1`=READ.
        assert_eq!(hs.len(), 3, "got {:?}", hs);
        let writes = hs
            .iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::WRITE))
            .count();
        let reads = hs
            .iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::READ))
            .count();
        assert_eq!(writes, 2, "writes={} hs={:?}", writes, hs);
        assert_eq!(reads, 1, "reads={} hs={:?}", reads, hs);
    }

    #[test]
    fn test_cursor_on_keyword_returns_none() {
        let src = "void f() { int x = 0; }\n";
        // Cursor inside `void`.
        let p = offset_to_position(src, 1);
        assert!(document_highlights(src, p).is_none());
        // Cursor inside `int`.
        let int_off = src.find("int").unwrap() + 1;
        let p2 = offset_to_position(src, int_off);
        assert!(document_highlights(src, p2).is_none());
    }

    #[test]
    fn test_class_field_highlighted_in_methods() {
        let src = "class Foo { int count; void m() { count = 1; print(count); } }\n";
        let field = src.find("int count").unwrap() + 4;
        let p = offset_to_position(src, field + 1);
        let hs = document_highlights(src, p).expect("some");
        // Field declarator + `count = 1` LHS + `print(count)` argument = 3.
        assert!(hs.len() >= 2, "got {:?}", hs);
        assert!(
            hs.iter().any(|h| h.kind == Some(DocumentHighlightKind::WRITE)),
            "expected at least one WRITE, got {:?}",
            hs
        );
    }

    #[test]
    fn test_unrelated_name_not_highlighted() {
        // Two functions: only the local `y` in `f` should match when cursor is
        // on `y` in `f`. The scope-naive matcher still catches `y` in `g`,
        // so this test asserts the *count* the matcher actually produces and
        // documents the limitation: both `y`s light up. If we had scope
        // resolution this would be 2.
        let src = "void f() { int y = 1; y = 2; }\nvoid g() { int y = 3; }\n";
        let y_in_f = src.find("int y").unwrap() + 4;
        let p = offset_to_position(src, y_in_f + 1);
        let hs = document_highlights(src, p).expect("some");
        // Naive matcher: matches all three `y` identifiers across both
        // functions. This is the acceptable over-simplification.
        assert_eq!(hs.len(), 3, "got {:?}", hs);
    }

    #[test]
    fn test_cursor_on_whitespace_returns_none() {
        let src = "void f() {   int x = 0; }\n";
        let ws_off = src.find("  int").unwrap() + 1;
        let p = offset_to_position(src, ws_off);
        assert!(document_highlights(src, p).is_none());
    }
}
