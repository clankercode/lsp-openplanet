//! Shared test scaffolding for the code-action modules.
//!
//! Kept out of the `#[cfg(test)] mod tests` blocks so the seam's own tests
//! and each fix family's tests build their world the same way — a family
//! author writes assertions about *text*, not about `SymbolTable` wiring.

use tower_lsp::lsp_types::{Diagnostic, TextEdit, Url};

use crate::analysis::DocumentAnalysis;
use crate::server::code_actions::fix::FixCandidate;
use crate::server::diagnostics::position_to_offset;
use crate::symbols::SymbolTable;

/// Parse `source` into the trio a [`super::fix::FixContext`] needs: the
/// analysis, a one-file workspace symbol table, and the document URI.
pub fn ctx_for(source: &str) -> (DocumentAnalysis, SymbolTable, Url) {
    let analysis = DocumentAnalysis::analyze_plain(source);
    let mut workspace = SymbolTable::new();
    let fid = workspace.allocate_file_id();
    let syms = SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
    workspace.set_file_symbols(fid, syms);
    let uri = Url::parse("file:///test/a.as").expect("valid test uri");
    (analysis, workspace, uri)
}

/// The unused-declaration diagnostics the server really publishes for
/// `source`, straight through
/// [`crate::server::diagnostics::unused_declaration_diagnostics`].
///
/// Code-action tests must answer the diagnostics the USER sees, not a
/// synthetic stand-in that could drift from the published range, message or
/// `code` — the diagnostic-keyed registry keys on exactly those fields.
pub fn published_unused_diagnostics(source: &str) -> Vec<Diagnostic> {
    let path = std::path::PathBuf::from("/test/a.as");
    let snapshot = crate::analysis_snapshot::AnalysisSnapshot::from_files(
        &[(path, source.to_string())],
        &crate::config::LspConfig::default(),
    );
    let usage = crate::analysis::unused::WorkspaceUsage::collect(&snapshot, None);
    crate::server::diagnostics::unused_declaration_diagnostics(
        source,
        &usage,
        &snapshot,
        0,
        &std::collections::HashSet::new(),
        crate::config::UnusedLevel::Warn,
    )
}

/// Apply `edits` to `source` sequentially, exactly as a naive client would —
/// so a test that asserts the resulting text also proves the edit ordering
/// is safe.
pub fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut out = source.to_string();
    for edit in edits {
        let start = position_to_offset(&out, edit.range.start);
        let end = position_to_offset(&out, edit.range.end);
        out.replace_range(start..end, &edit.new_text);
    }
    out
}

/// The text after applying every non-conflicting fix in `candidates`.
pub fn apply_fixes(source: &str, candidates: &[FixCandidate]) -> String {
    let fixes: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| {
            let edits = candidate
                .fix
                .edit
                .changes
                .as_ref()?
                .values()
                .next()?
                .clone();
            Some(super::fix_all::DocumentFix {
                title: candidate.fix.title.clone(),
                edits,
            })
        })
        .collect();
    apply_edits(source, &super::fix_all::merged_edits(&fixes))
}
