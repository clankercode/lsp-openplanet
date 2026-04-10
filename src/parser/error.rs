use crate::lexer::{Span, TokenKind};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub kind: ParseErrorKind,
}

#[derive(Debug, Clone)]
pub enum ParseErrorKind {
    Expected { expected: TokenKind, found: TokenKind },
    ExpectedIdent { found: TokenKind },
    ExpectedExpr { found: TokenKind },
    ExpectedType { found: TokenKind },
    ExpectedItem { found: TokenKind },
    UnexpectedToken(TokenKind),
    UnexpectedEof,
    Custom(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ParseErrorKind::Expected { expected, found } => {
                write!(f, "expected {:?}, found {:?}", expected, found)
            }
            ParseErrorKind::ExpectedIdent { found } => {
                write!(f, "expected identifier, found {:?}", found)
            }
            ParseErrorKind::ExpectedExpr { found } => {
                write!(f, "expected expression, found {:?}", found)
            }
            ParseErrorKind::ExpectedType { found } => {
                write!(f, "expected type, found {:?}", found)
            }
            ParseErrorKind::ExpectedItem { found } => {
                write!(f, "expected declaration, found {:?}", found)
            }
            ParseErrorKind::UnexpectedToken(kind) => {
                write!(f, "unexpected token {:?}", kind)
            }
            ParseErrorKind::UnexpectedEof => write!(f, "unexpected end of file"),
            ParseErrorKind::Custom(msg) => write!(f, "{}", msg),
        }
    }
}
