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

fn isolated_home(name: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "openplanet-lsp-home-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".config/openplanet-lsp")).unwrap();
    home
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
fn check_command_keeps_utf8_byte_columns_with_unicode_prefixes() {
    let root = make_temp_plugin("unicode-byte-columns");
    let source = "void Main() { /* é😀 */ MissingType@ bad; }\n";
    fs::write(root.join("src/Main.as"), source).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let byte_column = source.find("MissingType").unwrap() + 1;
    assert!(
        stdout.contains(&format!(
            "src/Main.as:1:{byte_column}: error: unknown type `MissingType`"
        )),
        "CLI byte-column output changed: {stdout:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_command_exits_zero_when_clean() {
    let root = make_temp_plugin("clean");
    // `_f`, not `f`: this local is genuinely unused (GH #55 warns on that)
    // and only exists to make the call a statement. Do not "tidy" the
    // prefix away — the subject here is the exit code, not the warning.
    fs::write(
        root.join("src/Main.as"),
        "class Foo {}\nFoo@ MakeFoo() { return null; }\nvoid Main() { Foo@ _f = MakeFoo(); }\n",
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
fn check_command_reports_each_config_problem_once() {
    let root = make_temp_plugin("single-config-warning");
    let home = isolated_home("single-config-warning");
    fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
    fs::write(root.join(".openplanet-lsp.toml"), "unused = \"banana\"\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "invalid config values should warn without failing check; stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        stderr.matches("warning: config:").count(),
        1,
        "each config problem must be reported once; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("key `unused`") && stderr.contains("expected one of"),
        "warning should identify the bad key and accepted values; stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// A source filter can omit a sibling file that contributes to an open
/// AngelScript namespace. The real plugin module still includes that file, so
/// `check` must not claim its remaining namespace surface is authoritative.
fn assert_omitted_own_module_namespace_member_stays_silent(name: &str, config: &str) {
    let root = make_temp_plugin(name);
    fs::create_dir_all(root.join("omitted")).unwrap();
    fs::write(root.join(".openplanet-lsp.toml"), config).unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
namespace Shared { void Present() {} }
void Main() {
    Shared::Present();
    Shared::FromOmittedOwnModuleFile();
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("omitted/Contribution.as"),
        "namespace Shared { void FromOmittedOwnModuleFile() {} }\n",
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
        !stdout.contains("No matching symbol 'Shared::FromOmittedOwnModuleFile'"),
        "an omitted sibling can extend Shared in the real module; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        output.status.success(),
        "the source filter must not turn an omitted sibling declaration into a diagnostic; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn check_command_source_paths_omit_sibling_namespace_member_without_false_positive() {
    assert_omitted_own_module_namespace_member_stays_silent(
        "source-paths-namespace-authority",
        "source_paths = [\"src\"]\n",
    );
}

#[test]
fn check_command_ignore_paths_omit_sibling_namespace_member_without_false_positive() {
    assert_omitted_own_module_namespace_member_stays_silent(
        "ignore-paths-namespace-authority",
        "ignore_paths = [\"omitted\"]\n",
    );
}

#[test]
fn check_command_accepts_relative_plugin_path() {
    let root = make_temp_plugin("relative-clean");
    // `_f`, not `f`: GH #55 unused-local suppression. The subject here is
    // relative-path resolution, not the unused warning — keep the prefix.
    fs::write(
        root.join("src/Main.as"),
        "class Foo {}\nvoid Main() { Foo@ _f = Foo(); }\n",
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

/// B024: a workspace `defines` list ADDS to the game-target set. Before the
/// fix, `defines = ["DEV"]` replaced everything, so `#if TMNEXT` (and every
/// always-on flag) went dead and the masked-out code errored.
#[test]
fn check_command_adds_config_defines_without_dropping_target_defines() {
    let root = make_temp_plugin("config-defines-additive");
    let home = isolated_home("config-defines-additive");
    fs::write(root.join(".openplanet-lsp.toml"), "defines = [\"DEV\"]\n").unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if !TMNEXT
MissingType should_fail_without_platform_define;
#endif

#if !WINDOWS
MissingType should_fail_without_always_on_define;
#endif

#if !DEV
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
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "custom defines must not wipe the target set; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B024: `-NAME` / `!NAME` still turns a define OFF — the one thing the old
/// replace semantics were good for.
#[test]
fn check_command_honors_define_removal_prefixes() {
    let root = make_temp_plugin("config-defines-removal");
    let home = isolated_home("config-defines-removal");
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "defines = [\"DEV\", \"-LOGS\", \"!SIG_DEVELOPER\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if LOGS
MissingType should_fail_when_dash_removal_ignored;
#endif

#if SIG_DEVELOPER
MissingType should_fail_when_bang_removal_ignored;
#endif

#if !TMNEXT
MissingType should_fail_without_platform_define;
#endif

#if !DEV
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
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "removal prefixes not applied; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B026: `check` used to re-apply the plugin manifest AFTER `LspConfig::load`
/// had already applied it and run the user's `-NAME` removals, so a removal of
/// a manifest- or dependency-derived define was silently undone on the CLI
/// path while the LSP path honoured it.
#[test]
fn check_command_honors_removal_of_manifest_and_dependency_defines() {
    let root = make_temp_plugin("config-defines-manifest-removal");
    let home = isolated_home("config-defines-manifest-removal");
    fs::write(
        root.join("info.toml"),
        r#"
[meta]
name = "CLI Check Fixture"
version = "0.1.0"

[script]
optional_dependencies = ["Editor"]
defines = ["MANIFEST_OFF", "MANIFEST_ON"]
"#,
    )
    .unwrap();
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "defines = [\"-MANIFEST_OFF\", \"-DEPENDENCY_EDITOR\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if MANIFEST_OFF
MissingType manifest_removal_was_undone;
#endif

#if DEPENDENCY_EDITOR
MissingType dependency_removal_was_undone;
#endif

#if !MANIFEST_ON
MissingType removal_over_deleted_a_sibling;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "manifest/dependency define removals must survive the CLI path; \
         stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B026 variant: the CLI flag is the top layer, so `--define X` still wins
/// over a config-file `-X`.
#[test]
fn check_command_define_flag_beats_a_config_removal() {
    let root = make_temp_plugin("config-defines-flag-beats-removal");
    let home = isolated_home("config-defines-flag-beats-removal");
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "defines = [\"-FEATURE_X\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        "#if !FEATURE_X\nMissingType flag_did_not_win;\n#endif\n\nvoid Main() {}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--define")
        .arg("FEATURE_X")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--define must win over a config removal; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// GH #64: `--signature` narrows the SIG_* set for one run, so a plugin can
/// be checked the way it compiles once signed.
#[test]
fn check_command_signature_flag_narrows_sig_defines() {
    let root = make_temp_plugin("signature-flag");
    let home = isolated_home("signature-flag");
    fs::write(
        root.join("src/Main.as"),
        r#"
#if SIG_DEVELOPER
MissingType developer_branch_should_be_dead;
#endif

#if SIG_SCHOOL
MissingType school_branch_should_be_dead;
#endif

#if !SIG_REGULAR
MissingType regular_branch_should_be_live;
#endif

#if !SIG_OFFICIAL
MissingType official_branch_should_be_live;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let run = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
        cmd.arg("check").arg("--no-typedb");
        for a in extra {
            cmd.arg(a);
        }
        cmd.arg(&root).env("HOME", &home).output().unwrap()
    };

    let output = run(&["--signature", "regular"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--signature regular must leave exactly SIG_OFFICIAL + SIG_REGULAR; \
         stdout={stdout:?} stderr={stderr:?}"
    );

    // The alias form and `=value` spelling behave identically.
    let output = run(&["--signature=REG"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--signature=REG must match `regular`; stdout={stdout:?}"
    );

    // Default (no flag) is developer: the dev branches are live, so the
    // fixture's `#if SIG_DEVELOPER` block errors.
    let output = run(&[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() && !stdout.contains("0 diagnostics"),
        "default must stay developer (all four SIG_* on); stdout={stdout:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// GH #64: `--signature` is the top layer — it beats a workspace
/// `signature_mode`, and `defines` removals still apply after it.
#[test]
fn check_command_signature_flag_beats_config_signature_mode() {
    let root = make_temp_plugin("signature-flag-precedence");
    let home = isolated_home("signature-flag-precedence");
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "signature_mode = \"official\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        "#if !SIG_DEVELOPER\nMissingType flag_did_not_win;\n#endif\n\nvoid Main() {}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--signature")
        .arg("developer")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--signature must beat config signature_mode; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// GH #64: `--game-target` selects the platform family for one run, beating
/// a workspace `game_target`.
#[test]
fn check_command_game_target_flag_selects_platform() {
    let root = make_temp_plugin("game-target-flag");
    let home = isolated_home("game-target-flag");
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "game_target = \"TMNEXT\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if TMNEXT
MissingType tmnext_should_be_dead;
#endif

#if !MP4
MissingType mp4_should_be_live;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--game-target=mp4")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--game-target must beat config game_target; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// A typo'd selector is a usage error (exit 2), never a silently different
/// define set.
#[test]
fn check_command_rejects_unknown_selector_values() {
    let root = make_temp_plugin("selector-usage");
    for (flag, value, needle) in [
        ("--game-target", "BANANA", "unknown --game-target value"),
        ("--signature", "banana", "unknown --signature value"),
        ("--os", "banana", "unknown --os value"),
        ("--arch", "banana", "unknown --arch value"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
            .arg("check")
            .arg("--no-typedb")
            .arg(flag)
            .arg(value)
            .arg(&root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{flag} exit code: {stderr}");
        assert!(stderr.contains(needle), "{flag} message: {stderr}");
    }
    let _ = fs::remove_dir_all(root);
}

/// B024 + GH #36: a hand-listed platform selector REPLACES the target's, so
/// exactly one `#if <platform>` family stays live — and the run says so.
#[test]
fn check_command_swaps_platform_when_defines_list_another_selector() {
    let root = make_temp_plugin("config-defines-platform");
    let home = isolated_home("config-defines-platform");
    fs::write(root.join(".openplanet-lsp.toml"), "defines = [\"MP4\"]\n").unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if TMNEXT
MissingType should_fail_when_target_platform_still_live;
#endif

#if !MP4
MissingType should_fail_without_listed_platform;
#endif

#if !WINDOWS
MissingType should_fail_without_always_on_define;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "listed platform selector must replace the target's; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("game_target"),
        "expected a warning pointing at game_target; stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

// ── B028: `--os` / `--arch` selectors and build-flag narrowing ──────────
//
// The live game (2026-08-29 Wine probe, tests/fixtures/issue-repros/
// b028-*-probe) defines exactly one OS (WINDOWS + WINDOWS_WINE under Wine)
// and one arch (MANIA64); SERVER/LOGS/HAS_DEV/DEVELOPER are off on a
// client. The LSP used to define all of them at once (B028).

/// B028: `--os linux` models a native Linux run — LINUX live, WINDOWS dead —
/// and beats a workspace `os_target` (same ladder as `--game-target`).
#[test]
fn check_command_os_flag_selects_os_defines() {
    let root = make_temp_plugin("os-flag");
    let home = isolated_home("os-flag");
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "os_target = \"windows\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if WINDOWS
MissingType windows_should_be_dead_under_linux;
#endif

#if !LINUX
MissingType linux_should_be_live_under_linux;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--os")
        .arg("linux")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--os linux must make LINUX live and WINDOWS dead; \
         stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B028: `--arch 32` flips the arch pair — MANIA32 live, MANIA64 dead.
/// Never both (that is the reported failure mode).
#[test]
fn check_command_arch_flag_selects_arch_defines() {
    let root = make_temp_plugin("arch-flag");
    let home = isolated_home("arch-flag");
    fs::write(
        root.join("src/Main.as"),
        r#"
#if MANIA64
MissingType sixty_four_should_be_dead_on_32;
#endif

#if !MANIA32
MissingType thirty_two_should_be_live_on_32;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--arch")
        .arg("32")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--arch 32 must make MANIA32 live and MANIA64 dead; \
         stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B028 (2026-08-29 Wine probe ground truth): `--os wine` defines WINDOWS
/// and WINDOWS_WINE TOGETHER (the game under Wine has both), with LINUX and
/// MANIA32 dead.
#[test]
fn check_command_os_wine_defines_windows_and_wine_together() {
    let root = make_temp_plugin("os-wine");
    let home = isolated_home("os-wine");
    fs::write(
        root.join("src/Main.as"),
        r#"
#if !WINDOWS_WINE
MissingType wine_should_be_defined;
#endif

#if !WINDOWS
MissingType windows_rides_along_with_wine;
#endif

#if LINUX
MissingType linux_should_be_dead_under_wine;
#endif

#if MANIA32
MissingType thirty_two_should_be_dead_by_default;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--os")
        .arg("wine")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--os wine must define WINDOWS + WINDOWS_WINE and nothing else; \
         stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B028: `--os` is the top of the ladder — it beats a workspace
/// `os_target`, like `--signature` beats `signature_mode` (GH #64).
#[test]
fn check_command_os_flag_beats_config_os_target() {
    let root = make_temp_plugin("os-flag-precedence");
    let home = isolated_home("os-flag-precedence");
    fs::write(root.join(".openplanet-lsp.toml"), "os_target = \"wine\"\n").unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if WINDOWS
MissingType flag_did_not_win;
#endif

#if !LINUX
MissingType linux_should_be_live;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--os")
        .arg("linux")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "--os must beat config os_target; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

/// B028 decision (a): the build flags default ON (no default churn) and are
/// narrowed via config (`server = false`, `arch_target`), while `defines`
/// still gets the last word — `defines = ["SERVER"]` re-enables SERVER in
/// the narrowed profile. That is the whole narrowing flow end-to-end.
#[test]
fn check_command_defines_re_enable_server_under_narrowed_profile() {
    let root = make_temp_plugin("narrowed-profile-server");
    let home = isolated_home("narrowed-profile-server");
    fs::write(
        root.join(".openplanet-lsp.toml"),
        "os_target = \"linux\"\narch_target = \"32\"\nserver = false\ndefines = [\"SERVER\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        r#"
#if !SERVER
MissingType server_should_be_re_enabled_by_defines;
#endif

#if MANIA64
MissingType sixty_four_should_be_dead_on_32;
#endif

void Main() {}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "defines = [\"SERVER\"] must re-enable SERVER under the narrowed \
         profile; stdout={stdout:?} stderr={stderr:?}"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn check_command_applies_manifest_defines_and_dependency_defines() {
    let root = make_temp_plugin("manifest-defines");
    let home = isolated_home("manifest-defines");
    let plugins_dir = root.join("plugins");
    fs::create_dir_all(plugins_dir.join("Editor")).unwrap();
    // B019 only defines DEPENDENCY_<ID> for installed optional dependencies.
    // Keep the dependency under this fixture's explicit search root so the
    // assertion never relies on a developer's Openplanet plugin install.
    fs::write(
        plugins_dir.join("Editor/info.toml"),
        "[meta]\nname = \"Editor\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
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
        .arg("--plugins-dir")
        .arg(&plugins_dir)
        .arg(&root)
        .env("HOME", &home)
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
    let _ = fs::remove_dir_all(home);
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

/// GH #28 negative guard, narrowed by GH #58: a namespace the checker has
/// no authoritative surface for stays silent (`NotDeclaredAnywhere::Foo` —
/// absent from both workspace and typedb), and legal relocated
/// (`Display::GetWidth`) / known-member (`UI::Begin`) calls stay clean. A
/// namespace DECLARED in the workspace is authoritative once the dependency
/// closure is complete, so its unknown member now errors (GH #58).
#[test]
fn check_command_issue_repro_28b_user_namespace_stays_silent() {
    let (_, stdout, _) = run_issue_repro_typedb("28b-user-namespace-silent");
    for line in ["Main.as:17", "Main.as:19", "Main.as:20", "Main.as:21"] {
        assert!(
            !stdout
                .lines()
                .any(|l| l.contains(line) && l.contains("error")),
            "{line} must stay silent; stdout={stdout:?}"
        );
    }
    let flagged = stdout.lines().any(|l| {
        l.contains("Main.as:18") && l.contains("No matching symbol 'MyHelpers::NotInDbEither'")
    });
    assert!(
        flagged,
        "GH #58: an unknown member of a workspace-declared namespace must error; stdout={stdout:?}"
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

/// GH #63 regression net: `UI::InputTextCallback` (namespaced Core funcdef)
/// end-to-end usage must stay clean — free-function / method / funcdef-handle
/// decay into `InputTextCallback@` parameters, the ctor form, bare-handle
/// assignment, and calls through a funcdef handle. Game ground truth
/// (RemoteBuild probe 2026-08-29, OP 1.29.5): the fixture compiles clean.
/// Fixed in 8e49f81 — a checker regression here fails CI.
#[test]
fn check_command_issue_repro_63_inputtext_callback_funcdef_is_clean() {
    let (ok, stdout, stderr) = run_issue_repro_typedb("63-inputtext-callback-funcdef");
    assert!(
        ok && stdout.contains("0 diagnostics"),
        "expected 0 diagnostics on issue-repro 63-inputtext-callback-funcdef; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B025: `this.Method` / `obj.Method` references at funcdef ctor sites are
/// method *references* — the ctor is the required method-to-funcdef
/// conversion (methods do not implicitly decay like free functions), so the
/// member-access return type must not be used as the arg type. Game ground
/// truth (RemoteBuild probe 2026-08-29, OP 1.29.5): the LEGAL half compiles
/// clean; the three DIAGNOSE lines fail in-game (`Can't create delegate` /
/// `No matching signatures to 'CoroutineFunc(string)'`).
#[test]
fn check_command_issue_repro_b025_method_references_at_funcdef_ctors() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b025-this-method-funcdef-ctor");
    // Exactly the three game-rejected lines are flagged; every legal form
    // (this.Tick, w.Tick, this.OV overload bind, this.Revert void(ref@),
    // handle init, ternary) stays silent.
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains("error:")).collect();
    assert_eq!(
        errors.len(),
        3,
        "expected exactly 3 errors on the diagnosed lines; stdout={stdout:?}"
    );
    assert!(
        errors.iter().any(|l| l.contains("Main.as:49")),
        "bool() method into void() CoroutineFunc must be flagged; stdout={stdout:?}"
    );
    assert!(
        errors.iter().any(|l| l.contains("Main.as:51")),
        "void() method into CoroutineFuncUserdata must be flagged; stdout={stdout:?}"
    );
    assert!(
        errors.iter().any(|l| l.contains("Main.as:53")),
        "property reference must be flagged; stdout={stdout:?}"
    );
}

/// B028 regression: the same function declared in `#if MANIA32` and
/// `#if MANIA64` (and `#if WINDOWS` / `#if LINUX`) branches. The LSP used to
/// define both sides of each pair at once, so the second declaration was a
/// phantom duplicate-declaration error. Game ground truth (RemoteBuild probe
/// 2026-08-29): CLEAN — the game defines one arch and one OS and compiles a
/// single branch. The default profile (windows + 64) must match that.
#[test]
fn check_command_issue_repro_b028_dup_decl_across_arch_os_is_clean() {
    let (ok, stdout, stderr) = run_issue_repro_typedb("b028-dup-decl-arch-os");
    assert!(
        ok && stdout.contains("0 diagnostics"),
        "b028-dup-decl-arch-os must be clean under the default profile \
         (windows + 64); stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B028 probe fixture: one positive/negative trap per OS/arch flag. Game
/// ground truth (RemoteBuild probe 2026-08-29): CLEAN with WINDOWS,
/// WINDOWS_WINE and MANIA64 defined and LINUX/MANIA32 undefined — so the
/// LSP must also be clean when `--os wine` models the live game exactly.
#[test]
fn check_command_issue_repro_b028_os_arch_probe_matches_game_with_os_wine() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros/b028-os-arch-defines-probe");
    let typedb = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let home = isolated_home("b028-os-arch-probe");
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--typedb-dir")
        .arg(&typedb)
        .arg("--os")
        .arg("wine")
        .arg(&fixture)
        .env("HOME", &home)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "b028-os-arch-defines-probe must be clean under --os wine (the \
         measured game set); stdout={stdout:?} stderr={stderr:?}"
    );
    let _ = fs::remove_dir_all(home);
}

/// B028 probe fixture for the build-shape flags: SERVER/LOGS/HAS_DEV/
/// DEVELOPER traps under `#if FLAG`. Game ground truth (RemoteBuild probe
/// 2026-08-29): CLEAN — none of the four is defined on a normal client.
/// Per decision (a) the LSP defaults them ON (no default churn), so matching
/// the game's client profile takes the `defines` removals a real user would
/// write — which must yield the same clean verdict as the game.
#[test]
fn check_command_issue_repro_b028_build_flag_probe_matches_client_profile() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros/b028-build-flag-probe");
    let typedb = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let home = isolated_home("b028-build-flag-probe");
    fs::write(
        home.join(".config/openplanet-lsp/config.toml"),
        "defines = [\"-SERVER\", \"-LOGS\", \"-HAS_DEV\", \"-DEVELOPER\"]\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--typedb-dir")
        .arg(&typedb)
        .arg(&fixture)
        .env("HOME", &home)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("0 diagnostics"),
        "b028-build-flag-probe must match the game's client profile once \
         the four build flags are narrowed away; stdout={stdout:?} \
         stderr={stderr:?}"
    );
    let _ = fs::remove_dir_all(home);
}

/// B027: delegate construction requires EXACT parameter passing modes.
/// Game ground truth (RemoteBuild probe 2026-08-29, OP 1.29.5): the seven
/// MISMATCH lines error in-game (`Can't create delegate`, free-fn ones as
/// `Can't implicitly convert from '<function>@const'`); the exact-match
/// lines (plain/&in/&out methods and free fns, object-typed &inout) compile
/// clean. `&in` and `&inout` are enforced exactly like `&out`.
#[test]
fn check_command_issue_repro_b027_funcdef_param_modes() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b027-funcdef-param-modes");
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains("error:")).collect();
    let expected = [
        // &out method -> plain funcdef / plain method -> &out funcdef
        "Main.as:37",
        "Main.as:39",
        // free functions, both polarities
        "Main.as:41",
        "Main.as:42",
        // &in enforced exactly, both polarities
        "Main.as:44",
        "Main.as:45",
        // object-typed &inout funcdef vs &out method
        "Main.as:53",
    ];
    assert_eq!(
        errors.len(),
        expected.len(),
        "expected exactly {} errors; stdout={stdout:?} stderr={stderr:?}",
        expected.len()
    );
    for line in expected {
        assert!(
            errors.iter().any(|e| e.contains(line)),
            "mode mismatch on {line} must be flagged; stdout={stdout:?}"
        );
    }
    // Exact matches (47/48/50/51/55) must stay silent — covered by the
    // exact count above, but name them for the failure message.
    for line in [
        "Main.as:47",
        "Main.as:48",
        "Main.as:50",
        "Main.as:51",
        "Main.as:55",
    ] {
        assert!(
            !errors.iter().any(|e| e.contains(line)),
            "exact mode match on {line} must stay silent; stdout={stdout:?}"
        );
    }
}

/// B023: wrong-signature / non-function args passed directly into typedb
/// funcdef-typed parameter slots must be rejected (`NoMatchingSignatures`,
/// like the game's `No matching signatures to '...'`). Game ground truth
/// (RemoteBuild probe 2026-08-29, OP 1.29.5): the seven REJECT lines error
/// in-game — UI::InputText / InputTextMultiline callback slots (wrong-sig
/// fn, string literal, wrong-typed funcdef var), startnew, UI::NodTree and
/// Dev::InterceptProc — while every matching form (free fn, ctor-wrapped
/// fn, right-typed var, void() into startnew, matching member callbacks)
/// compiles clean.
#[test]
fn check_command_issue_repro_b023_funcdef_slot_signatures() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b023-funcdef-slot-signatures");
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains("error:")).collect();
    let expected = [
        "Main.as:23", // InputText + wrong-sig fn
        "Main.as:25", // InputText + string literal
        "Main.as:27", // InputText + wrong-typed funcdef var
        "Main.as:29", // InputTextMultiline + wrong-sig fn
        "Main.as:31", // startnew + wrong-sig fn
        "Main.as:33", // NodTree + wrong-sig fn
        "Main.as:35", // InterceptProc + wrong-sig fn
    ];
    assert_eq!(
        errors.len(),
        expected.len(),
        "expected exactly {} errors; stdout={stdout:?} stderr={stderr:?}",
        expected.len()
    );
    for line in expected {
        assert!(
            errors.iter().any(|e| e.contains(line)),
            "slot misuse on {line} must be flagged; stdout={stdout:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains(line) && e.contains("No matching signatures")),
            "{line} must be a NoMatchingSignatures diagnostic; stdout={stdout:?}"
        );
    }
    // Legal forms must stay silent (lines 37-43).
    for line in [
        "Main.as:37",
        "Main.as:38",
        "Main.as:39",
        "Main.as:41",
        "Main.as:42",
        "Main.as:43",
    ] {
        assert!(
            !errors.iter().any(|e| e.contains(line)),
            "matching callback on {line} must stay silent; stdout={stdout:?}"
        );
    }
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

/// todo-cover-cases-next.txt (2026-08-25, tm-dips-plus-plus): a second
/// local of the same name in the SAME function scope is a game error
/// (`'cp0' is already declared`). The failed redeclaration must leave the
/// first binding's type in scope, so a later member access is checked
/// against that first type (cascade). Inner-block shadowing stays the
/// GH #37d warning, not this error.
///
#[test]
fn check_command_issue_repro_dup_local_same_scope_diagnoses() {
    let (ok, stdout, _) = run_issue_repro("dup-local-same-scope");
    let already: Vec<&str> = [
        "Main.as:20",
        "Main.as:26",
        "Main.as:31",
        "Main.as:36",
        "Main.as:41",
        "Main.as:46",
        "Main.as:51",
        "Main.as:55",
        "Main.as:59",
        "Main.as:63",
        "Main.as:67",
        "Main.as:71",
        "Main.as:75",
        "Main.as:80",
    ]
    .into_iter()
    .collect();
    for line in &already {
        let hit = stdout
            .lines()
            .any(|l| l.contains(line) && l.contains("already declared") && l.contains("error"));
        assert!(
            hit,
            "expected `already declared` error on {line}; stdout={stdout:?}"
        );
    }
    let cascade = stdout.lines().any(|l| {
        l.contains("Main.as:21")
            && (l.contains("index") && (l.contains("CpSpec") || l.contains("member")))
    });
    assert!(
        cascade,
        "failed redecl must keep first type: cp0.index vs CpSpec; stdout={stdout:?}"
    );
    let shadow_as_redecl = stdout
        .lines()
        .any(|l| l.contains("Main.as:86") && l.contains("already declared"));
    assert!(
        !shadow_as_redecl,
        "inner-block shadow must not be `already declared`; stdout={stdout:?}"
    );
    let shadow_warn = stdout
        .lines()
        .any(|l| l.contains("Main.as:86") && l.contains("hides another variable"));
    assert!(
        shadow_warn,
        "inner-block shadow must stay the GH #37d warning; stdout={stdout:?}"
    );
    assert!(!ok, "redecl errors must fail the check; stdout={stdout:?}");
}

/// todo-cover-cases-next.txt (2026-08-24, tm-tournament-station): `out` /
/// `in` / `inout` are reserved as identifiers. Game wording:
/// `Instead found reserved keyword 'out'`. A generic parse error that does
/// not name the reserved keyword is not enough — the diagnostic must say
/// the name is reserved so the first signal is not a reload cycle.
///
#[test]
fn check_command_issue_repro_reserved_keyword_local_diagnoses() {
    let (_, stdout, _) = run_issue_repro("reserved-keyword-local");
    for (line, word) in [
        ("Main.as:6", "out"),
        ("Main.as:11", "in"),
        ("Main.as:16", "inout"),
        ("Main.as:21", "out"),
        ("Main.as:25", "out"),
        ("Main.as:29", "out"),
        ("Main.as:30", "in"),
        ("Main.as:31", "inout"),
    ] {
        let hit = stdout.lines().any(|l| {
            l.contains(line)
                && l.contains("error")
                && l.to_ascii_lowercase().contains("reserved keyword")
                && l.contains(word)
        });
        assert!(
            hit,
            "expected reserved-keyword error naming `{word}` on {line}; stdout={stdout:?}"
        );
    }
    let legal_flagged = stdout.lines().any(|l| {
        (l.contains("Main.as:35") || l.contains("Main.as:36") || l.contains("Main.as:37"))
            && l.contains("reserved keyword")
    });
    assert!(
        !legal_flagged,
        "in/out/inout as parameter modifiers must stay legal; stdout={stdout:?}"
    );
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

/// Game-verified 2026-08-23 (spike-live-add-kinematic-ao, HatDump.as:60):
/// a non-const method invoked on a `const T &in` receiver fails the game
/// compile with "No matching signatures to 'SceneHandle::PayloadValid()
/// const'". The fixture carries the exact reported shape plus the
/// const-method and handle-receiver silent variants; the const-param call
/// site must error (exit 1) while the rest stays quiet.
#[test]
fn check_command_issue_repro_57_const_param_nonconst_method() {
    let (ok, stdout, stderr) = run_issue_repro("57-const-param-nonconst-method");
    assert!(
        stdout.contains("no matching signatures to 'SceneHandle::PayloadValid() const'"),
        "expected const-receiver error in stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(!ok, "error-severity fixture must exit 1; stdout={stdout:?}");
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

/// Game-verified 2026-08-23: a strict `[script] exports` file compiles
/// into CONSUMERS only — function bodies written there do NOT resolve in
/// the defining plugin's own module (game: "No matching symbol"). The
/// sibling caller must now error.
#[test]
fn check_command_issue_repro_59_exports_bodies_dont_resolve_internally() {
    let (ok, stdout, stderr) = run_issue_repro("59-exports-scope");
    assert!(
        stdout.contains("No matching symbol 'SendEvent'")
            || stdout.contains("undefined identifier `SendEvent`"),
        "expected unresolved SendEvent in stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(!ok, "must exit 1 (error severity); stdout={stdout:?}");
}

/// The CORRECT pattern stays clean: strict export file carries only
/// `import … from "T";` decls, bodies live in ordinary files,
/// shared_exports stay visible internally. Error-severity diagnostics
/// (undefined symbol / No matching symbol) must NOT appear; warnings
/// (e.g. the pre-existing unused-decl quirk on import/decl pairs,
/// verified identical on master 0f059f5) are out of scope.
#[test]
fn check_command_issue_repro_60_exports_correct_pattern_clean() {
    let (ok, stdout, stderr) = run_issue_repro("60-exports-correct-pattern");
    assert!(
        !stdout.contains("undefined identifier"),
        "no scoping errors expected; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("No matching symbol"),
        "no scoping errors expected; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("unknown type"),
        "shared exports must resolve; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(ok, "warnings-only plugin must exit 0; stdout={stdout:?}");
}

/// GH #58: a call to a namespace-qualified symbol that exists nowhere in
/// the workspace is a game compile error the LSP used to miss —
/// `No matching symbol 'TournamentStation::SendEvent'` (tournament-station,
/// 2026-08-23). The namespace is declared in a sibling file, so this covers
/// the cross-file half too. Every resolvable form under the same namespace
/// (function, class-as-type, value-type ctor, enum cast, funcdef-typed
/// global, plain global) must stay silent.
#[test]
fn check_command_issue_repro_58b_unknown_namespaced_symbol_diagnoses() {
    let (_, stdout, _) = run_issue_repro_typedb("58b-ns-unknown-symbol");
    let flagged = stdout.lines().any(|l| {
        l.contains("Main.as:7")
            && l.contains("error")
            && l.contains("No matching symbol 'TournamentStation::SendEvent'")
    });
    assert!(
        flagged,
        "expected a No-matching-symbol error on TournamentStation::SendEvent; stdout={stdout:?}"
    );
    for line in [
        "Main.as:8",
        "Main.as:9",
        "Main.as:10",
        "Main.as:11",
        "Main.as:12",
        "Main.as:13",
    ] {
        assert!(
            !stdout
                .lines()
                .any(|l| l.contains(line) && l.contains("error")),
            "resolvable namespace member on {line} must stay silent; stdout={stdout:?}"
        );
    }
}

/// GH #58, hint half: a near-name candidate under the same namespace is
/// named in the message, mirroring the typedb `UI::GetIO` rule. The
/// motivating repro has NO near name, so the hint is additive only —
/// never a gate on the diagnostic (B013 decision 2).
#[test]
fn check_command_issue_repro_58b_near_name_hint_names_candidate() {
    let (_, stdout, _) = run_issue_repro_typedb("58b-ns-unknown-symbol");
    let hinted = stdout.lines().any(|l| {
        l.contains("Main.as:14")
            && l.contains("No matching symbol 'Hints::Refrsh'")
            && l.contains("Hints::Refresh")
    });
    assert!(
        hinted,
        "expected a near-name hint naming Hints::Refresh; stdout={stdout:?}"
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.contains("Main.as:15") && l.contains("error")),
        "the correctly spelled call must stay silent; stdout={stdout:?}"
    );
}

/// GH #58 false-positive fences: AngelScript resolves a qualified call
/// RELATIVE to the enclosing namespace first (`Inner::Deep()` inside
/// `Outer` binds `Outer::Inner::Deep`), and only single-segment namespaces
/// are authoritative in this release (`Outer::Inner::Nope` stays silent).
#[test]
fn check_command_issue_repro_58c_relative_and_nested_namespaces_stay_silent() {
    let (_, stdout, _) = run_issue_repro_typedb("58c-ns-relative-nested");
    for line in [
        "Main.as:19",
        "Main.as:20",
        "Main.as:25",
        "Main.as:26",
        "Main.as:27",
    ] {
        assert!(
            !stdout
                .lines()
                .any(|l| l.contains(line) && l.contains("error")),
            "relative/nested namespace resolution on {line} must stay silent; stdout={stdout:?}"
        );
    }
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:28") && l.contains("No matching symbol 'Inner::Nope'"));
    assert!(
        flagged,
        "expected an error for Inner::Nope on the top-level namespace; stdout={stdout:?}"
    );
}

/// GH #58: the workspace half of the rule needs no typedb — but an ENGINE
/// namespace the plugin also adds helpers to (`namespace UI { … }`) must
/// stay silent without one, because only the typedb knows the real API
/// surface (`UI::Begin`).
#[test]
fn check_command_issue_repro_58d_engine_namespace_fence_without_typedb() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/issue-repros/58d-ns-engine-fence");
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg(&fixture)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in ["Main.as:14", "Main.as:15", "Main.as:16"] {
        assert!(
            !stdout
                .lines()
                .any(|l| l.contains(line) && l.contains("error")),
            "engine-namespace call on {line} must stay silent with no typedb; stdout={stdout:?}"
        );
    }
    let flagged = stdout
        .lines()
        .any(|l| l.contains("Main.as:17") && l.contains("No matching symbol 'Local::Nope'"));
    assert!(
        flagged,
        "expected an error for Local::Nope with no typedb loaded; stdout={stdout:?}"
    );
}

/// todo-cover-cases-next.txt (2026-08-25): `Stats::Current()` from inside
/// `namespace Tests` must diagnose. This was the TDD-breaking miss; it is
/// already fixed (GH #58). `Tests` itself is an engine namespace and stays
/// fenced — that is a different rule.
#[test]
fn check_command_stats_unknown_from_inside_tests_diagnoses() {
    let root = make_temp_plugin("stats-from-tests");
    fs::write(
        root.join("src/Main.as"),
        r#"
namespace Stats {
    int Existing() { return 1; }
}
namespace Tests {
    class Context {}
    void Probe(Tests::Context@ ctx) {
        auto @x = Stats::ThisFunctionDoesNotExistAtAll();
        int y = TotallyUndefinedGlobalFunction(3);
        Stats::Existing();
    }
}
void Main() {}
"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--unused")
        .arg("ignore")
        .arg(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No matching symbol 'Stats::ThisFunctionDoesNotExistAtAll'"),
        "Stats:: miss from inside Tests must diagnose; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("undefined identifier `TotallyUndefinedGlobalFunction`"),
        "unqualified miss must still diagnose; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("Stats::Existing"),
        "declared Stats::Existing must stay silent; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// todo-cover-cases-next.txt (2026-08-23): `check` on a plugin whose tree
/// contains a nested plugin root (its own info.toml — research fixtures,
/// .scratch copies) must not report that nested plugin's diagnostics.
/// `ignore_paths` / `.gitignore` can hide them today; the desired default
/// is to stop descending at a nested info.toml.
#[test]
fn check_command_skips_nested_plugin_roots() {
    let root = make_temp_plugin("nested-parent");
    fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
    fs::create_dir_all(root.join(".scratch/research/nested/src")).unwrap();
    fs::write(
        root.join(".scratch/research/nested/info.toml"),
        "[meta]\nname = \"Nested Research\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join(".scratch/research/nested/src/Main.as"),
        "void Broken() { MissingType@ x; }\nvoid Main() {}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--unused")
        .arg("ignore")
        .arg(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("MissingType"),
        "nested plugin diagnostics must not leak into the parent check; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("0 diagnostics"),
        "parent-only tree must be clean; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// GH #58 trigger (c): the namespace is declared only in a RESOLVED
/// dependency's export surface. That surface is authoritative for a
/// consumer (the game compiles exactly those files into it), so an unknown
/// member is a real `No matching symbol`; the exported one stays silent.
#[test]
fn check_command_unknown_member_of_dependency_namespace_diagnoses() {
    let base = std::env::temp_dir().join(format!("openplanet-lsp-dep-ns-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("deps/dep-plugin/src")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("deps/dep-plugin/info.toml"),
        "[meta]\nname = \"Dependency Plugin\"\nversion = \"0.1.0\"\n\n[script]\nmodule = \"DepPlugin\"\nexports = [\"Export.as\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("deps/dep-plugin/src/Export.as"),
        "namespace DepPlugin {\n    import void Hello() from \"DepPlugin\";\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n\n[script]\ndependencies = [\"DepPlugin\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/src/Main.as"),
        "void Main() {\n    DepPlugin::Hello();\n    DepPlugin::NotExported();\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("deps"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No matching symbol 'DepPlugin::NotExported'"),
        "expected an error for a member absent from the dep export surface; stdout={stdout:?}"
    );
    assert!(
        !stdout
            .lines()
            .any(|l| l.contains("Main.as:2") && l.contains("error")),
        "the exported member must stay silent; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// GH #58 authority guard (B013 decision 3): when a REQUIRED dependency
/// cannot be resolved, the workspace symbol set is not authoritative —
/// the missing dep may declare the very member being called (AngelScript
/// namespaces are open and additive across modules), so the rule stays
/// silent even though the namespace is declared locally.
#[test]
fn check_command_unknown_namespaced_symbol_silent_when_required_dep_missing() {
    let base = std::env::temp_dir().join(format!("openplanet-lsp-ns-nodep-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("plugins")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n\n[script]\ndependencies = [\"NoSuchDep\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/src/Main.as"),
        "namespace Shared {\n    void Known() {}\n}\n\nvoid Main() {\n    Shared::Known();\n    Shared::MaybeInTheMissingDep();\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("plugins"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("No matching symbol"),
        "an unresolved required dependency must silence the rule; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("NoSuchDep"),
        "the missing-dependency error itself must still be reported; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// GH #58 with the exports machinery in play: a namespace declared across a
/// strict `[script] exports` file, a `shared_exports` file and an ordinary
/// source file resolves for every member any of those contributes (the
/// own-module view keeps shared exports visible and the real body lives in
/// the ordinary file). Only members nothing contributes are errors.
#[test]
fn check_command_namespaced_symbol_across_export_files() {
    let base = std::env::temp_dir().join(format!("openplanet-lsp-ns-exp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(
        base.join("info.toml"),
        "[meta]\nname = \"ExportNs\"\nversion = \"0.1.0\"\n\n[script]\nmodule = \"T\"\nexports = [\"Exports_Decls.as\"]\nshared_exports = [\"Shared_Ns.as\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("src/Exports_Decls.as"),
        "namespace Api {\n    import void Send() from \"T\";\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("src/Shared_Ns.as"),
        "namespace Shared {\n    void Helper() {}\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("src/Main.as"),
        "namespace Api {\n    void Send() {}\n}\n\nvoid Main() {\n    Api::Send();\n    Shared::Helper();\n    Shared::Nope();\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg(&base)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No matching symbol 'Shared::Nope'"),
        "expected an error for the member nothing declares; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("No matching symbol 'Api::Send'")
            && !stdout.contains("No matching symbol 'Shared::Helper'"),
        "members contributed by any pooled file must resolve; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// GH #58: AngelScript namespaces are ADDITIVE across modules — a resolved
/// dependency's export surface and the consumer's own files both contribute
/// to `namespace Shared`, and a member from either side resolves. Only a
/// name neither declares is a `No matching symbol`.
#[test]
fn check_command_namespace_members_union_across_modules() {
    let base = std::env::temp_dir().join(format!("openplanet-lsp-ns-add-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("deps/dep-plugin/src")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("deps/dep-plugin/info.toml"),
        "[meta]\nname = \"Dependency Plugin\"\nversion = \"0.1.0\"\n\n[script]\nmodule = \"DepPlugin\"\nexports = [\"Export.as\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("deps/dep-plugin/src/Export.as"),
        "namespace Shared {\n    import void FromDep() from \"DepPlugin\";\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n\n[script]\ndependencies = [\"DepPlugin\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/src/Main.as"),
        "namespace Shared {\n    void FromOwn() {}\n}\n\nvoid Main() {\n    Shared::FromOwn();\n    Shared::FromDep();\n    Shared::FromNeither();\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("deps"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No matching symbol 'Shared::FromNeither'"),
        "expected an error for the member neither module declares; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("No matching symbol 'Shared::FromOwn'")
            && !stdout.contains("No matching symbol 'Shared::FromDep'"),
        "additive namespace members from either module must resolve; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// GH #58 authority guard, optional-dependency half — REWRITTEN by B019.
///
/// It used to assert that an uninstalled optional dep silences the namespace
/// rule entirely, which was correct while `apply_manifest` defined
/// `DEPENDENCY_<ID>` regardless: the guarded block stayed live against a
/// module we do not have, so firing anywhere in the file was unsafe.
///
/// B019 removed the cause instead of the symptom. The macro is no longer
/// defined, so the block is masked out exactly as the game masks it — and the
/// rule REGAINS authority over the rest of the file. Both halves are asserted:
/// nothing fires inside the (now dead) guard, and a genuinely unknown member
/// OUTSIDE it is reported again.
#[test]
fn check_command_unknown_namespaced_symbol_rearms_when_optional_dep_missing() {
    let base = std::env::temp_dir().join(format!("openplanet-lsp-ns-opt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("plugins")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        "[meta]\nname = \"OptFP\"\nversion = \"0.1.0\"\n\n[script]\noptional_dependencies = [\"MissingOpt\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/src/Main.as"),
        "namespace Shared {\n    void Own() {}\n}\n\nvoid Main() {\n    Shared::Own();\n    Shared::NotDeclaredAnywhere();\n#if DEPENDENCY_MISSINGOPT\n    Shared::FromTheOptionalDep();\n#endif\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("plugins"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Shared::FromTheOptionalDep"),
        "the `#if DEPENDENCY_MISSINGOPT` block must be masked out, exactly as \
         the game masks it; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("No matching symbol 'Shared::NotDeclaredAnywhere'"),
        "with the phantom define gone the pooled table IS the complete compile \
         surface, so the rule must re-arm for the rest of the file; \
         stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// GH #58 authority guard, transitive half: a resolved dependency names an
/// `export_dependency` that no plugins dir provides. Today that is only a
/// `tracing::warn!`, not a missing-required entry — but the unreadable
/// module may declare the member, so the rule must stay silent.
#[test]
fn check_command_unknown_namespaced_symbol_silent_when_nested_export_dep_missing() {
    let base = std::env::temp_dir().join(format!("openplanet-lsp-ns-nest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("deps/dep-plugin/src")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("deps/dep-plugin/info.toml"),
        "[meta]\nname = \"Dependency Plugin\"\nversion = \"0.1.0\"\n\n[script]\nmodule = \"DepPlugin\"\nexports = [\"Export.as\"]\nexport_dependencies = [\"NestedMissing\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("deps/dep-plugin/src/Export.as"),
        "namespace Shared {\n    import void FromDep() from \"DepPlugin\";\n}\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n\n[script]\ndependencies = [\"DepPlugin\"]\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/src/Main.as"),
        "namespace Shared {\n    void Own() {}\n}\n\nvoid Main() {\n    Shared::Own();\n    Shared::MaybeInNestedMissing();\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("deps"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("No matching symbol"),
        "an unresolved nested export_dependency must silence the rule; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// GH #58 decision 4: every declaration kind a namespace can contribute
/// participates in its additive member surface. In particular, a typedef is
/// both legal AngelScript syntax and a type name that a namespaced cast must
/// resolve without a parser error or a `No matching symbol` diagnostic.
#[test]
fn check_command_namespaced_typedef_resolves_without_parse_or_symbol_error() {
    let root = make_temp_plugin("namespace-typedef");
    fs::write(
        root.join("src/Main.as"),
        "namespace Names {\n    typedef int Alias;\n}\n\nvoid Main() {\n    Names::Alias(0);\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "a namespaced typedef cast must be clean; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("parse error") && !stdout.contains("No matching symbol 'Names::Alias'"),
        "typedef must parse and resolve as a namespace member; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Write a plugin whose `.gitignore` line is caller-chosen: `generated/Gen.as`
/// contributes `Shared::FromGenerated`, `src/Main.as` calls it plus a member
/// nothing declares. Shared by the GH #58 × GH #60 authority triplet.
fn gitignore_authority_plugin(name: &str, gitignore: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("openplanet-lsp-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("generated")).unwrap();
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"GitignoreAuthority\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), gitignore).unwrap();
    fs::write(
        root.join("generated/Gen.as"),
        "namespace Shared {\n    void FromGenerated() {}\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/Main.as"),
        "namespace Shared {\n    void Own() {}\n}\n\nvoid Main() {\n    Shared::Own();\n    Shared::FromGenerated();\n    Shared::DeclaredNowhere();\n}\n",
    )
    .unwrap();
    root
}

fn check_plugin(root: &std::path::Path, extra_args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
    cmd.env("NO_COLOR", "1").arg("check").arg("--no-typedb");
    for arg in extra_args {
        cmd.arg(arg);
    }
    let output = cmd.arg(root).output().unwrap();
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// GH #58 × GH #60: gitignore honoring drops `.as` files at DISCOVERY time,
/// before any `source_paths` / `ignore_paths` filter runs. The game compiles
/// those files regardless — gitignore is a git concept, not a compile concept
/// — so a namespace member declared only in a gitignored file is legal, and
/// the pooled table is no longer an authoritative surface for that namespace.
/// The whole rule must go silent for the plugin.
#[test]
fn check_command_namespaced_symbol_silent_when_gitignore_excluded_a_source() {
    let root = gitignore_authority_plugin("gitignore-authority", "generated/\n");
    let stdout = check_plugin(&root, &[]);
    assert!(
        !stdout.contains("No matching symbol"),
        "a gitignore-excluded own-module file must silence the rule; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// The same plugin with `--no-gitignore`: the generated file is discovered, so
/// `Shared::FromGenerated` resolves for the ordinary reason AND the surface is
/// authoritative again — the member nothing declares still errors. This is the
/// non-vacuity half of the fence.
#[test]
fn check_command_namespaced_symbol_armed_when_gitignore_honoring_is_disabled() {
    let root = gitignore_authority_plugin("gitignore-disabled", "generated/\n");
    let stdout = check_plugin(&root, &["--no-gitignore"]);
    assert!(
        !stdout.contains("No matching symbol 'Shared::FromGenerated'"),
        "the discovered generated declaration must resolve; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("No matching symbol 'Shared::DeclaredNowhere'"),
        "with nothing excluded the rule must stay armed; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Honoring gitignore is the DEFAULT, so "honoring is on" cannot be the
/// disarm condition — that would silence the rule in nearly every repo. Only
/// an actual exclusion breaks authority: a `.gitignore` that matches no `.as`
/// file leaves the rule armed.
#[test]
fn check_command_namespaced_symbol_armed_when_gitignore_excludes_nothing() {
    let root = gitignore_authority_plugin("gitignore-noop", "*.log\nbuild/\n");
    let stdout = check_plugin(&root, &[]);
    assert!(
        stdout.contains("No matching symbol 'Shared::DeclaredNowhere'"),
        "a gitignore that excludes no .as file must leave the rule armed; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("No matching symbol 'Shared::FromGenerated'"),
        "the discovered generated declaration must resolve; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// GH #61 helper: a plugin whose ONLY caller of `UsedOnlyByOmitted` lives in
/// `omitted/`, a dir the checked set drops. `gitignore` and `config` are
/// written verbatim when non-empty (`.gitignore` / `.openplanet-lsp.toml`),
/// so the same tree exercises the gitignore route and the `ignore_paths` /
/// `source_paths` routes. `extra_omitted` is appended to the omitted file.
fn omitted_caller_plugin(
    name: &str,
    gitignore: &str,
    config: &str,
    extra_omitted: &str,
) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("openplanet-lsp-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("omitted")).unwrap();
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"OmittedCaller\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    if !gitignore.is_empty() {
        fs::write(root.join(".gitignore"), gitignore).unwrap();
    }
    if !config.is_empty() {
        fs::write(root.join(".openplanet-lsp.toml"), config).unwrap();
    }
    fs::write(
        root.join("src/Main.as"),
        "void UsedOnlyByOmitted() {}\nvoid Main() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("omitted/Gen.as"),
        format!("void GenCaller() {{ UsedOnlyByOmitted(); }}\n{extra_omitted}"),
    )
    .unwrap();
    root
}

/// GH #61 repro 1 (gitignore route, no config needed since GH #60). The game
/// compiles `omitted/Gen.as`, so `UsedOnlyByOmitted` is used and the
/// "never used" warning is a false positive.
#[test]
fn check_command_unused_silent_when_the_only_caller_is_gitignored() {
    let root = omitted_caller_plugin("unused-gitignored-caller", "omitted/\n", "", "");
    let stdout = check_plugin(&root, &[]);
    assert!(
        !stdout.contains("UsedOnlyByOmitted"),
        "a gitignored caller still counts as usage; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// GH #61 repro 2 (`ignore_paths` route) — proof the defect PREDATES GH #60:
/// no gitignore anywhere, and the same false positive appears.
#[test]
fn check_command_unused_silent_when_the_only_caller_is_in_ignore_paths() {
    let root = omitted_caller_plugin(
        "unused-ignorepaths-caller",
        "",
        "ignore_paths = [\"omitted\"]\n",
        "",
    );
    let stdout = check_plugin(&root, &[]);
    assert!(
        !stdout.contains("UsedOnlyByOmitted"),
        "an ignore_paths caller still counts as usage; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Variant: the `source_paths` allowlist route.
#[test]
fn check_command_unused_silent_when_the_only_caller_is_outside_source_paths() {
    let root = omitted_caller_plugin(
        "unused-sourcepaths-caller",
        "",
        "source_paths = [\"src\"]\n",
        "",
    );
    let stdout = check_plugin(&root, &[]);
    assert!(
        !stdout.contains("UsedOnlyByOmitted"),
        "a caller outside source_paths still counts as usage; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// NON-VACUITY: the pass must still warn when the omitted files really do
/// not use the declaration. Without this the fix could be "stop warning
/// whenever anything was omitted" — refuted alternative 2 on GH #61.
#[test]
fn check_command_unused_still_warns_when_omitted_files_do_not_use_it() {
    let root = omitted_caller_plugin("unused-still-warns", "omitted/\n", "", "");
    fs::write(
        root.join("src/Main.as"),
        "void NobodyCallsThis() {}\nvoid UsedOnlyByOmitted() {}\nvoid Main() {}\n",
    )
    .unwrap();
    let stdout = check_plugin(&root, &[]);
    assert!(
        stdout.contains("`NobodyCallsThis` is never used"),
        "a genuinely unused declaration must still warn; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("UsedOnlyByOmitted"),
        "the omitted caller's usage must still be credited; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// GUARD: the GH #60 goal must survive — a file the author believes is
/// excluded gets NO squiggles of its own, not even from the errors and dead
/// declarations it contains.
#[test]
fn check_command_reports_no_diagnostics_inside_an_omitted_file() {
    let root = omitted_caller_plugin(
        "unused-omitted-silent",
        "omitted/\n",
        "",
        "void GenDead() {}\nvoid GenBroken() { int x = \"not an int\"; }\n",
    );
    let stdout = check_plugin(&root, &[]);
    assert!(
        !stdout.contains("Gen.as"),
        "an omitted file must produce no diagnostics of its own; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("GenDead") && !stdout.contains("GenCaller"),
        "an omitted file's declarations must never be reported; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

/// `--no-gitignore` is unchanged: the file is CHECKED, so `GenCaller` is a
/// real declaration nothing calls and is reported — exactly what the GH #61
/// repro observed on that flag.
#[test]
fn check_command_unused_reports_the_omitted_caller_with_no_gitignore() {
    let root = omitted_caller_plugin("unused-no-gitignore", "omitted/\n", "", "");
    let stdout = check_plugin(&root, &["--no-gitignore"]);
    assert!(
        stdout.contains("`GenCaller` is never used"),
        "with honoring off the discovered file is checked; stdout={stdout:?}"
    );
    assert!(
        !stdout.contains("UsedOnlyByOmitted"),
        "the call is visible either way; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(root);
}

fn unused_kinds_plugin(name: &str) -> std::path::PathBuf {
    let root = make_temp_plugin(name);
    fs::write(
        root.join("src/Main.as"),
        r#"
void DeadFn() {}
int deadVar = 1;
class DeadClass {
    void DeadMethod() {}
}
void Main() {
    int deadLocal = 0;
}
"#,
    )
    .unwrap();
    root
}

fn check_plugin_status(root: &std::path::Path, extra_args: &[&str]) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"));
    cmd.env("NO_COLOR", "1").arg("check").arg("--no-typedb");
    for arg in extra_args {
        cmd.arg(arg);
    }
    let output = cmd.arg(root).output().unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
    )
}

fn unused_mentions(stdout: &str) -> bool {
    stdout.contains("`DeadFn`")
        || stdout.contains("`deadVar`")
        || stdout.contains("`DeadClass`")
        || stdout.contains("`DeadMethod`")
        || stdout.contains("`deadLocal`")
}

/// Default unused level is warn: findings print as warnings and do not fail
/// the run. `--unused ignore` / `--no-unused` drop every kind. `--unused
/// error` promotes them to errors (exit 1 without -Werror).
#[test]
fn check_command_unused_flag_ignore_warn_error_variants() {
    let root = unused_kinds_plugin("unused-levels");

    let (code, stdout) = check_plugin_status(&root, &[]);
    assert_eq!(code, Some(0), "warnings-only is exit 0: {stdout}");
    assert!(
        unused_mentions(&stdout),
        "default warn must report: {stdout}"
    );
    assert!(
        stdout.contains("warning:"),
        "default unused must be warnings: {stdout}"
    );

    for flag in ["--no-unused", "--unused=ignore", "--unused=off"] {
        let (code, stdout) = check_plugin_status(&root, &[flag]);
        assert_eq!(code, Some(0), "{flag} exit: {stdout}");
        assert!(
            !unused_mentions(&stdout),
            "{flag} must silence unused function/var/class/local: {stdout}"
        );
    }

    let (code, stdout) = check_plugin_status(&root, &["--unused=error"]);
    assert_eq!(code, Some(1), "unused=error must fail check: {stdout}");
    assert!(unused_mentions(&stdout), "error still reports: {stdout}");
    assert!(
        stdout.contains("error:") && stdout.contains("`DeadFn`"),
        "unused=error must emit error severity: {stdout}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// B030: `hint` / `info` report the same findings at a quieter severity —
/// exit 0, labelled `hint:` / `info:`, counted as "other" rather than
/// warnings, and NOT promoted by `--warnings-as-errors` (which only
/// promotes WARNING).
#[test]
fn check_command_unused_quiet_levels_are_reported_but_never_fail() {
    let root = unused_kinds_plugin("unused-quiet-levels");

    for (flag, label) in [
        ("--unused=hint", "hint:"),
        ("--unused=grey", "hint:"),
        ("--unused=info", "info:"),
        ("--unused=note", "info:"),
    ] {
        let (code, stdout) = check_plugin_status(&root, &[flag]);
        assert_eq!(code, Some(0), "{flag} must not fail the run: {stdout}");
        assert!(
            unused_mentions(&stdout),
            "{flag} must still report the findings: {stdout}"
        );
        assert!(
            stdout.contains(label),
            "{flag} must label findings `{label}`: {stdout}"
        );
        assert!(
            !stdout.contains("warning:"),
            "{flag} must not emit warning-severity findings: {stdout}"
        );
        assert!(
            stdout.contains("other"),
            "quiet findings count as `other` in the summary: {stdout}"
        );
    }

    // -Werror promotes WARNING only; the quiet levels stay non-fatal.
    for flag in ["--unused=hint", "--unused=info"] {
        let (code, stdout) = check_plugin_status(&root, &[flag, "--warnings-as-errors"]);
        assert_eq!(code, Some(0), "-Werror must not promote {flag}: {stdout}");
    }
    let (code, stdout) = check_plugin_status(&root, &["--unused=warn", "--warnings-as-errors"]);
    assert_eq!(
        code,
        Some(1),
        "-Werror must still promote warn-level unused: {stdout}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// B031 design call 1: greying out preprocessor-dead `#if` branches is an
/// LSP-only affordance. `check` must NEVER emit it — a plugin with a few
/// hundred `#if MP4` lines would drown the CLI output, and HINT severity does
/// not save us because `check` prints hints. This test is the guard on that
/// boundary: the config key exists and is honoured by the server, but no
/// value of it may leak an inactive-region diagnostic into `check`.
#[test]
fn check_command_never_reports_inactive_regions() {
    // NB: the fixture name must not contain the word this test greps
    // for — the temp dir path is printed in the summary line.
    let root = make_temp_plugin("dead-branch-cli");
    fs::write(
        root.join("src/Main.as"),
        "#if MP4\nvoid DeadForNext() { MissingTypeInDeadBranch@ x; }\n#endif\nvoid Main() {}\n",
    )
    .unwrap();

    // Default (hint), and every explicit level, including via the config file.
    for pin in [
        "",
        "inactive_regions = \"hint\"\n",
        "inactive_regions = \"info\"\n",
    ] {
        if pin.is_empty() {
            let _ = fs::remove_file(root.join(".openplanet-lsp.toml"));
        } else {
            fs::write(root.join(".openplanet-lsp.toml"), pin).unwrap();
        }
        let (code, stdout) = check_plugin_status(&root, &[]);
        assert_eq!(code, Some(0), "pin={pin:?} must stay clean: {stdout}");
        assert!(
            !stdout.contains("inactive code"),
            "pin={pin:?} must not leak an inactive-region diagnostic: {stdout}"
        );
        // The dead branch is still dead: its contents must not be analysed.
        assert!(
            !stdout.contains("MissingTypeInDeadBranch"),
            "pin={pin:?} dead branch must stay unanalysed: {stdout}"
        );
        assert!(
            stdout.contains("0 diagnostics"),
            "pin={pin:?} expected a clean run: {stdout}"
        );
    }

    let _ = fs::remove_dir_all(&root);
}

/// GH #62: `check --typedb-dir --no-typedb <path>` must be a usage error
/// (exit 2), not a load failure for `--no-typedb/OpenplanetCore.json`.
#[test]
fn check_command_typedb_dir_swallowing_following_flag_is_usage_error() {
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tm-counter");
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--typedb-dir")
        .arg("--no-typedb")
        .arg(&fixture)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "swallowed --no-typedb must be usage (exit 2); stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stderr.contains("--typedb-dir requires a directory argument"),
        "expected the flag-swallowed usage message; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("OpenplanetCore.json"),
        "must not attempt to load --no-typedb/OpenplanetCore.json; stderr={stderr:?}"
    );
}

/// Workspace `unused = "ignore"` silences the pass; `--unused error` still
/// wins over that pin.
#[test]
fn check_command_unused_config_and_flag_precedence() {
    let root = unused_kinds_plugin("unused-config");
    fs::write(root.join(".openplanet-lsp.toml"), "unused = \"ignore\"\n").unwrap();

    let (_, stdout) = check_plugin_status(&root, &[]);
    assert!(
        !unused_mentions(&stdout),
        "workspace unused=ignore must silence: {stdout}"
    );

    let (code, stdout) = check_plugin_status(&root, &["--unused", "error"]);
    assert_eq!(code, Some(1), "flag must beat config: {stdout}");
    assert!(
        unused_mentions(&stdout),
        "flag error still reports: {stdout}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn check_help_mentions_unused_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "check --help: {stdout}");
    assert!(
        stdout.contains("--unused") && stdout.contains("--no-unused"),
        "help must document unused flags: {stdout}"
    );
}

/// Plugin whose analysis order is warning, error, warning — so last / first /
/// off are distinguishable.
fn mixed_severity_plugin(name: &str) -> std::path::PathBuf {
    let root = make_temp_plugin(name);
    fs::write(root.join("src/Alpha.as"), "void AlphaDead() {}\n").unwrap();
    fs::write(
        root.join("src/Beta.as"),
        "void Main() {\n    MissingType@ bad;\n}\n",
    )
    .unwrap();
    fs::write(root.join("src/Gamma.as"), "void GammaDead() {}\n").unwrap();
    root
}

const MIXED_WARN_A: &str = "`AlphaDead`";
const MIXED_ERR: &str = "`MissingType`";
const MIXED_WARN_B: &str = "`GammaDead`";

fn mixed_marker_order(stdout: &str) -> Vec<&'static str> {
    let mut hits: Vec<(usize, &'static str)> = [MIXED_WARN_A, MIXED_ERR, MIXED_WARN_B]
        .into_iter()
        .filter_map(|m| stdout.find(m).map(|i| (i, m)))
        .collect();
    hits.sort_by_key(|(i, _)| *i);
    hits.into_iter().map(|(_, m)| m).collect()
}

fn json_marker_order(stdout: &str) -> Vec<String> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap();
    parsed["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["message"].as_str())
        .filter_map(|msg| {
            [MIXED_WARN_A, MIXED_ERR, MIXED_WARN_B]
                .into_iter()
                .find(|m| msg.contains(m))
                .map(|m| m.to_string())
        })
        .collect()
}

/// Default human output (plain + pretty) prints errors after warnings.
/// `--errors first` / `off` flip or preserve analysis order. JSON never
/// regroups. Config is honored; the flag wins. Trailer stays last.
#[test]
fn check_command_groups_errors_last_first_off_plain_pretty_json() {
    let root = mixed_severity_plugin("errors-group");
    let analysis = [MIXED_WARN_A, MIXED_ERR, MIXED_WARN_B];
    let last = [MIXED_WARN_A, MIXED_WARN_B, MIXED_ERR];
    let first = [MIXED_ERR, MIXED_WARN_A, MIXED_WARN_B];

    for format in ["plain", "pretty"] {
        let (code, stdout) = check_plugin_status(&root, &["--format", format]);
        assert_eq!(code, Some(1), "default {format} exit: {stdout}");
        assert_eq!(
            mixed_marker_order(&stdout),
            last,
            "default {format} is errors-last: {stdout}"
        );
        assert!(
            stdout.contains(ISSUE_ASK_TRAILER),
            "default {format} trailer: {stdout}"
        );
        assert!(
            stdout.trim_end().ends_with(ISSUE_ASK_TRAILER),
            "default {format} trailer is last: {stdout}"
        );

        let (code, stdout) = check_plugin_status(&root, &["--format", format, "--errors", "first"]);
        assert_eq!(code, Some(1), "{format} --errors first: {stdout}");
        assert_eq!(
            mixed_marker_order(&stdout),
            first,
            "{format} --errors first: {stdout}"
        );

        let (code, stdout) = check_plugin_status(&root, &["--format", format, "--errors=off"]);
        assert_eq!(code, Some(1), "{format} --errors=off: {stdout}");
        assert_eq!(
            mixed_marker_order(&stdout),
            analysis,
            "{format} --errors=off keeps analysis order: {stdout}"
        );
        assert!(
            stdout.trim_end().ends_with(ISSUE_ASK_TRAILER),
            "{format} off trailer last: {stdout}"
        );
    }

    let (_, json) = check_plugin_status(&root, &["--format", "json"]);
    assert_eq!(
        json_marker_order(&json),
        analysis,
        "default json stays analysis order: {json}"
    );
    let (_, json) = check_plugin_status(&root, &["--format=json", "--errors=first"]);
    assert_eq!(
        json_marker_order(&json),
        analysis,
        "json ignores --errors first: {json}"
    );
    let (_, json) = check_plugin_status(&root, &["--format=json", "--errors=last"]);
    assert_eq!(
        json_marker_order(&json),
        analysis,
        "json ignores --errors last: {json}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// Workspace `errors = "first"` is honored; `--errors last` / `off` win.
#[test]
fn check_command_errors_config_and_flag_precedence() {
    let root = mixed_severity_plugin("errors-config");
    fs::write(root.join(".openplanet-lsp.toml"), "errors = \"first\"\n").unwrap();

    let (_, stdout) = check_plugin_status(&root, &["--format", "plain"]);
    assert_eq!(
        mixed_marker_order(&stdout),
        [MIXED_ERR, MIXED_WARN_A, MIXED_WARN_B],
        "workspace errors=first: {stdout}"
    );

    let (_, stdout) = check_plugin_status(&root, &["--format", "plain", "--errors", "last"]);
    assert_eq!(
        mixed_marker_order(&stdout),
        [MIXED_WARN_A, MIXED_WARN_B, MIXED_ERR],
        "flag last beats config first: {stdout}"
    );

    let (_, stdout) = check_plugin_status(&root, &["--format", "plain", "--errors=off"]);
    assert_eq!(
        mixed_marker_order(&stdout),
        [MIXED_WARN_A, MIXED_ERR, MIXED_WARN_B],
        "flag off beats config first: {stdout}"
    );

    fs::write(root.join(".openplanet-lsp.toml"), "errors = \"off\"\n").unwrap();
    let (_, stdout) = check_plugin_status(&root, &["--format", "plain"]);
    assert_eq!(
        mixed_marker_order(&stdout),
        [MIXED_WARN_A, MIXED_ERR, MIXED_WARN_B],
        "workspace errors=off: {stdout}"
    );

    fs::write(root.join(".openplanet-lsp.toml"), "errors = \"end\"\n").unwrap();
    let (_, stdout) = check_plugin_status(&root, &["--format", "pretty"]);
    assert_eq!(
        mixed_marker_order(&stdout),
        [MIXED_WARN_A, MIXED_WARN_B, MIXED_ERR],
        "workspace errors=end alias is last: {stdout}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn check_help_mentions_errors_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "check --help: {stdout}");
    assert!(
        stdout.contains("--errors") && stdout.contains("last") && stdout.contains("first"),
        "help must document --errors last|first|off: {stdout}"
    );
}

/// B016: members typed by an inline enum the engine dump leaves unnamed.
///
/// Those enums used to collapse onto one `<Module>::UnnamedEnum` key per
/// module — `Control` alone lost 14 of 15 — and B016 keys them by their
/// declaring context instead. The game exposes such a member as a plain
/// integer (the enum has no name, so no source can spell it), so re-keying
/// must not let the checker start rejecting reads, writes, comparisons
/// across two formerly-collided enums, or argument passing.
///
/// Game ground truth (RemoteBuild probe 2026-08-30, OP 1.29.5): everything
/// down to `Main` compiles clean — reads, writes, comparisons across two
/// formerly-collided enums and argument passing are all legal on a member
/// whose type is an unnamed enum. The LSP must agree, and does.
///
/// B044 (this assertion's other half): the same probe reports 3 ERRs
/// (`No matching symbol`) on the three lines of `BareEnumValueNames`, where
/// the LSP used to be silent. It is not any more — the bare-value scan is
/// narrowed to enums a source can name. The game stops at the first bad
/// symbol in a statement and so reports 3; the LSP checks every identifier
/// and reports all 8. Same verdict, finer granularity.
#[test]
fn check_command_issue_repro_b016_anon_member_enums_diagnoses_only_bare_value_names() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b016-anon-member-enums");
    let undefined: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("undefined identifier"))
        .collect();
    assert_eq!(
        undefined.len(),
        8,
        "only the 8 bare unnamed-enum value names may be diagnosed; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    for name in [
        "HyperTnL",
        "SlideNoControl",
        "NotTimed",
        "RenderPlaneReflect",
        "Download_WaitingSources",
        "MinimalReadOnly",
        "TimeHhMmSsMil",
        "Text_Icon",
    ] {
        assert!(
            undefined.iter().any(|l| l.contains(name)),
            "`{name}` must be diagnosed; stdout={stdout:?}"
        );
    }
    assert!(
        stdout.contains("8 diagnostics (8 errors)"),
        "member access, comparison, write and argument passing must stay silent; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B044: the exhaustive probe. Every value name of every anonymous member
/// enum in the shipped type DB that does NOT also belong to a named enum —
/// 258 of them, generated by `scripts/gen_b044_probe.py`, one statement per
/// line so the game reports one error per name.
///
/// Game ground truth (RemoteBuild probe 2026-08-30, OP 1.29.5): all 258
/// rejected with `No matching symbol`, and the LSP errors on all 258. The
/// probe agrees with the game exactly; there is no residual.
///
/// It did not always. The list was 261 names until B046, and 3 of them —
/// `DoubleJump`, `Jump` and `TechJump` — were false positives the fixture
/// carried as `KNOWN FP`. They are values of `ESceneCharVisNotice`, a
/// MODULE-LEVEL engine enum the Nadeo dump cannot represent (its `ns` map
/// holds types only, and no member anywhere is typed by it, so not even the
/// inline-enum route reaches it). B046 taught the loader to read
/// `Openplanet.h`, which names it; the 3 now resolve legitimately and the
/// generator excludes them, because they belong to a NAMED enum after all.
#[test]
fn check_command_issue_repro_b044_anon_enum_value_names_all_error() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b044-anon-enum-value-names");
    let undefined: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("undefined identifier"))
        .collect();
    assert_eq!(
        undefined.len(),
        258,
        "every bare anonymous-enum value name must be diagnosed; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    // B046: the three names the game ACCEPTS must no longer be diagnosed,
    // and must no longer be in the probe at all — the generator drops them
    // because the header names their enum. Pinned in the negative so a
    // regression that re-hides `Openplanet.h` fails here.
    for name in ["DoubleJump", "Jump", "TechJump"] {
        assert!(
            !undefined.iter().any(|l| l.contains(&format!("`{name}`"))),
            "`{name}` is a module-level enum value the game accepts; it must \
             not be diagnosed (B046); stdout={stdout:?}"
        );
    }
    assert!(
        stdout.contains("258 diagnostics (258 errors)"),
        "the probe must produce nothing but the undefined identifiers; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B044: the probe fixture is generated, and it is only ground truth if it
/// still matches the type DB it was generated from. Regenerate with
/// `scripts/gen_b044_probe.py` after a `typedb refresh`.
///
/// This guards the whole file — header counts and comments included. The
/// load-bearing half, that the fixture lists exactly the set the checker
/// narrows, is asserted in Rust by
/// `global_scope::tests::the_narrowing_touches_only_anonymous_only_value_names`.
///
/// A missing `python3` FAILS rather than skips: a guard that quietly
/// disappears on some machines is worse than no guard, and every runner this
/// project builds on has it (`scripts/` is Python throughout).
#[test]
fn b044_probe_fixture_matches_the_shipped_type_db() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("python3")
        .arg(repo.join("scripts/gen_b044_probe.py"))
        .arg("--check")
        .current_dir(&repo)
        .output()
        .expect("python3 must be available to check the generated B044 fixture");
    assert!(
        output.status.success(),
        "tests/fixtures/issue-repros/b044-anon-enum-value-names/Main.as is stale; \
         regenerate with scripts/gen_b044_probe.py. stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// B046: MODULE-LEVEL engine enums must resolve — the false-positive half of
/// the enum story, and the more damaging half, because the LSP rejects code
/// the game compiles.
///
/// The Nadeo dump's `ns` map is namespace -> type name -> type, so it can only
/// carry an enum nested in a class. It reaches most module-level enums anyway,
/// because a member typed by one carries the enum inline. Two —
/// `ESceneCharVisNotice` and `ESceneVehicleVisEvent` — have no such member
/// anywhere, so `Openplanet.h` is the only source that names them. Between
/// them they carry 55 value names that resolved nowhere.
///
/// Game ground truth (RemoteBuild probe, OP 1.29.5, 2026-08-30, re-probed
/// the same day for B053): every statement in the fixture compiles except the
/// four `SharedValues_*` ones, which the game rejects as AMBIGUOUS ("Found
/// multiple matching enum values") rather than unknown. The LSP reports the
/// same four statements, line for line.
///
/// B046 landed with those four SILENT and said so in the fixture, because
/// modelling the ambiguity needed its own probe and its own ticket. B053 is
/// that ticket, so the count here is 4 rather than 0. B046's own rule is
/// untouched: the names still RESOLVE, and what they now get is an ambiguity
/// error naming the candidate enums — not `No matching symbol`, which is what
/// a regression that lost the module-level enums would produce.
#[test]
fn check_command_issue_repro_b046_module_level_enums_resolve() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b046-module-level-enums");
    assert!(
        stdout.contains("4 diagnostics (4 errors)"),
        "only the four ambiguous shared values may be diagnosed; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    // Exactly the game's four, and each as AMBIGUITY, not as unknown.
    let ambiguous: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("ambiguous enum value"))
        .collect();
    assert_eq!(
        ambiguous.len(),
        4,
        "the four shared values must be diagnosed as ambiguous; stdout={stdout:?}"
    );
    for name in ["WallJump", "Turbo", "FreeWheeling"] {
        assert!(
            ambiguous.iter().any(|l| l.contains(&format!("`{name}`"))),
            "`{name}` is ambiguous in-game and must say so; stdout={stdout:?}"
        );
    }
    assert!(
        !stdout.contains("undefined identifier"),
        "an ambiguous name still RESOLVES; `No matching symbol` here would mean \
         the module-level enums were lost again; stdout={stdout:?}"
    );
    // Unambiguous module-level values, and the enum name used as a type,
    // must still be silent. Checked as a DIAGNOSED name rather than as a
    // substring: `ESceneCharVisNotice` now appears legitimately inside the
    // ambiguity messages, as one of the candidate enums it names.
    for name in [
        "DoubleJump",
        "TechJump",
        "WallBounce",
        "ESceneCharVisNotice",
    ] {
        assert!(
            !stdout
                .lines()
                .any(|l| l.contains(&format!("error: unknown type `{name}`"))
                    || l.contains(&format!("error: undefined identifier `{name}`"))
                    || l.contains(&format!("error: ambiguous enum value `{name}`"))),
            "`{name}` must not be diagnosed; stdout={stdout:?}"
        );
    }
}

/// B052: a qualified path to a global engine enum, both directions.
///
/// AngelScript looks `T` up in scope `P` and then in P's ancestors out to the
/// global namespace, so `P::T` resolves for a global enum `T` behind ANY real
/// scope — and for no other prefix, because a path through something that is
/// not a scope never starts the walk.
///
/// Game ground truth (RemoteBuild probe, OP 1.29.5, 2026-08-30): the fixture
/// was compiled in-game as one plugin and produced exactly the 77 errors of
/// `Rejected()` and nothing else. `Accepted()` is silent in-game and must be
/// silent here — deleting the fallback instead of narrowing it would have
/// turned all 103 of those into false POSITIVES.
///
/// Before B052 the LSP diagnosed 2 of the 77: `EMenuOrder` is not a complete
/// index key, so the tail fallback never fired for the two names ending in
/// it. The other 75 were silent — the false negatives this ticket is about.
///
/// The 3 in `KnownParserGap()` are a PRE-EXISTING false positive the
/// merge-base binary emits identically: the lexer treats `string` as a
/// keyword, so `string::VirtualKey` never parses. Pinned so the gap stays
/// visible and a future parser fix is told to update the fixture.
#[test]
fn check_command_issue_repro_b052_qualified_enum_paths() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b052-qualified-enum-paths");

    let unknown_type: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("unknown type"))
        .collect();
    assert_eq!(
        unknown_type.len(),
        77,
        "every path the game rejects must be diagnosed; \
         stdout={stdout:?} stderr={stderr:?}"
    );

    // Spot-check both halves of the rejection rule by name, so a fix that
    // merely changes the count cannot pass.
    for rejected in [
        "Scene::ESceneCharVisNotice",
        "Enums::ESceneCharVisNotice",
        "Game::VirtualKey",
        "Game::CGameMenu::VirtualKey",
        "CSceneVehicleVis::VirtualKey",
        "Meta::Plugin::VirtualKey",
        "CGameHighScore::EMenuOrder",
    ] {
        // Backticked: the diagnostic renders the name inside backticks, and
        // an unanchored substring would let `Game::CGameMenu::VirtualKey`
        // satisfy a check for `CGameMenu::VirtualKey`.
        let quoted = format!("`{rejected}`");
        assert!(
            unknown_type.iter().any(|l| l.contains(&quoted)),
            "`{rejected}` is rejected by the game and must be diagnosed; \
             stdout={stdout:?}"
        );
    }

    // The fence: a real scope's qualifier is not an error.
    for accepted in [
        "UI::VirtualKey",
        "Meta::ESceneCharVisNotice",
        "UI::Font::VirtualKey",
        "CGameMenu::VirtualKey",
        "CGameCtnApp::ESceneVehicleVisEvent",
        "CGameMenu::EMenuOrder",
        "CGameCtnBlock::ECardinalDirections",
    ] {
        let quoted = format!("`{accepted}`");
        assert!(
            !unknown_type.iter().any(|l| l.contains(&quoted)),
            "the game accepts `{accepted}`; diagnosing it would be a false \
             positive; stdout={stdout:?}"
        );
    }

    assert!(
        stdout.contains("80 diagnostics (80 errors)"),
        "77 rejected paths plus the 3 pre-existing `string::` parse errors, \
         and nothing else; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B053: a bare enum value two globally visible enums carry.
///
/// The game resolves a bare enum-value name against the enums in scope and
/// refuses to choose when two of them carry it: `Found multiple matching enum
/// values`. openplanet-lsp took the first match and said nothing.
///
/// Game ground truth (RemoteBuild, OP 1.29.5, 2026-08-30): all 2989 bare
/// value names of the shipped DB's named enums were submitted individually —
/// 2373 `No matching symbol`, 592 accepted, 22 ambiguous, 2 resolving to a
/// non-enum symbol. This fixture then compiled in-game as one plugin and
/// produced exactly 22 errors, on exactly the `Ambiguous()` statements, line
/// for line with what the LSP reports here.
///
/// The ticket predicted this set from "carried by two or more NAMED enums",
/// which is 758 names — 34x too many, because nested and namespaced enums are
/// not in scope for a bare name at all. Only enums the game puts in the
/// GLOBAL namespace take part.
///
/// `Unambiguous()` is the fence: 40 names carried by exactly one such enum,
/// which the game compiles and the LSP must not touch.
#[test]
fn check_command_issue_repro_b053_ambiguous_enum_values() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b053-ambiguous-enum-values");
    let ambiguous: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("ambiguous enum value"))
        .collect();
    assert_eq!(
        ambiguous.len(),
        22,
        "every name the game rejects as ambiguous must be diagnosed; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    // The message has to name the candidates, or "qualify it" is not
    // actionable advice.
    assert!(
        ambiguous.iter().any(|l| l.contains("`Turbo`")
            && l.contains("ESceneCharVisNotice")
            && l.contains("ESceneVehicleVisEvent")),
        "the message must name the candidate enums; stdout={stdout:?}"
    );
    assert!(
        stdout.contains("22 diagnostics (22 errors)"),
        "the 40 unambiguous names and the 4 qualified ones must stay silent; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B053, second half: how a plugin's OWN declarations meet the engine enums.
///
/// Game ground truth (RemoteBuild, OP 1.29.5, 2026-08-30) — ONE error, on the
/// bare `GlassSmash`:
///
/// * a global `enum` of the plugin's own JOINS the pool. `GlassSmash` is
///   carried by one engine enum and compiles bare until the plugin declares
///   it too. A workspace enum is a candidate, not a shield — the
///   counter-intuitive half of the rule.
/// * a global VARIABLE outranks the enums: `int Down = 1;` makes bare `Down`
///   compile even though two engine enums carry it.
/// * a NAMESPACED enum and a class member take no part at all.
#[test]
fn check_command_issue_repro_b053_workspace_and_ambiguity() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b053-ambiguous-enum-values-workspace");
    let ambiguous: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("ambiguous enum value"))
        .collect();
    assert_eq!(
        ambiguous.len(),
        1,
        "only the plugin's own enum creates a collision here; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        ambiguous[0].contains("`GlassSmash`")
            && ambiguous[0].contains("ESceneVehicleVisEvent")
            && ambiguous[0].contains("LocalPick"),
        "the workspace enum must appear as a candidate by name; stdout={stdout:?}"
    );
    // `Down` is ambiguous in the engine and must NOT be reported: the
    // plugin's `int Down = 1;` is the answer, and the game accepts it.
    for silent in ["`Down`", "`Horn`", "`JumpChain1`"] {
        assert!(
            !ambiguous.iter().any(|l| l.contains(silent)),
            "the game accepts {silent} here; reporting it would be a false \
             positive; stdout={stdout:?}"
        );
    }
}

/// B057: a bare enum-value name is a symbol only if a GLOBALLY VISIBLE enum
/// carries it.
///
/// The game resolves a bare value name against the enums it puts in the global
/// namespace. openplanet-lsp scanned EVERY named enum, so it stayed silent on
/// 2373 names the compiler answers `No matching symbol` for — a false negative
/// 100x the size of B053's ambiguity set, and the same defect family.
///
/// Ground truth is exhaustive, not sampled: all 2989 bare value names of the
/// shipped DB's named enums were submitted to the live game individually
/// (RemoteBuild, OP 1.29.5, 2026-08-30) and the verdicts are committed at
/// `docs/probes/b053-bare-enum-values.tsv` — 2373 `No matching symbol`, 592
/// accepted, 22 ambiguous, 2 resolving to a non-enum symbol.
/// `every_probed_bare_value_matches_the_games_verdict` checks the rule against
/// every one of those rows.
///
/// This fixture went to the game as ONE plugin (RemoteBuild, OP 1.29.5,
/// 2026-08-30) and produced exactly 14 errors, on exactly the lines and
/// columns the LSP reports, with `No matching symbol` each time. The
/// `GloballyVisible()` fence compiled clean.
///
/// How a plugin's OWN declaration of one of these names beats the engine is
/// the companion fixture `b057-nonglobal-enum-values-workspace`, not this
/// one: declaring `North` here would make the statement above it resolve.
///
/// B044 is the same narrowing one level less general: an ANONYMOUS enum has no
/// name for the visibility rule to place, so it was already excluded. B044
/// then generalised from the only evidence it had and concluded a named enum's
/// values always resolve, which the probe refuted — `VCenter2` below is the
/// name B044 pinned as resolving and the game rejects.
#[test]
fn check_command_issue_repro_b057_nonglobal_enum_value_names() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b057-nonglobal-enum-value-names");

    let undefined: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("undefined identifier"))
        .collect();
    assert_eq!(
        undefined.len(),
        14,
        "every name the game answers `No matching symbol` for must be \
         diagnosed; stdout={stdout:?} stderr={stderr:?}"
    );

    // Every arm of the rejection rule, by name, so a fix that merely changes
    // the count cannot pass.
    for rejected in [
        // Nested engine enum.
        "North",
        "East",
        "South",
        "West",
        "Achievements",
        // Carried by two or more nested enums — still zero global ones.
        "Party",
        "Aborted",
        "Action1",
        // Namespaced Core enum (`UI::Cond`).
        "FirstUseEver",
        "Appearing",
        // Namespaced Nadeo enum (`Meta::NPlugAnim::EExtractMotionMode`).
        "Absolute",
        // Anonymous enum AND a nested named one — the case B044 got wrong.
        "VCenter2",
        // A namespaced Core enum plus several nested ones.
        "Always",
        "Once",
    ] {
        let quoted = format!("`{rejected}`");
        assert!(
            undefined.iter().any(|l| l.contains(&quoted)),
            "the game answers `No matching symbol '{rejected}'`; it must be \
             diagnosed; stdout={stdout:?}"
        );
    }

    // The fence, and the direction that matters: these 592-set names compile
    // in-game, so diagnosing one would be a false POSITIVE.
    for accepted in [
        // `Openplanet.h` module-level enums (B046).
        "Jump",
        "DoubleJump",
        "TechJump",
        "GlassSmash",
        // Core enum with no namespace.
        "Space",
        "Accept",
        "A",
        // Nadeo module-level enum, under the loader's synthetic `Meta::` key.
        "Action1IsEvent",
        // Carried by a global enum AND by nested/anonymous ones.
        "Left",
        "Border",
    ] {
        let quoted = format!("`{accepted}`");
        assert!(
            !undefined.iter().any(|l| l.contains(&quoted)),
            "the game accepts `{accepted}`; diagnosing it would be a false \
             positive; stdout={stdout:?}"
        );
    }

    assert!(
        stdout.contains("14 diagnostics (14 errors)"),
        "the 14 rejects and nothing else — no warning, no second diagnostic \
         on the same name; stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B057, second half: a plugin's OWN declaration of a rejected name wins.
///
/// The false-POSITIVE direction, which is the damaging one for this tool. The
/// narrowing runs LAST in `has_global_ident`, after the type, function,
/// virtual-property, workspace-variable and workspace-enum-value lookups, so a
/// plugin that declares one of the 2373 names for itself must keep it.
///
/// This needs its own plugin: declaring `North` inside
/// `b057-nonglobal-enum-value-names` would make the statement that fixture
/// asserts an error on resolve. Same split as B053's `-workspace` companion.
///
/// All four names are real collisions from
/// `docs/probes/b053-bare-enum-values.tsv` and the game rejects all four bare
/// when nothing declares them, so a merge-base binary cannot pass this by
/// accident — it passes for the wrong reason there, and this fixture is the
/// one that would go red if the narrowing were placed before the workspace
/// lookups instead of after.
///
/// Game ground truth (RemoteBuild, OP 1.29.5, 2026-08-30): compiled with zero
/// diagnostics.
#[test]
fn check_command_issue_repro_b057_workspace_declarations_win() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b057-nonglobal-enum-values-workspace");

    for name in ["North", "VCenter2", "Once", "Appearing"] {
        let quoted = format!("`{name}`");
        assert!(
            !stdout.contains(&quoted),
            "the plugin declares `{name}` itself and the game compiles it; \
             any diagnostic naming it is a false positive; \
             stdout={stdout:?} stderr={stderr:?}"
        );
    }
    assert!(
        stdout.contains("0 diagnostics"),
        "the game compiled this clean, so the LSP must too; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

/// B046: the header is OPTIONAL. An install with no `Openplanet.h` must still
/// load its type DB and check cleanly — the module-level enums simply stay
/// unknown, exactly as before B046. Without this, a user whose install lacks
/// the header would get a hard load error rather than the old behaviour.
#[test]
fn check_command_without_a_header_still_loads_the_type_db() {
    let base = std::env::temp_dir().join(format!(
        "openplanet-lsp-b046-no-header-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("tests/fixtures/typedb");
    for name in ["OpenplanetCore.json", "OpenplanetNext.json"] {
        fs::copy(src.join(name), base.join(name)).unwrap();
    }
    assert!(
        !base.join("Openplanet.h").exists(),
        "no header by construction"
    );

    let fixture = manifest.join("tests/fixtures/issue-repros/b046-module-level-enums");
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg(&fixture)
        .arg("--typedb-dir")
        .arg(&base)
        .output()
        .expect("check must run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("failed to load type database"),
        "a missing header must not fail the load; stderr={stderr:?}"
    );
    // Without the header nothing names those two enums, so the B046 fixture
    // reverts to its pre-fix shape: 55 unresolvable value names plus the two
    // enum names used as types.
    assert!(
        stdout.contains("57 errors"),
        "without the header the pre-B046 diagnostics return; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(&base);
}

/// B019: the config layer decides whether `DEPENDENCY_<ID>` is defined, and it
/// must resolve dependencies over the SAME search dirs the workspace loader
/// uses — `--plugins-dir` included. If the probe saw fewer dirs than the
/// loader it would withhold the define for a dependency whose symbols ARE
/// pooled, masking out code the game compiles: a silent loss of coverage in
/// the opposite direction to the bug B019 fixes.
#[test]
fn check_command_plugins_dir_flag_is_visible_to_the_dependency_define_probe() {
    let base = std::env::temp_dir().join(format!(
        "openplanet-lsp-b019-plugins-dir-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("deps/ZzzFlagOnlyDep/src")).unwrap();
    fs::create_dir_all(base.join("consumer/src")).unwrap();
    fs::write(
        base.join("deps/ZzzFlagOnlyDep/info.toml"),
        "[meta]\nname = \"ZzzFlagOnlyDep\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    fs::write(
        base.join("consumer/info.toml"),
        "[meta]\nname = \"FlagDepConsumer\"\nversion = \"0.1.0\"\n\n[script]\noptional_dependencies = [\"ZzzFlagOnlyDep\"]\n",
    )
    .unwrap();
    // The guarded body is only CHECKED when the define is live, so the
    // undefined identifier is the observable for "the branch is live".
    fs::write(
        base.join("consumer/src/Main.as"),
        "void Main() {\n#if DEPENDENCY_ZZZFLAGONLYDEP\n    ZzzDefinitelyUndefinedIdentifier();\n#endif\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugins-dir")
        .arg(base.join("deps"))
        .arg(base.join("consumer"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ZzzDefinitelyUndefinedIdentifier"),
        "a dependency resolvable only via --plugins-dir must still define its \
         DEPENDENCY_ macro, keeping the guarded branch live; stdout={stdout:?}"
    );
    let _ = fs::remove_dir_all(base);
}

/// B033: a declaration whose type begins with `const` is anchored at the
/// `const`, not at the named type inside it.
///
/// The parser used to give a `const T@` type expression a span starting at
/// `T`, so every declaration whose type has that shape reported a column a few
/// characters into itself — `2:11` instead of `2:5` on
/// `    const Json::Value@ GetLocalStats(){` in the tm-archivist fixture. The
/// span fix that made hover stop losing the `const` also moved this column,
/// and it is user-visible, so it is pinned here on an exact `line:col` rather
/// than a fuzzy contains.
///
/// Four spaces then `const `, so the qualified type always starts at column
/// 11 and the declaration at column 5. `const Doc` and `Doc@` are controls:
/// neither shape was affected, and both must still report column 5.
#[test]
fn check_command_const_handle_declaration_is_anchored_at_the_const_keyword() {
    let root = make_temp_plugin("const-handle-column");
    fs::write(
        root.join("src/Main.as"),
        concat!(
            "class Doc {}\n",
            "namespace N {\n",
            "    const Doc@ Handle() { return null; }\n",
            "    const Doc@[] HandleArray() { return {}; }\n",
            "    const Doc Value() { Doc d; return d; }\n",
            "    Doc@ Plain() { return null; }\n",
            "}\n",
            "void Main() {}\n",
        ),
    )
    .unwrap();
    let stdout = check_plugin(&root, &[]);
    for (line, name) in [
        (3, "Handle"),
        (4, "HandleArray"),
        (5, "Value"),
        (6, "Plain"),
    ] {
        let expected = format!("src/Main.as:{line}:5: warning: a function `{name}`");
        assert!(
            stdout.contains(&expected),
            "expected {expected:?} (column 5 = the first token of the declaration); stdout={stdout:?}"
        );
    }
    // The pre-fix columns for the two `const T@` shapes, spelled out so the
    // regression is unmistakable if the span is ever moved back.
    for line in [3, 4] {
        assert!(
            !stdout.contains(&format!("src/Main.as:{line}:11: warning: a function")),
            "a `const Doc@` declaration re-anchored at the named type; stdout={stdout:?}"
        );
    }
    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------
// B040: info.toml rules reach `check`, and Error severity fails the run.
//
// The manifest decides whether the plugin loads at all, so `check` — which
// CI and `just check-showcase` run — must see the same findings the editor
// does, and a manifest that cannot load must not report success.
// ---------------------------------------------------------------------

/// A plugin whose `info.toml` is exactly `manifest`, plus one trivial source.
fn plugin_with_manifest(name: &str, manifest: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "openplanet-lsp-manifest-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("info.toml"), manifest).unwrap();
    fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
    root
}

fn run_check(root: &std::path::Path, home: &std::path::Path) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg(root)
        .env("HOME", home)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn check_reports_manifest_errors_and_exits_non_zero() {
    for (name, manifest, needle) in [
        ("no-version", "[meta]\nname = \"T\"\n", "[meta].version"),
        (
            "missing-export",
            "[meta]\nversion = \"1\"\n\n[script]\nexports = [\"Gone.as\"]\n",
            "Export file not found: Gone.as",
        ),
        (
            "missing-shared-export",
            "[meta]\nversion = \"1\"\n\n[script]\nshared_exports = [\"Gone.as\"]\n",
            "Shared export file not found: Gone.as",
        ),
        (
            "malformed",
            "[meta]\nversion = = \"1\"\n",
            "TOML parse error",
        ),
    ] {
        let root = plugin_with_manifest(name, manifest);
        let home = isolated_home(&format!("manifest-{name}"));
        let (code, stdout) = run_check(&root, &home);
        assert!(
            stdout.contains(needle),
            "{name}: expected {needle:?} in check output; got {stdout}"
        );
        assert!(
            stdout.contains("info.toml"),
            "{name}: the finding must be attributed to info.toml; got {stdout}"
        );
        assert_eq!(
            code, 1,
            "{name}: a manifest error must fail the run; got {stdout}"
        );
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }
}

/// A deprecated field is a warning: worth saying, not worth failing over.
#[test]
fn check_reports_manifest_warnings_without_failing() {
    let root = plugin_with_manifest(
        "warn-only",
        "[meta]\nname = \"T\"\nversion = \"1\"\nperms = \"full\"\n",
    );
    let home = isolated_home("manifest-warn-only");
    let (code, stdout) = run_check(&root, &home);
    assert!(
        stdout.contains("'perms' is deprecated"),
        "expected the deprecation warning; got {stdout}"
    );
    assert_eq!(
        code, 0,
        "a warning alone must not fail the run; got {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&home);
}

/// False-positive guard: exports that resolve through the loader's `src/`
/// search path — where most plugins keep their sources — are not reported.
#[test]
fn check_does_not_report_exports_that_resolve_under_src() {
    let root = plugin_with_manifest(
        "exports-under-src",
        "[meta]\nname = \"T\"\nversion = \"1\"\n\n[script]\nexports = [\"Ex/Fns.as\"]\nshared_exports = [\"Ex/Shared.as\"]\n",
    );
    fs::create_dir_all(root.join("src/Ex")).unwrap();
    fs::write(root.join("src/Ex/Fns.as"), "void F() {}\n").unwrap();
    fs::write(root.join("src/Ex/Shared.as"), "void S() {}\n").unwrap();
    let home = isolated_home("manifest-exports-under-src");

    let (code, stdout) = run_check(&root, &home);
    assert!(
        !stdout.contains("Export file not found"),
        "exports under src/ must resolve; got {stdout}"
    );
    assert_eq!(code, 0, "{stdout}");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&home);
}

/// `--plugin-files-search-path` moves where exports are looked for, and the
/// manifest rules must follow it — otherwise `check` contradicts the loader
/// it just ran.
#[test]
fn check_manifest_export_rules_follow_the_search_path_flag() {
    let root = plugin_with_manifest(
        "exports-custom-search",
        "[meta]\nname = \"T\"\nversion = \"1\"\n\n[script]\nexports = [\"Fns.as\"]\n",
    );
    fs::create_dir_all(root.join("code")).unwrap();
    fs::write(root.join("code/Fns.as"), "void F() {}\n").unwrap();
    let home = isolated_home("manifest-exports-custom-search");

    // Default search paths (`src/`) do not find it.
    let (code, stdout) = run_check(&root, &home);
    assert!(
        stdout.contains("Export file not found: Fns.as"),
        "control: not found under the default search path; got {stdout}"
    );
    assert_eq!(code, 1, "{stdout}");

    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .arg("check")
        .arg("--no-typedb")
        .arg("--plugin-files-search-path")
        .arg("code")
        .arg(&root)
        .env("HOME", &home)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Export file not found"),
        "the flag must move the export lookup too; got {stdout}"
    );
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&home);
}

/// B059: Openplanet-injected engine methods. `uint GetCurrentEntityID()` and
/// `array<CGameManialinkPage@>@ GetManialinkPages()` are registered by the
/// Openplanet runtime on six engine types and appear in no Nadeo dump, so
/// B006's trusted-member-list rule fired `UndefinedMember` on calls the game
/// compiles. The complete injected surface — recovered from Openplanet.dll
/// 1.29.5, md5 c179218b3b24582130ef42b92a3585f6 — is documented in
/// `docs/probes/openplanet-injected-methods.md`.
///
/// The fixture is deliberately half legal, half fence: silencing the legal
/// half is the fix, and keeping the fence half loud is what stops the fix
/// from becoming a B006 widening. On the merge-base binary all 8 lines
/// errored; only the 4 fences may error now.
///
/// Ground truth from BOTH directions, which is the point — the registration
/// surface came from static analysis of `Openplanet.dll` 1.29.5 (next,
/// Public, 1234ad9e), and the live compiler was then asked independently so
/// the binary reading is not the only witness.
///
/// RemoteBuild probe, Trackmania + Openplanet 1.29.5, 2026-08-30, this
/// fixture: game answers 4 ERR, exactly the four fences, with the legal half
/// silent —
///   (41,19) No matching symbol 'GetCurrentEntityID'          [wrong receiver]
///   (43,22) No matching symbol 'GetManialinkPages'           [wrong receiver]
///   (45,22) No matching signatures to
///           'CSmPlayer::GetCurrentEntityID(const int)'       [arity]
///   (47,22) No matching symbol 'GetCurrentEntityIdd'         [typo]
///
/// The arity line is why `MwAddRef`/`MwRelease` joining the shared table and
/// gaining arity checking is correct rather than scope creep: the game
/// rejects a wrong-arity call on an injected method, and we used to be
/// silent.
///
/// Return types were established in the REJECT direction rather than assumed
/// from silence — a companion probe compiled `string s = <call>;` so the
/// compiler had to NAME the type it refused to convert:
///   'uint' to 'string'                        => GetCurrentEntityID -> uint
///   'CGameManialinkPage@[]@&' to 'string'     => GetManialinkPages
/// `CGameManialinkPage@[]@` IS `array<CGameManialinkPage@>@` in the other
/// spelling; the trailing `&` is AngelScript rendering the expression in an
/// error, not part of the registered declaration, which is byte-exactly
/// `array<CGameManialinkPage@>@ GetManialinkPages()` in `.rdata`. The old
/// `convert_nadeo.ts` said `CGameManialinkPage@[]&` — a REFERENCE to an
/// array rather than a HANDLE to one, which is the part that was wrong.
///
/// That last paragraph decodes a spelling, and a decoding is an inference, so
/// the `@` was checked directly rather than argued: a fifth probe compiled
/// `if (net.GetManialinkPages() is null)`, which AngelScript permits only on
/// a handle. CLEAN. The handle is real, not a reading of an error message.
/// (It changes no behaviour either way — `parse_type_string` strips a
/// trailing `&` as a calling-convention decoration, so both spellings give
/// the same `Handle(Array(Handle(Named)))` — but "it wouldn't have mattered"
/// is a thing you can only say once you know.)
///
/// All six receivers were confirmed by a paired discriminator: each of
/// `CGameNetwork`, `CGameCtnNetwork`, `CGameManiaPlanetNetwork`,
/// `CTrackManiaNetwork`, `CGamePlayer`, `CSmPlayer` accepted its real call
/// while rejecting `ZZZNoSuchMember` with `No matching symbol`. The rejection
/// is what makes the acceptance mean something — it proves member lookup
/// actually runs on that receiver.
#[test]
fn check_command_issue_repro_b059_injected_methods_silences_only_the_legal_half() {
    let (_, stdout, stderr) = run_issue_repro_typedb("b059-injected-methods");
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains(": error: ")).collect();
    assert_eq!(
        errors.len(),
        4,
        "only the 4 fence lines may error; stdout={stdout:?} stderr={stderr:?}"
    );
    // Legal half (lines 25, 27, 31, 32) — the false positives B059 removes.
    for line in ["Main.as:25", "Main.as:27", "Main.as:31", "Main.as:32"] {
        assert!(
            !errors.iter().any(|l| l.contains(line)),
            "`{line}` is a legal injected call and must be silent; stdout={stdout:?}"
        );
    }
    // Fence half. Wrong receiver, wrong receiver, wrong arity, real typo.
    for (line, needle) in [
        ("Main.as:41", "has no member `GetCurrentEntityID`"),
        ("Main.as:43", "has no member `GetManialinkPages`"),
        ("Main.as:45", "expects 0 args, got 1"),
        ("Main.as:47", "has no member `GetCurrentEntityIdd`"),
    ] {
        assert!(
            errors
                .iter()
                .any(|l| l.contains(line) && l.contains(needle)),
            "`{line}` must still report `{needle}`; stdout={stdout:?}"
        );
    }
}
