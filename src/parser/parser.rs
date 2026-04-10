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

/// Convenience: tokenize + parse a type expression from source text.
pub fn parse_type(source: &str) -> (TypeExpr, Vec<ParseError>) {
    let tokens = tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let ty = parser.parse_type_expr().expect("failed to parse type");
    (ty, parser.errors)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
                    assert!(matches!(base.kind, TypeExprKind::Primitive(TokenKind::KwString)));
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
                assert!(matches!(inner.kind, TypeExprKind::Primitive(TokenKind::KwInt)));
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
}
