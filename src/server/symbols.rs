use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::lexer;
use crate::parser::Parser;
use crate::parser::ast::{ClassMember, Item};
use crate::server::diagnostics::span_to_range;
use crate::symbols::SymbolTable;
use crate::symbols::scope::SymbolKind as InternalSymbolKind;

pub fn document_symbols(source: &str) -> Option<DocumentSymbolResponse> {
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file = parser.parse_file();

    let symbols: Vec<DocumentSymbol> = file
        .items
        .iter()
        .filter_map(|item| item_to_symbol(item, source))
        .collect();

    if symbols.is_empty() {
        None
    } else {
        Some(DocumentSymbolResponse::Nested(symbols))
    }
}

#[allow(deprecated)]
fn item_to_symbol(item: &Item, source: &str) -> Option<DocumentSymbol> {
    match item {
        Item::Function(f) => Some(DocumentSymbol {
            name: f.name.text(source).to_string(),
            detail: None,
            kind: SymbolKind::FUNCTION,
            range: span_to_range(source, f.span),
            selection_range: span_to_range(source, f.name.span),
            children: None,
            tags: None,
            deprecated: None,
        }),
        Item::Class(c) => {
            let mut children: Vec<DocumentSymbol> = Vec::new();
            for member in &c.members {
                match member {
                    ClassMember::Field(var) => {
                        for decl in &var.declarators {
                            children.push(DocumentSymbol {
                                name: decl.name.text(source).to_string(),
                                detail: None,
                                kind: SymbolKind::FIELD,
                                range: span_to_range(source, var.span),
                                selection_range: span_to_range(source, decl.name.span),
                                children: None,
                                tags: None,
                                deprecated: None,
                            });
                        }
                    }
                    ClassMember::Method(func) => {
                        children.push(DocumentSymbol {
                            name: func.name.text(source).to_string(),
                            detail: None,
                            kind: SymbolKind::METHOD,
                            range: span_to_range(source, func.span),
                            selection_range: span_to_range(source, func.name.span),
                            children: None,
                            tags: None,
                            deprecated: None,
                        });
                    }
                    ClassMember::Constructor(func) => {
                        children.push(DocumentSymbol {
                            name: func.name.text(source).to_string(),
                            detail: None,
                            kind: SymbolKind::CONSTRUCTOR,
                            range: span_to_range(source, func.span),
                            selection_range: span_to_range(source, func.name.span),
                            children: None,
                            tags: None,
                            deprecated: None,
                        });
                    }
                    ClassMember::Destructor(func) => {
                        children.push(DocumentSymbol {
                            name: func.name.text(source).to_string(),
                            detail: None,
                            kind: SymbolKind::METHOD,
                            range: span_to_range(source, func.span),
                            selection_range: span_to_range(source, func.name.span),
                            children: None,
                            tags: None,
                            deprecated: None,
                        });
                    }
                    ClassMember::Property(prop) => {
                        children.push(DocumentSymbol {
                            name: prop.name.text(source).to_string(),
                            detail: None,
                            kind: SymbolKind::PROPERTY,
                            range: span_to_range(source, prop.span),
                            selection_range: span_to_range(source, prop.name.span),
                            children: None,
                            tags: None,
                            deprecated: None,
                        });
                    }
                }
            }
            Some(DocumentSymbol {
                name: c.name.text(source).to_string(),
                detail: None,
                kind: SymbolKind::CLASS,
                range: span_to_range(source, c.span),
                selection_range: span_to_range(source, c.name.span),
                children: Some(children),
                tags: None,
                deprecated: None,
            })
        }
        Item::Interface(i) => {
            let children: Vec<DocumentSymbol> = i
                .methods
                .iter()
                .map(|func| DocumentSymbol {
                    name: func.name.text(source).to_string(),
                    detail: None,
                    kind: SymbolKind::METHOD,
                    range: span_to_range(source, func.span),
                    selection_range: span_to_range(source, func.name.span),
                    children: None,
                    tags: None,
                    deprecated: None,
                })
                .collect();
            Some(DocumentSymbol {
                name: i.name.text(source).to_string(),
                detail: None,
                kind: SymbolKind::INTERFACE,
                range: span_to_range(source, i.span),
                selection_range: span_to_range(source, i.name.span),
                children: Some(children),
                tags: None,
                deprecated: None,
            })
        }
        Item::Enum(e) => Some(DocumentSymbol {
            name: e.name.text(source).to_string(),
            detail: None,
            kind: SymbolKind::ENUM,
            range: span_to_range(source, e.span),
            selection_range: span_to_range(source, e.name.span),
            children: Some(
                e.values
                    .iter()
                    .map(|v| DocumentSymbol {
                        name: v.name.text(source).to_string(),
                        detail: None,
                        kind: SymbolKind::ENUM_MEMBER,
                        range: span_to_range(source, v.span),
                        selection_range: span_to_range(source, v.name.span),
                        children: None,
                        tags: None,
                        deprecated: None,
                    })
                    .collect(),
            ),
            tags: None,
            deprecated: None,
        }),
        Item::Namespace(ns) => Some(DocumentSymbol {
            name: ns.name.text(source).to_string(),
            detail: None,
            kind: SymbolKind::NAMESPACE,
            range: span_to_range(source, ns.span),
            selection_range: span_to_range(source, ns.name.span),
            children: Some(
                ns.items
                    .iter()
                    .filter_map(|i| item_to_symbol(i, source))
                    .collect(),
            ),
            tags: None,
            deprecated: None,
        }),
        Item::VarDecl(v) => {
            let (name, name_span) = v
                .declarators
                .first()
                .map(|d| (d.name.text(source).to_string(), d.name.span))
                .unwrap_or_else(|| ("?".to_string(), v.span));
            Some(DocumentSymbol {
                name,
                detail: None,
                kind: SymbolKind::VARIABLE,
                range: span_to_range(source, v.span),
                selection_range: span_to_range(source, name_span),
                children: None,
                tags: None,
                deprecated: None,
            })
        }
        _ => None,
    }
}

#[allow(deprecated)]
pub fn workspace_symbols(
    query: &str,
    workspace: &SymbolTable,
    file_uris: &HashMap<usize, (Url, String)>,
) -> Vec<SymbolInformation> {
    let q = query.to_lowercase();
    let mut results = Vec::new();
    for sym in workspace.all_symbols() {
        let bare = sym.name.rsplit("::").next().unwrap_or(&sym.name);
        if !q.is_empty() && !bare.to_lowercase().contains(&q) {
            continue;
        }
        if matches!(sym.kind, InternalSymbolKind::EnumValue { .. }) && q.is_empty() {
            continue;
        }
        let kind = match &sym.kind {
            InternalSymbolKind::Class { .. } => SymbolKind::CLASS,
            InternalSymbolKind::Interface { .. } => SymbolKind::INTERFACE,
            InternalSymbolKind::Enum { .. } => SymbolKind::ENUM,
            InternalSymbolKind::EnumValue { .. } => SymbolKind::ENUM_MEMBER,
            InternalSymbolKind::Function { .. } => SymbolKind::FUNCTION,
            InternalSymbolKind::Funcdef { .. } => SymbolKind::FUNCTION,
            InternalSymbolKind::Variable { .. } => SymbolKind::VARIABLE,
            InternalSymbolKind::Namespace => SymbolKind::NAMESPACE,
        };
        let Some((uri, source)) = file_uris.get(&sym.file_id) else {
            continue;
        };
        let range = span_to_range(source, sym.span);
        results.push(SymbolInformation {
            name: sym.name.clone(),
            kind,
            tags: None,
            deprecated: None,
            location: Location {
                uri: uri.clone(),
                range,
            },
            container_name: None,
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::SourceFile;

    fn parse(src: &str) -> SourceFile {
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        parser.parse_file()
    }

    fn doc_symbols(src: &str) -> Vec<DocumentSymbol> {
        match document_symbols(src) {
            Some(DocumentSymbolResponse::Nested(v)) => v,
            _ => Vec::new(),
        }
    }

    #[test]
    fn document_symbols_includes_class_members() {
        let src = "class C { int x; void m() {} }";
        let syms = doc_symbols(src);
        assert_eq!(syms.len(), 1);
        let c = &syms[0];
        assert_eq!(c.name, "C");
        assert_eq!(c.kind, SymbolKind::CLASS);
        let children = c.children.as_ref().expect("class should have children");
        assert_eq!(children.len(), 2, "children: {:?}", children);
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"m"));
        let x = children.iter().find(|c| c.name == "x").unwrap();
        assert_eq!(x.kind, SymbolKind::FIELD);
        let m = children.iter().find(|c| c.name == "m").unwrap();
        assert_eq!(m.kind, SymbolKind::METHOD);
    }

    #[test]
    fn document_symbols_includes_interface_methods() {
        let src = "interface I { void m(); }";
        let syms = doc_symbols(src);
        assert_eq!(syms.len(), 1);
        let i = &syms[0];
        assert_eq!(i.kind, SymbolKind::INTERFACE);
        let children = i.children.as_ref().expect("interface should have children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "m");
        assert_eq!(children[0].kind, SymbolKind::METHOD);
    }

    #[test]
    fn document_symbols_includes_class_constructor() {
        let src = "class C { C() {} }";
        let syms = doc_symbols(src);
        assert_eq!(syms.len(), 1);
        let c = &syms[0];
        let children = c.children.as_ref().expect("class should have children");
        assert!(
            children.iter().any(|c| c.kind == SymbolKind::CONSTRUCTOR),
            "expected at least one CONSTRUCTOR child, got {:?}",
            children
        );
    }

    fn build_workspace(
        sources: &[(&str, &str)],
    ) -> (SymbolTable, HashMap<usize, (Url, String)>) {
        let mut table = SymbolTable::new();
        let mut files: HashMap<usize, (Url, String)> = HashMap::new();
        for (name, src) in sources {
            let file = parse(src);
            let fid = table.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, src, &file);
            table.set_file_symbols(fid, syms);
            let uri = Url::parse(&format!("file:///tmp/{}", name)).unwrap();
            files.insert(fid, (uri, src.to_string()));
        }
        (table, files)
    }

    #[test]
    fn workspace_symbols_finds_class() {
        let (table, files) = build_workspace(&[("a.as", "class Hello {}")]);
        let results = workspace_symbols("hello", &table, &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Hello");
        assert_eq!(results[0].kind, SymbolKind::CLASS);
    }

    #[test]
    fn workspace_symbols_substring_match() {
        let (table, files) = build_workspace(&[("a.as", "class Hello {}")]);
        let results = workspace_symbols("el", &table, &files);
        assert!(
            results.iter().any(|s| s.name == "Hello"),
            "expected Hello in results: {:?}",
            results.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn workspace_symbols_empty_query_returns_all_top_level() {
        let (table, files) = build_workspace(&[(
            "a.as",
            "class Hello {} enum E { A, B } void f() {}",
        )]);
        let results = workspace_symbols("", &table, &files);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Hello"));
        assert!(names.contains(&"E"));
        assert!(names.contains(&"f"));
        // Enum values should be excluded for empty query
        assert!(!names.contains(&"E::A"));
        assert!(!names.contains(&"E::B"));
    }
}
