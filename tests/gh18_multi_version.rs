//! GH #18: multi-version checking. Two tiny typedb snapshots that differ
//! by one function each; a plugin calling both sides plus a function in
//! neither. Checked against the pair, each version-specific miss is
//! labeled with the version(s) it fires on, and a miss common to every
//! target carries no label (it is not version news).

use openplanet_lsp::cli::{parse_check_args, run_check};
use openplanet_lsp::testing::TempDir;
use std::fs;
use std::path::Path;

/// Snapshot dir loadable by `TypeIndex::load`: `Ns` carries the given
/// functions (shape mirrors `src/typedb/index.rs` test dumps).
fn write_snapshot(root: &Path, dir: &str, version: &str, fns: &[&str]) {
    let d = root.join(dir);
    fs::create_dir_all(&d).unwrap();
    let functions = fns
        .iter()
        .map(|name| format!(r#"{{"ns":"Ns","name":"{name}","returntypedecl":"void","args":[]}}"#))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        d.join("OpenplanetCore.json"),
        format!(
            r#"{{"op":"{version} (next, Public, 00000000)","functions":[{functions}],"classes":[],"enums":[]}}"#
        ),
    )
    .unwrap();
    fs::write(
        d.join("OpenplanetNext.json"),
        format!(r#"{{"op":"{version}","mp":"2026-02-03 03:51:19","ns":{{}}}}"#),
    )
    .unwrap();
}

/// A plugin whose one source file calls `Ns::Old()` (v1-only),
/// `Ns::Gone()` (v2-only), and `Ns::Never()` (in neither snapshot).
fn write_plugin(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("info.toml"),
        r#"
[meta]
name = "GH18 Version Straddle"
version = "0.1.0"
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"void Main() {
    Ns::Old();
    Ns::Gone();
    Ns::Never();
}
"#,
    )
    .unwrap();
}

struct Setup {
    _guard: TempDir,
    plugin: std::path::PathBuf,
    versions_root: std::path::PathBuf,
    v1_dir: std::path::PathBuf,
}

fn setup(tag: &str) -> Setup {
    let base = TempDir::new(&format!("openplanet-lsp-gh18-{tag}"));
    let versions_root = base.join("typedb-versions");
    write_snapshot(&versions_root, "1.0.0", "1.0.0", &["Old"]);
    write_snapshot(&versions_root, "2.0.0", "2.0.0", &["Gone"]);
    let plugin = base.join("plugin");
    write_plugin(&plugin);
    Setup {
        v1_dir: versions_root.join("1.0.0"),
        plugin,
        versions_root,
        _guard: base,
    }
}

/// Diagnostics whose message names `symbol`, as (message, versions) pairs.
fn diags_for<'r>(
    report: &'r openplanet_lsp::cli::CheckReport,
    symbol: &str,
) -> Vec<(&'r str, &'r [String])> {
    report
        .diagnostics
        .iter()
        .filter(|d| d.diagnostic.message.contains(symbol))
        .map(|d| (d.diagnostic.message.as_str(), d.versions.as_slice()))
        .collect()
}

#[test]
fn multi_target_labels_version_specific_misses() {
    let s = setup("multi");
    let opts = parse_check_args(&[
        "--typedb-versions-dir".into(),
        s.versions_root.display().to_string(),
        "--openplanet-version".into(),
        "1.0.0,2.0.0".into(),
        s.plugin.display().to_string(),
    ])
    .unwrap();
    let report = run_check(&opts).unwrap();

    assert_eq!(report.targets, ["1.0.0", "2.0.0"], "report names its targets");

    // Ns::Old exists on 1.0.0, missing on 2.0.0 → labeled with 2.0.0 only.
    let old = diags_for(&report, "Ns::Old");
    assert_eq!(old.len(), 1, "one merged diagnostic for Ns::Old: {old:?}");
    assert_eq!(old[0].1, ["2.0.0"]);

    // Ns::Gone exists on 2.0.0, missing on 1.0.0 → labeled with 1.0.0 only.
    let gone = diags_for(&report, "Ns::Gone");
    assert_eq!(gone.len(), 1, "one merged diagnostic for Ns::Gone: {gone:?}");
    assert_eq!(gone[0].1, ["1.0.0"]);

    // Ns::Never is missing on every target → merged, and NOT labeled:
    // firing everywhere is not version news.
    let never = diags_for(&report, "Ns::Never");
    assert_eq!(never.len(), 1, "one merged diagnostic for Ns::Never: {never:?}");
    assert!(
        never[0].1.is_empty(),
        "all-targets diagnostic carries no version label: {never:?}"
    );
}

#[test]
fn single_target_run_has_no_version_labels() {
    let s = setup("single");
    let opts = parse_check_args(&[
        "--typedb-dir".into(),
        s.v1_dir.display().to_string(),
        s.plugin.display().to_string(),
    ])
    .unwrap();
    let report = run_check(&opts).unwrap();

    assert!(report.targets.is_empty(), "legacy single-target run: {:?}", report.targets);
    assert!(
        report.diagnostics.iter().all(|d| d.versions.is_empty()),
        "no version labels on a single-target run"
    );
    // And the v1 view itself: Gone + Never missing, Old fine.
    assert_eq!(diags_for(&report, "Ns::Gone").len(), 1);
    assert_eq!(diags_for(&report, "Ns::Old").len(), 0);
}

#[test]
fn unknown_requested_version_is_a_hard_error_naming_known() {
    let s = setup("unknown");
    let opts = parse_check_args(&[
        "--typedb-versions-dir".into(),
        s.versions_root.display().to_string(),
        "--openplanet-version".into(),
        "9.9.9".into(),
        s.plugin.display().to_string(),
    ])
    .unwrap();
    let err = run_check(&opts).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("9.9.9"), "names the request: {msg}");
    assert!(
        msg.contains("1.0.0") && msg.contains("2.0.0"),
        "names the known versions: {msg}"
    );
}
