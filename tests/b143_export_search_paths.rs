//! B143: `info.toml` export entries resolve across Max's `src/` <-> root moves,
//! and the search roots that make that work are user-configurable.
//!
//! Max's plugin dev norm shuffles sources between `<root>/src/` and `<root>/`.
//! An export entry written for one layout goes stale on the move, so
//! resolution tries, in order: the entry as written, the entry under each
//! search root, and the entry with a leading search root STRIPPED. Any deeper
//! path the entry carries is preserved throughout.
//!
//! Every test runs the real binary over a throwaway plugin and asserts on the
//! manifest diagnostic — the instrument is shown firing (the missing-file
//! control) before any test concludes anything from its silence.

use openplanet_lsp::testing::{TempDir, TempPath};
use std::fs;
use std::path::Path;
use std::process::Command;

const EXPORT_BODY: &str = "namespace Pub { void ApiCall() {} }\n";

/// Throwaway plugin root whose `[script]` lists `entry` under `kind`
/// (`exports` or `shared_exports`), with the export file written at
/// `on_disk` (root-relative). `extra_toml` is appended to
/// `.openplanet-lsp.toml` when non-empty; a `home/` sibling is used as `HOME`
/// so the developer's real user config can never change a result.
fn plugin(name: &str, kind: &str, entry: &str, on_disk: Option<&str>) -> TempPath {
    let base = TempDir::new(&format!("openplanet-lsp-b143-{name}"));
    let root = base.join("plugin");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(base.join("home")).unwrap();
    write(
        &root,
        "info.toml",
        &format!(
            "[meta]\nname = \"B143 Fixture\"\nversion = \"0.1.0\"\n\
             [script]\n{kind} = [\"{entry}\"]\n"
        ),
    );
    // Positive control: always present, always checked, never an export.
    write(&root, "src/Main.as", "void Main() {}\n");
    if let Some(rel) = on_disk {
        write(&root, rel, EXPORT_BODY);
    }
    base.into_child("plugin")
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
}

fn base_of(root: &Path) -> &Path {
    root.parent().unwrap()
}

fn write_workspace_config(root: &Path, contents: &str) {
    write(root, ".openplanet-lsp.toml", contents);
}

fn write_user_config(root: &Path, contents: &str) {
    write(
        base_of(root),
        "home/.config/openplanet-lsp/config.toml",
        contents,
    );
}

/// Run `check` over `root` and return stdout+stderr. Asserts the run actually
/// produced output for the positive control, so a silent verdict below can
/// never come from a binary that did nothing.
fn check(root: &Path, extra_args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--format")
        .arg("plain")
        .args(extra_args)
        .arg(root)
        .env("HOME", base_of(root).join("home"))
        .env_remove("USERPROFILE")
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("diagnostic"),
        "the run produced no summary line at all — the instrument is dead, \
         not the diagnostic: {combined:?}"
    );
    combined
}

/// The export entry resolved: no "not found" manifest error names it.
#[track_caller]
fn assert_resolved(out: &str, entry: &str) {
    assert!(
        !out.contains(&format!("not found: {entry}")),
        "expected `{entry}` to resolve; check output was:\n{out}"
    );
}

/// The export entry did NOT resolve.
#[track_caller]
fn assert_not_found(out: &str, entry: &str) {
    assert!(
        out.contains(&format!("not found: {entry}")),
        "expected a not-found error for `{entry}`; check output was:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// The layout matrix, for BOTH export kinds
// ---------------------------------------------------------------------------

/// Every combination of "entry as written" x "where the file actually is",
/// run for `exports` and `shared_exports` alike — they are separate loops in
/// the loader and must not be allowed to drift.
#[test]
fn every_entry_and_layout_combination_resolves_for_both_export_kinds() {
    // (case, entry, file on disk)
    let cases: &[(&str, &str, &str)] = &[
        ("no prefix, file at root", "Export.as", "Export.as"),
        ("no prefix, file under src", "Export.as", "src/Export.as"),
        (
            "src/ prefix, file under src",
            "src/Export.as",
            "src/Export.as",
        ),
        (
            "src/ prefix, file MOVED to root",
            "src/Export.as",
            "Export.as",
        ),
    ];
    for kind in ["exports", "shared_exports"] {
        for (i, (case, entry, on_disk)) in cases.iter().enumerate() {
            let root = plugin(&format!("matrix-{kind}-{i}"), kind, entry, Some(on_disk));
            let out = check(&root, &[]);
            assert!(
                !out.contains("not found"),
                "{kind}: {case}: entry `{entry}` with `{on_disk}` on disk \
                 must resolve; output:\n{out}"
            );
        }
    }
}

/// "Keeping their path if they have one": stripping the search root leaves the
/// rest of the entry intact, so a nested export still resolves to the nested
/// file rather than being flattened into the root.
#[test]
fn a_nested_entry_keeps_its_path_when_the_search_root_is_stripped() {
    for kind in ["exports", "shared_exports"] {
        let root = plugin(
            &format!("nested-{kind}"),
            kind,
            "src/Ex/Shared.as",
            Some("Ex/Shared.as"),
        );
        assert_resolved(&check(&root, &[]), "src/Ex/Shared.as");
        // The flattened path was never created, so a flattening bug could not
        // have resolved by accident.
        assert!(!root.join("Shared.as").exists());
    }
}

// ---------------------------------------------------------------------------
// The missing-file control: the instrument fires
// ---------------------------------------------------------------------------

/// An entry whose file exists in NEITHER place is still an error, for both
/// kinds. This is the positive control for every silent assertion above.
#[test]
fn an_entry_missing_everywhere_is_still_reported() {
    let root = plugin("missing-exports", "exports", "Export.as", None);
    assert_not_found(&check(&root, &[]), "Export.as");

    let root = plugin("missing-shared", "shared_exports", "Export.as", None);
    assert_not_found(&check(&root, &[]), "Export.as");

    // With a prefix, and with a search root that cannot rescue it.
    let root = plugin("missing-prefixed", "exports", "src/Export.as", None);
    assert_not_found(&check(&root, &[]), "src/Export.as");
}

// ---------------------------------------------------------------------------
// Configuration: workspace, user-global, CLI flag
// ---------------------------------------------------------------------------

/// A workspace `plugin_files_search_paths` REPLACES the built-in `["src"]`:
/// the configured root works in both directions, and `src` stops being
/// searched (which is what makes this an override rather than an append).
#[test]
fn a_workspace_config_search_path_replaces_the_default() {
    // Forward: bare entry, file under the configured root.
    let root = plugin(
        "cfg-fwd",
        "exports",
        "Exports.as",
        Some("Source/Exports.as"),
    );
    write_workspace_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    assert_resolved(&check(&root, &[]), "Exports.as");

    // Reverse: prefixed entry, file moved to the root.
    let root = plugin(
        "cfg-rev",
        "exports",
        "Source/Exports.as",
        Some("Exports.as"),
    );
    write_workspace_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    assert_resolved(&check(&root, &[]), "Source/Exports.as");

    // Replacement, not addition: with only `Source` configured, the built-in
    // `src` root no longer rescues a file that lives there.
    let root = plugin(
        "cfg-replaces",
        "exports",
        "Exports.as",
        Some("src/Exports.as"),
    );
    write_workspace_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    assert_not_found(&check(&root, &[]), "Exports.as");
}

/// `shared_exports` rides the same config, not just `exports`.
#[test]
fn a_config_search_path_applies_to_shared_exports_too() {
    let root = plugin(
        "cfg-shared",
        "shared_exports",
        "Source/Shared.as",
        Some("Shared.as"),
    );
    write_workspace_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    assert_resolved(&check(&root, &[]), "Source/Shared.as");
}

/// The user-global config carries the key, and a workspace config beats it —
/// the shared precedence ladder, same as `source_paths` / `honor_gitignore`.
#[test]
fn workspace_config_beats_the_user_global_one() {
    // User-global alone is enough.
    let root = plugin(
        "user-only",
        "exports",
        "Exports.as",
        Some("Source/Exports.as"),
    );
    write_user_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    assert_resolved(&check(&root, &[]), "Exports.as");

    // Workspace wins: it names a root that cannot resolve the entry, so the
    // user-global `Source` must NOT still be in force.
    let root = plugin(
        "ws-beats-user",
        "exports",
        "Exports.as",
        Some("Source/Exports.as"),
    );
    write_user_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    write_workspace_config(&root, "plugin_files_search_paths = [\"Elsewhere\"]\n");
    assert_not_found(&check(&root, &[]), "Exports.as");
}

/// `--plugin-files-search-path` is the top of the ladder: it beats a workspace
/// setting, a user-global setting, and the built-in default. Asserted in both
/// directions (flag rescues where config cannot, and flag overrides a config
/// that would otherwise have resolved).
#[test]
fn the_cli_flag_beats_config_and_the_default() {
    // Flag rescues where the workspace config's root cannot.
    let root = plugin(
        "flag-rescues",
        "exports",
        "Exports.as",
        Some("Source/Exports.as"),
    );
    write_workspace_config(&root, "plugin_files_search_paths = [\"Elsewhere\"]\n");
    assert_not_found(&check(&root, &[]), "Exports.as");
    assert_resolved(
        &check(&root, &["--plugin-files-search-path", "Source"]),
        "Exports.as",
    );

    // Flag overrides a workspace config that WOULD have resolved.
    let root = plugin(
        "flag-overrides",
        "exports",
        "Exports.as",
        Some("Source/Exports.as"),
    );
    write_workspace_config(&root, "plugin_files_search_paths = [\"Source\"]\n");
    assert_resolved(&check(&root, &[]), "Exports.as");
    assert_not_found(
        &check(&root, &["--plugin-files-search-path", "Elsewhere"]),
        "Exports.as",
    );

    // And over the built-in default, with no config file at all.
    let root = plugin(
        "flag-beats-default",
        "exports",
        "Exports.as",
        Some("src/Exports.as"),
    );
    assert_resolved(&check(&root, &[]), "Exports.as");
    assert_not_found(
        &check(&root, &["--plugin-files-search-path", "Elsewhere"]),
        "Exports.as",
    );
}

/// Several search roots, from config and from repeated flags, are all tried.
#[test]
fn multiple_search_roots_are_all_tried() {
    let root = plugin(
        "multi-cfg",
        "exports",
        "Exports.as",
        Some("second/Exports.as"),
    );
    write_workspace_config(
        &root,
        "plugin_files_search_paths = [\"first\", \"second\"]\n",
    );
    assert_resolved(&check(&root, &[]), "Exports.as");

    let root = plugin(
        "multi-flag",
        "exports",
        "second/Exports.as",
        Some("Exports.as"),
    );
    assert_resolved(
        &check(
            &root,
            &[
                "--plugin-files-search-path",
                "first",
                "--plugin-files-search-path",
                "second",
            ],
        ),
        "second/Exports.as",
    );
}

/// Negative control: a search root that matches nothing must not change a
/// resolution that already worked, in either direction. The control's premise
/// (the dead root really is absent) is asserted, not assumed.
#[test]
fn a_dead_search_root_changes_nothing() {
    let root = plugin("dead-fwd", "exports", "Exports.as", Some("src/Exports.as"));
    assert!(
        !root.join("nowhere").exists(),
        "control premise: dead root absent"
    );
    assert_resolved(&check(&root, &[]), "Exports.as");
    write_workspace_config(
        &root,
        "plugin_files_search_paths = [\"nowhere\", \"src\"]\n",
    );
    assert_resolved(&check(&root, &[]), "Exports.as");

    let root = plugin("dead-rev", "exports", "src/Exports.as", Some("Exports.as"));
    assert!(
        !root.join("nowhere").exists(),
        "control premise: dead root absent"
    );
    assert_resolved(&check(&root, &[]), "src/Exports.as");
    write_workspace_config(
        &root,
        "plugin_files_search_paths = [\"nowhere\", \"src\"]\n",
    );
    assert_resolved(&check(&root, &[]), "src/Exports.as");
}

// ---------------------------------------------------------------------------
// Discoverability
// ---------------------------------------------------------------------------

/// `check --help` names both the flag and the config key it now shares a
/// ladder with.
#[test]
fn check_help_documents_the_flag_and_the_config_key() {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--plugin-files-search-path"),
        "help must document the flag; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("plugin_files_search_paths"),
        "help must name the config key; stdout={stdout:?}"
    );
}
