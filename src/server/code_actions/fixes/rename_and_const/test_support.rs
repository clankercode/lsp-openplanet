//! Test scaffolding shared by the three B038 families.
//!
//! Deliberately local to this module rather than added to
//! `code_actions::test_fixtures`: that file is shared with the sibling
//! fix-family lanes, and these helpers are only useful here. The one thing
//! they add over `test_fixtures` is a **multi-file** workspace — the
//! const-receiver fix has to produce a `WorkspaceEdit` addressed at a file
//! other than the document the action was requested on, and
//! `test_fixtures::apply_fixes` collapses a multi-file edit to whichever
//! entry the `HashMap` iterator happens to yield first.

use tower_lsp::lsp_types::{TextEdit, Url, WorkspaceEdit};

use crate::analysis::DocumentAnalysis;
use crate::server::code_actions::fix::{
    collect_fix_descriptors, materialize_descriptor, FixCandidate, FixContext,
    WorkspaceFileResolver, REGISTRIES,
};
use crate::server::code_actions::test_fixtures::apply_edits;
use crate::symbols::SymbolTable;

/// A workspace of one or more analysed documents plus their pooled symbol
/// table — the shape a [`FixContext`] needs.
pub struct Workspace {
    pub docs: Vec<(Url, DocumentAnalysis)>,
    pub symbols: SymbolTable,
}

impl Workspace {
    pub fn new(files: &[(&str, &str)]) -> Self {
        let mut symbols = SymbolTable::new();
        let mut docs = Vec::new();
        for (uri, source) in files {
            let analysis = DocumentAnalysis::analyze_plain(source);
            let fid = symbols.allocate_file_id();
            let extracted =
                SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
            symbols.set_file_symbols(fid, extracted);
            docs.push((Url::parse(uri).expect("valid test uri"), analysis));
        }
        Self { docs, symbols }
    }

    pub fn single(source: &str) -> Self {
        Self::new(&[("file:///test/a.as", source)])
    }

    /// Borrowed `(uri, analysis)` pairs, as [`WorkspaceFileResolver`] wants.
    pub fn pairs(&self) -> Vec<(Url, &DocumentAnalysis)> {
        self.docs
            .iter()
            .map(|(uri, analysis)| (uri.clone(), analysis))
            .collect()
    }

    pub fn uri(&self, index: usize) -> &Url {
        &self.docs[index].0
    }

    pub fn source(&self, index: usize) -> &str {
        &self.docs[index].1.source
    }
}

/// Build a context for document `index` of `workspace`.
pub fn context<'a>(
    workspace: &'a Workspace,
    pairs: &'a [(Url, &'a DocumentAnalysis)],
    index: usize,
) -> FixContext<'a> {
    FixContext {
        uri: &workspace.docs[index].0,
        analysis: &workspace.docs[index].1,
        workspace: &workspace.symbols,
        type_index: None,
        files: WorkspaceFileResolver::new(pairs),
    }
}

/// Every fix every registered family offers for document `index`.
pub fn fixes_in(workspace: &Workspace, index: usize) -> Vec<FixCandidate> {
    let pairs = workspace.pairs();
    let ctx = context(workspace, &pairs, index);
    collect_fix_descriptors(&ctx)
        .iter()
        .map(|descriptor| materialize_descriptor(&ctx, descriptor, REGISTRIES))
        .collect()
}

/// Every fix offered for a one-file workspace holding `source`.
pub fn fixes_for(source: &str) -> Vec<FixCandidate> {
    fixes_in(&Workspace::single(source), 0)
}

/// The edits `edit` carries for `uri`, or an empty slice.
pub fn edits_for<'a>(edit: &'a WorkspaceEdit, uri: &Url) -> &'a [TextEdit] {
    edit.changes
        .as_ref()
        .and_then(|changes| changes.get(uri))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// `source` with `edit`'s edits for `uri` applied.
pub fn applied(source: &str, edit: &WorkspaceEdit, uri: &Url) -> String {
    let mut edits = edits_for(edit, uri).to_vec();
    // Apply back to front so earlier edits do not shift later offsets.
    edits
        .sort_by_key(|edit| std::cmp::Reverse((edit.range.start.line, edit.range.start.character)));
    apply_edits(source, &edits)
}

/// The URIs an edit touches, sorted for a stable assertion.
pub fn touched_uris(edit: &WorkspaceEdit) -> Vec<String> {
    let mut uris: Vec<String> = edit
        .changes
        .as_ref()
        .map(|changes| changes.keys().map(Url::to_string).collect())
        .unwrap_or_default();
    uris.sort();
    uris
}
