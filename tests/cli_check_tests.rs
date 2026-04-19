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
dependencies = ["Editor"]
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
