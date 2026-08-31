use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::parser::ast::{ClassMember, Item};
use crate::server::diagnostics::span_to_range;
use crate::symbols::scope::SymbolKind as InternalSymbolKind;
use crate::symbols::SymbolTable;

/// B135: the visibility keyword for an outline entry's `detail`, or `None`.
///
/// # The decision
///
/// The document outline SHOWS every member, including `private` ones, and
/// annotates rather than filters. An outline is a map of the file the user
/// already has open: hiding a declaration that is plainly visible three lines
/// away misrepresents the file rather than protecting anyone. That is the
/// opposite conclusion from B117's completion filter, and deliberately so —
/// completion PROPOSES code to write, an outline DESCRIBES code that exists.
///
/// `detail` was `None` on every entry before this, so the annotation is purely
/// additive; nothing that was rendered before renders differently.
fn visibility_detail(is_private: bool, is_protected: bool) -> Option<String> {
    // The parser takes the first keyword it sees and never sets both.
    if is_private {
        Some("private".to_string())
    } else if is_protected {
        Some("protected".to_string())
    } else {
        None
    }
}

pub fn document_symbols(analysis: &DocumentAnalysis) -> Option<DocumentSymbolResponse> {
    // Names and ranges come from the masked AST; masking preserves byte
    // offsets, so span→text/line mapping via the masked source is exact.
    let source = analysis.masked_source();

    let symbols: Vec<DocumentSymbol> = analysis
        .file
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
                                detail: visibility_detail(var.is_private, var.is_protected),
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
                            detail: visibility_detail(func.is_private, func.is_protected),
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
                            detail: visibility_detail(func.is_private, func.is_protected),
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
                            detail: visibility_detail(func.is_private, func.is_protected),
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
                            detail: visibility_detail(prop.is_private, prop.is_protected),
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
    file_uris: &HashMap<usize, (Url, &crate::analysis::DocumentAnalysis)>,
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
        let Some((uri, analysis)) = file_uris.get(&sym.file_id) else {
            continue;
        };
        let range = span_to_range(analysis.masked_source(), sym.span);
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

    pub(super) fn doc_symbols(src: &str) -> Vec<DocumentSymbol> {
        let analysis = DocumentAnalysis::analyze_plain(src);
        match document_symbols(&analysis) {
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
    ) -> Vec<crate::server::test_support::TestWorkspace> {
        sources
            .iter()
            .map(|(name, src)| crate::server::test_support::TestWorkspace::one_file(name, src))
            .collect()
    }

    #[test]
    fn workspace_symbols_finds_class() {
        let tws = build_workspace(&[("a.as", "class Hello {}")]);
        let files = tws[0].uri_map();
        let results = workspace_symbols("hello", tws[0].snapshot.symbols(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Hello");
        assert_eq!(results[0].kind, SymbolKind::CLASS);
    }

    #[test]
    fn workspace_symbols_substring_match() {
        let tws = build_workspace(&[("a.as", "class Hello {}")]);
        let files = tws[0].uri_map();
        let results = workspace_symbols("el", tws[0].snapshot.symbols(), &files);
        assert!(
            results.iter().any(|s| s.name == "Hello"),
            "expected Hello in results: {:?}",
            results.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn workspace_symbols_empty_query_returns_all_top_level() {
        let tws = build_workspace(&[("a.as", "class Hello {} enum E { A, B } void f() {}")]);
        let files = tws[0].uri_map();
        let results = workspace_symbols("", tws[0].snapshot.symbols(), &files);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Hello"));
        assert!(names.contains(&"E"));
        assert!(names.contains(&"f"));
        // Enum values should be excluded for empty query
        assert!(!names.contains(&"E::A"));
        assert!(!names.contains(&"E::B"));
    }

    /// B054: workspace symbols reads the pool, which held no interface
    /// member at all — `I::Go` could not be searched for, while the
    /// class's `C::Go` could.
    #[test]
    fn workspace_symbols_lists_interface_methods() {
        let tws = build_workspace(&[(
            "a.as",
            "interface I { void Go(); }\nclass C : I { void Go() {} }\n",
        )]);
        let files = tws[0].uri_map();
        let results = workspace_symbols("Go", tws[0].snapshot.symbols(), &files);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"I::Go"), "missing I::Go in {names:?}");
        assert!(names.contains(&"C::Go"), "missing C::Go in {names:?}");
    }

    #[test]
    fn workspace_symbols_lists_namespaced_interface_methods() {
        let tws = build_workspace(&[("a.as", "namespace Ns { interface I { void Go(); } }")]);
        let files = tws[0].uri_map();
        let names: Vec<String> = workspace_symbols("Go", tws[0].snapshot.symbols(), &files)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(names.contains(&"Ns::I::Go".to_string()), "got {names:?}");
    }
}

// ── B135: the outline SHOWS every member and annotates it ───────────────────
//
// The opposite conclusion from B117's completion filter, deliberately. An
// outline is a map of the file the user already has open; hiding a declaration
// that is plainly visible a few lines away misrepresents the file rather than
// protecting anyone. Completion proposes code to write, an outline describes
// code that exists.
#[cfg(test)]
mod b135_outline_visibility_tests {
    use super::tests::doc_symbols;

    fn member<'a>(syms: &'a [super::DocumentSymbol], name: &str) -> &'a super::DocumentSymbol {
        syms[0]
            .children
            .as_ref()
            .expect("class children")
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no outline entry for `{name}`"))
    }

    const SRC: &str = "class C { private int privF; protected int protF; int pubF; \
        private void privM() {} protected void protM() {} void pubM() {} \
        private int privP { get { return 1; } } }";

    /// The load-bearing negative control: nothing is HIDDEN. Every assertion
    /// about `detail` below would pass vacuously against an outline that had
    /// dropped the entry entirely.
    #[test]
    fn every_member_appears_in_the_outline_whatever_its_visibility() {
        let syms = doc_symbols(SRC);
        let children = syms[0].children.as_ref().expect("class children");
        for name in ["privF", "protF", "pubF", "privM", "protM", "pubM", "privP"] {
            assert!(
                children.iter().any(|c| c.name == name),
                "`{name}` must appear in the outline, got {:?}",
                children.iter().map(|c| &c.name).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_private_member_is_annotated_private() {
        let syms = doc_symbols(SRC);
        assert_eq!(member(&syms, "privF").detail.as_deref(), Some("private"));
        assert_eq!(member(&syms, "privM").detail.as_deref(), Some("private"));
        assert_eq!(member(&syms, "privP").detail.as_deref(), Some("private"));
    }

    #[test]
    fn a_protected_member_is_annotated_protected() {
        let syms = doc_symbols(SRC);
        assert_eq!(member(&syms, "protF").detail.as_deref(), Some("protected"));
        assert_eq!(member(&syms, "protM").detail.as_deref(), Some("protected"));
    }

    /// The default, pinned. Also the row that stops an implementation from
    /// annotating everything.
    #[test]
    fn a_public_member_carries_no_annotation() {
        let syms = doc_symbols(SRC);
        assert_eq!(member(&syms, "pubF").detail, None);
        assert_eq!(member(&syms, "pubM").detail, None);
    }

    /// Stated as a difference, so an implementation rendering one "not public"
    /// label passes everything above and fails here.
    #[test]
    fn private_and_protected_annotations_differ() {
        let syms = doc_symbols(SRC);
        assert_ne!(member(&syms, "privM").detail, member(&syms, "protM").detail);
    }

    /// A private CONSTRUCTOR is a real pattern (a factory-only class) and the
    /// engine enforces it — B117 measured `Illegal call to private method` on
    /// one called from an unrelated function. It must still be outlined.
    #[test]
    fn a_private_constructor_is_outlined_and_annotated() {
        let syms = doc_symbols("class C { private C() {} }");
        assert_eq!(member(&syms, "C").detail.as_deref(), Some("private"));
    }
}
