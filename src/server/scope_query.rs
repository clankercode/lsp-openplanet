//! AST-walking scope queries for hover / completion.
//!
//! This is the pragmatic counterpart to the full type-checker: instead of
//! recording per-node scope snapshots during checking, we re-walk the AST on
//! every hover/completion request and collect just the information the UI
//! needs. Cheap because requests are rare, files are small, and we already
//! have the parsed AST handy.
//!
//! The queries are intentionally narrow:
//!
//! - [`find_locals_in_scope`] returns every local variable declared inside
//!   the innermost function body enclosing `offset`, up to `offset` itself.
//!   Scope nesting is approximated by textual containment (each local's
//!   span lies inside the block where it was declared) — close enough for
//!   this iteration.
//! - [`find_enclosing_class`] walks the top-level items and nested
//!   namespaces/classes to find the class whose body contains `offset`.
//! - [`find_enclosing_function`] is shared between the two above.

use crate::parser::ast::{
    ClassDecl, ClassMember, FunctionBody, FunctionDecl, Item, NamespaceDecl, SourceFile, Stmt,
    StmtKind, TypeExpr, VarDeclStmt,
};

/// A local variable visible to hover/completion at some cursor position.
#[derive(Debug, Clone)]
pub struct LocalVar {
    pub name: String,
    /// Source text of the declared type expression (best-effort).
    pub type_text: String,
    /// Span of the declarator's name — used for tie-breaking.
    pub decl_offset: u32,
}

/// Collect every local declared in the innermost enclosing function up to
/// (but not including) `offset`. Function parameters are included.
///
/// Returns an empty vec if the cursor is not inside any function body.
pub fn find_locals_in_scope(source: &str, file: &SourceFile, offset: u32) -> Vec<LocalVar> {
    let Some(func) = find_enclosing_function(file, offset) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Parameters first (always in scope).
    for p in &func.params {
        if let Some(name) = &p.name {
            out.push(LocalVar {
                name: name.text(source).to_string(),
                type_text: type_expr_text(&p.type_expr, source),
                decl_offset: name.span.start,
            });
        }
    }
    if let Some(body) = &func.body {
        collect_locals_from_body(source, body, offset, &mut out);
    }
    out
}

/// Return the innermost function whose body's span contains `offset`.
pub fn find_enclosing_function(file: &SourceFile, offset: u32) -> Option<&FunctionDecl> {
    let mut best: Option<&FunctionDecl> = None;
    for item in &file.items {
        walk_items_for_function(item, offset, &mut best);
    }
    best
}

/// Return the innermost class whose body span contains `offset`, walking into
/// namespaces as needed.
pub fn find_enclosing_class(file: &SourceFile, offset: u32) -> Option<&ClassDecl> {
    let mut best: Option<&ClassDecl> = None;
    for item in &file.items {
        walk_items_for_class(item, offset, &mut best);
    }
    best
}

fn walk_items_for_function<'a>(
    item: &'a Item,
    offset: u32,
    best: &mut Option<&'a FunctionDecl>,
) {
    match item {
        Item::Function(func) => consider_function(func, offset, best),
        Item::Namespace(NamespaceDecl { items, .. }) => {
            for sub in items {
                walk_items_for_function(sub, offset, best);
            }
        }
        Item::Class(ClassDecl { members, .. }) => {
            for m in members {
                match m {
                    ClassMember::Method(f)
                    | ClassMember::Constructor(f)
                    | ClassMember::Destructor(f) => consider_function(f, offset, best),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn consider_function<'a>(
    func: &'a FunctionDecl,
    offset: u32,
    best: &mut Option<&'a FunctionDecl>,
) {
    let Some(body) = &func.body else { return };
    if body.span.start <= offset && offset <= body.span.end {
        // Prefer the narrower span (innermost).
        match *best {
            None => *best = Some(func),
            Some(prev) => {
                let prev_len = prev
                    .body
                    .as_ref()
                    .map(|b| b.span.end - b.span.start)
                    .unwrap_or(u32::MAX);
                let cur_len = body.span.end - body.span.start;
                if cur_len < prev_len {
                    *best = Some(func);
                }
            }
        }
    }
}

fn walk_items_for_class<'a>(
    item: &'a Item,
    offset: u32,
    best: &mut Option<&'a ClassDecl>,
) {
    match item {
        Item::Class(cls) => {
            if cls.span.start <= offset && offset <= cls.span.end {
                match *best {
                    None => *best = Some(cls),
                    Some(prev) => {
                        if (cls.span.end - cls.span.start) < (prev.span.end - prev.span.start) {
                            *best = Some(cls);
                        }
                    }
                }
            }
        }
        Item::Namespace(NamespaceDecl { items, .. }) => {
            for sub in items {
                walk_items_for_class(sub, offset, best);
            }
        }
        _ => {}
    }
}

fn collect_locals_from_body(
    source: &str,
    body: &FunctionBody,
    offset: u32,
    out: &mut Vec<LocalVar>,
) {
    for stmt in &body.stmts {
        collect_locals_from_stmt(source, stmt, offset, out);
    }
}

fn collect_locals_from_stmt(
    source: &str,
    stmt: &Stmt,
    offset: u32,
    out: &mut Vec<LocalVar>,
) {
    if stmt.span.start > offset {
        return;
    }
    match &stmt.kind {
        StmtKind::VarDecl(vd) => add_var_decl(source, vd, offset, out),
        StmtKind::Block(stmts) => {
            for s in stmts {
                collect_locals_from_stmt(source, s, offset, out);
            }
        }
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            collect_locals_from_stmt(source, then_branch, offset, out);
            if let Some(e) = else_branch {
                collect_locals_from_stmt(source, e, offset, out);
            }
        }
        StmtKind::For {
            init, body, ..
        } => {
            if let Some(i) = init {
                collect_locals_from_stmt(source, i, offset, out);
            }
            collect_locals_from_stmt(source, body, offset, out);
        }
        StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => {
            collect_locals_from_stmt(source, body, offset, out);
        }
        StmtKind::Switch { cases, .. } => {
            for case in cases {
                for s in &case.stmts {
                    collect_locals_from_stmt(source, s, offset, out);
                }
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            collect_locals_from_stmt(source, try_body, offset, out);
            collect_locals_from_stmt(source, catch_body, offset, out);
        }
        _ => {}
    }
}

fn add_var_decl(source: &str, vd: &VarDeclStmt, offset: u32, out: &mut Vec<LocalVar>) {
    // Only include declarators whose name ends before the cursor; otherwise
    // typing `int x = |` would offer `x` to itself.
    let ty_text = type_expr_text(&vd.type_expr, source);
    for decl in &vd.declarators {
        if decl.name.span.end > offset {
            continue;
        }
        out.push(LocalVar {
            name: decl.name.text(source).to_string(),
            type_text: ty_text.clone(),
            decl_offset: decl.name.span.start,
        });
    }
}

/// Display-ish text for a type expression. Pragmatic: grabs the source slice
/// covered by the span. That's accurate for simple declarations and good
/// enough for hover tooltips.
pub fn type_expr_text(type_expr: &TypeExpr, source: &str) -> String {
    let start = type_expr.span.start as usize;
    let end = type_expr.span.end as usize;
    if start <= end && end <= source.len() {
        source[start..end].trim().to_string()
    } else {
        String::new()
    }
}

/// Best-effort: given a local's declared type text like `C@`, `const C&`,
/// `array<Foo>`, etc., strip handle/ref/const/array decorations and return the
/// bare base type name (e.g., `C`, `C`, `Foo`). Returns `None` if the result
/// would be empty or looks like a primitive keyword the member-lookup layer
/// doesn't handle.
pub fn strip_to_base_type(type_text: &str) -> Option<String> {
    let trimmed = type_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Drop `const ` prefix.
    let without_const = trimmed.strip_prefix("const ").unwrap_or(trimmed).trim();
    // Drop trailing `@`, `&`, `&in`, `&out`, `&inout`, `[]`.
    let mut s = without_const.to_string();
    loop {
        let before = s.clone();
        for suf in ["&inout", "&in", "&out", "&", "@", "[]"] {
            if let Some(stripped) = s.strip_suffix(suf) {
                s = stripped.trim_end().to_string();
            }
        }
        if s == before {
            break;
        }
    }
    let s = s.trim().to_string();
    if s.is_empty() {
        return None;
    }
    // Reject primitives — caller can special-case them.
    const PRIM: &[&str] = &[
        "void", "bool", "int", "uint", "float", "double", "string", "int8", "int16", "int64",
        "uint8", "uint16", "uint64", "auto",
    ];
    if PRIM.contains(&s.as_str()) {
        return None;
    }
    Some(s)
}

/// Find a field or property declared directly on `cls` with the given name.
/// Returns its type text.
pub fn class_member_type(cls: &ClassDecl, source: &str, member: &str) -> Option<String> {
    for m in &cls.members {
        match m {
            ClassMember::Field(vd) => {
                for d in &vd.declarators {
                    if d.name.text(source) == member {
                        return Some(type_expr_text(&vd.type_expr, source));
                    }
                }
            }
            ClassMember::Property(p) => {
                if p.name.text(source) == member {
                    return Some(type_expr_text(&p.type_expr, source));
                }
            }
            _ => {}
        }
    }
    None
}

/// Walk `func.body` looking for the most recent `VarDecl` whose declarator
/// matches `name` and lies before `offset`; return its type text.
pub fn local_type_at(
    source: &str,
    file: &SourceFile,
    offset: u32,
    name: &str,
) -> Option<String> {
    let locals = find_locals_in_scope(source, file, offset);
    // Prefer the local with the largest decl_offset (most recent, so innermost).
    locals
        .into_iter()
        .filter(|l| l.name == name)
        .max_by_key(|l| l.decl_offset)
        .map(|l| l.type_text)
}

