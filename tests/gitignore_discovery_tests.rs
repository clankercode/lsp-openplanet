//! GH #60: `.as` discovery honors `.gitignore` (config `honor_gitignore`,
//! default enabled; CLI `--no-gitignore` opts out).
//!
//! Every test runs the real binary over a throwaway plugin tree and asserts on
//! which files appear in the diagnostic output — a file that was discovered
//! reports its unknown-type error, one that was excluded reports nothing.
//! `HOME` is redirected into the fixture so the developer's real user-global
//! `~/.config/openplanet-lsp/config.toml` can never change a result.

use openplanet_lsp::testing::{TempDir, TempPath};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Throwaway plugin root: `info.toml` + `src/Main.as` (the positive control,
/// always discovered) + a `home/` dir used as `HOME` for the child process.
fn make_temp_plugin(name: &str) -> TempPath {
    let base = TempDir::new(&format!("openplanet-lsp-gitignore-{name}"));
    let root = base.join("plugin");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(base.join("home")).unwrap();
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Gitignore Fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    write_source(&root, "src/Main.as", "MainMissing");
    base.into_child("plugin")
}

/// The dir holding the plugin root; also holds the fake `HOME`.
fn base_of(root: &Path) -> &Path {
    root.parent().unwrap()
}

/// Write a `.as` file whose only diagnostic is `unknown type <marker>`, so the
/// file's presence in the checked set is visible in `check` output by path.
fn write_source(root: &Path, rel: &str, marker: &str) {
    write_file(
        root,
        rel,
        &format!("void F_{marker}() {{\n  {marker}@ bad;\n}}\n"),
    );
}

fn write_file(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
}

/// Run `check --no-typedb --format plain` over `root` with `HOME` redirected
/// into the fixture, and return the root-relative paths of every `.as` file
/// that produced a diagnostic — i.e. the discovered-and-checked set.
fn checked_paths(root: &Path, extra_args: &[&str]) -> Vec<String> {
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Plain format is `<path>:<line>:<col>: <severity>: <message>`.
    let mut paths: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.split_once(".as:").map(|(p, _)| format!("{p}.as")))
        .collect();
    paths.sort();
    paths.dedup();
    assert!(
        paths.iter().any(|p| p == "src/Main.as"),
        "positive control missing: src/Main.as must always be checked; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    paths
}

/// Assert `rel` produced no diagnostics (it was excluded from discovery).
#[track_caller]
fn assert_excluded(paths: &[String], rel: &str) {
    assert!(
        !paths.iter().any(|p| p == rel),
        "expected {rel} to be excluded from discovery; checked={paths:?}"
    );
}

/// Assert `rel` produced diagnostics (it was part of the checked set).
#[track_caller]
fn assert_checked(paths: &[String], rel: &str) {
    assert!(
        paths.iter().any(|p| p == rel),
        "expected {rel} to be checked; checked={paths:?}"
    );
}

// ---------------------------------------------------------------------------
// Default-on exclusion
// ---------------------------------------------------------------------------

/// The GH #60 repro: a `.gitignore`d directory is not discovered.
#[test]
fn gitignored_directory_is_not_checked_by_default() {
    let root = make_temp_plugin("dir-pattern");
    write_file(&root, ".gitignore", "generated/\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_excluded(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// Single-file pattern variant (`Gen.as`, no trailing slash, no directory).
#[test]
fn gitignored_single_file_pattern_is_not_checked() {
    let root = make_temp_plugin("file-pattern");
    write_file(&root, ".gitignore", "Gen.as\n");
    write_source(&root, "generated/Gen.as", "GenMissing");
    write_source(&root, "generated/Keep.as", "KeepMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "generated/Gen.as");
    assert_checked(&paths, "generated/Keep.as");
}

/// Glob pattern variant (`*.gen.as`).
#[test]
fn gitignored_glob_pattern_is_not_checked() {
    let root = make_temp_plugin("glob-pattern");
    write_file(&root, ".gitignore", "*.gen.as\n");
    write_source(&root, "src/Table.gen.as", "TableGenMissing");
    write_source(&root, "src/Table.as", "TableMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "src/Table.gen.as");
    assert_checked(&paths, "src/Table.as");
}

/// Rooted pattern variant: `/generated/` anchors at the plugin root, so a
/// same-named dir deeper in the tree is untouched.
#[test]
fn rooted_gitignore_pattern_only_matches_at_root() {
    let root = make_temp_plugin("rooted-pattern");
    write_file(&root, ".gitignore", "/generated/\n");
    write_source(&root, "generated/Gen.as", "TopGenMissing");
    write_source(&root, "src/generated/Gen.as", "NestedGenMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "generated/Gen.as");
    assert_checked(&paths, "src/generated/Gen.as");
}

/// Nested per-directory `.gitignore`: patterns are relative to their own dir.
#[test]
fn nested_gitignore_applies_relative_to_its_own_directory() {
    let root = make_temp_plugin("nested-gitignore");
    write_file(&root, "src/build/.gitignore", "Out.as\n");
    write_source(&root, "src/build/Out.as", "OutMissing");
    write_source(&root, "src/Out.as", "SrcOutMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "src/build/Out.as");
    assert_checked(&paths, "src/Out.as");
}

/// Negation, working form: `generated/*` leaves the directory itself
/// traversable, so `!generated/Keep.as` re-includes that one file.
#[test]
fn negation_reincludes_file_when_directory_is_still_traversed() {
    let root = make_temp_plugin("negation-works");
    write_file(&root, ".gitignore", "generated/*\n!generated/Keep.as\n");
    write_source(&root, "generated/Gen.as", "GenMissing");
    write_source(&root, "generated/Keep.as", "KeepMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "generated/Gen.as");
    assert_checked(&paths, "generated/Keep.as");
}

/// Negation, git's no-op form: an excluded DIRECTORY (`generated/`) is never
/// descended into, so `!generated/Keep.as` cannot resurrect anything. This
/// matches `git check-ignore`; asserted so the behavior stays deliberate.
#[test]
fn negation_cannot_reinclude_inside_an_excluded_directory() {
    let root = make_temp_plugin("negation-noop");
    write_file(&root, ".gitignore", "generated/\n!generated/Keep.as\n");
    write_source(&root, "generated/Gen.as", "GenMissing");
    write_source(&root, "generated/Keep.as", "KeepMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "generated/Gen.as");
    assert_excluded(&paths, "generated/Keep.as");
}

/// A gitignored subtree is excluded wholesale, including files several levels
/// below the ignored directory (the real-world `.worktrees/` case: a git
/// worktree holding a second copy of the plugin's own sources).
#[test]
fn gitignored_subtree_is_excluded_at_every_depth() {
    let root = make_temp_plugin("subtree");
    write_file(&root, ".gitignore", ".worktrees/\n");
    write_source(&root, ".worktrees/feature/src/Main.as", "WorktreeMissing");
    write_source(&root, ".worktrees/feature/src/deep/More.as", "DeepMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, ".worktrees/feature/src/Main.as");
    assert_excluded(&paths, ".worktrees/feature/src/deep/More.as");
}

// ---------------------------------------------------------------------------
// Unchanged behavior (no surprise exclusions)
// ---------------------------------------------------------------------------

/// No `.gitignore` anywhere → identical to pre-GH-#60 behavior.
#[test]
fn no_gitignore_present_checks_everything() {
    let root = make_temp_plugin("no-gitignore");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// A `.gitignore` ABOVE the plugin root must not apply: plugin roots often
/// live inside a bigger repo whose rules say nothing about the plugin's
/// checked set (B015 decision 7).
#[test]
fn gitignore_above_the_plugin_root_is_not_consulted() {
    let root = make_temp_plugin("parent-gitignore");
    write_file(base_of(&root), ".gitignore", "generated/\nsrc/\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    let paths = checked_paths(&root, &[]);
    assert_checked(&paths, "generated/Gen.as");
}

/// Only gitignore sources count: a `.ignore` file (ripgrep/fd convention) is
/// NOT a `.gitignore` and must not silently shrink the checked set.
#[test]
fn dot_ignore_file_is_not_honored() {
    let root = make_temp_plugin("dot-ignore");
    write_file(&root, ".ignore", "generated/\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// Hidden directories are still walked (today's behavior); only a `.gitignore`
/// rule excludes them.
#[test]
fn hidden_directories_are_still_walked() {
    let root = make_temp_plugin("hidden-dirs");
    write_file(&root, ".gitignore", "generated/\n");
    write_source(&root, ".hidden/Hidden.as", "HiddenMissing");

    assert_checked(&checked_paths(&root, &[]), ".hidden/Hidden.as");
}

/// `.git` internals are never part of a plugin's source set.
#[test]
fn git_directory_contents_are_never_checked() {
    let root = make_temp_plugin("git-dir");
    write_file(&root, ".gitignore", "generated/\n");
    write_source(&root, ".git/hooks/Hook.as", "HookMissing");

    assert_excluded(&checked_paths(&root, &[]), ".git/hooks/Hook.as");
}

/// A root-local `.git/info/exclude` is a gitignore source (same repo, same
/// root — no walking above the plugin root involved).
#[test]
fn git_info_exclude_is_honored() {
    let root = make_temp_plugin("info-exclude");
    write_file(&root, ".git/info/exclude", "generated/\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_excluded(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// Symlinked source dirs inside a plugin root are still walked (the dips++
/// symlink-farm layout); the pre-GH-#60 walk followed them via `is_dir()`.
#[cfg(unix)]
#[test]
fn symlinked_source_directory_is_still_discovered() {
    let root = make_temp_plugin("symlinked-src");
    write_file(&root, ".gitignore", "generated/\n");
    let real = base_of(&root).join("elsewhere");
    fs::create_dir_all(&real).unwrap();
    fs::write(
        real.join("Linked.as"),
        "void F_LinkedMissing() {\n  LinkedMissing@ bad;\n}\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&real, root.join("linked")).unwrap();

    assert_checked(&checked_paths(&root, &[]), "linked/Linked.as");
}

// ---------------------------------------------------------------------------
// Precedence ladder: CLI flag > workspace config > user-global config
// ---------------------------------------------------------------------------

/// `honor_gitignore = false` in the workspace config restores pre-GH-#60
/// behavior (the compile-critical-but-gitignored codegen escape hatch).
#[test]
fn workspace_config_false_restores_old_behavior() {
    let root = make_temp_plugin("ws-false");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(&root, ".openplanet-lsp.toml", "honor_gitignore = false\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// `honor_gitignore = true` is also accepted explicitly (same as default).
#[test]
fn workspace_config_true_matches_the_default() {
    let root = make_temp_plugin("ws-true");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(&root, ".openplanet-lsp.toml", "honor_gitignore = true\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_excluded(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// User-global config applies when the workspace config says nothing.
#[test]
fn user_global_false_applies_without_a_workspace_config() {
    let root = make_temp_plugin("user-false");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(
        base_of(&root),
        "home/.config/openplanet-lsp/config.toml",
        "honor_gitignore = false\n",
    );
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// Workspace `false` beats user-global `true` (same rule as
/// `warnings_as_errors`: the repo-local file wins).
#[test]
fn workspace_false_beats_user_global_true() {
    let root = make_temp_plugin("ws-false-user-true");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(&root, ".openplanet-lsp.toml", "honor_gitignore = false\n");
    write_file(
        base_of(&root),
        "home/.config/openplanet-lsp/config.toml",
        "honor_gitignore = true\n",
    );
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// Workspace `true` beats user-global `false` (the other direction).
#[test]
fn workspace_true_beats_user_global_false() {
    let root = make_temp_plugin("ws-true-user-false");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(&root, ".openplanet-lsp.toml", "honor_gitignore = true\n");
    write_file(
        base_of(&root),
        "home/.config/openplanet-lsp/config.toml",
        "honor_gitignore = false\n",
    );
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_excluded(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// `--no-gitignore` beats a workspace `honor_gitignore = true`.
#[test]
fn cli_no_gitignore_beats_workspace_true() {
    let root = make_temp_plugin("cli-beats-ws");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(&root, ".openplanet-lsp.toml", "honor_gitignore = true\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(
        &checked_paths(&root, &["--no-gitignore"]),
        "generated/Gen.as",
    );
}

/// `--no-gitignore` with no config at all still disables honoring.
#[test]
fn cli_no_gitignore_beats_the_default() {
    let root = make_temp_plugin("cli-beats-default");
    write_file(&root, ".gitignore", "generated/\n");
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_checked(
        &checked_paths(&root, &["--no-gitignore"]),
        "generated/Gen.as",
    );
}

/// `check --help` documents the flag.
#[test]
fn check_help_documents_no_gitignore() {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--no-gitignore"),
        "expected check help to document --no-gitignore; stdout={stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// Composition with source_paths / ignore_paths (B015 decision 6)
// ---------------------------------------------------------------------------

/// A gitignored file living under a `source_paths` ALLOWLIST entry stays
/// excluded: the allowlist narrows the checked set, it never resurrects a
/// gitignored file.
#[test]
fn source_paths_allowlist_does_not_resurrect_a_gitignored_file() {
    let root = make_temp_plugin("allowlist-vs-gitignore");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(
        &root,
        ".openplanet-lsp.toml",
        "source_paths = [\"src\", \"generated\"]\n",
    );
    write_source(&root, "generated/Gen.as", "GenMissing");

    assert_excluded(&checked_paths(&root, &[]), "generated/Gen.as");
}

/// `ignore_paths` and `.gitignore` are independent blocklists: a file excluded
/// by either one is out (final set = discovered − gitignored − ignore_paths).
#[test]
fn ignore_paths_and_gitignore_both_apply() {
    let root = make_temp_plugin("blocklist-union");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(
        &root,
        ".openplanet-lsp.toml",
        "ignore_paths = [\"vendor\"]\n",
    );
    write_source(&root, "generated/Gen.as", "GenMissing");
    write_source(&root, "vendor/Vendor.as", "VendorMissing");
    write_source(&root, "extra/Extra.as", "ExtraMissing");

    let paths = checked_paths(&root, &[]);
    assert_excluded(&paths, "generated/Gen.as");
    assert_excluded(&paths, "vendor/Vendor.as");
    assert_checked(&paths, "extra/Extra.as");
}

/// `--no-gitignore` does not disturb `ignore_paths`.
#[test]
fn no_gitignore_leaves_ignore_paths_in_force() {
    let root = make_temp_plugin("no-gitignore-keeps-ignore-paths");
    write_file(&root, ".gitignore", "generated/\n");
    write_file(
        &root,
        ".openplanet-lsp.toml",
        "ignore_paths = [\"vendor\"]\n",
    );
    write_source(&root, "generated/Gen.as", "GenMissing");
    write_source(&root, "vendor/Vendor.as", "VendorMissing");

    let paths = checked_paths(&root, &["--no-gitignore"]);
    assert_checked(&paths, "generated/Gen.as");
    assert_excluded(&paths, "vendor/Vendor.as");
}
