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
//! pub const FAMILY: FixFamily = FixFamily { name: "my-family", matches, produce };
//!
//! /// The discriminant(s) this family claims. One `matches!` arm.
//! fn matches(kind: &TypeDiagnosticKind) -> bool {
//!     matches!(kind, TypeDiagnosticKind::MyKind { .. })
//! }
//!
//! /// The fixes for one claimed diagnostic. Must be pure and cheap: it is
//! /// called again during `codeAction/resolve` to materialise the edits.
//! fn produce(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<Fix> { .. }
//! ```
//!
//! `produce` is deliberately a plain `fn` pointer: the registry stays a
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
    /// Edits within the single document the diagnostic came from.
    pub edits: Vec<TextEdit>,
    /// Only where the fix is unambiguous *and* lossless (B035 decision).
    /// A family offering two alternatives must leave this `false` on both.
    pub is_preferred: bool,
    /// Eligible for batching into `source.fixAll.openplanet`. A fix that
    /// needs user judgement must set this `false` even when it is preferred.
    pub auto_applicable: bool,
}

/// Everything a fix producer is allowed to look at.
pub struct FixContext<'a> {
    pub uri: &'a Url,
    pub analysis: &'a DocumentAnalysis,
    pub workspace: &'a SymbolTable,
    pub type_index: Option<&'a TypeIndex>,
}

/// A registered fix family. See the module docs for the author's contract.
pub struct FixFamily {
    /// Stable identifier, round-tripped through `CodeAction.data` so
    /// `codeAction/resolve` can re-enter the same producer.
    pub name: &'static str,
    /// Discriminant test: does this family claim that diagnostic kind?
    pub matches: fn(&TypeDiagnosticKind) -> bool,
    /// Produce the fixes for one claimed diagnostic.
    pub produce: fn(&TypeDiagnostic, &FixContext<'_>) -> Vec<Fix>,
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
    let source = &ctx.analysis.source;
    let mut out = Vec::new();
    for diag in type_diagnostics(ctx) {
        for family in FAMILIES {
            if !(family.matches)(&diag.kind) {
                continue;
            }
            let published = published_form(source, &diag);
            for fix in (family.produce)(&diag, ctx) {
                out.push(FixCandidate {
                    family: family.name,
                    diagnostic: published.clone(),
                    fix,
                });
            }
        }
    }
    out
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
