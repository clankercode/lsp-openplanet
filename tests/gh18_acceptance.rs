//! GH #18 acceptance, against the REAL recorded snapshots in
//! `tests/fixtures/typedb-versions/`: a plugin migrated for 1.29.14
//! (`Text::Join`) passes on that target, is flagged with "added in
//! Openplanet 1.29.14" on 1.29.5, is version-labeled when both are
//! targeted, and an unrecorded version is a hard error naming the known
//! ones. `--typedb-dir` pins the installed pair to the 1.29.5 snapshot so
//! the machine's real install never skews the catalog (hermeticity).

use openplanet_lsp::cli::{parse_check_args, run_check};
use std::path::PathBuf;
use tower_lsp::lsp_types::DiagnosticSeverity;

fn versions_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-versions")
}

fn plugin_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros/gh18-version-straddle")
}

fn check(
    version_flag: &str,
) -> Result<openplanet_lsp::cli::CheckReport, openplanet_lsp::cli::CliError> {
    let opts = parse_check_args(&[
        "--typedb-dir".into(),
        versions_root().join("1.29.5").display().to_string(),
        "--typedb-versions-dir".into(),
        versions_root().display().to_string(),
        "--openplanet-version".into(),
        version_flag.into(),
        plugin_root().display().to_string(),
    ])
    .unwrap();
    run_check(&opts)
}

fn errors(report: &openplanet_lsp::cli::CheckReport) -> Vec<(String, Vec<String>)> {
    report
        .diagnostics
        .iter()
        .filter(|d| d.diagnostic.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| (d.diagnostic.message.clone(), d.versions.clone()))
        .collect()
}

#[test]
fn migrated_code_passes_on_1_29_14() {
    let report = check("1.29.14").unwrap();
    assert_eq!(errors(&report), Vec::<(String, Vec<String>)>::new());
}

#[test]
fn migrated_code_flagged_with_added_in_on_1_29_5() {
    let report = check("1.29.5").unwrap();
    let errs = errors(&report);
    assert_eq!(errs.len(), 1, "exactly the Text::Join miss: {errs:?}");
    let (message, versions) = &errs[0];
    assert!(message.contains("Text::Join"), "{message}");
    assert!(
        message.contains("added in Openplanet 1.29.14")
            && message.contains("target is Openplanet 1.29.5"),
        "derived version note: {message}"
    );
    assert!(versions.is_empty(), "single-target run carries no labels");
}

#[test]
fn pair_of_targets_labels_the_miss_with_its_version() {
    let report = check("1.29.5,1.29.14").unwrap();
    assert_eq!(report.targets, ["1.29.5", "1.29.14"]);
    let errs = errors(&report);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].1, ["1.29.5"], "fires on the old target only");
}

#[test]
fn unrecorded_version_is_a_hard_error_naming_known_versions() {
    let err = check("9.9.9").unwrap_err().to_string();
    assert!(err.contains("9.9.9"), "{err}");
    assert!(err.contains("1.29.5") && err.contains("1.29.14"), "{err}");
}
