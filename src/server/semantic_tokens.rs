//! LSP `textDocument/semanticTokens/full` provider.
//!
//! Walks the AST to classify identifiers (class/function/enum/parameter/…)
//! and then iterates the raw token stream to emit an LSP semantic-tokens
//! payload. Keywords, literals, comments, and operators are classified
//! directly from the token kind; identifiers default to `variable` when
//! not seen by the AST walker.

use std::collections::HashMap;

use tower_lsp::lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensLegend,
};

use crate::lexer::{Span, Token, TokenKind};
use crate::parser::ast::*;
use crate::server::diagnostics::offset_to_position;

// ── Legend ───────────────────────────────────────────────────────────────────

pub const TT_KEYWORD: u32 = 0;
pub const TT_TYPE: u32 = 1;
pub const TT_CLASS: u32 = 2;
pub const TT_INTERFACE: u32 = 3;
pub const TT_ENUM: u32 = 4;
pub const TT_ENUM_MEMBER: u32 = 5;
pub const TT_FUNCTION: u32 = 6;
pub const TT_METHOD: u32 = 7;
pub const TT_VARIABLE: u32 = 8;
pub const TT_PARAMETER: u32 = 9;
pub const TT_PROPERTY: u32 = 10;
pub const TT_NAMESPACE: u32 = 11;
pub const TT_STRING: u32 = 12;
pub const TT_NUMBER: u32 = 13;
pub const TT_COMMENT: u32 = 14;
pub const TT_OPERATOR: u32 = 15;
pub const TT_MACRO: u32 = 16;

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::TYPE,
            SemanticTokenType::CLASS,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::ENUM,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::OPERATOR,
            SemanticTokenType::MACRO,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::STATIC,
        ],
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Build the semantic-tokens payload for `source`.
pub fn semantic_tokens(source: &str) -> SemanticTokens {
    // Raw tokens (with trivia) so we can emit comments and operators too.
    let raw_tokens = crate::lexer::tokenize(source);

    // Parse to get AST-based identifier classifications.
    let filtered = crate::lexer::tokenize_filtered(source);
    let mut parser = crate::parser::Parser::new(&filtered, source);
    let file = parser.parse_file();

    let mut classifier = IdentClassifier::default();
    classifier.classify_file(&file);

    let mut emitter = TokenEmitter::new(source);
    for tok in &raw_tokens {
        if let Some((tt, mods)) = classify_token(tok, &classifier) {
            emitter.emit(tok.span, tt, mods);
        }
    }

    SemanticTokens {
        result_id: None,
        data: emitter.into_data(),
    }
}

// ── Identifier classifier (AST walker) ──────────────────────────────────────

#[derive(Default)]
pub struct IdentClassifier {
    /// Map from identifier-token start offset → (token-type index, modifiers).
    pub by_offset: HashMap<u32, (u32, u32)>,
}

impl IdentClassifier {
    fn mark(&mut self, span: Span, tt: u32, mods: u32) {
        self.by_offset.insert(span.start, (tt, mods));
    }

    pub fn classify_file(&mut self, file: &SourceFile) {
        for item in &file.items {
            self.classify_item(item);
        }
    }

    fn classify_item(&mut self, item: &Item) {
        match item {
            Item::Class(c) => {
                self.mark(c.name.span, TT_CLASS, 0);
                for member in &c.members {
                    match member {
                        ClassMember::Field(v) => {
                            for d in &v.declarators {
                                self.mark(d.name.span, TT_PROPERTY, 0);
                                if let Some(e) = &d.init {
                                    self.classify_expr(e);
                                }
                            }
                        }
                        ClassMember::Method(f)
                        | ClassMember::Constructor(f)
                        | ClassMember::Destructor(f) => {
                            self.mark(f.name.span, TT_METHOD, 0);
                            self.classify_fn_inner(f);
                        }
                        ClassMember::Property(p) => {
                            self.mark(p.name.span, TT_PROPERTY, 0);
                            if let Some(body) = &p.getter {
                                self.classify_body(body);
                            }
                            if let Some((_, body)) = &p.setter {
                                self.classify_body(body);
                            }
                        }
                    }
                }
            }
            Item::Interface(i) => {
                self.mark(i.name.span, TT_INTERFACE, 0);
                for m in &i.methods {
                    self.mark(m.name.span, TT_METHOD, 0);
                    for p in &m.params {
                        if let Some(n) = &p.name {
                            self.mark(n.span, TT_PARAMETER, 0);
                        }
                    }
                }
            }
            Item::Enum(e) => {
                self.mark(e.name.span, TT_ENUM, 0);
                for v in &e.values {
                    self.mark(v.name.span, TT_ENUM_MEMBER, 0);
                    if let Some(init) = &v.value {
                        self.classify_expr(init);
                    }
                }
            }
            Item::Namespace(n) => {
                self.mark(n.name.span, TT_NAMESPACE, 0);
                for inner in &n.items {
                    self.classify_item(inner);
                }
            }
            Item::Funcdef(f) => {
                self.mark(f.name.span, TT_FUNCTION, 0);
                for p in &f.params {
                    if let Some(n) = &p.name {
                        self.mark(n.span, TT_PARAMETER, 0);
                    }
                }
            }
            Item::Function(f) => {
                self.mark(f.name.span, TT_FUNCTION, 0);
                self.classify_fn_inner(f);
            }
            Item::VarDecl(v) => {
                for d in &v.declarators {
                    self.mark(d.name.span, TT_VARIABLE, 0);
                    if let Some(e) = &d.init {
                        self.classify_expr(e);
                    }
                }
            }
            Item::Property(p) => {
                self.mark(p.name.span, TT_PROPERTY, 0);
                if let Some(body) = &p.getter {
                    self.classify_body(body);
                }
                if let Some((_, body)) = &p.setter {
                    self.classify_body(body);
                }
            }
            Item::Import(imp) => {
                if let ImportTarget::Function { name, params, .. } = &imp.what {
                    self.mark(name.span, TT_FUNCTION, 0);
                    for p in params {
                        if let Some(n) = &p.name {
                            self.mark(n.span, TT_PARAMETER, 0);
                        }
                    }
                } else if let ImportTarget::Module { alias: Some(a), .. } = &imp.what {
                    self.mark(a.span, TT_NAMESPACE, 0);
                }
            }
            Item::Error(_) => {}
        }
    }

    fn classify_fn_inner(&mut self, f: &FunctionDecl) {
        for p in &f.params {
            if let Some(n) = &p.name {
                self.mark(n.span, TT_PARAMETER, 0);
            }
            if let Some(def) = &p.default_value {
                self.classify_expr(def);
            }
        }
        if let Some(body) = &f.body {
            self.classify_body(body);
        }
    }

    fn classify_body(&mut self, body: &FunctionBody) {
        for s in &body.stmts {
            self.classify_stmt(s);
        }
    }

    fn classify_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(e) => self.classify_expr(e),
            StmtKind::VarDecl(v) => {
                for d in &v.declarators {
                    self.mark(d.name.span, TT_VARIABLE, 0);
                    if let Some(e) = &d.init {
                        self.classify_expr(e);
                    }
                }
            }
            StmtKind::Block(stmts) => {
                for s in stmts {
                    self.classify_stmt(s);
                }
            }
            StmtKind::If { condition, then_branch, else_branch } => {
                self.classify_expr(condition);
                self.classify_stmt(then_branch);
                if let Some(e) = else_branch {
                    self.classify_stmt(e);
                }
            }
            StmtKind::For { init, condition, step, body } => {
                if let Some(i) = init {
                    self.classify_stmt(i);
                }
                if let Some(c) = condition {
                    self.classify_expr(c);
                }
                for s in step {
                    self.classify_expr(s);
                }
                self.classify_stmt(body);
            }
            StmtKind::While { condition, body } => {
                self.classify_expr(condition);
                self.classify_stmt(body);
            }
            StmtKind::DoWhile { body, condition } => {
                self.classify_stmt(body);
                self.classify_expr(condition);
            }
            StmtKind::Switch { expr, cases } => {
                self.classify_expr(expr);
                for case in cases {
                    if let SwitchLabel::Case(e) = &case.label {
                        self.classify_expr(e);
                    }
                    for s in &case.stmts {
                        self.classify_stmt(s);
                    }
                }
            }
            StmtKind::Return(Some(e)) => self.classify_expr(e),
            StmtKind::TryCatch { try_body, catch_body } => {
                self.classify_stmt(try_body);
                self.classify_stmt(catch_body);
            }
            StmtKind::Return(None)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Empty
            | StmtKind::Error => {}
        }
    }

    fn classify_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Binary { lhs, rhs, .. } => {
                self.classify_expr(lhs);
                self.classify_expr(rhs);
            }
            ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => {
                self.classify_expr(expr);
            }
            ExprKind::Call { callee, args } => {
                self.classify_expr(callee);
                for a in args {
                    self.classify_expr(a);
                }
            }
            ExprKind::Member { object, .. } => {
                self.classify_expr(object);
                // Leave the member name as default (variable) — we don't
                // have enough context to know whether it's a method or a
                // field. A later iteration with type resolution can refine.
            }
            ExprKind::Index { object, index } => {
                self.classify_expr(object);
                self.classify_expr(index);
            }
            ExprKind::Cast { expr, .. } => self.classify_expr(expr),
            ExprKind::TypeConstruct { args, .. } => {
                for a in args {
                    self.classify_expr(a);
                }
            }
            ExprKind::Is { expr, target, .. } => {
                self.classify_expr(expr);
                if let IsTarget::Expr(e) = target {
                    self.classify_expr(e);
                }
            }
            ExprKind::Ternary { condition, then_expr, else_expr } => {
                self.classify_expr(condition);
                self.classify_expr(then_expr);
                self.classify_expr(else_expr);
            }
            ExprKind::Assign { lhs, rhs, .. }
            | ExprKind::HandleAssign { lhs, rhs } => {
                self.classify_expr(lhs);
                self.classify_expr(rhs);
            }
            ExprKind::ArrayInit(items) => {
                for i in items {
                    self.classify_expr(i);
                }
            }
            ExprKind::Lambda { params, body } => {
                for p in params {
                    if let Some(n) = &p.name {
                        self.mark(n.span, TT_PARAMETER, 0);
                    }
                }
                self.classify_body(body);
            }
            // Leaves / things we don't refine here.
            ExprKind::Ident(_)
            | ExprKind::NamespaceAccess { .. }
            | ExprKind::IntLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::StringLit
            | ExprKind::HexLit(_)
            | ExprKind::BoolLit(_)
            | ExprKind::Null
            | ExprKind::This
            | ExprKind::Super
            | ExprKind::Error => {}
        }
    }
}

// ── Per-token classification ────────────────────────────────────────────────

fn classify_token(tok: &Token, classifier: &IdentClassifier) -> Option<(u32, u32)> {
    use TokenKind::*;
    match tok.kind {
        LineComment | BlockComment => Some((TT_COMMENT, 0)),
        StringLit => Some((TT_STRING, 0)),
        IntLit | FloatLit | HexLit => Some((TT_NUMBER, 0)),
        Ident => Some(classifier
            .by_offset
            .get(&tok.span.start)
            .copied()
            .unwrap_or((TT_VARIABLE, 0))),
        Eof | Error => None,
        // Punctuation that we don't want to color at all.
        LParen | RParen | LBrace | RBrace | LBracket | RBracket | Semi | Comma
        | Dot | ColonColon | Colon => None,
        // Operators.
        Plus | Minus | Star | StarStar | Slash | Percent | Eq | EqEq | BangEq
        | Lt | Gt | LtEq | GtEq | AmpAmp | PipePipe | Bang | Amp | Pipe
        | Caret | CaretCaret | Tilde | LtLt | GtGt | PlusEq | MinusEq | StarEq
        | SlashEq | PercentEq | AmpEq | PipeEq | CaretEq | LtLtEq | GtGtEq
        | PlusPlus | MinusMinus | Question => Some((TT_OPERATOR, 0)),
        At | Hash => Some((TT_OPERATOR, 0)),
        // Everything else is a keyword.
        _ => Some((TT_KEYWORD, 0)),
    }
}

// ── Delta encoder ───────────────────────────────────────────────────────────

struct TokenEmitter<'a> {
    source: &'a str,
    data: Vec<SemanticToken>,
    prev_line: u32,
    prev_start: u32,
}

impl<'a> TokenEmitter<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            data: Vec::new(),
            prev_line: 0,
            prev_start: 0,
        }
    }

    fn emit(&mut self, span: Span, tt: u32, mods: u32) {
        if span.end <= span.start {
            return;
        }
        let text = &self.source[span.start as usize..span.end as usize];
        // YAGNI: skip multi-line tokens (multi-line string literals, block
        // comments that span lines). Single-line tokens cover the vast
        // majority of interesting cases.
        if text.contains('\n') {
            return;
        }
        let pos = offset_to_position(self.source, span.start as usize);
        let length = span.end - span.start;
        let (delta_line, delta_start) = if pos.line == self.prev_line {
            (0, pos.character - self.prev_start)
        } else {
            (pos.line - self.prev_line, pos.character)
        };
        self.data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: tt,
            token_modifiers_bitset: mods,
        });
        self.prev_line = pos.line;
        self.prev_start = pos.character;
    }

    /// Convert emitted `SemanticToken` records into the flat `data` vector
    /// that LSP expects. We use the typed vector for readability but LSP
    /// serializes `SemanticTokens { data: Vec<SemanticToken> }` directly.
    fn into_data(self) -> Vec<SemanticToken> {
        self.data
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a `SemanticTokens` into the raw u32 stream, mirroring the
    /// on-wire encoding the task's tests expect.
    fn flat(st: &SemanticTokens) -> Vec<u32> {
        let mut out = Vec::new();
        for t in &st.data {
            out.push(t.delta_line);
            out.push(t.delta_start);
            out.push(t.length);
            out.push(t.token_type);
            out.push(t.token_modifiers_bitset);
        }
        out
    }

    #[test]
    fn keyword_classified() {
        let st = semantic_tokens("void f() {}");
        let data = flat(&st);
        assert!(data.len() >= 5);
        assert_eq!(data[3], TT_KEYWORD);
    }

    #[test]
    fn function_name_classified() {
        let st = semantic_tokens("void greet() {}");
        let data = flat(&st);
        assert!(data.len() >= 10);
        assert_eq!(data[3], TT_KEYWORD); // void
        assert_eq!(data[8], TT_FUNCTION); // greet
    }

    #[test]
    fn class_name_classified() {
        let st = semantic_tokens("class Foo { }");
        let data = flat(&st);
        assert_eq!(data[3], TT_KEYWORD);
        assert_eq!(data[8], TT_CLASS);
    }

    #[test]
    fn string_literal_classified() {
        let st = semantic_tokens(r#"string s = "hello";"#);
        let data = flat(&st);
        let mut found_string = false;
        for chunk in data.chunks(5) {
            if chunk[3] == TT_STRING {
                found_string = true;
                break;
            }
        }
        assert!(found_string);
    }

    #[test]
    fn enum_member_classified() {
        let st = semantic_tokens("enum E { A, B }");
        let data = flat(&st);
        let types: Vec<u32> = data.chunks(5).map(|c| c[3]).collect();
        assert!(types.contains(&TT_ENUM));
        assert!(types.contains(&TT_ENUM_MEMBER));
    }

    #[test]
    fn number_literal_classified() {
        let st = semantic_tokens("int x = 42;");
        let data = flat(&st);
        let types: Vec<u32> = data.chunks(5).map(|c| c[3]).collect();
        assert!(types.contains(&TT_NUMBER));
    }

    #[test]
    fn comment_classified() {
        let st = semantic_tokens("// hello\nint x = 1;");
        let data = flat(&st);
        let types: Vec<u32> = data.chunks(5).map(|c| c[3]).collect();
        assert!(types.contains(&TT_COMMENT));
    }

    #[test]
    fn parameter_classified() {
        let st = semantic_tokens("void f(int a) {}");
        let data = flat(&st);
        let types: Vec<u32> = data.chunks(5).map(|c| c[3]).collect();
        assert!(types.contains(&TT_PARAMETER));
    }

    #[test]
    fn operator_classified() {
        let st = semantic_tokens("int x = 1 + 2;");
        let data = flat(&st);
        let types: Vec<u32> = data.chunks(5).map(|c| c[3]).collect();
        assert!(types.contains(&TT_OPERATOR));
    }

    #[test]
    fn namespace_classified() {
        let st = semantic_tokens("namespace Foo { void bar() {} }");
        let data = flat(&st);
        let types: Vec<u32> = data.chunks(5).map(|c| c[3]).collect();
        assert!(types.contains(&TT_NAMESPACE));
        assert!(types.contains(&TT_FUNCTION));
    }

    #[test]
    fn delta_encoding_monotonic() {
        // Ensure the encoder produces valid delta rows (delta_line
        // non-negative, and delta_start non-negative when delta_line == 0).
        let st = semantic_tokens("int a = 1;\nint b = 2;");
        for t in &st.data {
            if t.delta_line == 0 {
                // delta_start is u32 so non-negative by construction; the
                // important correctness property is that prev_start was
                // reset on newlines — which we verify implicitly by not
                // panicking on the subtraction in emit().
                let _ = t.delta_start;
            }
        }
    }
}
