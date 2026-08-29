//! The code-action **fix seam** (B035).
//!
//! One dispatch point between "the checker found something" and "here is an
//! edit that fixes it", so a fix family can be added as a new file plus one
//! row in [`super::fixes::FAMILIES`] — never by editing the request handler.
//!
//! A family author writes exactly two things (see
//! [`super::fixes::string_by_value`] for the worked example):
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
//! ) -> Option<Fix> { .. }
//! ```
//!
//! Both callbacks are deliberately plain `fn` pointers: the registry stays a
//! `const` slice, so adding a family costs no runtime setup and no shared
//! mutable state.

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::server::diagnostics::span_to_range;
use crate::symbols::SymbolTable;
use crate::typecheck::{
    Checker, GlobalScope, TypeDiagnostic, TypeDiagnosticKind, TypeDiagnosticSeverity,
};
use crate::typedb::TypeIndex;

use super::fixes::FAMILIES;

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
    /// Materialize exactly the selected descriptor.
    pub materialize: fn(&TypeDiagnostic, &FixContext<'_>, &str) -> Option<Fix>,
}

#[derive(Debug, Clone)]
pub struct FixDescriptorCandidate {
    pub family: &'static str,
    pub diagnostic: Diagnostic,
    pub descriptor: FixDescriptor,
    type_diagnostic: TypeDiagnostic,
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

/// Every fix every registered family offers for this document, in a stable
/// order: checker order, then registry order, then producer order.
pub fn collect_fixes(ctx: &FixContext<'_>) -> Vec<FixCandidate> {
    collect_fix_descriptors(ctx)
        .iter()
        .filter_map(|descriptor| materialize_descriptor(ctx, descriptor, FAMILIES))
        .collect()
}

/// Cheap list-time view: checker diagnostics plus family metadata, no edits.
pub fn collect_fix_descriptors(ctx: &FixContext<'_>) -> Vec<FixDescriptorCandidate> {
    collect_fix_descriptors_from(ctx, FAMILIES)
}

pub fn collect_fix_descriptors_from(
    ctx: &FixContext<'_>,
    families: &[FixFamily],
) -> Vec<FixDescriptorCandidate> {
    let source = &ctx.analysis.source;
    let mut out = Vec::new();
    for diag in type_diagnostics(ctx) {
        for family in families {
            if !(family.matches)(&diag.kind) {
                continue;
            }
            let published = published_form(source, &diag);
            for descriptor in (family.describe)(&diag, ctx) {
                out.push(FixDescriptorCandidate {
                    family: family.name,
                    diagnostic: published.clone(),
                    descriptor,
                    type_diagnostic: diag.clone(),
                });
            }
        }
    }
    out
}

pub fn materialize_descriptor(
    ctx: &FixContext<'_>,
    candidate: &FixDescriptorCandidate,
    families: &[FixFamily],
) -> Option<FixCandidate> {
    let family = families
        .iter()
        .find(|family| family.name == candidate.family)?;
    let fix = (family.materialize)(&candidate.type_diagnostic, ctx, &candidate.descriptor.title)?;
    Some(FixCandidate {
        family: family.name,
        diagnostic: candidate.diagnostic.clone(),
        fix,
    })
}

/// Materialize only the descriptors eligible for `source.fixAll.openplanet`.
///
/// The batcher never needs the fixes a user has to choose between, and a
/// family's `materialize` may be the expensive half — so a fix-all over a
/// document with many offers must not pay for the ones it will discard.
pub fn materialize_auto_descriptors(
    ctx: &FixContext<'_>,
    descriptors: &[FixDescriptorCandidate],
    families: &[FixFamily],
) -> Vec<FixCandidate> {
    descriptors
        .iter()
        .filter(|candidate| candidate.descriptor.auto_applicable)
        .filter_map(|candidate| materialize_descriptor(ctx, candidate, families))
        .collect()
}

/// Re-find and materialize one descriptor against the current document.
/// Missing means the action is stale and resolve should leave it unchanged.
pub fn materialize_fix(
    ctx: &FixContext<'_>,
    family_name: &str,
    diagnostic_range: Range,
    title: &str,
) -> Option<FixCandidate> {
    let descriptor = collect_fix_descriptors(ctx).into_iter().find(|candidate| {
        candidate.family == family_name
            && candidate.diagnostic.range == diagnostic_range
            && candidate.descriptor.title == title
    })?;
    materialize_descriptor(ctx, &descriptor, FAMILIES)
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
        title: &str,
    ) -> Option<Fix> {
        if title != "Synthetic cross-file fix" {
            return None;
        }
        let second_uri = ctx
            .files
            .iter()
            .find(|(_, analysis)| analysis.source.contains("enable synthetic"))?
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
        Some(Fix {
            title: title.to_string(),
            kind: CodeActionKind::QUICKFIX,
            edit: WorkspaceEdit::new(changes),
            is_preferred: false,
            auto_applicable: false,
        })
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

    fn counted_fix(ctx: &FixContext<'_>, title: &str) -> Fix {
        let mut changes = std::collections::HashMap::new();
        changes.insert(
            ctx.uri.clone(),
            vec![TextEdit::new(Range::default(), title.to_string())],
        );
        Fix {
            title: title.to_string(),
            kind: CodeActionKind::QUICKFIX,
            edit: WorkspaceEdit::new(changes),
            is_preferred: false,
            auto_applicable: title == "auto",
        }
    }

    fn auto_materialize(_: &TypeDiagnostic, ctx: &FixContext<'_>, _: &str) -> Option<Fix> {
        AUTO_MATERIALIZED.with(|count| count.set(count.get() + 1));
        Some(counted_fix(ctx, "auto"))
    }

    fn manual_materialize(_: &TypeDiagnostic, ctx: &FixContext<'_>, _: &str) -> Option<Fix> {
        MANUAL_MATERIALIZED.with(|count| count.set(count.get() + 1));
        Some(counted_fix(ctx, "manual"))
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
        let descriptors = collect_fix_descriptors_from(&ctx, &COUNTED_FAMILIES);
        AUTO_MATERIALIZED.set(0);
        MANUAL_MATERIALIZED.set(0);

        let candidates = materialize_auto_descriptors(&ctx, &descriptors, &COUNTED_FAMILIES);
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

        let descriptors = collect_fix_descriptors_from(&ctx, &[SYNTHETIC_FAMILY]);
        assert_eq!(descriptors.len(), 1);
        let fix = materialize_descriptor(&ctx, &descriptors[0], &[SYNTHETIC_FAMILY])
            .expect("synthetic fix materialized");
        let changes = fix.fix.edit.changes.expect("plain workspace changes");
        assert_eq!(changes.get(&uri).unwrap()[0].new_text, "current");
        assert_eq!(changes.get(&second_uri).unwrap()[0].new_text, "second");

        let no_context_files = vec![(uri.clone(), &analysis)];
        let no_context = FixContext {
            files: WorkspaceFileResolver::new(&no_context_files),
            ..ctx
        };
        assert!(collect_fix_descriptors_from(&no_context, &[SYNTHETIC_FAMILY]).is_empty());
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
