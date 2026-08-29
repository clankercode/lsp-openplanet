//! Lexical-scope machinery shared by the B038 fix families.
//!
//! The server already has two identifier-occurrence finders
//! ([`crate::server::navigation::rename`] and
//! [`crate::server::highlights`]) and **neither is scope-aware**: both
//! rewrite/light-up every `Ident` token in the workspace whose text matches
//! a bare name. That is acceptable for a highlight and tolerable for a
//! user-driven rename where the user picks the symbol, but it is exactly
//! wrong for the shadow fix, whose entire point is to rename the *inner*
//! binding and leave the outer one alone. So the rename here is built on a
//! narrow, conservative scope model instead.
//!
//! The model has one job: given the offset of a local's declarator, decide
//! the half-open byte range `[decl, scope_end)` in which references to that
//! local live. It is deliberately built from two independent sources and
//! takes the tighter answer:
//!
//! * **AST statement scopes** — function/lambda bodies, block statements,
//!   `if` branches, loop bodies, `switch` cases, `try`/`catch` bodies, and
//!   the whole `for` statement (a `for`-init declaration is visible in the
//!   condition, step and body, so the scope is the statement, not its
//!   body's braces).
//! * **Brace regions from the token stream** — every `{ … }` pair. This
//!   covers scopes the statement walk cannot reach without an expression
//!   visitor, most importantly a lambda body nested inside an expression.
//!
//! Neither source alone is right: braces alone get `for`-init wrong (too
//! wide), and statements alone miss lambda bodies (also too wide). The
//! narrower of the two is never too wide, and "too narrow" is not a failure
//! mode here — both sources always contain the declaration itself.
//!
//! Everything in this module is intentionally local to the B038 families.
//! Promoting it to `navigation` so `textDocument/rename` becomes
//! scope-aware is a worthwhile follow-up, but that would change a live
//! request handler's behaviour and is out of this ticket's scope.

use crate::lexer::{self, Span, Token, TokenKind};
use crate::parser::ast::{
    ClassMember, FunctionBody, FunctionDecl, Item, SourceFile, Stmt, StmtKind, VarDeclStmt,
};

/// `span` contains `offset` (half-open).
fn contains(span: Span, offset: u32) -> bool {
    span.start <= offset && offset < span.end
}

/// End offset of the innermost lexical scope containing `offset`.
///
/// `None` when the offset is not inside any scope at all — a global
/// declaration, or a span that does not correspond to this document.
pub fn innermost_scope_end(file: &SourceFile, tokens: &[Token], offset: u32) -> Option<u32> {
    let mut candidates = Vec::new();
    for item in &file.items {
        collect_item_scopes(item, offset, &mut candidates);
    }
    collect_brace_scopes(tokens, offset, &mut candidates);
    candidates
        .into_iter()
        .min_by_key(|span| span.end - span.start)
        .map(|span| span.end)
}

fn collect_item_scopes(item: &Item, offset: u32, out: &mut Vec<Span>) {
    match item {
        Item::Function(func) => collect_function_scopes(func, offset, out),
        Item::Class(cls) => {
            for member in &cls.members {
                match member {
                    ClassMember::Method(f)
                    | ClassMember::Constructor(f)
                    | ClassMember::Destructor(f) => collect_function_scopes(f, offset, out),
                    ClassMember::Property(prop) => {
                        for body in prop
                            .getter
                            .iter()
                            .chain(prop.setter.iter().map(|(_, body)| body))
                        {
                            collect_body_scopes(body, offset, out);
                        }
                    }
                    ClassMember::Field(_) => {}
                }
            }
        }
        Item::Namespace(ns) => {
            if contains(ns.span, offset) {
                for nested in &ns.items {
                    collect_item_scopes(nested, offset, out);
                }
            }
        }
        Item::Property(prop) => {
            for body in prop
                .getter
                .iter()
                .chain(prop.setter.iter().map(|(_, body)| body))
            {
                collect_body_scopes(body, offset, out);
            }
        }
        _ => {}
    }
}

fn collect_function_scopes(func: &FunctionDecl, offset: u32, out: &mut Vec<Span>) {
    if let Some(body) = &func.body {
        collect_body_scopes(body, offset, out);
    }
}

fn collect_body_scopes(body: &FunctionBody, offset: u32, out: &mut Vec<Span>) {
    if !contains(body.span, offset) {
        return;
    }
    out.push(body.span);
    for stmt in &body.stmts {
        collect_stmt_scopes(stmt, offset, out);
    }
}

/// Push every scope span introduced at or below `stmt` that contains
/// `offset`. Statement scopes mirror the checker's frame pushes in
/// `Checker::check_stmt` — keeping the two in step is what makes the
/// renamed range agree with the frame the shadow warning came from.
fn collect_stmt_scopes(stmt: &Stmt, offset: u32, out: &mut Vec<Span>) {
    if !contains(stmt.span, offset) {
        return;
    }
    match &stmt.kind {
        StmtKind::Block(stmts) => {
            out.push(stmt.span);
            for nested in stmts {
                collect_stmt_scopes(nested, offset, out);
            }
        }
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            for branch in std::iter::once(then_branch).chain(else_branch.iter()) {
                if contains(branch.span, offset) {
                    out.push(branch.span);
                    collect_stmt_scopes(branch, offset, out);
                }
            }
        }
        StmtKind::For { init, body, .. } => {
            // The checker pushes ONE frame around init + condition + step +
            // body, so a `for`-init local is visible across the whole
            // statement — not just the body's braces.
            out.push(stmt.span);
            if let Some(init) = init {
                collect_stmt_scopes(init, offset, out);
            }
            collect_stmt_scopes(body, offset, out);
        }
        StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => {
            if contains(body.span, offset) {
                out.push(body.span);
                collect_stmt_scopes(body, offset, out);
            }
        }
        StmtKind::Switch { cases, .. } => {
            for case in cases {
                if contains(case.span, offset) {
                    out.push(case.span);
                    for nested in &case.stmts {
                        collect_stmt_scopes(nested, offset, out);
                    }
                }
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            for body in [try_body, catch_body] {
                if contains(body.span, offset) {
                    out.push(body.span);
                    collect_stmt_scopes(body, offset, out);
                }
            }
        }
        _ => {}
    }
}

/// Every `{ … }` region containing `offset`, from the token stream.
fn collect_brace_scopes(tokens: &[Token], offset: u32, out: &mut Vec<Span>) {
    let mut open: Vec<u32> = Vec::new();
    for tok in tokens {
        match tok.kind {
            TokenKind::LBrace => open.push(tok.span.start),
            TokenKind::RBrace => {
                if let Some(start) = open.pop() {
                    let span = Span {
                        start,
                        end: tok.span.end,
                    };
                    if contains(span, offset) {
                        out.push(span);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Spans of every reference to a local named `name` declared at
/// `decl_start` and living until `scope_end`, including the declarator
/// itself.
///
/// Skips identifiers that only *look* like the local: a member name after
/// `.` and a namespace-qualified name after `::` are different symbols.
/// `excluded` ranges (a nested redeclaration's own scope) are skipped
/// wholesale — inside them the name belongs to the deeper binding.
pub fn local_occurrences(
    tokens: &[Token],
    source: &str,
    name: &str,
    decl_start: u32,
    scope_end: u32,
    excluded: &[(u32, u32)],
) -> Vec<Span> {
    let mut out = Vec::new();
    for (idx, tok) in tokens.iter().enumerate() {
        if tok.kind != TokenKind::Ident
            || tok.span.start < decl_start
            || tok.span.end > scope_end
            || tok.span.text(source) != name
        {
            continue;
        }
        if idx > 0 && matches!(tokens[idx - 1].kind, TokenKind::Dot | TokenKind::ColonColon) {
            continue;
        }
        if excluded
            .iter()
            .any(|(start, end)| tok.span.start >= *start && tok.span.start < *end)
        {
            continue;
        }
        out.push(tok.span);
    }
    out
}

/// Every declarator/parameter named `name` inside `(after, until)`,
/// reported as its declarator-name offset. Used to find the *nested*
/// redeclarations whose scopes must be excluded from a rename.
pub fn redeclarations_in_range(
    file: &SourceFile,
    source: &str,
    name: &str,
    after: u32,
    until: u32,
) -> Vec<u32> {
    let mut out = Vec::new();
    for item in &file.items {
        collect_redecls_in_item(item, source, name, &mut out);
    }
    out.retain(|start| *start > after && *start < until);
    out.sort_unstable();
    out
}

fn collect_redecls_in_item(item: &Item, source: &str, name: &str, out: &mut Vec<u32>) {
    match item {
        Item::Function(func) => collect_redecls_in_function(func, source, name, out),
        Item::Class(cls) => {
            for member in &cls.members {
                match member {
                    ClassMember::Method(f)
                    | ClassMember::Constructor(f)
                    | ClassMember::Destructor(f) => {
                        collect_redecls_in_function(f, source, name, out)
                    }
                    ClassMember::Property(prop) => {
                        for body in prop
                            .getter
                            .iter()
                            .chain(prop.setter.iter().map(|(_, body)| body))
                        {
                            collect_redecls_in_stmts(&body.stmts, source, name, out);
                        }
                    }
                    ClassMember::Field(_) => {}
                }
            }
        }
        Item::Namespace(ns) => {
            for nested in &ns.items {
                collect_redecls_in_item(nested, source, name, out);
            }
        }
        Item::Property(prop) => {
            for body in prop
                .getter
                .iter()
                .chain(prop.setter.iter().map(|(_, body)| body))
            {
                collect_redecls_in_stmts(&body.stmts, source, name, out);
            }
        }
        _ => {}
    }
}

fn collect_redecls_in_function(func: &FunctionDecl, source: &str, name: &str, out: &mut Vec<u32>) {
    for param in &func.params {
        if let Some(ident) = &param.name {
            if ident.text(source) == name {
                out.push(ident.span.start);
            }
        }
    }
    if let Some(body) = &func.body {
        collect_redecls_in_stmts(&body.stmts, source, name, out);
    }
}

fn collect_redecls_in_stmts(stmts: &[Stmt], source: &str, name: &str, out: &mut Vec<u32>) {
    for stmt in stmts {
        collect_redecls_in_stmt(stmt, source, name, out);
    }
}

fn collect_redecls_in_stmt(stmt: &Stmt, source: &str, name: &str, out: &mut Vec<u32>) {
    match &stmt.kind {
        StmtKind::VarDecl(var) => collect_redecls_in_var(var, source, name, out),
        StmtKind::Block(stmts) => collect_redecls_in_stmts(stmts, source, name, out),
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            collect_redecls_in_stmt(then_branch, source, name, out);
            if let Some(branch) = else_branch {
                collect_redecls_in_stmt(branch, source, name, out);
            }
        }
        StmtKind::For { init, body, .. } => {
            if let Some(init) = init {
                collect_redecls_in_stmt(init, source, name, out);
            }
            collect_redecls_in_stmt(body, source, name, out);
        }
        StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } => {
            collect_redecls_in_stmt(body, source, name, out)
        }
        StmtKind::Switch { cases, .. } => {
            for case in cases {
                collect_redecls_in_stmts(&case.stmts, source, name, out);
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            collect_redecls_in_stmt(try_body, source, name, out);
            collect_redecls_in_stmt(catch_body, source, name, out);
        }
        _ => {}
    }
}

fn collect_redecls_in_var(var: &VarDeclStmt, source: &str, name: &str, out: &mut Vec<u32>) {
    for declarator in &var.declarators {
        if declarator.name.text(source) == name {
            out.push(declarator.name.span.start);
        }
    }
}

/// True when `text` lexes as exactly one identifier — i.e. it is a legal
/// new name and not a keyword. `int6` + `4` would otherwise generate the
/// keyword `int64`.
pub fn is_plain_identifier(text: &str) -> bool {
    let tokens: Vec<_> = lexer::tokenize_filtered(text)
        .into_iter()
        .filter(|tok| tok.kind != TokenKind::Eof)
        .collect();
    matches!(tokens.as_slice(), [tok] if tok.kind == TokenKind::Ident
        && tok.span.start == 0
        && tok.span.end as usize == text.len())
}

/// How many numeric suffixes to try before giving up. A file with `x2`
/// through `x99` already taken does not need a quick fix.
const MAX_SUFFIX: u32 = 99;

/// First `base<n>` (n from 2) that `taken` rejects nothing about: not an
/// identifier anywhere in the file, not a workspace/external global, and a
/// legal identifier in its own right.
pub fn fresh_name(base: &str, taken: &dyn Fn(&str) -> bool) -> Option<String> {
    (2..=MAX_SUFFIX).find_map(|n| {
        let candidate = format!("{base}{n}");
        (is_plain_identifier(&candidate) && !taken(&candidate)).then_some(candidate)
    })
}

/// True when `name` occurs as an identifier token anywhere in `tokens`.
///
/// Deliberately blunt: it counts member names and unrelated functions too.
/// Over-rejecting a candidate costs one more suffix; under-rejecting one
/// produces a rename that silently captures another symbol.
pub fn name_occurs_in_file(tokens: &[Token], source: &str, name: &str) -> bool {
    tokens
        .iter()
        .any(|tok| tok.kind == TokenKind::Ident && tok.span.text(source) == name)
}

/// Index of the `)` closing the parameter list that opens at or after
/// `from` — the insertion point for a trailing `const` on a method.
pub fn params_close_paren(tokens: &[Token], from: u32) -> Option<Span> {
    let open = tokens
        .iter()
        .position(|tok| tok.span.start >= from && tok.kind == TokenKind::LParen)?;
    let mut depth = 0usize;
    for tok in &tokens[open..] {
        match tok.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(tok.span);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::DocumentAnalysis;

    fn scope_end_of(source: &str, needle: &str) -> Option<u32> {
        let analysis = DocumentAnalysis::analyze_plain(source);
        let offset = source.find(needle).expect("needle in source") as u32;
        innermost_scope_end(&analysis.file, &analysis.tokens, offset)
    }

    #[test]
    fn block_scope_ends_at_its_closing_brace() {
        let src = "void f() { int a; { int b; } int c; }";
        let end = scope_end_of(src, "b").expect("scope");
        assert_eq!(&src[..end as usize], "void f() { int a; { int b; }");
    }

    #[test]
    fn for_init_scope_covers_the_whole_for_statement() {
        // The checker frames init+cond+step+body together; a brace-only
        // model would wrongly extend `i`'s scope to the function's `}`.
        let src = "void f() { for (int i = 0; i < 2; i++) { } int after; }";
        let end = scope_end_of(src, "i =").expect("scope");
        let text = &src[..end as usize];
        assert!(text.ends_with("{ }"), "got `{}`", text);
        assert!(!text.contains("after"));
    }

    #[test]
    fn lambda_body_is_found_through_the_brace_fallback() {
        // Lambdas live inside expressions, which the statement walk does
        // not descend into — the brace scan is what covers them.
        let src = "void f() { auto g = function() { int q; }; int after; }";
        let end = scope_end_of(src, "q").expect("scope");
        let text = &src[..end as usize];
        assert!(text.ends_with("{ int q; }"), "got `{}`", text);
    }

    #[test]
    fn switch_case_scope_stops_at_the_next_case() {
        let src = "void f(int v) { switch (v) { case 1: int a; break; case 2: int b; break; } }";
        let end = scope_end_of(src, "a;").expect("scope");
        let text = &src[..end as usize];
        assert!(!text.contains("case 2"), "got `{}`", text);
    }

    #[test]
    fn a_global_declaration_has_no_enclosing_scope() {
        assert_eq!(scope_end_of("int g = 1;", "g"), None);
    }

    #[test]
    fn keyword_candidates_are_rejected() {
        assert!(!is_plain_identifier("int64"));
        assert!(!is_plain_identifier("const"));
        assert!(!is_plain_identifier("2x"));
        assert!(!is_plain_identifier("a b"));
        assert!(is_plain_identifier("x2"));
        assert!(is_plain_identifier("_x2"));
    }

    #[test]
    fn fresh_name_skips_taken_and_keyword_candidates() {
        let taken = |n: &str| n == "x2" || n == "x3";
        assert_eq!(fresh_name("x", &taken).as_deref(), Some("x4"));
        // `int6` + `4` would be the keyword `int64`; the next suffix wins.
        let free = |_: &str| false;
        assert_eq!(fresh_name("int6", &free).as_deref(), Some("int62"));
    }

    #[test]
    fn params_close_paren_matches_nested_parens_in_defaults() {
        let src = "void f(int a = g(1, h(2)), int b) {}";
        let analysis = DocumentAnalysis::analyze_plain(src);
        let from = src.find('(').unwrap() as u32;
        let span = params_close_paren(&analysis.tokens, from).expect("close paren");
        assert_eq!(span.start as usize, src.find(") {").unwrap());
    }
}
