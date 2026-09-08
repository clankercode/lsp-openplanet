use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::*;

use crate::analysis::unused::UnusedDeclaration;
use crate::analysis::DocumentAnalysis;
use crate::config::{InactiveLevel, LspConfig, UnusedLevel};
use crate::symbols::SymbolTable;
use crate::typecheck::{Checker, GlobalScope, TypeDiagnosticKind, TypeDiagnosticSeverity};
use crate::typedb::TypeIndex;
use crate::workspace::manifest::{
    DiagSeverity, Manifest, ManifestAnchor, ManifestDiagnostic, ManifestWorkspace,
};

/// The `source` field on every diagnostic this server publishes. Code
/// actions key on it so another server's diagnostics can never drive our
/// quick fixes.
pub const DIAGNOSTIC_SOURCE: &str = "openplanet-lsp";

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
    compute_diagnostics_with_versions(
        uri,
        &analysis,
        config,
        type_index,
        &symbols,
        None,
        Some(fid),
    )
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

/// Every diagnostic that belongs on an info.toml: the manifest rules plus
/// unresolved required plugin dependencies. Kept together so replacing the
/// published set never drops one kind while refreshing the other.
///
/// Takes the workspace root explicitly. The URI a client opened may be a
/// symlink alias (dips++-all layout), so it is publication identity only —
/// never the directory `exports` entries resolve against.
pub fn compute_manifest_diagnostics_in_workspace(
    source: &str,
    workspace: &ManifestWorkspace<'_>,
    missing_required_dependencies: &[String],
) -> Vec<Diagnostic> {
    let mut diagnostics = manifest_rule_diagnostics(source, Some(workspace));
    diagnostics.extend(missing_required_dependency_diagnostics(
        missing_required_dependencies,
    ));
    diagnostics
}

/// Diagnostics from an existing [`DocumentAnalysis`] (shared pipeline).
pub fn compute_diagnostics_from_analysis(
    uri: &Url,
    analysis: &DocumentAnalysis,
    config: &LspConfig,
    type_index: Option<&TypeIndex>,
    workspace_symbols: &SymbolTable,
    file_id: Option<usize>,
) -> Vec<Diagnostic> {
    compute_diagnostics_with_versions(
        uri,
        analysis,
        config,
        type_index,
        workspace_symbols,
        None,
        file_id,
    )
}

/// [`compute_diagnostics_from_analysis`] plus GH #18 version context:
/// `(notes, target_label)` from the version catalog. A missing symbol the
/// recorded snapshots know (added later / removed earlier than the target)
/// gets that fact appended to its message. Keyed on the STRUCTURED
/// `NoMatchingSymbol.qualified` field — never on message text — and all
/// version comparison happened in the catalog; this only renders the
/// supplied strings.
pub fn compute_diagnostics_with_versions(
    uri: &Url,
    analysis: &DocumentAnalysis,
    config: &LspConfig,
    type_index: Option<&TypeIndex>,
    workspace_symbols: &SymbolTable,
    version: Option<(&crate::typedb::catalog::VersionNotes, &str)>,
    file_id: Option<usize>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let source = analysis.source.as_str();

    if uri.path().ends_with("info.toml") {
        diagnostics.extend(manifest_diagnostics_for_uri(uri, source, config));
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
    if let Some(file_id) = file_id {
        checker = checker.with_file_id(file_id);
    }
    checker.check_file(&analysis.file);
    for diag in &checker.diagnostics {
        let range = span_to_range(source, diag.span);
        let severity = match diag.severity() {
            TypeDiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
            TypeDiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
        };
        let mut message = diag.message();
        if let (
            Some((notes, target_label)),
            TypeDiagnosticKind::NoMatchingSymbol { qualified, .. },
        ) = (version, &diag.kind)
        {
            if let Some(note) = notes.get(qualified) {
                message = format!("{message} ({note}; target is Openplanet {target_label})");
            }
        }
        diagnostics.push(Diagnostic {
            range,
            severity: Some(severity),
            message,
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        });
    }

    diagnostics
}

/// Every info.toml rule, as LSP diagnostics: a TOML syntax error, or the
/// findings of [`Manifest::validate`].
///
/// B040: `validate` was written but never called, so a manifest that stops
/// the plugin loading in-game produced no editor signal at all. This is the
/// one place its findings become diagnostics; the LSP publish path and CLI
/// `check` both come through here, so they cannot drift apart.
///
/// `workspace` is `None` when the manifest has no directory we can stat —
/// the export rules are then skipped (see [`Manifest::validate`]).
pub fn manifest_rule_diagnostics(
    source: &str,
    workspace: Option<&ManifestWorkspace<'_>>,
) -> Vec<Diagnostic> {
    let to_diagnostic = |diag: ManifestDiagnostic| Diagnostic {
        range: manifest_range(source, &diag.anchor),
        severity: Some(match diag.severity {
            // A plugin that cannot load is not a passing check: Error here
            // is also what makes CLI `check` exit non-zero.
            DiagSeverity::Error => DiagnosticSeverity::ERROR,
            DiagSeverity::Warning => DiagnosticSeverity::WARNING,
        }),
        message: diag.message,
        source: Some(DIAGNOSTIC_SOURCE.to_string()),
        code: Some(NumberOrString::String("manifest".to_string())),
        ..Default::default()
    };
    match Manifest::parse(source) {
        Ok(manifest) => manifest
            .validate(workspace)
            .into_iter()
            .map(to_diagnostic)
            .collect(),
        // A manifest we cannot parse is reported, not skipped: silence here
        // reads as "your info.toml is fine" while the game refuses to load it.
        Err(parse_error) => vec![to_diagnostic(parse_error)],
    }
}

/// Byte-range anchor → LSP range. Both ends are clamped into `source` by
/// [`manifest_span::anchor_range`], so this cannot produce an out-of-bounds
/// position.
fn manifest_range(source: &str, anchor: &ManifestAnchor) -> Range {
    let span = crate::workspace::manifest_span::anchor_range(source, anchor);
    Range::new(
        offset_to_position(source, span.start),
        offset_to_position(source, span.end),
    )
}

/// The plugin root a manifest URI implies: info.toml sits AT the plugin
/// root, so its parent directory is that root by definition. `None` for a
/// URI with no local path — a `.op` archive entry or an unsaved buffer,
/// where there is no directory to resolve `exports` against.
fn manifest_workspace_root(uri: &Url) -> Option<PathBuf> {
    let path = uri.to_file_path().ok()?;
    path.parent().map(Path::to_path_buf)
}

/// Manifest rules for a document addressed by URI: the plugin root comes from
/// the URI, the export-file search paths from `config`.
///
/// B143: these MUST be the same list the loader resolves with. Before B143
/// this hardcoded `with_defaults()`, so a workspace or user-global
/// `plugin_files_search_paths` moved the loader but not this check — and the
/// LSP would squiggle "Export file not found" on an info.toml entry whose file
/// the very same session had loaded and pooled.
pub fn manifest_diagnostics_for_uri(
    uri: &Url,
    source: &str,
    config: &LspConfig,
) -> Vec<Diagnostic> {
    let root = manifest_workspace_root(uri);
    let search_paths = crate::workspace::load::DependencySearch::with_defaults()
        .finalize_with_config(config)
        .plugin_files_search_paths;
    let workspace = root.as_deref().map(|root| ManifestWorkspace {
        root,
        plugin_files_search_paths: &search_paths,
    });
    manifest_rule_diagnostics(source, workspace.as_ref())
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

/// Diagnostics that grey out the `#if` branches the current defines switch
/// off, one per contiguous dead body (see
/// [`crate::preprocessor::PreprocessResult::inactive_regions`]).
///
/// LSP-ONLY. Never call this from `check` or the TUI: a plugin with a few
/// hundred `#if MP4` lines would drown the CLI output, and HINT severity does
/// not save us there because `check` prints hints. The greying is a
/// live-editor affordance, not a finding.
///
/// The fade comes from [`DiagnosticTag::UNNECESSARY`], not the severity —
/// HINT alone would render as an ordinary (if quiet) squiggle, and VS Code
/// gates the grey-out on `editor.showUnused`.
pub fn inactive_region_diagnostics(
    source: &str,
    analysis: &crate::analysis::DocumentAnalysis,
    level: InactiveLevel,
) -> Vec<Diagnostic> {
    let Some(severity) = inactive_severity(level) else {
        return Vec::new();
    };
    analysis
        .preprocess
        .inactive_regions
        .iter()
        .map(|span| Diagnostic {
            range: span_to_range(source, *span),
            severity: Some(severity),
            message: "inactive code: the current preprocessor defines switch this branch off \
                      (see `defines` / `game_target` / `signature_mode`)"
                .to_string(),
            source: Some("openplanet-lsp".to_string()),
            code: Some(NumberOrString::String("inactive-region".to_string())),
            tags: Some(vec![DiagnosticTag::UNNECESSARY]),
            ..Default::default()
        })
        .collect()
}

fn inactive_severity(level: InactiveLevel) -> Option<DiagnosticSeverity> {
    match level {
        InactiveLevel::Ignore => None,
        InactiveLevel::Hint => Some(DiagnosticSeverity::HINT),
        InactiveLevel::Info => Some(DiagnosticSeverity::INFORMATION),
    }
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
        // B090: the CLI summary's kind breakdown reads this — the kind must
        // travel as data because parsing it back out of the message prose
        // is forbidden (same doctrine as removed_api replacements).
        data: Some(serde_json::json!({ "unusedKind": unused.kind })),
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
        Position::new(line as u32, line_text.encode_utf16().count() as u32),
    )
}

pub fn span_to_range(source: &str, span: crate::lexer::Span) -> Range {
    let start = offset_to_position(source, span.start as usize);
    let end = offset_to_position(source, span.end as usize);
    Range::new(start, end)
}

pub fn offset_to_position(source: &str, offset: usize) -> Position {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let prefix = &source[..offset];
    let line = prefix.matches('\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |nl| nl + 1);
    let col = source[line_start..offset].encode_utf16().count();
    Position::new(line as u32, col as u32)
}

pub fn position_to_offset(source: &str, pos: Position) -> usize {
    let line_start = if pos.line == 0 {
        Some(0)
    } else {
        source
            .match_indices('\n')
            .nth(pos.line as usize - 1)
            .map(|(offset, _)| offset + 1)
    };
    let Some(line_start) = line_start else {
        return source.len();
    };
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |offset| line_start + offset);
    let mut utf16 = 0u32;
    for (relative, ch) in source[line_start..line_end].char_indices() {
        let width = ch.len_utf16() as u32;
        if pos.character < utf16 + width {
            return line_start + relative;
        }
        utf16 += width;
        if pos.character == utf16 {
            return line_start + relative + ch.len_utf8();
        }
    }
    line_end
}

/// Adapt an LSP UTF-16 range to the CLI's historical UTF-8 byte columns.
/// The checker output contract predates the LSP server and remains byte-based.
pub fn range_to_utf8_columns(source: &str, range: Range) -> Range {
    fn position(source: &str, lsp: Position) -> Position {
        let offset = position_to_offset(source, lsp);
        let prefix = &source[..offset];
        let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
        Position::new(lsp.line, (offset - line_start) as u32)
    }

    Range::new(position(source, range.start), position(source, range.end))
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
        let search = [std::path::PathBuf::from("src")];
        let workspace = ManifestWorkspace {
            root: Path::new("/tmp"),
            plugin_files_search_paths: &search,
        };
        let diagnostics = compute_manifest_diagnostics_in_workspace(
            "[meta]\nname = \"Missing Version\"\n",
            &workspace,
            &["MissingDep".to_string()],
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("[meta].version")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("MissingDep")));
    }

    /// B040: the manifest seam both `check` and the LSP publish through.
    /// Severity is the contract with the CLI exit code — Error fails a run,
    /// Warning does not — so pin it here rather than only end to end.
    #[test]
    fn manifest_rule_severities_map_error_and_warning() {
        let source = "[meta]\nname = \"T\"\nperms = \"full\"\n";
        let diagnostics = manifest_rule_diagnostics(source, None);
        let by_message = |needle: &str| {
            diagnostics
                .iter()
                .find(|d| d.message.contains(needle))
                .unwrap_or_else(|| panic!("no diagnostic mentioning {needle}: {diagnostics:?}"))
        };
        assert_eq!(
            by_message("version").severity,
            Some(DiagnosticSeverity::ERROR),
            "a plugin that cannot load is not a passing check"
        );
        assert_eq!(
            by_message("deprecated").severity,
            Some(DiagnosticSeverity::WARNING)
        );
        for diagnostic in &diagnostics {
            assert_eq!(diagnostic.source.as_deref(), Some(DIAGNOSTIC_SOURCE));
            assert_eq!(
                diagnostic.code,
                Some(NumberOrString::String("manifest".into()))
            );
        }
    }

    #[test]
    fn a_valid_manifest_produces_no_rule_diagnostics() {
        let source = "[meta]\nname = \"T\"\nversion = \"1.0\"\n";
        assert_eq!(manifest_rule_diagnostics(source, None), Vec::new());
    }

    /// The manifest URI decides the plugin root, because info.toml sits AT
    /// the root. A URI with no local path (a `.op` entry) yields none, and
    /// the export rules are then skipped rather than guessed at.
    #[test]
    fn manifest_workspace_root_is_the_manifest_directory() {
        let uri = Url::parse("file:///plugins/Counter/info.toml").unwrap();
        assert_eq!(
            manifest_workspace_root(&uri).as_deref(),
            Some(Path::new("/plugins/Counter"))
        );
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(manifest_workspace_root(&uri), None);
    }

    fn dead_analysis(source: &str) -> crate::analysis::DocumentAnalysis {
        // No defines: every `#if` body below is dead.
        crate::analysis::DocumentAnalysis::analyze(source, &std::collections::HashSet::new())
    }

    #[test]
    fn inactive_region_diagnostics_follow_level() {
        let src = "#if MP4\nvoid Dead() {}\n#endif\nvoid Main() {}\n";
        let analysis = dead_analysis(src);
        for (level, want) in [
            (InactiveLevel::Ignore, None),
            (InactiveLevel::Hint, Some(DiagnosticSeverity::HINT)),
            (InactiveLevel::Info, Some(DiagnosticSeverity::INFORMATION)),
        ] {
            let diags = inactive_region_diagnostics(src, &analysis, level);
            match want {
                None => assert!(diags.is_empty(), "{level:?} must emit nothing"),
                Some(sev) => {
                    assert_eq!(diags.len(), 1, "{level:?}");
                    assert_eq!(diags[0].severity, Some(sev), "{level:?}");
                }
            }
        }
    }

    #[test]
    fn inactive_region_diagnostics_are_tagged_unnecessary_with_a_stable_code() {
        let src = "#if MP4\nvoid Dead() {}\n#endif\nvoid Main() {}\n";
        let analysis = dead_analysis(src);
        for level in [InactiveLevel::Hint, InactiveLevel::Info] {
            for diag in inactive_region_diagnostics(src, &analysis, level) {
                assert_eq!(
                    diag.tags,
                    Some(vec![DiagnosticTag::UNNECESSARY]),
                    "{level:?} must fade the range"
                );
                assert_eq!(
                    diag.code,
                    Some(NumberOrString::String("inactive-region".into())),
                    "{level:?}"
                );
                assert_eq!(diag.source, Some("openplanet-lsp".to_string()));
            }
        }
    }

    /// The range must cover exactly the dead body — not the directives, and
    /// not the live code after it.
    #[test]
    fn inactive_region_diagnostic_range_covers_the_dead_body_only() {
        let src = "#if MP4\nvoid Dead() {}\n#endif\nvoid Main() {}\n";
        let diags = inactive_region_diagnostics(src, &dead_analysis(src), InactiveLevel::Hint);
        assert_eq!(diags.len(), 1);
        let range = diags[0].range;
        assert_eq!(range.start, Position::new(1, 0), "starts at the dead body");
        assert_eq!(
            range.end,
            Position::new(1, "void Dead() {}".len() as u32),
            "ends at the last real character, not the newline"
        );
    }

    /// Generalise over what the dead branch contains: whole declarations,
    /// statements inside a live function, class members, globals, and a
    /// block nested in a namespace all fade the same way.
    #[test]
    fn inactive_region_diagnostics_generalise_over_what_is_dead() {
        for (label, src) in [
            ("whole function", "#if MP4\nvoid Dead() {}\n#endif\n"),
            (
                "statements in a live function",
                "void Main() {\n#if MP4\n    int dead = 1;\n#endif\n}\n",
            ),
            (
                "class member",
                "class C {\n#if MP4\n    int dead;\n#endif\n}\n",
            ),
            ("global", "#if MP4\nint dead = 1;\n#endif\n"),
            (
                "inside a namespace",
                "namespace N {\n#if MP4\n    void Dead() {}\n#endif\n}\n",
            ),
            (
                "else arm",
                "#if MP4\nvoid A() {}\n#else\nvoid B() {}\n#endif\n",
            ),
        ] {
            let diags = inactive_region_diagnostics(src, &dead_analysis(src), InactiveLevel::Hint);
            assert_eq!(diags.len(), 1, "{label}: one region");
            assert_eq!(diags[0].severity, Some(DiagnosticSeverity::HINT), "{label}");
            assert_eq!(
                diags[0].tags,
                Some(vec![DiagnosticTag::UNNECESSARY]),
                "{label}"
            );
        }
    }

    #[test]
    fn live_source_has_no_inactive_region_diagnostics() {
        for src in [
            "void Main() {}\n",
            "#if MP4\nvoid Dead() {}\n#endif\n", // dead, but see defines below
        ] {
            let analysis = crate::analysis::DocumentAnalysis::analyze(
                src,
                &["MP4".to_string()].into_iter().collect(),
            );
            assert!(
                inactive_region_diagnostics(src, &analysis, InactiveLevel::Hint).is_empty(),
                "nothing is switched off: {src:?}"
            );
        }
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
                // B090: the CLI summary's kind breakdown reads the
                // structured kind — prose parsing is forbidden, so it must
                // travel as data.
                assert_eq!(
                    diag.data,
                    Some(serde_json::json!({ "unusedKind": kind })),
                    "{kind} {sev:?} must carry the structured kind"
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
