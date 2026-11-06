use openplanet_lsp::testing::TempDir;
use std::{fs, path::Path, process::Command};

fn check(root: &Path, extra: &[&str]) -> (i32, Vec<serde_json::Value>) {
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .args(["check", "--no-typedb", "--format", "json"])
        .args(extra)
        .arg(root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"));
    let errors = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .cloned()
        .collect();
    (output.status.code().unwrap(), errors)
}

fn plugin() -> TempDir {
    let root = TempDir::new("gh75-duplicates");
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Duplicates\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    root
}

#[test]
fn gh75_reports_the_later_field_and_namespace_declarators() {
    let root = plugin();
    fs::write(root.join("Main.as"), "class Car {\n    string model, skin;\n    string model, owner;\n}\nnamespace S {\n    string a, b;\n    string a;\n}\nvoid Main() { Car c; print(c.model + S::a); }\nvoid print(const string &in text) {}\n").unwrap();
    let (status, errors) = check(&root, &[]);
    assert_eq!(status, 1, "{errors:?}");
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert_eq!(errors[0]["message"], "'model' is already declared");
    assert_eq!(
        (errors[0]["line"].as_u64(), errors[0]["col"].as_u64()),
        (Some(3), Some(12))
    );
    assert_eq!(errors[1]["message"], "'a' is already declared");
    assert_eq!(
        (errors[1]["line"].as_u64(), errors[1]["col"].as_u64()),
        (Some(7), Some(12))
    );
}

#[test]
fn gh75_generalizes_declarators_types_and_reopened_scopes() {
    for source in [
        "int x, x; void Main() {}",
        "namespace N { int x; } namespace N { string x; } void Main() {}",
        "class T {} class C { T@ x; const T@ x; } void Main() {}",
        "class C { int x, x; } void Main() {}",
        "int x; const int x = 1; void Main() {}",
    ] {
        let root = plugin();
        fs::write(root.join("Main.as"), source).unwrap();
        let (status, errors) = check(&root, &[]);
        assert_eq!(status, 1, "{source}: {errors:?}");
        assert_eq!(errors.len(), 1, "{source}: {errors:?}");
        assert_eq!(errors[0]["message"], "'x' is already declared", "{source}");
    }
}

#[test]
fn gh75_reports_only_the_later_cross_file_namespace_global() {
    let root = plugin();
    fs::write(root.join("A.as"), "namespace N { int x; }").unwrap();
    fs::write(
        root.join("B.as"),
        "namespace N { string x; } void Main() {}",
    )
    .unwrap();
    let (status, errors) = check(&root, &[]);
    assert_eq!(status, 1, "{errors:?}");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0]["path"], "B.as");
    assert_eq!(errors[0]["message"], "'x' is already declared");
}

#[test]
fn gh75_distinct_owners_inheritance_and_profile_alternatives_are_legal() {
    let root = plugin();
    fs::write(root.join("Main.as"), "int x; namespace N { int x; } namespace M { int x; }\nclass Base { int x; } class Derived : Base { int x; }\nclass Other { int x; }\n#if DEV\nint selected;\n#else\nint selected;\n#endif\nvoid Main() { int x; }\n").unwrap();
    for extra in [vec![], vec!["--define", "DEV"]] {
        let (status, errors) = check(&root, &extra);
        assert_eq!(status, 0, "{extra:?}: {errors:?}");
        assert!(errors.is_empty(), "{extra:?}: {errors:?}");
    }
    fs::write(
        root.join("Main.as"),
        "int selected;\n#if DEV\nint selected;\n#endif\nvoid Main() {}\n",
    )
    .unwrap();
    assert!(check(&root, &[]).1.is_empty());
    let (status, errors) = check(&root, &["--define", "DEV"]);
    assert_eq!(status, 1);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0]["line"], 3);
}
