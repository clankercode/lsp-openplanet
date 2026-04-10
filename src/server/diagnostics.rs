use tower_lsp::lsp_types::*;

use crate::config::LspConfig;
use crate::lexer;
use crate::parser::Parser;
use crate::preprocessor;

/// Compute diagnostics for a single file.
pub fn compute_diagnostics(uri: &Url, source: &str, config: &LspConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // Check if this is info.toml
    if uri.path().ends_with("info.toml") {
        compute_toml_diagnostics(source, &mut diagnostics);
        return diagnostics;
    }

    // Preprocess
    let preprocess_result = preprocessor::preprocess(source, &config.defines);
    for err in &preprocess_result.errors {
        diagnostics.push(Diagnostic {
            range: line_range(source, err.line),
            severity: Some(DiagnosticSeverity::ERROR),
            message: format!("{:?}", err.kind),
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        });
    }

    // Lex
    let tokens = lexer::tokenize_filtered(&preprocess_result.masked_source);

    // Parse
    let mut parser = Parser::new(&tokens, &preprocess_result.masked_source);
    let _file = parser.parse_file();

    for err in &parser.errors {
        let range = span_to_range(source, err.span);
        diagnostics.push(Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: err.to_string(),
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        });
    }

    diagnostics
}

fn compute_toml_diagnostics(source: &str, diagnostics: &mut Vec<Diagnostic>) {
    use crate::workspace::manifest::Manifest;
    match Manifest::parse(source) {
        Ok(manifest) => {
            // Can't validate export file paths without workspace root here,
            // but can check for missing required fields
            if manifest.meta.version.is_none() {
                diagnostics.push(Diagnostic {
                    range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                    severity: Some(DiagnosticSeverity::ERROR),
                    message: "Missing required field: [meta].version".to_string(),
                    source: Some("openplanet-lsp".to_string()),
                    ..Default::default()
                });
            }
        }
        Err(diag) => {
            diagnostics.push(Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                severity: Some(DiagnosticSeverity::ERROR),
                message: diag.message,
                source: Some("openplanet-lsp".to_string()),
                ..Default::default()
            });
        }
    }
}

fn line_range(source: &str, line: usize) -> Range {
    let _line_start = source.lines().take(line).map(|l| l.len() + 1).sum::<usize>();
    let line_text = source.lines().nth(line).unwrap_or("");
    Range::new(
        Position::new(line as u32, 0),
        Position::new(line as u32, line_text.len() as u32),
    )
}

pub fn span_to_range(source: &str, span: crate::lexer::Span) -> Range {
    let start = offset_to_position(source, span.start as usize);
    let end = offset_to_position(source, span.end as usize);
    Range::new(start, end)
}

pub fn offset_to_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    let line = prefix.matches('\n').count();
    let col = prefix.rfind('\n').map_or(offset, |nl| offset - nl - 1);
    Position::new(line as u32, col as u32)
}

pub fn position_to_offset(source: &str, pos: Position) -> usize {
    let mut line = 0u32;
    let mut offset = 0;
    for ch in source.chars() {
        if line == pos.line {
            if (offset - source[..offset].rfind('\n').map_or(0, |n| n + 1)) as u32 >= pos.character {
                return offset;
            }
        }
        if ch == '\n' {
            line += 1;
        }
        offset += ch.len_utf8();
    }
    offset
}
