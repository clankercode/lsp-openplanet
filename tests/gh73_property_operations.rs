use openplanet_lsp::testing::TempDir;
use std::{fs, path::Path, process::Command};

fn check(root: &Path, extra: &[&str]) -> Vec<serde_json::Value> {
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
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .cloned()
        .collect()
}

fn plugin(source: &str) -> TempDir {
    let root = TempDir::new("gh73-properties");
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Property operations\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(root.join("Main.as"), source).unwrap();
    root
}

#[test]
fn gh73_rejects_the_reported_implicit_this_postfix_decrement() {
    let root = plugin("class TabGroup {\n int _selectedTabIx;\n int get_selectedTabIx() { return _selectedTabIx; }\n void set_selectedTabIx(int value) { _selectedTabIx = value; }\n void Remove() { selectedTabIx--; }\n}\nvoid Main() {}\n");
    let errors = check(&root, &[]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0]["message"],
        "Invalid reference. Property accessors cannot be used in combined read/write operations"
    );
    assert_eq!(errors[0]["line"], 5);
}

#[test]
fn gh73_checks_each_combined_operator_and_accessor_spelling() {
    for operation in [
        "P++", "P--", "++P", "--P", "P += 1", "P -= 1", "P *= 2", "P /= 2", "P %= 2", "P &= 1",
        "P |= 1", "P ^= 1", "P <<= 1", "P >>= 1",
    ] {
        for target in [
            "P",
            "this.P",
            "c.P",
            "child.P",
            "items[0].P",
            "Global",
            "N::Global",
        ] {
            let statement = operation.replace('P', target);
            let source = format!("int get_Global() {{ return 0; }} void set_Global(int v) {{}}\nnamespace N {{ int get_Global() {{ return 0; }} void set_Global(int v) {{}} }}\nclass C {{ int get_P() {{ return 0; }} void set_P(int v) {{}} void Use(C@ c, Child@ child, array<C@>@ items) {{ {statement}; }} }}\nclass Child : C {{}}\nvoid Main() {{}}\n");
            let root = plugin(&source);
            let errors = check(&root, &[]);
            assert_eq!(errors.len(), 1, "{statement}: {errors:?}");
            assert_eq!(errors[0]["message"], "Invalid reference. Property accessors cannot be used in combined read/write operations", "{statement}");
        }
    }
}

#[test]
fn gh73_preserves_plain_assignments_fields_local_shadows_and_indirect_writes() {
    let root = plugin("int get_Global() { return 0; } void set_Global(int v) {}\nclass C { int field; int get_P() { return field; } void set_P(int v) { field = v; } array<int>@ values; array<int>@ get_Items() { return values; } void Use() { P = P - 1; this.P = P + 1; field++; ++field; field += 1; Items[0]++; int P = 0; P++; P += 1; int Global = 0; Global--; } }\nclass Base { int get_X() { return 0; } void set_X(int v) {} }\nclass Child : Base { int X; void Use() { X++; this.X += 1; } }\nvoid Main() { C c; c.field++; c.P = c.P - 1; Global = Global + 1; }\n");
    assert!(check(&root, &[]).is_empty());
}

#[test]
fn gh73_follows_accessors_declared_in_sibling_files() {
    let root = plugin("void Main() { N::Counter@ c; c.P++; N::Value += 1; }");
    fs::write(root.join("Types.as"), "namespace N { class Counter { int get_P() { return 0; } void set_P(int v) {} } int get_Value() { return 0; } void set_Value(int v) {} }").unwrap();
    let errors = check(&root, &[]);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
}

#[test]
fn gh73_property_block_syntax_retains_accessor_identity_across_files() {
    let root = plugin("void Main() { Counter c; c.P++; Settings::Value += 1; }");
    fs::write(root.join("Properties.as"), "class Counter { int backing; int P { get { return backing; } set { backing = value; } } }\nnamespace Settings { int backing; int Value { get { return backing; } set { backing = value; } } }\n").unwrap();
    let errors = check(&root, &[]);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
}

#[test]
fn gh73_absolute_globals_do_not_resolve_through_an_enclosing_namespace() {
    for (global, nested, expected) in [
        (
            "int P;",
            "int get_P() { return 0; } void set_P(int v) {}",
            0,
        ),
        (
            "int get_P() { return 0; } void set_P(int v) {}",
            "int P;",
            1,
        ),
    ] {
        for operation in ["::P++", "--::P", "::P += 1"] {
            let root = plugin(&format!("{global} namespace N {{ {nested} void Use() {{ {operation}; }} }} void Main() {{}}"));
            let errors = check(&root, &[]);
            assert_eq!(errors.len(), expected, "{global}/{operation}: {errors:?}");
            assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
        }
    }
}
