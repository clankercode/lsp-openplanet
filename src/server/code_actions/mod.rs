//! `textDocument/codeAction` and `codeAction/resolve` handlers.
//!
//! Action phases, in the order the client sees them:
//!
//! 1. Published-diagnostic families, in registry order (did-you-mean, then
//!    unused declarations), independently of client diagnostic ordering.
//! 2. **"Wrap in try/catch" refactor** — the one action no diagnostic
//!    drives: it is offered for whatever range the client asked about.
//! 3. Checker-keyed families, then the batched
//!    `source.fixAll.openplanet` source action ([`fix_all`]).
//!
//! Both family groups use the same fix seam ([`fix`]); this phase split only
//! preserves the response order published before B047 unified production.
//!
//! Seam actions are returned **without** their edit when the client
//! advertises `codeAction/resolve` support; the edit is materialised in
//! [`resolve_code_action`] for the single action the user picked. Producers
//! are pure, so re-entering them on resolve is cheap and cannot diverge —
//! with one wrinkle a diagnostic-keyed producer has to be given for free:
//! `codeAction/resolve` carries no `CodeActionContext`, so the diagnostic
//! such a producer reads travels inside the action's own `data` as a compact
//! range/message/code record, never as the arbitrary full client diagnostic.

pub mod fix;
pub mod fix_all;
pub mod fixes;
#[cfg(test)]
pub mod test_fixtures;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::server::diagnostics::position_to_offset;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

pub use fix::SOURCE_FIX_ALL_OPENPLANET;
use fix::{
    collect_fix_descriptors_from, kind_selected, materialize_auto_descriptors,
    materialize_descriptor, materialize_fix_from, single_file_edit, FixContext,
    FixDescriptorCandidate, Registries, WorkspaceFileResolver,
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

/// Only the diagnostic fields the registered published-diagnostic families
/// need to match and materialize an action. The full client diagnostic stays
/// on `CodeAction.diagnostics`; arbitrary metadata is not echoed into data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveDiagnostic {
    range: Range,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<NumberOrString>,
}

impl From<&Diagnostic> for ResolveDiagnostic {
    fn from(diagnostic: &Diagnostic) -> Self {
        Self {
            range: diagnostic.range,
            message: diagnostic.message.clone(),
            code: diagnostic.code.clone(),
        }
    }
}

impl From<ResolveDiagnostic> for Diagnostic {
    fn from(diagnostic: ResolveDiagnostic) -> Self {
        Diagnostic {
            range: diagnostic.range,
            message: diagnostic.message,
            code: diagnostic.code,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ResolveAction {
    Fix {
        family: String,
        diagnostic_range: Range,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        materialization_id: Option<String>,
        /// Compact identity of the published diagnostic this action answers.
        /// Checker-keyed families omit it and re-derive their finding.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        published: Option<ResolveDiagnostic>,
    },
    FixAll {
        /// As above, for every diagnostic that fed an auto-applicable
        /// descriptor — otherwise the resolved batch would quietly be a
        /// subset of the one the offer was validated against.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        published: Vec<ResolveDiagnostic>,
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
    code_actions_with(req, fix::REGISTRIES)
}

/// As [`code_actions`], against explicit registries — the seam's own tests
/// drive synthetic families through the real handler this way.
pub(crate) fn code_actions_with(
    req: &CodeActionRequest<'_>,
    registries: Registries<'_>,
) -> Vec<CodeActionOrCommand> {
    let only = req.context.only.as_deref();
    let mut actions: Vec<CodeAction> = Vec::new();
    // Every action from one response represents this same document revision.
    // Carry the digest into its data instead of re-hashing per action.
    let source_fingerprint = source_fingerprint(&req.analysis.source);

    let wants_quickfix = kind_selected(&CodeActionKind::QUICKFIX, only);
    let wants_fix_all = kind_selected(&SOURCE_FIX_ALL_OPENPLANET, only);
    let fix_state = if wants_quickfix || wants_fix_all {
        let ctx = FixContext {
            uri: req.uri,
            analysis: req.analysis,
            workspace: req.workspace,
            type_index: req.type_index,
            files: req.files,
        };
        let descriptors = collect_fix_descriptors_from(&ctx, &req.context.diagnostics, registries);
        if wants_quickfix {
            for candidate in descriptors
                .iter()
                .filter(|candidate| candidate.is_published())
            {
                if !ranges_touch(&candidate.diagnostic.range, &req.range) {
                    continue;
                }
                // A resolving client gets the menu entry alone; the edit is
                // materialized for the one action it picks.
                let edit = (!req.lazy_edits)
                    .then(|| materialize_descriptor(&ctx, candidate, registries).fix.edit);
                actions.push(candidate_action(req, candidate, edit, source_fingerprint));
            }
        }
        Some((ctx, descriptors))
    } else {
        None
    };
    if kind_selected(&CodeActionKind::REFACTOR_REWRITE, only) {
        actions.extend(refactor_wrap_in_try_catch(
            req.uri,
            &req.analysis.source,
            source_fingerprint,
            req.range,
            req.lazy_edits,
        ));
    }
    if let Some((ctx, descriptors)) = fix_state {
        if wants_quickfix {
            for candidate in descriptors
                .iter()
                .filter(|candidate| !candidate.is_published())
            {
                if !ranges_touch(&candidate.diagnostic.range, &req.range) {
                    continue;
                }
                let edit = (!req.lazy_edits)
                    .then(|| materialize_descriptor(&ctx, candidate, registries).fix.edit);
                actions.push(candidate_action(req, candidate, edit, source_fingerprint));
            }
        }
        if wants_fix_all {
            // Only the auto-applicable half is worth materializing: the
            // batch discards everything else anyway.
            let candidates = materialize_auto_descriptors(&ctx, &descriptors, registries);
            if let Some(mut action) = fix_all::fix_all_action(req.uri, &candidates) {
                action.data = resolve_data(
                    req.uri,
                    source_fingerprint,
                    ResolveAction::FixAll {
                        published: auto_published_diagnostics(&descriptors),
                    },
                );
                if req.lazy_edits {
                    action.edit = None;
                }
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
    resolve_code_action_with(
        action,
        analysis,
        workspace,
        type_index,
        files,
        fix::REGISTRIES,
    )
}

pub(crate) fn resolve_code_action_with(
    mut action: CodeAction,
    analysis: &DocumentAnalysis,
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
    files: WorkspaceFileResolver<'_>,
    registries: Registries<'_>,
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
    let ctx = FixContext {
        uri: &resolve.uri,
        analysis,
        workspace,
        type_index,
        files,
    };
    match resolve.action {
        ResolveAction::Fix {
            family,
            diagnostic_range,
            title,
            materialization_id,
            published,
        } => {
            let echoed: Vec<Diagnostic> = published.map(Diagnostic::from).into_iter().collect();
            if let Some(edit) = materialize_fix_from(
                &ctx,
                &echoed,
                &family,
                diagnostic_range,
                &title,
                materialization_id.as_deref(),
                registries,
            ) {
                action.edit = Some(edit);
            }
        }
        ResolveAction::FixAll { published } => {
            let echoed: Vec<Diagnostic> = published.into_iter().map(Diagnostic::from).collect();
            let descriptors = collect_fix_descriptors_from(&ctx, &echoed, registries);
            let candidates = materialize_auto_descriptors(&ctx, &descriptors, registries);
            if let Some(resolved) = fix_all::fix_all_action(&resolve.uri, &candidates) {
                action.edit = resolved.edit;
            }
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

/// One offered action, lazy or eager. Both shapes are built here so a
/// resolving and a non-resolving client cannot drift apart in title, kind,
/// preferred flag, attached diagnostic or resolve identity — the only
/// difference between them is whether `edit` is filled in yet.
fn candidate_action(
    req: &CodeActionRequest<'_>,
    candidate: &FixDescriptorCandidate,
    edit: Option<WorkspaceEdit>,
    source_fingerprint: SourceFingerprint,
) -> CodeAction {
    let diagnostic = if candidate.is_published() {
        candidate.diagnostic.clone()
    } else {
        client_diagnostic(req, &candidate.diagnostic)
    };
    CodeAction {
        title: candidate.descriptor.title.clone(),
        kind: Some(candidate.descriptor.kind.clone()),
        diagnostics: Some(vec![diagnostic]),
        edit,
        command: None,
        // LSP treats an absent `isPreferred` and an explicit `false` the
        // same, and `lsp_types` skips serializing `None`, so the seam sends
        // the field only for the action it actually leads with.
        is_preferred: candidate.descriptor.is_preferred.then_some(true),
        disabled: None,
        data: resolve_data(
            req.uri,
            source_fingerprint,
            ResolveAction::Fix {
                family: candidate.family.to_string(),
                diagnostic_range: candidate.diagnostic.range,
                title: candidate.descriptor.title.clone(),
                materialization_id: candidate.materialization_id().map(str::to_string),
                published: candidate.published_subject().map(ResolveDiagnostic::from),
            },
        ),
    }
}

/// The published diagnostics that fed an auto-applicable descriptor, so the
/// fix-all batch can be rebuilt at resolve time. Without them a
/// diagnostic-keyed family would contribute to the batch the client was
/// offered and then silently drop out of the batch it is handed.
///
/// Only compact range/message/code values are retained; the full client
/// diagnostics remain attached to the individual quick-fix actions.
fn auto_published_diagnostics(descriptors: &[FixDescriptorCandidate]) -> Vec<ResolveDiagnostic> {
    let mut out: Vec<ResolveDiagnostic> = Vec::new();
    for candidate in descriptors
        .iter()
        .filter(|candidate| candidate.descriptor.auto_applicable)
    {
        if let Some(diagnostic) = candidate.published_subject() {
            let diagnostic = ResolveDiagnostic::from(diagnostic);
            if !out.contains(&diagnostic) {
                out.push(diagnostic);
            }
        }
    }
    out
}

/// Prefer the client's own copy of the diagnostic (it may carry fields we
/// never sent, and matching by identity is what ties the action to the
/// squiggle); fall back to the form we published.
fn client_diagnostic(req: &CodeActionRequest<'_>, published: &Diagnostic) -> Diagnostic {
    req.context
        .diagnostics
        .iter()
        .find(|d| d.range == published.range && d.message == published.message)
        .cloned()
        .unwrap_or_else(|| published.clone())
}

/// Inclusive intersection: a zero-width cursor sitting at either end of a
/// diagnostic still counts as "on" it, which is what the lightbulb needs.
fn ranges_touch(a: &Range, b: &Range) -> bool {
    a.start <= b.end && b.start <= a.end
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

#[cfg(test)]
mod tests {
    use super::test_fixtures::{apply_edits, ctx_for, published_unused_diagnostics};
    use super::*;
    use crate::lexer::Span;
    use crate::server::diagnostics::DIAGNOSTIC_SOURCE;
    use crate::symbols::scope::{Symbol, SymbolKind};
    use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};
    use std::collections::HashMap;

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
        workspace_with_classes(&[name])
    }

    fn workspace_with_classes(names: &[&str]) -> SymbolTable {
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        let symbols = names
            .iter()
            .map(|name| Symbol {
                name: (*name).to_string(),
                kind: SymbolKind::Class {
                    parents: Vec::new(),
                    members: Vec::new(),
                },
                span: Span { start: 0, end: 0 },
                file_id: fid,
                doc: None,
            })
            .collect();
        table.set_file_symbols(fid, symbols);
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
            self.request_with(ctx, range, lazy, fix::REGISTRIES)
        }

        fn request_with(
            &self,
            ctx: CodeActionContext,
            range: Range,
            lazy: bool,
            registries: Registries<'_>,
        ) -> Vec<CodeAction> {
            let files = [(self.uri.clone(), &self.analysis)];
            code_actions_with(
                &CodeActionRequest {
                    uri: &self.uri,
                    analysis: &self.analysis,
                    range,
                    context: &ctx,
                    workspace: &self.workspace,
                    type_index: None,
                    files: WorkspaceFileResolver::new(&files),
                    lazy_edits: lazy,
                },
                registries,
            )
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

        /// A request scoped to one range, the way a client asks about the
        /// cursor: only the diagnostics overlapping that range are echoed.
        fn at(&self, range: Range, diagnostics: Vec<Diagnostic>) -> Vec<CodeAction> {
            self.request(
                CodeActionContext {
                    diagnostics,
                    ..Default::default()
                },
                range,
                true,
            )
        }

        /// Offer at `range`, resolve the one action whose title contains
        /// `needle`, apply its edit, return the resulting document. This is
        /// the full client round trip: nothing about the fix is asserted
        /// from the inside.
        fn round_trip(&self, range: Range, diagnostics: Vec<Diagnostic>, needle: &str) -> String {
            let offered = self.at(range, diagnostics);
            let action = offered
                .iter()
                .find(|a| a.title.contains(needle))
                .unwrap_or_else(|| {
                    panic!("no action containing `{needle}` in {:?}", titles(&offered))
                })
                .clone();
            let resolved = self.resolve(action);
            apply_edits(&self.source, edits_of(&resolved, &self.uri))
        }

        fn resolve(&self, action: CodeAction) -> CodeAction {
            self.resolve_with(action, fix::REGISTRIES)
        }

        fn resolve_with(&self, action: CodeAction, registries: Registries<'_>) -> CodeAction {
            let files = [(self.uri.clone(), &self.analysis)];
            resolve_code_action_with(
                action,
                &self.analysis,
                &self.workspace,
                None,
                WorkspaceFileResolver::new(&files),
                registries,
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

        let lazy = code_actions_with(
            &request(true),
            Registries::checker_only(&[DESCRIPTOR_OWNED_FAMILY]),
        );
        let eager = code_actions_with(
            &request(false),
            Registries::checker_only(&[DESCRIPTOR_OWNED_FAMILY]),
        );
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

        let resolved = resolve_code_action_with(
            lazy.into_iter().next().expect("one synthetic action"),
            &h.analysis,
            &h.workspace,
            None,
            WorkspaceFileResolver::new(&files),
            Registries::checker_only(&[DESCRIPTOR_OWNED_FAMILY]),
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
        let actions = code_actions_with(
            &request,
            Registries::checker_only(&[UNBATCHABLE_AUTO_FAMILY]),
        );
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
        // The squiggle sits on line 1, cols 4..7, so the whole-document
        // request the harness makes really does cover it.
        let h = Harness::with_workspace("\n    Foa value;", table);
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
    fn quickfix_did_you_mean_finds_typo() {
        let table = workspace_with_class("Foo");
        let h = Harness::with_workspace("\n    Foa value;", table);
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
    /// Exactly one spelling leads the menu — the closest — and the
    /// alternatives do not.
    ///
    /// Asserted as "preferred / not preferred", not as `Some(true)` /
    /// `Some(false)`: LSP's `isPreferred?: boolean` gives an absent field
    /// and an explicit `false` the same meaning, so the seam sends the
    /// field only for the action it leads with and every other action
    /// omits it.
    fn exactly_one_did_you_mean_suggestion_leads_the_menu() {
        let table = workspace_with_classes(&["Foo", "Food"]);
        let h = Harness::with_workspace("Foa value;", table);
        let range = mk_range(0, 0, 0, 3);
        let suggestions: Vec<_> = h
            .at(range, vec![mk_diag(range, "unknown type `Foa`")])
            .into_iter()
            .filter(|action| action.title.starts_with("Did you mean"))
            .collect();
        assert!(suggestions.len() >= 2, "titles: {:?}", titles(&suggestions));
        assert_eq!(suggestions[0].title, "Did you mean 'Foo'?");
        assert_eq!(suggestions[0].is_preferred, Some(true));
        for alternative in &suggestions[1..] {
            assert_ne!(
                alternative.is_preferred,
                Some(true),
                "`{}` must not also lead the menu",
                alternative.title
            );
        }
        assert_eq!(
            suggestions
                .iter()
                .filter(|action| action.is_preferred == Some(true))
                .count(),
            1
        );
    }

    #[test]
    fn quickfix_did_you_mean_ignores_unrelated_diagnostics() {
        let h = Harness::with_workspace("Foa value;", workspace_with_class("Foo"));
        let range = mk_range(0, 0, 0, 3);
        let actions = h.at(range, vec![mk_diag(range, "some other error")]);
        assert!(
            of_kind(&actions, &CodeActionKind::QUICKFIX).is_empty(),
            "got {:?}",
            titles(&actions)
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

    // ── B047: one seam for both producer paths ────────────────────────────

    // A synthetic diagnostic-keyed family, so these tests exercise the SEAM
    // rather than whichever families happen to be registered.

    const SYNTH_CODE: &str = "synthetic-published";
    const SYNTH_ALT_CODE: &str = "synthetic-published-alt";
    const SYNTH_AUTO_CODE: &str = "synthetic-published-auto";

    fn synth_diag(range: Range, code: &str) -> Diagnostic {
        Diagnostic {
            code: Some(NumberOrString::String(code.to_string())),
            ..mk_diag(range, "a synthetic finding")
        }
    }

    fn synth_matches(diagnostic: &Diagnostic) -> bool {
        matches!(
            &diagnostic.code,
            Some(NumberOrString::String(c)) if c == SYNTH_CODE || c == SYNTH_ALT_CODE
        )
    }

    fn synth_auto_matches(diagnostic: &Diagnostic) -> bool {
        matches!(&diagnostic.code, Some(NumberOrString::String(c)) if c == SYNTH_AUTO_CODE)
    }

    fn synth_describe(_: &Diagnostic, _: &FixContext<'_>) -> Vec<fix::DiagnosticFixDescriptor> {
        vec![fix::DiagnosticFixDescriptor {
            descriptor: fix::FixDescriptor {
                title: "Synthetic published fix".to_string(),
                kind: CodeActionKind::QUICKFIX,
                is_preferred: false,
                auto_applicable: false,
            },
            materialization_id: None,
        }]
    }

    fn synth_auto_describe(
        _: &Diagnostic,
        _: &FixContext<'_>,
    ) -> Vec<fix::DiagnosticFixDescriptor> {
        vec![fix::DiagnosticFixDescriptor {
            descriptor: fix::FixDescriptor {
                title: "Synthetic batchable fix".to_string(),
                kind: CodeActionKind::QUICKFIX,
                is_preferred: false,
                auto_applicable: true,
            },
            materialization_id: None,
        }]
    }

    fn synth_materialize(diagnostic: &Diagnostic, ctx: &FixContext<'_>, _: &str) -> WorkspaceEdit {
        single_file_edit(
            ctx.uri,
            vec![TextEdit {
                range: diagnostic.range,
                new_text: "SYNTH".to_string(),
            }],
        )
    }

    const SYNTH_FAMILY: fix::DiagnosticFixFamily = fix::DiagnosticFixFamily {
        name: "test-synthetic-published",
        matches: synth_matches,
        describe: synth_describe,
        materialize: synth_materialize,
    };

    const SYNTH_AUTO_FAMILY: fix::DiagnosticFixFamily = fix::DiagnosticFixFamily {
        name: "test-synthetic-published-auto",
        matches: synth_auto_matches,
        describe: synth_auto_describe,
        materialize: synth_materialize,
    };

    #[test]
    fn published_actions_attach_each_exact_originating_duplicate_diagnostic() {
        let registries = Registries {
            checker: &[],
            published: &[SYNTH_FAMILY],
        };
        let h = Harness::new("void Main() { }");
        let range = mk_range(0, 0, 0, 4);
        let mut first = synth_diag(range, SYNTH_CODE);
        first.tags = Some(vec![DiagnosticTag::UNNECESSARY]);
        first.data = Some(serde_json::json!({ "origin": "first" }));
        let mut second = synth_diag(range, SYNTH_ALT_CODE);
        second.tags = Some(vec![DiagnosticTag::DEPRECATED]);
        second.data = Some(serde_json::json!({ "origin": "second" }));

        // Same range, source and message: only code/tags/data distinguish
        // these two client diagnostics.
        let actions = h.request_with(
            CodeActionContext {
                diagnostics: vec![first.clone(), second.clone()],
                ..Default::default()
            },
            range,
            true,
            registries,
        );
        let attached: Vec<Diagnostic> = actions
            .iter()
            .filter(|action| action.kind == Some(CodeActionKind::QUICKFIX))
            .map(|action| {
                action
                    .diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.first())
                    .expect("one attached diagnostic")
                    .clone()
            })
            .collect();
        assert_eq!(attached, vec![first, second]);
    }

    /// A producer registered ONLY in the diagnostic-keyed registry is
    /// offered for the diagnostic it claims — and only for that one.
    #[test]
    fn a_diagnostic_keyed_producer_is_offered_for_exactly_what_it_claims() {
        let registries = Registries {
            checker: &[],
            published: &[SYNTH_FAMILY],
        };
        let h = Harness::new("void Main() { }");
        let range = mk_range(0, 0, 0, 4);

        let claimed = h.request_with(
            CodeActionContext {
                diagnostics: vec![synth_diag(range, SYNTH_CODE)],
                ..Default::default()
            },
            range,
            true,
            registries,
        );
        assert_eq!(
            titles(
                &of_kind(&claimed, &CodeActionKind::QUICKFIX)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            ),
            vec!["Synthetic published fix"]
        );

        for unclaimed in [
            // Ours, but a code this family does not claim.
            synth_diag(range, "some-other-code"),
            // A diagnostic with no code at all.
            mk_diag(range, "a synthetic finding"),
        ] {
            let actions = h.request_with(
                CodeActionContext {
                    diagnostics: vec![unclaimed.clone()],
                    ..Default::default()
                },
                range,
                true,
                registries,
            );
            assert!(
                of_kind(&actions, &CodeActionKind::QUICKFIX).is_empty(),
                "claimed {:?}",
                unclaimed.code
            );
        }
    }

    /// A diagnostic-keyed action defers its edit and resolves it, exactly
    /// like a checker-keyed one — including the part a checker family gets
    /// for free: `codeAction/resolve` carries no `CodeActionContext`, so the
    /// diagnostic the producer reads has to survive the round trip.
    #[test]
    fn a_diagnostic_keyed_action_resolves_lazily() {
        let registries = Registries {
            checker: &[],
            published: &[SYNTH_FAMILY],
        };
        let h = Harness::new("void Main() { }");
        let range = mk_range(0, 5, 0, 9);
        let offered = h.request_with(
            CodeActionContext {
                diagnostics: vec![synth_diag(range, SYNTH_CODE)],
                ..Default::default()
            },
            range,
            true,
            registries,
        );
        let action = offered
            .iter()
            .find(|a| a.title == "Synthetic published fix")
            .expect("the synthetic action")
            .clone();
        assert!(action.edit.is_none(), "a lazy offer must carry no edit");

        let resolved = h.resolve_with(action, registries);
        assert_eq!(
            apply_edits(&h.source, edits_of(&resolved, &h.uri)),
            "void SYNTH() { }"
        );
    }

    /// Both registries active, both claiming a finding at the same place:
    /// both actions are offered, diagnostic-keyed first, and the order does
    /// not wander between identical requests.
    #[test]
    fn both_registries_offer_their_fix_in_a_deterministic_order() {
        let registries = Registries {
            checker: fixes::FAMILIES,
            published: &[SYNTH_FAMILY],
        };
        let source = "void f(string value) {}";
        let h = Harness::new(source);
        let range = mk_range(0, 7, 0, 19);
        let context = CodeActionContext {
            diagnostics: vec![synth_diag(range, SYNTH_CODE)],
            ..Default::default()
        };

        let first = h.request_with(context.clone(), range, true, registries);
        let quick: Vec<String> = of_kind(&first, &CodeActionKind::QUICKFIX)
            .into_iter()
            .map(|a| a.title.clone())
            .collect();
        assert_eq!(
            quick,
            vec![
                "Synthetic published fix",
                "Pass `value` by reference (`const string &in`)",
            ],
            "diagnostic-keyed producers are offered before checker-keyed ones"
        );
        let again = h.request_with(context, range, true, registries);
        assert_eq!(titles(&first), titles(&again));
    }

    /// Preserve the published response contract, independently of the order
    /// diagnostics happen to arrive from the client: all did-you-mean
    /// actions, all unused actions, the refactor, then checker seam actions
    /// and its fix-all batch.
    #[test]
    fn real_producers_keep_their_published_order_across_mixed_diagnostics() {
        let source = "void f(string s, int p) { Foa x; Print(s); }\nvoid Main() { f(\"x\", 1); }";
        let mut echoed = published_unused_diagnostics(source);
        let unused_p = echoed
            .drain(..)
            .find(|diagnostic| diagnostic.message.contains("`p`"))
            .expect("unused parameter p");
        let typo_start = source.find("Foa").expect("typo") as u32;
        let typo = mk_diag(
            mk_range(0, typo_start, 0, typo_start + 3),
            "unknown type `Foa`",
        );
        let h = Harness::with_workspace(source, workspace_with_class("Foo"));

        // Deliberately echo unused first; producer ordering must win over
        // diagnostic ordering.
        let actions = h.with_diags(vec![unused_p, typo]);
        assert_eq!(
            titles(&actions),
            vec![
                "Did you mean 'Foo'?",
                "Prefix `p` with `_` to mark it unused",
                "Wrap in try/catch",
                "Pass `s` by reference (`const string &in`)",
                "Fix all auto-fixable Openplanet problems",
            ]
        );
    }

    /// `source.fixAll.openplanet` batches exactly the auto-applicable
    /// descriptors, wherever they came from — and the batch the client is
    /// handed on resolve is the batch it was offered, which only holds
    /// because the diagnostics a published producer needs travel inside the
    /// action's `data`.
    #[test]
    fn fix_all_batches_auto_applicable_fixes_from_both_registries() {
        let registries = Registries {
            checker: fixes::FAMILIES,
            published: &[SYNTH_FAMILY, SYNTH_AUTO_FAMILY],
        };
        let source = "void f(string value) {}
void Main() { }";
        let h = Harness::new(source);
        let synth_range = mk_range(1, 5, 1, 9);
        let context = CodeActionContext {
            diagnostics: vec![
                // Auto-applicable, and away from the checker fix.
                synth_diag(synth_range, SYNTH_AUTO_CODE),
                // Not auto-applicable: must stay out of the batch.
                synth_diag(mk_range(0, 7, 0, 19), SYNTH_CODE),
            ],
            only: Some(vec![SOURCE_FIX_ALL_OPENPLANET]),
            ..Default::default()
        };
        let batched = "void f(const string &in value) {}
void SYNTH() { }";

        // Eager client: the edit arrives with the offer.
        let eager = h.request_with(context.clone(), whole_doc(source), false, registries);
        assert_eq!(eager.len(), 1, "{:?}", titles(&eager));
        assert_eq!(apply_edits(source, edits_of(&eager[0], &h.uri)), batched);

        // Resolving client: the same batch has to survive the round trip.
        let lazy = h.request_with(context, whole_doc(source), true, registries);
        assert_eq!(lazy.len(), 1, "{:?}", titles(&lazy));
        assert!(lazy[0].edit.is_none());
        let resolved = h.resolve_with(lazy[0].clone(), registries);
        assert_eq!(
            apply_edits(source, edits_of(&resolved, &h.uri)),
            batched,
            "the resolved batch must not silently drop the published-registry fix"
        );
    }

    /// The `only` filter is applied to every action the seam produces,
    /// whichever registry it came from.
    #[test]
    fn the_only_filter_is_honoured_for_both_registries() {
        let registries = Registries {
            checker: fixes::FAMILIES,
            published: &[SYNTH_FAMILY, SYNTH_AUTO_FAMILY],
        };
        let source = "void f(string value) {}";
        let h = Harness::new(source);
        let range = mk_range(0, 7, 0, 19);
        let diagnostics = vec![
            synth_diag(range, SYNTH_CODE),
            synth_diag(mk_range(0, 0, 0, 4), SYNTH_AUTO_CODE),
        ];
        let ask = |only: Option<Vec<CodeActionKind>>| {
            titles(&h.request_with(
                CodeActionContext {
                    diagnostics: diagnostics.clone(),
                    only,
                    ..Default::default()
                },
                whole_doc(source),
                true,
                registries,
            ))
        };

        let quickfix_only = ask(Some(vec![CodeActionKind::QUICKFIX]));
        assert!(
            quickfix_only.contains(&"Synthetic published fix".to_string())
                && quickfix_only.contains(&"Synthetic batchable fix".to_string())
                && quickfix_only
                    .contains(&"Pass `value` by reference (`const string &in`)".to_string()),
            "got {quickfix_only:?}"
        );
        assert!(
            !quickfix_only
                .iter()
                .any(|t| t.starts_with("Fix all") || t == "Wrap in try/catch"),
            "got {quickfix_only:?}"
        );

        assert_eq!(
            ask(Some(vec![SOURCE_FIX_ALL_OPENPLANET])),
            vec!["Fix all auto-fixable Openplanet problems"]
        );
        assert_eq!(
            ask(Some(vec![CodeActionKind::REFACTOR_REWRITE])),
            vec!["Wrap in try/catch"]
        );
        assert!(ask(Some(Vec::new())).is_empty());
    }

    /// Every quick fix, whichever registry produced it, is offered WITHOUT
    /// its edit to a resolving client. Before B047 the unused-declaration
    /// producer attached its edit eagerly, so the laziness discipline held
    /// for some quick fixes and not others.
    #[test]
    fn a_resolving_client_gets_every_quick_fix_without_its_edit() {
        let source = "void Main() { int x = 1; }";
        let h = Harness::new(source);
        let diagnostics = published_unused_diagnostics(source);
        assert!(!diagnostics.is_empty(), "test premise: `x` is unused");
        let offered = h.at(diagnostics[0].range, diagnostics);
        let quick_fixes = of_kind(&offered, &CodeActionKind::QUICKFIX);
        assert!(!quick_fixes.is_empty(), "test premise: fixes are offered");
        for action in quick_fixes {
            assert!(
                action.edit.is_none(),
                "`{}` carried its edit to a resolving client",
                action.title
            );
            assert!(
                action.data.is_some(),
                "`{}` has no resolve data, so its edit can never arrive",
                action.title
            );
        }
    }

    /// The full client round trip for the unused-declaration fixes: ask at
    /// the diagnostic's own range, resolve the action, apply the edit,
    /// assert the resulting document.
    #[test]
    fn the_unused_declaration_fixes_round_trip_through_resolve() {
        let source = "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}";
        let h = Harness::new(source);
        let diagnostics = published_unused_diagnostics(source);
        let range = diagnostics[0].range;
        assert_eq!(
            h.round_trip(range, diagnostics.clone(), "Prefix"),
            "void Main() {\n    int _x = 1;\n    Print(\"hi\");\n}"
        );
        assert_eq!(
            h.round_trip(range, diagnostics, "Remove"),
            "void Main() {\n    Print(\"hi\");\n}"
        );
    }

    /// The same round trip for the "did you mean" fixes.
    #[test]
    fn the_did_you_mean_fix_round_trips_through_resolve() {
        let source = "Foa value;";
        let h = Harness::with_workspace(source, workspace_with_class("Foo"));
        let range = mk_range(0, 0, 0, 3);
        let diagnostics = vec![mk_diag(range, "unknown type `Foa`")];
        assert_eq!(h.round_trip(range, diagnostics, "Foo"), "Foo value;");
    }

    /// The action identifies the spelling that was offered. A workspace
    /// rebuild may legitimately remove that candidate before the user
    /// clicks the lightbulb; as long as the target document itself is byte-
    /// identical, resolve must still apply the already-offered spelling.
    #[test]
    fn did_you_mean_resolves_the_offered_spelling_after_workspace_candidates_change() {
        let source = "Foa value;";
        let range = mk_range(0, 0, 0, 3);
        let offered = Harness::with_workspace(source, workspace_with_class("Foo"))
            .at(range, vec![mk_diag(range, "unknown type `Foa`")])
            .into_iter()
            .find(|action| action.title == "Did you mean 'Foo'?")
            .expect("the Foo spelling was offered");

        let current = Harness::new(source);
        let resolved = current.resolve(offered);
        assert_eq!(
            apply_edits(source, edits_of(&resolved, &current.uri)),
            "Foo value;"
        );
    }

    #[test]
    fn published_action_json_uses_compact_resolve_data_but_keeps_attached_diagnostic() {
        let source = "Foa value;";
        let range = mk_range(0, 0, 0, 3);
        let mut diagnostic = mk_diag(range, "unknown type `Foa`");
        diagnostic.code = Some(NumberOrString::String("client-code".into()));
        diagnostic.severity = Some(DiagnosticSeverity::WARNING);
        diagnostic.tags = Some(vec![DiagnosticTag::UNNECESSARY]);
        diagnostic.data = Some(serde_json::json!({
            "arbitraryClientPayload": "MUST_NOT_BE_AMPLIFIED"
        }));
        let offered = Harness::with_workspace(source, workspace_with_class("Foo"))
            .at(range, vec![diagnostic.clone()])
            .into_iter()
            .find(|action| action.title == "Did you mean 'Foo'?")
            .expect("did-you-mean action");
        assert_eq!(
            offered.diagnostics.as_deref(),
            Some(std::slice::from_ref(&diagnostic)),
            "the action still points at the exact client diagnostic"
        );

        let json = serde_json::to_value(&offered).expect("serialize CodeAction");
        let resolve_json = json.get("data").expect("resolve data").to_string();
        for forbidden in [
            "MUST_NOT_BE_AMPLIFIED",
            "arbitraryClientPayload",
            "severity",
            "tags",
        ] {
            assert!(
                !resolve_json.contains(forbidden),
                "resolve data copied arbitrary diagnostic field `{forbidden}`: {resolve_json}"
            );
        }

        let wire = serde_json::to_string(&offered).expect("serialize action to wire JSON");
        let decoded: CodeAction = serde_json::from_str(&wire).expect("deserialize wire action");
        let current = Harness::new(source);
        let resolved = current.resolve(decoded);
        assert_eq!(
            apply_edits(source, edits_of(&resolved, &current.uri)),
            "Foo value;"
        );
    }

    #[test]
    fn fix_all_json_also_compacts_published_diagnostics_and_round_trips() {
        let source = "void Main() { int x = 1; Print(\"hi\"); }";
        let mut diagnostics = published_unused_diagnostics(source);
        for diagnostic in &mut diagnostics {
            diagnostic.data = Some(serde_json::json!({
                "arbitraryClientPayload": "FIX_ALL_MUST_NOT_AMPLIFY"
            }));
            diagnostic.tags = Some(vec![DiagnosticTag::UNNECESSARY]);
        }
        let h = Harness::new(source);
        let offered = h
            .request(
                CodeActionContext {
                    diagnostics,
                    only: Some(vec![SOURCE_FIX_ALL_OPENPLANET]),
                    ..Default::default()
                },
                whole_doc(source),
                true,
            )
            .into_iter()
            .find(|action| action.kind == Some(SOURCE_FIX_ALL_OPENPLANET))
            .expect("fix-all action");
        let resolve_json = offered.data.as_ref().expect("resolve data").to_string();
        assert!(!resolve_json.contains("FIX_ALL_MUST_NOT_AMPLIFY"));
        assert!(!resolve_json.contains("arbitraryClientPayload"));
        assert!(!resolve_json.contains("tags"));

        let wire = serde_json::to_string(&offered).expect("serialize action");
        let decoded: CodeAction = serde_json::from_str(&wire).expect("deserialize action");
        let resolved = h.resolve(decoded);
        assert_eq!(
            apply_edits(source, edits_of(&resolved, &h.uri)),
            "void Main() { int _x = 1; Print(\"hi\"); }"
        );
    }

    /// A published diagnostic the request did not ask about must not drive
    /// an action. Spec-compliant clients only echo diagnostics overlapping
    /// the requested range, but the seam applies its own range filter to
    /// checker-keyed fixes and must apply the same one here rather than
    /// trusting the client to have filtered.
    #[test]
    fn a_published_diagnostic_outside_the_requested_range_offers_no_action() {
        let source = "void Main() {\n    int x = 1;\n    Print(\"hi\");\n}";
        let h = Harness::new(source);
        let diagnostics = published_unused_diagnostics(source);
        let elsewhere = mk_range(3, 0, 3, 1);
        assert!(
            !ranges_touch(&diagnostics[0].range, &elsewhere),
            "test premise: the chosen range is away from the finding"
        );
        assert!(
            of_kind(&h.at(elsewhere, diagnostics), &CodeActionKind::QUICKFIX).is_empty(),
            "an unused-declaration fix was offered for a range it does not cover"
        );

        let typo = "Foa value;\nint later;\n";
        let h = Harness::with_workspace(typo, workspace_with_class("Foo"));
        let diagnostics = vec![mk_diag(mk_range(0, 0, 0, 3), "unknown type `Foa`")];
        assert!(
            of_kind(
                &h.at(mk_range(1, 0, 1, 1), diagnostics),
                &CodeActionKind::QUICKFIX
            )
            .is_empty(),
            "a did-you-mean fix was offered for a range it does not cover"
        );
    }

    /// LSP ranges are UTF-16 code units. An astral character earlier on the
    /// edited line shifts every column, and a fix that measured bytes would
    /// land two columns late.
    #[test]
    fn published_registry_edits_are_measured_in_utf16_code_units() {
        // `𝄞` is one astral char: 1 char, 4 bytes, 2 UTF-16 code units.
        let source = "void Main() { Print(\"𝄞\"); int x = 1; }";
        let h = Harness::new(source);
        let diagnostics = published_unused_diagnostics(source);
        let range = diagnostics[0].range;
        assert_eq!(
            range.start.character,
            source
                .split_once("int x")
                .expect("the declaration")
                .0
                .encode_utf16()
                .count() as u32
                + 4,
            "the published range itself must already be in UTF-16 units"
        );
        assert_eq!(
            h.round_trip(range, diagnostics, "Prefix"),
            "void Main() { Print(\"𝄞\"); int _x = 1; }"
        );
    }
}
