//! Registered fix families.
//!
//! **Adding a family.** Write `src/server/code_actions/fixes/<name>.rs`
//! exporting a `pub const FAMILY`, then add its `pub mod` line and its row in
//! the registry that matches where its diagnostic is produced:
//!
//! * a finding of the **type checker** — a [`FixFamily`], keyed on a
//!   `TypeDiagnosticKind`, registered in [`FAMILIES`];
//! * a finding produced **anywhere else** in the server — a
//!   [`DiagnosticFixFamily`], keyed on the published `Diagnostic`, registered
//!   in [`DIAGNOSTIC_FAMILIES`]. The seam has no way to re-derive such a
//!   finding, so its input is the diagnostic the client echoed back with the
//!   request. See [`unused_decl`].
//!
//! Two adjacent one-line edits here are the entire shared-file footprint
//! either way, so parallel lanes conflict at most on this file's import
//! block. Nothing else in the server changes: the request handler, the
//! resolve handler and the fix-all batcher are all registry-agnostic (B047 —
//! before it, only the [`FAMILIES`] half was, and a diagnostic-keyed
//! producer had to be wired into the handler by hand).

use super::fix::{DiagnosticFixFamily, FixFamily};

pub mod conversion;
pub mod did_you_mean;
pub mod removed_api;
pub mod rename_and_const;
pub mod string_by_value;
pub mod unused_decl;
pub mod with_run_context;

/// The checker-keyed registry. Order here decides the order fixes are
/// offered in when two families claim the same diagnostic; it does not
/// affect which fixes the fix-all batcher keeps (see
/// [`super::fix_all::select_non_overlapping`]).
pub const FAMILIES: &[FixFamily] = &[
    string_by_value::FAMILY,
    conversion::float_truncation::FAMILY,
    conversion::sign_change::FAMILY,
    conversion::signed_unsigned::FAMILY,
    conversion::unreachable_code::FAMILY,
    conversion::missing_return::FAMILY,
    rename_and_const::shadowed_variable::FAMILY,
    rename_and_const::duplicate_function::FAMILY,
    rename_and_const::const_receiver::FAMILY,
    removed_api::FAMILY,
    with_run_context::FAMILY,
];

/// The diagnostic-keyed registry, for findings the type checker does not
/// produce. Offered before [`FAMILIES`], which is the order the client saw
/// these two producers in before they were registries at all.
pub const DIAGNOSTIC_FAMILIES: &[DiagnosticFixFamily] =
    &[did_you_mean::FAMILY, unused_decl::FAMILY];

#[cfg(test)]
mod tests {
    use super::*;

    /// A family name is the key `codeAction/resolve` re-enters the producer
    /// with, and the two registries share that namespace — a collision would
    /// resolve one family's action against another family's producer.
    #[test]
    fn family_names_are_unique_across_registries() {
        let mut names: Vec<&str> = FAMILIES
            .iter()
            .map(|family| family.name)
            .chain(DIAGNOSTIC_FAMILIES.iter().map(|family| family.name))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate family name in {names:?}");
    }
}
