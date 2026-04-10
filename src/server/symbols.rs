use tower_lsp::lsp_types::*;

use crate::lexer;
use crate::parser::Parser;
use crate::parser::ast::Item;
use crate::server::diagnostics::span_to_range;

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
        Item::Class(c) => Some(DocumentSymbol {
            name: c.name.text(source).to_string(),
            detail: None,
            kind: SymbolKind::CLASS,
            range: span_to_range(source, c.span),
            selection_range: span_to_range(source, c.name.span),
            children: None,
            tags: None,
            deprecated: None,
        }),
        Item::Interface(i) => Some(DocumentSymbol {
            name: i.name.text(source).to_string(),
            detail: None,
            kind: SymbolKind::INTERFACE,
            range: span_to_range(source, i.span),
            selection_range: span_to_range(source, i.name.span),
            children: None,
            tags: None,
            deprecated: None,
        }),
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
