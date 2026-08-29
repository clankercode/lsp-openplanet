//! Shared AST location + type-naming helpers for the B037 fix families.
//!
//! All five B037 diagnostics are anchored on a span but carry no pointer
//! back to the node the fix has to edit: `FloatTruncation` and `SignChange`
//! sit on the *initializer* while the fix needs the *declared type*,
//! `SignedUnsignedMismatch` sits on the whole comparison while the fix needs
//! one operand, `UnreachableCode` sits on the first dead statement while the
//! fix needs the whole dead run, and `MissingReturn` sits on the function
//! *name* while the fix needs the body's closing brace.
//!
//! So every family starts by re-finding its node. [`walk_file`] is the one
//! traversal that serves all of them: a visitor with no-op defaults, so a
//! family implements only the callback it needs and gains nothing to keep in
//! step when the AST grows a variant.
//!
//! Offsets are interchangeable between `DocumentAnalysis::source` and
//! `masked_source` — the preprocessor masks in place and documents
//! `masked_source.len() == source.len()` (`preprocessor/filter.rs`) — so a
//! span produced by the checker (which runs on the masked text) indexes the
//! original source directly. Fix text is always read from the ORIGINAL
//! source, which is what the user sees and what the edit lands in.

use crate::lexer::token::TokenKind;
use crate::lexer::Span;
use crate::parser::ast::*;

// ── Visitor ──────────────────────────────────────────────────────────────

/// Callbacks into a whole-file AST walk. Every method defaults to a no-op:
/// a family overrides just the node kind it re-finds.
///
/// The `'a` lifetime is the file's, so a visitor may *keep* the node it
/// finds rather than copying spans out of it.
pub trait AstVisit<'a> {
    /// Every statement *slice* the checker treats as a block — function
    /// bodies, nested `{ … }` blocks and switch-case bodies. Exactly the
    /// three lists `Checker::check_stmt_block` runs over, which is what
    /// makes it the right unit for an `UnreachableCode` dead tail.
    fn stmt_list(&mut self, _stmts: &'a [Stmt]) {}
    /// Every declared function, method, constructor, destructor and
    /// interface method (not lambdas — the checker never reports
    /// `MissingReturn` against one).
    fn function(&mut self, _func: &'a FunctionDecl) {}
    /// Every variable declaration: global, class field and local.
    fn var_decl(&mut self, _var: &'a VarDeclStmt) {}
    /// Every expression, outermost first.
    fn expr(&mut self, _expr: &'a Expr) {}
}

pub fn walk_file<'a>(file: &'a SourceFile, v: &mut dyn AstVisit<'a>) {
    walk_items(&file.items, v);
}

fn walk_items<'a>(items: &'a [Item], v: &mut dyn AstVisit<'a>) {
    for item in items {
        match item {
            Item::Function(func) => walk_function(func, v),
            Item::VarDecl(var) => walk_var_decl(var, v),
            Item::Class(class) => {
                for member in &class.members {
                    match member {
                        ClassMember::Field(var) => walk_var_decl(var, v),
                        ClassMember::Method(func)
                        | ClassMember::Constructor(func)
                        | ClassMember::Destructor(func) => walk_function(func, v),
                        ClassMember::Property(prop) => walk_property(prop, v),
                    }
                }
            }
            Item::Interface(iface) => {
                for method in &iface.methods {
                    walk_function(method, v);
                }
            }
            Item::Namespace(ns) => walk_items(&ns.items, v),
            Item::Enum(decl) => {
                for value in &decl.values {
                    if let Some(expr) = &value.value {
                        walk_expr(expr, v);
                    }
                }
            }
            Item::Property(prop) => walk_property(prop, v),
            Item::Funcdef(_) | Item::Typedef(_) | Item::Import(_) | Item::Error(_) => {}
        }
    }
}

fn walk_property<'a>(prop: &'a PropertyDecl, v: &mut dyn AstVisit<'a>) {
    if let Some(getter) = &prop.getter {
        walk_stmts(&getter.stmts, v);
    }
    if let Some((_, setter)) = &prop.setter {
        walk_stmts(&setter.stmts, v);
    }
}

fn walk_function<'a>(func: &'a FunctionDecl, v: &mut dyn AstVisit<'a>) {
    v.function(func);
    for param in &func.params {
        if let Some(default) = &param.default_value {
            walk_expr(default, v);
        }
    }
    if let Some(body) = &func.body {
        walk_stmts(&body.stmts, v);
    }
}

fn walk_stmts<'a>(stmts: &'a [Stmt], v: &mut dyn AstVisit<'a>) {
    v.stmt_list(stmts);
    for stmt in stmts {
        walk_stmt(stmt, v);
    }
}

fn walk_stmt<'a>(stmt: &'a Stmt, v: &mut dyn AstVisit<'a>) {
    match &stmt.kind {
        StmtKind::Expr(expr) => walk_expr(expr, v),
        StmtKind::VarDecl(var) => walk_var_decl(var, v),
        StmtKind::Block(inner) => walk_stmts(inner, v),
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            walk_expr(condition, v);
            walk_stmt(then_branch, v);
            if let Some(branch) = else_branch {
                walk_stmt(branch, v);
            }
        }
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => {
            if let Some(init) = init {
                walk_stmt(init, v);
            }
            if let Some(condition) = condition {
                walk_expr(condition, v);
            }
            for expr in step {
                walk_expr(expr, v);
            }
            walk_stmt(body, v);
        }
        StmtKind::While { condition, body } => {
            walk_expr(condition, v);
            walk_stmt(body, v);
        }
        StmtKind::DoWhile { body, condition } => {
            walk_stmt(body, v);
            walk_expr(condition, v);
        }
        StmtKind::Switch { expr, cases } => {
            walk_expr(expr, v);
            for case in cases {
                if let SwitchLabel::Case(label) = &case.label {
                    walk_expr(label, v);
                }
                walk_stmts(&case.stmts, v);
            }
        }
        StmtKind::Return(value) => {
            if let Some(value) = value {
                walk_expr(value, v);
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            walk_stmt(try_body, v);
            walk_stmt(catch_body, v);
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Empty | StmtKind::Error => {}
    }
}

fn walk_var_decl<'a>(var: &'a VarDeclStmt, v: &mut dyn AstVisit<'a>) {
    v.var_decl(var);
    for declarator in &var.declarators {
        if let Some(init) = &declarator.init {
            walk_expr(init, v);
        }
    }
}

fn walk_expr<'a>(expr: &'a Expr, v: &mut dyn AstVisit<'a>) {
    v.expr(expr);
    match &expr.kind {
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, v);
            walk_expr(rhs, v);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Postfix { expr: inner, .. } => {
            walk_expr(inner, v)
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, v);
            for arg in args {
                walk_expr(&arg.value, v);
            }
        }
        ExprKind::Member { object, .. } => walk_expr(object, v),
        ExprKind::Index { object, index } => {
            walk_expr(object, v);
            walk_expr(index, v);
        }
        ExprKind::Cast { expr: inner, .. } => walk_expr(inner, v),
        ExprKind::TypeConstruct { args, .. } => {
            for arg in args {
                walk_expr(arg, v);
            }
        }
        ExprKind::Is {
            expr: inner,
            target,
            ..
        } => {
            walk_expr(inner, v);
            if let IsTarget::Expr(rhs) = target {
                walk_expr(rhs, v);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, v);
            walk_expr(then_expr, v);
            walk_expr(else_expr, v);
        }
        ExprKind::Assign { lhs, rhs, .. } | ExprKind::HandleAssign { lhs, rhs } => {
            walk_expr(lhs, v);
            walk_expr(rhs, v);
        }
        ExprKind::ArrayInit(items) => {
            for item in items {
                walk_expr(item, v);
            }
        }
        ExprKind::Lambda { params, body } => {
            for param in params {
                if let Some(default) = &param.default_value {
                    walk_expr(default, v);
                }
            }
            walk_stmts(&body.stmts, v);
        }
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::StringLit
        | ExprKind::HexLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Null
        | ExprKind::This
        | ExprKind::Super
        | ExprKind::Ident(_)
        | ExprKind::NamespaceAccess { .. }
        | ExprKind::Error => {}
    }
}

// ── Node lookups ─────────────────────────────────────────────────────────

/// The variable declaration and declarator whose initializer starts exactly
/// at `init_start` — the anchor `FloatTruncation` and `SignChange` use.
pub fn var_decl_by_init(
    file: &SourceFile,
    init_start: u32,
) -> Option<(&VarDeclStmt, &VarDeclarator)> {
    struct Find<'a> {
        init_start: u32,
        found: Option<(&'a VarDeclStmt, &'a VarDeclarator)>,
    }
    impl<'a> AstVisit<'a> for Find<'a> {
        fn var_decl(&mut self, var: &'a VarDeclStmt) {
            if self.found.is_some() {
                return;
            }
            self.found = var
                .declarators
                .iter()
                .find(|d| {
                    d.init
                        .as_ref()
                        .is_some_and(|init| init.span.start == self.init_start)
                })
                .map(|d| (var, d));
        }
    }
    let mut find = Find {
        init_start,
        found: None,
    };
    walk_file(file, &mut find);
    find.found
}

/// The binary expression spanning exactly `span` — the anchor
/// `SignedUnsignedMismatch` uses. Exact-span matching, not containment: the
/// checker anchors that diagnostic on the whole comparison, and a fix that
/// picked a different node would cast the wrong operand.
pub fn binary_by_span(file: &SourceFile, span: Span) -> Option<(&Expr, &Expr)> {
    struct Find<'a> {
        span: Span,
        found: Option<(&'a Expr, &'a Expr)>,
    }
    impl<'a> AstVisit<'a> for Find<'a> {
        fn expr(&mut self, expr: &'a Expr) {
            if self.found.is_some() || expr.span != self.span {
                return;
            }
            if let ExprKind::Binary { lhs, rhs, .. } = &expr.kind {
                self.found = Some((lhs, rhs));
            }
        }
    }
    let mut find = Find { span, found: None };
    walk_file(file, &mut find);
    find.found
}

/// The statements from the one starting at `start` to the end of its block —
/// the dead run an `UnreachableCode` fix deletes. The checker warns once, on
/// the FIRST unreachable statement, so everything after it in the same block
/// is dead too.
pub fn dead_tail(file: &SourceFile, start: u32) -> Option<&[Stmt]> {
    struct Find<'a> {
        start: u32,
        found: Option<&'a [Stmt]>,
    }
    impl<'a> AstVisit<'a> for Find<'a> {
        fn stmt_list(&mut self, stmts: &'a [Stmt]) {
            if self.found.is_some() {
                return;
            }
            if let Some(idx) = stmts.iter().position(|s| s.span.start == self.start) {
                self.found = Some(&stmts[idx..]);
            }
        }
    }
    let mut find = Find { start, found: None };
    walk_file(file, &mut find);
    find.found.filter(|tail| !tail.is_empty())
}

/// The function whose *name* spans exactly `name_span` — the anchor
/// `MissingReturn` uses.
pub fn function_by_name_span(file: &SourceFile, name_span: Span) -> Option<&FunctionDecl> {
    struct Find<'a> {
        name_span: Span,
        found: Option<&'a FunctionDecl>,
    }
    impl<'a> AstVisit<'a> for Find<'a> {
        fn function(&mut self, func: &'a FunctionDecl) {
            if self.found.is_none() && func.name.span == self.name_span {
                self.found = Some(func);
            }
        }
    }
    let mut find = Find {
        name_span,
        found: None,
    };
    walk_file(file, &mut find);
    find.found
}

// ── Type naming ──────────────────────────────────────────────────────────

/// The source keyword and its span for a primitive type expression, looking
/// through a leading `const`. `None` for every non-primitive shape (named
/// types, handles, arrays, templates, `auto`) — a family that cannot name
/// the type must offer nothing rather than guess.
pub fn primitive_type(ty: &TypeExpr) -> Option<(&'static str, Span)> {
    match &ty.kind {
        TypeExprKind::Const(inner) => primitive_type(inner),
        TypeExprKind::Primitive(kind) => primitive_keyword(*kind).map(|kw| (kw, ty.span)),
        _ => None,
    }
}

fn primitive_keyword(kind: TokenKind) -> Option<&'static str> {
    Some(match kind {
        TokenKind::KwVoid => "void",
        TokenKind::KwBool => "bool",
        TokenKind::KwInt => "int",
        TokenKind::KwInt8 => "int8",
        TokenKind::KwInt16 => "int16",
        TokenKind::KwInt32 => "int32",
        TokenKind::KwInt64 => "int64",
        TokenKind::KwUint => "uint",
        TokenKind::KwUint8 => "uint8",
        TokenKind::KwUint16 => "uint16",
        TokenKind::KwUint32 => "uint32",
        TokenKind::KwUint64 => "uint64",
        TokenKind::KwFloat => "float",
        TokenKind::KwDouble => "double",
        TokenKind::KwString => "string",
        _ => return None,
    })
}

/// Is `keyword` one of the integer primitives (the `FloatTruncation` /
/// `SignChange` targets)?
pub fn is_integer_keyword(keyword: &str) -> bool {
    signed_counterpart(keyword).is_some() || is_signed_keyword(keyword)
}

pub fn is_signed_keyword(keyword: &str) -> bool {
    matches!(keyword, "int" | "int8" | "int16" | "int32" | "int64")
}

/// The signed type with the same width, for an unsigned keyword.
pub fn signed_counterpart(keyword: &str) -> Option<&'static str> {
    Some(match keyword {
        "uint" => "int",
        "uint8" => "int8",
        "uint16" => "int16",
        "uint32" => "int32",
        "uint64" => "int64",
        _ => return None,
    })
}

// ── Text helpers ─────────────────────────────────────────────────────────

/// `span`'s text in the ORIGINAL document, trimmed. Returns `None` when the
/// span does not sit on character boundaries of this source — impossible for
/// a parser-produced span, but a fix must never panic in the server.
pub fn span_text(source: &str, span: Span) -> Option<&str> {
    source.get(span.start as usize..span.end as usize)
}

/// A title-safe rendering of `text`: whitespace collapsed, elided past
/// `max` characters so a multi-line operand cannot smear the action menu.
pub fn abbreviate(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let head: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::DocumentAnalysis;

    #[test]
    fn primitive_type_looks_through_const() {
        let analysis = DocumentAnalysis::analyze_plain("const uint64 g = 1;");
        let Item::VarDecl(var) = &analysis.file.items[0] else {
            panic!("expected a global var decl");
        };
        assert_eq!(primitive_type(&var.type_expr).unwrap().0, "uint64");
    }

    #[test]
    fn primitive_type_refuses_non_primitives() {
        for src in [
            "MyType g;",
            "MyType@ g;",
            "int[] g;",
            "array<int> g;",
            "auto g = 1;",
        ] {
            let analysis = DocumentAnalysis::analyze_plain(src);
            let Item::VarDecl(var) = &analysis.file.items[0] else {
                panic!("expected a global var decl for `{src}`");
            };
            assert!(
                primitive_type(&var.type_expr).is_none(),
                "expected no primitive keyword for `{src}`"
            );
        }
    }

    #[test]
    fn signed_counterpart_covers_every_unsigned_width() {
        for (unsigned, signed) in [
            ("uint", "int"),
            ("uint8", "int8"),
            ("uint16", "int16"),
            ("uint32", "int32"),
            ("uint64", "int64"),
        ] {
            assert_eq!(signed_counterpart(unsigned), Some(signed));
        }
        assert_eq!(signed_counterpart("int"), None);
        assert_eq!(signed_counterpart("float"), None);
    }

    #[test]
    fn integer_keywords_exclude_float_and_double() {
        for kw in [
            "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
        ] {
            assert!(is_integer_keyword(kw), "`{kw}` should be an integer");
        }
        for kw in ["float", "double", "bool", "string", "void"] {
            assert!(!is_integer_keyword(kw), "`{kw}` should not be an integer");
        }
    }

    #[test]
    fn abbreviate_collapses_and_elides() {
        assert_eq!(abbreviate("a  +\n  b", 40), "a + b");
        assert_eq!(abbreviate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn walk_reaches_locals_in_every_nesting_shape() {
        // Each source declares exactly one `int inner` local; the walk must
        // reach it wherever the parser puts it.
        for src in [
            "void f() { int inner = 1; }",
            "void f() { { int inner = 1; } }",
            "void f() { if (true) { int inner = 1; } }",
            "void f() { while (true) { int inner = 1; } }",
            "void f() { do { int inner = 1; } while (true); }",
            "void f() { for (int i = 0; i < 1; i++) { int inner = 1; } }",
            "void f() { switch (1) { case 1: { int inner = 1; } } }",
            "void f() { try { int inner = 1; } catch { } }",
            "class C { void m() { int inner = 1; } }",
            "namespace N { void f() { int inner = 1; } }",
        ] {
            let analysis = DocumentAnalysis::analyze_plain(src);
            let init_start = src.find("inner = 1").map(|i| i + "inner = ".len()).unwrap() as u32;
            assert!(
                var_decl_by_init(&analysis.file, init_start).is_some(),
                "walk missed the local in `{src}`"
            );
        }
    }
}
