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
    Variable { type_name: String },
    Function { return_type: String, params: Vec<(String, String)> },
    Class { parent: Option<String>, members: Vec<String> },
    Interface { methods: Vec<String> },
    Enum { values: Vec<(String, Option<i64>)> },
    Namespace,
    Funcdef { return_type: String, params: Vec<(String, String)> },
    EnumValue { enum_name: String, value: Option<i64> },
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
