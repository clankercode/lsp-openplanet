//! GH35 exercises the public executable with authentic MP4 dumps.
use openplanet_lsp::testing::TempDir;
use serde_json::Value;
use std::fs;
use std::path::Path;
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
    assert!(
        output.status.success(),
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("successful command emits JSON")
}

#[test]
fn discovers_authentic_openplanet4_for_every_mp4_target() {
    for target in ["MP4", "MP40", "MP41"] {
        let root = TempDir::new(&format!("gh35-discover-{target}"));
        copy_mp4_snapshot(&root.join("Openplanet4"), "Openplanet4.json");
        let plugin = plugin(&root, MP4_SOURCE);
        let report = json_success(
            cli(&root)
                .args([
                    "check",
                    "--game-target",
                    target,
                    "--format",
                    "json",
                    "--unused",
                    "ignore",
                ])
                .arg(plugin)
                .output()
                .unwrap(),
        );
        assert_eq!(report["type_database_loaded"], true, "{report:#}");
        assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
    }
}

#[test]
fn explicit_legacy_mp4_install_remains_usable() {
    let root = TempDir::new("gh35-legacy-install");
    let install = root.join("custom-install");
    copy_mp4_snapshot(&install, "OpenplanetMP4.json");
    let plugin = plugin(&root, MP4_SOURCE);
    fs::write(
        plugin.join(".openplanet-lsp.toml"),
        format!("openplanet_dir = {:?}\n", install.to_str().unwrap()),
    )
    .unwrap();
    let report = json_success(
        cli(&root)
            .args([
                "check",
                "--game-target",
                "MP4",
                "--format",
                "json",
                "--unused",
                "ignore",
            ])
            .arg(plugin)
            .output()
            .unwrap(),
    );
    assert_eq!(report["type_database_loaded"], true, "{report:#}");
    assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
}

#[test]
fn typedb_operations_accept_authentic_mp4_and_preserve_the_trio() {
    let root = TempDir::new("gh35-typedb-operations");
    let snapshot = mp4_snapshot();
    let status = json_success(
        cli(&root)
            .args(["typedb", "status", "--json", "--from"])
            .arg(&snapshot)
            .output()
            .unwrap(),
    );
    assert!(
        status.to_string().contains("Openplanet4.json"),
        "{status:#}"
    );

    let diff = json_success(
        cli(&root)
            .args(["typedb", "diff"])
            .arg(&snapshot)
            .arg(&snapshot)
            .arg("--json")
            .output()
            .unwrap(),
    );
    assert_eq!(diff["summary"]["added"], 0, "{diff:#}");
    assert_eq!(diff["summary"]["removed"], 0, "{diff:#}");
    assert_eq!(diff["summary"]["changed"], 0, "{diff:#}");

    let destination = root.join("harvest");
    let refresh = |yes: bool| {
        let mut command = cli(&root);
        command
            .args(["typedb", "refresh", "--from"])
            .arg(&snapshot)
            .arg("--into")
            .arg(&destination);
        if yes {
            command.arg("--yes");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    refresh(false);
    assert!(!destination.exists(), "dry run writes no harvest");
    refresh(true);
    for name in ["OpenplanetCore.json", "Openplanet4.json", "Openplanet.h"] {
        assert_eq!(
            fs::read(destination.join(name)).unwrap(),
            fs::read(snapshot.join(name)).unwrap(),
            "refresh preserves {name} byte-for-byte"
        );
    }
    let plugin = plugin(&root, MP4_SOURCE);
    let report = json_success(
        cli(&root)
            .args(["check", "--game-target", "MP4", "--typedb-dir"])
            .arg(destination)
            .args(["--format", "json", "--unused", "ignore"])
            .arg(plugin)
            .output()
            .unwrap(),
    );
    assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
}

#[test]
fn recorded_mp4_snapshot_loads_without_an_install() {
    let root = TempDir::new("gh35-recorded-mp4");
    let plugin = plugin(&root, MP4_SOURCE);
    for target in ["MP4", "MP40", "MP41"] {
        let report = json_success(
            cli(&root)
                .args(["check", "--game-target", target, "--typedb-versions-dir"])
                .arg(mp4_snapshot().parent().unwrap())
                .args([
                    "--openplanet-version",
                    "1.29.14",
                    "--format",
                    "json",
                    "--unused",
                    "ignore",
                ])
                .arg(&plugin)
                .output()
                .unwrap(),
        );
        assert_eq!(report["type_database_loaded"], true, "{report:#}");
        assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
        assert_eq!(report["targets"], serde_json::json!(["1.29.14"]));
    }
}

#[test]
fn mixed_recorded_versions_cannot_substitute_next_for_mp4() {
    let root = TempDir::new("gh35-recorded-mixed");
    let versions = root.join("versions");
    let next = versions.join("1.29.5");
    fs::create_dir_all(&next).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-versions/1.29.5");
    for name in ["OpenplanetCore.json", "OpenplanetNext.json", "Openplanet.h"] {
        fs::copy(fixture.join(name), next.join(name)).unwrap();
    }
    copy_mp4_snapshot(&versions.join("1.29.14"), "Openplanet4.json");
    let plugin = plugin(&root, MP4_SOURCE);
    let run = |target: &str, requested: &str| {
        cli(&root)
            .args(["check", "--game-target", target, "--typedb-versions-dir"])
            .arg(&versions)
            .args([
                "--openplanet-version",
                requested,
                "--format",
                "json",
                "--unused",
                "ignore",
            ])
            .arg(&plugin)
            .output()
            .unwrap()
    };
    let good = json_success(run("MP4", "1.29.14"));
    assert_eq!(good["diagnostics"], serde_json::json!([]), "{good:#}");
    for (target, requested, unavailable) in [
        ("MP4", "1.29.5", "1.29.5"),
        ("MP4", "1.29.14,1.29.5", "1.29.5"),
        ("TMNEXT", "1.29.14", "1.29.14"),
        ("TMNEXT", "1.29.5,1.29.14", "1.29.14"),
    ] {
        let output = run(target, requested);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(
            stderr.contains("unknown Openplanet version")
                && stderr.contains(unavailable)
                && stderr.contains(target),
            "{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "no partial target result on failure"
        );
    }
}

#[test]
fn explicit_directory_rejects_mixed_core_variant_and_game_version() {
    for version in ["1.29.5", "1.29.14"] {
        let root = TempDir::new("gh35-mixed-pair");
        let install = root.join("typedb");
        copy_mp4_snapshot(&install, "Openplanet4.json");
        let next_core = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb-versions")
            .join(version)
            .join("OpenplanetCore.json");
        fs::copy(next_core, install.join("OpenplanetCore.json")).unwrap();
        let plugin = plugin(&root, MP4_SOURCE);
        let output = cli(&root)
            .args(["check", "--game-target", "MP4", "--typedb-dir"])
            .arg(&install)
            .args(["--format", "json", "--unused", "ignore"])
            .arg(plugin)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("OpenplanetCore.json") && stderr.contains("MP4"),
            "{stderr}"
        );
    }
}

#[test]
fn explicit_typedb_directory_respects_every_mp4_target() {
    for target in ["MP4", "MP40", "MP41"] {
        let root = TempDir::new(&format!("gh35-explicit-{target}"));
        let plugin = plugin(&root, MP4_SOURCE);
        let report = json_success(
            cli(&root)
                .args(["check", "--game-target", target, "--typedb-dir"])
                .arg(mp4_snapshot())
                .args(["--format", "json", "--unused", "ignore"])
                .arg(plugin)
                .output()
                .unwrap(),
        );
        assert_eq!(report["type_database_loaded"], true, "{report:#}");
        assert_eq!(report["diagnostics"], serde_json::json!([]), "{report:#}");
    }
}
