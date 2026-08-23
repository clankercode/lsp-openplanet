//! CLI `typedb` command family (GH #59): `status`, `diff`, `refresh`.
//!
//! The pipeline is deliberately offline — every path here resolves DBs from a
//! local Openplanet install directory (`--from`), the auto-detect ladder, or an
//! explicit repo directory. Nothing fetches.
//!
//! Tests that must not see this machine's real install override `HOME` **and**
//! `USERPROFILE` (the two vars `LspConfig::auto_detect` probes) with an empty
//! temp dir, so the "no install" path is deterministic on a dev box as well as
//! in CI.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
}

fn fixture_typedb() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb")
}

/// Fresh empty temp dir, unique per `tag` (the pid alone repeats across the
/// tests in one binary).
fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "openplanet-lsp-typedb-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Temp dir holding a copy of both fixture DBs — stands in for an install.
fn tmp_install(tag: &str) -> PathBuf {
    let dir = tmp_dir(tag);
    for name in ["OpenplanetCore.json", "OpenplanetNext.json"] {
        fs::copy(fixture_typedb().join(name), dir.join(name)).unwrap();
    }
    dir
}

/// Run the binary with `HOME`/`USERPROFILE` pointed at an empty dir so
/// auto-detect finds no install.
fn bin_without_install(tag: &str) -> Command {
    let home = tmp_dir(&format!("home-{tag}"));
    let mut cmd = bin();
    cmd.env("HOME", &home).env("USERPROFILE", &home);
    cmd
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn run(mut cmd: Command, args: &[&str]) -> Run {
    let out = cmd.args(args).output().unwrap();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

// === status ===

/// `--from INSTALL_DIR` prints both resolved paths with their version stamps:
/// Core carries only `op`, the Nadeo dump carries `op` + `mp`.
#[test]
fn typedb_status_from_install_dir_prints_paths_and_versions() {
    let install = tmp_install("status-from");
    let r = run(
        bin_without_install("status-from"),
        &["typedb", "status", "--from", install.to_str().unwrap()],
    );
    assert_eq!(r.code, Some(0), "status should exit 0: {}", r.all());
    let out = &r.stdout;
    assert_eq!(
        out.matches("op=1.29.5").count(),
        2,
        "both DBs must report their op stamp: {out}"
    );
    assert!(
        out.contains("mp=2026-02-03"),
        "game dump must report its mp timestamp: {out}"
    );
    assert!(
        out.contains("OpenplanetCore.json") && out.contains("OpenplanetNext.json"),
        "both resolved paths must be shown: {out}"
    );
}

/// `--from=DIR` (equals form) is accepted like the space form.
#[test]
fn typedb_status_accepts_equals_form_of_from() {
    let install = tmp_install("status-eq");
    let r = run(
        bin_without_install("status-eq"),
        &[
            "typedb",
            "status",
            &format!("--from={}", install.to_str().unwrap()),
        ],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(r.stdout.contains("op=1.29.5"), "{}", r.stdout);
}

/// No install anywhere: a warning, not a failure — `check` degrades the same
/// way (`type database not loaded`).
#[test]
fn typedb_status_without_any_install_warns_and_exits_zero() {
    let r = run(bin_without_install("status-none"), &["typedb", "status"]);
    assert_eq!(
        r.code,
        Some(0),
        "missing install is a warning, not an error: {}",
        r.all()
    );
    let all = r.all();
    assert!(
        all.contains("type database not found"),
        "expected the not-found warning: {all}"
    );
    assert!(
        all.contains("--from"),
        "the warning must name the escape hatch: {all}"
    );
}

/// An explicit `--from` that does not hold the DBs is a usage failure, not a
/// silent warning — the user named a directory and it was wrong.
#[test]
fn typedb_status_explicit_from_without_dbs_exits_one() {
    let empty = tmp_dir("status-empty");
    let r = run(
        bin_without_install("status-empty"),
        &["typedb", "status", "--from", empty.to_str().unwrap()],
    );
    assert_eq!(r.code, Some(1), "{}", r.all());
    assert!(
        r.stderr.contains("OpenplanetCore.json"),
        "error must name what was missing: {}",
        r.stderr
    );
}

/// A resolved-but-corrupt DB exits 1 with an `error:` line — a stale or
/// truncated harvest must never read as "fine".
#[test]
fn typedb_status_corrupt_db_exits_one() {
    let install = tmp_install("status-corrupt");
    fs::write(install.join("OpenplanetCore.json"), "not json").unwrap();
    let r = run(
        bin_without_install("status-corrupt"),
        &["typedb", "status", "--from", install.to_str().unwrap()],
    );
    assert_eq!(r.code, Some(1), "{}", r.all());
    assert!(
        r.stderr.contains("error:") && r.stderr.contains("OpenplanetCore.json"),
        "expected a named parse error: {}",
        r.stderr
    );
}

/// A truncated DB (mid-array) is the realistic corrupt-harvest shape.
#[test]
fn typedb_status_truncated_db_exits_one() {
    let install = tmp_install("status-trunc");
    fs::write(
        install.join("OpenplanetNext.json"),
        r#"{"op":"1.29.5","ns":{"MwFoundations":{"CMwNod""#,
    )
    .unwrap();
    let r = run(
        bin_without_install("status-trunc"),
        &["typedb", "status", "--from", install.to_str().unwrap()],
    );
    assert_eq!(r.code, Some(1), "{}", r.all());
}

/// `--json` is the machine surface: resolved paths plus stamps and counts.
#[test]
fn typedb_status_json_reports_paths_stamps_and_counts() {
    let install = tmp_install("status-json");
    let r = run(
        bin_without_install("status-json"),
        &[
            "typedb",
            "status",
            "--from",
            install.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());
    let parsed: serde_json::Value = serde_json::from_str(&r.stdout).unwrap_or_else(|e| {
        panic!(
            "status --json must emit one JSON object ({e}): {}",
            r.stdout
        )
    });
    assert!(parsed["core"]["op"].as_str().unwrap().starts_with("1.29.5"));
    assert_eq!(parsed["game"]["op"], "1.29.5");
    assert_eq!(parsed["game"]["mp"], "2026-02-03 03:51:19");
    assert!(parsed["core"]["counts"]["functions"].as_u64().unwrap() > 700);
    assert!(parsed["game"]["counts"]["classes"].as_u64().unwrap() > 2000);
    assert!(parsed["core"]["path"]
        .as_str()
        .unwrap()
        .ends_with("OpenplanetCore.json"));
}

/// A `--from` dir holding a non-TMNEXT variant dump is harvested by filename,
/// so `--from` works for any install variant (Turbo/MP4/United) without a
/// `game_target` flag (per-variant defaults stay deferred — GH #59 I6).
#[test]
fn typedb_status_resolves_non_tmnext_variant_dump() {
    for variant in [
        "OpenplanetMP4.json",
        "OpenplanetTurbo.json",
        "OpenplanetUnited.json",
    ] {
        let tag = format!("status-{}", variant.trim_end_matches(".json"));
        let dir = tmp_dir(&tag);
        fs::copy(
            fixture_typedb().join("OpenplanetCore.json"),
            dir.join("OpenplanetCore.json"),
        )
        .unwrap();
        fs::copy(
            fixture_typedb().join("OpenplanetNext.json"),
            dir.join(variant),
        )
        .unwrap();
        let r = run(
            bin_without_install(&tag),
            &["typedb", "status", "--from", dir.to_str().unwrap()],
        );
        assert_eq!(r.code, Some(0), "{variant}: {}", r.all());
        assert!(
            r.stdout.contains(variant),
            "{variant} should be the resolved game DB: {}",
            r.stdout
        );
    }
}

// === command surface ===

#[test]
fn top_level_help_lists_typedb_command() {
    let r = run(bin(), &["--help"]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(
        r.stdout.contains("typedb"),
        "top-level help must advertise the typedb command: {}",
        r.stdout
    );
}

#[test]
fn typedb_help_lists_subcommands() {
    let r = run(bin(), &["typedb", "--help"]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    for sub in ["status", "diff", "refresh"] {
        assert!(
            r.stdout.contains(sub),
            "typedb --help must list `{sub}`: {}",
            r.stdout
        );
    }
}

#[test]
fn typedb_without_subcommand_is_a_usage_error() {
    let r = run(bin(), &["typedb"]);
    assert_eq!(r.code, Some(2), "{}", r.all());
    assert!(
        r.stderr.contains("status"),
        "usage error must point at the subcommands: {}",
        r.stderr
    );
}

#[test]
fn typedb_unknown_subcommand_is_a_usage_error() {
    let r = run(bin(), &["typedb", "fetch"]);
    assert_eq!(r.code, Some(2), "{}", r.all());
    assert!(
        r.stderr.contains("fetch"),
        "error must name the bad subcommand: {}",
        r.stderr
    );
}

#[test]
fn typedb_unknown_option_is_a_usage_error() {
    let r = run(bin(), &["typedb", "status", "--fetch-from-web"]);
    assert_eq!(r.code, Some(2), "{}", r.all());
}
