//! GH #57: define matrix — expand `define_matrix` / `define_groups` /
//! `define_combinations` config into the list of define combos `check`
//! runs, merge duplicate diagnostics across combos, and label them.
//!
//! Combos carry ONLY the axis defines: the base define set (game target /
//! platform + config `defines`) applies to every combo inside
//! [`super::run_check`] and is deliberately not part of the label.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use super::CliDiagnostic;

/// Platform / game-selector defines. When two or more appear as matrix
/// axes they form an implicit mutually-exclusive group (at most one
/// defined per combo) — users never declare this group themselves.
pub const PLATFORM_SELECTORS: [&str; 3] = ["TMNEXT", "MP4", "TURBO"];

/// Matrix size cap when `define_matrix_max_combinations` is absent.
/// 16 keeps `check` wall-clock bounded (each combo is a full analysis
/// pass) while covering 2–4 axis plugins; raise via the config key.
pub const DEFAULT_MAX_COMBINATIONS: usize = 16;

/// Expand the configured matrix into the define combos to check.
///
/// - `axes`: `define_matrix` — each axis toggled on/off on top of the base
///   define set (cross-product, `{}` = base-only combo included).
/// - `groups`: `define_groups` — mutually exclusive: at most one member
///   defined per combo; violating combos are skipped (never analyzed).
///   The platform selectors {TMNEXT, MP4, TURBO} form an implicit group
///   whenever ≥2 of them are axes.
/// - `combinations`: `define_combinations` — when `Some` (even `[]`), the
///   cross-product is skipped entirely and ONLY these combos are checked.
/// - `max`: cap (`define_matrix_max_combinations`, default 16). Exceeding
///   the cap is a hard error naming `define_combinations`; combos are
///   never silently dropped.
///
/// Every returned combo is a sorted, deduped define list; combos
/// themselves are sorted lexicographically for stable output.
pub fn expand_combinations(
    axes: &[String],
    groups: &[Vec<String>],
    combinations: Option<&[Vec<String>]>,
    max: usize,
) -> Result<Vec<Vec<String>>, String> {
    // Explicit pin list wins over the cross-product entirely.
    if let Some(combos) = combinations {
        let mut out: Vec<Vec<String>> = combos
            .iter()
            .map(|combo| {
                combo
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .collect();
        out.sort();
        out.dedup();
        return Ok(out);
    }

    // Dedup axes, preserving order.
    let axes: Vec<&String> = axes.iter().collect::<BTreeSet<_>>().into_iter().collect();

    // Implicit platform group: ≥2 platform selectors as axes → at most one
    // of them per combo.
    let mut all_groups: Vec<Vec<String>> = groups.to_vec();
    let platform_axes: Vec<String> = PLATFORM_SELECTORS
        .iter()
        .filter(|sel| axes.iter().any(|a| a.as_str() == **sel))
        .map(|s| s.to_string())
        .collect();
    if platform_axes.len() >= 2 {
        all_groups.push(platform_axes);
    }

    let group_of = |axis: &str| -> Option<usize> {
        all_groups.iter().position(|g| g.iter().any(|m| m == axis))
    };

    // Raw cross-product size, for the cap error message.
    let Some(raw) = 1usize.checked_shl(axes.len() as u32) else {
        return Err(format!(
            "define matrix expands to more than {max} combinations (cap: {max}) — \
             pin the combos to check with `define_combinations` in \
             .openplanet-lsp.toml or raise `define_matrix_max_combinations`"
        ));
    };

    let mut combos: Vec<Vec<String>> = Vec::new();
    for mask in 0..raw {
        let mut combo: Vec<String> = Vec::new();
        let mut used_groups: BTreeSet<usize> = BTreeSet::new();
        let mut violates = false;
        for (bit, axis) in axes.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                if let Some(g) = group_of(axis) {
                    if !used_groups.insert(g) {
                        // Two members of one mutually-exclusive group → skip.
                        violates = true;
                        break;
                    }
                }
                combo.push(axis.to_string());
            }
        }
        if !violates {
            combos.push(combo);
        }
    }
    combos.sort();

    if combos.len() > max {
        return Err(format!(
            "define matrix expands to {} combinations (cap: {max}) — \
             pin the combos to check with `define_combinations` in \
             .openplanet-lsp.toml or raise `define_matrix_max_combinations`",
            combos.len()
        ));
    }
    Ok(combos)
}

/// Plain-output prefix for one diagnostic: `[DEV] ` /
/// `[DEPENDENCY_MLHOOK, DEV] ` for a single combo; `[seen in: {} | DEV] `
/// when it fired in several combos (the base combo renders as `{}`).
/// No prefix when the run was single-combo (non-matrix or `--define`)
/// or the diagnostic fired only in the base combo.
pub fn combo_prefix(combos: &[Vec<String>]) -> String {
    match combos {
        [] => String::new(),
        [one] if one.is_empty() => String::new(),
        [one] => format!("[{}] ", one.join(", ")),
        many => {
            let rendered: Vec<String> = many
                .iter()
                .map(|c| {
                    if c.is_empty() {
                        "{}".to_string()
                    } else {
                        c.join(", ")
                    }
                })
                .collect();
            format!("[seen in: {}] ", rendered.join(" | "))
        }
    }
}

/// GH #18: plain/pretty-output suffix naming the target versions a
/// diagnostic fired on: ` [op 1.29.5]` / ` [op 1.29.5, 1.29.14]`. Empty
/// when the diagnostic carries no version label (single-target run, or it
/// fired on every target).
pub fn version_suffix(versions: &[String]) -> String {
    if versions.is_empty() {
        String::new()
    } else {
        format!(" [op {}]", versions.join(", "))
    }
}

/// Merge duplicate diagnostics across combos — and, GH #18, across target
/// versions: identical (path, line, col, severity, message) diagnostics
/// are reported ONCE with the combined combo and version lists.
/// First-seen order and first-seen Diagnostic payload win.
pub fn merge_combo_diagnostics(diags: Vec<CliDiagnostic>) -> Vec<CliDiagnostic> {
    #[derive(PartialEq, Eq, Hash)]
    struct Key {
        path: PathBuf,
        line: u32,
        col: u32,
        severity: String,
        message: String,
    }

    let mut out: Vec<CliDiagnostic> = Vec::new();
    let mut seen: HashMap<Key, usize> = HashMap::new();

    for diag in diags {
        let key = Key {
            path: diag.path.clone(),
            line: diag.diagnostic.range.start.line,
            col: diag.diagnostic.range.start.character,
            severity: format!("{:?}", diag.diagnostic.severity),
            message: diag.diagnostic.message.clone(),
        };
        match seen.get(&key) {
            Some(&idx) => {
                for combo in diag.combos {
                    if !out[idx].combos.contains(&combo) {
                        out[idx].combos.push(combo);
                    }
                }
                for version in diag.versions {
                    if !out[idx].versions.contains(&version) {
                        out[idx].versions.push(version);
                    }
                }
            }
            None => {
                seen.insert(key, out.len());
                out.push(diag);
            }
        }
    }
    // Stable, readable combo ordering in labels.
    for diag in &mut out {
        diag.combos.sort();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn combos_of(result: Result<Vec<Vec<String>>, String>) -> Vec<Vec<String>> {
        result.expect("expected combos, got error")
    }

    fn diag(
        path: &str,
        line: u32,
        col: u32,
        message: &str,
        combos: Vec<Vec<String>>,
    ) -> CliDiagnostic {
        use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
        CliDiagnostic {
            path: PathBuf::from(path),
            combos,
            versions: Vec::new(),
            diagnostic: Diagnostic {
                range: Range::new(Position::new(line, col), Position::new(line, col + 1)),
                severity: Some(DiagnosticSeverity::ERROR),
                message: message.to_string(),
                ..Default::default()
            },
        }
    }

    // ── define_matrix cross-product ─────────────────────────────────────

    #[test]
    fn no_axes_yields_single_base_combo() {
        let combos = combos_of(expand_combinations(
            &[],
            &[],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(combos, vec![Vec::<String>::new()]);
    }

    #[test]
    fn two_axes_cross_product_includes_base_combo() {
        let combos = combos_of(expand_combinations(
            &s(&["DEV", "DEPENDENCY_MLHOOK"]),
            &[],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(
            combos,
            vec![
                vec![],
                s(&["DEPENDENCY_MLHOOK"]),
                s(&["DEPENDENCY_MLHOOK", "DEV"]),
                s(&["DEV"]),
            ]
        );
    }

    #[test]
    fn duplicate_axes_collapse() {
        let combos = combos_of(expand_combinations(
            &s(&["DEV", "DEV"]),
            &[],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(combos, vec![vec![], s(&["DEV"])]);
    }

    // ── define_groups (explicit + implicit platform group) ──────────────

    #[test]
    fn explicit_group_skips_violating_combos() {
        let combos = combos_of(expand_combinations(
            &s(&["DEV", "A", "B"]),
            &[s(&["A", "B"])],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        // {A,B} together is skipped; everything else stays.
        assert_eq!(
            combos,
            vec![
                vec![],
                s(&["A"]),
                s(&["A", "DEV"]),
                s(&["B"]),
                s(&["B", "DEV"]),
                s(&["DEV"]),
            ]
        );
    }

    #[test]
    fn implicit_platform_group_when_two_selectors_are_axes() {
        let combos = combos_of(expand_combinations(
            &s(&["TMNEXT", "MP4"]),
            &[],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        // {TMNEXT, MP4} would be skipped by the implicit group.
        assert_eq!(combos, vec![vec![], s(&["MP4"]), s(&["TMNEXT"])]);
    }

    #[test]
    fn implicit_platform_group_with_three_selectors() {
        let combos = combos_of(expand_combinations(
            &s(&["TMNEXT", "MP4", "TURBO"]),
            &[],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(
            combos,
            vec![vec![], s(&["MP4"]), s(&["TMNEXT"]), s(&["TURBO"])]
        );
    }

    #[test]
    fn single_platform_selector_is_not_grouped() {
        let combos = combos_of(expand_combinations(
            &s(&["MP4", "DEV"]),
            &[],
            None,
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(
            combos,
            vec![vec![], s(&["DEV"]), s(&["DEV", "MP4"]), s(&["MP4"])]
        );
    }

    // ── define_combinations override ────────────────────────────────────

    #[test]
    fn explicit_combinations_skip_cross_product() {
        let combos = combos_of(expand_combinations(
            &s(&["DEV", "DEPENDENCY_MLHOOK"]),
            &[],
            Some(&[vec![], s(&["DEV"]), s(&["DEV", "DEPENDENCY_MLHOOK"])]),
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(
            combos,
            vec![vec![], s(&["DEPENDENCY_MLHOOK", "DEV"]), s(&["DEV"])]
        );
    }

    #[test]
    fn explicit_combinations_work_without_axes() {
        let combos = combos_of(expand_combinations(
            &[],
            &[],
            Some(&[s(&["DEV"])]),
            DEFAULT_MAX_COMBINATIONS,
        ));
        assert_eq!(combos, vec![s(&["DEV"])]);
    }

    // ── cap ─────────────────────────────────────────────────────────────

    #[test]
    fn cross_product_over_cap_is_a_hard_error() {
        let axes: Vec<String> = (0..5).map(|i| format!("AX{i}")).collect();
        let err = expand_combinations(&axes, &[], None, DEFAULT_MAX_COMBINATIONS)
            .expect_err("32 combos must exceed the default cap of 16");
        assert!(
            err.contains("32 combinations") && err.contains("cap: 16"),
            "error must name the combo count and cap: {err}"
        );
        assert!(
            err.contains("define_combinations"),
            "error must point at the escape hatch: {err}"
        );
    }

    #[test]
    fn cap_is_configurable() {
        let axes: Vec<String> = (0..5).map(|i| format!("AX{i}")).collect();
        let combos = combos_of(expand_combinations(&axes, &[], None, 32));
        assert_eq!(combos.len(), 32);
    }

    #[test]
    fn at_cap_is_allowed() {
        let axes: Vec<String> = (0..4).map(|i| format!("AX{i}")).collect();
        let combos = combos_of(expand_combinations(&axes, &[], None, 16));
        assert_eq!(combos.len(), 16);
    }

    // ── combo_prefix ────────────────────────────────────────────────────

    #[test]
    fn merge_reports_duplicate_once_with_combined_combos() {
        let merged = merge_combo_diagnostics(vec![
            diag("src/Main.as", 2, 3, "boom", vec![s(&["DEV"])]),
            diag("src/Main.as", 2, 3, "boom", vec![s(&["DEPENDENCY_MLHOOK"])]),
            diag("src/Other.as", 1, 1, "other", vec![s(&["DEV"])]),
        ]);
        assert_eq!(merged.len(), 2, "duplicate must collapse to one entry");
        assert_eq!(
            merged[0].combos,
            vec![s(&["DEPENDENCY_MLHOOK"]), s(&["DEV"])],
            "merged diagnostic lists every combo it fired in, sorted"
        );
        assert_eq!(merged[1].combos, vec![s(&["DEV"])]);
    }

    #[test]
    fn merge_keeps_base_combo_marker() {
        // A diagnostic firing in the base combo AND in DEV must show both
        // (base = empty combo → rendered as {} in the prefix).
        let merged = merge_combo_diagnostics(vec![
            diag("src/Main.as", 0, 1, "boom", vec![vec![]]),
            diag("src/Main.as", 0, 1, "boom", vec![s(&["DEV"])]),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].combos, vec![vec![], s(&["DEV"])]);
        assert_eq!(combo_prefix(&merged[0].combos), "[seen in: {} | DEV] ");
    }

    #[test]
    fn merge_distinct_positions_stay_separate() {
        let merged = merge_combo_diagnostics(vec![
            diag("src/Main.as", 2, 3, "boom", vec![s(&["DEV"])]),
            diag("src/Main.as", 4, 3, "boom", vec![s(&["DEV"])]),
            diag("src/Main.as", 2, 3, "other message", vec![s(&["DEV"])]),
        ]);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn merge_ignores_exact_duplicate_combo_entries() {
        let merged = merge_combo_diagnostics(vec![
            diag("src/Main.as", 2, 3, "boom", vec![s(&["DEV"])]),
            diag("src/Main.as", 2, 3, "boom", vec![s(&["DEV"])]),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].combos, vec![s(&["DEV"])]);
    }

    #[test]
    fn prefix_shapes() {
        // No matrix run (or no combo fired) → no prefix.
        assert_eq!(combo_prefix(&[]), "");
        // Base combo only → no prefix (issue: "empty base combo = no prefix").
        assert_eq!(combo_prefix(&[vec![]]), "");
        assert_eq!(combo_prefix(&[s(&["DEV"])]), "[DEV] ");
        assert_eq!(
            combo_prefix(&[s(&["DEPENDENCY_MLHOOK", "DEV"])]),
            "[DEPENDENCY_MLHOOK, DEV] "
        );
        assert_eq!(combo_prefix(&[vec![], s(&["DEV"])]), "[seen in: {} | DEV] ");
    }
}
