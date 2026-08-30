//! `textDocument/codeAction` and `codeAction/resolve` handlers.
//!
//! Three sources of actions, in the order the client sees them:
//!
//! 1. **"Did you mean X?" quick-fixes** driven by `unknown type` and
//!    `undefined identifier` diagnostics echoed back by the client.
//!    Candidates come from the workspace symbol table and the external type
//!    index (short-name map). Similarity is scored with Levenshtein distance
//!    (≤ 2), plus cheap fallbacks: case-insensitive substring containment and
//!    common-prefix length ≥ 3.
//! 2. **"Wrap in try/catch" refactor** — always offered regardless of the
//!    diagnostic context.
//! 3. **The fix seam** ([`fix`]) — registered fix families keyed on
//!    `TypeDiagnosticKind`, plus the batched `source.fixAll.openplanet`
//!    source action ([`fix_all`]). New fix families plug in at
//!    [`fixes::FAMILIES`] and touch nothing here.
//!
//! Seam actions are returned **without** their edit when the client
//! advertises `codeAction/resolve` support; the edit is materialised in
//! [`resolve_code_action`] for the single action the user picked. Producers
//! are pure, so re-entering them on resolve is cheap and cannot diverge.

pub mod fix;
pub mod fix_all;
pub mod fixes;
#[cfg(test)]
pub mod test_fixtures;
mod unused_decl;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::server::diagnostics::{position_to_offset, DIAGNOSTIC_SOURCE};
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub use fix::SOURCE_FIX_ALL_OPENPLANET;
use fix::{
    collect_fix_descriptors_from, kind_selected, materialize_auto_descriptors,
    materialize_descriptor, materialize_fix_from, FixCandidate, FixContext, FixDescriptorCandidate,
    FixFamily, WorkspaceFileResolver,
};

/// Tag stamped into `CodeAction.data` so [`resolve_code_action`] never tries
/// to interpret another provider's payload.
const FIX_DATA_TAG: &str = "openplanet.codeAction.fix";
type SourceFingerprint = [u8; 32];

/// Enough to re-derive one seam fix during `codeAction/resolve` without
/// depending on the order actions happened to be returned in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveData {
    tag: String,
    uri: Url,
    source_fingerprint: SourceFingerprint,
    #[serde(flatten)]
    action: ResolveAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ResolveAction {
    Fix {
        family: String,
        diagnostic_range: Range,
        title: String,
    },
    FixAll,
    DidYouMean {
        range: Range,
        new_text: String,
    },
    WrapInTryCatch {
        range: Range,
    },
}

/// The kinds this server can produce, and the promise that it resolves
/// edits lazily.
///
/// `source.fixAll.openplanet` is namespaced deliberately: it lets a user put
/// exactly our fixes in `editor.codeActionsOnSave` without opting into every
/// other language server's `source.fixAll`. It is never applied by default —
/// a checker warning is a suggestion, and silently rewriting a signature on
/// save is worse than the warning it removes (B035 decision).
pub fn code_action_provider() -> CodeActionProviderCapability {
    CodeActionProviderCapability::Options(CodeActionOptions {
        code_action_kinds: Some(vec![
            CodeActionKind::QUICKFIX,
            CodeActionKind::REFACTOR_REWRITE,
            CodeActionKind::SOURCE_FIX_ALL,
            SOURCE_FIX_ALL_OPENPLANET,
        ]),
        work_done_progress_options: WorkDoneProgressOptions::default(),
        resolve_provider: Some(true),
    })
}

/// One `textDocument/codeAction` request, as the handler sees it.
pub struct CodeActionRequest<'a> {
    pub uri: &'a Url,
    pub analysis: &'a DocumentAnalysis,
    /// The range the client asked about (cursor or selection).
    pub range: Range,
    pub context: &'a CodeActionContext,
    pub workspace: &'a SymbolTable,
    pub type_index: Option<&'a TypeIndex>,
    pub files: WorkspaceFileResolver<'a>,
    /// The client advertised `codeAction.dataSupport` + `resolveSupport`, so
    /// seam actions may be returned without their `edit`. Clients that did
    /// not must get the edit up front or the action would silently do
    /// nothing when picked.
    pub lazy_edits: bool,
}

/// Entry point invoked by the LSP `code_action` handler.
pub fn code_actions(req: &CodeActionRequest<'_>) -> Vec<CodeActionOrCommand> {
    code_actions_with_families(req, fixes::FAMILIES)
}

fn code_actions_with_families(
    req: &CodeActionRequest<'_>,
    families: &[FixFamily],
) -> Vec<CodeActionOrCommand> {
    let only = req.context.only.as_deref();
    let mut actions: Vec<CodeAction> = Vec::new();
    // Every action from one response represents this same document revision.
    // Carry the digest into its data instead of re-hashing per action.
    let source_fingerprint = source_fingerprint(&req.analysis.source);

    if kind_selected(&CodeActionKind::QUICKFIX, only) {
        actions.extend(quick_fix_did_you_mean(
            req.uri,
            source_fingerprint,
            &req.context.diagnostics,
            req.workspace,
            req.type_index,
            req.lazy_edits,
        ));
        actions.extend(unused_decl::quick_fixes(req));
    }
    if kind_selected(&CodeActionKind::REFACTOR_REWRITE, only) {
        actions.extend(refactor_wrap_in_try_catch(
            req.uri,
            &req.analysis.source,
            source_fingerprint,
            req.range,
            req.lazy_edits,
        ));
    }

    let wants_registered_quickfix = kind_selected(&CodeActionKind::QUICKFIX, only);
    let wants_fix_all = kind_selected(&SOURCE_FIX_ALL_OPENPLANET, only);
    if req.lazy_edits && (wants_registered_quickfix || wants_fix_all) {
        let ctx = FixContext {
            uri: req.uri,
            analysis: req.analysis,
            workspace: req.workspace,
            type_index: req.type_index,
            files: req.files,
        };
        let descriptors = collect_fix_descriptors_from(&ctx, families);
        if wants_registered_quickfix {
            for candidate in &descriptors {
                if ranges_touch(&candidate.diagnostic.range, &req.range) {
                    actions.push(descriptor_action(req, candidate, source_fingerprint));
                }
            }
        }
        if wants_fix_all {
            let candidates = materialize_auto_descriptors(&ctx, &descriptors, families);
            if fix_all::fix_all_action(req.uri, &candidates).is_some() {
                actions.push(deferred_fix_all_action(req.uri, source_fingerprint));
            }
        }
    } else if !req.lazy_edits && (wants_registered_quickfix || wants_fix_all) {
        let ctx = FixContext {
            uri: req.uri,
            analysis: req.analysis,
            workspace: req.workspace,
            type_index: req.type_index,
            files: req.files,
        };
        let descriptors = collect_fix_descriptors_from(&ctx, families);
        let candidates: Vec<_> = descriptors
            .iter()
            .map(|descriptor| materialize_descriptor(&ctx, descriptor, families))
            .collect();
        if wants_registered_quickfix {
            for candidate in &candidates {
                if ranges_touch(&candidate.diagnostic.range, &req.range) {
                    actions.push(seam_action(req, candidate, source_fingerprint));
                }
            }
        }
        if wants_fix_all {
            if let Some(mut action) = fix_all::fix_all_action(req.uri, &candidates) {
                action.data = resolve_data(req.uri, source_fingerprint, ResolveAction::FixAll);
                actions.push(action);
            }
        }
    }

    actions
        .into_iter()
        .filter(|a| a.kind.as_ref().is_none_or(|k| kind_selected(k, only)))
        .map(CodeActionOrCommand::CodeAction)
        .collect()
}

/// `codeAction/resolve`: materialise the edit for one previously offered
/// seam action. An action we did not produce — or one that already carries
/// an edit — is returned untouched rather than rejected, because the client
/// is free to resolve anything it was given.
pub fn resolve_code_action(
    action: CodeAction,
    analysis: &DocumentAnalysis,
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
    files: WorkspaceFileResolver<'_>,
) -> CodeAction {
    resolve_code_action_with_families(
        action,
        analysis,
        workspace,
        type_index,
        files,
        fixes::FAMILIES,
    )
}

fn resolve_code_action_with_families(
    mut action: CodeAction,
    analysis: &DocumentAnalysis,
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
    files: WorkspaceFileResolver<'_>,
    families: &[FixFamily],
) -> CodeAction {
    if action.edit.is_some() {
        return action;
    }
    let Some(resolve) = parse_resolve_data(&action) else {
        return action;
    };
    if resolve.source_fingerprint != source_fingerprint(&analysis.source) {
        return action;
    }
    match resolve.action {
        ResolveAction::Fix {
            family,
            diagnostic_range,
            title,
        } => {
            let ctx = FixContext {
                uri: &resolve.uri,
                analysis,
                workspace,
                type_index,
                files,
            };
            if let Some(candidate) =
                materialize_fix_from(&ctx, &family, diagnostic_range, &title, families)
            {
                action.edit = Some(candidate.fix.edit);
            }
        }
        ResolveAction::FixAll => {
            let ctx = FixContext {
                uri: &resolve.uri,
                analysis,
                workspace,
                type_index,
                files,
            };
            // Only the auto-applicable half is worth materializing here:
            // the batch discards everything else anyway.
            let descriptors = collect_fix_descriptors_from(&ctx, families);
            let candidates = materialize_auto_descriptors(&ctx, &descriptors, families);
            if let Some(resolved) = fix_all::fix_all_action(&resolve.uri, &candidates) {
                action.edit = resolved.edit;
            }
        }
        ResolveAction::DidYouMean { range, new_text } => {
            action.edit = Some(single_file_edit(
                &resolve.uri,
                vec![TextEdit { range, new_text }],
            ));
        }
        ResolveAction::WrapInTryCatch { range } => {
            action.edit = Some(wrap_in_try_catch_edit(
                &resolve.uri,
                &analysis.source,
                range,
            ));
        }
    }
    action
}

/// Document a `codeAction/resolve` request needs loaded, if any. `None` for
/// an action that is not ours to resolve.
pub fn resolve_target_uri(action: &CodeAction) -> Option<Url> {
    parse_resolve_data(action).map(|r| r.uri)
}

fn parse_resolve_data(action: &CodeAction) -> Option<ResolveData> {
    let data = action.data.clone()?;
    let resolve: ResolveData = serde_json::from_value(data).ok()?;
    (resolve.tag == FIX_DATA_TAG).then_some(resolve)
}

fn source_fingerprint(source: &str) -> SourceFingerprint {
    Sha256::digest(source.as_bytes()).into()
}

fn resolve_data(
    uri: &Url,
    source_fingerprint: SourceFingerprint,
    action: ResolveAction,
) -> Option<serde_json::Value> {
    serde_json::to_value(ResolveData {
        tag: FIX_DATA_TAG.to_string(),
        uri: uri.clone(),
        source_fingerprint,
        action,
    })
    .ok()
}

fn seam_action(
    req: &CodeActionRequest<'_>,
    candidate: &FixCandidate,
    source_fingerprint: SourceFingerprint,
) -> CodeAction {
    let edit = candidate.fix.edit.clone();
    let data = resolve_data(
        req.uri,
        source_fingerprint,
        ResolveAction::Fix {
            family: candidate.family.to_string(),
            diagnostic_range: candidate.diagnostic.range,
            title: candidate.fix.title.clone(),
        },
    );
    CodeAction {
        title: candidate.fix.title.clone(),
        kind: Some(candidate.fix.kind.clone()),
        diagnostics: Some(vec![client_diagnostic(req, candidate)]),
        edit: (!req.lazy_edits).then_some(edit),
        command: None,
        is_preferred: candidate.fix.is_preferred.then_some(true),
        disabled: None,
        data,
    }
}

fn descriptor_action(
    req: &CodeActionRequest<'_>,
    candidate: &FixDescriptorCandidate,
    source_fingerprint: SourceFingerprint,
) -> CodeAction {
    CodeAction {
        title: candidate.descriptor.title.clone(),
        kind: Some(candidate.descriptor.kind.clone()),
        diagnostics: Some(vec![client_descriptor_diagnostic(req, candidate)]),
        edit: None,
        command: None,
        is_preferred: candidate.descriptor.is_preferred.then_some(true),
        disabled: None,
        data: resolve_data(
            req.uri,
            source_fingerprint,
            ResolveAction::Fix {
                family: candidate.family.to_string(),
                diagnostic_range: candidate.diagnostic.range,
                title: candidate.descriptor.title.clone(),
            },
        ),
    }
}

fn deferred_fix_all_action(uri: &Url, source_fingerprint: SourceFingerprint) -> CodeAction {
    CodeAction {
        title: "Fix all auto-fixable Openplanet problems".to_string(),
        kind: Some(SOURCE_FIX_ALL_OPENPLANET),
        diagnostics: None,
        edit: None,
        command: None,
        is_preferred: None,
        disabled: None,
        data: resolve_data(uri, source_fingerprint, ResolveAction::FixAll),
    }
}

/// Prefer the client's own copy of the diagnostic (it may carry fields we
/// never sent, and matching by identity is what ties the action to the
/// squiggle); fall back to the form we published.
fn client_diagnostic(req: &CodeActionRequest<'_>, candidate: &FixCandidate) -> Diagnostic {
    req.context
        .diagnostics
        .iter()
        .find(|d| {
            d.range == candidate.diagnostic.range && d.message == candidate.diagnostic.message
        })
        .cloned()
        .unwrap_or_else(|| candidate.diagnostic.clone())
}

fn client_descriptor_diagnostic(
    req: &CodeActionRequest<'_>,
    candidate: &FixDescriptorCandidate,
) -> Diagnostic {
    req.context
        .diagnostics
        .iter()
        .find(|d| {
            d.range == candidate.diagnostic.range && d.message == candidate.diagnostic.message
        })
        .cloned()
        .unwrap_or_else(|| candidate.diagnostic.clone())
}

fn single_file_edit(uri: &Url, edits: Vec<TextEdit>) -> WorkspaceEdit {
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    changes.insert(uri.clone(), edits);
    WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }
}

/// Inclusive intersection: a zero-width cursor sitting at either end of a
/// diagnostic still counts as "on" it, which is what the lightbulb needs.
fn ranges_touch(a: &Range, b: &Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// Is this diagnostic one of ours? A diagnostic published by another server
/// must never drive our quick fixes, even when the message happens to match.
/// A missing `source` is treated as ours: some clients drop the field on the
/// round trip, and refusing would silently disable the feature.
fn is_our_diagnostic(diag: &Diagnostic) -> bool {
    diag.source
        .as_deref()
        .is_none_or(|s| s == DIAGNOSTIC_SOURCE)
}

fn quick_fix_did_you_mean(
    uri: &Url,
    source_fingerprint: SourceFingerprint,
    diagnostics: &[Diagnostic],
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
    lazy_edits: bool,
) -> Vec<CodeAction> {
    let mut out = Vec::new();
    for diag in diagnostics {
        if !is_our_diagnostic(diag) {
            continue;
        }
        if !is_did_you_mean_candidate(&diag.message) {
            continue;
        }
        let Some(needle) = extract_name_from_diagnostic(&diag.message) else {
            continue;
        };
        if needle.is_empty() {
            continue;
        }
        let candidates = find_candidates(needle, workspace, type_index);
        for (i, (name, _dist)) in candidates.iter().enumerate() {
            out.push(CodeAction {
                title: format!("Did you mean '{}'?", name),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: (!lazy_edits).then(|| {
                    single_file_edit(
                        uri,
                        vec![TextEdit {
                            range: diag.range,
                            new_text: name.clone(),
                        }],
                    )
                }),
                command: None,
                is_preferred: Some(i == 0),
                disabled: None,
                data: resolve_data(
                    uri,
                    source_fingerprint,
                    ResolveAction::DidYouMean {
                        range: diag.range,
                        new_text: name.clone(),
                    },
                ),
            });
        }
    }
    out
}

fn wrap_in_try_catch_edit(uri: &Url, source: &str, range: Range) -> WorkspaceEdit {
    let start_off = position_to_offset(source, range.start);
    let end_off = position_to_offset(source, range.end);
    let (start_off, end_off) = if start_off <= end_off {
        (start_off, end_off)
    } else {
        (end_off, start_off)
    };
    let selected = source.get(start_off..end_off).unwrap_or("");
    let new_text = format!("try {{\n    {}\n}} catch {{ }}\n", selected);

    single_file_edit(uri, vec![TextEdit { range, new_text }])
}

fn refactor_wrap_in_try_catch(
    uri: &Url,
    source: &str,
    source_fingerprint: SourceFingerprint,
    range: Range,
    lazy_edits: bool,
) -> Vec<CodeAction> {
    vec![CodeAction {
        title: "Wrap in try/catch".to_string(),
        kind: Some(CodeActionKind::REFACTOR_REWRITE),
        diagnostics: None,
        edit: (!lazy_edits).then(|| wrap_in_try_catch_edit(uri, source, range)),
        command: None,
        is_preferred: None,
        disabled: None,
        data: resolve_data(
            uri,
            source_fingerprint,
            ResolveAction::WrapInTryCatch { range },
        ),
    }]
}

fn is_did_you_mean_candidate(message: &str) -> bool {
    message.starts_with("unknown type `") || message.starts_with("undefined identifier `")
}

fn extract_name_from_diagnostic(message: &str) -> Option<&str> {
    let start = message.find('`')? + 1;
    let rest = &message[start..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

/// Bounded Levenshtein distance. Returns a large sentinel for over-long
/// inputs so the caller naturally skips them.
fn levenshtein(a: &str, b: &str) -> usize {
    if a.len() > 30 || b.len() > 30 {
        return 999;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 0..m {
        curr[0] = i + 1;
        for j in 0..n {
            let cost = if a_chars[i] == b_chars[j] { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn is_similar(needle: &str, candidate: &str) -> Option<usize> {
    let needle_lower = needle.to_lowercase();
    let cand_lower = candidate.to_lowercase();
    let dist = levenshtein(&needle_lower, &cand_lower);
    if dist <= 2 {
        return Some(dist);
    }
    if needle.len() >= 3 && candidate.len() >= 3 && cand_lower.contains(&needle_lower) {
        return Some(dist);
    }
    if common_prefix_len(&needle_lower, &cand_lower) >= 3 {
        return Some(dist);
    }
    None
}

fn find_candidates(
    needle: &str,
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
) -> Vec<(String, usize)> {
    let mut candidates: Vec<(String, usize)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Workspace symbols: use the tail segment ("Foo" from "Ns::Foo").
    for sym in workspace.all_symbols() {
        let bare = sym.name.rsplit("::").next().unwrap_or(&sym.name);
        if bare == needle {
            continue;
        }
        if let Some(dist) = is_similar(needle, bare) {
            if seen.insert(bare.to_string()) {
                candidates.push((bare.to_string(), dist));
            }
        }
    }

    // External type index short names (types + enums).
    if let Some(idx) = type_index {
        for short in idx.all_short_names() {
            if short == needle {
                continue;
            }
            if let Some(dist) = is_similar(needle, &short) {
                if seen.insert(short.clone()) {
                    candidates.push((short, dist));
                }
            }
        }
    }

    candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    candidates.truncate(3);
    candidates
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{apply_edits, ctx_for};
    use super::*;
    use crate::lexer::Span;
    use crate::symbols::scope::{Symbol, SymbolKind};
    use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

    fn mk_range(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
        Range::new(Position::new(l0, c0), Position::new(l1, c1))
    }

    fn whole_doc(source: &str) -> Range {
        let lines = source.lines().count().max(1) as u32;
        mk_range(0, 0, lines, 0)
    }

    fn mk_diag(range: Range, message: &str) -> Diagnostic {
        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: message.to_string(),
            source: Some(DIAGNOSTIC_SOURCE.to_string()),
            ..Default::default()
        }
    }

    fn workspace_with_class(name: &str) -> SymbolTable {
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        let sym = Symbol {
            name: name.to_string(),
            kind: SymbolKind::Class {
                parents: Vec::new(),
            },
            span: Span { start: 0, end: 0 },
            file_id: fid,
            doc: None,
        };
        table.set_file_symbols(fid, vec![sym]);
        table
    }

    struct Harness {
        analysis: DocumentAnalysis,
        workspace: SymbolTable,
        uri: Url,
        source: String,
    }

    impl Harness {
        fn new(source: &str) -> Self {
            let (analysis, workspace, uri) = ctx_for(source);
            Self {
                analysis,
                workspace,
                uri,
                source: source.to_string(),
            }
        }

        fn with_workspace(source: &str, workspace: SymbolTable) -> Self {
            let mut h = Self::new(source);
            h.workspace = workspace;
            h
        }

        fn request(&self, ctx: CodeActionContext, range: Range, lazy: bool) -> Vec<CodeAction> {
            let files = [(self.uri.clone(), &self.analysis)];
            code_actions(&CodeActionRequest {
                uri: &self.uri,
                analysis: &self.analysis,
                range,
                context: &ctx,
                workspace: &self.workspace,
                type_index: None,
                files: WorkspaceFileResolver::new(&files),
                lazy_edits: lazy,
            })
            .into_iter()
            .map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => ca,
                CodeActionOrCommand::Command(c) => panic!("unexpected bare Command: {:?}", c),
            })
            .collect()
        }

        /// The common case: whole document, no diagnostics echoed, lazy.
        fn all(&self) -> Vec<CodeAction> {
            self.request(CodeActionContext::default(), whole_doc(&self.source), true)
        }

        fn with_diags(&self, diagnostics: Vec<Diagnostic>) -> Vec<CodeAction> {
            self.request(
                CodeActionContext {
                    diagnostics,
                    ..Default::default()
                },
                whole_doc(&self.source),
                true,
            )
        }

        fn resolve(&self, action: CodeAction) -> CodeAction {
            let files = [(self.uri.clone(), &self.analysis)];
            resolve_code_action(
                action,
                &self.analysis,
                &self.workspace,
                None,
                WorkspaceFileResolver::new(&files),
            )
        }
    }

    fn titles(actions: &[CodeAction]) -> Vec<String> {
        actions.iter().map(|a| a.title.clone()).collect()
    }

    fn of_kind<'a>(actions: &'a [CodeAction], kind: &CodeActionKind) -> Vec<&'a CodeAction> {
        actions
            .iter()
            .filter(|a| a.kind.as_ref() == Some(kind))
            .collect()
    }

    fn edits_of<'a>(action: &'a CodeAction, uri: &Url) -> &'a [TextEdit] {
        action
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .and_then(|c| c.get(uri))
            .expect("workspace edit for uri")
    }

    fn unbatchable_auto_matches(kind: &TypeDiagnosticKind) -> bool {
        matches!(kind, TypeDiagnosticKind::StringByValueParam { .. })
    }

    fn unbatchable_auto_describe(
        _: &TypeDiagnostic,
        _: &FixContext<'_>,
    ) -> Vec<fix::FixDescriptor> {
        vec![fix::FixDescriptor {
            title: "Unbatchable auto fix".to_string(),
            kind: CodeActionKind::QUICKFIX,
            is_preferred: false,
            auto_applicable: true,
        }]
    }

    fn unbatchable_auto_materialize(
        _: &TypeDiagnostic,
        ctx: &FixContext<'_>,
        _: &str,
    ) -> WorkspaceEdit {
        let other = Url::parse("file:///test/other.as").unwrap();
        let mut changes = HashMap::new();
        changes.insert(
            ctx.uri.clone(),
            vec![TextEdit::new(Range::default(), "here".into())],
        );
        changes.insert(other, vec![TextEdit::new(Range::default(), "there".into())]);
        WorkspaceEdit::new(changes)
    }

    const UNBATCHABLE_AUTO_FAMILY: fix::FixFamily = fix::FixFamily {
        name: "test-unbatchable-auto",
        matches: unbatchable_auto_matches,
        describe: unbatchable_auto_describe,
        materialize: unbatchable_auto_materialize,
    };

    fn descriptor_owned_matches(kind: &TypeDiagnosticKind) -> bool {
        matches!(kind, TypeDiagnosticKind::StringByValueParam { .. })
    }

    fn descriptor_owned_describe(
        _: &TypeDiagnostic,
        _: &FixContext<'_>,
    ) -> Vec<fix::FixDescriptor> {
        vec![fix::FixDescriptor {
            title: "Descriptor-owned synthetic fix".to_string(),
            kind: CodeActionKind::QUICKFIX,
            is_preferred: false,
            auto_applicable: false,
        }]
    }

    fn descriptor_owned_materialize(
        _: &TypeDiagnostic,
        ctx: &FixContext<'_>,
        _: &str,
    ) -> WorkspaceEdit {
        let mut changes = HashMap::new();
        changes.insert(
            ctx.uri.clone(),
            vec![TextEdit::new(
                mk_range(0, 7, 0, 13),
                "const string &in".into(),
            )],
        );
        WorkspaceEdit::new(changes)
    }

    const DESCRIPTOR_OWNED_FAMILY: fix::FixFamily = fix::FixFamily {
        name: "test-descriptor-owned",
        matches: descriptor_owned_matches,
        describe: descriptor_owned_describe,
        materialize: descriptor_owned_materialize,
    };

    // ── Capability ────────────────────────────────────────────────────────

    #[test]
    fn capability_advertises_quickfix_and_fixall_kinds() {
        let CodeActionProviderCapability::Options(opts) = code_action_provider() else {
            panic!("expected CodeActionOptions, not Simple(bool)");
        };
        let kinds = opts
            .code_action_kinds
            .expect("code_action_kinds advertised");
        for want in [
            CodeActionKind::QUICKFIX,
            CodeActionKind::REFACTOR_REWRITE,
            CodeActionKind::SOURCE_FIX_ALL,
            SOURCE_FIX_ALL_OPENPLANET,
        ] {
            assert!(
                kinds.contains(&want),
                "missing kind {:?} in {:?}",
                want,
                kinds
            );
        }
        assert_eq!(opts.resolve_provider, Some(true));
    }

    #[test]
    fn capability_fix_all_kind_is_namespaced_to_openplanet() {
        assert_eq!(
            SOURCE_FIX_ALL_OPENPLANET.as_str(),
            "source.fixAll.openplanet"
        );
    }

    // ── Lazy resolve ──────────────────────────────────────────────────────

    #[test]
    fn code_action_resolve_computes_edit_lazily() {
        let h = Harness::new("void f(string s) {}");
        let actions = h.all();
        let quickfixes = of_kind(&actions, &CodeActionKind::QUICKFIX);
        assert_eq!(quickfixes.len(), 1, "titles: {:?}", titles(&actions));
        let offered = quickfixes[0].clone();
        assert!(
            offered.edit.is_none(),
            "seam action must be offered without an edit"
        );
        assert!(
            offered.data.is_some(),
            "seam action must carry resolve data"
        );

        let resolved = h.resolve(offered);
        let edits = edits_of(&resolved, &h.uri);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "const string &in");
        assert_eq!(
            apply_edits(&h.source, edits),
            "void f(const string &in s) {}"
        );
    }

    #[test]
    fn resolving_client_defers_and_resolves_every_action_class() {
        let source = "void f(string s) { Foa x; }";
        let table = workspace_with_class("Foo");
        let h = Harness::with_workspace(source, table);
        let typo_start = source.find("Foa").unwrap() as u32;
        let actions = h.with_diags(vec![mk_diag(
            mk_range(0, typo_start, 0, typo_start + 3),
            "unknown type `Foa`",
        )]);

        for (name, action) in [
            (
                "registered quick fix",
                actions
                    .iter()
                    .find(|a| a.title.starts_with("Pass `s`"))
                    .expect("registered quick fix"),
            ),
            (
                "did you mean",
                actions
                    .iter()
                    .find(|a| a.title == "Did you mean 'Foo'?")
                    .expect("did-you-mean action"),
            ),
            (
                "refactor",
                actions
                    .iter()
                    .find(|a| a.kind == Some(CodeActionKind::REFACTOR_REWRITE))
                    .expect("refactor action"),
            ),
            (
                "fix all",
                actions
                    .iter()
                    .find(|a| a.kind == Some(SOURCE_FIX_ALL_OPENPLANET))
                    .expect("fix-all action"),
            ),
        ] {
            assert!(action.edit.is_none(), "{name} must be cheap to list");
            assert!(action.data.is_some(), "{name} must carry resolve data");
            assert!(
                h.resolve(action.clone()).edit.is_some(),
                "{name} must materialize on resolve"
            );
        }

        let eager = h.request(
            CodeActionContext {
                diagnostics: vec![mk_diag(
                    mk_range(0, typo_start, 0, typo_start + 3),
                    "unknown type `Foa`",
                )],
                ..Default::default()
            },
            whole_doc(source),
            false,
        );
        assert!(
            eager.iter().all(|action| action.edit.is_some()),
            "non-resolving clients need eager edits for every class: {:?}",
            titles(&eager)
        );
    }

    #[test]
    fn quickfix_listing_stays_lazy_while_fix_all_validation_materializes_auto_candidates() {
        use super::fixes::string_by_value::{materialization_count, reset_materialization_count};

        let h = Harness::new("void f(string a, string b) {}");
        reset_materialization_count();
        let lazy = h.request(
            CodeActionContext {
                only: Some(vec![CodeActionKind::QUICKFIX]),
                ..Default::default()
            },
            whole_doc(&h.source),
            true,
        );
        assert_eq!(
            materialization_count(),
            0,
            "per-diagnostic lazy listing must not compute edits"
        );

        let quickfix = of_kind(&lazy, &CodeActionKind::QUICKFIX)[0].clone();
        assert!(h.resolve(quickfix).edit.is_some());
        assert_eq!(materialization_count(), 1, "resolve must compute one edit");

        reset_materialization_count();
        let eager = h.request(
            CodeActionContext {
                only: Some(vec![CodeActionKind::QUICKFIX]),
                ..Default::default()
            },
            whole_doc(&h.source),
            false,
        );
        assert!(of_kind(&eager, &CodeActionKind::QUICKFIX)
            .iter()
            .all(|action| action.edit.is_some()));
        assert_eq!(materialization_count(), 2, "eager listing needs both edits");
    }

    #[test]
    fn lazy_fix_all_materializes_only_auto_candidates_to_validate_its_batch() {
        use super::fixes::string_by_value::{materialization_count, reset_materialization_count};

        let h = Harness::new("void f(string a, string b) {}");
        reset_materialization_count();
        let actions = h.request(
            CodeActionContext {
                only: Some(vec![SOURCE_FIX_ALL_OPENPLANET]),
                ..Default::default()
            },
            whole_doc(&h.source),
            true,
        );
        assert_eq!(materialization_count(), 2);
        assert_eq!(of_kind(&actions, &SOURCE_FIX_ALL_OPENPLANET).len(), 1);
    }

    #[test]
    fn lazy_and_eager_actions_have_the_same_identities() {
        let h = Harness::new("void f(string a, string b) {}");
        let lazy = h.all();
        let eager = h.request(CodeActionContext::default(), whole_doc(&h.source), false);
        let identity = |actions: &[CodeAction]| {
            actions
                .iter()
                .map(|action| {
                    (
                        action.title.clone(),
                        action.kind.clone(),
                        action.diagnostics.clone(),
                        action.data.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(identity(&lazy), identity(&eager));
    }

    #[test]
    fn descriptor_owned_family_has_matching_lazy_eager_actions_and_resolves_its_edit() {
        let h = Harness::new("void f(string value) {}");
        let files = [(h.uri.clone(), &h.analysis)];
        let context = CodeActionContext {
            only: Some(vec![CodeActionKind::QUICKFIX]),
            ..Default::default()
        };
        let request = |lazy_edits| CodeActionRequest {
            uri: &h.uri,
            analysis: &h.analysis,
            range: whole_doc(&h.source),
            context: &context,
            workspace: &h.workspace,
            type_index: None,
            files: WorkspaceFileResolver::new(&files),
            lazy_edits,
        };

        let lazy = code_actions_with_families(&request(true), &[DESCRIPTOR_OWNED_FAMILY]);
        let eager = code_actions_with_families(&request(false), &[DESCRIPTOR_OWNED_FAMILY]);
        let actions = |actions: Vec<CodeActionOrCommand>| {
            actions
                .into_iter()
                .map(|action| match action {
                    CodeActionOrCommand::CodeAction(action) => action,
                    CodeActionOrCommand::Command(command) => {
                        panic!("unexpected command: {command:?}")
                    }
                })
                .collect::<Vec<_>>()
        };
        let lazy = actions(lazy);
        let eager = actions(eager);
        let identity = |actions: &[CodeAction]| {
            actions
                .iter()
                .map(|action| {
                    (
                        action.title.clone(),
                        action.kind.clone(),
                        action.diagnostics.clone(),
                        action.is_preferred,
                        action.data.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(identity(&lazy), identity(&eager));
        assert_eq!(lazy.len(), 1, "one descriptor produces one quick fix");
        assert!(lazy[0].edit.is_none(), "lazy action must defer its edit");
        let expected = TextEdit::new(mk_range(0, 7, 0, 13), "const string &in".into());
        assert_eq!(edits_of(&eager[0], &h.uri), std::slice::from_ref(&expected));

        let resolved = resolve_code_action_with_families(
            lazy.into_iter().next().expect("one synthetic action"),
            &h.analysis,
            &h.workspace,
            None,
            WorkspaceFileResolver::new(&files),
            &[DESCRIPTOR_OWNED_FAMILY],
        );
        assert_eq!(edits_of(&resolved, &h.uri), &[expected]);
    }

    #[test]
    fn lazy_fix_all_is_not_offered_when_auto_descriptor_cannot_be_batched() {
        let h = Harness::new("void f(string value) {}");
        let files = [(h.uri.clone(), &h.analysis)];
        let request = CodeActionRequest {
            uri: &h.uri,
            analysis: &h.analysis,
            range: whole_doc(&h.source),
            context: &CodeActionContext {
                only: Some(vec![SOURCE_FIX_ALL_OPENPLANET]),
                ..Default::default()
            },
            workspace: &h.workspace,
            type_index: None,
            files: WorkspaceFileResolver::new(&files),
            lazy_edits: true,
        };
        let actions = code_actions_with_families(&request, &[UNBATCHABLE_AUTO_FAMILY]);
        assert!(
            actions.is_empty(),
            "unbatchable auto fix must not advertise fix-all"
        );
    }

    #[test]
    fn non_resolving_client_gets_the_edit_up_front() {
        let h = Harness::new("void f(string s) {}");
        let actions = h.request(
            CodeActionContext::default(),
            whole_doc(&h.source),
            /* lazy_edits */ false,
        );
        let quickfixes = of_kind(&actions, &CodeActionKind::QUICKFIX);
        assert_eq!(quickfixes.len(), 1);
        assert_eq!(edits_of(quickfixes[0], &h.uri).len(), 1);
    }

    #[test]
    fn resolve_on_an_action_with_no_fix_data_returns_it_unchanged() {
        let h = Harness::new("void f(string s) {}");
        let plain = CodeAction {
            title: "Someone else's action".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            ..Default::default()
        };
        let resolved = h.resolve(plain.clone());
        assert_eq!(resolved, plain);
    }

    #[test]
    fn resolve_ignores_data_from_another_provider() {
        let h = Harness::new("void f(string s) {}");
        let foreign = CodeAction {
            title: "Other".to_string(),
            data: Some(serde_json::json!({ "tag": "some.other.provider", "n": 1 })),
            ..Default::default()
        };
        assert_eq!(h.resolve(foreign.clone()), foreign);
    }

    #[test]
    fn resolve_leaves_an_already_resolved_action_alone() {
        let h = Harness::new("void f(string s) {}");
        let mut action = of_kind(&h.all(), &CodeActionKind::QUICKFIX)[0].clone();
        let sentinel = single_file_edit(
            &h.uri,
            vec![TextEdit {
                range: mk_range(0, 0, 0, 0),
                new_text: "sentinel".into(),
            }],
        );
        action.edit = Some(sentinel.clone());
        assert_eq!(h.resolve(action).edit, Some(sentinel));
    }

    #[test]
    fn resolve_of_a_stale_action_does_not_error() {
        // The document changed under the action: the referenced diagnostic
        // no longer exists, so nothing resolves — but no error either.
        let h_offer = Harness::new("void f(string s) {}");
        let stale = of_kind(&h_offer.all(), &CodeActionKind::QUICKFIX)[0].clone();
        let h_now = Harness::new("void f(const string &in s) {}");
        let resolved = h_now.resolve(stale);
        assert!(resolved.edit.is_none());
    }

    #[test]
    fn stale_legacy_actions_do_not_edit_shifted_or_replaced_content() {
        let original = "Foa value;";
        let typo_range = mk_range(0, 0, 0, 3);
        let offered = Harness::with_workspace(original, workspace_with_class("Foo"));
        let actions = offered.with_diags(vec![mk_diag(typo_range, "unknown type `Foa`")]);
        let did_you_mean = actions
            .iter()
            .find(|action| action.title == "Did you mean 'Foo'?")
            .expect("did-you-mean action")
            .clone();
        let wrap = actions
            .iter()
            .find(|action| action.kind == Some(CodeActionKind::REFACTOR_REWRITE))
            .expect("wrap action")
            .clone();

        for (case, changed) in [
            ("shifted", "int prefix; Foa value;"),
            ("same-range replacement", "Bar value;"),
        ] {
            let current = Harness::with_workspace(changed, workspace_with_class("Foo"));
            assert!(
                current.resolve(did_you_mean.clone()).edit.is_none(),
                "stale did-you-mean edited {case} content"
            );
            assert!(
                current.resolve(wrap.clone()).edit.is_none(),
                "stale refactor edited {case} content"
            );
        }
    }

    #[test]
    fn resolve_target_uri_reports_the_document_to_load() {
        let h = Harness::new("void f(string s) {}");
        let action = of_kind(&h.all(), &CodeActionKind::QUICKFIX)[0].clone();
        assert_eq!(resolve_target_uri(&action), Some(h.uri.clone()));
        assert_eq!(resolve_target_uri(&CodeAction::default()), None);
    }

    // ── fixAll ────────────────────────────────────────────────────────────

    #[test]
    fn fix_all_applies_every_nonoverlapping_fix() {
        let src = "void f(string a) {}\nvoid g(string b) {}\nvoid h(string c) {}";
        let h = Harness::new(src);
        let actions = h.all();
        let fix_all = of_kind(&actions, &SOURCE_FIX_ALL_OPENPLANET);
        assert_eq!(fix_all.len(), 1, "titles: {:?}", titles(&actions));
        let resolved = h.resolve(fix_all[0].clone());
        let edits = edits_of(&resolved, &h.uri);
        assert_eq!(edits.len(), 3);
        assert_eq!(
            apply_edits(src, edits),
            "void f(const string &in a) {}\nvoid g(const string &in b) {}\nvoid h(const string &in c) {}"
        );
    }

    #[test]
    fn fix_all_absent_when_nothing_is_fixable() {
        let h = Harness::new("void f(const string &in s) {}");
        let actions = h.all();
        assert!(
            of_kind(&actions, &SOURCE_FIX_ALL_OPENPLANET).is_empty(),
            "expected NO fixAll action, got {:?}",
            titles(&actions)
        );
    }

    #[test]
    fn fix_all_handles_two_fixes_on_one_line() {
        let src = "void f(string a, string b) {}";
        let h = Harness::new(src);
        let fix_all = h.resolve(of_kind(&h.all(), &SOURCE_FIX_ALL_OPENPLANET)[0].clone());
        let edits = edits_of(&fix_all, &h.uri);
        assert_eq!(edits.len(), 2);
        assert_eq!(
            apply_edits(src, edits),
            "void f(const string &in a, const string &in b) {}"
        );
    }

    #[test]
    fn fix_all_edits_are_ordered_descending() {
        let src = "void f(string a, string b, string c) {}";
        let h = Harness::new(src);
        let fix_all = h.resolve(of_kind(&h.all(), &SOURCE_FIX_ALL_OPENPLANET)[0].clone());
        let starts: Vec<Position> = edits_of(&fix_all, &h.uri)
            .iter()
            .map(|e| e.range.start)
            .collect();
        let mut descending = starts.clone();
        descending.sort_by(|a, b| b.cmp(a));
        assert_eq!(starts, descending);
    }

    #[test]
    fn fix_all_is_not_preferred() {
        // A source action must not hijack the "auto fix" keybinding.
        let h = Harness::new("void f(string s) {}");
        let fix_all = of_kind(&h.all(), &SOURCE_FIX_ALL_OPENPLANET)[0].clone();
        assert_eq!(fix_all.is_preferred, None);
    }

    // ── `only` filtering ──────────────────────────────────────────────────

    #[test]
    fn only_quickfix_excludes_the_refactor_and_the_source_action() {
        let h = Harness::new("void f(string s) {}");
        let actions = h.request(
            CodeActionContext {
                only: Some(vec![CodeActionKind::QUICKFIX]),
                ..Default::default()
            },
            whole_doc(&h.source),
            true,
        );
        assert!(
            actions
                .iter()
                .all(|a| a.kind == Some(CodeActionKind::QUICKFIX)),
            "got {:?}",
            actions.iter().map(|a| a.kind.clone()).collect::<Vec<_>>()
        );
        assert!(!actions.is_empty());
    }

    #[test]
    fn only_source_fix_all_excludes_per_diagnostic_quickfixes() {
        let h = Harness::new("void f(string s) {}");
        let actions = h.request(
            CodeActionContext {
                only: Some(vec![CodeActionKind::SOURCE_FIX_ALL]),
                ..Default::default()
            },
            whole_doc(&h.source),
            true,
        );
        assert_eq!(actions.len(), 1, "got {:?}", titles(&actions));
        assert_eq!(actions[0].kind, Some(SOURCE_FIX_ALL_OPENPLANET));
    }

    #[test]
    fn only_namespaced_fix_all_selects_our_source_action() {
        let h = Harness::new("void f(string s) {}");
        let actions = h.request(
            CodeActionContext {
                only: Some(vec![SOURCE_FIX_ALL_OPENPLANET]),
                ..Default::default()
            },
            whole_doc(&h.source),
            true,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, Some(SOURCE_FIX_ALL_OPENPLANET));
    }

    #[test]
    fn only_refactor_returns_just_the_refactor() {
        let h = Harness::new("void f(string s) {}");
        let actions = h.request(
            CodeActionContext {
                only: Some(vec![CodeActionKind::REFACTOR]),
                ..Default::default()
            },
            whole_doc(&h.source),
            true,
        );
        assert_eq!(titles(&actions), vec!["Wrap in try/catch".to_string()]);
    }

    // ── Range scoping ─────────────────────────────────────────────────────

    #[test]
    fn quickfix_is_scoped_to_the_requested_range_but_fixall_is_not() {
        let src = "void f(string a) {}\nvoid g(string b) {}";
        let h = Harness::new(src);
        // Cursor on line 0 only.
        let actions = h.request(CodeActionContext::default(), mk_range(0, 14, 0, 14), true);
        let quickfixes = of_kind(&actions, &CodeActionKind::QUICKFIX);
        assert_eq!(quickfixes.len(), 1, "titles: {:?}", titles(&actions));
        assert!(quickfixes[0].title.contains('a'));
        // The source action still covers the whole file.
        let fix_all = of_kind(&actions, &SOURCE_FIX_ALL_OPENPLANET);
        assert_eq!(edits_of(&h.resolve(fix_all[0].clone()), &h.uri).len(), 2);
    }

    // ── Diagnostic plumbing ───────────────────────────────────────────────

    #[test]
    fn seam_action_carries_the_diagnostic_it_fixes() {
        let h = Harness::new("void f(string s) {}");
        let action = of_kind(&h.all(), &CodeActionKind::QUICKFIX)[0].clone();
        let diags = action.diagnostics.expect("diagnostics attached");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(diags[0].source.as_deref(), Some(DIAGNOSTIC_SOURCE));
        assert!(diags[0].message.contains("const string &in"));
    }

    #[test]
    fn seam_action_prefers_the_clients_copy_of_the_diagnostic() {
        let h = Harness::new("void f(string s) {}");
        let ours = of_kind(&h.all(), &CodeActionKind::QUICKFIX)[0]
            .diagnostics
            .clone()
            .unwrap()[0]
            .clone();
        let mut echoed = ours.clone();
        echoed.code = Some(NumberOrString::String("client-side".into()));
        let action = of_kind(
            &h.with_diags(vec![echoed.clone()]),
            &CodeActionKind::QUICKFIX,
        )[0]
        .diagnostics
        .clone()
        .unwrap()[0]
            .clone();
        assert_eq!(action.code, echoed.code);
    }

    #[test]
    fn a_diagnostic_from_another_source_never_drives_a_quick_fix() {
        let table = workspace_with_class("Foo");
        let h = Harness::with_workspace("", table);
        let mut foreign = mk_diag(mk_range(1, 4, 1, 7), "unknown type `Foa`");
        foreign.source = Some("some-other-server".to_string());
        let actions = h.with_diags(vec![foreign]);
        assert!(
            !actions.iter().any(|a| a.title.contains("Did you mean")),
            "got {:?}",
            titles(&actions)
        );
        // Sanity: the same diagnostic from us DOES produce the fix.
        let ours = mk_diag(mk_range(1, 4, 1, 7), "unknown type `Foa`");
        assert!(h
            .with_diags(vec![ours])
            .iter()
            .any(|a| a.title.contains("Did you mean")));
    }

    // ── Regression guards for the pre-existing actions ────────────────────

    #[test]
    fn extract_name_works() {
        assert_eq!(
            extract_name_from_diagnostic("unknown type `Foo`"),
            Some("Foo")
        );
        assert_eq!(
            extract_name_from_diagnostic("undefined identifier `bar`"),
            Some("bar")
        );
        assert_eq!(extract_name_from_diagnostic("no backtick here"), None);
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("foo", "foo"), 0);
        assert_eq!(levenshtein("foo", "fop"), 1);
        assert_eq!(levenshtein("foo", "bar"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn is_did_you_mean_candidate_detects_messages() {
        assert!(is_did_you_mean_candidate("unknown type `Foo`"));
        assert!(is_did_you_mean_candidate("undefined identifier `bar`"));
        assert!(!is_did_you_mean_candidate("expected ; after expression"));
    }

    #[test]
    fn quickfix_did_you_mean_finds_typo() {
        let table = workspace_with_class("Foo");
        let h = Harness::with_workspace("", table);
        let diag_range = mk_range(1, 4, 1, 7);
        let actions = h.with_diags(vec![mk_diag(diag_range, "unknown type `Foa`")]);

        let foo_action = actions
            .iter()
            .find(|a| a.title.contains("Foo"))
            .unwrap_or_else(|| {
                panic!(
                    "expected a 'Did you mean Foo?' action in {:?}",
                    titles(&actions)
                )
            });
        assert_eq!(foo_action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(foo_action.is_preferred, Some(true));
        let resolved = h.resolve(foo_action.clone());
        let edits = edits_of(&resolved, &h.uri);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "Foo");
        assert_eq!(edits[0].range, diag_range);
    }

    #[test]
    fn quickfix_did_you_mean_ignores_unrelated_diagnostics() {
        let table = workspace_with_class("Foo");
        let uri = Url::parse("file:///tmp/a.as").unwrap();
        let diag = mk_diag(mk_range(0, 0, 0, 3), "some other error");
        assert!(
            quick_fix_did_you_mean(&uri, source_fingerprint(""), &[diag], &table, None, true)
                .is_empty()
        );
    }

    #[test]
    fn wrap_in_try_catch_always_offered() {
        let source = "print(x);";
        let h = Harness::new(source);
        let actions = h.request(CodeActionContext::default(), mk_range(0, 0, 0, 9), true);
        let refactors = of_kind(&actions, &CodeActionKind::REFACTOR_REWRITE);
        assert_eq!(refactors.len(), 1);
        assert_eq!(refactors[0].title, "Wrap in try/catch");
        let resolved = h.resolve(refactors[0].clone());
        let edits = edits_of(&resolved, &h.uri);
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.contains("try {"));
        assert!(edits[0].new_text.contains("print(x);"));
        assert!(edits[0].new_text.contains("} catch { }"));
    }

    #[test]
    fn common_prefix_matches_long_prefix() {
        assert!(is_similar("GetFoo", "GetFooBar").is_some());
    }
}
