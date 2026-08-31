use std::collections::HashMap;

use crate::lexer::Span;

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
    pub file_id: usize,
    pub doc: Option<String>,
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
        /// B101: the trailing `const` qualifier on a class or interface
        /// METHOD (`void M() const`). The engine treats `void M() const`
        /// and `void M()` as different signatures — measured, B094's
        /// 18-fixture probe against OP 1.29.14 answered a non-const method
        /// offered against a const requirement with `Missing
        /// implementation of 'void I::M() const'`. Without this field the
        /// two are indistinguishable to every consumer of the pool.
        ///
        /// Threaded verbatim from `FunctionDecl::is_const`, so it says
        /// what the source said. A constructor and a destructor are
        /// synthesized without the qualifier and are always `false`; a
        /// free function cannot carry it in valid AngelScript, and the
        /// parser records whatever was written.
        is_const: bool,
    },
    Class {
        parents: Vec<String>,
        /// B111: the `mixin` modifier. A mixin class's members are COPIED
        /// into each class that includes it; the mixin is never
        /// instantiated and never itself owes the interface it names, so
        /// anything that applies a class-level REQUIREMENT has to be able
        /// to tell one apart. Flagging a mixin for not implementing its
        /// interface is a false ERROR on code that compiles — measured
        /// against two shipped plugins (tm-buffer-time,
        /// tm-cotd-buffer-time) in B101's corpus sweep.
        ///
        /// Threaded verbatim from `ClassDecl::is_mixin`. An interface
        /// cannot be a mixin, which is why `Interface` carries no such
        /// flag.
        is_mixin: bool,
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
