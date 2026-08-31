use std::collections::HashMap;

use crate::lexer::Span;

/// B114: what a declaration was WRITTEN as, as opposed to what it is.
///
/// `SymbolKind` records the latter — a `Function`, a `Class` — and every
/// AngelScript declaration modifier was being dropped on the way into the
/// pool. Each one then had to be re-derived from the AST by whoever wanted
/// it, and **an AST walk is per-DOCUMENT while the pool is the CROSS-FILE
/// surface**, so a modifier for a declaration in another file could not be
/// answered at all. That is the defect; three sidecars existed to work
/// around it.
///
/// These live on [`Symbol`] rather than inside `SymbolKind` deliberately.
/// `shared` alone applies to five different kinds, so per-variant fields
/// would duplicate it five times and re-state the very confusion this
/// fixes: a modifier is a property of the DECLARATION, not of the kind.
/// It also means the next modifier costs no construction-site churn.
///
/// Two consequences worth knowing:
///
/// * All-false is the default, and that is what a SYNTHESIZED symbol gets
///   — engine/typedb types, resolver placeholders. They are not
///   declarations in this workspace and nothing should read a modifier off
///   them. (`SymbolKind::Function::is_const` had the same property before
///   this struct existed.)
/// * A flat struct permits combinations the language does not, such as a
///   `mixin` variable. That is accepted: the alternative duplicates
///   `is_shared` across five variants, and an always-false bool is a
///   cheaper wrong than a fivefold repetition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclModifiers {
    /// `shared` — the cross-plugin entity mechanism, on a function, class,
    /// interface, enum or funcdef. B087 needed this and had to walk the
    /// AST for it.
    pub is_shared: bool,
    /// Trailing `const` on a method (`void M() const`). B101.
    pub is_const: bool,
    /// `mixin class` — members are copied into each including class, so it
    /// never owes the interface it names. B111.
    pub is_mixin: bool,
    /// `abstract class`.
    ///
    /// Carried for UNIFORMITY, and it is not the reason this exists: B094
    /// MEASURED that `abstract` does not exempt a class from implementing
    /// its interfaces (`an_abstract_class_is_not_exempt` pins it), so no
    /// consumer is wrong today for lacking it.
    pub is_abstract: bool,
    /// `override` on a method.
    pub is_override: bool,
    /// `final` on a method or class.
    pub is_final: bool,
    /// `private` on a member (or, per the parser, a file-scope
    /// declaration). NOTE: nothing reads visibility yet — completion has
    /// no filtering at all — so this is the enabling half of a
    /// user-visible feature, pooled here because that feature cannot be
    /// written without it.
    pub is_private: bool,
    /// `protected` on a member.
    pub is_protected: bool,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
    pub file_id: usize,
    pub doc: Option<String>,
    /// B114: how this declaration was written. Always
    /// `DeclModifiers::default()` for a symbol that is not a workspace
    /// declaration.
    pub modifiers: DeclModifiers,
}

#[derive(Debug, Clone)]
pub enum SymbolKind {
    Variable {
        type_name: String,
    },
    Function {
        return_type: String,
        params: Vec<(String, String)>,
        /// Number of parameters that have no default value. `max_args` is
        /// `params.len()`. For arity checking: `min_args..=params.len()`.
        min_args: usize,
        /// True when declared by an `import ... from` statement (an alias for
        /// a function defined elsewhere), false for real definitions.
        imported: bool,
    },
    Class {
        parents: Vec<String>,
    },
    /// An interface declaration. Its methods are pooled as their own
    /// `I::method` symbols, exactly as a class's members are (B054) — this
    /// variant carries only what a member symbol cannot: the declared base
    /// interfaces, so the inheritance walkers can cross an interface
    /// boundary. It used to carry a `methods: Vec<String>` that was
    /// hardcoded empty at the one construction site and read by nobody,
    /// which made interface members look supported when they were absent
    /// from the pool entirely.
    Interface {
        parents: Vec<String>,
    },
    Enum {
        values: Vec<(String, Option<i64>)>,
    },
    Namespace,
    Funcdef {
        return_type: String,
        params: Vec<(String, String)>,
        /// True for `typedef` aliases (stored under the Funcdef kind for
        /// the type surface). Tagged at extraction — shape is ambiguous
        /// now that real funcdefs carry signatures, and a typedef must
        /// never resolve as a callable signature (B027 review).
        is_typedef: bool,
    },
    EnumValue {
        enum_name: String,
        value: Option<i64>,
    },
}

#[derive(Debug)]
pub struct Scope {
    pub symbols: HashMap<String, Symbol>,
    pub parent: Option<usize>, // index into scope arena
}

impl Scope {
    pub fn new(parent: Option<usize>) -> Self {
        Self {
            symbols: HashMap::new(),
            parent,
        }
    }

    pub fn define(&mut self, name: String, symbol: Symbol) {
        self.symbols.insert(name, symbol);
    }

    pub fn lookup(&self, name: &str) -> Option<&Symbol> {
        self.symbols.get(name)
    }
}

impl Symbol {
    /// True when this symbol was declared by an `import ... from` statement —
    /// a local alias for a function whose real definition lives elsewhere
    /// (possibly outside the workspace). Navigation features prefer real
    /// definitions over these import-site declarations when both exist.
    pub fn is_import_alias(&self) -> bool {
        matches!(&self.kind, SymbolKind::Function { imported: true, .. })
    }
}
