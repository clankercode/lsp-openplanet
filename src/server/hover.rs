//! Hover information provider.
//!
//! Priority order at the cursor:
//!
//! 1. Local variable / parameter in the enclosing function
//! 2. Field/property on the enclosing class (when the cursor is inside a method)
//! 3. Workspace symbol (user-defined function / class / enum / etc.)
//! 4. External type database (Openplanet core + Nadeo)
//!
//! The first hit wins. Everything below is built on top of the existing
//! `navigation::name_at_position` helper plus a small AST walker
//! (`scope_query`) so we don't need to re-run the full type checker.

use tower_lsp::lsp_types::*;

use crate::analysis_snapshot::CheckedFile;
use crate::parser::ast::SourceFile;
use crate::server::diagnostics::position_to_offset;
use crate::server::navigation;
use crate::server::scope_query;
use crate::symbols::scope::SymbolKind;
use crate::typecheck::GlobalScope;
use crate::typedb::TypeIndex;

pub fn hover(
    analysis: &crate::analysis::DocumentAnalysis,
    checked: Option<&CheckedFile<'_>>,
    position: Position,
    scope: &GlobalScope<'_>,
) -> Option<Hover> {
    let source = analysis.masked_source();
    let qualified = navigation::name_at_position(analysis, position)?;
    let bare = qualified
        .rsplit("::")
        .next()
        .unwrap_or(&qualified)
        .to_string();
    let offset = position_to_offset(source, position) as u32;

    // 1) + 2) Recorded expression type at the cursor (GH #42): the checker
    // already resolved locals, params, fields and initializers — query its
    // span→type map instead of re-walking the AST with scope_query.
    if !qualified.contains("::") {
        if let Some(checked) = checked {
            if let Some(ty) = checked.type_at_offset(offset) {
                let display = ty.display();
                let ty_display = if display.is_empty() {
                    "?"
                } else {
                    display.as_str()
                };
                let kind_label = if is_field_name(analysis, offset, &bare) {
                    format!(
                        "(field) {}:{} {}",
                        class_name_at(analysis, offset),
                        display,
                        bare
                    )
                } else if is_method_name(analysis, offset, &bare) {
                    format!("(method) {} {}()", ty_display, bare)
                } else {
                    format!("(local) {} {}", ty_display, bare)
                };
                let md = format!("```angelscript\n{}\n```", kind_label);
                return Some(markdown_hover(md));
            }
        }
    }

    // Legacy fallback when no checked view is available (should not happen
    // in production; keeps the query surface optional for tests).
    if !qualified.contains("::") && checked.is_none() {
        let file: &SourceFile = &analysis.file;
        if let Some(ty_text) = scope_query::local_type_at(source, file, offset, &bare) {
            let ty_display = if ty_text.is_empty() {
                "?"
            } else {
                ty_text.as_str()
            };
            let md = format!("```angelscript\n(local) {} {}\n```", ty_display, bare);
            return Some(markdown_hover(md));
        }
        if let Some(cls) = scope_query::find_enclosing_class(file, offset) {
            if let Some(ty_text) = scope_query::class_member_type(cls, source, &bare) {
                let cls_name = cls.name.text(source);
                let md = format!(
                    "```angelscript\n(field) {}::{}: {}\n```",
                    cls_name, bare, ty_text
                );
                return Some(markdown_hover(md));
            }
        }
    }

    // 3) Workspace symbol lookup.
    let workspace_candidates = scope.lookup_reference(&qualified);
    if let Some(sym) = super::navigation::prefer_definition(&workspace_candidates) {
        if let Some(md) = format_workspace_symbol(sym) {
            return Some(markdown_hover(md));
        }
    }

    // 4) External type database.
    if let Some(index) = scope.external() {
        if let Some(h) = lookup_external(&qualified, index) {
            return Some(h);
        }
        // Also try the bare name as a fallback for short-name references.
        if qualified != bare {
            if let Some(h) = lookup_external(&bare, index) {
                return Some(h);
            }
        }
    }

    None
}

/// True when the ident at `offset` is a field of the enclosing class
/// (drives the `(field)` hover label, GH #42).
fn is_field_name(analysis: &crate::analysis::DocumentAnalysis, offset: u32, bare: &str) -> bool {
    let file: &SourceFile = &analysis.file;
    scope_query::find_enclosing_class(file, offset)
        .and_then(|cls| scope_query::class_member_type(cls, analysis.masked_source(), bare))
        .is_some()
}

/// True when the ident at `offset` names a method of the enclosing class
/// (drives the `(method)` hover label).
fn is_method_name(analysis: &crate::analysis::DocumentAnalysis, offset: u32, bare: &str) -> bool {
    let file: &SourceFile = &analysis.file;
    let source = analysis.masked_source();
    scope_query::find_enclosing_class(file, offset).is_some_and(|cls| {
        cls.members.iter().any(|m| {
            matches!(
                m,
                crate::parser::ast::ClassMember::Method(f) if f.name.text(source) == bare
            )
        })
    })
}

/// Qualified name of the class enclosing `offset`, when any.
fn class_name_at(analysis: &crate::analysis::DocumentAnalysis, offset: u32) -> String {
    let file: &SourceFile = &analysis.file;
    scope_query::find_enclosing_class(file, offset)
        .map(|cls| cls.name.text(analysis.masked_source()).to_string())
        .unwrap_or_default()
}

fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

fn format_workspace_symbol(sym: &crate::symbols::scope::Symbol) -> Option<String> {
    match &sym.kind {
        SymbolKind::Function {
            return_type,
            params,
            ..
        } => {
            let rt = if return_type.is_empty() {
                "void"
            } else {
                return_type.as_str()
            };
            let params_str: Vec<String> = params
                .iter()
                .map(|(ty, name)| format!("{} {}", ty, name))
                .collect();
            let sig = format!("{} {}({})", rt, sym.name, params_str.join(", "));
            Some(format!("```angelscript\n{}\n```", sig))
        }
        SymbolKind::Variable { type_name } => {
            let ty = if type_name.is_empty() {
                "?"
            } else {
                type_name.as_str()
            };
            Some(format!("```angelscript\n{} {}\n```", ty, sym.name))
        }
        SymbolKind::Class { parents, .. } => {
            let mut s = format!("class {}", sym.name);
            if !parents.is_empty() {
                s.push_str(&format!(" : {}", parents.join(", ")));
            }
            Some(format!("```angelscript\n{}\n```", s))
        }
        SymbolKind::Interface { .. } => {
            Some(format!("```angelscript\ninterface {}\n```", sym.name))
        }
        SymbolKind::Enum { values } => {
            let lines: Vec<String> = values.iter().map(|(n, _)| format!("  {},", n)).collect();
            Some(format!(
                "```angelscript\nenum {} {{\n{}\n}}\n```",
                sym.name,
                lines.join("\n")
            ))
        }
        SymbolKind::EnumValue { enum_name, value } => {
            let v = value
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            Some(format!(
                "```angelscript\n{}::{} = {}\n```",
                enum_name, sym.name, v
            ))
        }
        SymbolKind::Namespace => Some(format!("```angelscript\nnamespace {}\n```", sym.name)),
        SymbolKind::Funcdef {
            return_type,
            params,
        } => {
            let params_str: Vec<String> = params
                .iter()
                .map(|(ty, name)| format!("{} {}", ty, name))
                .collect();
            Some(format!(
                "```angelscript\nfuncdef {} {}({})\n```",
                return_type,
                sym.name,
                params_str.join(", ")
            ))
        }
    }
}

fn lookup_external(qualified: &str, index: &TypeIndex) -> Option<Hover> {
    if let Some(ty) = index.lookup_type(qualified) {
        let mut info = format!("**{}**", qualified);
        if let Some(parent) = &ty.parent {
            info.push_str(&format!(" : {}", parent));
        }
        if let Some(doc) = &ty.doc {
            info.push_str(&format!("\n\n{}", doc));
        }
        if !ty.properties.is_empty() || !ty.methods.is_empty() {
            info.push_str("\n\n```angelscript\n");
            for p in ty.properties.iter().take(6) {
                info.push_str(&format!("{} {};\n", p.type_name, p.name));
            }
            for m in ty.methods.iter().take(6) {
                let params: Vec<String> = m
                    .params
                    .iter()
                    .map(|a| {
                        let n = a.name.as_deref().unwrap_or("_");
                        format!("{} {}", a.type_name, n)
                    })
                    .collect();
                info.push_str(&format!(
                    "{} {}({});\n",
                    m.return_type,
                    m.name,
                    params.join(", ")
                ));
            }
            info.push_str("```");
        }
        return Some(markdown_hover(info));
    }

    if let Some(fns) = index.lookup_function(qualified) {
        let func = &fns[0];
        let params_str: Vec<String> = func
            .params
            .iter()
            .map(|p| {
                let name = p.name.as_deref().unwrap_or("_");
                format!("{} {}", p.type_name, name)
            })
            .collect();
        let sig = format!(
            "{} {}({})",
            func.return_type,
            func.name,
            params_str.join(", ")
        );
        let mut info = format!("```angelscript\n{}\n```", sig);
        if let Some(doc) = &func.doc {
            info.push_str(&format!("\n\n{}", doc));
        }
        return Some(markdown_hover(info));
    }

    if let Some(en) = index.lookup_enum(qualified) {
        let values_str: Vec<String> = en
            .values
            .iter()
            .map(|(name, val)| format!("  {} = {}", name, val))
            .collect();
        let info = format!(
            "```angelscript\nenum {} {{\n{}\n}}\n```",
            en.name,
            values_str.join(",\n")
        );
        return Some(markdown_hover(info));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::TestWorkspace;

    /// Find a 1-based column of the first occurrence of `needle` starting at
    /// position `from` (byte offset). Returns the (line, character).
    fn pos_of(source: &str, needle: &str, occurrence: usize) -> Position {
        let mut start = 0;
        for _ in 0..occurrence {
            let idx = source[start..]
                .find(needle)
                .unwrap_or_else(|| panic!("needle {:?} not found", needle));
            start += idx;
            if start + 1 <= source.len() {
                start += 1;
            }
        }
        let byte = start - 1;
        let prefix = &source[..byte];
        let line = prefix.matches('\n').count() as u32;
        let col = prefix.rfind('\n').map_or(byte, |nl| byte - nl - 1) as u32;
        Position::new(line, col)
    }

    #[test]
    fn hover_shows_local_var_type() {
        let src = "void f() { int x = 5; x; }";
        // Second occurrence of `x` — cursor sits inside it.
        let pos = pos_of(src, "x", 2);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
        assert!(m.value.contains('x'), "missing x in {:?}", m.value);
    }

    #[test]
    fn hover_shows_local_param_type() {
        let src = "void f(int arg) { arg; }";
        let pos = pos_of(src, "arg", 2);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
    }

    #[test]
    fn hover_shows_workspace_function() {
        let src = "void greet() {}\nvoid main() { greet(); }";
        let tw = TestWorkspace::one_file("hover.as", src);
        let pos = pos_of(src, "greet", 2);
        let analysis = tw.analysis();
        let scope = tw.scope();
        let h = hover(analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("greet"), "missing greet in {:?}", m.value);
    }

    #[test]
    fn hover_shows_class_field() {
        let src = "class C { int field; void m() { field; } }";
        let pos = pos_of(src, "field", 2);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("field"), "missing field in {:?}", m.value);
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
    }

    #[test]
    fn hover_returns_none_outside_ident() {
        let src = "void f() {}";
        // Column 4: the space between `void` and `f`.
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, Position::new(0, 4), &scope);
        assert!(h.is_none());
    }

    #[test]
    fn hover_implicit_this_method_call() {
        // GH: hovering a bare implicit-this call of a class method must NOT
        // show "(local) <error>" — the callee resolves to the method with
        // its return type.
        let src = "class C { void CheckInit() {} void m() { CheckInit(); } }";
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let checked = tw
            .snapshot
            .checked_file(&tw.uri(), &scope)
            .expect("checked");
        let analysis = tw.analysis();
        let pos = pos_of(src, "CheckInit", 2);
        let h = hover(analysis, Some(&checked), pos, &scope).expect("hover");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(
            !m.value.contains("<error>"),
            "hover showed <error>: {:?}",
            m.value
        );
        assert!(m.value.contains("CheckInit"), "missing name: {:?}", m.value);
    }

    #[test]
    fn hover_uses_checked_view_types() {
        // GH #42: with a checked view, hover reads the checker's recorded
        // span→type instead of the scope_query approximation.
        let dir = std::env::temp_dir().join("ols-hover-checked-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("e.as");
        let src = "void main() { int x = 1; int y = x; }";
        std::fs::write(&path, src).unwrap();
        let snap = crate::analysis_snapshot::AnalysisSnapshot::from_files(
            &[(path, src.to_string())],
            &crate::config::LspConfig::default(),
        );
        let uri = snap.uri_map()[&0].0.clone();
        let symbols = snap.symbols();
        let scope = GlobalScope::new(symbols, None);
        let checked = snap.checked_file(&uri, &scope).expect("checked view");

        // Cursor over the use of `x` (offset 33 → line 0, char 33).
        let analysis = snap.analysis_of(&uri).expect("analysis");
        let h = hover(analysis, Some(&checked), Position::new(0, 33), &scope)
            .expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(
            m.value.contains("(local) int x"),
            "expected recorded int type in {:?}",
            m.value
        );
    }
}
