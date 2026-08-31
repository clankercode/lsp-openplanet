//! AST node types for Openplanet's AngelScript dialect.
//!
//! Every node carries a `Span` for source position mapping.
//! This is a pure data types file — no parsing logic.

use crate::lexer::{Span, TokenKind};

// ── Common ───────────────────────────────────────────────────────────────────

/// An identifier token, stored as a span into the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub span: Span,
}

impl Ident {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        self.span.text(source)
    }
}

/// A string literal token, stored as a span into the source (includes quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringLiteral {
    pub span: Span,
}

impl StringLiteral {
    /// Returns the string value with surrounding quotes stripped.
    pub fn value<'a>(&self, source: &'a str) -> &'a str {
        let raw = self.span.text(source);
        // Strip one leading and one trailing character (the quote chars).
        if raw.len() >= 2 {
            &raw[1..raw.len() - 1]
        } else {
            raw
        }
    }
}

/// A `::` separated qualified name, e.g. `Foo::Bar`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    pub span: Span,
    pub segments: Vec<Ident>,
}

impl QualifiedName {
    /// Construct a single-segment qualified name from an identifier.
    pub fn simple(ident: Ident) -> Self {
        let span = ident.span;
        QualifiedName {
            span,
            segments: vec![ident],
        }
    }

    /// Build a display string from the source text.
    pub fn to_string(&self, source: &str) -> String {
        self.segments
            .iter()
            .map(|s| s.text(source))
            .collect::<Vec<_>>()
            .join("::")
    }
}

// ── Attributes ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub span: Span,
    pub name: Ident,
    pub args: Vec<AttributeArg>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributeArg {
    pub span: Span,
    pub kind: AttributeArgKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttributeArgKind {
    Flag(Ident),
    KeyValue { key: Ident, value: AttrValue },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    String(StringLiteral),
    Int(i64),
    Float(f64),
    Ident(Ident),
}

// ── Type expressions ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub span: Span,
    pub kind: TypeExprKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExprKind {
    /// A primitive type keyword (e.g. `int`, `bool`, `void`).
    Primitive(TokenKind),
    /// A user-defined named type.
    Named(QualifiedName),
    /// A handle to another type: `T@`.
    Handle(Box<TypeExpr>),
    /// A reference to a type with an optional in/out modifier: `T&in`.
    Reference(Box<TypeExpr>, ParamModifier),
    /// An array of a type: `T[]`.
    Array(Box<TypeExpr>),
    /// A template instantiation: `array<T>`.
    Template(QualifiedName, Vec<TypeExpr>),
    /// A const-qualified type.
    Const(Box<TypeExpr>),
    Auto,
    Error,
}

// ── Parameter modifier ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamModifier {
    None,
    In,
    Out,
    Inout,
}

// ── Expressions ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

/// A single call-site argument. AngelScript allows named arguments
/// (`name: value`); when present, `name` is the parameter identifier.
#[derive(Debug, Clone, PartialEq)]
pub struct CallArg {
    /// Span covering `name: value` when named, otherwise just `value`.
    pub span: Span,
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    IntLit(i64),
    FloatLit(f64),
    StringLit,
    HexLit(u64),
    BoolLit(bool),
    Null,
    This,
    Super,
    Ident(Ident),
    Binary {
        lhs: Box<Expr>,
        op: BinOp,
        rhs: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Postfix {
        expr: Box<Expr>,
        op: UnaryOp,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
    },
    Member {
        object: Box<Expr>,
        member: Ident,
    },
    NamespaceAccess {
        path: QualifiedName,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Cast {
        target_type: TypeExpr,
        expr: Box<Expr>,
    },
    /// Type-construction expression: `int(x)`, `uint64(n)`, `string(x)`,
    /// `array<int>(size)`, `dictionary()`. AngelScript allows calling a type
    /// like a function for conversion/construction.
    TypeConstruct {
        target_type: TypeExpr,
        args: Vec<Expr>,
    },
    Is {
        expr: Box<Expr>,
        target: IsTarget,
        negated: bool,
    },
    Ternary {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Assign {
        lhs: Box<Expr>,
        op: AssignOp,
        rhs: Box<Expr>,
    },
    HandleAssign {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    ArrayInit(Vec<Expr>),
    Lambda {
        params: Vec<Param>,
        body: FunctionBody,
    },
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IsTarget {
    Null,
    Type(TypeExpr),
    /// Arbitrary right-hand-side expression: `a is b`, `a is this`,
    /// `a is g_Game.currMS`. AngelScript's `is` does handle equality,
    /// so any handle expression is valid on the RHS.
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// Power: `a ** b`
    Pow,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    /// Logical XOR `^^`
    Xor,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
    Inc,
    Dec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShlAssign,
    ShrAssign,
}

// ── Statements ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Expr(Expr),
    VarDecl(VarDeclStmt),
    Block(Vec<Stmt>),
    If {
        condition: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    For {
        init: Option<Box<Stmt>>,
        condition: Option<Expr>,
        step: Vec<Expr>,
        body: Box<Stmt>,
    },
    While {
        condition: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        condition: Expr,
    },
    Switch {
        expr: Expr,
        cases: Vec<SwitchCase>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    TryCatch {
        try_body: Box<Stmt>,
        catch_body: Box<Stmt>,
    },
    Empty,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VarDeclStmt {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub type_expr: TypeExpr,
    pub declarators: Vec<VarDeclarator>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VarDeclarator {
    pub name: Ident,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SwitchCase {
    pub span: Span,
    pub label: SwitchLabel,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SwitchLabel {
    Case(Expr),
    Default,
}

// ── Function / body ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionBody {
    pub span: Span,
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub span: Span,
    pub type_expr: TypeExpr,
    pub name: Option<Ident>,
    pub default_value: Option<Expr>,
    pub modifier: ParamModifier,
}

// ── Declarations ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDecl {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub return_type: TypeExpr,
    pub name: Ident,
    pub params: Vec<Param>,
    /// B087: the `shared` modifier — a cross-plugin export root. Only ever
    /// true for a file-scope function; `shared` is not a member modifier.
    /// Evidence for the forms this flag can carry: see `EnumDecl::is_shared`.
    pub is_shared: bool,
    pub is_const: bool,
    pub is_override: bool,
    pub is_final: bool,
    pub is_private: bool,
    pub is_protected: bool,
    pub body: Option<FunctionBody>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncdefDecl {
    pub span: Span,
    pub return_type: TypeExpr,
    pub name: Ident,
    pub params: Vec<Param>,
    /// B087: the `shared` modifier — a cross-plugin export root.
    /// Evidence for the forms this flag can carry: see `EnumDecl::is_shared`.
    pub is_shared: bool,
}

/// A type alias declaration: `typedef ExistingType Alias;`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedefDecl {
    pub span: Span,
    pub target_type: TypeExpr,
    pub name: Ident,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyDecl {
    pub span: Span,
    pub type_expr: TypeExpr,
    pub name: Ident,
    pub getter: Option<FunctionBody>,
    pub setter: Option<(Ident, FunctionBody)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClassMember {
    Field(VarDeclStmt),
    Method(FunctionDecl),
    Constructor(FunctionDecl),
    Destructor(FunctionDecl),
    Property(PropertyDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassDecl {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub is_shared: bool,
    pub is_mixin: bool,
    pub is_abstract: bool,
    pub name: Ident,
    pub base_classes: Vec<TypeExpr>,
    pub members: Vec<ClassMember>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDecl {
    pub span: Span,
    pub name: Ident,
    pub bases: Vec<TypeExpr>,
    pub methods: Vec<FunctionDecl>,
    /// B087: the `shared` modifier — a cross-plugin export root.
    /// Evidence for the forms this flag can carry: see `EnumDecl::is_shared`.
    pub is_shared: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumValue {
    pub span: Span,
    pub name: Ident,
    pub value: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub span: Span,
    pub name: Ident,
    pub values: Vec<EnumValue>,
    /// B087: the `shared` modifier — a cross-plugin export root.
    ///
    /// WHAT THE PRESENCE OF THIS FLAG DOES AND DOES NOT CLAIM. Recording the
    /// modifier is not a statement that the engine accepts every form that
    /// can carry it. `parse_item` ate `shared` and discarded it for
    /// everything but `ClassDecl` long before B087, so all of these already
    /// PARSED; B087 only stopped throwing the bit away. Nothing here widens
    /// what the parser accepts.
    ///
    /// Evidence, stated per form so the next reader does not have to infer
    /// it — each is a declaration inside Editor++ (siteid 377), a published
    /// plugin that compiles and runs, which is stronger than a synthetic
    /// probe: a probe's ACCEPT can be supplied by another installed plugin
    /// (see docs/probes/README.md), whereas a shipped plugin's own
    /// declaration cannot be.
    ///
    /// * `shared class`     — `Exports/Item_MovingItems_Shared.as:73`
    /// * `shared interface` — `Editor/Shared_General.as:45`
    /// * `shared enum`      — `Editor.as:574`
    /// * `shared funcdef`   — `Components/common/UI_Components.as:63`
    /// * `shared` function  — `FormatX.as:2`
    /// * `shared` VARIABLE  — REJECTED by the game, and the parser rejects it
    ///   too; game-probed, see `parse_func_or_var_item`.
    ///
    /// NOT MEASURED: `shared abstract class` / `shared mixin class`. The
    /// composition has no witness in any fixture and was not probed — the
    /// RemoteBuild harness could not run (see below). It is covered by a unit
    /// test only, and rooting a form the engine might reject costs a missed
    /// warning, never a false positive.
    ///
    /// Why it was not probed, 2026-08-31 (B097): RemoteBuild on OP 1.29.14 splits any
    /// request payload at ~64 bytes and handles each chunk as a separate
    /// request, so `load_plugin` — whose SHORTEST possible payload is 73
    /// bytes — cannot be delivered at all. Every probe path in
    /// `scripts/issue_repro_game.py` and `scripts/probe_enum_corpus.py` goes
    /// through it.
    pub is_shared: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceDecl {
    pub span: Span,
    pub name: Ident,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportDecl {
    pub span: Span,
    pub what: ImportTarget,
    pub from: Option<StringLiteral>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportTarget {
    Function {
        return_type: TypeExpr,
        name: Ident,
        params: Vec<Param>,
    },
    Module {
        path: StringLiteral,
        alias: Option<Ident>,
    },
}

// ── Top-level ────────────────────────────────────────────────────────────────

/// A top-level item in a source file.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Class(ClassDecl),
    Interface(InterfaceDecl),
    Enum(EnumDecl),
    Namespace(NamespaceDecl),
    Funcdef(FuncdefDecl),
    Typedef(TypedefDecl),
    Function(FunctionDecl),
    VarDecl(VarDeclStmt),
    Property(PropertyDecl),
    Import(ImportDecl),
    Error(Span),
}

/// The root of the AST: a parsed source file.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub items: Vec<Item>,
}
