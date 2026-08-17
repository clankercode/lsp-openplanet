use std::fs;
use std::process::Command;

fn make_temp_plugin(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("openplanet-lsp-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("info.toml"),
        r#"
[meta]
name = "CLI Check Fixture"
version = "0.1.0"
"#,
    )
    .unwrap();
    root
}

#[test]
fn top_level_help_mentions_check_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("--help")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected --help to exit zero; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("USAGE:") && stdout.contains("check"),
        "expected top-level help to mention usage and check command; stdout={stdout:?}"
    );
}

#[test]
fn check_help_describes_dependency_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--help")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected check --help to exit zero; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("--plugins-dir") && stdout.contains("--plugin-files-search-path"),
        "expected check help to describe dependency options; stdout={stdout:?}"
    );
}

#[test]
fn check_command_reports_workspace_diagnostics() {
    let root = make_temp_plugin("workspace-diagnostics");
    fs::write(root.join("src/Foo.as"), "class Foo {}\n").unwrap();
    fs::write(
        root.join("src/Main.as"),
        "void Main() {\n  Foo@ ok;\n  MissingType@ bad;\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "expected diagnostics to produce a non-zero exit; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("src/Main.as:3:3: error: unknown type `MissingType`"),
        "expected diagnostic with relative path and location, got stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("unknown type `Foo`"),
        "expected check command to use plugin-wide workspace symbols, got stdout={stdout:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_command_exits_zero_when_clean() {
    let root = make_temp_plugin("clean");
    fs::write(
        root.join("src/Main.as"),
        "class Foo {}\nFoo@ MakeFoo() { return null; }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected clean plugin to exit zero; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("0 diagnostics"),
        "expected summary output, got stdout={stdout:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_command_accepts_relative_plugin_path() {
    let root = make_temp_plugin("relative-clean");
    fs::write(root.join("src/Main.as"), "class Foo {}\n").unwrap();
    let parent = root.parent().unwrap();
    let relative = root.file_name().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .current_dir(parent)
        .arg("check")
        .arg("--no-typedb")
        .arg(relative)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected relative fixture path to be accepted; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("0 diagnostics"),
        "expected summary output, got stdout={stdout:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_command_applies_manifest_defines_and_dependency_defines() {
    let root = make_temp_plugin("manifest-defines");
    fs::write(
        root.join("info.toml"),
        r#"
[meta]
name = "CLI Check Fixture"
version = "0.1.0"

[script]
optional_dependencies = ["Editor"]
defines = ["CUSTOM_DEF"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if !DEPENDENCY_EDITOR
MissingType should_fail_without_dependency_define;
#endif

#if !CUSTOM_DEF
MissingType should_fail_without_custom_define;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected manifest defines to suppress inactive branches; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("0 diagnostics"),
        "expected no diagnostics, got stdout={stdout:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_command_resolves_exports_via_plugin_files_search_path() {
    let base = std::env::temp_dir().join(format!(
        "openplanet-lsp-plugin-search-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("deps/dep-plugin/src")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();

    fs::write(
        base.join("deps/dep-plugin/info.toml"),
        r#"
[meta]
name = "Dependency Plugin"
version = "0.1.0"

[script]
module = "DepPlugin"
exports = ["Export.as"]
"#,
    )
    .unwrap();
    fs::write(
        base.join("deps/dep-plugin/src/Export.as"),
        r#"
namespace DepPlugin {
    import void Hello() from "DepPlugin";
}
"#,
    )
    .unwrap();

    fs::write(
        base.join("consumer/info.toml"),
        r#"
[meta]
name = "Consumer"
version = "0.1.0"

[script]
dependencies = ["DepPlugin"]
"#,
    )
    .unwrap();
    fs::write(
        base.join("consumer/src/Main.as"),
        "void Main() { DepPlugin::Hello(); }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("deps"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected src/ export fallback to resolve dependency exports; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("0 diagnostics"),
        "expected no diagnostics, got stdout={stdout:?}"
    );

    let _ = fs::remove_dir_all(base);
}

#[test]
fn check_command_reports_missing_required_dependency() {
    let base =
        std::env::temp_dir().join(format!("openplanet-lsp-missing-dep-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("plugins")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
dependencies = ["NoSuchDep"]
"#,
    )
    .unwrap();
    fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("plugins"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected missing required dep to fail check; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("NoSuchDep") || stderr.contains("NoSuchDep"),
        "expected missing dep name in output; stdout={stdout:?} stderr={stderr:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// Curated screenshot/CI fixture: must stay in the demo band (>=10 diags).
#[test]
fn check_command_showcase_diags_fixture_has_many_diagnostics() {
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/showcase-diags");
    let typedb = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--typedb-dir")
        .arg(&typedb)
        .arg(&fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Errors are expected — fixture is deliberately broken.
    assert!(
        !output.status.success(),
        "expected showcase-diags to fail check; stdout={stdout:?} stderr={stderr:?}"
    );

    // Summary line: "N diagnostics (...)" — require a demo-worthy floor.
    let count = stdout
        .lines()
        .rev()
        .find_map(|line| {
            let (num, rest) = line.trim().split_once(' ')?;
            if rest.starts_with("diagnostics") {
                num.parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    assert!(
        count >= 10,
        "expected >= 10 diagnostics on showcase-diags, got {count}; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("unknown type")
            || stdout.contains("undefined identifier")
            || stdout.contains("expects"),
        "expected demo-worthy diagnostic messages; stdout={stdout:?}"
    );
}

/// Minimal issue reproductions (tests/fixtures/issue-repros/<n>-<slug>).
/// Each fixture is a tiny standalone plugin; the test drives the real CLI.
/// A fixed issue asserts the clean outcome so a checker regression fails CI.
fn run_issue_repro(slug: &str) -> (bool, String, String) {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros")
        .join(slug);
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg(&fixture)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Same as run_issue_repro but with the shared typedb fixtures — required
/// for repros whose trigger involves engine API types (#38, #28).
fn run_issue_repro_typedb(slug: &str) -> (bool, String, String) {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros")
        .join(slug);
    let typedb = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--typedb-dir")
        .arg(&typedb)
        .arg(&fixture)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// GH #46: mixin body references a member its consuming class declares.
/// Fixed in 8c13ef5 — the fixture must stay clean.
#[test]
fn check_command_issue_repro_46_mixin_consumer_member_is_clean() {
    let (ok, stdout, stderr) = run_issue_repro("46-mixin-consumer-member");
    assert!(
        ok && stdout.contains("0 diagnostics"),
        "expected 0 diagnostics on issue-repro 46-mixin-consumer-member; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// GH #44: `@arr[0] = expr` handle-assign into an indexed Json::Value is not
/// an l-value — the game compiler rejects it and so must the LSP.
/// Game-compiler ground truth (matrix probe, 2026-08-17): the handle-assign
/// line errors, the value-copy counterpart compiles clean. Fixed on master.
#[test]
fn check_command_issue_repro_44_indexed_handle_assign_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("44-indexed-handle-assign");
    // The handle-assign into the Json index must be flagged (invalid LHS).
    assert!(
        stdout.contains("Main.as:11") && stdout.contains("invalid left-hand side"),
        "expected an invalid-assignment diagnostic at the @arr[0] line; stdout={stdout:?}"
    );
    // The legal value-copy counterpart must NOT be flagged.
    let legal_line_flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:17") || l.contains("Main.as:18"));
    assert!(
        !legal_line_flagged,
        "value-copy `arr[0] = tiny` must stay silent; stdout={stdout:?}"
    );
}

/// GH #30: bare ident in a class method must not resolve to a sibling
/// class's field via unqualified tail matching. Game-compiler ground truth
/// (scripts/issue_repro_game.py, 2026-08-17): `No matching symbol 'nod'`.
/// Already fixed on master — the fixture must keep flagging `nod`.
#[test]
fn check_command_issue_repro_30_sibling_field_bare_ident_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("30-sibling-field-bare-ident");
    let nod_diags: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("undefined identifier") && l.contains("`nod`"))
        .collect();
    assert_eq!(
        nod_diags.len(),
        1,
        "expected exactly 1 `undefined identifier `nod`` (ItemModel only; \
         ItemModelTreeElement's own-field use must stay silent); stdout={stdout:?}"
    );
}

/// GH #38: a workspace class named like an engine typedb type (`Status` vs
/// `Discord::Status`) must shadow it — the game compiles this clean.
/// Game-compiler ground truth (scripts/issue_repro_game.py, 2026-08-17):
/// fixture loads with 0 errors. Fixed on master — must stay clean.
#[test]
fn check_command_issue_repro_38_typedb_shadowed_class_is_clean() {
    let (ok, stdout, stderr) = run_issue_repro_typedb("38-typedb-shadowed-class");
    assert!(
        ok && stdout.contains("0 diagnostics"),
        "expected 0 diagnostics on issue-repro 38-typedb-shadowed-class; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// GH #28: `Draw::GetWidth`/`GetHeight` were removed from the Openplanet
/// API — the game compiler rejects them (`No matching symbol`) but the LSP
/// silently accepts unknown `Ns::Fn` calls. OPEN (proper fix is #18
/// version-ranged API rules). Un-ignore as part of the fix.
#[test]
#[ignore = "GH #28 open — removed-API FN, blocked on #18 version-ranged rules"]
fn check_command_issue_repro_28_removed_draw_api_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("28-removed-draw-api");
    assert!(
        stdout.contains("GetWidth") && stdout.contains("GetHeight"),
        "expected diagnostics naming Draw::GetWidth/GetHeight on issue-repro \
         28-removed-draw-api; stdout={stdout:?}"
    );
}

// ── GH #37: warning-parity batch ────────────────────────────────────────
// Game-compiler ground truth for every class captured via live RemoteBuild
// probe 2026-08-17 (see issue comments). These gates assert the game's
// diagnostics exist at the flagged constructs and that legal counterparts
// stay silent.

/// #37a: `i < u` → WARN Signed/Unsigned mismatch.
#[test]
fn check_command_issue_repro_37a_signed_unsigned_warns() {
    let (ok, stdout, _) = run_issue_repro("37a-signed-unsigned-mismatch");
    assert!(
        stdout.contains("Signed/Unsigned mismatch"),
        "expected Signed/Unsigned mismatch warning; ok={ok} stdout={stdout:?}"
    );
    // Warnings must not fail the check.
    assert!(ok, "warnings-only plugin must exit 0; stdout={stdout:?}");
    let warned_lines: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("Signed/Unsigned mismatch"))
        .collect();
    assert_eq!(
        warned_lines.len(),
        1,
        "exactly one signed/unsigned warning (both-signed and both-unsigned stay silent); stdout={stdout:?}"
    );
}

/// #37b: float→int implicit conversion warnings (both game wordings).
#[test]
fn check_command_issue_repro_37b_float_truncation_warns() {
    let (ok, stdout, _) = run_issue_repro("37b-float-truncation");
    assert!(
        stdout.contains("Implicit conversion of value is not exact"),
        "expected not-exact literal warning; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("Float value truncated in implicit conversion to integer"),
        "expected truncation warning; stdout={stdout:?}"
    );
    assert!(ok, "warnings-only plugin must exit 0; stdout={stdout:?}");
}

/// #37c: statement after `return` → WARN Unreachable code.
#[test]
fn check_command_issue_repro_37c_unreachable_code_warns() {
    let (ok, stdout, _) = run_issue_repro("37c-unreachable-code");
    let hits: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("Unreachable code"))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one Unreachable code warning; stdout={stdout:?}"
    );
    assert!(ok, "warnings-only plugin must exit 0; stdout={stdout:?}");
}

/// #37d: inner local hides outer → WARN Variable 'x' hides …
#[test]
fn check_command_issue_repro_37d_variable_shadow_warns() {
    let (ok, stdout, _) = run_issue_repro("37d-variable-shadow");
    let hits: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("hides another variable of same name in outer scope"))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one shadow warning (distinct-name decl stays silent); stdout={stdout:?}"
    );
    assert!(ok, "warnings-only plugin must exit 0; stdout={stdout:?}");
}

/// #37e: exact duplicate function → ERR (overloads stay silent).
#[test]
fn check_command_issue_repro_37e_duplicate_function_errors() {
    let (ok, stdout, _) = run_issue_repro("37e-duplicate-function");
    let hits: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("same name and parameters already exists"))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one duplicate-function error (overload silent); stdout={stdout:?}"
    );
    assert!(
        !ok,
        "error-level diagnostic must fail the check; stdout={stdout:?}"
    );
}

/// #37f: `uint u = -1;` → WARN Implicit conversion changed sign of value.
#[test]
fn check_command_issue_repro_37f_sign_change_warns() {
    let (ok, stdout, _) = run_issue_repro("37f-sign-change");
    let hits: Vec<_> = stdout
        .lines()
        .filter(|l| l.contains("Implicit conversion changed sign of value"))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one sign-change warning (positive literal silent); stdout={stdout:?}"
    );
    assert!(ok, "warnings-only plugin must exit 0; stdout={stdout:?}");
}
