//! GH35 review regressions at the public executable boundary. Corrupt cases
//! modify disposable copies of recorded dumps; no API or history is invented.
use openplanet_lsp::testing::TempDir;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/gh35_mp4.rs"]
mod support;
use support::{copy_mp4_snapshot, mp4_snapshot, plugin, MP4_SOURCE};

fn cli(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE");
    command
}

fn json_success(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).expect("command emits JSON")
}

fn next_snapshot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-versions/1.29.14")
}

fn copy_next_snapshot(into: &Path) {
    fs::create_dir_all(into).unwrap();
    for name in ["OpenplanetCore.json", "OpenplanetNext.json", "Openplanet.h"] {
        fs::copy(next_snapshot().join(name), into.join(name)).unwrap();
    }
}

fn status(home: &Path, directory: &Path) -> Output {
    cli(home)
        .args(["typedb", "status", "--json", "--from"])
        .arg(directory)
        .output()
        .unwrap()
}

fn diff(home: &Path, old: &Path, new: &Path) -> Output {
    cli(home)
        .args(["typedb", "diff"])
        .arg(old)
        .arg(new)
        .arg("--json")
        .output()
        .unwrap()
}

fn refresh(home: &Path, from: &Path, into: &Path) -> Output {
    cli(home)
        .args(["typedb", "refresh", "--from"])
        .arg(from)
        .arg("--into")
        .arg(into)
        .arg("--yes")
        .output()
        .unwrap()
}

fn assert_no_drift(output: Output) {
    let report = json_success(output);
    for bucket in ["added", "removed", "changed"] {
        assert_eq!(report["summary"][bucket], 0, "{report:#}");
    }
}

fn replace_stamp(path: &Path, stamp: Option<&str>) {
    // Deliberately corrupt a disposable copy of authentic data, never a fixture.
    let mut dump: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    if let Some(stamp) = stamp {
        dump["op"] = Value::String(stamp.to_string());
    } else {
        dump.as_object_mut().unwrap().remove("op");
    }
    fs::write(path, serde_json::to_vec(&dump).unwrap()).unwrap();
}

fn check(
    home: &Path,
    plugin: &Path,
    target: &str,
    directory_flag: &str,
    directory: &Path,
    requested: Option<&str>,
) -> Output {
    let mut command = cli(home);
    command
        .args(["check", "--game-target", target, directory_flag])
        .arg(directory)
        .args(["--format", "json", "--unused", "ignore"]);
    if let Some(version) = requested {
        command.args(["--openplanet-version", version]);
    }
    command.arg(plugin).output().unwrap()
}

#[test]
fn typedb_mixed_directory_uses_core_variant_and_refresh_remains_usable() {
    for game_name in ["Openplanet4.json", "OpenplanetMP4.json"] {
        let root = TempDir::new("gh35-review-mixed");
        let mixed = root.join("mixed");
        copy_mp4_snapshot(&mixed, game_name);
        // Both game dumps claim the same numeric version. Only Core's real
        // build identity distinguishes the coherent pair.
        fs::copy(
            next_snapshot().join("OpenplanetNext.json"),
            mixed.join("OpenplanetNext.json"),
        )
        .unwrap();
        let report = json_success(status(&root, &mixed));
        assert!(
            report["game"]["path"]
                .as_str()
                .unwrap()
                .ends_with(game_name),
            "{report:#}"
        );
        assert_no_drift(diff(&root, &mp4_snapshot(), &mixed));

        let plugin = plugin(&root, MP4_SOURCE);
        for existing_next in [false, true] {
            let destination = root.join(if existing_next {
                "old-next-harvest"
            } else {
                "clean-harvest"
            });
            if existing_next {
                copy_next_snapshot(&destination);
            }
            let output = refresh(&root, &mixed, &destination);
            assert!(output.status.success(), "{output:?}");
            let report = json_success(status(&root, &destination));
            assert!(
                report["game"]["path"]
                    .as_str()
                    .unwrap()
                    .ends_with(game_name),
                "{report:#}"
            );
            assert_no_drift(diff(&root, &mp4_snapshot(), &destination));
            for name in ["OpenplanetCore.json", game_name, "Openplanet.h"] {
                assert_eq!(
                    fs::read(destination.join(name)).unwrap(),
                    fs::read(mixed.join(name)).unwrap(),
                    "harvest preserves {name}"
                );
            }
            let report = json_success(
                cli(&root)
                    .args(["check", "--game-target", "MP4", "--typedb-dir"])
                    .arg(&destination)
                    .args(["--format", "json", "--unused", "ignore"])
                    .arg(&plugin)
                    .output()
                    .unwrap(),
            );
            assert_eq!(report["type_database_loaded"], true, "{report:#}");
            assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
        }
    }
}

#[test]
fn typedb_bare_core_rejects_ambiguous_variants_before_refresh_writes() {
    let root = TempDir::new("gh35-review-bare-mixed");
    let mixed = root.join("corrupt-ambiguous-copy");
    copy_mp4_snapshot(&mixed, "Openplanet4.json");
    replace_stamp(&mixed.join("OpenplanetCore.json"), Some("1.29.14"));
    // A single filename still identifies a legacy bare-stamped pair.
    json_success(status(&root, &mixed));
    fs::copy(
        next_snapshot().join("OpenplanetNext.json"),
        mixed.join("OpenplanetNext.json"),
    )
    .unwrap();
    let destination = root.join("must-not-be-written");
    for output in [
        status(&root, &mixed),
        diff(&root, &mp4_snapshot(), &mixed),
        refresh(&root, &mixed, &destination),
    ] {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("ambiguous"),
            "{output:?}"
        );
    }
    assert!(!destination.exists());
}

#[test]
fn catalog_selected_variant_requires_its_game_dump_even_with_foreign_files() {
    for target in ["MP4", "MP40", "MP41", "TMNEXT"] {
        let root = TempDir::new("gh35-review-missing-selected");
        let versions = root.join("corrupt-missing-game-catalog");
        let snapshot = versions.join("1.29.14");
        let (selected_game, foreign_game, foreign_source) = if target == "TMNEXT" {
            copy_next_snapshot(&snapshot);
            ("OpenplanetNext.json", "Openplanet4.json", mp4_snapshot())
        } else {
            copy_mp4_snapshot(&snapshot, "Openplanet4.json");
            ("Openplanet4.json", "OpenplanetNext.json", next_snapshot())
        };
        // Deliberately remove the selected dump from a disposable recorded
        // snapshot. Adding a foreign file must not hide this same corruption.
        fs::remove_file(snapshot.join(selected_game)).unwrap();
        let plugin = plugin(&root, "void Main() {}\n");
        for foreign_present in [false, true] {
            if foreign_present {
                fs::copy(
                    foreign_source.join(foreign_game),
                    snapshot.join(foreign_game),
                )
                .unwrap();
            }
            for requested in [None, Some("1.29.14")] {
                let output = check(
                    &root,
                    &plugin,
                    target,
                    "--typedb-versions-dir",
                    &versions,
                    requested,
                );
                assert_eq!(
                    output.status.code(),
                    Some(2),
                    "target={target} foreign={foreign_present} request={requested:?}: {output:?}"
                );
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains(selected_game), "{stderr}");
                assert!(output.stdout.is_empty(), "no partial report: {output:?}");
            }
        }
    }
}

#[test]
fn catalog_excludes_coherent_foreign_snapshots_with_known_or_bare_core() {
    for target in ["MP4", "TMNEXT"] {
        for bare_core in [false, true] {
            let root = TempDir::new("gh35-review-coherent-foreign");
            let versions = root.join("versions");
            let snapshot = versions.join("1.29.14");
            if target == "TMNEXT" {
                copy_mp4_snapshot(&snapshot, "OpenplanetMP4.json");
            } else {
                copy_next_snapshot(&snapshot);
            }
            if bare_core {
                // Strip only the variant metadata on this disposable legacy
                // copy. Its real numeric version and API remain unchanged.
                replace_stamp(&snapshot.join("OpenplanetCore.json"), Some("1.29.14"));
            }
            let plugin = plugin(&root, "void Main() {}\n");
            let report = json_success(check(
                &root,
                &plugin,
                target,
                "--typedb-versions-dir",
                &versions,
                None,
            ));
            assert_eq!(report["type_database_loaded"], false, "{report:#}");
            let output = check(
                &root,
                &plugin,
                target,
                "--typedb-versions-dir",
                &versions,
                Some("1.29.14"),
            );
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("unknown Openplanet version") && stderr.contains(target),
                "{stderr}"
            );
        }
    }
}

fn assert_version_pair_rejected(home: &Path, snapshot: &Path, plugin: &Path) {
    // Exercise both the direct loaded-index validation and the recorded
    // catalog path, independently of Core's variant-mismatch check.
    for target in ["MP4", "MP40", "MP41"] {
        for (flag, directory, requested) in [
            ("--typedb-dir", snapshot, None),
            ("--typedb-versions-dir", snapshot.parent().unwrap(), None),
            (
                "--typedb-versions-dir",
                snapshot.parent().unwrap(),
                Some("1.29.14"),
            ),
        ] {
            let output = check(home, plugin, target, flag, directory, requested);
            assert_eq!(
                output.status.code(),
                Some(2),
                "target={target} flag={flag} request={requested:?}: {output:?}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("OpenplanetCore.json")
                    || stderr.contains("Openplanet4.json")
                    || stderr.contains("OpenplanetMP4.json"),
                "{stderr}"
            );
            assert!(output.stdout.is_empty(), "no partial report: {output:?}");
        }
    }
}

#[test]
fn version_pair_same_v4_variant_rejects_unequal_numeric_versions() {
    for game_name in ["Openplanet4.json", "OpenplanetMP4.json"] {
        let root = TempDir::new("gh35-review-unequal-versions");
        let snapshot = root.join("corrupt-version-pair/1.29.14");
        copy_mp4_snapshot(&snapshot, game_name);
        // Copy the older recorded stamp onto the real v4 game dump. This is
        // deliberate stamp corruption, not a claim about MP4's API history.
        replace_stamp(&snapshot.join(game_name), Some("1.29.5"));
        let plugin = plugin(&root, "void Main() {}\n");
        assert_version_pair_rejected(&root, &snapshot, &plugin);
    }
}

#[test]
fn version_pair_real_v4_core_rejects_malformed_or_missing_game_version() {
    for game_name in ["Openplanet4.json", "OpenplanetMP4.json"] {
        for stamp in [Some(""), Some("1.29"), None] {
            let root = TempDir::new("gh35-review-corrupt-game-stamp");
            let snapshot = root.join("corrupt-game-stamp/1.29.14");
            copy_mp4_snapshot(&snapshot, game_name);
            replace_stamp(&snapshot.join(game_name), stamp);
            let plugin = plugin(&root, "void Main() {}\n");
            assert_version_pair_rejected(&root, &snapshot, &plugin);
        }
    }
}

#[test]
fn version_pair_valid_game_rejects_malformed_or_missing_core_version() {
    for game_name in ["Openplanet4.json", "OpenplanetMP4.json"] {
        for stamp in [
            Some(""),
            Some("1.29"),
            Some("1.29.14 (v4, Public, 6fd4200f"),
            None,
        ] {
            let root = TempDir::new("gh35-review-corrupt-core-stamp");
            let snapshot = root.join("corrupt-core-stamp/1.29.14");
            copy_mp4_snapshot(&snapshot, game_name);
            replace_stamp(&snapshot.join("OpenplanetCore.json"), stamp);
            let plugin = plugin(&root, "void Main() {}\n");
            assert_version_pair_rejected(&root, &snapshot, &plugin);
        }
    }
}

#[test]
fn version_pair_accepts_matching_numeric_full_and_bare_stamps() {
    for game_name in ["Openplanet4.json", "OpenplanetMP4.json"] {
        for bare_core in [false, true] {
            let root = TempDir::new("gh35-review-valid-stamps");
            let snapshot = root.join("versions/1.29.14");
            copy_mp4_snapshot(&snapshot, game_name);
            if bare_core {
                replace_stamp(&snapshot.join("OpenplanetCore.json"), Some("1.29.14"));
            }
            let plugin = plugin(&root, MP4_SOURCE);
            for (flag, directory, requested) in [
                ("--typedb-dir", snapshot.as_path(), None),
                (
                    "--typedb-versions-dir",
                    snapshot.parent().unwrap(),
                    Some("1.29.14"),
                ),
            ] {
                let report = json_success(check(&root, &plugin, "MP4", flag, directory, requested));
                assert_eq!(report["type_database_loaded"], true, "{report:#}");
                assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
            }
        }
    }
}
