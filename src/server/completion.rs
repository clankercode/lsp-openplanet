use tower_lsp::lsp_types::*;

use crate::server::diagnostics::position_to_offset;
use crate::typedb::TypeIndex;

pub fn complete(
    source: &str,
    position: Position,
    type_index: Option<&TypeIndex>,
) -> Vec<CompletionItem> {
    let offset = position_to_offset(source, position);
    let prefix = &source[..offset];

    // Determine completion context from the text before cursor
    if let Some(ns) = extract_namespace_prefix(prefix) {
        // After "::" — complete namespace members
        return complete_namespace_members(&ns, type_index);
    }

    if prefix.ends_with('.') {
        // After "." — would need type resolution for member completion
        // Placeholder: return empty for now, needs symbol table integration
        return Vec::new();
    }

    if prefix.ends_with('#') {
        // Preprocessor directive completion
        return vec![
            make_item("if", CompletionItemKind::KEYWORD),
            make_item("elif", CompletionItemKind::KEYWORD),
            make_item("else", CompletionItemKind::KEYWORD),
            make_item("endif", CompletionItemKind::KEYWORD),
        ];
    }

    // Top-level: keywords + global types + global functions
    let mut items = Vec::new();

    // AngelScript keywords
    for kw in &[
        "void", "bool", "int", "uint", "float", "double", "string", "auto",
        "class", "interface", "enum", "namespace", "funcdef",
        "if", "else", "for", "while", "do", "switch", "case", "default",
        "break", "continue", "return", "try", "catch",
        "null", "true", "false", "const", "cast", "import",
    ] {
        items.push(make_item(kw, CompletionItemKind::KEYWORD));
    }

    // Namespace names from type DB
    if let Some(index) = type_index {
        for ns in index.namespaces() {
            items.push(make_item(&ns, CompletionItemKind::MODULE));
        }
    }

    items
}

fn extract_namespace_prefix(prefix: &str) -> Option<String> {
    // Look for "Namespace::" at the end of prefix
    if prefix.ends_with("::") {
        let before = prefix.trim_end_matches(':');
        let start = before
            .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
            .map_or(0, |i| i + 1);
        let ns = &before[start..];
        if !ns.is_empty() {
            return Some(ns.to_string());
        }
    }
    None
}

fn complete_namespace_members(
    namespace: &str,
    type_index: Option<&TypeIndex>,
) -> Vec<CompletionItem> {
    let Some(index) = type_index else { return Vec::new() };
    index
        .namespace_members(namespace)
        .into_iter()
        .map(|name| make_item(&name, CompletionItemKind::FUNCTION))
        .collect()
}

fn make_item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..Default::default()
    }
}
