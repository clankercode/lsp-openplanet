/// A half-open byte-offset span `[start, end)` inside a source string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Extract the substring this span refers to.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start as usize..self.end as usize]
    }
}

/// A single token produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, start: u32, end: u32) -> Self {
        Self {
            kind,
            span: Span::new(start, end),
        }
    }
}

/// All token kinds for OpenPlanet AngelScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // ── Literals ────────────────────────────────────────────────────────────
    IntLit,
    FloatLit,
    StringLit,
    HexLit,

    // ── Identifier ──────────────────────────────────────────────────────────
    Ident,

    // ── Type keywords ───────────────────────────────────────────────────────
    KwVoid,
    KwBool,
    KwInt,
    KwInt8,
    KwInt16,
    KwInt32,
    KwInt64,
    KwUint,
    KwUint8,
    KwUint16,
    KwUint32,
    KwUint64,
    KwFloat,
    KwDouble,
    KwString,
    KwAuto,

    // ── Declaration keywords ─────────────────────────────────────────────────
    KwClass,
    KwInterface,
    KwEnum,
    KwNamespace,
    KwFuncdef,
    KwMixin,
    KwShared,
    KwAbstract,
    KwImport,
    KwFrom,

    // ── Control flow keywords ────────────────────────────────────────────────
    KwIf,
    KwElse,
    KwFor,
    KwWhile,
    KwDo,
    KwSwitch,
    KwCase,
    KwDefault,
    KwBreak,
    KwContinue,
    KwReturn,
    KwTry,
    KwCatch,

    // ── Value / modifier keywords ────────────────────────────────────────────
    KwNull,
    KwTrue,
    KwFalse,
    KwConst,
    KwOverride,
    KwFinal,
    KwCast,
    KwIs,
    KwProperty,
    KwIn,
    KwOut,
    KwInout,
    KwGet,
    KwSet,
    KwPrivate,
    KwProtected,
    KwThis,
    KwSuper,
    KwArray,
    KwDictionary,

    // ── Operators ────────────────────────────────────────────────────────────
    Plus,       // +
    Minus,      // -
    Star,       // *
    Slash,      // /
    Percent,    // %
    Eq,         // =
    EqEq,       // ==
    BangEq,     // !=
    Lt,         // <
    Gt,         // >
    LtEq,       // <=
    GtEq,       // >=
    AmpAmp,     // &&
    PipePipe,   // ||
    Bang,       // !
    Amp,        // &
    Pipe,       // |
    Caret,      // ^
    Tilde,      // ~
    LtLt,       // <<
    GtGt,       // >>
    PlusEq,     // +=
    MinusEq,    // -=
    StarEq,     // *=
    SlashEq,    // /=
    PercentEq,  // %=
    AmpEq,      // &=
    PipeEq,     // |=
    CaretEq,    // ^=
    LtLtEq,    // <<=
    GtGtEq,    // >>=
    PlusPlus,   // ++
    MinusMinus, // --
    Question,   // ?

    // ── Punctuation ──────────────────────────────────────────────────────────
    LParen,     // (
    RParen,     // )
    LBrace,     // {
    RBrace,     // }
    LBracket,   // [
    RBracket,   // ]
    Semi,       // ;
    Comma,      // ,
    Dot,        // .
    ColonColon, // ::
    Colon,      // :
    At,         // @
    Hash,       // #

    // ── Trivia ───────────────────────────────────────────────────────────────
    LineComment,
    BlockComment,

    // ── Special ──────────────────────────────────────────────────────────────
    Eof,
    Error,
}

impl TokenKind {
    /// Returns true for tokens that are typically skipped (comments).
    pub fn is_trivia(self) -> bool {
        matches!(self, TokenKind::LineComment | TokenKind::BlockComment)
    }
}

/// Map an identifier text to its keyword `TokenKind`, or return `Ident`.
pub fn keyword_lookup(text: &str) -> TokenKind {
    match text {
        // Type keywords
        "void" => TokenKind::KwVoid,
        "bool" => TokenKind::KwBool,
        "int" => TokenKind::KwInt,
        "int8" => TokenKind::KwInt8,
        "int16" => TokenKind::KwInt16,
        "int32" => TokenKind::KwInt32,
        "int64" => TokenKind::KwInt64,
        "uint" => TokenKind::KwUint,
        "uint8" => TokenKind::KwUint8,
        "uint16" => TokenKind::KwUint16,
        "uint32" => TokenKind::KwUint32,
        "uint64" => TokenKind::KwUint64,
        "float" => TokenKind::KwFloat,
        "double" => TokenKind::KwDouble,
        "string" => TokenKind::KwString,
        "auto" => TokenKind::KwAuto,
        // Declaration keywords
        "class" => TokenKind::KwClass,
        "interface" => TokenKind::KwInterface,
        "enum" => TokenKind::KwEnum,
        "namespace" => TokenKind::KwNamespace,
        "funcdef" => TokenKind::KwFuncdef,
        "mixin" => TokenKind::KwMixin,
        "shared" => TokenKind::KwShared,
        "abstract" => TokenKind::KwAbstract,
        "import" => TokenKind::KwImport,
        "from" => TokenKind::KwFrom,
        // Control flow keywords
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "for" => TokenKind::KwFor,
        "while" => TokenKind::KwWhile,
        "do" => TokenKind::KwDo,
        "switch" => TokenKind::KwSwitch,
        "case" => TokenKind::KwCase,
        "default" => TokenKind::KwDefault,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "return" => TokenKind::KwReturn,
        "try" => TokenKind::KwTry,
        "catch" => TokenKind::KwCatch,
        // Value / modifier keywords
        "null" => TokenKind::KwNull,
        "true" => TokenKind::KwTrue,
        "false" => TokenKind::KwFalse,
        "const" => TokenKind::KwConst,
        "override" => TokenKind::KwOverride,
        "final" => TokenKind::KwFinal,
        "cast" => TokenKind::KwCast,
        "is" => TokenKind::KwIs,
        "property" => TokenKind::KwProperty,
        "in" => TokenKind::KwIn,
        "out" => TokenKind::KwOut,
        "inout" => TokenKind::KwInout,
        "get" => TokenKind::KwGet,
        "set" => TokenKind::KwSet,
        "private" => TokenKind::KwPrivate,
        "protected" => TokenKind::KwProtected,
        "this" => TokenKind::KwThis,
        "super" => TokenKind::KwSuper,
        "array" => TokenKind::KwArray,
        "dictionary" => TokenKind::KwDictionary,
        // Anything else is an identifier
        _ => TokenKind::Ident,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keywords() {
        assert_eq!(keyword_lookup("class"), TokenKind::KwClass);
        assert_eq!(keyword_lookup("int64"), TokenKind::KwInt64);
        assert_eq!(keyword_lookup("funcdef"), TokenKind::KwFuncdef);
        assert_eq!(keyword_lookup("myVar"), TokenKind::Ident);
        assert_eq!(keyword_lookup("void"), TokenKind::KwVoid);
        assert_eq!(keyword_lookup("is"), TokenKind::KwIs);
        assert_eq!(keyword_lookup("null"), TokenKind::KwNull);
        assert_eq!(keyword_lookup("true"), TokenKind::KwTrue);
        assert_eq!(keyword_lookup("false"), TokenKind::KwFalse);
    }

    #[test]
    fn test_span_text() {
        let source = "void Main() {}";
        let span = Span::new(5, 9);
        assert_eq!(span.text(source), "Main");

        let span2 = Span::new(0, 4);
        assert_eq!(span2.text(source), "void");

        let span3 = Span::new(9, 10);
        assert_eq!(span3.text(source), "(");
    }

    #[test]
    fn test_is_trivia() {
        assert!(TokenKind::LineComment.is_trivia());
        assert!(TokenKind::BlockComment.is_trivia());
        assert!(!TokenKind::Ident.is_trivia());
        assert!(!TokenKind::KwVoid.is_trivia());
        assert!(!TokenKind::Eof.is_trivia());
    }
}
