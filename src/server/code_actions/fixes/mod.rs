//! Registered fix families.
//!
//! **Adding a family (B036 / B037 / B038):** write
//! `src/server/code_actions/fixes/<name>.rs` exporting a
//! `pub const FAMILY: FixFamily`, then add its `pub mod` line and its row in
//! [`FAMILIES`] below. Nothing else in the server changes — not the request
//! handler, not the resolve handler, not the fix-all batcher. Two adjacent
//! one-line edits here are the entire shared-file footprint, so parallel
//! lanes conflict at most on this file's import block.

use super::fix::FixFamily;

pub mod removed_api;
pub mod string_by_value;

/// The registry. Order here decides the order fixes are offered in when two
/// families claim the same diagnostic; it does not affect which fixes the
/// fix-all batcher keeps (see [`super::fix_all::select_non_overlapping`]).
pub const FAMILIES: &[FixFamily] = &[removed_api::FAMILY, string_by_value::FAMILY];
