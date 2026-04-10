//! Context-aware completion.
//!
//! Completion contexts detected (in priority order):
//!
//! 1. `#` at cursor → preprocessor directive keywords
//! 2. `::` at cursor → namespace member / enum value / nested type
//! 3. `.` at cursor → member completion (fields + methods + properties)
//!    for the expression immediately before the dot. The expression is
//!    resolved pragmatically: local variable in scope → class field in the
//!    enclosing class → workspace/external type name.
//! 4. Everything else → identifier position completion: keywords, locals in
//!    scope, enclosing class members, workspace globals, external top-level
//!    namespaces and functions.

use tower_lsp::lsp_types::*;

use crate::lexer;
use crate::parser::Parser;
use crate::parser::ast::{ClassMember, SourceFile};
use crate::server::diagnostics::position_to_offset;
use crate::server::scope_query;
use crate::symbols::scope::SymbolKind;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub fn complete(
    source: &str,
    position: Position,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Vec<CompletionItem> {
    let offset = position_to_offset(source, position);
    let prefix = &source[..offset];
    let trimmed = prefix.trim_end_matches(is_ident_char);

    // Preprocessor directive — highest priority, always wins.
    if trimmed.trim_end().ends_with('#') || prefix.ends_with('#') {
        return vec![
            make_item("if", CompletionItemKind::KEYWORD),
            make_item("elif", CompletionItemKind::KEYWORD),
            make_item("else", CompletionItemKind::KEYWORD),
            make_item("endif", CompletionItemKind::KEYWORD),
        ];
    }

    // Namespace/scope resolution completion (`::`).
    if let Some(ns) = extract_namespace_prefix(trimmed) {
        return complete_namespace_members(&ns, type_index);
    }

    // Member completion (`.`).
    if trimmed.ends_with('.') {
        return complete_dot_members(source, trimmed, offset, type_index, workspace);
    }

    // Default: identifier position.
    complete_identifier(source, offset, type_index, workspace)
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn extract_namespace_prefix(prefix: &str) -> Option<String> {
    if !prefix.ends_with("::") {
        return None;
    }
    let before = &prefix[..prefix.len() - 2];
    let start = before
        .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
        .map_or(0, |i| i + 1);
    let ns = &before[start..];
    if ns.is_empty() { None } else { Some(ns.to_string()) }
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

/// Extract the identifier immediately before the trailing `.` in `prefix`.
fn receiver_name_before_dot(prefix_with_dot: &str) -> Option<String> {
    let without_dot = prefix_with_dot.strip_suffix('.')?;
    let end = without_dot.len();
    let start = without_dot
        .rfind(|c: char| !is_ident_char(c))
        .map_or(0, |i| i + 1);
    if start >= end {
        return None;
    }
    Some(without_dot[start..end].to_string())
}

fn complete_dot_members(
    source: &str,
    prefix_with_dot: &str,
    offset_hint: usize,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Vec<CompletionItem> {
    let Some(receiver) = receiver_name_before_dot(prefix_with_dot) else {
        return Vec::new();
    };

    // Parse the file so we can resolve the receiver via scope queries.
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file: SourceFile = parser.parse_file();
    let offset = offset_hint as u32;

    // 1) Local variable in scope.
    let mut type_text: Option<String> =
        scope_query::local_type_at(source, &file, offset, &receiver);

    // 2) Field on the enclosing class.
    if type_text.is_none() {
        if let Some(cls) = scope_query::find_enclosing_class(&file, offset) {
            type_text = scope_query::class_member_type(cls, source, &receiver);
        }
    }

    let mut items = Vec::new();

    if let Some(tt) = type_text {
        if let Some(base) = scope_query::strip_to_base_type(&tt) {
            add_type_members(&base, type_index, workspace, &mut items);
        }
    } else {
        // The receiver might itself be a type name (e.g., `MyClass.` for
        // static lookups — not strictly valid AngelScript, but harmless).
        add_type_members(&receiver, type_index, workspace, &mut items);
    }

    items
}

/// Append field/method/property completion items for the given type name.
/// Tries the external index first, then walks workspace class definitions.
fn add_type_members(
    type_name: &str,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
    items: &mut Vec<CompletionItem>,
) {
    // Try external: walk the type and its parent chain.
    if let Some(index) = type_index {
        // Try direct name + short-name fallback.
        let qname = index
            .lookup_type(type_name)
            .map(|_| type_name.to_string())
            .or_else(|| {
                let hits = index.find_by_short_name(type_name);
                hits.first().cloned()
            });
        if let Some(name) = qname {
            let mut current = Some(name);
            let mut hops = 0;
            while let Some(n) = current.take() {
                hops += 1;
                if hops > 16 {
                    break;
                }
                if let Some(info) = index.lookup_type(&n) {
                    for p in &info.properties {
                        items.push(make_item(&p.name, CompletionItemKind::PROPERTY));
                    }
                    for m in &info.methods {
                        items.push(make_item(&m.name, CompletionItemKind::METHOD));
                    }
                    current = info.parent.clone();
                }
            }
        }
    }

    // Workspace fallback: look up the type's fields/methods via the global
    // symbol index ("TypeName::" prefix).
    if let Some(ws) = workspace {
        let prefix = format!("{}::", type_name);
        for sym in ws.all_symbols() {
            if let Some(rest) = sym.name.strip_prefix(&prefix) {
                if rest.contains("::") {
                    continue;
                }
                let kind = match &sym.kind {
                    SymbolKind::Function { .. } => CompletionItemKind::METHOD,
                    SymbolKind::Variable { .. } => CompletionItemKind::FIELD,
                    _ => CompletionItemKind::FIELD,
                };
                items.push(make_item(rest, kind));
            }
        }
    }
}

fn complete_identifier(
    source: &str,
    offset: usize,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Keywords.
    for kw in &[
        "void", "bool", "int", "uint", "float", "double", "string", "auto", "class", "interface",
        "enum", "namespace", "funcdef", "if", "else", "for", "while", "do", "switch", "case",
        "default", "break", "continue", "return", "try", "catch", "null", "true", "false",
        "const", "cast", "import",
    ] {
        items.push(make_item(kw, CompletionItemKind::KEYWORD));
    }

    // Parse for locals / enclosing class.
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file: SourceFile = parser.parse_file();
    let offset_u32 = offset as u32;

    // Locals in scope.
    for local in scope_query::find_locals_in_scope(source, &file, offset_u32) {
        items.push(make_item(&local.name, CompletionItemKind::VARIABLE));
    }

    // Members of enclosing class.
    if let Some(cls) = scope_query::find_enclosing_class(&file, offset_u32) {
        for m in &cls.members {
            match m {
                ClassMember::Field(vd) => {
                    for d in &vd.declarators {
                        items.push(make_item(
                            d.name.text(source),
                            CompletionItemKind::FIELD,
                        ));
                    }
                }
                ClassMember::Method(f) => {
                    items.push(make_item(
                        f.name.text(source),
                        CompletionItemKind::METHOD,
                    ));
                }
                ClassMember::Property(p) => {
                    items.push(make_item(
                        p.name.text(source),
                        CompletionItemKind::PROPERTY,
                    ));
                }
                _ => {}
            }
        }
    }

    // Workspace globals.
    if let Some(ws) = workspace {
        for sym in ws.all_symbols() {
            // Skip nested/qualified names — only offer top-level identifiers.
            if sym.name.contains("::") {
                continue;
            }
            let kind = match &sym.kind {
                SymbolKind::Function { .. } => CompletionItemKind::FUNCTION,
                SymbolKind::Class { .. } => CompletionItemKind::CLASS,
                SymbolKind::Interface { .. } => CompletionItemKind::INTERFACE,
                SymbolKind::Enum { .. } => CompletionItemKind::ENUM,
                SymbolKind::EnumValue { .. } => CompletionItemKind::ENUM_MEMBER,
                SymbolKind::Namespace => CompletionItemKind::MODULE,
                SymbolKind::Variable { .. } => CompletionItemKind::VARIABLE,
                SymbolKind::Funcdef { .. } => CompletionItemKind::INTERFACE,
            };
            items.push(make_item(&sym.name, kind));
        }
    }

    // External top-level namespaces.
    if let Some(index) = type_index {
        for ns in index.namespaces() {
            items.push(make_item(&ns, CompletionItemKind::MODULE));
        }
    }

    items
}

fn make_item(label: &str, kind: CompletionItemKind) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_from(source: &str) -> SymbolTable {
        let mut table = SymbolTable::new();
        let tokens = lexer::tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        table.set_file_symbols(fid, syms);
        table
    }

    fn pos_after(source: &str, marker: &str) -> Position {
        let byte = source
            .find(marker)
            .unwrap_or_else(|| panic!("marker {:?} not found", marker))
            + marker.len();
        let prefix = &source[..byte];
        let line = prefix.matches('\n').count() as u32;
        let col = prefix.rfind('\n').map_or(byte, |nl| byte - nl - 1) as u32;
        Position::new(line, col)
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn completion_offers_dot_members() {
        // A workspace class `C` with field `x` and method `m`. Cursor is
        // after `obj.` inside `f`.
        let src = "class C { int x; void m() {} } void f() { C@ obj; obj. }";
        let ws = ws_from(src);
        let pos = pos_after(src, "obj.");
        let items = complete(src, pos, None, Some(&ws));
        let labels = labels(&items);
        assert!(labels.contains(&"x"), "missing x in {:?}", labels);
        assert!(labels.contains(&"m"), "missing m in {:?}", labels);
    }

    #[test]
    fn completion_offers_in_scope_locals() {
        let src = "void f() { int abc = 5; zz }";
        // Cursor sits just after `zz`, well after the `abc` declaration.
        let pos = pos_after(src, "zz");
        let items = complete(src, pos, None, None);
        let labels = labels(&items);
        assert!(labels.contains(&"abc"), "missing abc in {:?}", labels);
    }

    #[test]
    fn completion_offers_workspace_functions() {
        let src = "void greet() {} void f() { gr }";
        let ws = ws_from(src);
        let pos = pos_after(src, "gr");
        let items = complete(src, pos, None, Some(&ws));
        let labels = labels(&items);
        assert!(labels.contains(&"greet"), "missing greet in {:?}", labels);
    }

    #[test]
    fn completion_namespace_still_works() {
        let src = "void f() { Foo:: }";
        // No type index — fall through to empty namespace member list. But
        // the important thing is we don't crash and we don't return the
        // generic identifier list.
        let pos = pos_after(src, "Foo::");
        let items = complete(src, pos, None, None);
        // No type index ⇒ empty namespace list, and we did NOT fall into
        // identifier completion (which would emit "void", "bool", ...).
        let labels = labels(&items);
        assert!(
            !labels.contains(&"void"),
            "should not fall through to identifier completion: {:?}",
            labels
        );
    }

    #[test]
    fn completion_preprocessor_directive() {
        let src = "#";
        let pos = pos_after(src, "#");
        let items = complete(src, pos, None, None);
        let labels = labels(&items);
        assert!(labels.contains(&"if"));
        assert!(labels.contains(&"endif"));
    }

    #[test]
    fn completion_offers_enclosing_class_members() {
        let src = "class C { int field; void m() {  } }";
        // Cursor inside method body.
        let pos = pos_after(src, "void m() { ");
        let items = complete(src, pos, None, None);
        let labels = labels(&items);
        assert!(
            labels.contains(&"field"),
            "missing field in {:?}",
            labels
        );
    }
}
