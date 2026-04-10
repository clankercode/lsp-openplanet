use crate::lexer::{tokenize_filtered, Span, Token, TokenKind};
use crate::parser::ast::*;
use crate::parser::error::{ParseError, ParseErrorKind};

// ── Parser struct ───────────────────────────────────────────────────────────

pub struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    pos: usize,
    pub errors: Vec<ParseError>,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token], source: &'a str) -> Self {
        Parser {
            tokens,
            source,
            pos: 0,
            errors: Vec::new(),
        }
    }

    // ── Core utility methods ────────────────────────────────────────────

    /// Peek at the current token kind without advancing.
    fn peek(&self) -> TokenKind {
        self.tokens
            .get(self.pos)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    /// Peek ahead by `n` tokens (0 = current).
    fn peek_ahead(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.pos + n)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }

    /// Get the span of the current token.
    fn current_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or_else(|| {
                // Point at the end of the source
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
        let tok = self.tokens[self.pos].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        tok
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

        // Leading const
        let is_const = self.eat(TokenKind::KwConst);

        let mut ty = self.parse_base_type()?;

        // Parse suffixes: @, &[in/out/inout], []
        loop {
            match self.peek() {
                TokenKind::At => {
                    self.advance();
                    let span = self.span_from(ty.span.start);
                    ty = TypeExpr {
                        span,
                        kind: TypeExprKind::Handle(Box::new(ty)),
                    };
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
                    self.expect(TokenKind::Gt)?;
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
                    self.expect(TokenKind::Gt)?;
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
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(err) => {
                    let span = err.span;
                    self.error(err);
                    items.push(Item::Error(span));
                    self.synchronize();
                }
            }
        }
        SourceFile { items }
    }

    /// Parse a single top-level item.
    fn parse_item(&mut self) -> Result<Item, ParseError> {
        // Collect attributes
        let attrs = self.parse_attributes()?;

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
            match self.parse_class_member(&name) {
                Ok(member) => members.push(member),
                Err(err) => {
                    self.error(err);
                    self.synchronize();
                }
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

        // Otherwise: parse type, then name, then determine if method or field
        let type_expr = self.parse_type_expr()?;
        let member_name = self.expect_ident()?;

        if self.at(TokenKind::LParen) {
            // It's a method
            let decl =
                self.parse_function_rest(attrs, type_expr, member_name, is_private, is_protected)?;
            Ok(ClassMember::Method(decl))
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
            match self.parse_interface_method() {
                Ok(method) => methods.push(method),
                Err(err) => {
                    self.error(err);
                    self.synchronize();
                }
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
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;

        let mut items = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_end() {
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(err) => {
                    let span = err.span;
                    self.error(err);
                    items.push(Item::Error(span));
                    self.synchronize();
                }
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
            self.expect(TokenKind::KwFrom)?;
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

    /// Parse attributes: `[Attr1, Attr2(arg)]`
    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, ParseError> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::LBracket) {
            self.advance(); // eat [
            while !self.at(TokenKind::RBracket) && !self.at_end() {
                let attr_start = self.current_span().start;
                let name = self.expect_ident()?;
                let mut args = Vec::new();
                if self.eat(TokenKind::LParen) {
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
                }
                let attr_span = self.span_from(attr_start);
                attrs.push(Attribute {
                    span: attr_span,
                    name,
                    args,
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket)?;
        }
        Ok(attrs)
    }

    /// Parse an attribute value.
    fn parse_attr_value(&mut self) -> Result<AttrValue, ParseError> {
        match self.peek() {
            TokenKind::StringLit => {
                let tok = self.advance();
                Ok(AttrValue::String(StringLiteral { span: tok.span }))
            }
            TokenKind::IntLit => {
                let tok = self.advance();
                let val: i64 = tok.span.text(self.source).parse().unwrap_or(0);
                Ok(AttrValue::Int(val))
            }
            TokenKind::FloatLit => {
                let tok = self.advance();
                let val: f64 = tok.span.text(self.source).parse().unwrap_or(0.0);
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
        let type_expr = self.parse_type_expr()?;
        let name = self.expect_ident()?;

        if self.at(TokenKind::LParen) {
            // Function declaration
            let decl = self.parse_function_rest(attrs, type_expr, name, false, false)?;
            Ok(Item::Function(decl))
        } else {
            // Variable declaration
            let decl = self.parse_var_decl_rest(attrs, type_expr, name)?;
            Ok(Item::VarDecl(decl))
        }
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

        // Optional modifiers after params
        let is_const = self.eat(TokenKind::KwConst);
        let is_override = self.eat(TokenKind::KwOverride);
        let is_final = self.eat(TokenKind::KwFinal);

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
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(err) => {
                    self.error(err);
                    // Skip to next ; or }
                    self.synchronize();
                }
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

        // Optional default value
        let default_value = if self.eat(TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        let span = self.span_from(start);
        Ok(Param {
            span,
            type_expr,
            name,
            default_value,
            modifier: ParamModifier::None,
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
        let init = if self.eat(TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        declarators.push(VarDeclarator {
            name: first_name,
            init,
        });

        // Additional declarators
        while self.eat(TokenKind::Comma) {
            let name = self.expect_ident()?;
            let init = if self.eat(TokenKind::Eq) {
                Some(self.parse_expr()?)
            } else {
                None
            };
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

    // ── Expression parsing (minimal stub) ───────────────────────────────

    /// Minimal expression parser — enough to handle simple expressions
    /// for variable initializers, enum values, and return statements.
    /// Full implementation comes in Task 8.
    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_assignment_expr()
    }

    fn parse_assignment_expr(&mut self) -> Result<Expr, ParseError> {
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

    fn parse_or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and_expr()?;
        while self.at(TokenKind::PipePipe) {
            self.advance();
            let rhs = self.parse_and_expr()?;
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

    fn parse_and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_comparison_expr()?;
        while self.at(TokenKind::AmpAmp) {
            self.advance();
            let rhs = self.parse_comparison_expr()?;
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

    fn parse_comparison_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_additive_expr()?;
        while matches!(
            self.peek(),
            TokenKind::EqEq
                | TokenKind::BangEq
                | TokenKind::Lt
                | TokenKind::Gt
                | TokenKind::LtEq
                | TokenKind::GtEq
        ) {
            let op = match self.advance().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::NotEq,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::GtEq => BinOp::GtEq,
                _ => unreachable!(),
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

    fn parse_multiplicative_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary_expr()?;
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
            let rhs = self.parse_unary_expr()?;
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

    fn parse_unary_expr(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
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
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        args.push(self.parse_expr()?);
                        while self.eat(TokenKind::Comma) {
                            args.push(self.parse_expr()?);
                        }
                    }
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
                TokenKind::ColonColon => {
                    // Namespace access: expr::member
                    // Only valid if expr is an ident
                    if self.peek_ahead(1) == TokenKind::Ident {
                        self.advance(); // eat ::
                        let member = self.expect_ident()?;

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
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
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
            TokenKind::Ident => {
                let tok = self.advance();
                Ok(Expr {
                    span: tok.span,
                    kind: ExprKind::Ident(Ident { span: tok.span }),
                })
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                Ok(expr)
            }
            other => Err(ParseError {
                span: self.current_span(),
                kind: ParseErrorKind::ExpectedExpr { found: other },
            }),
        }
    }

    // ── Statement parsing (minimal stub) ────────────────────────────────

    /// Minimal statement parser — handles return and expression statements.
    /// Full implementation comes in Task 9.
    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek() {
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
            TokenKind::Semi => {
                let tok = self.advance();
                Ok(Stmt {
                    span: tok.span,
                    kind: StmtKind::Empty,
                })
            }
            TokenKind::LBrace => {
                let start = self.current_span().start;
                self.advance();
                let mut stmts = Vec::new();
                while !self.at(TokenKind::RBrace) && !self.at_end() {
                    match self.parse_stmt() {
                        Ok(s) => stmts.push(s),
                        Err(e) => {
                            self.error(e);
                            self.synchronize();
                        }
                    }
                }
                self.expect(TokenKind::RBrace)?;
                let span = self.span_from(start);
                Ok(Stmt {
                    span,
                    kind: StmtKind::Block(stmts),
                })
            }
            _ => {
                // Try expression statement
                let start = self.current_span().start;
                let expr = self.parse_expr()?;
                self.expect(TokenKind::Semi)?;
                let span = self.span_from(start);
                Ok(Stmt {
                    span,
                    kind: StmtKind::Expr(expr),
                })
            }
        }
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

}
