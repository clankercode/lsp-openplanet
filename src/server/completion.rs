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

use crate::parser::ast::{ClassMember, SourceFile};
use crate::server::diagnostics::position_to_offset;
use crate::server::scope_query;
use crate::symbols::scope::SymbolKind;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub fn complete(
    analysis: &crate::analysis::DocumentAnalysis,
    checked: Option<&crate::analysis_snapshot::CheckedFile<'_>>,
    position: Position,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
    shortcut_namespaces: &[String],
    shortcut_match_enums: bool,
) -> Vec<CompletionItem> {
    let source = analysis.masked_source();
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
        return complete_dot_members(analysis, checked, trimmed, offset, type_index, workspace);
    }

    // Default: identifier position.
    complete_identifier(
        analysis,
        offset,
        type_index,
        workspace,
        shortcut_namespaces,
        shortcut_match_enums,
    )
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
    if ns.is_empty() {
        None
    } else {
        Some(ns.to_string())
    }
}

fn complete_namespace_members(
    namespace: &str,
    type_index: Option<&TypeIndex>,
) -> Vec<CompletionItem> {
    let Some(index) = type_index else {
        return Vec::new();
    };
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
    analysis: &crate::analysis::DocumentAnalysis,
    checked: Option<&crate::analysis_snapshot::CheckedFile<'_>>,
    prefix_with_dot: &str,
    offset_hint: usize,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Vec<CompletionItem> {
    let source = analysis.masked_source();
    let Some(receiver) = receiver_name_before_dot(prefix_with_dot) else {
        return Vec::new();
    };

    // Use the caller's analysis (snapshot parse) for scope queries.
    let file: &SourceFile = &analysis.file;
    let offset = offset_hint as u32;

    // 0) Recorded receiver type (GH #42): the checker already resolved
    // the receiver expression — query its span→type map before falling
    // back to the scope_query approximations.
    let mut type_text: Option<String> = None;
    if let Some(checked) = checked {
        // The receiver's last byte sits just before the `.` (which is at
        // `prefix_with_dot.len() - 1`).
        let receiver_last = prefix_with_dot.len().saturating_sub(2) as u32;
        if let Some(ty) = checked.type_at_offset(receiver_last) {
            type_text = Some(ty.display());
        }
    }

    // 1) Local variable in scope.
    if type_text.is_none() {
        type_text = scope_query::local_type_at(source, &file, offset, &receiver);
    }

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
    analysis: &crate::analysis::DocumentAnalysis,
    offset: usize,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
    shortcut_namespaces: &[String],
    shortcut_match_enums: bool,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Keywords.
    for kw in &[
        "void",
        "bool",
        "int",
        "uint",
        "float",
        "double",
        "string",
        "auto",
        "class",
        "interface",
        "enum",
        "namespace",
        "funcdef",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "default",
        "break",
        "continue",
        "return",
        "try",
        "catch",
        "null",
        "true",
        "false",
        "const",
        "cast",
        "import",
    ] {
        items.push(make_item(kw, CompletionItemKind::KEYWORD));
    }

    // Use the snapshot's parse for locals / enclosing class.
    let source = analysis.masked_source();
    let file: &SourceFile = &analysis.file;
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
                        items.push(make_item(d.name.text(source), CompletionItemKind::FIELD));
                    }
                }
                ClassMember::Method(f) => {
                    items.push(make_item(f.name.text(source), CompletionItemKind::METHOD));
                }
                ClassMember::Property(p) => {
                    items.push(make_item(p.name.text(source), CompletionItemKind::PROPERTY));
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

    // Namespace-shortcut completions (old vscode-openplanet-angelscript
    // parity): functions (and optionally enums/enum-values) from a small
    // configured set of namespaces appear at identifier positions, labelled
    // fully qualified (`UI::Begin`) but filtered by the bare member name
    // (`filterText = "Begin"`), so typing `Beg` surfaces `UI::Begin`.
    // Sorted after everything else (`sortText = "z"`).
    if let Some(index) = type_index {
        if !shortcut_namespaces.is_empty() {
            let source = analysis.masked_source();
            let start = source[..offset]
                .rfind(|c: char| !is_ident_char(c))
                .map_or(0, |i| i + 1);
            let typed_prefix = source[start..offset].to_lowercase();
            add_shortcut_completions(
                &mut items,
                index,
                shortcut_namespaces,
                shortcut_match_enums,
                &typed_prefix,
            );
        }
    }

    items
}

/// Append namespace-shortcut completions for the configured namespaces.
/// Members whose name does not start with `typed_prefix` (case-insensitive)
/// are skipped; labels already present in `items` are not duplicated.
fn add_shortcut_completions(
    items: &mut Vec<CompletionItem>,
    index: &TypeIndex,
    namespaces: &[String],
    match_enums: bool,
    typed_prefix: &str,
) {
    let existing: std::collections::HashSet<String> =
        items.iter().map(|i| i.label.clone()).collect();
    let mut seen = std::collections::HashSet::new();

    for ns in namespaces {
        for (qname, overloads) in index.namespace_functions(ns) {
            let Some(short) = qname.strip_prefix(&format!("{}::", ns)) else {
                continue;
            };
            if !short.to_lowercase().starts_with(typed_prefix) {
                continue;
            }
            if existing.contains(qname) || !seen.insert(qname.clone()) {
                continue;
            }
            let first = &overloads[0];
            items.push(CompletionItem {
                label: qname.clone(),
                kind: Some(CompletionItemKind::METHOD),
                filter_text: Some(short.to_string()),
                sort_text: Some("z".to_string()),
                detail: Some(shortcut_detail(first)),
                ..Default::default()
            });
        }

        if match_enums {
            for (qname, info) in index.namespace_enums(ns) {
                let Some(short) = qname.strip_prefix(&format!("{}::", ns)) else {
                    continue;
                };
                if short.to_lowercase().starts_with(typed_prefix)
                    && !existing.contains(qname)
                    && seen.insert(qname.clone())
                {
                    items.push(CompletionItem {
                        label: qname.clone(),
                        kind: Some(CompletionItemKind::ENUM),
                        filter_text: Some(short.to_string()),
                        sort_text: Some("z".to_string()),
                        ..Default::default()
                    });
                }
                for (value, _) in &info.values {
                    if !value.to_lowercase().starts_with(typed_prefix) {
                        continue;
                    }
                    let label = format!("{}::{}", qname, value);
                    if existing.contains(&label) || !seen.insert(label.clone()) {
                        continue;
                    }
                    items.push(CompletionItem {
                        label,
                        kind: Some(CompletionItemKind::ENUM_MEMBER),
                        filter_text: Some(value.clone()),
                        sort_text: Some("z".to_string()),
                        ..Default::default()
                    });
                }
            }
        }
    }
}

/// Compact signature for the shortcut item's `detail` (e.g.
/// `bool Begin(string title, bool& open)`).
fn shortcut_detail(f: &crate::typedb::index::FunctionInfo) -> String {
    let params = f
        .params
        .iter()
        .map(|p| match &p.name {
            Some(name) => format!("{} {}", p.type_name, name),
            None => p.type_name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} {}({})", f.return_type, f.name, params)
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
    use crate::server::test_support::TestWorkspace;

    fn ws_from(source: &str) -> TestWorkspace {
        TestWorkspace::one_file("compl.as", source)
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
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let items = complete(
            &analysis,
            None,
            pos,
            None,
            Some(ws.snapshot.symbols()),
            &[],
            false,
        );
        let labels = labels(&items);
        assert!(labels.contains(&"x"), "missing x in {:?}", labels);
        assert!(labels.contains(&"m"), "missing m in {:?}", labels);
    }

    #[test]
    fn completion_offers_in_scope_locals() {
        let src = "void f() { int abc = 5; zz }";
        // Cursor sits just after `zz`, well after the `abc` declaration.
        let pos = pos_after(src, "zz");
        let items = {
            let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
            complete(&analysis, None, pos, None, None, &[], false)
        };
        let labels = labels(&items);
        assert!(labels.contains(&"abc"), "missing abc in {:?}", labels);
    }

    #[test]
    fn completion_offers_workspace_functions() {
        let src = "void greet() {} void f() { gr }";
        let ws = ws_from(src);
        let pos = pos_after(src, "gr");
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let items = complete(
            &analysis,
            None,
            pos,
            None,
            Some(ws.snapshot.symbols()),
            &[],
            false,
        );
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
        let items = {
            let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
            complete(&analysis, None, pos, None, None, &[], false)
        };
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
        let items = {
            let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
            complete(&analysis, None, pos, None, None, &[], false)
        };
        let labels = labels(&items);
        assert!(labels.contains(&"if"));
        assert!(labels.contains(&"endif"));
    }

    #[test]
    fn completion_offers_enclosing_class_members() {
        let src = "class C { int field; void m() {  } }";
        // Cursor inside method body.
        let pos = pos_after(src, "void m() { ");
        let items = {
            let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
            complete(&analysis, None, pos, None, None, &[], false)
        };
        let labels = labels(&items);
        assert!(labels.contains(&"field"), "missing field in {:?}", labels);
    }

    // === Namespace shortcut completions (old vscode extension parity) ===

    fn index_with_ui() -> crate::typedb::TypeIndex {
        // Minimal fake typedb: UI::Begin/BeginDisabled overloads, a Math
        // function, an enum + values in UX, and a UI enum for the
        // match-enums test.
        let mut idx = crate::typedb::TypeIndex::new();
        let db: crate::typedb::core_format::CoreDatabase =
            serde_json::from_value(serde_json::json!({
                "op": "1.0",
                "functions": [
                    {"ns": "UI", "name": "Begin", "returntypedecl": "bool", "args": []},
                    {"ns": "UI", "name": "BeginDisabled", "returntypedecl": "void", "args": []},
                    {"ns": "Math", "name": "Abs", "returntypedecl": "float", "args": []},
                    {"ns": "UX", "name": "DrawGrid", "returntypedecl": "void", "args": []},
                    {"ns": "MathX", "name": "Lerp", "returntypedecl": "float", "args": []}
                ],
                "enums": [
                    {"ns": "UI", "name": "WindowFlags", "values": {"NoTitleBar": {"v": 1}}},
                    {"ns": "UI", "name": "Cond", "values": {"Always": {"v": 2}}}
                ]
            }))
            .expect("fake core db");
        idx.add_core(&db);
        idx
    }

    fn complete_with_shortcuts(
        src: &str,
        marker: &str,
        idx: &crate::typedb::TypeIndex,
        namespaces: &[&str],
        match_enums: bool,
    ) -> Vec<CompletionItem> {
        let pos = pos_after(src, marker);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let ns: Vec<String> = namespaces.iter().map(|s| s.to_string()).collect();
        complete(&analysis, None, pos, Some(idx), None, &ns, match_enums)
    }

    #[test]
    fn shortcut_completion_offers_ui_functions_by_prefix() {
        let src = "void f() { Beg }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "Beg", &idx, &["UI"], false);
        let labels = labels(&items);
        assert!(
            labels.contains(&"UI::Begin"),
            "missing UI::Begin in {:?}",
            labels
        );
        assert!(
            labels.contains(&"UI::BeginDisabled"),
            "missing UI::BeginDisabled in {:?}",
            labels
        );
    }

    #[test]
    fn shortcut_completion_filters_to_case_insensitive_prefix() {
        let src = "void f() { beg }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "beg", &idx, &["UI"], false);
        let labels = labels(&items);
        assert!(
            labels.contains(&"UI::Begin"),
            "case-insensitive prefix: {:?}",
            labels
        );
    }

    #[test]
    fn shortcut_completion_item_has_filter_and_sort_text() {
        let src = "void f() { Beg }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "Beg", &idx, &["UI"], false);
        let item = items
            .iter()
            .find(|i| i.label == "UI::Begin")
            .expect("UI::Begin item");
        assert_eq!(item.filter_text.as_deref(), Some("Begin"));
        assert_eq!(item.sort_text.as_deref(), Some("z"));
        assert_eq!(item.kind, Some(CompletionItemKind::METHOD));
    }

    #[test]
    fn shortcut_completion_respects_namespace_list() {
        let src = "void f() { Dra }";
        let idx = index_with_ui();
        // UX not enabled → no UX::DrawGrid
        let items = complete_with_shortcuts(src, "Dra", &idx, &["UI", "Math"], false);
        let off_labels = labels(&items);
        assert!(
            !off_labels.contains(&"UX::DrawGrid"),
            "UX should be off: {:?}",
            off_labels
        );
        // UX enabled → present
        let items = complete_with_shortcuts(src, "Dra", &idx, &["UI", "UX"], false);
        let on_labels = labels(&items);
        assert!(
            on_labels.contains(&"UX::DrawGrid"),
            "UX should be on: {:?}",
            on_labels
        );
    }

    #[test]
    fn shortcut_completion_skips_enums_by_default() {
        let src = "void f() { Win }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "Win", &idx, &["UI"], false);
        let labels = labels(&items);
        assert!(
            !labels.contains(&"UI::WindowFlags"),
            "enums off by default: {:?}",
            labels
        );
        assert!(
            !labels.contains(&"UI::NoTitleBar"),
            "enum values off by default: {:?}",
            labels
        );
    }

    #[test]
    fn shortcut_completion_offers_enums_and_values_when_enabled() {
        let src = "void f() { Win }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "Win", &idx, &["UI"], true);
        let win_labels = labels(&items);
        assert!(
            win_labels.contains(&"UI::WindowFlags"),
            "enum name missing when enabled: {:?}",
            win_labels
        );
        // Enum values get the nested qualified label.
        let src2 = "void f() { NoTit }";
        let items2 = complete_with_shortcuts(src2, "NoTit", &idx, &["UI"], true);
        let value_labels = labels(&items2);
        assert!(
            value_labels.contains(&"UI::WindowFlags::NoTitleBar"),
            "enum value missing when enabled: {:?}",
            value_labels
        );
    }

    #[test]
    fn shortcut_completion_not_offered_mid_namespace() {
        let src = "void f() { UI::Beg }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "UI::Beg", &idx, &["UI"], false);
        let labels = labels(&items);
        // After `::` we're in namespace-member completion, not identifier
        // completion — shortcut items must not leak in.
        assert!(
            !labels.contains(&"UI::Begin"),
            "shortcuts must not fire after `::`: {:?}",
            labels
        );
    }

    #[test]
    fn shortcut_completion_does_not_duplicate_scope_items() {
        // Dedupe by label: the same shortcut namespace listed twice must
        // not produce duplicate items.
        let src = "void f() { Beg }";
        let idx = index_with_ui();
        let items = complete_with_shortcuts(src, "Beg", &idx, &["UI", "UI"], false);
        let count = items.iter().filter(|i| i.label == "UI::Begin").count();
        assert_eq!(count, 1, "duplicate UI::Begin items: {:?}", labels(&items));
    }

    #[test]
    fn shortcut_completion_full_typedb_is_fast() {
        // Perf smoke check: full real typedb (OpenplanetCore + OpenplanetNext
        // JSONs if present) with all 4 default shortcut namespaces must be
        // far below any per-keystroke budget. Prints timings; assert a
        // generous bound so CI machines with no Openplanet install skip.
        let core = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("OpenplanetNext/OpenplanetCore.json");
        let game = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("OpenplanetNext/OpenplanetNext.json");
        if !core.exists() || !game.exists() {
            eprintln!("skipping full-typedb perf test: no Openplanet JSONs under HOME");
            return;
        }
        let idx = crate::typedb::TypeIndex::load(&core, &game).expect("load typedb");
        let namespaces = ["UI", "Math", "UX", "MathX"];
        let src = "void f() { Beg }";
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let pos = pos_after(src, "Beg");

        let t0 = std::time::Instant::now();
        let mut total = 0usize;
        let n = 200;
        for _ in 0..n {
            total += complete(
                &analysis,
                None,
                pos,
                Some(&idx),
                None,
                &namespaces.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                false,
            )
            .len();
        }
        let per_call = t0.elapsed() / n;
        eprintln!(
            "shortcut completion: {} items avg, {:?} per call over {} calls",
            total / n as usize,
            per_call,
            n
        );
        assert!(
            per_call < std::time::Duration::from_millis(20),
            "completion too slow: {:?} per call",
            per_call
        );
    }
}
