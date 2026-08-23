use std::fs;
use std::process::Command;

/// Trailer `check` must print after diagnostics: invites reports of
/// Openplanet-vs-openplanet-lsp diagnostic mismatches (see main.rs ISSUE_URL).
const ISSUE_ASK_TRAILER: &str = "https://github.com/clankercode/lsp-openplanet/issues";

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
        "class Foo {}\nFoo@ MakeFoo() { return null; }\nvoid Main() { Foo@ f = MakeFoo(); }\n",
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
    fs::write(
        root.join("src/Main.as"),
        "class Foo {}\nvoid Main() { Foo@ f = Foo(); }\n",
    )
    .unwrap();
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

    // The mismatch-report ask must ride along whenever diagnostics print.
    assert!(
        stdout.contains(ISSUE_ASK_TRAILER),
        "expected the Openplanet-mismatch report ask on check output; stdout={stdout:?}"
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
    // The legal value-copy counterpart (line 16) must NOT be flagged.
    // (A prior version of this gate checked lines 17–18 — those are the
    // closing braces, so the assertion was dead.)
    let legal_line_flagged = stdout.lines().any(|l| l.contains("Main.as:16"));
    assert!(
        !legal_line_flagged,
        "value-copy `arr[0] = tiny` (Main.as:16) must stay silent; stdout={stdout:?}"
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

/// GH #26: `unknown type MLFeed::PlayerCpInfo` (qualified name whose leading
/// segment is not a Core/Nadeo engine namespace) should carry a note hinting
/// the cross-plugin export cause: dependency missing / not in --plugins-dir /
/// exports failed to load. Engine-namespace typos (`Math::NotARealThing`)
/// must stay plain errors with no note.
#[test]
fn check_command_issue_repro_26_export_ns_hint() {
    let (_, stdout, _) = run_issue_repro_typedb("26-export-ns-hint");
    let mlfeed_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("unknown type `MLFeed::PlayerCpInfo`"))
        .collect();
    assert_eq!(
        mlfeed_lines.len(),
        1,
        "expected exactly one MLFeed unknown-type diagnostic; stdout={stdout:?}"
    );
    assert!(
        mlfeed_lines[0].contains("note: "),
        "MLFeed::… diagnostic must carry the plugin-export note; stdout={stdout:?}"
    );
    let math_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("unknown type `Math::NotARealThing`"))
        .collect();
    assert_eq!(
        math_lines.len(),
        1,
        "expected exactly one Math unknown-type diagnostic; stdout={stdout:?}"
    );
    assert!(
        math_lines.iter().all(|l| !l.contains("plugin export")),
        "engine-namespace typo must NOT carry the plugin-export note; stdout={stdout:?}"
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

/// GH #28 (+ #27): the game compiler rejects unknown `Ns::Fn` free calls —
/// `No matching symbol 'Draw::GetWidth'` (namespace absent from typedb on
/// 1.29.5) and `No matching symbol 'UI::GetIO'` (member missing from a
/// KNOWN namespace). The LSP used to stay silent on both. Fixed — must
/// keep flagging. Legal counterparts: `Display::GetWidth()` and `UI::Begin`
/// stay silent, and unknown *user* namespaces (`MyHelpers::Foo()`) stay
/// quiet too (no typedb evidence → no diagnostic).
#[test]
fn check_command_issue_repro_28_removed_draw_api_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("28-removed-draw-api");
    let flagged_draw = stdout
        .lines()
        .any(|l| l.contains("Main.as:20") && l.contains("GetWidth") && l.contains("error"));
    let flagged_getheight = stdout
        .lines()
        .any(|l| l.contains("Main.as:21") && l.contains("GetHeight") && l.contains("error"));
    let flagged_getio = stdout
        .lines()
        .any(|l| l.contains("Main.as:27") && l.contains("GetIO") && l.contains("error"));
    assert!(
        flagged_draw && flagged_getheight && flagged_getio,
        "expected No-matching-symbol errors on Draw::GetWidth/GetHeight and UI::GetIO; stdout={stdout:?}"
    );
    // Relocation hint (GH #27): suggest the Display:: replacement for Draw::*.
    let hinted = stdout
        .lines()
        .any(|l| l.contains("Main.as:20") && l.contains("Display::GetWidth"));
    assert!(
        hinted,
        "expected a relocation hint naming Display::GetWidth; stdout={stdout:?}"
    );
}

/// GH #28 negative guard: unknown USER namespaces stay silent — the checker
/// can't distinguish a plugin's own helper namespace from a typedb gap, and
/// the game only rejects calls into *its* unknown API namespaces. Legal
/// relocated (`Display::GetWidth`) and known-member (`UI::Begin`) calls
/// stay clean too.
#[test]
fn check_command_issue_repro_28b_user_namespace_stays_silent() {
    let (_, stdout, _) = run_issue_repro_typedb("28b-user-namespace-silent");
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:1") && l.contains("error"));
    assert!(
        !flagged,
        "user-namespace calls and legal API calls must stay silent; stdout={stdout:?}"
    );
}

/// Get-only virtual property writes (todo-cover-cases-next.txt,
/// spike-live-add-kinematic-ao): `iso4.tx` is `get_tx()` only — the game
/// rejects `loc.tx += 8.0` and `loc.tx = 3.0` with
/// `The property has no set accessor` (and, for compound assigns,
/// `Compound assignments with property accessors require both get and set
/// accessors`). Reads and whole-struct assigns stay legal. Game-compiler
/// ground truth: Tools.as (242,16), OP 1.29.5.
#[test]
fn check_command_issue_repro_55_getonly_prop_assign_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("55-getonly-prop-assign");
    const MSG: &str = "The property";
    let flagged_compound = stdout
        .lines()
        .any(|l| l.contains("Main.as:12") && l.contains(MSG) && l.contains("no set accessor"));
    let flagged_plain = stdout
        .lines()
        .any(|l| l.contains("Main.as:13") && l.contains(MSG) && l.contains("no set accessor"));
    assert!(
        flagged_compound && flagged_plain,
        "expected no-set-accessor errors on `loc.tx +=` and `loc.tx =`; stdout={stdout:?}"
    );
    // Legal: read (14), whole-struct assign (15), compound on a float local (16).
    let legal_flagged = stdout.lines().any(|l| {
        (l.contains("Main.as:14") || l.contains("Main.as:15") || l.contains("Main.as:16"))
            && (l.contains("error") || l.contains("warning"))
    });
    assert!(
        !legal_flagged,
        "legal reads / whole-struct assigns must stay silent; stdout={stdout:?}"
    );
}

/// Read-only handle rebind (todo-cover-cases-next.txt,
/// spike-live-add-kinematic-ao): `auto ty = Reflection::GetType(…)`
/// infers `const Reflection::MwClassInfo@`; a value-assign rebind
/// (`ty = …`) is rejected by the game with `Reference is read-only`
/// (Scene.as 258:28, OP 1.29.5). The `@ty = …` handle rebind is legal —
/// the handle *slot* is mutable even when the referent is const.
#[test]
fn check_command_issue_repro_56_readonly_handle_rebind_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("56-readonly-handle-rebind");
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:13") && l.contains("read-only"));
    assert!(
        flagged,
        "expected a read-only error on the `ty = …` value rebind; stdout={stdout:?}"
    );
    // Legal: `@ty = …` handle rebind on line 14 stays silent.
    let legal_flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:14") && (l.contains("error") || l.contains("warning")));
    assert!(
        !legal_flagged,
        "the `@ty = …` handle rebind must stay silent; stdout={stdout:?}"
    );
}

/// GH #53: `check --define DEV` injects extra preprocessor defines so a
/// source-tree gate matches the staged/dev compile. Without the flag the
/// `#if DEV` branch stays dead (clean check); with it, the branch's
/// undefined identifier must surface.
#[test]
fn check_command_issue_repro_53_define_flag_activates_branch() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros/53-define-flag");
    let base = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
        cmd.env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env("NO_COLOR", "1")
            .arg("check");
        for e in extra {
            cmd.arg(e);
        }
        cmd.arg(&fixture);
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    // Baseline: no --define → dead branch → clean.
    let (ok, stdout) = base(&[]);
    assert!(
        ok && stdout.contains("0 diagnostics"),
        "without --define the #if DEV branch must stay dead/clean; stdout={stdout:?}"
    );
    // With --define DEV → branch live → undefined identifier surfaces.
    let (ok, stdout) = base(&["--define", "DEV"]);
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:12") && l.contains("error"));
    assert!(
        !ok && flagged,
        "--define DEV must activate the #if DEV branch and surface its error; stdout={stdout:?}"
    );
    // = form parses too.
    let (ok, stdout) = base(&["--define=DEV"]);
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:12") && l.contains("error"));
    assert!(
        !ok && flagged,
        "--define=DEV must parse the same as --define DEV; stdout={stdout:?}"
    );
}

/// GH #47: `UI::InputText(label, string, false)` has no matching overload —
/// the 3rd arg must be `bool&out changed` (l-value) or omitted; a `bool`
/// value binds to neither candidate. Game-compiler ground truth quoted in the
/// issue (OP 1.27.9). Fixed — must keep flagging.
#[test]
fn check_command_issue_repro_47_inputtext_overload_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("47-inputtext-overload");
    let flagged_multi = stdout
        .lines()
        .any(|l| l.contains("Main.as:20") && l.contains("InputText"));
    let flagged_out = stdout
        .lines()
        .any(|l| l.contains("Main.as:27") && l.contains("Begin"));
    assert!(
        flagged_multi,
        "expected a diagnostic on the `UI::InputText(.., false)` line; stdout={stdout:?}"
    );
    assert!(
        flagged_out,
        "expected a diagnostic on the `UI::Begin(.., true)` bool-literal-into-&out line; stdout={stdout:?}"
    );
    // Legal: 2-arg (17), bool&out l-value (19), float&out l-value (22),
    // numeric-literal temp bind (23), bool&out l-value Begin (25) stay silent.
    let legal_flagged = stdout.lines().any(|l| {
        (l.contains("Main.as:17")
            || l.contains("Main.as:19")
            || l.contains("Main.as:22")
            || l.contains("Main.as:23")
            || l.contains("Main.as:25"))
            && l.contains("error")
    });
    assert!(
        !legal_flagged,
        "legal InputText calls must stay silent; stdout={stdout:?}"
    );
}

/// GH #49: AngelScript has no implicit nat3 -> int3 conversion — the game
/// rejects both the implicit init and the int3(nat3) constructor form.
/// Ground truth quoted in the issue (tm-editor-plus-plus). Fixed — must flag.
#[test]
fn check_command_issue_repro_49_nat3_int3_conversion_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("49-nat3-int3-conversion");
    let bad_init = stdout.lines().any(|l| l.contains("Main.as:9"));
    let bad_ctor = stdout.lines().any(|l| l.contains("Main.as:12"));
    assert!(
        bad_init && bad_ctor,
        "expected diagnostics on both nat3->int3 lines (implicit init + ctor form); stdout={stdout:?}"
    );
    // Legal: exact nat3 copy (line 10), three-int ctor (line 11).
    let legal_flagged = stdout
        .lines()
        .any(|l| (l.contains("Main.as:10") || l.contains("Main.as:11")) && l.contains("error"));
    assert!(
        !legal_flagged,
        "legal nat3/int3 initializers must stay silent; stdout={stdout:?}"
    );
}

/// GH #50: member access on a typedb engine type must check the type's own
/// members AND its base-class chain. `CControlBase` has no `Visible` (it
/// lives on `CGameManialinkControl`); the game rejects `c.Visible` with
/// `'Visible' is not a member of 'CControlBase'`. Fixed — must flag.
#[test]
fn check_command_issue_repro_50_base_class_member_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("50-base-class-member");
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:13") && l.contains("Visible"));
    assert!(
        flagged,
        "expected a diagnostic on `c.Visible` (CControlBase has no Visible); stdout={stdout:?}"
    );
    // Legal: own member IsReadOnly (line 14), inherited CSceneMobil.Model (line 15).
    let legal_flagged = stdout
        .lines()
        .any(|l| (l.contains("Main.as:14") || l.contains("Main.as:15")) && l.contains("error"));
    assert!(
        !legal_flagged,
        "legal member accesses must stay silent; stdout={stdout:?}"
    );
}

/// GH #51: member access on a `cast<T>()` result must check T's member set.
/// `CControlFrame` only has `ChildsRelativeLocations`; the game rejects
/// `cf.Visible` with `'Visible' is not a member of 'CControlFrame'`. Fixed.
#[test]
fn check_command_issue_repro_51_cast_member_check_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("51-cast-member-check");
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:15") && l.contains("Visible"));
    assert!(
        flagged,
        "expected a diagnostic on `cf.Visible` (CControlFrame has no Visible); stdout={stdout:?}"
    );
    // Legal: own member (12), base CControlBase (13), base CControlContainer (14).
    let legal_flagged = stdout.lines().any(|l| {
        (l.contains("Main.as:12") || l.contains("Main.as:13") || l.contains("Main.as:14"))
            && l.contains("error")
    });
    assert!(
        !legal_flagged,
        "legal member accesses on the cast result must stay silent; stdout={stdout:?}"
    );
}

/// GH #52 (repro 1): member lookup on an engine type consults the own set
/// plus base chain only. The game rejects `map.FileName` (`'FileName' is
/// not a member of 'CGameCtnChallenge'`); the legal path is
/// `map.MapInfo.FileName` (base CGameFid carries it). Game-compiler ground
/// truth (RemoteBuild probe 2026-08-18): line 11, exact wording. Must flag.
#[test]
fn check_command_issue_repro_52_engine_member_miss_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("52-engine-member-miss");
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:11") && l.contains("FileName"));
    assert!(
        flagged,
        "expected a diagnostic on `map.FileName`; stdout={stdout:?}"
    );
    // Legal: base-chain MapInfo.FileName (line 12), own MapName (line 13).
    let legal_flagged = stdout
        .lines()
        .any(|l| (l.contains("Main.as:12") || l.contains("Main.as:13")) && l.contains("error"));
    assert!(
        !legal_flagged,
        "legal member accesses must stay silent; stdout={stdout:?}"
    );
}

/// GH #52 (repro 2): a ternary whose branches have no unifiable common
/// type. The game rejects `c ? "" : map.MapInfo.FileName` (string vs
/// wstring), `c ? 1 : true` (int vs bool) and `c ? map.Size : int3(1,2,3)`
/// (nat3 vs int3) with `Can't find unambiguous implicit conversion to make
/// both expressions have the same type`. Game-compiler ground truth
/// (RemoteBuild probe 2026-08-18): ERR at 13:19, 14:16, 15:17, exact
/// wording, nothing else. Must flag all three; legal counterparts stay
/// silent.
#[test]
fn check_command_issue_repro_52b_ternary_type_ambiguity_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("52b-ternary-type-ambiguity");
    const MSG: &str = "Can't find unambiguous implicit conversion";
    for line in [13u16, 14, 15] {
        let flagged = stdout
            .lines()
            .any(|l| l.contains(&format!("Main.as:{line}")) && l.contains(MSG));
        assert!(
            flagged,
            "expected the unambiguous-conversion error on Main.as:{line}; stdout={stdout:?}"
        );
    }
    // Legal: same-type / numeric / null+handle ternaries (lines 16-20) —
    // the game verdict records NOTHING else on those lines, so guard both
    // errors and warnings.
    let legal_flagged = stdout.lines().any(|l| {
        (l.contains("Main.as:16")
            || l.contains("Main.as:17")
            || l.contains("Main.as:18")
            || l.contains("Main.as:19")
            || l.contains("Main.as:20"))
            && (l.contains("error") || l.contains("warning"))
    });
    assert!(
        !legal_flagged,
        "legal ternaries must stay silent; stdout={stdout:?}"
    );
}

/// GH #54: a single-argument value-type constructor is an implicit
/// conversion — `mat4(const mat3&in)` is registered, so
/// `mat4::Inverse(someMat3)` compiles and the LSP must stay silent (it used
/// to emit `expected mat4, got mat3`). The counterpart has no such ctor and
/// must still flag. Game-compiler ground truth (RemoteBuild probe of this
/// exact fixture, 2026-08-22): exactly ONE diagnostic, `(18,16) ERR: No
/// matching signatures to 'mat4::Inverse(vec3&)'` — every conversion line
/// compiles clean. Fixed — must stay clean on the legal lines.
#[test]
fn check_command_issue_repro_54_ctor_implicit_conversion_is_silent() {
    let (_, stdout, _) = run_issue_repro_typedb("54-ctor-implicit-conversion");
    // Legal: mat4(mat3) ctor conversion (12), mat3(mat4) (16), exact (17).
    let legal_flagged = stdout.lines().any(|l| {
        (l.contains("Main.as:12") || l.contains("Main.as:16") || l.contains("Main.as:17"))
            && (l.contains("error") || l.contains("warning"))
    });
    assert!(
        !legal_flagged,
        "ctor-conversion and exact-match args must stay silent; stdout={stdout:?}"
    );
    // Negative: no `mat4(const vec3&in)` exists — the game rejects line 18.
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:18") && l.contains("error"));
    assert!(
        flagged,
        "expected an error on `mat4::Inverse(vec3)` (no such ctor); stdout={stdout:?}"
    );
}

/// GH #48: exit-code contract. Warnings-only → exit 0 by default;
/// `--warnings-as-errors` / `-Werror` → exit 1 when warnings are present;
/// errors always exit 1.
#[test]
fn check_command_exit_codes_and_warnings_as_errors() {
    // Warnings-only plugin (Signed/Unsigned mismatch, GH #37 warning class).
    let root = make_temp_plugin("gh48-warn-only");
    std::fs::write(
        root.join("Main.as"),
        "void Main() {\n    int i = 0;\n    uint u = 1;\n    if (i < u) {\n        u = u + 1;\n    }\n}\n",
    )
    .unwrap();

    let run = |extra: &[&str]| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
        cmd.arg("check").arg("--no-typedb").arg(&root);
        for a in extra {
            cmd.arg(a);
        }
        cmd.env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };

    // Default: warnings-only exits 0.
    let out = run(&[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Signed/Unsigned mismatch"),
        "expected the warning in output; stdout={stdout:?}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "warnings-only must exit 0 by default; stdout={stdout:?}"
    );

    // --warnings-as-errors: warnings-only exits 1.
    let out = run(&["--warnings-as-errors"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "--warnings-as-errors must exit 1 on warnings; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );

    // -Werror short form, same behavior.
    let out = run(&["-Werror"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "-Werror must exit 1 on warnings; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );

    // --warnings-as-errors=value form.
    let out = run(&["--warnings-as-errors=true"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "--warnings-as-errors=true must exit 1 on warnings; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Invalid value → usage error (exit 2).
    let out = run(&["--warnings-as-errors=banana"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "invalid --warnings-as-errors value must be a usage error"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// GH #48 follow-up: `warnings_as_errors` as a persistent config option.
/// Precedence: CLI flag > workspace `.openplanet-lsp.toml` > user global
/// config (`$HOME/.config/openplanet-lsp/config.toml`) > default(false).
/// Runs the real binary as a subprocess so $HOME can be pinned per case
/// (deterministic — no dependence on the dev machine's user config).
#[test]
fn check_command_warnings_as_errors_config_precedence() {
    // Warnings-only plugin (Signed/Unsigned mismatch, GH #37 warning class).
    let root = make_temp_plugin("gh48-werror-config");
    std::fs::write(
        root.join("Main.as"),
        "void Main() {\n    int i = 0;\n    uint u = 1;\n    if (i < u) {\n        u = u + 1;\n    }\n}\n",
    )
    .unwrap();

    // Fake $HOME with a user-global config setting warnings_as_errors.
    let home =
        std::env::temp_dir().join(format!("openplanet-lsp-gh48-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".config/openplanet-lsp")).unwrap();

    let run = |home_value: Option<&std::path::Path>, extra: &[&str]| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
        cmd.arg("check").arg("--no-typedb").arg(&root);
        for a in extra {
            cmd.arg(a);
        }
        if let Some(h) = home_value {
            cmd.env("HOME", h);
        }
        cmd.env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };

    // Sanity: warnings-only, no config anywhere → exit 0.
    let out = run(None, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "no config, no flag: warnings-only must exit 0; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );

    // ── User-global layer ──────────────────────────────────────────────
    fs::write(
        home.join(".config/openplanet-lsp/config.toml"),
        "warnings_as_errors = true\n",
    )
    .unwrap();

    // Flag absent + user config true → exit 1.
    let out = run(Some(&home), &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "user config warnings_as_errors=true (flag absent) must exit 1 on warnings"
    );

    // Explicit --warnings-as-errors=false beats user config true → exit 0.
    let out = run(Some(&home), &["--warnings-as-errors=false"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--warnings-as-errors=false must override user config true"
    );

    // ── Workspace layer (beats user global) ────────────────────────────
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "warnings_as_errors = true\n",
    )
    .unwrap();

    // Workspace true + user config true, flag absent → exit 1 (workspace
    // still consulted even though the user layer already says true).
    let out = run(Some(&home), &[]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "workspace warnings_as_errors=true (flag absent) must exit 1 on warnings"
    );

    // Workspace false beats user global true → exit 0.
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "warnings_as_errors = false\n",
    )
    .unwrap();
    let out = run(Some(&home), &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "workspace warnings_as_errors=false must override user config true"
    );

    // Flag true beats workspace false → exit 1.
    let out = run(Some(&home), &["--warnings-as-errors"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "--warnings-as-errors must override workspace warnings_as_errors=false"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
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

/// Game-verified 2026-08-23: (1) unknown members on parentless script
/// classes fail the game compile; (2) `.WithRunContext(` on an awaitable
/// is a removed API (silent runtime no-op) — replacement
/// `Meta::StartWithRunContext`. Fixture 58 carries both plus the
/// real-member negative case.
#[test]
fn check_command_issue_repro_58_member_access_and_removed_api() {
    let (_, stdout, stderr) = run_issue_repro_typedb("58-member-access-checks");
    assert!(
        stdout.contains("type `SceneHandle` has no member `AssertSame`"),
        "expected UndefinedMember in stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("`awaitable.WithRunContext()` is no longer valid. Use `Meta::StartWithRunContext` instead"),
        "expected removed-API hint in stdout={stdout:?} stderr={stderr:?}"
    );
    // `h.PayloadValid()` is a real member — must NOT be flagged.
    assert!(
        !stdout.contains("has no member `PayloadValid`"),
        "real member must stay silent; stdout={stdout:?}"
    );
}
