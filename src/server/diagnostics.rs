use tower_lsp::lsp_types::*;

use crate::analysis::unused::UnusedDeclaration;
use crate::analysis::DocumentAnalysis;
use crate::config::{LspConfig, UnusedLevel};
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
) -> Vec<Diagnostic> {
    let analysis = DocumentAnalysis::analyze(source, &config.defines);
    // No snapshot behind this call — pool symbols from this parse alone.
    let mut symbols = SymbolTable::new();
    let fid = symbols.allocate_file_id();
    let file_syms = SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
    symbols.set_file_symbols(fid, file_syms);
    compute_diagnostics_from_analysis(uri, &analysis, config, type_index, &symbols)
}

/// Diagnostics for required plugin dependencies that were not resolved by the
/// shared workspace loader. These are attached to `info.toml` by the CLI and
/// LSP backend alike.
pub fn missing_required_dependency_diagnostics(dependencies: &[String]) -> Vec<Diagnostic> {
    dependencies
        .iter()
        .map(|dep_id| Diagnostic {
            range: Range::default(),
            severity: Some(DiagnosticSeverity::ERROR),
            message: format!(
                "dependency `{}` not found in any configured plugin directory",
                dep_id
            ),
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        })
        .collect()
}

/// Manifest diagnostics, including unresolved required plugin dependencies.
/// Kept together so replacing the LSP diagnostic set never drops TOML errors.
pub fn compute_manifest_diagnostics(
    uri: &Url,
    source: &str,
    config: &LspConfig,
    missing_required_dependencies: &[String],
) -> Vec<Diagnostic> {
    let mut diagnostics = compute_diagnostics(uri, source, config, None);
    diagnostics.extend(missing_required_dependency_diagnostics(
        missing_required_dependencies,
    ));
    diagnostics
}

/// Diagnostics from an existing [`DocumentAnalysis`] (shared pipeline).
pub fn compute_diagnostics_from_analysis(
    uri: &Url,
    analysis: &DocumentAnalysis,
    _config: &LspConfig,
    type_index: Option<&TypeIndex>,
    workspace_symbols: &SymbolTable,
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

    let scope = GlobalScope::new(workspace_symbols, type_index);
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

/// Unused-declaration warnings for one file of a snapshot, converted to
/// LSP diagnostics, with usage PRE-COLLECTED by the caller (collecting
/// runs a check pass over every workspace file — do it once per report,
/// not once per file, or an N-file workspace pays N² passes).
/// `export_paths` are the plugin's own info.toml export files (public
/// API — exempt). Severity follows [`UnusedLevel`]; [`UnusedLevel::Ignore`]
/// emits nothing.
pub fn unused_declaration_diagnostics(
    source: &str,
    usage: &crate::analysis::unused::WorkspaceUsage,
    snapshot: &crate::analysis_snapshot::AnalysisSnapshot,
    file_id: usize,
    export_paths: &std::collections::HashSet<std::path::PathBuf>,
    unused: UnusedLevel,
) -> Vec<Diagnostic> {
    let Some(severity) = unused_severity(unused) else {
        return Vec::new();
    };
    crate::analysis::unused::unused_declarations_for_file(snapshot, usage, export_paths, file_id)
        .iter()
        .map(|u| unused_to_diagnostic(source, u, severity))
        .collect()
}

fn unused_severity(level: UnusedLevel) -> Option<DiagnosticSeverity> {
    match level {
        UnusedLevel::Ignore => None,
        UnusedLevel::Hint => Some(DiagnosticSeverity::HINT),
        UnusedLevel::Info => Some(DiagnosticSeverity::INFORMATION),
        UnusedLevel::Warn => Some(DiagnosticSeverity::WARNING),
        UnusedLevel::Error => Some(DiagnosticSeverity::ERROR),
    }
}

fn unused_to_diagnostic(
    source: &str,
    unused: &UnusedDeclaration,
    severity: DiagnosticSeverity,
) -> Diagnostic {
    let article = match unused.kind.chars().next() {
        Some(c) if "aeiou".contains(c) => "an",
        _ => "a",
    };
    Diagnostic {
        range: span_to_range(source, unused.span),
        severity: Some(severity),
        message: format!(
            "{} {} `{}` is never used (suppress with a leading `_` or a trailing `// lsp: ignore unused` comment)",
            article, unused.kind, unused.simple_name
        ),
        source: Some("openplanet-lsp".to_string()),
        code: Some(NumberOrString::String("unused-declaration".to_string())),
        // The UNNECESSARY tag — not the severity — is what makes a client
        // fade the range (VS Code gates the grey-out on `editor.showUnused`).
        // Sent at every level: it is orthogonal to severity, it is what keeps
        // `unused = "hint"` visible when the client omits HINT diagnostics
        // from its problems list, and clients without
        // `publishDiagnostics.tagSupport` simply ignore the field.
        tags: Some(vec![DiagnosticTag::UNNECESSARY]),
        ..Default::default()
    }
}

fn line_range(source: &str, line: usize) -> Range {
    let _line_start = source
        .lines()
        .take(line)
        .map(|l| l.len() + 1)
        .sum::<usize>();
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
            if (offset - source[..offset].rfind('\n').map_or(0, |n| n + 1)) as u32 >= pos.character
            {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_required_dependency_diagnostic_is_an_error_at_manifest_start() {
        let diagnostics = missing_required_dependency_diagnostics(&["MissingDep".to_string()]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].range, Range::default());
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(diagnostics[0].message.contains("MissingDep"));
    }

    #[test]
    fn manifest_diagnostics_preserve_toml_and_missing_dependency_errors() {
        let uri = Url::parse("file:///tmp/info.toml").unwrap();
        let diagnostics = compute_manifest_diagnostics(
            &uri,
            "[meta]\nname = \"Missing Version\"\n",
            &LspConfig::default(),
            &["MissingDep".to_string()],
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("[meta].version")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("MissingDep")));
    }

    #[test]
    fn unused_severity_follows_level() {
        for (level, want) in [
            (UnusedLevel::Ignore, None),
            (UnusedLevel::Hint, Some(DiagnosticSeverity::HINT)),
            (UnusedLevel::Info, Some(DiagnosticSeverity::INFORMATION)),
            (UnusedLevel::Warn, Some(DiagnosticSeverity::WARNING)),
            (UnusedLevel::Error, Some(DiagnosticSeverity::ERROR)),
        ] {
            assert_eq!(unused_severity(level), want, "{level:?}");
        }
    }

    /// Every unused finding carries `DiagnosticTag::UNNECESSARY` regardless
    /// of severity — that tag, not the severity, is what makes a client grey
    /// the range out (VS Code `editor.showUnused`), and it is the only thing
    /// that keeps `unused = "hint"` visible at all.
    #[test]
    fn unused_to_diagnostic_tags_unnecessary_at_every_severity() {
        for kind in [
            "function",
            "variable",
            "class",
            "parameter",
            "enum",
            "import",
        ] {
            for sev in [
                DiagnosticSeverity::HINT,
                DiagnosticSeverity::INFORMATION,
                DiagnosticSeverity::WARNING,
                DiagnosticSeverity::ERROR,
            ] {
                let unused = UnusedDeclaration {
                    qualified_name: "Foo".into(),
                    simple_name: "Foo".into(),
                    kind,
                    span: crate::lexer::Span::new(5, 8),
                    file_id: 0,
                };
                let diag = unused_to_diagnostic("void Foo() {}", &unused, sev);
                assert_eq!(
                    diag.tags,
                    Some(vec![DiagnosticTag::UNNECESSARY]),
                    "{kind} {sev:?} must be tagged unnecessary"
                );
            }
        }
    }

    #[test]
    fn unused_to_diagnostic_kinds_use_requested_severity() {
        for kind in ["function", "variable", "class", "parameter"] {
            for sev in [
                DiagnosticSeverity::HINT,
                DiagnosticSeverity::INFORMATION,
                DiagnosticSeverity::WARNING,
                DiagnosticSeverity::ERROR,
            ] {
                let unused = UnusedDeclaration {
                    qualified_name: "Foo".into(),
                    simple_name: "Foo".into(),
                    kind,
                    span: crate::lexer::Span::new(5, 8),
                    file_id: 0,
                };
                let diag = unused_to_diagnostic("void Foo() {}", &unused, sev);
                assert_eq!(diag.severity, Some(sev), "{kind} {sev:?}");
                assert!(
                    diag.message.contains("Foo"),
                    "message should name the symbol: {}",
                    diag.message
                );
                assert_eq!(
                    diag.code,
                    Some(NumberOrString::String("unused-declaration".into()))
                );
            }
        }
    }
}
