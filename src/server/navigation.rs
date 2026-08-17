//! Goto-definition and find-references for the LSP.
//!
//! Both features share a position → symbol-name lookup that walks the token
//! stream at the cursor, reconstructing any `Ns::Name` prefix. Definitions
//! are resolved against the checker's resolution surface ([`GlobalScope`])
//! built from the open documents, and references are located via a pragmatic
//! token-scan (no AST-aware shadowing).
//!
//! Since GH #40 every entry point consumes a [`DocumentAnalysis`] — the
//! per-file view (masked source + AST) — instead of re-lexing/parsing raw
//! source on its own.

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::lexer::{self, TokenKind};
use crate::server::diagnostics::{position_to_offset, span_to_range};
use crate::symbols::scope::Symbol;
use crate::typecheck::GlobalScope;

/// Per-file view used by navigation / call hierarchy / workspace symbols:
/// `file_id` → `(uri, analysis)`. Built from an [`AnalysisSnapshot`] so
/// cross-file features reuse the snapshot's parse instead of re-lexing
/// every workspace file per request.
pub struct WorkspaceFiles<'a> {
    pub files: &'a HashMap<usize, (Url, &'a DocumentAnalysis)>,
}

impl<'a> WorkspaceFiles<'a> {
    pub fn get(&self, fid: usize) -> Option<&(Url, &'a DocumentAnalysis)> {
        self.files.get(&fid)
    }

    /// Source text (masked) of `fid`'s analysis.
    pub fn source_of(&self, fid: usize) -> Option<&'a str> {
        self.files.get(&fid).map(|(_, a)| a.masked_source())
    }
}

/// Find the qualified identifier name at the given position. Returns `None`
/// if the cursor is not over an identifier.
///
/// When the cursor is on the `Name` in `Ns::Sub::Name`, the returned value is
/// `"Ns::Sub::Name"`. When the cursor is on `Sub`, it is `"Ns::Sub"`.
pub fn ident_span_at(
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<(u32, u32, String)> {
    let source = analysis.masked_source();
    let offset = position_to_offset(source, position);
    let tokens = &analysis.tokens;
    let (idx, token) = tokens.iter().enumerate().find(|(_, t)| {
        let start = t.span.start as usize;
        let end = t.span.end as usize;
        start <= offset && offset < end.max(start + 1)
    })?;
    if token.kind != TokenKind::Ident {
        return None;
    }
    let mut parts = vec![token.span.text(source).to_string()];
    let start = token.span.start;
    let end = token.span.end;
    let mut i = idx;
    while i >= 2
        && tokens[i - 1].kind == TokenKind::ColonColon
        && tokens[i - 2].kind == TokenKind::Ident
    {
        parts.push(tokens[i - 2].span.text(source).to_string());
        i -= 2;
    }
    parts.reverse();
    Some((start, end, parts.join("::")))
}

pub fn name_at_position(analysis: &DocumentAnalysis, position: Position) -> Option<String> {
    ident_span_at(analysis, position).map(|(_, _, name)| name)
}

/// Resolve the definition location of the symbol at `position`.
///
/// Looks up the qualified name in `workspace` first, then falls back to the
/// bare (last-segment) name. When candidates include both real definitions
/// and `import ... from` alias declarations, real definitions win; the
/// import site is used only when no definition exists in the workspace.
/// Returns `None` if the cursor is not on an identifier, no matching symbol
/// exists, or the owning file is not in `files`.
pub fn goto_definition(
    analysis: &DocumentAnalysis,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Option<Location> {
    let qual = name_at_position(analysis, position)?;
    let candidates = scope.lookup_reference(&qual);
    let sym = prefer_definition(&candidates)?;
    let (uri, def_analysis) = files.get(sym.file_id)?;
    Some(Location {
        uri: uri.clone(),
        range: span_to_range(def_analysis.masked_source(), sym.span),
    })
}

/// Pick the best candidate from a `lookup_reference` result: a real
/// definition if any exists, else the first import-site alias.
pub fn prefer_definition<'s>(candidates: &[&'s Symbol]) -> Option<&'s Symbol> {
    candidates
        .iter()
        .find(|s| !s.is_import_alias())
        .copied()
        .or_else(|| candidates.first().copied())
}

/// Build a workspace rename edit replacing every textual reference to the
/// identifier under `position` with `new_name`.
///
/// Uses the same token-scan strategy as [`find_references`] — every `Ident`
/// token in the open workspace whose text matches the cursor's bare name is
/// rewritten. Returns `None` if the cursor is not on an identifier or if no
/// matches were found.
pub fn rename(
    analysis: &DocumentAnalysis,
    position: Position,
    new_name: &str,
    files: &WorkspaceFiles,
) -> Option<WorkspaceEdit> {
    let qual = name_at_position(analysis, position)?;
    let bare = qual.rsplit("::").next().unwrap_or(&qual).to_string();
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for (_fid, (uri, analysis)) in files.files.iter() {
        let src = analysis.masked_source();
        let tokens = lexer::tokenize_filtered(src);
        let mut edits = Vec::new();
        for tok in &tokens {
            if tok.kind != TokenKind::Ident {
                continue;
            }
            if tok.span.text(src) == bare {
                edits.push(TextEdit {
                    range: span_to_range(src, tok.span),
                    new_text: new_name.to_string(),
                });
            }
        }
        if !edits.is_empty() {
            changes.insert(uri.clone(), edits);
        }
    }
    if changes.is_empty() {
        return None;
    }
    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// Find all references to the symbol at `position` across the workspace.
///
/// Pragmatic implementation: once the bare identifier is known, every file
/// in `files` is tokenized and every `Ident` whose text equals the bare name
/// is reported. `include_declaration` is accepted for API compatibility but
/// not acted on — clients can dedupe against the declaration site if they
/// care.
pub fn find_references(
    analysis: &DocumentAnalysis,
    position: Position,
    files: &WorkspaceFiles,
    _include_declaration: bool,
) -> Vec<Location> {
    let Some(qual) = name_at_position(analysis, position) else {
        return Vec::new();
    };
    let bare = qual.rsplit("::").next().unwrap_or(&qual);
    let mut results = Vec::new();
    for (_fid, (uri, analysis)) in files.files.iter() {
        let src = analysis.masked_source();
        let tokens = lexer::tokenize_filtered(src);
        for tok in &tokens {
            if tok.kind != TokenKind::Ident {
                continue;
            }
            if tok.span.text(src) == bare {
                results.push(Location {
                    uri: uri.clone(),
                    range: span_to_range(src, tok.span),
                });
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SymbolTable;

    fn build_single_file_workspace(
        uri_str: &str,
        source: &str,
    ) -> crate::server::test_support::TestWorkspace {
        let name = uri_str.rsplit('/').next().unwrap_or("a.as");
        crate::server::test_support::TestWorkspace::one_file(name, source)
    }

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn name_at_position_finds_bare_identifier() {
        let src = "void main() { Foo foo; }\n";
        let analysis = DocumentAnalysis::analyze_plain(src);
        // Cursor over `Foo` (decl type position).
        let name = name_at_position(&analysis, pos(0, 14)).unwrap();
        assert_eq!(name, "Foo");
    }

    #[test]
    fn name_at_position_reconstructs_qualified_name() {
        let src = "void main() { Ns::Sub::Name(); }\n";
        let analysis = DocumentAnalysis::analyze_plain(src);
        let name = name_at_position(&analysis, pos(0, 23)).unwrap();
        assert_eq!(name, "Ns::Sub::Name");
    }

    #[test]
    fn goto_definition_resolves_single_file() {
        let src = "int target = 1;\nvoid main() { int x = target; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let loc = goto_definition(analysis, pos(1, 26), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, tw.uri());
        // Points at the declaration (line 0), not the use.
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_prefers_definition_over_import() {
        // A name declared both by an `import ... from` statement (file A) and
        // a real function definition (file B) must resolve to the definition:
        // the import is just a local alias declaration for it. File A is
        // registered first, so a naive first-match lookup lands on the import.
        let imports_src = "import void DoThing(int x) from 'Other';\nvoid main() { DoThing(1); }\n";
        let defs_src = "void DoThing(int x) { }\n";
        let snap = crate::analysis_snapshot::AnalysisSnapshot::from_files(
            &[
                (
                    std::path::PathBuf::from("/test/imports.as"),
                    imports_src.to_string(),
                ),
                (
                    std::path::PathBuf::from("/test/defs.as"),
                    defs_src.to_string(),
                ),
            ],
            &crate::config::LspConfig::default(),
        );
        let files = snap.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = crate::typecheck::GlobalScope::new(snap.symbols(), None);
        let analysis = &snap.files()[0].analysis;
        // Cursor on the `DoThing` call site (line 1, char 14).
        let loc = goto_definition(analysis, pos(1, 14), &scope, &ws_files).unwrap();
        assert_eq!(
            loc.uri, files[&1].0,
            "must jump to the definition file, not the importing file"
        );
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_falls_back_to_import_when_no_definition() {
        // When the real definition is not part of the workspace, the import
        // site is still the best (only) answer.
        let src = "import void Ext(int x) from 'Other';\nvoid main() { Ext(1); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let loc = goto_definition(analysis, pos(1, 14), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, tw.uri());
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn find_references_scans_open_documents() {
        let src = "int counter = 0;\nvoid main() { counter += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let refs = find_references(analysis, pos(1, 15), &ws_files, true);
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn rename_rewrites_matching_identifiers() {
        let src = "int counter = 0;\nvoid main() { counter += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let edit = rename(analysis, pos(1, 15), "tally", &ws_files).unwrap();
        let changes = edit.changes.unwrap();
        let edits = changes.get(&tw.uri()).unwrap();
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|e| e.new_text == "tally"));
    }
}
