use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::config::LspConfig;
use crate::symbols::SymbolTable;
use crate::typecheck::{Checker, GlobalScope, TypeDiagnosticSeverity};
use crate::typedb::TypeIndex;

/// Compute diagnostics for a single file.
///
/// If `workspace_symbols` is `Some`, the supplied pooled [`SymbolTable`] is
/// used as the workspace for name resolution (so sibling-file declarations
/// and dependency exports are visible). If `None`, a single-file symbol table
/// is built on the fly from `source` alone.
pub fn compute_diagnostics(
    uri: &Url,
    source: &str,
    config: &LspConfig,
    type_index: Option<&TypeIndex>,
    workspace_symbols: Option<&SymbolTable>,
) -> Vec<Diagnostic> {
    let analysis = DocumentAnalysis::analyze(source, &config.defines);
    compute_diagnostics_from_analysis(uri, &analysis, config, type_index, workspace_symbols)
}

/// Diagnostics from an existing [`DocumentAnalysis`] (shared pipeline).
pub fn compute_diagnostics_from_analysis(
    uri: &Url,
    analysis: &DocumentAnalysis,
    _config: &LspConfig,
    type_index: Option<&TypeIndex>,
    workspace_symbols: Option<&SymbolTable>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let source = analysis.source.as_str();

    if uri.path().ends_with("info.toml") {
        compute_toml_diagnostics(source, &mut diagnostics);
        return diagnostics;
    }

    for err in analysis.preprocess_errors() {
        diagnostics.push(Diagnostic {
            range: line_range(source, err.line),
            severity: Some(DiagnosticSeverity::ERROR),
            message: format!("{:?}", err.kind),
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        });
    }

    for err in &analysis.parse_errors {
        let range = span_to_range(source, err.span);
        diagnostics.push(Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: err.to_string(),
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        });
    }

    let owned_symbols: Option<SymbolTable> = if workspace_symbols.is_some() {
        None
    } else {
        let mut symbols = SymbolTable::new();
        let fid = symbols.allocate_file_id();
        let file_syms =
            SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
        symbols.set_file_symbols(fid, file_syms);
        Some(symbols)
    };
    let symbols_ref: &SymbolTable = workspace_symbols
        .unwrap_or_else(|| owned_symbols.as_ref().expect("owned symbols built above"));
    let scope = GlobalScope::new(symbols_ref, type_index);
    let mut checker = Checker::new(analysis.masked_source(), &scope);
    checker.check_file(&analysis.file);
    for diag in &checker.diagnostics {
        let range = span_to_range(source, diag.span);
        let severity = match diag.severity() {
            TypeDiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
            TypeDiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
        };
        diagnostics.push(Diagnostic {
            range,
            severity: Some(severity),
            message: diag.message(),
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
