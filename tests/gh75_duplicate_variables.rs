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

#[test]
fn gh75_own_strict_exports_do_not_conflict_with_the_defining_module() {
    for (ordinary, exported) in [("A.as", "Z_Exports.as"), ("Z.as", "A_Exports.as")] {
        let root = plugin();
        fs::write(root.join("info.toml"), format!("[meta]\nname = \"Duplicates\"\nversion = \"0.1.0\"\n[script]\nexports = [\"{exported}\"]\n")).unwrap();
        fs::write(root.join(ordinary), "int x; void Main() {}").unwrap();
        fs::write(root.join(exported), "int x;").unwrap();
        let (status, errors) = check(&root, &[]);
        assert_eq!(status, 0, "{ordinary}/{exported}: {errors:?}");
        assert!(errors.is_empty(), "{ordinary}/{exported}: {errors:?}");
    }
}

#[test]
fn gh75_consumer_globals_conflict_with_non_reporting_dependency_exports() {
    for kind in ["exports", "shared_exports"] {
        let root = plugin();
        let dependency = root.join("plugins/Dep");
        fs::create_dir_all(&dependency).unwrap();
        fs::write(
            root.join("info.toml"),
            "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n[script]\ndependencies = [\"Dep\"]\n",
        )
        .unwrap();
        fs::write(
            root.join("Main.as"),
            "namespace N { int x; } void Main() {}",
        )
        .unwrap();
        fs::write(
            dependency.join("info.toml"),
            format!(
                "[meta]\nname = \"Dep\"\nversion = \"0.1.0\"\n[script]\n{kind} = [\"Api.as\"]\n"
            ),
        )
        .unwrap();
        fs::write(dependency.join("Api.as"), "namespace N { int x; }").unwrap();
        let plugins = root.join("plugins");
        let (status, errors) = check(&root, &["--plugins-dir", plugins.to_str().unwrap()]);
        assert_eq!(status, 1, "{kind}: {errors:?}");
        assert_eq!(errors.len(), 1, "{kind}: {errors:?}");
        assert_eq!(errors[0]["path"], "Main.as");
        assert_eq!(errors[0]["message"], "'x' is already declared");
    }
}

#[test]
fn gh75_strict_exports_respect_root_and_transitive_dependency_reexports() {
    let mut actual = Vec::new();
    for (case, declarations, bridge_target, archive, expected) in [
        ("direct", "export_dependencies = [\"Dep\"]", "Dep", false, 1),
        (
            "direct_already_private",
            "dependencies = [\"Dep\"]\nexport_dependencies = [\"Dep\"]",
            "Dep",
            false,
            1,
        ),
        (
            "transitive_already_private",
            "dependencies = [\"Dep\"]\nexport_dependencies = [\"Bridge\"]",
            "Dep",
            false,
            1,
        ),
        ("private_only", "dependencies = [\"Dep\"]", "Dep", false, 0),
        // Dep's folder ID is loaded privately first. The module ID then
        // resolves the same identity via a different dependency spelling.
        (
            "alias_already_private",
            "dependencies = [\"Dep\"]\nexport_dependencies = [\"DepAlias\"]",
            "Dep",
            false,
            1,
        ),
        (
            "transitive_alias_already_private",
            "dependencies = [\"Dep\"]\nexport_dependencies = [\"Bridge\"]",
            "DepAlias",
            false,
            1,
        ),
        // Archives contribute pseudo paths, which cannot canonicalize on disk.
        (
            "archive_direct",
            "export_dependencies = [\"Dep\"]",
            "Dep",
            true,
            1,
        ),
        (
            "archive_already_private",
            "dependencies = [\"Dep\"]\nexport_dependencies = [\"Dep\"]",
            "Dep",
            true,
            1,
        ),
        (
            "archive_transitive_already_private",
            "dependencies = [\"Dep\"]\nexport_dependencies = [\"Bridge\"]",
            "Dep",
            true,
            1,
        ),
        (
            "archive_private_only",
            "dependencies = [\"Dep\"]",
            "Dep",
            true,
            0,
        ),
    ] {
        let root = plugin();
        let plugins = root.join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::write(root.join("info.toml"), format!("[meta]\nname = \"Provider\"\nversion = \"0.1.0\"\n[script]\nexports = [\"Api.as\"]\n{declarations}\n")).unwrap();
        fs::write(
            root.join("Main.as"),
            "void Main() { DependencyAvailable(); }",
        )
        .unwrap();
        fs::write(root.join("Api.as"), "namespace N { int x; }").unwrap();
        for (name, manifest, source) in [
            ("Dep", "[meta]\nname = \"Dep\"\nversion = \"0.1.0\"\n[script]\nmodule = \"DepAlias\"\nexports = [\"Api.as\"]\n".to_string(), "namespace N { int x; } void DependencyAvailable() {}"),
            ("Bridge", format!("[meta]\nname = \"Bridge\"\nversion = \"0.1.0\"\n[script]\nexport_dependencies = [\"{bridge_target}\"]\n"), ""),
        ] {
            if archive {
                use std::io::Write;
                let file = fs::File::create(plugins.join(format!("{name}.op"))).unwrap();
                let mut zip = zip::ZipWriter::new(file);
                let options = zip::write::SimpleFileOptions::default();
                zip.start_file("info.toml", options).unwrap();
                zip.write_all(manifest.as_bytes()).unwrap();
                zip.start_file("Api.as", options).unwrap();
                zip.write_all(source.as_bytes()).unwrap();
                zip.finish().unwrap();
            } else {
                let dependency = plugins.join(name);
                fs::create_dir_all(&dependency).unwrap();
                fs::write(dependency.join("info.toml"), manifest).unwrap();
                fs::write(dependency.join("Api.as"), source).unwrap();
            }
        }
        let (status, errors) = check(&root, &["--plugins-dir", plugins.to_str().unwrap()]);
        assert!(
            errors
                .iter()
                .all(|d| d["path"] == "Api.as" && d["message"] == "'x' is already declared"),
            "{case}: {errors:?}"
        );
        actual.push((case, status, errors.len(), expected));
    }
    let failures: Vec<_> = actual
        .iter()
        .filter(|(_, status, count, expected)| {
            count != expected || *status != i32::from(*expected > 0)
        })
        .collect();
    assert!(
        failures.is_empty(),
        "root/transitive/private reachability: {actual:?}"
    );
}
