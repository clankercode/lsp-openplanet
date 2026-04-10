pub mod scanner;
pub mod token;

pub use scanner::Lexer;
pub use token::{Span, Token, TokenKind};

/// Tokenize `source`, returning all tokens including trivia (comments).
/// The last token is always `Eof`.
pub fn tokenize(source: &str) -> Vec<Token> {
    Lexer::new(source).tokenize()
}

/// Tokenize `source` and strip trivia tokens (line/block comments).
/// The last token is always `Eof`.
pub fn tokenize_filtered(source: &str) -> Vec<Token> {
    tokenize(source)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect()
}
