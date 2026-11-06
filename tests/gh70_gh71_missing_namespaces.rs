use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use openplanet_lsp::testing::TempDir;

fn plugin(root: &Path, script: &str, source: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("info.toml"),
        format!("[meta]\nname = \"Namespace regression\"\nversion = \"0.1.0\"\n[script]\n{script}"),
    )
    .unwrap();
    fs::write(root.join("src/Main.as"), source).unwrap();
}

fn check(base: &Path, root: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .args(["check", "--no-typedb", "--format", "json"])
        .args(extra)
        .arg(root)
        .env("HOME", base.join("home"))
        .env("XDG_CONFIG_HOME", base.join("config"))
        .output()
        .unwrap()
}

fn errors(output: &Output) -> Vec<serde_json::Value> {
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("invalid report: {e}; output={output:?}"));
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .cloned()
        .collect()
}

#[test]
fn gh71_new_file_reports_a_wholly_missing_namespace() {
    let base = TempDir::new("gh71-missing-namespace");
    let root = base.join("plugin");
    plugin(&root, "", "void Main() {}\n");
    fs::write(
        root.join("src/Foo_Test.as"),
        "#if UNIT_TEST\nvoid Test() { Foo::Bar(); }\n#endif\n",
    )
    .unwrap();
    let output = check(&base, &root, &["--define", "UNIT_TEST"]);
    let found = errors(&output);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["message"], "No matching symbol 'Foo::Bar'");
    assert!(found[0]["path"].as_str().unwrap().ends_with("Foo_Test.as"));
}

#[test]
fn gh70_installed_exports_require_a_manifest_dependency() {
    let base = TempDir::new("gh70-manifest-visibility");
    let plugins = base.join("plugins");
    let camera = plugins.join("Camera");
    plugin(
        &camera,
        "module = \"Camera\"\nexports = [\"Main.as\"]\n",
        "namespace Camera { int ToScreen() { return 1; } int GetCurrent() { return 1; } }",
    );
    for declaration in [
        "",
        "dependencies",
        "optional_dependencies",
        "export_dependencies",
    ] {
        let root = base.join(format!("consumer-{declaration}"));
        let script = if declaration.is_empty() {
            String::new()
        } else {
            format!("{declaration} = [\"Camera\"]\n")
        };
        plugin(
            &root,
            &script,
            "void Main() { Camera::ToScreen(); Camera::GetCurrent(); }",
        );
        let output = check(&base, &root, &["--plugins-dir", plugins.to_str().unwrap()]);
        let found = errors(&output);
        if declaration.is_empty() {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            let messages: Vec<_> = found
                .iter()
                .map(|d| d["message"].as_str().unwrap())
                .collect();
            assert_eq!(
                messages,
                [
                    "No matching symbol 'Camera::ToScreen'",
                    "No matching symbol 'Camera::GetCurrent'"
                ]
            );
        } else {
            assert!(output.status.success(), "{declaration}: {output:?}");
            assert!(found.is_empty(), "{declaration}: {found:?}");
        }
    }
}

#[test]
fn gh71_preserves_incomplete_closure_class_and_relative_guards() {
    let base = TempDir::new("gh71-guards");
    let root = base.join("plugin");
    plugin(
        &root,
        "",
        "class Thing {}\nnamespace Outer { class Local {} namespace Inner { void Exists() {} } void Use() { Inner::Exists(); Local::Unknown(); } }\nvoid Main() { Thing::Unknown(); UI::UnloadedEngineCall(); }",
    );
    let output = check(&base, &root, &[]);
    assert!(errors(&output).is_empty(), "{output:?}");
    assert!(output.status.success(), "{output:?}");

    plugin(
        &root,
        "dependencies = [\"Absent\"]\n",
        "void Main() { Unknown::Function(); }",
    );
    let plugins = base.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let output = check(&base, &root, &["--plugins-dir", plugins.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "missing required dependency must still fail"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("No matching symbol 'Unknown::Function'"),
        "{output:?}"
    );
}
