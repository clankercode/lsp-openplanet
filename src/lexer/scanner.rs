use super::token::{keyword_lookup, Token, TokenKind};

/// Hand-written lexer for OpenPlanet AngelScript.
pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            src: source.as_bytes(),
            pos: 0,
        }
    }

    fn len(&self) -> usize {
        self.src.len()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.src.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> u8 {
        let b = self.src[self.pos];
        self.pos += 1;
        b
    }

    fn skip_while(&mut self, pred: impl Fn(u8) -> bool) {
        while let Some(b) = self.peek() {
            if pred(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn make_token(&self, kind: TokenKind, start: usize) -> Token {
        Token::new(kind, start as u32, self.pos as u32)
    }

    /// Tokenize the entire source, producing a Vec of tokens ending with Eof.
    pub fn tokenize(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        tokens
    }

    fn next_token(&mut self) -> Token {
        // Skip whitespace
        self.skip_while(|b| b == b' ' || b == b'\t' || b == b'\r' || b == b'\n');

        if self.at_end() {
            let pos = self.pos as u32;
            return Token::new(TokenKind::Eof, pos, pos);
        }

        let start = self.pos;
        let b = self.bump();

        match b {
            // ── Line comment or / operators ──────────────────────────────────
            b'/' => {
                match self.peek() {
                    Some(b'/') => {
                        // Line comment: consume until end of line
                        self.pos += 1; // consume second /
                        self.skip_while(|b| b != b'\n');
                        self.make_token(TokenKind::LineComment, start)
                    }
                    Some(b'*') => {
                        // Block comment — no nesting in standard AS but we handle it
                        self.pos += 1; // consume *
                        self.scan_block_comment(start)
                    }
                    Some(b'=') => {
                        self.pos += 1;
                        self.make_token(TokenKind::SlashEq, start)
                    }
                    _ => self.make_token(TokenKind::Slash, start),
                }
            }

            // ── String literals ───────────────────────────────────────────────
            b'"' => self.scan_string(start, b'"'),
            b'\'' => self.scan_string(start, b'\''),

            // ── Number literals ───────────────────────────────────────────────
            b'0'..=b'9' => self.scan_number(start, b),

            // ── Dot — could be float (.5f) or just Dot ────────────────────────
            b'.' => {
                if self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    self.scan_float_after_dot(start)
                } else {
                    self.make_token(TokenKind::Dot, start)
                }
            }

            // ── Identifiers / keywords ────────────────────────────────────────
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                self.skip_while(|c| c.is_ascii_alphanumeric() || c == b'_');
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .expect("identifier is valid utf-8");
                let kind = keyword_lookup(text);
                self.make_token(kind, start)
            }

            // ── Single/multi-character operators ─────────────────────────────
            b'+' => match self.peek() {
                Some(b'+') => { self.pos += 1; self.make_token(TokenKind::PlusPlus, start) }
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::PlusEq, start) }
                _ => self.make_token(TokenKind::Plus, start),
            },
            b'-' => match self.peek() {
                Some(b'-') => { self.pos += 1; self.make_token(TokenKind::MinusMinus, start) }
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::MinusEq, start) }
                _ => self.make_token(TokenKind::Minus, start),
            },
            b'*' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::StarEq, start) }
                _ => self.make_token(TokenKind::Star, start),
            },
            b'%' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::PercentEq, start) }
                _ => self.make_token(TokenKind::Percent, start),
            },
            b'=' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::EqEq, start) }
                _ => self.make_token(TokenKind::Eq, start),
            },
            b'!' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::BangEq, start) }
                _ => self.make_token(TokenKind::Bang, start),
            },
            b'<' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::LtEq, start) }
                Some(b'<') => {
                    self.pos += 1;
                    if self.peek() == Some(b'=') {
                        self.pos += 1;
                        self.make_token(TokenKind::LtLtEq, start)
                    } else {
                        self.make_token(TokenKind::LtLt, start)
                    }
                }
                _ => self.make_token(TokenKind::Lt, start),
            },
            b'>' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::GtEq, start) }
                Some(b'>') => {
                    self.pos += 1;
                    if self.peek() == Some(b'=') {
                        self.pos += 1;
                        self.make_token(TokenKind::GtGtEq, start)
                    } else {
                        self.make_token(TokenKind::GtGt, start)
                    }
                }
                _ => self.make_token(TokenKind::Gt, start),
            },
            b'&' => match self.peek() {
                Some(b'&') => { self.pos += 1; self.make_token(TokenKind::AmpAmp, start) }
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::AmpEq, start) }
                _ => self.make_token(TokenKind::Amp, start),
            },
            b'|' => match self.peek() {
                Some(b'|') => { self.pos += 1; self.make_token(TokenKind::PipePipe, start) }
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::PipeEq, start) }
                _ => self.make_token(TokenKind::Pipe, start),
            },
            b'^' => match self.peek() {
                Some(b'=') => { self.pos += 1; self.make_token(TokenKind::CaretEq, start) }
                _ => self.make_token(TokenKind::Caret, start),
            },
            b'~' => self.make_token(TokenKind::Tilde, start),
            b'?' => self.make_token(TokenKind::Question, start),

            // ── Punctuation ───────────────────────────────────────────────────
            b'(' => self.make_token(TokenKind::LParen, start),
            b')' => self.make_token(TokenKind::RParen, start),
            b'{' => self.make_token(TokenKind::LBrace, start),
            b'}' => self.make_token(TokenKind::RBrace, start),
            b'[' => self.make_token(TokenKind::LBracket, start),
            b']' => self.make_token(TokenKind::RBracket, start),
            b';' => self.make_token(TokenKind::Semi, start),
            b',' => self.make_token(TokenKind::Comma, start),
            b'@' => self.make_token(TokenKind::At, start),
            b'#' => self.make_token(TokenKind::Hash, start),
            b':' => match self.peek() {
                Some(b':') => { self.pos += 1; self.make_token(TokenKind::ColonColon, start) }
                _ => self.make_token(TokenKind::Colon, start),
            },

            // ── Anything else is an error token ──────────────────────────────
            _ => self.make_token(TokenKind::Error, start),
        }
    }

    /// Scan a block comment `/* ... */`. `start` is at the `/`.
    /// We've already consumed `/*` before entering this function.
    fn scan_block_comment(&mut self, start: usize) -> Token {
        // Simple (non-nested) block comment scanning
        loop {
            match self.peek() {
                None => break, // unterminated — emit what we have
                Some(b'*') => {
                    self.pos += 1;
                    if self.peek() == Some(b'/') {
                        self.pos += 1;
                        break;
                    }
                }
                _ => { self.pos += 1; }
            }
        }
        self.make_token(TokenKind::BlockComment, start)
    }

    /// Scan a string literal starting after the opening quote `q`.
    fn scan_string(&mut self, start: usize, q: u8) -> Token {
        loop {
            match self.peek() {
                None => break, // unterminated string
                Some(b'\\') => {
                    self.pos += 1; // consume backslash
                    if !self.at_end() {
                        self.pos += 1; // consume escape char
                    }
                }
                Some(c) if c == q => {
                    self.pos += 1; // consume closing quote
                    break;
                }
                _ => { self.pos += 1; }
            }
        }
        self.make_token(TokenKind::StringLit, start)
    }

    /// Scan a number literal. `first` is the first digit already consumed.
    fn scan_number(&mut self, start: usize, first: u8) -> Token {
        // Check for hex literal: 0x...
        if first == b'0' && self.peek().map(|c| c == b'x' || c == b'X').unwrap_or(false) {
            self.pos += 1; // consume 'x'
            self.skip_while(|c| c.is_ascii_hexdigit());
            return self.make_token(TokenKind::HexLit, start);
        }

        // Consume remaining integer digits
        self.skip_while(|c| c.is_ascii_digit());

        // Check for float: decimal part
        let mut is_float = false;
        if self.peek() == Some(b'.') && self.peek2().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            is_float = true;
            self.pos += 1; // consume '.'
            self.skip_while(|c| c.is_ascii_digit());
        }

        // Scientific notation: e/E [+/-] digits
        if self.peek().map(|c| c == b'e' || c == b'E').unwrap_or(false) {
            is_float = true;
            self.pos += 1; // consume 'e'/'E'
            if self.peek().map(|c| c == b'+' || c == b'-').unwrap_or(false) {
                self.pos += 1;
            }
            self.skip_while(|c| c.is_ascii_digit());
        }

        // f/F suffix → float
        if self.peek().map(|c| c == b'f' || c == b'F').unwrap_or(false) {
            is_float = true;
            self.pos += 1;
        }

        if is_float {
            self.make_token(TokenKind::FloatLit, start)
        } else {
            self.make_token(TokenKind::IntLit, start)
        }
    }

    /// Scan a float that starts with `.` followed by digits (e.g. `.5f`).
    /// `start` is at the `.`.
    fn scan_float_after_dot(&mut self, start: usize) -> Token {
        self.skip_while(|c| c.is_ascii_digit());
        // Scientific notation
        if self.peek().map(|c| c == b'e' || c == b'E').unwrap_or(false) {
            self.pos += 1;
            if self.peek().map(|c| c == b'+' || c == b'-').unwrap_or(false) {
                self.pos += 1;
            }
            self.skip_while(|c| c.is_ascii_digit());
        }
        // f/F suffix
        if self.peek().map(|c| c == b'f' || c == b'F').unwrap_or(false) {
            self.pos += 1;
        }
        self.make_token(TokenKind::FloatLit, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{tokenize, tokenize_filtered, Span};

    fn kinds(tokens: &[Token]) -> Vec<TokenKind> {
        tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn test_simple_function() {
        let tokens = tokenize_filtered("void Main() { }");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::KwVoid,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_variable_with_setting() {
        let src = "[Setting hidden]\nbool S_IsActive = true;";
        let tokens = tokenize_filtered(src);
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::LBracket,
                TokenKind::Ident,   // Setting
                TokenKind::Ident,   // hidden
                TokenKind::RBracket,
                TokenKind::KwBool,
                TokenKind::Ident,   // S_IsActive
                TokenKind::Eq,
                TokenKind::KwTrue,
                TokenKind::Semi,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_namespace_access() {
        let tokens = tokenize_filtered("UI::InputBlocking");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident,
                TokenKind::ColonColon,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_cast_expression() {
        let tokens = tokenize_filtered("cast<CTrackMania>(GetApp())");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::KwCast,
                TokenKind::Lt,
                TokenKind::Ident,   // CTrackMania
                TokenKind::Gt,
                TokenKind::LParen,
                TokenKind::Ident,   // GetApp
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_preprocessor_hash() {
        let src = "#if TMNEXT\nint x;\n#endif";
        let tokens = tokenize_filtered(src);
        // # becomes Hash; "if" becomes KwIf; "TMNEXT" is Ident
        // "endif" is NOT a keyword, so it's Ident
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Hash,
                TokenKind::KwIf,
                TokenKind::Ident,   // TMNEXT
                TokenKind::KwInt,
                TokenKind::Ident,   // x
                TokenKind::Semi,
                TokenKind::Hash,
                TokenKind::Ident,   // endif
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_handle_type() {
        let tokens = tokenize_filtered("CGameEditorPluginMap@ GetEditor()");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident,   // CGameEditorPluginMap
                TokenKind::At,
                TokenKind::Ident,   // GetEditor
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_string_with_escapes() {
        let src = r#""hello \"world\"""#;
        let tokens = tokenize_filtered(src);
        assert_eq!(kinds(&tokens), vec![TokenKind::StringLit, TokenKind::Eof]);
    }

    #[test]
    fn test_float_literals() {
        let tokens = tokenize_filtered("0.9f 3.14 1e5 .5f");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::FloatLit,
                TokenKind::FloatLit,
                TokenKind::FloatLit,
                TokenKind::FloatLit,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_hex_literal() {
        let tokens = tokenize_filtered("0xFF00");
        assert_eq!(kinds(&tokens), vec![TokenKind::HexLit, TokenKind::Eof]);
    }

    #[test]
    fn test_comments_are_trivia() {
        let src = "// line comment\nint x; /* block */ int y;";
        let all = tokenize(src);
        let filtered = tokenize_filtered(src);

        // All tokens include trivia
        assert!(all.iter().any(|t| t.kind == TokenKind::LineComment));
        assert!(all.iter().any(|t| t.kind == TokenKind::BlockComment));

        // Filtered tokens have no trivia
        assert!(!filtered.iter().any(|t| t.kind.is_trivia()));

        // Filtered contains the meaningful tokens
        assert_eq!(
            kinds(&filtered),
            vec![
                TokenKind::KwInt,
                TokenKind::Ident,   // x
                TokenKind::Semi,
                TokenKind::KwInt,
                TokenKind::Ident,   // y
                TokenKind::Semi,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_is_and_not_is() {
        // "x is null" → Ident, KwIs, KwNull
        let tokens = tokenize_filtered("x is null");
        assert_eq!(
            kinds(&tokens),
            vec![TokenKind::Ident, TokenKind::KwIs, TokenKind::KwNull, TokenKind::Eof]
        );

        // "x !is null" → Ident, Bang, KwIs, KwNull  (parser combines these)
        let tokens2 = tokenize_filtered("x !is null");
        assert_eq!(
            kinds(&tokens2),
            vec![TokenKind::Ident, TokenKind::Bang, TokenKind::KwIs, TokenKind::KwNull, TokenKind::Eof]
        );
    }

    #[test]
    fn test_real_openplanet_snippet() {
        let src = r#"
namespace MyPlugin {
    int g_Counter = 0;

    void Main() {
        CTrackMania@ app = cast<CTrackMania>(GetApp());
        if (app is null) return;
        g_Counter++;
        string msg = "Hello \"World\"";
        float ratio = 0.5f;
    }
}
"#;
        let tokens = tokenize_filtered(src);
        let ks = kinds(&tokens);

        // Spot-check some key tokens exist in order
        let ns_idx = ks.iter().position(|&k| k == TokenKind::KwNamespace).expect("namespace");
        let void_idx = ks.iter().position(|&k| k == TokenKind::KwVoid).expect("void");
        let cast_idx = ks.iter().position(|&k| k == TokenKind::KwCast).expect("cast");
        let is_idx = ks.iter().position(|&k| k == TokenKind::KwIs).expect("is");
        let str_idx = ks.iter().position(|&k| k == TokenKind::StringLit).expect("string lit");
        let flt_idx = ks.iter().position(|&k| k == TokenKind::FloatLit).expect("float lit");

        assert!(ns_idx < void_idx);
        assert!(void_idx < cast_idx);
        assert!(cast_idx < is_idx);
        assert!(is_idx < str_idx);
        assert!(str_idx < flt_idx);

        // Ends with Eof
        assert_eq!(*ks.last().unwrap(), TokenKind::Eof);

        // No trivia in filtered output
        assert!(!ks.iter().any(|k| k.is_trivia()));

        // ColonColon present (from cast return type and maybe other uses)
        // Actually the snippet doesn't have ::, so let's check PlusPlus (g_Counter++)
        assert!(ks.contains(&TokenKind::PlusPlus));
    }

    #[test]
    fn test_span_positions() {
        let src = "void Main()";
        let tokens = tokenize_filtered(src);
        // "void" is at bytes 0..4
        assert_eq!(tokens[0].span, Span::new(0, 4));
        // "Main" is at bytes 5..9
        assert_eq!(tokens[1].span, Span::new(5, 9));
        // "(" is at byte 9..10
        assert_eq!(tokens[2].span, Span::new(9, 10));
    }

    #[test]
    fn test_compound_operators() {
        let tokens = tokenize_filtered("+= -= *= /= %= &= |= ^= <<= >>=");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::PlusEq,
                TokenKind::MinusEq,
                TokenKind::StarEq,
                TokenKind::SlashEq,
                TokenKind::PercentEq,
                TokenKind::AmpEq,
                TokenKind::PipeEq,
                TokenKind::CaretEq,
                TokenKind::LtLtEq,
                TokenKind::GtGtEq,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_int_literal() {
        let tokens = tokenize_filtered("42 0 100");
        assert_eq!(
            kinds(&tokens),
            vec![TokenKind::IntLit, TokenKind::IntLit, TokenKind::IntLit, TokenKind::Eof]
        );
    }

    #[test]
    fn test_single_quoted_string() {
        let tokens = tokenize_filtered("'hello'");
        assert_eq!(kinds(&tokens), vec![TokenKind::StringLit, TokenKind::Eof]);
    }
}
