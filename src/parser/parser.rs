use crate::lexer::{tokenize_filtered, Span, Token, TokenKind};
use crate::parser::ast::*;
use crate::parser::error::{ParseError, ParseErrorKind};

// ── Parser struct ───────────────────────────────────────────────────────────

pub struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    pos: usize,
    /// When true, the current `GtGt` token is being treated as two separate
    /// `Gt` tokens; the first has already been consumed (used to close the
    /// inner of nested templates like `array<array<int>>`). The next call to
    /// `advance` clears the flag and advances past the original `GtGt`.
    split_gtgt: bool,
    pub errors: Vec<ParseError>,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token], source: &'a str) -> Self {
        Parser {
            tokens,
            source,
            pos: 0,
            split_gtgt: false,
            errors: Vec::new(),
        }
    }

    // ── Core utility methods ────────────────────────────────────────────

    /// Peek at the current token kind without advancing.
    fn peek(&self) -> TokenKind {
        if self.split_gtgt {
            return TokenKind::Gt;
        }
        self.tokens
            .get(self.pos)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    /// Peek ahead by `n` tokens (0 = current).
    fn peek_ahead(&self, n: usize) -> TokenKind {
        if self.split_gtgt {
            if n == 0 {
                return TokenKind::Gt;
            }
            return self
                .tokens
                .get(self.pos + n)
                .map(|t| t.kind)
                .unwrap_or(TokenKind::Eof);
        }
        self.tokens
            .get(self.pos + n)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    /// Get the span of the current token.
    fn current_span(&self) -> Span {
        if self.split_gtgt {
            // Synthetic span for the second half of a GtGt
            if let Some(t) = self.tokens.get(self.pos) {
                let mid = t.span.start + 1;
                return Span::new(mid, t.span.end);
            }
        }
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or_else(|| {
                let end = self.source.len() as u32;
                Span::new(end, end)
            })
    }

    /// Check if the current token is of the given kind.
    fn at(&self, kind: TokenKind) -> bool {
        self.peek() == kind
    }

    /// Check if we've reached the end of input.
    fn at_end(&self) -> bool {
        self.peek() == TokenKind::Eof
    }

    /// Advance the parser and return the current token (cloned).
    fn advance(&mut self) -> Token {
        if self.split_gtgt {
            // Consume the second half of a GtGt: clear the flag and advance
            // past the original token. Synthesize a Gt token at the second
            // half's span.
            self.split_gtgt = false;
            let real = self.tokens[self.pos].clone();
            let mid = real.span.start + 1;
            let synthetic = Token {
                kind: TokenKind::Gt,
                span: Span::new(mid, real.span.end),
            };
            if self.pos < self.tokens.len() - 1 {
                self.pos += 1;
            }
            return synthetic;
        }
        let tok = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        tok
    }

    /// Consume one closing `>` of a template-arg list. Splits a `GtGt`
    /// token into two `Gt`s when needed (the second half stays in place
    /// for the enclosing template parser to consume).
    fn consume_template_close(&mut self) -> Result<(), ParseError> {
        if self.split_gtgt {
            // Already split; consume the second half normally.
            self.advance();
            return Ok(());
        }
        match self.peek() {
            TokenKind::Gt => {
                self.advance();
                Ok(())
            }
            TokenKind::GtGt => {
                // Mark the GtGt as half-consumed.
                self.split_gtgt = true;
                Ok(())
            }
            other => Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::Expected {
                    expected: TokenKind::Gt,
                    found: other,
                },
            }),
        }
    }

    /// Expect a specific token kind; advance if matching, error otherwise.
    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        if self.at(kind) {
            Ok(self.advance())
        } else {
            let err = ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::Expected {
                    expected: kind,
                    found: self.peek(),
                },
            };
            Err(err)
        }
    }

    /// Expect an identifier token, returning an Ident AST node.
    fn expect_ident(&mut self) -> Result<Ident, ParseError> {
        if self.at(TokenKind::Ident) {
            let tok = self.advance();
            Ok(Ident { span: tok.span })
        } else {
            let err = ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::ExpectedIdent {
                    found: self.peek(),
                },
            };
            Err(err)
        }
    }

    /// If the current token matches `kind`, advance and return true; otherwise false.
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Build a span from `start` to the end of the previous token.
    fn span_from(&self, start: u32) -> Span {
        let end = if self.pos > 0 {
            self.tokens[self.pos - 1].span.end
        } else {
            start
        };
        Span::new(start, end)
    }

    /// Push an error into the errors vector.
    fn error(&mut self, err: ParseError) {
        self.errors.push(err);
    }

    // ── Type expression parsing ─────────────────────────────────────────

    /// Parse a type expression: optional `const`, base type, then suffixes (@, &, []).
    pub fn parse_type_expr(&mut self) -> Result<TypeExpr, ParseError> {
        let start = self.current_span().start;

        // Leading const. We defer wrapping until we know whether a `@`
        // suffix follows: `const Foo@` is a handle-to-const (pointee is
        // const) → `Handle(Const(Foo))`, whereas `const Foo` (no handle)
        // is simply `Const(Foo)`.
        let mut is_const = self.eat(TokenKind::KwConst);

        let mut ty = self.parse_base_type()?;

        // Parse suffixes: @, &[in/out/inout], []
        loop {
            match self.peek() {
                TokenKind::At => {
                    self.advance();
                    // If a leading `const` was seen, it binds to the pointee:
                    // `const Foo@` → `Handle(Const(Foo))`.
                    if is_const {
                        let span = self.span_from(ty.span.start);
                        ty = TypeExpr {
                            span,
                            kind: TypeExprKind::Const(Box::new(ty)),
                        };
                        is_const = false;
                    }
                    // Optional trailing `const` after `@`: `Foo@ const` makes
                    // the handle itself const (pointee still mutable) →
                    // `Const(Handle(Foo))`.
                    let trailing_const = self.eat(TokenKind::KwConst);
                    let span = self.span_from(ty.span.start);
                    ty = TypeExpr {
                        span,
                        kind: TypeExprKind::Handle(Box::new(ty)),
                    };
                    if trailing_const {
                        let span = self.span_from(ty.span.start);
                        ty = TypeExpr {
                            span,
                            kind: TypeExprKind::Const(Box::new(ty)),
                        };
                    }
                }
                TokenKind::Amp => {
                    self.advance();
                    let modifier = self.parse_param_modifier();
                    let span = self.span_from(ty.span.start);
                    ty = TypeExpr {
                        span,
                        kind: TypeExprKind::Reference(Box::new(ty), modifier),
                    };
                }
                TokenKind::LBracket if self.peek_ahead(1) == TokenKind::RBracket => {
                    self.advance(); // [
                    self.advance(); // ]
                    let span = self.span_from(ty.span.start);
                    ty = TypeExpr {
                        span,
                        kind: TypeExprKind::Array(Box::new(ty)),
                    };
                }
                _ => break,
            }
        }

        if is_const {
            let span = self.span_from(start);
            ty = TypeExpr {
                span,
                kind: TypeExprKind::Const(Box::new(ty)),
            };
        }

        Ok(ty)
    }

    /// Parse a base type: primitive keywords, auto, array<T>, dictionary, named types.
    fn parse_base_type(&mut self) -> Result<TypeExpr, ParseError> {
        let start = self.current_span().start;

        match self.peek() {
            // Primitive type keywords
            kind if is_primitive_keyword(kind) => {
                let tok = self.advance();
                Ok(TypeExpr {
                    span: tok.span,
                    kind: TypeExprKind::Primitive(tok.kind),
                })
            }
            TokenKind::KwAuto => {
                let tok = self.advance();
                Ok(TypeExpr {
                    span: tok.span,
                    kind: TypeExprKind::Auto,
                })
            }
            TokenKind::KwArray => {
                let tok = self.advance();
                if self.eat(TokenKind::Lt) {
                    let inner = self.parse_type_expr()?;
                    self.consume_template_close()?;
                    let span = self.span_from(start);
                    Ok(TypeExpr {
                        span,
                        kind: TypeExprKind::Array(Box::new(inner)),
                    })
                } else {
                    // Just 'array' without <T>, treat as named
                    let name = QualifiedName::simple(Ident { span: tok.span });
                    Ok(TypeExpr {
                        span: tok.span,
                        kind: TypeExprKind::Named(name),
                    })
                }
            }
            TokenKind::KwDictionary => {
                let tok = self.advance();
                let name = QualifiedName::simple(Ident { span: tok.span });
                Ok(TypeExpr {
                    span: tok.span,
                    kind: TypeExprKind::Named(name),
                })
            }
            TokenKind::Ident => {
                let qname = self.parse_qualified_name()?;
                // Check for template args: name<T, U, ...>
                if self.at(TokenKind::Lt) && self.looks_like_type_args() {
                    self.advance(); // eat <
                    let mut args = Vec::new();
                    if !self.at(TokenKind::Gt) {
                        args.push(self.parse_type_expr()?);
                        while self.eat(TokenKind::Comma) {
                            args.push(self.parse_type_expr()?);
                        }
                    }
                    self.consume_template_close()?;
                    let span = self.span_from(start);
                    Ok(TypeExpr {
                        span,
                        kind: TypeExprKind::Template(qname, args),
                    })
                } else {
                    let span = qname.span;
                    Ok(TypeExpr {
                        span,
                        kind: TypeExprKind::Named(qname),
                    })
                }
            }
            other => Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::ExpectedType { found: other },
            }),
        }
    }

    /// Parse a qualified name: Ident (:: Ident)*
    fn parse_qualified_name(&mut self) -> Result<QualifiedName, ParseError> {
        let first = self.expect_ident()?;
        let start = first.span.start;
        let mut segments = vec![first];

        while self.at(TokenKind::ColonColon) {
            // Only consume :: if followed by an Ident
            if self.peek_ahead(1) == TokenKind::Ident {
                self.advance(); // eat ::
                let seg = self.expect_ident()?;
                segments.push(seg);
            } else {
                break;
            }
        }

        let span = self.span_from(start);
        Ok(QualifiedName { span, segments })
    }

    /// Parse a parameter modifier (in/out/inout) if present.
    fn parse_param_modifier(&mut self) -> ParamModifier {
        match self.peek() {
            TokenKind::KwIn => {
                self.advance();
                ParamModifier::In
            }
            TokenKind::KwOut => {
                self.advance();
                ParamModifier::Out
            }
            TokenKind::KwInout => {
                self.advance();
                ParamModifier::Inout
            }
            _ => ParamModifier::None,
        }
    }

    /// Heuristic to decide if `<` starts type arguments or is a comparison.
    /// Scans forward from the `<`, tracking nesting, looking for a matching `>`.
    fn looks_like_type_args(&self) -> bool {
        let mut depth = 1i32;
        let mut i = 1; // start after the <
        loop {
            let kind = self.peek_ahead(i);
            match kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                TokenKind::GtGt => {
                    depth -= 2;
                    if depth <= 0 {
                        return true;
                    }
                }
                // Tokens that are valid inside type arguments
                TokenKind::Ident
                | TokenKind::ColonColon
                | TokenKind::Comma
                | TokenKind::At
                | TokenKind::Amp
                | TokenKind::KwConst
                | TokenKind::KwIn
                | TokenKind::KwOut
                | TokenKind::KwInout
                | TokenKind::KwArray
                | TokenKind::KwDictionary
                | TokenKind::LBracket
                | TokenKind::RBracket => {}
                // Primitive type keywords are valid inside type args
                k if is_primitive_keyword(k) => {}
                // Anything else means this is not type args
                _ => return false,
            }
            i += 1;
        }
    }

    // ── Declaration parsing ─────────────────────────────────────────────

    /// Parse a complete file: items until EOF.
    pub fn parse_file(&mut self) -> SourceFile {
        let mut items = Vec::new();
        while !self.at_end() {
            // Skip stray semicolons at file scope (e.g. trailing `;` after
            // a declaration block, or a lone `;` at end of file).
            if self.eat(TokenKind::Semi) {
                continue;
            }
            let pos_before = self.pos;
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(err) => {
                    let span = err.span;
                    self.error(err);
                    items.push(Item::Error(span));
                    self.synchronize();
                }
            }
            // Guarantee forward progress: if nothing was consumed this
            // iteration, force-advance to prevent infinite error loops.
            if self.pos == pos_before {
                self.advance();
            }
        }
        SourceFile { items }
    }

    /// Parse a single top-level item.
    fn parse_item(&mut self) -> Result<Item, ParseError> {
        // Skip stray semicolons (e.g. `class X {}; class Y {}` has an empty
        // top-level statement after the first class).
        while self.eat(TokenKind::Semi) {}

        // Collect attributes
        let attrs = self.parse_attributes()?;
        // Tolerate stray `;` after an attribute block (some plugins use
        // `[Setting ...];` then put the actual declaration on the next line).
        while self.eat(TokenKind::Semi) {}

        // File-scope visibility: AngelScript allows `private` (and rarely
        // `protected`) on top-level functions/variables to limit them to the
        // current file/namespace. Currently parsed but not propagated to the
        // declaration; the parser just consumes it so the declaration parses.
        let _ = self.eat(TokenKind::KwPrivate);
        let _ = self.eat(TokenKind::KwProtected);

        // Check for modifiers: shared, mixin, abstract
        let is_shared = self.eat(TokenKind::KwShared);
        let is_mixin = self.eat(TokenKind::KwMixin);
        let is_abstract = self.eat(TokenKind::KwAbstract);

        match self.peek() {
            TokenKind::KwClass => {
                let mut decl = self.parse_class_decl(attrs)?;
                decl.is_shared = is_shared;
                decl.is_mixin = is_mixin;
                decl.is_abstract = is_abstract;
                Ok(Item::Class(decl))
            }
            TokenKind::KwInterface => {
                let decl = self.parse_interface_decl()?;
                Ok(Item::Interface(decl))
            }
            TokenKind::KwEnum => {
                let decl = self.parse_enum_decl()?;
                Ok(Item::Enum(decl))
            }
            TokenKind::KwNamespace => {
                let decl = self.parse_namespace_decl()?;
                Ok(Item::Namespace(decl))
            }
            TokenKind::KwFuncdef => {
                let decl = self.parse_funcdef_decl()?;
                Ok(Item::Funcdef(decl))
            }
            TokenKind::KwImport => {
                let decl = self.parse_import_decl()?;
                Ok(Item::Import(decl))
            }
            _ if self.looks_like_type_start() => self.parse_func_or_var_item(attrs),
            other => Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::ExpectedItem { found: other },
            }),
        }
    }

    /// Parse a class declaration: `class Name [: Base, Interface] { members }`
    fn parse_class_decl(&mut self, attrs: Vec<Attribute>) -> Result<ClassDecl, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwClass)?;
        let name = self.expect_ident()?;

        // Optional base class list
        let mut base_classes = Vec::new();
        if self.eat(TokenKind::Colon) {
            base_classes.push(self.parse_type_expr()?);
            while self.eat(TokenKind::Comma) {
                base_classes.push(self.parse_type_expr()?);
            }
        }

        self.expect(TokenKind::LBrace)?;

        // Parse class members
        let mut members = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            // Skip stray semicolons (e.g. `};` after a method body)
            if self.eat(TokenKind::Semi) {
                continue;
            }
            let pos_before = self.pos;
            match self.parse_class_member(&name) {
                Ok(member) => members.push(member),
                Err(err) => {
                    self.error(err);
                    self.synchronize();
                }
            }
            if self.pos == pos_before {
                self.advance();
            }
        }

        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);

        Ok(ClassDecl {
            span,
            attributes: attrs,
            is_shared: false,
            is_mixin: false,
            is_abstract: false,
            name,
            base_classes,
            members,
        })
    }

    /// Parse a single class member: field, method, constructor, or destructor.
    fn parse_class_member(&mut self, class_name: &Ident) -> Result<ClassMember, ParseError> {
        let attrs = self.parse_attributes()?;

        // Check visibility modifiers
        let is_private = self.eat(TokenKind::KwPrivate);
        let is_protected = if !is_private {
            self.eat(TokenKind::KwProtected)
        } else {
            false
        };

        // Check for destructor: ~ClassName()
        if self.at(TokenKind::Tilde) {
            let start = self.current_span().start;
            self.advance(); // eat ~
            let dtor_name = self.expect_ident()?;
            let params = self.parse_param_list()?;
            let body = if self.at(TokenKind::LBrace) {
                Some(self.parse_function_body()?)
            } else {
                self.expect(TokenKind::Semi)?;
                None
            };
            let span = self.span_from(start);
            return Ok(ClassMember::Destructor(FunctionDecl {
                span,
                attributes: attrs,
                return_type: TypeExpr {
                    span: Span::new(start, start),
                    kind: TypeExprKind::Primitive(TokenKind::KwVoid),
                },
                name: dtor_name,
                params,
                is_const: false,
                is_override: false,
                is_final: false,
                is_private,
                is_protected,
                body,
            }));
        }

        // Check for constructor: ClassName(params) { }
        // A constructor has the same name as the class, followed by (
        let class_name_text = class_name.text(self.source);
        if self.at(TokenKind::Ident)
            && self.current_span().text(self.source) == class_name_text
            && self.peek_ahead(1) == TokenKind::LParen
        {
            let start = self.current_span().start;
            let ctor_name = self.expect_ident()?;
            let params = self.parse_param_list()?;
            let body = if self.at(TokenKind::LBrace) {
                Some(self.parse_function_body()?)
            } else {
                self.expect(TokenKind::Semi)?;
                None
            };
            let span = self.span_from(start);
            return Ok(ClassMember::Constructor(FunctionDecl {
                span,
                attributes: attrs,
                return_type: TypeExpr {
                    span: Span::new(start, start),
                    kind: TypeExprKind::Primitive(TokenKind::KwVoid),
                },
                name: ctor_name,
                params,
                is_const: false,
                is_override: false,
                is_final: false,
                is_private,
                is_protected,
                body,
            }));
        }

        // Otherwise: parse type, then name, then determine if method, property, or field
        let type_start = self.current_span().start;
        let type_expr = self.parse_type_expr()?;
        let member_name = self.expect_ident()?;

        if self.at(TokenKind::LParen) {
            // Same disambiguation as at file scope.
            if self.looks_like_constructor_init() {
                let decl = self.parse_var_decl_rest(attrs, type_expr, member_name)?;
                return Ok(ClassMember::Field(decl));
            }
            // It's a method
            let decl =
                self.parse_function_rest(attrs, type_expr, member_name, is_private, is_protected)?;
            Ok(ClassMember::Method(decl))
        } else if self.at(TokenKind::LBrace) {
            // Property accessor block: `Type name { get { ... } [set { ... }] }`
            let prop = self.parse_property_accessor_block(type_start, type_expr, member_name)?;
            Ok(ClassMember::Property(prop))
        } else {
            // It's a field
            let decl = self.parse_var_decl_rest(attrs, type_expr, member_name)?;
            Ok(ClassMember::Field(decl))
        }
    }

    /// Parse an interface declaration: `interface Name [: Base] { methods }`
    fn parse_interface_decl(&mut self) -> Result<InterfaceDecl, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwInterface)?;
        let name = self.expect_ident()?;

        // Optional bases
        let mut bases = Vec::new();
        if self.eat(TokenKind::Colon) {
            bases.push(self.parse_type_expr()?);
            while self.eat(TokenKind::Comma) {
                bases.push(self.parse_type_expr()?);
            }
        }

        self.expect(TokenKind::LBrace)?;

        let mut methods = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let pos_before = self.pos;
            match self.parse_interface_method() {
                Ok(method) => methods.push(method),
                Err(err) => {
                    self.error(err);
                    self.synchronize();
                }
            }
            if self.pos == pos_before {
                self.advance();
            }
        }

        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);

        Ok(InterfaceDecl {
            span,
            name,
            bases,
            methods,
        })
    }

    /// Parse an interface method signature: `RetType Name(params);`
    fn parse_interface_method(&mut self) -> Result<FunctionDecl, ParseError> {
        let start = self.current_span().start;
        let return_type = self.parse_type_expr()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;

        // Optional modifiers
        let is_const = self.eat(TokenKind::KwConst);

        self.expect(TokenKind::Semi)?;
        let span = self.span_from(start);

        Ok(FunctionDecl {
            span,
            attributes: Vec::new(),
            return_type,
            name,
            params,
            is_const,
            is_override: false,
            is_final: false,
            is_private: false,
            is_protected: false,
            body: None,
        })
    }

    /// Parse an enum declaration: `enum Name { V1 [= expr], V2, ... }`
    fn parse_enum_decl(&mut self) -> Result<EnumDecl, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwEnum)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;

        let mut values = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let val_start = self.current_span().start;
            let val_name = self.expect_ident()?;
            let value = if self.eat(TokenKind::Eq) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            let val_span = self.span_from(val_start);
            values.push(EnumValue {
                span: val_span,
                name: val_name,
                value,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }

        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);

        Ok(EnumDecl { span, name, values })
    }

    /// Parse a namespace declaration: `namespace Name { items }`
    fn parse_namespace_decl(&mut self) -> Result<NamespaceDecl, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwNamespace)?;
        // Allow namespace names that collide with type/value keywords
        // (`namespace string { ... }` is real AS code that adds utility
        // functions to the `string` namespace).
        let name = if self.at(TokenKind::Ident) {
            self.expect_ident()?
        } else if matches!(
            self.peek(),
            TokenKind::KwString
                | TokenKind::KwInt
                | TokenKind::KwUint
                | TokenKind::KwFloat
                | TokenKind::KwBool
                | TokenKind::KwArray
                | TokenKind::KwDictionary
        ) {
            let tok = self.advance();
            Ident { span: tok.span }
        } else {
            return Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::ExpectedIdent {
                    found: self.peek(),
                },
            });
        };
        self.expect(TokenKind::LBrace)?;

        let mut items = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let pos_before = self.pos;
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(err) => {
                    let span = err.span;
                    self.error(err);
                    items.push(Item::Error(span));
                    self.synchronize();
                }
            }
            if self.pos == pos_before {
                self.advance();
            }
        }

        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);

        Ok(NamespaceDecl { span, name, items })
    }

    /// Parse a funcdef declaration: `funcdef RetType Name(params);`
    fn parse_funcdef_decl(&mut self) -> Result<FuncdefDecl, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwFuncdef)?;
        let return_type = self.parse_type_expr()?;
        let name = self.expect_ident()?;
        let params = self.parse_param_list()?;
        self.expect(TokenKind::Semi)?;
        let span = self.span_from(start);

        Ok(FuncdefDecl {
            span,
            return_type,
            name,
            params,
        })
    }

    /// Parse an import declaration. Two forms:
    /// 1. `import "path" [as Alias];`
    /// 2. `import RetType Name(params) from "module";`
    fn parse_import_decl(&mut self) -> Result<ImportDecl, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwImport)?;

        if self.at(TokenKind::StringLit) {
            // Form 1: import "path" [as Alias];
            let path_tok = self.advance();
            let path = StringLiteral {
                span: path_tok.span,
            };
            // Optional: as Alias (not a keyword, check for ident "as")
            let alias =
                if self.at(TokenKind::Ident) && self.current_span().text(self.source) == "as" {
                    self.advance(); // eat "as"
                    Some(self.expect_ident()?)
                } else {
                    None
                };
            self.expect(TokenKind::Semi)?;
            let span = self.span_from(start);
            Ok(ImportDecl {
                span,
                what: ImportTarget::Module { path, alias },
                from: None,
            })
        } else {
            // Form 2: import RetType Name(params) from "module";
            let return_type = self.parse_type_expr()?;
            let name = self.expect_ident()?;
            let params = self.parse_param_list()?;
            // `from` is a contextual keyword (lexed as Ident).
            if self.at(TokenKind::Ident) && self.current_span().text(self.source) == "from" {
                self.advance();
            } else {
                return Err(ParseError {
                    span: self.current_span(),
                    kind: ParseErrorKind::Expected {
                        expected: TokenKind::StringLit,
                        found: self.peek(),
                    },
                });
            }
            let module_tok = self.expect(TokenKind::StringLit)?;
            let from = StringLiteral {
                span: module_tok.span,
            };
            self.expect(TokenKind::Semi)?;
            let span = self.span_from(start);
            Ok(ImportDecl {
                span,
                what: ImportTarget::Function {
                    return_type,
                    name,
                    params,
                },
                from: Some(from),
            })
        }
    }

    /// Parse attributes: `[Name args...]` where args are space-separated flags or key=value pairs.
    /// Multiple attributes can appear in one bracket separated by commas: `[A, B]`
    /// Also supports paren-style: `[Name(flag, key=value)]`
    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::LBracket) {
            self.advance(); // eat [
            while !self.at(TokenKind::RBracket) && !self.at_end() {
                let attr = self.parse_attribute()?;
                attrs.push(attr);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket)?;
        }
        Ok(attrs)
    }

    /// Parse a single attribute: `Name [args...]`
    fn parse_attribute(&mut self) -> Result<Attribute, ParseError> {
        let attr_start = self.current_span().start;
        let name = self.expect_ident()?;
        let mut args = Vec::new();

        if self.eat(TokenKind::LParen) {
            // Paren-style args: (flag, key=value, ...)
            while !self.at(TokenKind::RParen) && !self.at_end() {
                let arg_start = self.current_span().start;
                let arg_name = self.expect_ident()?;
                if self.eat(TokenKind::Eq) {
                    let value = self.parse_attr_value()?;
                    let arg_span = self.span_from(arg_start);
                    args.push(AttributeArg {
                        span: arg_span,
                        kind: AttributeArgKind::KeyValue {
                            key: arg_name,
                            value,
                        },
                    });
                } else {
                    let arg_span = self.span_from(arg_start);
                    args.push(AttributeArg {
                        span: arg_span,
                        kind: AttributeArgKind::Flag(arg_name),
                    });
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen)?;
        } else {
            // Space-separated args: `Name flag key=value key2="str" ...`
            // Args continue while we see identifiers that aren't followed by `]` or `,`
            while self.at(TokenKind::Ident) {
                let arg_start = self.current_span().start;
                let arg_name = self.expect_ident()?;
                if self.eat(TokenKind::Eq) {
                    let value = self.parse_attr_value()?;
                    let arg_span = self.span_from(arg_start);
                    args.push(AttributeArg {
                        span: arg_span,
                        kind: AttributeArgKind::KeyValue {
                            key: arg_name,
                            value,
                        },
                    });
                } else {
                    let arg_span = self.span_from(arg_start);
                    args.push(AttributeArg {
                        span: arg_span,
                        kind: AttributeArgKind::Flag(arg_name),
                    });
                }
            }
        }

        let attr_span = self.span_from(attr_start);
        Ok(Attribute {
            span: attr_span,
            name,
            args,
        })
    }

    /// Parse an attribute value.
    fn parse_attr_value(&mut self) -> Result<AttrValue, ParseError> {
        // Optional leading sign for numeric values: `min=-60`, `max=+5`.
        let mut neg = false;
        if self.eat(TokenKind::Minus) {
            neg = true;
        } else {
            let _ = self.eat(TokenKind::Plus);
        }
        match self.peek() {
            TokenKind::StringLit => {
                let tok = self.advance();
                Ok(AttrValue::String(StringLiteral { span: tok.span }))
            }
            TokenKind::IntLit => {
                let tok = self.advance();
                let mut val: i64 = tok.span.text(self.source).parse().unwrap_or(0);
                if neg {
                    val = -val;
                }
                Ok(AttrValue::Int(val))
            }
            TokenKind::FloatLit => {
                let tok = self.advance();
                let mut val: f64 = tok.span.text(self.source).parse().unwrap_or(0.0);
                if neg {
                    val = -val;
                }
                Ok(AttrValue::Float(val))
            }
            TokenKind::Ident => {
                let tok = self.advance();
                Ok(AttrValue::Ident(Ident { span: tok.span }))
            }
            _ => Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::Custom("expected attribute value".into()),
            }),
        }
    }

    /// Skip tokens until we find a synchronization point (`;`, `}`, or a declaration keyword).
    fn synchronize(&mut self) {
        loop {
            match self.peek() {
                TokenKind::Semi => {
                    self.advance();
                    return;
                }
                TokenKind::RBrace => {
                    // Don't consume the }, let the caller handle it
                    return;
                }
                TokenKind::KwClass
                | TokenKind::KwInterface
                | TokenKind::KwEnum
                | TokenKind::KwNamespace
                | TokenKind::KwFuncdef
                | TokenKind::KwImport => return,
                TokenKind::Eof => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    // ── Functions, Variables, Parameters ─────────────────────────────────

    /// Parse a top-level function or variable declaration.
    /// We've already determined this looks like a type start.
    fn parse_func_or_var_item(&mut self, attrs: Vec<Attribute>) -> Result<Item, ParseError> {
        let type_start = self.current_span().start;
        let type_expr = self.parse_type_expr()?;
        let name = self.expect_ident()?;

        if self.at(TokenKind::LParen) {
            // Ambiguity: `Type name(...)` is either a function declaration
            // or a variable with constructor-style initializer.
            if self.looks_like_constructor_init() {
                let decl = self.parse_var_decl_rest(attrs, type_expr, name)?;
                return Ok(Item::VarDecl(decl));
            }
            // Function declaration
            let decl = self.parse_function_rest(attrs, type_expr, name, false, false)?;
            Ok(Item::Function(decl))
        } else if self.at(TokenKind::LBrace) {
            // Top-level property accessor: `[const] Type name { get { ... } }`
            let prop = self.parse_property_accessor_block(type_start, type_expr, name)?;
            Ok(Item::Property(prop))
        } else {
            // Variable declaration
            let decl = self.parse_var_decl_rest(attrs, type_expr, name)?;
            Ok(Item::VarDecl(decl))
        }
    }

    /// Parse a property accessor block following `Type name`. Caller has
    /// already consumed the type and name; current token is `{`.
    fn parse_property_accessor_block(
        &mut self,
        type_start: u32,
        type_expr: TypeExpr,
        name: Ident,
    ) -> Result<PropertyDecl, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut getter: Option<FunctionBody> = None;
        let mut setter: Option<(Ident, FunctionBody)> = None;
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let pos_before = self.pos;
            let _ = self.eat(TokenKind::KwConst);
            match self.peek() {
                TokenKind::KwGet => {
                    self.advance();
                    let _ = self.eat(TokenKind::KwConst);
                    if self.at(TokenKind::LBrace) {
                        getter = Some(self.parse_function_body()?);
                    } else {
                        // abstract accessor: `get;`
                        self.expect(TokenKind::Semi)?;
                    }
                }
                TokenKind::KwSet => {
                    let set_tok = self.advance();
                    let set_ident = Ident { span: set_tok.span };
                    let _ = self.eat(TokenKind::KwConst);
                    if self.at(TokenKind::LBrace) {
                        let body = self.parse_function_body()?;
                        setter = Some((set_ident, body));
                    } else {
                        self.expect(TokenKind::Semi)?;
                    }
                }
                _ => {
                    self.error(ParseError {
                        span: self.current_span(),
                        kind: ParseErrorKind::ExpectedIdent {
                            found: self.peek(),
                        },
                    });
                    self.synchronize();
                }
            }
            if self.pos == pos_before {
                self.advance();
            }
        }
        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(type_start);
        Ok(PropertyDecl {
            span,
            type_expr,
            name,
            getter,
            setter,
        })
    }

    /// Parse the rest of a function: `(params) [const] [override] [final] body_or_semi`
    fn parse_function_rest(
        &mut self,
        attrs: Vec<Attribute>,
        return_type: TypeExpr,
        name: Ident,
        is_private: bool,
        is_protected: bool,
    ) -> Result<FunctionDecl, ParseError> {
        let start = return_type.span.start;
        let params = self.parse_param_list()?;

        // Optional modifiers after params. AS allows these in any order; we
        // accept any sequence and just record the flags.
        let mut is_const = false;
        let mut is_override = false;
        let mut is_final = false;
        let mut _is_property = false;
        loop {
            if !is_const && self.eat(TokenKind::KwConst) {
                is_const = true;
                continue;
            }
            if !is_override && self.eat(TokenKind::KwOverride) {
                is_override = true;
                continue;
            }
            if !is_final && self.eat(TokenKind::KwFinal) {
                is_final = true;
                continue;
            }
            if !_is_property && self.eat(TokenKind::KwProperty) {
                _is_property = true;
                continue;
            }
            break;
        }

        // Optional `from "module";` clause: marks the function as imported
        // from a plugin. AS spec form: `RetType Name(params) from "Plugin";`.
        // The `from` here is contextual (lexed as Ident).
        if self.at(TokenKind::Ident) && self.current_span().text(self.source) == "from" {
            self.advance();
            let _ = self.expect(TokenKind::StringLit)?;
            self.expect(TokenKind::Semi)?;
            let span = self.span_from(start);
            return Ok(FunctionDecl {
                span,
                attributes: attrs,
                return_type,
                name,
                params,
                is_const,
                is_override,
                is_final,
                is_private,
                is_protected,
                body: None,
            });
        }

        // Body or semicolon
        let body = if self.at(TokenKind::LBrace) {
            Some(self.parse_function_body()?)
        } else {
            self.expect(TokenKind::Semi)?;
            None
        };

        let span = self.span_from(start);

        Ok(FunctionDecl {
            span,
            attributes: attrs,
            return_type,
            name,
            params,
            is_const,
            is_override,
            is_final,
            is_private,
            is_protected,
            body,
        })
    }

    /// Parse a function body: `{ stmts }`
    fn parse_function_body(&mut self) -> Result<FunctionBody, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::LBrace)?;

        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let pos_before = self.pos;
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(err) => {
                    self.error(err);
                    // Skip to next ; or }
                    self.synchronize();
                }
            }
            if self.pos == pos_before {
                self.advance();
            }
        }

        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);

        Ok(FunctionBody { span, stmts })
    }

    /// Parse a parameter list: `(param, param, ...)`
    fn parse_param_list(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();

        if !self.at(TokenKind::RParen) {
            params.push(self.parse_param()?);
            while self.eat(TokenKind::Comma) {
                params.push(self.parse_param()?);
            }
        }

        self.expect(TokenKind::RParen)?;
        Ok(params)
    }

    /// Parse a single parameter: `type [name] [= default]`
    fn parse_param(&mut self) -> Result<Param, ParseError> {
        let start = self.current_span().start;
        let type_expr = self.parse_type_expr()?;

        // Optional name — it's a name if we see an Ident that is NOT a param boundary
        let name = if self.at(TokenKind::Ident) && !self.at_param_boundary() {
            Some(self.expect_ident()?)
        } else {
            None
        };

        // Optional default value. Tolerate empty default (`int x =`) — some
        // codebases have placeholder/incomplete signatures and we want to
        // keep parsing rather than cascading.
        let default_value = if self.eat(TokenKind::Eq) {
            if self.at(TokenKind::RParen) || self.at(TokenKind::Comma) {
                None
            } else {
                Some(self.parse_expr()?)
            }
        } else {
            None
        };

        // Extract modifier from the type expression if it's a reference type
        let modifier = match &type_expr.kind {
            TypeExprKind::Reference(_, m) => *m,
            TypeExprKind::Const(inner) => match &inner.kind {
                TypeExprKind::Reference(_, m) => *m,
                _ => ParamModifier::None,
            },
            _ => ParamModifier::None,
        };

        let span = self.span_from(start);
        Ok(Param {
            span,
            type_expr,
            name,
            default_value,
            modifier,
        })
    }

    /// Check if we're at a parameter boundary (comma or close paren).
    fn at_param_boundary(&self) -> bool {
        matches!(self.peek(), TokenKind::Comma | TokenKind::RParen)
    }

    /// Parse a variable declaration after the type and first name have been read:
    /// `[= init] [, name [= init]]* ;`
    fn parse_var_decl_rest(
        &mut self,
        attrs: Vec<Attribute>,
        type_expr: TypeExpr,
        first_name: Ident,
    ) -> Result<VarDeclStmt, ParseError> {
        let start = type_expr.span.start;
        let mut declarators = Vec::new();

        // First declarator
        let init = self.parse_var_initializer(&type_expr)?;
        declarators.push(VarDeclarator {
            name: first_name,
            init,
        });

        // Additional declarators
        while self.eat(TokenKind::Comma) {
            let name = self.expect_ident()?;
            let init = self.parse_var_initializer(&type_expr)?;
            declarators.push(VarDeclarator { name, init });
        }

        self.expect(TokenKind::Semi)?;
        let span = self.span_from(start);

        Ok(VarDeclStmt {
            span,
            attributes: attrs,
            type_expr,
            declarators,
        })
    }

    /// Parse the initializer for a variable declarator. AngelScript supports:
    ///   - `= expr`            (assignment-style)
    ///   - `(args)`            (constructor-style: `IO::File f(path, mode);`)
    ///   - nothing             (uninitialized)
    fn parse_var_initializer(
        &mut self,
        type_expr: &TypeExpr,
    ) -> Result<Option<Expr>, ParseError> {
        if self.eat(TokenKind::Eq) {
            return Ok(Some(self.parse_expr()?));
        }
        if self.at(TokenKind::LParen) {
            // Constructor-style initializer: `Type name(arg1, arg2, ...);`
            // Represent as a TypeConstruct expression so the same node carries
            // the type and args.
            let start = self.current_span().start;
            self.advance(); // eat (
            let mut args = Vec::new();
            if !self.at(TokenKind::RParen) {
                args.push(self.parse_expr()?);
                while self.eat(TokenKind::Comma) {
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                    args.push(self.parse_expr()?);
                }
            }
            let end = self.expect(TokenKind::RParen)?;
            let span = Span::new(start, end.span.end);
            return Ok(Some(Expr {
                span,
                kind: ExprKind::TypeConstruct {
                    target_type: type_expr.clone(),
                    args,
                },
            }));
        }
        Ok(None)
    }

    /// Returns true if the current token could start a type expression.
    fn looks_like_type_start(&self) -> bool {
        match self.peek() {
            TokenKind::Ident
            | TokenKind::KwConst
            | TokenKind::KwAuto
            | TokenKind::KwArray
            | TokenKind::KwDictionary => true,
            k if is_primitive_keyword(k) => true,
            _ => false,
        }
    }

    // ── Expression parsing ────────────────────────────────────────────

    /// Parse an expression (entry point).
    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_assignment_expr()
    }

    /// Parse assignment expressions: lhs [assign-op] rhs (right-associative).
    /// Also handles handle assignment: `@x = @y`.
    fn parse_assignment_expr(&mut self) -> Result<Expr, ParseError> {
        // Handle `@lhs = @rhs` (handle assignment) or `@expr` used as a
        // value in any expression position (e.g. `@TmDojoButton !is null`).
        if self.at(TokenKind::At) {
            let start = self.current_span().start;
            self.advance(); // eat @
            // Parse the LHS with `parse_ternary_expr` so the inner call stops
            // before assignment operators — otherwise `@x = null` is greedily
            // consumed as a nested `Assign` and HandleAssign is never produced.
            let lhs = self.parse_ternary_expr()?;
            if self.at(TokenKind::Eq) {
                self.advance(); // eat =
                // Eat optional @ on rhs
                self.eat(TokenKind::At);
                let rhs = self.parse_assignment_expr()?;
                return Ok(Expr {
                    span: Span::new(start, rhs.span.end),
                    kind: ExprKind::HandleAssign {
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                });
            }
            // @ without assignment — just return the inner expression
            return Ok(lhs);
        }

        let expr = self.parse_ternary_expr()?;
        // Check for assignment operators
        if matches!(
            self.peek(),
            TokenKind::Eq
                | TokenKind::PlusEq
                | TokenKind::MinusEq
                | TokenKind::StarEq
                | TokenKind::SlashEq
                | TokenKind::PercentEq
                | TokenKind::AmpEq
                | TokenKind::PipeEq
                | TokenKind::CaretEq
                | TokenKind::LtLtEq
                | TokenKind::GtGtEq
        ) {
            let op = self.parse_assign_op();
            let rhs = self.parse_assignment_expr()?;
            let span = Span::new(expr.span.start, rhs.span.end);
            Ok(Expr {
                span,
                kind: ExprKind::Assign {
                    lhs: Box::new(expr),
                    op,
                    rhs: Box::new(rhs),
                },
            })
        } else {
            Ok(expr)
        }
    }

    fn parse_assign_op(&mut self) -> AssignOp {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Eq => AssignOp::Assign,
            TokenKind::PlusEq => AssignOp::AddAssign,
            TokenKind::MinusEq => AssignOp::SubAssign,
            TokenKind::StarEq => AssignOp::MulAssign,
            TokenKind::SlashEq => AssignOp::DivAssign,
            TokenKind::PercentEq => AssignOp::ModAssign,
            TokenKind::AmpEq => AssignOp::BitAndAssign,
            TokenKind::PipeEq => AssignOp::BitOrAssign,
            TokenKind::CaretEq => AssignOp::BitXorAssign,
            TokenKind::LtLtEq => AssignOp::ShlAssign,
            TokenKind::GtGtEq => AssignOp::ShrAssign,
            _ => AssignOp::Assign,
        }
    }

    /// Parse ternary: `expr ? expr : expr`
    fn parse_ternary_expr(&mut self) -> Result<Expr, ParseError> {
        let expr = self.parse_or_expr()?;
        if self.eat(TokenKind::Question) {
            let then_expr = self.parse_assignment_expr()?;
            self.expect(TokenKind::Colon)?;
            let else_expr = self.parse_assignment_expr()?;
            let span = Span::new(expr.span.start, else_expr.span.end);
            Ok(Expr {
                span,
                kind: ExprKind::Ternary {
                    condition: Box::new(expr),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                },
            })
        } else {
            Ok(expr)
        }
    }

    /// Logical OR: `expr || expr`
    fn parse_or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_xor_expr()?;
        while self.at(TokenKind::PipePipe) {
            self.advance();
            let rhs = self.parse_xor_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::Or,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Logical XOR: `expr ^^ expr` (AngelScript-specific, between `||` and `&&`)
    fn parse_xor_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and_expr()?;
        while self.at(TokenKind::CaretCaret) {
            self.advance();
            let rhs = self.parse_and_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::Xor,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Logical AND: `expr && expr`
    fn parse_and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bitor_expr()?;
        while self.at(TokenKind::AmpAmp) {
            self.advance();
            let rhs = self.parse_bitor_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::And,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Bitwise OR: `expr | expr`
    fn parse_bitor_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bitxor_expr()?;
        while self.at(TokenKind::Pipe) {
            self.advance();
            let rhs = self.parse_bitxor_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::BitOr,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Bitwise XOR: `expr ^ expr`
    fn parse_bitxor_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bitand_expr()?;
        while self.at(TokenKind::Caret) {
            self.advance();
            let rhs = self.parse_bitand_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::BitXor,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Bitwise AND: `expr & expr`
    fn parse_bitand_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_equality_expr()?;
        while self.at(TokenKind::Amp) {
            self.advance();
            let rhs = self.parse_equality_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::BitAnd,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Equality: `expr == expr`, `expr != expr`
    fn parse_equality_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_relational_expr()?;
        while matches!(self.peek(), TokenKind::EqEq | TokenKind::BangEq) {
            let op = if self.advance().kind == TokenKind::EqEq {
                BinOp::Eq
            } else {
                BinOp::NotEq
            };
            let rhs = self.parse_relational_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Relational: `<`, `>`, `<=`, `>=`, `is`, `!is`
    fn parse_relational_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_shift_expr()?;
        loop {
            match self.peek() {
                TokenKind::Lt | TokenKind::Gt | TokenKind::LtEq | TokenKind::GtEq => {
                    let op = match self.advance().kind {
                        TokenKind::Lt => BinOp::Lt,
                        TokenKind::Gt => BinOp::Gt,
                        TokenKind::LtEq => BinOp::LtEq,
                        TokenKind::GtEq => BinOp::GtEq,
                        _ => unreachable!(),
                    };
                    let rhs = self.parse_shift_expr()?;
                    let span = Span::new(lhs.span.start, rhs.span.end);
                    lhs = Expr {
                        span,
                        kind: ExprKind::Binary {
                            lhs: Box::new(lhs),
                            op,
                            rhs: Box::new(rhs),
                        },
                    };
                }
                TokenKind::KwIs => {
                    self.advance(); // eat `is`
                    let target = self.parse_is_target()?;
                    let span = self.span_from(lhs.span.start);
                    lhs = Expr {
                        span,
                        kind: ExprKind::Is {
                            expr: Box::new(lhs),
                            target,
                            negated: false,
                        },
                    };
                }
                TokenKind::Bang if self.peek_ahead(1) == TokenKind::KwIs => {
                    self.advance(); // eat `!`
                    self.advance(); // eat `is`
                    let target = self.parse_is_target()?;
                    let span = self.span_from(lhs.span.start);
                    lhs = Expr {
                        span,
                        kind: ExprKind::Is {
                            expr: Box::new(lhs),
                            target,
                            negated: true,
                        },
                    };
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    /// Parse the right-hand side of an `is`/`!is` operator. Accepts:
    ///   `null`             → `IsTarget::Null`
    ///   `this`             → handle expression on RHS
    ///   any expression     → `IsTarget::Expr(...)`
    fn parse_is_target(&mut self) -> Result<IsTarget, ParseError> {
        if self.at(TokenKind::KwNull) {
            self.advance();
            return Ok(IsTarget::Null);
        }
        // Anything else is just an expression. Use shift level so we don't
        // greedily eat operators that should belong to the enclosing
        // relational/logical level.
        let expr = self.parse_shift_expr()?;
        Ok(IsTarget::Expr(Box::new(expr)))
    }

    /// Shift: `expr << expr`, `expr >> expr`
    fn parse_shift_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_additive_expr()?;
        while matches!(self.peek(), TokenKind::LtLt | TokenKind::GtGt) {
            let op = if self.advance().kind == TokenKind::LtLt {
                BinOp::Shl
            } else {
                BinOp::Shr
            };
            let rhs = self.parse_additive_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Additive: `expr + expr`, `expr - expr`
    fn parse_additive_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_multiplicative_expr()?;
        while matches!(self.peek(), TokenKind::Plus | TokenKind::Minus) {
            let op = if self.advance().kind == TokenKind::Plus {
                BinOp::Add
            } else {
                BinOp::Sub
            };
            let rhs = self.parse_multiplicative_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Multiplicative: `expr * expr`, `expr / expr`, `expr % expr`
    fn parse_multiplicative_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_power_expr()?;
        while matches!(
            self.peek(),
            TokenKind::Star | TokenKind::Slash | TokenKind::Percent
        ) {
            let op = match self.advance().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => unreachable!(),
            };
            let rhs = self.parse_power_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op,
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    /// Power: `expr ** expr` (right-associative, higher precedence than `*`)
    fn parse_power_expr(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_unary_expr()?;
        if self.at(TokenKind::StarStar) {
            self.advance();
            // Right-associative: parse the RHS as another power expression
            let rhs = self.parse_power_expr()?;
            let span = Span::new(lhs.span.start, rhs.span.end);
            return Ok(Expr {
                span,
                kind: ExprKind::Binary {
                    lhs: Box::new(lhs),
                    op: BinOp::Pow,
                    rhs: Box::new(rhs),
                },
            });
        }
        Ok(lhs)
    }

    /// Unary prefix: `-`, `!`, `~`, `++`, `--`, `@`
    fn parse_unary_expr(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            // `@expr` — produces a handle to expr. Treat as transparent
            // wrapper since the AST has no distinct unary handle node.
            TokenKind::At => {
                self.advance();
                return self.parse_unary_expr();
            }
            TokenKind::Minus => {
                let start = self.current_span().start;
                self.advance();
                let expr = self.parse_unary_expr()?;
                let span = Span::new(start, expr.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr: Box::new(expr),
                    },
                })
            }
            TokenKind::Bang => {
                // Check for `!is` — but only in parse_relational_expr context.
                // At unary level, `!` is always logical not.
                let start = self.current_span().start;
                self.advance();
                let expr = self.parse_unary_expr()?;
                let span = Span::new(start, expr.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(expr),
                    },
                })
            }
            TokenKind::Tilde => {
                let start = self.current_span().start;
                self.advance();
                let expr = self.parse_unary_expr()?;
                let span = Span::new(start, expr.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::Unary {
                        op: UnaryOp::BitNot,
                        expr: Box::new(expr),
                    },
                })
            }
            TokenKind::PlusPlus => {
                let start = self.current_span().start;
                self.advance();
                let expr = self.parse_unary_expr()?;
                let span = Span::new(start, expr.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::Unary {
                        op: UnaryOp::Inc,
                        expr: Box::new(expr),
                    },
                })
            }
            TokenKind::MinusMinus => {
                let start = self.current_span().start;
                self.advance();
                let expr = self.parse_unary_expr()?;
                let span = Span::new(start, expr.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::Unary {
                        op: UnaryOp::Dec,
                        expr: Box::new(expr),
                    },
                })
            }
            _ => self.parse_postfix_expr(),
        }
    }

    /// Postfix: `.member`, `(args)`, `[index]`, `++`, `--`, `::name`
    fn parse_postfix_expr(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary_expr()?;
        loop {
            match self.peek() {
                TokenKind::Dot => {
                    self.advance();
                    let member = self.expect_ident()?;
                    let span = Span::new(expr.span.start, member.span.end);
                    expr = Expr {
                        span,
                        kind: ExprKind::Member {
                            object: Box::new(expr),
                            member,
                        },
                    };
                }
                TokenKind::LParen => {
                    self.advance();
                    let args = self.parse_arg_list()?;
                    let end = self.expect(TokenKind::RParen)?;
                    let span = Span::new(expr.span.start, end.span.end);
                    expr = Expr {
                        span,
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                    };
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.parse_expr()?;
                    let end = self.expect(TokenKind::RBracket)?;
                    let span = Span::new(expr.span.start, end.span.end);
                    expr = Expr {
                        span,
                        kind: ExprKind::Index {
                            object: Box::new(expr),
                            index: Box::new(index),
                        },
                    };
                }
                TokenKind::PlusPlus => {
                    let end = self.advance();
                    let span = Span::new(expr.span.start, end.span.end);
                    expr = Expr {
                        span,
                        kind: ExprKind::Postfix {
                            expr: Box::new(expr),
                            op: UnaryOp::Inc,
                        },
                    };
                }
                TokenKind::MinusMinus => {
                    let end = self.advance();
                    let span = Span::new(expr.span.start, end.span.end);
                    expr = Expr {
                        span,
                        kind: ExprKind::Postfix {
                            expr: Box::new(expr),
                            op: UnaryOp::Dec,
                        },
                    };
                }
                TokenKind::Lt => {
                    // Template function call: `name<TypeArgs>(args)`. Only
                    // disambiguate as a template call if the `<` actually
                    // looks like type args followed by a `(`.
                    if self.looks_like_type_args() {
                        // Quick scan: after the matching `>`, must be `(`.
                        // Skip past the type-args span by walking until we
                        // close them, then check.
                        let saved_pos = self.pos;
                        let saved_split = self.split_gtgt;
                        self.advance(); // eat `<`
                        let mut ate_template = true;
                        let mut _type_args = Vec::new();
                        if !self.at(TokenKind::Gt) {
                            match self.parse_type_expr() {
                                Ok(t) => _type_args.push(t),
                                Err(_) => {
                                    ate_template = false;
                                }
                            }
                            while ate_template && self.eat(TokenKind::Comma) {
                                match self.parse_type_expr() {
                                    Ok(t) => _type_args.push(t),
                                    Err(_) => {
                                        ate_template = false;
                                        break;
                                    }
                                }
                            }
                        }
                        if ate_template && self.consume_template_close().is_err() {
                            ate_template = false;
                        }
                        if ate_template && self.at(TokenKind::LParen) {
                            self.advance(); // eat (
                            let args = self.parse_arg_list()?;
                            let end = self.expect(TokenKind::RParen)?;
                            let span = Span::new(expr.span.start, end.span.end);
                            expr = Expr {
                                span,
                                kind: ExprKind::Call {
                                    callee: Box::new(expr),
                                    args,
                                },
                            };
                            continue;
                        }
                        // Not actually a template call — rewind.
                        self.pos = saved_pos;
                        self.split_gtgt = saved_split;
                    }
                    break;
                }
                TokenKind::ColonColon => {
                    // Namespace access: expr::member
                    // Allow ident-like tokens after `::` (some keywords like
                    // `null`, `true`, `false`, `get`, `set`, `function` are
                    // commonly reused as method/member names in AS namespaces).
                    let next = self.peek_ahead(1);
                    let next_can_be_member = matches!(
                        next,
                        TokenKind::Ident
                            | TokenKind::KwNull
                            | TokenKind::KwTrue
                            | TokenKind::KwFalse
                            | TokenKind::KwGet
                            | TokenKind::KwSet
                            | TokenKind::KwThis
                            | TokenKind::KwFunction
                    );
                    if next_can_be_member {
                        self.advance(); // eat ::
                        let mtok = self.advance();
                        let member = Ident { span: mtok.span };

                        // Build a NamespaceAccess by collecting segments
                        let mut segments = match expr.kind {
                            ExprKind::Ident(ref id) => vec![id.clone()],
                            ExprKind::NamespaceAccess { ref path } => path.segments.clone(),
                            _ => {
                                let span = Span::new(expr.span.start, member.span.end);
                                expr = Expr {
                                    span,
                                    kind: ExprKind::Member {
                                        object: Box::new(expr),
                                        member,
                                    },
                                };
                                continue;
                            }
                        };
                        segments.push(member);
                        let span =
                            Span::new(expr.span.start, segments.last().unwrap().span.end);
                        expr = Expr {
                            span,
                            kind: ExprKind::NamespaceAccess {
                                path: QualifiedName { span, segments },
                            },
                        };
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse a comma-separated argument list (without the surrounding parens).
    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if !self.at(TokenKind::RParen) {
            args.push(self.parse_call_argument()?);
            while self.eat(TokenKind::Comma) {
                args.push(self.parse_call_argument()?);
            }
        }
        Ok(args)
    }

    /// Parse a single call argument. Supports AngelScript named arguments:
    /// `name: value`. The name is currently dropped (the AST has no slot for
    /// it); only the value is returned. Future work could record names for
    /// signature-help and overload resolution.
    fn parse_call_argument(&mut self) -> Result<Expr, ParseError> {
        if self.at(TokenKind::Ident) && self.peek_ahead(1) == TokenKind::Colon {
            self.advance(); // eat name
            self.advance(); // eat :
        }
        self.parse_expr()
    }

    /// Parse a primary expression: literals, identifiers, cast, parenthesized, array init.
    fn parse_primary_expr(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            TokenKind::IntLit => {
                let tok = self.advance();
                let val: i64 = tok.span.text(self.source).parse().unwrap_or(0);
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::IntLit(val),
                })
            }
            TokenKind::HexLit => {
                let tok = self.advance();
                let text = tok.span.text(self.source);
                let hex_str = text
                    .strip_prefix("0x")
                    .or_else(|| text.strip_prefix("0X"))
                    .unwrap_or(text);
                let val = u64::from_str_radix(hex_str, 16).unwrap_or(0);
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::HexLit(val),
                })
            }
            TokenKind::FloatLit => {
                let tok = self.advance();
                let text = tok.span.text(self.source);
                // Strip trailing 'f' or 'F' if present
                let clean = text
                    .strip_suffix('f')
                    .or_else(|| text.strip_suffix('F'))
                    .unwrap_or(text);
                let val: f64 = clean.parse().unwrap_or(0.0);
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::FloatLit(val),
                })
            }
            TokenKind::StringLit => {
                let first = self.advance();
                let mut end = first.span.end;
                // Implicit string concatenation: adjacent string literals are
                // joined into a single StringLit (C-style; AngelScript also
                // supports this for multi-line wrapped regex/strings).
                while self.at(TokenKind::StringLit) {
                    let next = self.advance();
                    end = next.span.end;
                }
                Ok(Expr {
                    span: Span::new(first.span.start, end),
                    kind: ExprKind::StringLit,
                })
            }
            TokenKind::KwTrue => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::BoolLit(true),
                })
            }
            TokenKind::KwFalse => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::BoolLit(false),
                })
            }
            TokenKind::KwNull => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::Null,
                })
            }
            TokenKind::KwThis => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::This,
                })
            }
            TokenKind::KwSuper => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::Super,
                })
            }
            TokenKind::KwCast => {
                // cast<Type>(expr)
                let start = self.current_span().start;
                self.advance(); // eat `cast`
                self.expect(TokenKind::Lt)?;
                let target_type = self.parse_type_expr()?;
                self.consume_template_close()?;
                self.expect(TokenKind::LParen)?;
                let expr = self.parse_expr()?;
                let end = self.expect(TokenKind::RParen)?;
                let span = Span::new(start, end.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::Cast {
                        target_type,
                        expr: Box::new(expr),
                    },
                })
            }
            TokenKind::Ident => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::Ident(Ident { span: tok.span }),
                })
            }
            // Leading `::` — global-namespace prefix: `::Foo()` resolves
            // `Foo` in the global namespace explicitly.
            TokenKind::ColonColon => {
                let cc = self.advance(); // eat ::
                let ident = self.expect_ident()?;
                let mut segments = vec![ident];
                while self.at(TokenKind::ColonColon)
                    && self.peek_ahead(1) == TokenKind::Ident
                {
                    self.advance();
                    segments.push(self.expect_ident()?);
                }
                let span = Span::new(
                    cc.span.start,
                    segments.last().map(|s| s.span.end).unwrap_or(cc.span.end),
                );
                let qname = QualifiedName { span, segments };
                Ok(Expr {
                    span,
                    kind: ExprKind::NamespaceAccess { path: qname },
                })
            }
            // Anonymous function literal: `function(params) { body }`
            TokenKind::KwFunction => {
                let start = self.current_span().start;
                self.advance(); // eat `function`
                let params = self.parse_param_list()?;
                let body = self.parse_function_body()?;
                let span = self.span_from(start);
                Ok(Expr {
                    span,
                    kind: ExprKind::Lambda { params, body },
                })
            }
            // Type-construction expressions: `int(x)`, `uint64(n)`, `string(x)`,
            // `array<int>(size)`, `dictionary()`, etc. A primitive type keyword
            // (or `array`/`dictionary`) appearing in expression position can be
            // either a type-construction call OR the start of a namespace path
            // like `string::Join(...)` (AngelScript reuses `string` as a
            // namespace for string utility functions).
            kind if is_primitive_keyword(kind)
                || kind == TokenKind::KwArray
                || kind == TokenKind::KwDictionary =>
            {
                // Disambiguate by lookahead: `::` means namespace access.
                if self.peek_ahead(1) == TokenKind::ColonColon {
                    // Treat the keyword as the first segment of a qualified
                    // name and fall back to NamespaceAccess.
                    let kw_tok = self.advance();
                    let mut segments = vec![Ident { span: kw_tok.span }];
                    while self.at(TokenKind::ColonColon)
                        && self.peek_ahead(1) == TokenKind::Ident
                    {
                        self.advance(); // eat ::
                        let ident = self.expect_ident()?;
                        segments.push(ident);
                    }
                    let span = Span::new(
                        kw_tok.span.start,
                        segments.last().map(|s| s.span.end).unwrap_or(kw_tok.span.end),
                    );
                    let qname = QualifiedName { span, segments };
                    return Ok(Expr {
                        span,
                        kind: ExprKind::NamespaceAccess { path: qname },
                    });
                }
                let start = self.current_span().start;
                let mut target_type = self.parse_base_type()?;
                // Allow array shorthand suffix(es): `string[]`, `int[][]`.
                while self.at(TokenKind::LBracket)
                    && self.peek_ahead(1) == TokenKind::RBracket
                {
                    self.advance(); // [
                    self.advance(); // ]
                    let span = self.span_from(start);
                    target_type = TypeExpr {
                        span,
                        kind: TypeExprKind::Array(Box::new(target_type)),
                    };
                }
                // Constructor-call form: `int(x)`, `array<T>(n)`, etc.
                if self.at(TokenKind::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        args.push(self.parse_expr()?);
                        while self.eat(TokenKind::Comma) {
                            if self.at(TokenKind::RParen) {
                                break;
                            }
                            args.push(self.parse_expr()?);
                        }
                    }
                    let end = self.expect(TokenKind::RParen)?;
                    let span = Span::new(start, end.span.end);
                    return Ok(Expr {
                        span,
                        kind: ExprKind::TypeConstruct { target_type, args },
                    });
                }
                // Anonymous typed initializer: `array<T> = { init_list }`.
                // Used in code like `startnew(F, array<string> = {x, y})`.
                if self.at(TokenKind::Eq) && self.peek_ahead(1) == TokenKind::LBrace {
                    self.advance(); // eat =
                    self.advance(); // eat {
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RBrace) {
                        args.push(self.parse_expr()?);
                        while self.eat(TokenKind::Comma) {
                            if self.at(TokenKind::RBrace) {
                                break;
                            }
                            args.push(self.parse_expr()?);
                        }
                    }
                    let end = self.expect(TokenKind::RBrace)?;
                    let span = Span::new(start, end.span.end);
                    return Ok(Expr {
                        span,
                        kind: ExprKind::TypeConstruct { target_type, args },
                    });
                }
                Err(ParseError {
                    span: self.current_span(),
                    kind: ParseErrorKind::Expected {
                        expected: TokenKind::LParen,
                        found: self.peek(),
                    },
                })
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                Ok(expr)
            }
            TokenKind::LBrace => {
                // Array initializer: {a, b, c}
                let start = self.current_span().start;
                self.advance(); // eat {
                let mut exprs = Vec::new();
                if !self.at(TokenKind::RBrace) {
                    exprs.push(self.parse_expr()?);
                    while self.eat(TokenKind::Comma) {
                        if self.at(TokenKind::RBrace) {
                            break; // trailing comma
                        }
                        exprs.push(self.parse_expr()?);
                    }
                }
                let end = self.expect(TokenKind::RBrace)?;
                let span = Span::new(start, end.span.end);
                Ok(Expr {
                    span,
                    kind: ExprKind::ArrayInit(exprs),
                })
            }
            other => Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::ExpectedExpr { found: other },
            }),
        }
    }

    // ── Statement parsing ───────────────────────────────────────────────

    /// Parse a statement.
    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek() {
            TokenKind::Semi => {
                let tok = self.advance();
                Ok(Stmt {
                    span: tok.span,
                    kind: StmtKind::Empty,
                })
            }
            TokenKind::LBrace => self.parse_block_stmt(),
            TokenKind::KwIf => self.parse_if_stmt(),
            TokenKind::KwFor => self.parse_for_stmt(),
            TokenKind::KwWhile => self.parse_while_stmt(),
            TokenKind::KwDo => self.parse_do_while_stmt(),
            TokenKind::KwSwitch => self.parse_switch_stmt(),
            TokenKind::KwBreak => {
                let start = self.current_span().start;
                self.advance();
                self.expect(TokenKind::Semi)?;
                let span = self.span_from(start);
                Ok(Stmt {
                    span,
                    kind: StmtKind::Break,
                })
            }
            TokenKind::KwContinue => {
                let start = self.current_span().start;
                self.advance();
                self.expect(TokenKind::Semi)?;
                let span = self.span_from(start);
                Ok(Stmt {
                    span,
                    kind: StmtKind::Continue,
                })
            }
            TokenKind::KwReturn => {
                let start = self.current_span().start;
                self.advance();
                if self.at(TokenKind::Semi) {
                    self.advance();
                    let span = self.span_from(start);
                    Ok(Stmt {
                        span,
                        kind: StmtKind::Return(None),
                    })
                } else {
                    let expr = self.parse_expr()?;
                    self.expect(TokenKind::Semi)?;
                    let span = self.span_from(start);
                    Ok(Stmt {
                        span,
                        kind: StmtKind::Return(Some(expr)),
                    })
                }
            }
            TokenKind::KwTry => self.parse_try_catch_stmt(),
            _ => {
                // Disambiguate between var decl and expression statement.
                if self.looks_like_var_decl() {
                    self.parse_var_decl_stmt()
                } else {
                    self.parse_expression_stmt()
                }
            }
        }
    }

    /// Parse a block statement: `{ stmts }`
    fn parse_block_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let pos_before = self.pos;
            match self.parse_stmt() {
                Ok(s) => stmts.push(s),
                Err(e) => {
                    self.error(e);
                    self.synchronize();
                }
            }
            if self.pos == pos_before {
                self.advance();
            }
        }
        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::Block(stmts),
        })
    }

    /// Parse an if statement: `if (cond) stmt [else stmt]`
    fn parse_if_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwIf)?;
        self.expect(TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        let then_branch = Box::new(self.parse_stmt()?);
        let else_branch = if self.eat(TokenKind::KwElse) {
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::If {
                condition,
                then_branch,
                else_branch,
            },
        })
    }

    /// Parse a for loop: `for (init; cond; step) body`
    fn parse_for_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwFor)?;
        self.expect(TokenKind::LParen)?;

        // Init: can be var decl, expression, or empty
        let init = if self.at(TokenKind::Semi) {
            self.advance();
            None
        } else if self.looks_like_var_decl() {
            // parse_var_decl_stmt consumes the trailing `;`
            Some(Box::new(self.parse_var_decl_stmt()?))
        } else {
            let expr_start = self.current_span().start;
            let expr = self.parse_expr()?;
            self.expect(TokenKind::Semi)?;
            let span = self.span_from(expr_start);
            Some(Box::new(Stmt {
                span,
                kind: StmtKind::Expr(expr),
            }))
        };

        // Condition
        let condition = if self.at(TokenKind::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(TokenKind::Semi)?;

        // Step: comma-separated expressions
        let mut step = Vec::new();
        if !self.at(TokenKind::RParen) {
            step.push(self.parse_expr()?);
            while self.eat(TokenKind::Comma) {
                step.push(self.parse_expr()?);
            }
        }
        self.expect(TokenKind::RParen)?;

        let body = Box::new(self.parse_stmt()?);
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::For {
                init,
                condition,
                step,
                body,
            },
        })
    }

    /// Parse a while loop: `while (cond) body`
    fn parse_while_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwWhile)?;
        self.expect(TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        let body = Box::new(self.parse_stmt()?);
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::While { condition, body },
        })
    }

    /// Parse a do-while loop: `do body while (cond);`
    fn parse_do_while_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwDo)?;
        let body = Box::new(self.parse_stmt()?);
        self.expect(TokenKind::KwWhile)?;
        self.expect(TokenKind::LParen)?;
        let condition = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::Semi)?;
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::DoWhile { body, condition },
        })
    }

    /// Parse a switch statement: `switch (expr) { cases }`
    fn parse_switch_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwSwitch)?;
        self.expect(TokenKind::LParen)?;
        let expr = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::LBrace)?;

        let mut cases = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            let case_start = self.current_span().start;
            let label = if self.eat(TokenKind::KwCase) {
                let case_expr = self.parse_expr()?;
                self.expect(TokenKind::Colon)?;
                SwitchLabel::Case(case_expr)
            } else if self.eat(TokenKind::KwDefault) {
                self.expect(TokenKind::Colon)?;
                SwitchLabel::Default
            } else {
                return Err(ParseError {
                    span: self.current_span(),
                    kind: ParseErrorKind::Custom("expected 'case' or 'default'".into()),
                });
            };

            // Collect statements until next case/default/}
            let mut stmts = Vec::new();
            while !self.at(TokenKind::KwCase)
                && !self.at(TokenKind::KwDefault)
                && !self.at(TokenKind::RBrace)
                && !self.at_end()
            {
                let pos_before = self.pos;
                match self.parse_stmt() {
                    Ok(s) => stmts.push(s),
                    Err(e) => {
                        self.error(e);
                        self.synchronize();
                    }
                }
                if self.pos == pos_before {
                    self.advance();
                }
            }

            let case_span = self.span_from(case_start);
            cases.push(SwitchCase {
                span: case_span,
                label,
                stmts,
            });
        }

        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::Switch { expr, cases },
        })
    }

    /// Parse a try-catch statement: `try { } catch { }`
    fn parse_try_catch_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        self.expect(TokenKind::KwTry)?;
        let try_body = Box::new(self.parse_block_stmt()?);
        self.expect(TokenKind::KwCatch)?;
        let catch_body = Box::new(self.parse_block_stmt()?);
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::TryCatch {
                try_body,
                catch_body,
            },
        })
    }

    /// Parse a variable declaration statement inside a function body.
    fn parse_var_decl_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        let type_expr = self.parse_type_expr()?;
        let first_name = self.expect_ident()?;
        let decl = self.parse_var_decl_rest(Vec::new(), type_expr, first_name)?;
        let span = Span::new(start, decl.span.end);
        Ok(Stmt {
            span,
            kind: StmtKind::VarDecl(decl),
        })
    }

    /// Parse an expression statement: `expr;`
    fn parse_expression_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.current_span().start;
        let expr = self.parse_expr()?;
        self.expect(TokenKind::Semi)?;
        let span = self.span_from(start);
        Ok(Stmt {
            span,
            kind: StmtKind::Expr(expr),
        })
    }

    /// Heuristic lookahead to determine if the current position starts a variable
    /// declaration rather than an expression statement.
    /// Pattern: looks like a type followed by an identifier.
    /// Disambiguate `Type name(...)` between a function declaration and a
    /// variable with constructor-style initializer. Caller must be at the
    /// `(` token. Returns true if the args inside the parens look like
    /// expressions (so this should be a var init), false if they look like
    /// parameter declarations.
    fn looks_like_constructor_init(&self) -> bool {
        // Empty parens: `Type name()` — could be either, but for an empty
        // parameter list we prefer function decl (the more common case).
        if self.peek_ahead(1) == TokenKind::RParen {
            return false;
        }
        // First token after `(` is an obvious expression-only token.
        let first = self.peek_ahead(1);
        if matches!(
            first,
            TokenKind::StringLit
                | TokenKind::IntLit
                | TokenKind::FloatLit
                | TokenKind::HexLit
                | TokenKind::KwTrue
                | TokenKind::KwFalse
                | TokenKind::KwNull
                | TokenKind::KwThis
                | TokenKind::Minus
                | TokenKind::Bang
                | TokenKind::Tilde
                | TokenKind::At
                | TokenKind::LBrace
        ) {
            return true;
        }
        // `Ident(` → function call (var init), `Ident Ident` or `Ident@` etc → param.
        if first == TokenKind::Ident {
            let second = self.peek_ahead(2);
            // `Ident []` is an array TYPE shorthand (`vec4[] foo`), not an
            // index access — that's a parameter, not a var init.
            if second == TokenKind::LBracket && self.peek_ahead(3) == TokenKind::RBracket {
                return false;
            }
            // `Ident <…>` could be a template type — check `looks_like_type_args`.
            if second == TokenKind::Lt {
                return false;
            }
            // `Ident::Ident…` is a qualified type name — looks like a parameter.
            if second == TokenKind::ColonColon {
                return false;
            }
            return matches!(
                second,
                TokenKind::LParen
                    | TokenKind::Dot
                    | TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::Percent
                    | TokenKind::EqEq
                    | TokenKind::BangEq
                    | TokenKind::AmpAmp
                    | TokenKind::PipePipe
                    | TokenKind::PlusPlus
                    | TokenKind::MinusMinus
                    // Trailing comma/RParen on a bare ident: `f(x)` is a call
                    // with `x` as the value
                    | TokenKind::Comma
                    | TokenKind::RParen
            );
        }
        false
    }

    fn looks_like_var_decl(&self) -> bool {
        if !self.looks_like_type_start() {
            return false;
        }
        // Save position and try to scan forward past a type to see if an identifier follows.
        let mut i = 0;
        let kind = self.peek_ahead(i);

        // Handle `const` prefix
        if kind == TokenKind::KwConst {
            i += 1;
        }

        // Must see a type-starting token
        let base = self.peek_ahead(i);
        match base {
            TokenKind::KwAuto => {
                i += 1;
            }
            _ if is_primitive_keyword(base) => {
                i += 1;
            }
            TokenKind::Ident => {
                i += 1;
                // Skip qualified name (::Ident)*
                while self.peek_ahead(i) == TokenKind::ColonColon
                    && self.peek_ahead(i + 1) == TokenKind::Ident
                {
                    i += 2;
                }
                // Skip template args <...>
                if self.peek_ahead(i) == TokenKind::Lt {
                    let mut depth = 1i32;
                    i += 1;
                    loop {
                        let k = self.peek_ahead(i);
                        match k {
                            TokenKind::Lt => depth += 1,
                            TokenKind::Gt => {
                                depth -= 1;
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            TokenKind::GtGt => {
                                depth -= 2;
                                if depth <= 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            TokenKind::Eof => return false,
                            _ => {}
                        }
                        i += 1;
                    }
                }
            }
            TokenKind::KwArray => {
                i += 1;
                // Skip template args if present
                if self.peek_ahead(i) == TokenKind::Lt {
                    let mut depth = 1i32;
                    i += 1;
                    loop {
                        let k = self.peek_ahead(i);
                        match k {
                            TokenKind::Lt => depth += 1,
                            TokenKind::Gt => {
                                depth -= 1;
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            TokenKind::GtGt => {
                                depth -= 2;
                                if depth <= 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            TokenKind::Eof => return false,
                            _ => {}
                        }
                        i += 1;
                    }
                }
            }
            TokenKind::KwDictionary => {
                i += 1;
            }
            _ => return false,
        }

        // Skip handle (@) and array ([]) suffixes
        loop {
            match self.peek_ahead(i) {
                TokenKind::At => {
                    i += 1;
                    // Optional trailing `const` after `@` (`Foo@ const name`)
                    if self.peek_ahead(i) == TokenKind::KwConst {
                        i += 1;
                    }
                }
                TokenKind::LBracket if self.peek_ahead(i + 1) == TokenKind::RBracket => {
                    i += 2;
                }
                TokenKind::Amp => {
                    i += 1;
                    // Skip optional in/out/inout modifier
                    match self.peek_ahead(i) {
                        TokenKind::KwIn | TokenKind::KwOut | TokenKind::KwInout => {
                            i += 1;
                        }
                        _ => {}
                    }
                }
                _ => break,
            }
        }

        // After the type, we should see an identifier for it to be a var decl
        self.peek_ahead(i) == TokenKind::Ident
    }

}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Check if a TokenKind is a primitive type keyword.
fn is_primitive_keyword(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwVoid
            | TokenKind::KwBool
            | TokenKind::KwInt
            | TokenKind::KwInt8
            | TokenKind::KwInt16
            | TokenKind::KwInt32
            | TokenKind::KwInt64
            | TokenKind::KwUint
            | TokenKind::KwUint8
            | TokenKind::KwUint16
            | TokenKind::KwUint32
            | TokenKind::KwUint64
            | TokenKind::KwFloat
            | TokenKind::KwDouble
            | TokenKind::KwString
    )
}

/// Convenience: tokenize + parse a source string.
pub fn parse(source: &str) -> (SourceFile, Vec<ParseError>) {
    let tokens = tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file = parser.parse_file();
    let errors = parser.errors;
    (file, errors)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse a type expression from source text.
    fn parse_type(source: &str) -> (TypeExpr, Vec<ParseError>) {
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let ty = parser.parse_type_expr().expect("failed to parse type");
        (ty, parser.errors)
    }

    /// Helper: parse a file and return (SourceFile, errors).
    fn parse_file(source: &str) -> (SourceFile, Vec<ParseError>) {
        parse(source)
    }

    // ── Task 5: Type expression tests ────────────────────────────────

    #[test]
    fn test_type_primitive_int() {
        let (ty, errors) = parse_type("int");
        assert!(errors.is_empty());
        assert!(matches!(ty.kind, TypeExprKind::Primitive(TokenKind::KwInt)));
    }

    #[test]
    fn test_type_handle() {
        let (ty, errors) = parse_type("CGameCtnBlock@");
        assert!(errors.is_empty());
        match ty.kind {
            TypeExprKind::Handle(inner) => {
                assert!(matches!(inner.kind, TypeExprKind::Named(_)));
            }
            _ => panic!("expected Handle, got {:?}", ty.kind),
        }
    }

    #[test]
    fn test_type_const_ref() {
        let (ty, errors) = parse_type("const string &in");
        assert!(errors.is_empty());
        match ty.kind {
            TypeExprKind::Const(inner) => match inner.kind {
                TypeExprKind::Reference(base, modifier) => {
                    assert!(matches!(
                        base.kind,
                        TypeExprKind::Primitive(TokenKind::KwString)
                    ));
                    assert_eq!(modifier, ParamModifier::In);
                }
                _ => panic!("expected Reference, got {:?}", inner.kind),
            },
            _ => panic!("expected Const, got {:?}", ty.kind),
        }
    }

    #[test]
    fn parser_const_handle_ordering_leading() {
        // AC20: `const Foo@` → handle to const (pointee const).
        let (ty, errors) = parse_type("const Foo@");
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match ty.kind {
            TypeExprKind::Handle(inner) => match inner.kind {
                TypeExprKind::Const(base) => {
                    assert!(matches!(base.kind, TypeExprKind::Named(_)));
                }
                other => panic!(
                    "expected Const inside Handle for `const Foo@`, got {:?}",
                    other
                ),
            },
            other => panic!(
                "expected Handle(Const(_)) for `const Foo@`, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn parser_const_handle_ordering_trailing() {
        // AC20: `Foo@ const` → const handle to a mutable object.
        let (ty, errors) = parse_type("Foo@ const");
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match ty.kind {
            TypeExprKind::Const(inner) => match inner.kind {
                TypeExprKind::Handle(base) => {
                    assert!(matches!(base.kind, TypeExprKind::Named(_)));
                }
                other => panic!(
                    "expected Handle inside Const for `Foo@ const`, got {:?}",
                    other
                ),
            },
            other => panic!(
                "expected Const(Handle(_)) for `Foo@ const`, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn const_foo_at_distinct_from_foo_at_const() {
        // AC20: the two orderings must produce structurally different ASTs.
        let (lead, _) = parse_type("const Foo@");
        let (trail, _) = parse_type("Foo@ const");
        let lead_is_handle_of_const = matches!(
            &lead.kind,
            TypeExprKind::Handle(inner) if matches!(inner.kind, TypeExprKind::Const(_))
        );
        let trail_is_const_of_handle = matches!(
            &trail.kind,
            TypeExprKind::Const(inner) if matches!(inner.kind, TypeExprKind::Handle(_))
        );
        assert!(lead_is_handle_of_const, "lead form wrong: {:?}", lead.kind);
        assert!(
            trail_is_const_of_handle,
            "trail form wrong: {:?}",
            trail.kind
        );
    }

    #[test]
    fn test_type_array_shorthand() {
        let (ty, errors) = parse_type("int[]");
        assert!(errors.is_empty());
        match ty.kind {
            TypeExprKind::Array(inner) => {
                assert!(matches!(
                    inner.kind,
                    TypeExprKind::Primitive(TokenKind::KwInt)
                ));
            }
            _ => panic!("expected Array, got {:?}", ty.kind),
        }
    }

    #[test]
    fn test_type_generic_array() {
        let (ty, errors) = parse_type("array<CGameCtnBlock@>");
        assert!(errors.is_empty());
        match ty.kind {
            TypeExprKind::Array(inner) => match inner.kind {
                TypeExprKind::Handle(base) => {
                    assert!(matches!(base.kind, TypeExprKind::Named(_)));
                }
                _ => panic!("expected Handle inside Array, got {:?}", inner.kind),
            },
            _ => panic!("expected Array, got {:?}", ty.kind),
        }
    }

    #[test]
    fn test_type_qualified_name() {
        let source = "UI::InputBlocking";
        let (ty, errors) = parse_type(source);
        assert!(errors.is_empty());
        match ty.kind {
            TypeExprKind::Named(ref qname) => {
                assert_eq!(qname.segments.len(), 2);
                assert_eq!(qname.segments[0].text(source), "UI");
                assert_eq!(qname.segments[1].text(source), "InputBlocking");
            }
            _ => panic!("expected Named, got {:?}", ty.kind),
        }
    }

    #[test]
    fn test_type_template() {
        let source = "MwFastBuffer<wstring>";
        let (ty, errors) = parse_type(source);
        assert!(errors.is_empty());
        match ty.kind {
            TypeExprKind::Template(ref name, ref args) => {
                assert_eq!(name.segments.len(), 1);
                assert_eq!(name.segments[0].text(source), "MwFastBuffer");
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0].kind, TypeExprKind::Named(_)));
            }
            _ => panic!("expected Template, got {:?}", ty.kind),
        }
    }

    // ── Task 6: Declaration tests ────────────────────────────────────

    #[test]
    fn test_decl_enum() {
        let source = "enum WheelType { FL, FR, RL, RR }";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Enum(decl) => {
                assert_eq!(decl.name.text(source), "WheelType");
                assert_eq!(decl.values.len(), 4);
                assert_eq!(decl.values[0].name.text(source), "FL");
                assert_eq!(decl.values[1].name.text(source), "FR");
                assert_eq!(decl.values[2].name.text(source), "RL");
                assert_eq!(decl.values[3].name.text(source), "RR");
            }
            _ => panic!("expected Enum item"),
        }
    }

    #[test]
    fn test_decl_class() {
        let source = "class WheelState { float m_slipCoef; float m_dirt; }";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Class(decl) => {
                assert_eq!(decl.name.text(source), "WheelState");
                assert_eq!(decl.members.len(), 2);
                match &decl.members[0] {
                    ClassMember::Field(f) => {
                        assert_eq!(f.declarators[0].name.text(source), "m_slipCoef");
                    }
                    _ => panic!("expected Field"),
                }
                match &decl.members[1] {
                    ClassMember::Field(f) => {
                        assert_eq!(f.declarators[0].name.text(source), "m_dirt");
                    }
                    _ => panic!("expected Field"),
                }
            }
            _ => panic!("expected Class item"),
        }
    }

    #[test]
    fn test_decl_class_with_base() {
        let source = "class DashboardWheels : DashboardThing { }";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Class(decl) => {
                assert_eq!(decl.name.text(source), "DashboardWheels");
                assert_eq!(decl.base_classes.len(), 1);
                match &decl.base_classes[0].kind {
                    TypeExprKind::Named(qname) => {
                        assert_eq!(qname.segments[0].text(source), "DashboardThing");
                    }
                    _ => panic!("expected Named base class"),
                }
            }
            _ => panic!("expected Class item"),
        }
    }

    #[test]
    fn test_decl_namespace() {
        let source = r#"namespace AgentSettings { string S_Provider = "minimax"; }"#;
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Namespace(decl) => {
                assert_eq!(decl.name.text(source), "AgentSettings");
                assert_eq!(decl.items.len(), 1);
            }
            _ => panic!("expected Namespace item"),
        }
    }

    #[test]
    fn test_decl_funcdef() {
        let source = "funcdef void MsgHandler(Json::Value@);";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Funcdef(decl) => {
                assert_eq!(decl.name.text(source), "MsgHandler");
                assert_eq!(decl.params.len(), 1);
            }
            _ => panic!("expected Funcdef item"),
        }
    }

    #[test]
    fn test_decl_interface() {
        let source = "interface IRenderable { void Render(); }";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Interface(decl) => {
                assert_eq!(decl.name.text(source), "IRenderable");
                assert_eq!(decl.methods.len(), 1);
                assert_eq!(decl.methods[0].name.text(source), "Render");
            }
            _ => panic!("expected Interface item"),
        }
    }

    // ── Task 7: Function / variable tests ────────────────────────────

    #[test]
    fn test_func_void_main() {
        let source = "void Main() { }";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Function(decl) => {
                assert_eq!(decl.name.text(source), "Main");
                assert!(decl.body.is_some());
                assert!(decl.params.is_empty());
            }
            _ => panic!("expected Function item"),
        }
    }

    #[test]
    fn test_func_with_params() {
        let source = "UI::InputBlocking OnKeyPress(bool down, VirtualKey key) { return UI::InputBlocking::DoNothing; }";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::Function(decl) => {
                assert_eq!(decl.name.text(source), "OnKeyPress");
                assert_eq!(decl.params.len(), 2);
                assert_eq!(
                    decl.params[0].name.as_ref().unwrap().text(source),
                    "down"
                );
                assert_eq!(
                    decl.params[1].name.as_ref().unwrap().text(source),
                    "key"
                );
            }
            _ => panic!("expected Function item"),
        }
    }

    #[test]
    fn test_var_decl() {
        let source = "int g_Counter = 0;";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::VarDecl(decl) => {
                assert_eq!(decl.declarators.len(), 1);
                assert_eq!(decl.declarators[0].name.text(source), "g_Counter");
                assert!(decl.declarators[0].init.is_some());
            }
            _ => panic!("expected VarDecl item"),
        }
    }

    #[test]
    fn test_var_decl_const() {
        let source = "const string PluginIcon = Icons::Calculator;";
        let (file, errors) = parse_file(source);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(file.items.len(), 1);
        match &file.items[0] {
            Item::VarDecl(decl) => {
                assert_eq!(decl.declarators.len(), 1);
                assert_eq!(decl.declarators[0].name.text(source), "PluginIcon");
                assert!(decl.declarators[0].init.is_some());
                // The type should be const string
                assert!(matches!(decl.type_expr.kind, TypeExprKind::Const(_)));
            }
            _ => panic!("expected VarDecl item"),
        }
    }

    // ── Task 8: Expression parser tests ─────────────────────────────

    #[test]
    fn test_parse_binary_expr() {
        let src = "int x = 1 + 2 * 3;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_member_access() {
        let src = "auto x = app.Editor;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_cast() {
        let src = "auto app = cast<CTrackMania>(GetApp());";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_int() {
        let src = "int x = int(3.14);";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_string() {
        let src = "string s = string(resp[\"x\"]);";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_uint64() {
        let src = "uint64 n = uint64(x);";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_array() {
        let src = "auto a = array<int>(5);";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_dictionary_empty() {
        let src = "auto d = dictionary();";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_in_argument() {
        let src = "void f() { Print(int(x)); }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_type_construct_expression_stmt() {
        // Discard-result type-construction: `int(x);` as an expression statement.
        let src = "void f() { int(x); }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_anonymous_function_literal() {
        // Anonymous function passed as a callback argument.
        let src = "void main() { Call(function() { Print(\"hi\"); }); }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_anonymous_function_with_params() {
        let src = "void main() { Map(arr, function(int x) { return x * 2; }); }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_trailing_dot_float_param() {
        // Default parameter value with a trailing-dot float literal.
        let src = "void f(float radius = 25.) { }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_property_get_only() {
        let src = r#"
class X {
    int Foo { get { return 42; } }
}
"#;
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_property_get_set() {
        let src = r#"
class X {
    int _x;
    int Value {
        get { return _x; }
        set { _x = value; }
    }
}
"#;
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_property_handle_type() {
        let src = r#"
class X {
    CMwNod@ nod;
    CGameUserManagerScript@ userMgr { get { return cast<CGameUserManagerScript>(nod); } }
}
"#;
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_is_null() {
        let src = "bool b = app.Editor is null;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_not_is_null() {
        let src = "bool b = app.Editor !is null;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_ternary() {
        let src = "int x = a > b ? a : b;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_function_call_chain() {
        let src = "auto x = Meta::ExecutingPlugin().Name;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_string_concat() {
        let src = r#"string s = "hello " + "world";"#;
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    // ── Task 9: Statement, attribute, and error recovery tests ──────

    #[test]
    fn test_parse_if_else() {
        let src = "void f() { if (!down) return UI::InputBlocking::DoNothing; }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_for_loop() {
        let src = "void f() { for (int i = 0; i < 10; i++) { } }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_while_loop() {
        let src = "void f() { while (true) { yield(); } }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_switch() {
        let src = "void f() { switch (key) { case VirtualKey::A: break; default: break; } }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_setting_attribute() {
        let src = "[Setting hidden]\nbool S_IsActive = true;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        if let Item::VarDecl(v) = &file.items[0] {
            assert_eq!(v.attributes.len(), 1);
            let attr = &v.attributes[0];
            assert_eq!(attr.name.text(src), "Setting");
            assert_eq!(attr.args.len(), 1);
        }
    }

    #[test]
    fn test_parse_setting_with_key_value() {
        let src = r#"[Setting category="General" name="Force pad type"]
ForcePadType Setting_General_ForcePadType = ForcePadType::None;"#;
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        if let Item::VarDecl(v) = &file.items[0] {
            assert_eq!(v.attributes.len(), 1);
            let attr = &v.attributes[0];
            assert_eq!(attr.args.len(), 2);
        }
    }

    #[test]
    fn test_error_recovery_missing_semi() {
        let src = "int x = 1\nint y = 2;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let file = p.parse_file();
        assert!(!p.errors.is_empty());
        assert!(!file.items.is_empty());
    }

    #[test]
    fn test_parse_try_catch() {
        let src = "void f() { try { x(); } catch { } }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_real_main_function() {
        let src = r#"void Main() {
    startnew(OnLoop, 0);
}"#;
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_handle_assignment() {
        let src = "void f() { @editor = @GetApp().Editor; }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_is_type() {
        let src = "void f() { if (obj is CGameCtnChallenge) {} }";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let _file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
    }

    #[test]
    fn test_parse_precedence_mul_before_add() {
        // 1 + 2 * 3 should parse as Add(1, Mul(2, 3))
        let src = "int x = 1 + 2 * 3;";
        let tokens = tokenize_filtered(src);
        let mut p = Parser::new(&tokens, src);
        let file = p.parse_file();
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        // Verify structure: the init expr should be Binary(Add, 1, Binary(Mul, 2, 3))
        if let Item::VarDecl(v) = &file.items[0] {
            let init = v.declarators[0].init.as_ref().unwrap();
            if let ExprKind::Binary { op, rhs, .. } = &init.kind {
                assert_eq!(*op, BinOp::Add);
                assert!(matches!(&rhs.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
            } else {
                panic!("expected Binary expr, got {:?}", init.kind);
            }
        }
    }
}
