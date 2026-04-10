use std::collections::HashMap;

use super::scope::{Symbol, SymbolKind};
use crate::parser::ast;

/// Per-file symbol contributions
#[derive(Debug, Default)]
pub struct FileSymbols {
    pub file_id: usize,
    pub symbols: Vec<Symbol>,
}

/// Workspace-wide symbol table
#[derive(Debug, Default)]
pub struct SymbolTable {
    /// file_id → file symbols
    files: HashMap<usize, FileSymbols>,
    /// name → list of symbols (cross-file)
    global_index: HashMap<String, Vec<(usize, usize)>>, // (file_id, symbol_index)
    next_file_id: usize,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allocate_file_id(&mut self) -> usize {
        let id = self.next_file_id;
        self.next_file_id += 1;
        id
    }

    /// Register symbols from a parsed file. Replaces any previous symbols for this file.
    pub fn set_file_symbols(&mut self, file_id: usize, symbols: Vec<Symbol>) {
        // Remove old entries from global index
        self.remove_file(file_id);

        // Add new entries
        for (idx, sym) in symbols.iter().enumerate() {
            self.global_index
                .entry(sym.name.clone())
                .or_default()
                .push((file_id, idx));
        }

        self.files.insert(file_id, FileSymbols { file_id, symbols });
    }

    pub fn remove_file(&mut self, file_id: usize) {
        if self.files.remove(&file_id).is_some() {
            for entries in self.global_index.values_mut() {
                entries.retain(|(fid, _)| *fid != file_id);
            }
        }
    }

    pub fn lookup(&self, name: &str) -> Vec<&Symbol> {
        self.global_index
            .get(name)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(file_id, idx)| {
                        self.files.get(file_id)?.symbols.get(*idx)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn all_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.files.values().flat_map(|f| f.symbols.iter())
    }

    /// Extract symbols from a parsed AST file
    pub fn extract_symbols(file_id: usize, source: &str, file: &ast::SourceFile) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        for item in &file.items {
            Self::extract_item_symbols(file_id, source, item, None, &mut symbols);
        }
        symbols
    }

    fn extract_item_symbols(
        file_id: usize,
        source: &str,
        item: &ast::Item,
        namespace: Option<&str>,
        out: &mut Vec<Symbol>,
    ) {
        let qualify = |name: &str| -> String {
            match namespace {
                Some(ns) => format!("{}::{}", ns, name),
                None => name.to_string(),
            }
        };

        match item {
            ast::Item::Class(cls) => {
                let name = qualify(cls.name.text(source));
                out.push(Symbol {
                    name,
                    kind: SymbolKind::Class {
                        parent: None, // resolve later
                        members: Vec::new(),
                    },
                    span: cls.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Interface(iface) => {
                let name = qualify(iface.name.text(source));
                out.push(Symbol {
                    name,
                    kind: SymbolKind::Interface { methods: Vec::new() },
                    span: iface.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Enum(en) => {
                let enum_name = qualify(en.name.text(source));
                out.push(Symbol {
                    name: enum_name.clone(),
                    kind: SymbolKind::Enum {
                        values: en.values.iter().map(|v| (v.name.text(source).to_string(), None)).collect(),
                    },
                    span: en.span,
                    file_id,
                    doc: None,
                });
                // Also register each enum value
                for val in &en.values {
                    out.push(Symbol {
                        name: format!("{}::{}", enum_name, val.name.text(source)),
                        kind: SymbolKind::EnumValue { enum_name: enum_name.clone(), value: None },
                        span: val.span,
                        file_id,
                        doc: None,
                    });
                }
            }
            ast::Item::Namespace(ns) => {
                let ns_name = qualify(ns.name.text(source));
                out.push(Symbol {
                    name: ns_name.clone(),
                    kind: SymbolKind::Namespace,
                    span: ns.span,
                    file_id,
                    doc: None,
                });
                for sub_item in &ns.items {
                    Self::extract_item_symbols(file_id, source, sub_item, Some(&ns_name), out);
                }
            }
            ast::Item::Funcdef(fd) => {
                let name = qualify(fd.name.text(source));
                out.push(Symbol {
                    name,
                    kind: SymbolKind::Funcdef {
                        return_type: String::new(),
                        params: Vec::new(),
                    },
                    span: fd.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Function(func) => {
                let name = qualify(func.name.text(source));
                out.push(Symbol {
                    name,
                    kind: SymbolKind::Function {
                        return_type: String::new(),
                        params: Vec::new(),
                    },
                    span: func.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::VarDecl(var) => {
                for decl in &var.declarators {
                    let name = qualify(decl.name.text(source));
                    out.push(Symbol {
                        name,
                        kind: SymbolKind::Variable { type_name: String::new() },
                        span: var.span,
                        file_id,
                        doc: None,
                    });
                }
            }
            ast::Item::Import(_) | ast::Item::Error(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{self, Span};
    use crate::parser::Parser;

    #[test]
    fn test_extract_symbols_from_enum() {
        let src = "enum WheelType { FL, FR, RL, RR }";
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"WheelType"));
        assert!(names.contains(&"WheelType::FL"));
    }

    #[test]
    fn test_extract_symbols_from_namespace() {
        let src = r#"namespace AgentSettings {
    string S_Provider = "minimax";
}"#;
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AgentSettings"));
        assert!(names.contains(&"AgentSettings::S_Provider"));
    }

    #[test]
    fn test_symbol_table_lookup() {
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        table.set_file_symbols(fid, vec![
            Symbol {
                name: "Main".to_string(),
                kind: SymbolKind::Function { return_type: "void".into(), params: vec![] },
                span: Span::new(0, 4),
                file_id: fid,
                doc: None,
            },
        ]);
        let results = table.lookup("Main");
        assert_eq!(results.len(), 1);
    }
}
