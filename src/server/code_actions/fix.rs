//! The code-action **fix seam** (B035, widened by B047).
//!
//! One dispatch point between "something found a problem" and "here is an
//! edit that fixes it", so a fix family can be added as a new file plus one
//! row in a registry — never by editing the request handler.
//!
//! # Two registries, one path
//!
//! A fix family is keyed on whatever identifies its problem, and the server
//! produces problems two ways:
//!
//! * [`FixFamily`] — keyed on a `TypeDiagnosticKind`. Its findings come from
//!   the **type checker**, which the seam re-runs itself
//!   ([`type_diagnostics`]), so nothing has to be echoed back to us.
//! * [`DiagnosticFixFamily`] — keyed on the published [`Diagnostic`]. Its
//!   findings are produced elsewhere in the server (unused declarations come
//!   from [`crate::analysis::unused`], through inputs no [`FixContext`]
//!   carries), so the only thing the code-action handler has to work from is
//!   the diagnostic the **client echoed back** in `CodeActionContext`.
//!
//! Which registry a fix belongs in is decided by where its diagnostic is
//! produced, and by nothing else about the fix. Everything downstream of
//! [`collect_fix_descriptors_from`] — the offer, `codeAction/resolve`, the
//! `source.fixAll.openplanet` batcher — is registry-agnostic and sees one
//! stream of [`FixDescriptorCandidate`].
//!
//! A family author writes exactly two things (see
//! [`super::fixes::string_by_value`] and [`super::fixes::unused_decl`] for
//! the worked examples of each):
//!
//! ```ignore
//! pub const FAMILY: FixFamily = FixFamily {
//!     name: "my-family", matches, describe, materialize
//! };
//!
//! /// The discriminant(s) this family claims. One `matches!` arm.
//! fn matches(kind: &TypeDiagnosticKind) -> bool {
//!     matches!(kind, TypeDiagnosticKind::MyKind { .. })
//! }
//!
//! fn describe(diag: &TypeDiagnostic) -> Vec<FixDescriptor> { .. }
//! fn materialize(
//!     diag: &TypeDiagnostic, ctx: &FixContext<'_>, title: &str
//! ) -> WorkspaceEdit { .. }
//! ```
//!
//! Both callbacks are deliberately plain `fn` pointers: the registries stay
//! `const` slices, so adding a family costs no runtime setup and no shared
//! mutable state.

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::server::diagnostics::{span_to_range, DIAGNOSTIC_SOURCE};
use crate::symbols::SymbolTable;
use crate::typecheck::{
    Checker, GlobalScope, TypeDiagnostic, TypeDiagnosticKind, TypeDiagnosticSeverity,
};
use crate::typedb::TypeIndex;

use super::fixes::{DIAGNOSTIC_FAMILIES, FAMILIES};

/// Our namespaced fix-all kind. Namespaced (rather than plain
/// `source.fixAll`) so a user can name *our* fixes in
/// `editor.codeActionsOnSave` without opting into every other server's.
pub const SOURCE_FIX_ALL_OPENPLANET: CodeActionKind =
    CodeActionKind::new("source.fixAll.openplanet");

/// One concrete fix: a title plus the edits that apply it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub title: String,
    pub kind: CodeActionKind,
    /// The complete edit selected by the user. Families may edit multiple
    /// files; source fix-all separately admits only document-local edits.
    pub edit: WorkspaceEdit,
    /// Only where the fix is unambiguous *and* lossless (B035 decision).
    /// A family offering two alternatives must leave this `false` on both.
    pub is_preferred: bool,
    /// Eligible for batching into `source.fixAll.openplanet`. A fix that
    /// needs user judgement must set this `false` even when it is preferred.
    pub auto_applicable: bool,
}

/// Cheap menu metadata for one fix. Describing an action must never compute
/// its edits; those are materialized only for eager clients or on resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct FixDescriptor {
    pub title: String,
    pub kind: CodeActionKind,
    pub is_preferred: bool,
    pub auto_applicable: bool,
}

/// Read-only URI-to-analysis view of the workspace available to a family.
#[derive(Clone, Copy)]
pub struct WorkspaceFileResolver<'a> {
    files: &'a [(Url, &'a DocumentAnalysis)],
}

impl<'a> WorkspaceFileResolver<'a> {
    pub fn new(files: &'a [(Url, &'a DocumentAnalysis)]) -> Self {
        Self { files }
    }

    pub fn empty() -> Self {
        Self { files: &[] }
    }

    pub fn get(&self, uri: &Url) -> Option<&'a DocumentAnalysis> {
        self.files
            .iter()
            .find_map(|(candidate, analysis)| (candidate == uri).then_some(*analysis))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Url, &'a DocumentAnalysis)> + '_ {
        self.files.iter().map(|(uri, analysis)| (uri, *analysis))
    }
}

/// Everything a fix producer is allowed to look at.
#[derive(Clone, Copy)]
pub struct FixContext<'a> {
    pub uri: &'a Url,
    pub analysis: &'a DocumentAnalysis,
    pub workspace: &'a SymbolTable,
    pub type_index: Option<&'a TypeIndex>,
    pub files: WorkspaceFileResolver<'a>,
}

/// A registered fix family. See the module docs for the author's contract.
pub struct FixFamily {
    /// Stable identifier, round-tripped through `CodeAction.data` so
    /// `codeAction/resolve` can re-enter the same producer.
    pub name: &'static str,
    /// Discriminant test: does this family claim that diagnostic kind?
    pub matches: fn(&TypeDiagnosticKind) -> bool,
    /// Describe the menu entries for one claimed diagnostic, without edits.
    pub describe: fn(&TypeDiagnostic, &FixContext<'_>) -> Vec<FixDescriptor>,
    /// Materialize exactly the selected descriptor. This must be total for
    /// every descriptor returned by [`Self::describe`]. Action identity is
    /// deliberately absent: the seam owns it through [`FixDescriptor`].
    pub materialize: fn(&TypeDiagnostic, &FixContext<'_>, &str) -> WorkspaceEdit,
}

/// A registered fix family keyed on the **published** diagnostic rather than
/// on a checker discriminant, for findings the type checker does not produce.
///
/// Its input is the `Diagnostic` the client echoed back in
/// `CodeActionContext.diagnostics`. Match on [`Diagnostic::code`] — that is
/// the field the server stamps precisely so a fix can find its own findings
/// again (`"unused-declaration"`, say). Matching on the message is a last
/// resort, for a diagnostic class that carries no code.
///
/// The seam filters out diagnostics published by *another* server before
/// dispatch (see [`is_our_diagnostic`]), so a family never has to.
pub struct DiagnosticFixFamily {
    /// Stable identifier, round-tripped through `CodeAction.data`. Unique
    /// across BOTH registries — see `family_names_are_unique_across_registries`.
    pub name: &'static str,
    /// Does this family claim that published diagnostic?
    pub matches: fn(&Diagnostic) -> bool,
    /// Describe the menu entries for one claimed diagnostic, without edits.
    pub describe: fn(&Diagnostic, &FixContext<'_>) -> Vec<FixDescriptor>,
    /// Materialize exactly the selected descriptor. Total for every
    /// descriptor [`Self::describe`] returned.
    pub materialize: fn(&Diagnostic, &FixContext<'_>, &str) -> WorkspaceEdit,
}

/// The pair of registries one request dispatches through.
///
/// Carried as a value rather than reached for globally so a test can run the
/// whole handler against a synthetic registry — which is the only way to
/// prove the *seam* behaves, as opposed to proving the families do.
#[derive(Clone, Copy)]
pub struct Registries<'a> {
    /// Keyed on `TypeDiagnosticKind`; the seam re-runs the checker.
    pub checker: &'a [FixFamily],
    /// Keyed on the published `Diagnostic`; the client echoes these back.
    pub published: &'a [DiagnosticFixFamily],
}

impl<'a> Registries<'a> {
    /// Only the checker-keyed half — what a family's own tests exercise, and
    /// what a caller with no echoed diagnostics to offer can use.
    pub const fn checker_only(checker: &'a [FixFamily]) -> Self {
        Self {
            checker,
            published: &[],
        }
    }
}

/// Everything registered, which is what the server runs.
pub const REGISTRIES: Registries<'static> = Registries {
    checker: FAMILIES,
    published: DIAGNOSTIC_FAMILIES,
};

/// Which registry produced a candidate, and the producer's input.
#[derive(Debug, Clone)]
enum Subject {
    /// A checker finding. [`FixDescriptorCandidate::diagnostic`] is only its
    /// published *form*, so the structured finding travels alongside.
    Checker(TypeDiagnostic),
    /// A published diagnostic, which IS
    /// [`FixDescriptorCandidate::diagnostic`].
    Published,
}

#[derive(Debug, Clone)]
pub struct FixDescriptorCandidate {
    pub family: &'static str,
    pub diagnostic: Diagnostic,
    pub descriptor: FixDescriptor,
    subject: Subject,
}

impl FixDescriptorCandidate {
    /// The published diagnostic this candidate was produced *from*, for a
    /// candidate out of the diagnostic-keyed registry.
    ///
    /// `None` for a checker-keyed candidate: the seam re-derives those from
    /// the document, so nothing about them has to be carried. The difference
    /// matters at `codeAction/resolve`, where the client echoes no
    /// diagnostics and a diagnostic-keyed producer would otherwise have no
    /// input at all.
    pub fn published_subject(&self) -> Option<&Diagnostic> {
        matches!(self.subject, Subject::Published).then_some(&self.diagnostic)
    }
}

/// A produced fix plus the provenance needed to (a) attach it to the right
/// squiggle and (b) re-derive it during `codeAction/resolve`.
#[derive(Debug, Clone)]
pub struct FixCandidate {
    pub family: &'static str,
    /// The diagnostic this fix answers, in exactly the shape
    /// `publishDiagnostics` sent it — so the action can be tied back to the
    /// squiggle even when the client did not echo the diagnostic to us.
    pub diagnostic: Diagnostic,
    /// The seam-built fix, retained for fix-all's existing edit projection.
    /// Its metadata is copied from the selected descriptor, never supplied
    /// by a family.
    pub fix: Fix,
}

/// Run the type checker over the document the way `publishDiagnostics` does,
/// so fix producers see the same findings the user sees squiggled.
pub fn type_diagnostics(ctx: &FixContext<'_>) -> Vec<TypeDiagnostic> {
    let scope = GlobalScope::new(ctx.workspace, ctx.type_index);
    let mut checker = Checker::new(ctx.analysis.masked_source(), &scope);
    checker.check_file(&ctx.analysis.file);
    checker.diagnostics
}

/// Is this diagnostic one of ours? A diagnostic published by another server
/// must never drive our quick fixes, even when the message happens to match.
/// A missing `source` is treated as ours: some clients drop the field on the
/// round trip, and refusing would silently disable the feature.
pub fn is_our_diagnostic(diag: &Diagnostic) -> bool {
    diag.source
        .as_deref()
        .is_none_or(|s| s == DIAGNOSTIC_SOURCE)
}

/// A single-document workspace edit. The common shape by far: a family that
/// edits one file should not have to know `WorkspaceEdit`'s three fields.
pub fn single_file_edit(uri: &Url, edits: Vec<TextEdit>) -> WorkspaceEdit {
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    changes.insert(uri.clone(), edits);
    WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    }
}

/// The name a diagnostic of ours puts in backticks — every message that
/// names one declaration does it the same way, so both diagnostic-keyed
/// families read it through here rather than each parsing prose.
pub fn extract_name_from_diagnostic(message: &str) -> Option<&str> {
    let start = message.find('`')? + 1;
    let rest = &message[start..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

/// Every fix every registered checker family offers for this document, in a
/// stable order: checker order, then registry order, then producer order.
///
/// No echoed diagnostics, so no diagnostic-keyed family contributes — this
/// is the family-scoped view a `FixFamily`'s own tests want.
pub fn collect_fixes(ctx: &FixContext<'_>) -> Vec<FixCandidate> {
    collect_fix_descriptors(ctx)
        .iter()
        .map(|descriptor| materialize_descriptor(ctx, descriptor, REGISTRIES))
        .collect()
}

/// Cheap list-time view: checker diagnostics plus family metadata, no edits.
pub fn collect_fix_descriptors(ctx: &FixContext<'_>) -> Vec<FixDescriptorCandidate> {
    collect_fix_descriptors_from(ctx, &[], REGISTRIES)
}

/// Every descriptor both registries offer, in the order the client sees
/// them: diagnostic-keyed producers first (in the order the client echoed
/// their diagnostics), then checker-keyed ones (checker order, then registry
/// order).
///
/// `echoed` is `CodeActionContext.diagnostics` — the client's own copy of
/// what we published. Foreign diagnostics are dropped here, once, so no
/// producer has to re-check.
pub fn collect_fix_descriptors_from(
    ctx: &FixContext<'_>,
    echoed: &[Diagnostic],
    registries: Registries<'_>,
) -> Vec<FixDescriptorCandidate> {
    let mut out = Vec::new();
    for diagnostic in echoed {
        if !is_our_diagnostic(diagnostic) {
            continue;
        }
        for family in registries.published {
            if !(family.matches)(diagnostic) {
                continue;
            }
            for descriptor in (family.describe)(diagnostic, ctx) {
                out.push(FixDescriptorCandidate {
                    family: family.name,
                    diagnostic: diagnostic.clone(),
                    descriptor,
                    subject: Subject::Published,
                });
            }
        }
    }
    // Running the checker is the expensive half, and a caller with an empty
    // checker registry (a diagnostic-keyed family's own tests) must not pay
    // for it.
    if registries.checker.is_empty() {
        return out;
    }
    let source = &ctx.analysis.source;
    for diag in type_diagnostics(ctx) {
        for family in registries.checker {
            if !(family.matches)(&diag.kind) {
                continue;
            }
            let published = published_form(source, &diag);
            for descriptor in (family.describe)(&diag, ctx) {
                out.push(FixDescriptorCandidate {
                    family: family.name,
                    diagnostic: published.clone(),
                    descriptor,
                    subject: Subject::Checker(diag.clone()),
                });
            }
        }
    }
    out
}

pub fn materialize_descriptor(
    ctx: &FixContext<'_>,
    candidate: &FixDescriptorCandidate,
    registries: Registries<'_>,
) -> FixCandidate {
    // Callers pair descriptors with the registries that produced them, so a
    // miss is a programming error — loud in development, but an editor-
    // facing daemon must not die of it mid-session.
    let title = &candidate.descriptor.title;
    let edit = match &candidate.subject {
        Subject::Checker(diag) => registries
            .checker
            .iter()
            .find(|family| family.name == candidate.family)
            .map(|family| (family.materialize)(diag, ctx, title)),
        Subject::Published => registries
            .published
            .iter()
            .find(|family| family.name == candidate.family)
            .map(|family| (family.materialize)(&candidate.diagnostic, ctx, title)),
    };
    debug_assert!(
        edit.is_some(),
        "descriptor from unregistered family `{}`",
        candidate.family
    );
    let edit = edit.unwrap_or_default();
    FixCandidate {
        family: candidate.family,
        diagnostic: candidate.diagnostic.clone(),
        fix: Fix {
            title: candidate.descriptor.title.clone(),
            kind: candidate.descriptor.kind.clone(),
            edit,
            is_preferred: candidate.descriptor.is_preferred,
            auto_applicable: candidate.descriptor.auto_applicable,
        },
    }
}

/// Materialize only the descriptors eligible for `source.fixAll.openplanet`.
///
/// The batcher never needs the fixes a user has to choose between, and a
/// family's `materialize` may be the expensive half — so a fix-all over a
/// document with many offers must not pay for the ones it will discard.
pub fn materialize_auto_descriptors(
    ctx: &FixContext<'_>,
    descriptors: &[FixDescriptorCandidate],
    registries: Registries<'_>,
) -> Vec<FixCandidate> {
    descriptors
        .iter()
        .filter(|candidate| candidate.descriptor.auto_applicable)
        .map(|candidate| materialize_descriptor(ctx, candidate, registries))
        .collect()
}

/// Re-find and materialize one descriptor against the current document.
/// Missing means the action is stale and resolve should leave it unchanged.
///
/// `echoed` supplies the input for a diagnostic-keyed family, which at
/// `codeAction/resolve` is not the client's live diagnostic list but the one
/// diagnostic the action carried with it (see `ResolveAction::Fix`).
pub fn materialize_fix_from(
    ctx: &FixContext<'_>,
    echoed: &[Diagnostic],
    family_name: &str,
    diagnostic_range: Range,
    title: &str,
    registries: Registries<'_>,
) -> Option<FixCandidate> {
    let descriptor = collect_fix_descriptors_from(ctx, echoed, registries)
        .into_iter()
        .find(|candidate| {
            candidate.family == family_name
                && candidate.diagnostic.range == diagnostic_range
                && candidate.descriptor.title == title
        })?;
    Some(materialize_descriptor(ctx, &descriptor, registries))
}

/// The LSP diagnostic `publishDiagnostics` would have sent for this finding.
/// Kept in step with `super::super::diagnostics::compute_diagnostics_from_analysis`.
fn published_form(source: &str, diag: &TypeDiagnostic) -> Diagnostic {
    let severity = match diag.severity() {
        TypeDiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
        TypeDiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
    };
    Diagnostic {
        range: span_to_range(source, diag.span),
        severity: Some(severity),
        message: diag.message(),
        source: Some(crate::server::diagnostics::DIAGNOSTIC_SOURCE.to_string()),
        ..Default::default()
    }
}

/// LSP `CodeActionContext.only` matching: a requested kind selects an action
/// kind when it is equal to it, or is a `.`-separated *prefix* of it
/// (`source.fixAll` selects `source.fixAll.openplanet`, but `source.fixAll`
/// must not select a hypothetical `source.fixAllOfTheThings`).
pub fn kind_selected(kind: &CodeActionKind, only: Option<&[CodeActionKind]>) -> bool {
    let Some(only) = only else {
        return true;
    };
    let kind = kind.as_str();
    only.iter().any(|want| {
        let want = want.as_str();
        kind == want
            || (kind.len() > want.len()
                && kind.starts_with(want)
                && kind.as_bytes()[want.len()] == b'.')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::code_actions::test_fixtures::ctx_for;

    fn synthetic_matches(kind: &TypeDiagnosticKind) -> bool {
        matches!(kind, TypeDiagnosticKind::StringByValueParam { .. })
    }

    fn synthetic_describe(
        _diagnostic: &TypeDiagnostic,
        ctx: &FixContext<'_>,
    ) -> Vec<FixDescriptor> {
        ctx.files
            .iter()
            .any(|(_, analysis)| analysis.source.contains("enable synthetic"))
            .then(|| FixDescriptor {
                title: "Synthetic cross-file fix".to_string(),
                kind: CodeActionKind::QUICKFIX,
                is_preferred: false,
                auto_applicable: false,
            })
            .into_iter()
            .collect()
    }

    fn synthetic_materialize(
        _diagnostic: &TypeDiagnostic,
        ctx: &FixContext<'_>,
        _: &str,
    ) -> WorkspaceEdit {
        let second_uri = ctx
            .files
            .iter()
            .find(|(_, analysis)| analysis.source.contains("enable synthetic"))
            .expect("synthetic descriptor requires the second file")
            .0
            .clone();
        let mut changes = std::collections::HashMap::new();
        changes.insert(
            ctx.uri.clone(),
            vec![TextEdit::new(Range::default(), "current".to_string())],
        );
        changes.insert(
            second_uri,
            vec![TextEdit::new(Range::default(), "second".to_string())],
        );
        WorkspaceEdit::new(changes)
    }

    const SYNTHETIC_FAMILY: FixFamily = FixFamily {
        name: "test-cross-file",
        matches: synthetic_matches,
        describe: synthetic_describe,
        materialize: synthetic_materialize,
    };

    thread_local! {
        static AUTO_MATERIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static MANUAL_MATERIALIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn counted_descriptor(title: &str, auto_applicable: bool) -> FixDescriptor {
        FixDescriptor {
            title: title.to_string(),
            kind: CodeActionKind::QUICKFIX,
            is_preferred: false,
            auto_applicable,
        }
    }

    fn auto_describe(_: &TypeDiagnostic, _: &FixContext<'_>) -> Vec<FixDescriptor> {
        vec![counted_descriptor("auto", true)]
    }

    fn manual_describe(_: &TypeDiagnostic, _: &FixContext<'_>) -> Vec<FixDescriptor> {
        vec![counted_descriptor("manual", false)]
    }

    fn counted_fix(ctx: &FixContext<'_>, title: &str) -> WorkspaceEdit {
        let mut changes = std::collections::HashMap::new();
        changes.insert(
            ctx.uri.clone(),
            vec![TextEdit::new(Range::default(), title.to_string())],
        );
        WorkspaceEdit::new(changes)
    }

    fn auto_materialize(_: &TypeDiagnostic, ctx: &FixContext<'_>, _: &str) -> WorkspaceEdit {
        AUTO_MATERIALIZED.with(|count| count.set(count.get() + 1));
        counted_fix(ctx, "auto")
    }

    fn manual_materialize(_: &TypeDiagnostic, ctx: &FixContext<'_>, _: &str) -> WorkspaceEdit {
        MANUAL_MATERIALIZED.with(|count| count.set(count.get() + 1));
        counted_fix(ctx, "manual")
    }

    const COUNTED_FAMILIES: [FixFamily; 2] = [
        FixFamily {
            name: "test-auto",
            matches: synthetic_matches,
            describe: auto_describe,
            materialize: auto_materialize,
        },
        FixFamily {
            name: "test-manual",
            matches: synthetic_matches,
            describe: manual_describe,
            materialize: manual_materialize,
        },
    ];

    #[test]
    fn fix_all_materializes_auto_descriptors_only() {
        let (analysis, workspace, uri) = ctx_for("void f(string value) {}");
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::empty(),
        };
        let registries = Registries::checker_only(&COUNTED_FAMILIES);
        let descriptors = collect_fix_descriptors_from(&ctx, &[], registries);
        AUTO_MATERIALIZED.set(0);
        MANUAL_MATERIALIZED.set(0);

        let candidates = materialize_auto_descriptors(&ctx, &descriptors, registries);
        assert_eq!(AUTO_MATERIALIZED.get(), 1);
        assert_eq!(MANUAL_MATERIALIZED.get(), 0);
        let action = super::super::fix_all::fix_all_action(&uri, &candidates)
            .expect("one batched workspace edit");
        let edits = action
            .edit
            .and_then(|edit| edit.changes)
            .and_then(|mut changes| changes.remove(&uri))
            .expect("document edits");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "auto");
    }

    #[test]
    fn registry_family_can_use_workspace_context_and_materialize_cross_file_edit() {
        let (analysis, workspace, uri) = ctx_for("void f(string value) {}");
        let second_uri = Url::parse("file:///test/second.as").unwrap();
        let second = DocumentAnalysis::analyze_plain("// enable synthetic\n");
        let files = vec![
            (0, (uri.clone(), &analysis)),
            (1, (second_uri.clone(), &second)),
        ];
        let files: Vec<_> = files.into_iter().map(|(_, file)| file).collect();
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::new(&files),
        };

        let registries = Registries::checker_only(&[SYNTHETIC_FAMILY]);
        let descriptors = collect_fix_descriptors_from(&ctx, &[], registries);
        assert_eq!(descriptors.len(), 1);
        let fix = materialize_descriptor(&ctx, &descriptors[0], registries);
        let changes = fix.fix.edit.changes.expect("plain workspace changes");
        assert_eq!(changes.get(&uri).unwrap()[0].new_text, "current");
        assert_eq!(changes.get(&second_uri).unwrap()[0].new_text, "second");

        let no_context_files = vec![(uri.clone(), &analysis)];
        let no_context = FixContext {
            files: WorkspaceFileResolver::new(&no_context_files),
            ..ctx
        };
        assert!(collect_fix_descriptors_from(&no_context, &[], registries).is_empty());
    }

    #[test]
    fn kind_selected_without_filter_accepts_everything() {
        assert!(kind_selected(&CodeActionKind::QUICKFIX, None));
        assert!(kind_selected(&SOURCE_FIX_ALL_OPENPLANET, None));
    }

    #[test]
    fn kind_selected_matches_exactly() {
        let only = [CodeActionKind::QUICKFIX];
        assert!(kind_selected(&CodeActionKind::QUICKFIX, Some(&only)));
        assert!(!kind_selected(
            &CodeActionKind::REFACTOR_REWRITE,
            Some(&only)
        ));
        assert!(!kind_selected(&SOURCE_FIX_ALL_OPENPLANET, Some(&only)));
    }

    #[test]
    fn kind_selected_matches_dotted_prefix() {
        let only = [CodeActionKind::SOURCE_FIX_ALL];
        assert!(kind_selected(&SOURCE_FIX_ALL_OPENPLANET, Some(&only)));
        assert!(kind_selected(&CodeActionKind::SOURCE_FIX_ALL, Some(&only)));

        let only = [CodeActionKind::SOURCE];
        assert!(kind_selected(&SOURCE_FIX_ALL_OPENPLANET, Some(&only)));
        assert!(!kind_selected(&CodeActionKind::QUICKFIX, Some(&only)));
    }

    #[test]
    fn kind_selected_rejects_non_dot_prefix() {
        // `source.fixAll` must not swallow `source.fixAllOfTheThings`.
        let only = [CodeActionKind::SOURCE_FIX_ALL];
        let sneaky = CodeActionKind::new("source.fixAllOfTheThings");
        assert!(!kind_selected(&sneaky, Some(&only)));
    }

    #[test]
    fn kind_selected_with_empty_filter_accepts_nothing() {
        let only: [CodeActionKind; 0] = [];
        assert!(!kind_selected(&CodeActionKind::QUICKFIX, Some(&only)));
    }
}
