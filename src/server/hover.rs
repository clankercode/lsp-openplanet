use tower_lsp::lsp_types::*;

use crate::lexer::{self, TokenKind};
use crate::server::diagnostics::position_to_offset;
use crate::typedb::TypeIndex;

pub fn hover(
    source: &str,
    position: Position,
    type_index: Option<&TypeIndex>,
) -> Option<Hover> {
    let offset = position_to_offset(source, position);

    let tokens = lexer::tokenize_filtered(source);
    let (idx, token) = tokens.iter().enumerate().find(|(_, t)| {
        (t.span.start as usize) <= offset && offset <= (t.span.end as usize)
    })?;

    if token.kind != TokenKind::Ident {
        return None;
    }

    let qualified = find_qualified_name_at(&tokens, idx, source);
    let index = type_index?;

    if let Some(ty) = index.lookup_type(&qualified) {
        let mut info = format!("**{}**", qualified);
        if let Some(parent) = &ty.parent {
            info.push_str(&format!(" : {}", parent));
        }
        if let Some(doc) = &ty.doc {
            info.push_str(&format!("\n\n{}", doc));
        }
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: info,
            }),
            range: None,
        });
    }

    if let Some(fns) = index.lookup_function(&qualified) {
        let func = &fns[0];
        let params_str: Vec<String> = func
            .params
            .iter()
            .map(|p| {
                let name = p.name.as_deref().unwrap_or("_");
                format!("{} {}", p.type_name, name)
            })
            .collect();
        let sig = format!("{} {}({})", func.return_type, func.name, params_str.join(", "));
        let mut info = format!("```angelscript\n{}\n```", sig);
        if let Some(doc) = &func.doc {
            info.push_str(&format!("\n\n{}", doc));
        }
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: info,
            }),
            range: None,
        });
    }

    if let Some(en) = index.lookup_enum(&qualified) {
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
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: info,
            }),
            range: None,
        });
    }

    None
}

fn find_qualified_name_at(tokens: &[lexer::Token], idx: usize, source: &str) -> String {
    let mut parts = vec![tokens[idx].span.text(source).to_string()];

    let mut i = idx;
    while i >= 2
        && tokens[i - 1].kind == TokenKind::ColonColon
        && tokens[i - 2].kind == TokenKind::Ident
    {
        parts.push(tokens[i - 2].span.text(source).to_string());
        i -= 2;
    }

    parts.reverse();
    parts.join("::")
}
