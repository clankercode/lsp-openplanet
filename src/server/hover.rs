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

use crate::lexer;
use crate::parser::ast::SourceFile;
use crate::parser::Parser;
use crate::server::diagnostics::position_to_offset;
use crate::server::navigation;
use crate::server::scope_query;
use crate::symbols::scope::SymbolKind;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub fn hover(
    source: &str,
    position: Position,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Option<Hover> {
    let qualified = navigation::name_at_position(source, position)?;
    let bare = qualified
        .rsplit("::")
        .next()
        .unwrap_or(&qualified)
        .to_string();
    let offset = position_to_offset(source, position) as u32;

    // Parse once — we'll feed the AST into local + class lookups.
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file: SourceFile = parser.parse_file();

    // 1) Local variable / parameter in the enclosing function.
    if !qualified.contains("::") {
        if let Some(ty_text) = scope_query::local_type_at(source, &file, offset, &bare) {
            let ty_display = if ty_text.is_empty() {
                "?"
            } else {
                ty_text.as_str()
            };
            let md = format!("```angelscript\n(local) {} {}\n```", ty_display, bare);
            return Some(markdown_hover(md));
        }
    }

    // 2) Field on the enclosing class (only for bare names).
    if !qualified.contains("::") {
        if let Some(cls) = scope_query::find_enclosing_class(&file, offset) {
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
    if let Some(ws) = workspace {
        let mut hits = ws.lookup(&qualified);
        if hits.is_empty() {
            hits = ws.lookup(&bare);
        }
        if let Some(sym) = hits.first() {
            if let Some(md) = format_workspace_symbol(sym) {
                return Some(markdown_hover(md));
            }
        }
    }

    // 4) External type database.
    if let Some(index) = type_index {
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
    use crate::lexer;
    use crate::parser::Parser;

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
        let h = hover(src, pos, None, None).expect("hover should return");
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
        let h = hover(src, pos, None, None).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
    }

    #[test]
    fn hover_shows_workspace_function() {
        let src = "void greet() {}\nvoid main() { greet(); }";
        let ws = ws_from(src);
        let pos = pos_of(src, "greet", 2);
        let h = hover(src, pos, None, Some(&ws)).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("greet"), "missing greet in {:?}", m.value);
    }

    #[test]
    fn hover_shows_class_field() {
        let src = "class C { int field; void m() { field; } }";
        let pos = pos_of(src, "field", 2);
        let h = hover(src, pos, None, None).expect("hover should return");
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
        let h = hover(src, Position::new(0, 4), None, None);
        assert!(h.is_none());
    }
}
