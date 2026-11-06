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
            // OP1.29.14 live matrix: increment/decrement reject, while
            // every compound assignment is accepted for these targets.
            let expected = usize::from(!operation.contains('='));
            assert_eq!(errors.len(), expected, "{statement}: {errors:?}");
            assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"), "{statement}: {errors:?}");
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
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
}

#[test]
fn gh73_property_block_syntax_retains_accessor_identity_across_files() {
    let root = plugin("void Main() { Counter c; c.P++; Settings::Value += 1; }");
    fs::write(root.join("Properties.as"), "class Counter { int backing; int P { get { return backing; } set { backing = value; } } }\nnamespace Settings { int backing; int Value { get { return backing; } set { backing = value; } } }\n").unwrap();
    let errors = check(&root, &[]);
    assert_eq!(errors.len(), 1, "{errors:?}");
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
            let expected = if operation.contains('=') { 0 } else { expected };
            assert_eq!(errors.len(), expected, "{global}/{operation}: {errors:?}");
            assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
        }
    }
}

#[test]
fn gh73_cross_file_member_types_keep_their_declaring_namespace() {
    for (global_member, nested_member, expected) in [
        (
            "int get_P() { return 0; } void set_P(int v) {}",
            "int P;",
            0,
        ),
        (
            "int P;",
            "int get_P() { return 0; } void set_P(int v) {}",
            1,
        ),
    ] {
        for (declarations, receiver) in [
            ("class Box { C child; }", "b.child"),
            ("class Box { C@ child; }", "b.child"),
            ("class Box { C@ get_child() { return null; } }", "b.child"),
            ("class Box { array<C@> children; }", "b.children[0]"),
            (
                "class Box { array<C@>@ get_children() { return null; } }",
                "b.children[0]",
            ),
            (
                "class Parent { C@ child; } class Box : Parent {}",
                "b.child",
            ),
            (
                "class Box { C@ GetChild() { return null; } }",
                "b.GetChild()",
            ),
        ] {
            let root = plugin(&format!("void Main() {{ N::Box b; {receiver}.P++; }}"));
            fs::write(root.join("Types.as"), format!("class C {{ {global_member} }} namespace N {{ class C {{ {nested_member} }} {declarations} }}")).unwrap();
            let errors = check(&root, &[]);
            assert_eq!(
                errors.len(),
                expected,
                "global={global_member}/{declarations}: {errors:?}"
            );
            assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
        }
    }
}

#[test]
fn gh73_cross_file_factories_and_globals_keep_their_declaring_namespace() {
    let mut results = Vec::new();
    for (global_member, nested_member, expected) in [
        (
            "int get_P() { return 0; } void set_P(int v) {}",
            "int P;",
            0,
        ),
        (
            "int P;",
            "int get_P() { return 0; } void set_P(int v) {}",
            1,
        ),
    ] {
        for (declarations, receiver) in [
            ("C@ Make() { return null; }", "N::Make()"),
            (
                "C@ Make() { return null; } C@ Make(int v) { return null; }",
                "N::Make(1)",
            ),
            ("C@ Value;", "N::Value"),
            ("C@ get_Value() { return null; }", "N::Value"),
            ("C@ Value { get { return null; } }", "N::Value"),
            ("array<C@>@ Make() { return null; }", "N::Make()[0]"),
        ] {
            let root = plugin(&format!("void Main() {{ {receiver}.P++; }}"));
            fs::write(root.join("Types.as"), format!("class C {{ {global_member} }} namespace N {{ class C {{ {nested_member} }} {declarations} }}")).unwrap();
            let errors = check(&root, &[]);
            assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"), "{declarations}: {errors:?}");
            results.push((declarations, expected, errors.len()));
        }
    }
    assert!(
        results
            .iter()
            .all(|(_, expected, actual)| expected == actual),
        "{results:?}"
    );
}

#[test]
fn gh73_lexical_types_preserve_local_parameter_and_same_file_member_owners() {
    let mut results = Vec::new();
    for (global_member, nested_member, expected) in [
        (
            "int get_P() { return 0; } void set_P(int v) {}",
            "int P;",
            0,
        ),
        (
            "int P;",
            "int get_P() { return 0; } void set_P(int v) {}",
            1,
        ),
    ] {
        for target in ["b.child", "b.Make()", "local", "param"] {
            let root = plugin(&format!("class C {{ {global_member} }} namespace N {{ class C {{ {nested_member} }} class Box {{ C child; C@ Make() {{ return null; }} }} void Use(C@ param) {{ Box b; C local; {target}.P++; }} }} void Main() {{}}"));
            let errors = check(&root, &[]);
            assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"), "{target}: {errors:?}");
            results.push((target, expected, errors.len()));
        }
    }
    assert!(
        results
            .iter()
            .all(|(_, expected, actual)| expected == actual),
        "{results:?}"
    );
    let root = plugin("funcdef void Callback(); namespace N { funcdef int Callback(); void Sink(int value) {} void Use(Callback@ cb) { Sink(cb()); } } void Main() {}");
    let errors = check(&root, &[]);
    assert!(
        errors.is_empty(),
        "lexical funcdefs must remain callable: {errors:?}"
    );
}

#[test]
fn gh73_inherited_overload_returns_keep_the_winning_declaration_namespace() {
    for (declaring_member, caller_member, expected) in [
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
        let root = plugin(&format!("namespace M {{ class C {{ {declaring_member} }} class Root {{ C@ Make() {{ return null; }} C@ Make(int v) {{ return null; }} }} }} namespace N {{ class C {{ {caller_member} }} class Base : M::Root {{}} class Child : Base {{ void Use() {{ N::Base::Make(1).P++; }} }} }} void Main() {{}}"));
        let errors = check(&root, &[]);
        assert_eq!(errors.len(), expected, "{errors:?}");
        assert!(errors.iter().all(|d| d["message"] == "Invalid reference. Property accessors cannot be used in combined read/write operations"));
    }
}

#[test]
fn gh73_namespace_value_types_preserve_nearest_enum_validation() {
    let root = plugin(
        "enum E { X } namespace N { enum E { Y } void Use() { int value = E::X; } } void Main() {}",
    );
    let errors = check(&root, &[]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("No matching symbol 'E::X'"),
        "{errors:?}"
    );
}

#[test]
fn gh73_known_qualified_core_types_keep_precedence_over_nested_names() {
    let root = plugin("void Good(UI::InputTextCallbackData@ data) { data.SelectAll(); } namespace Qz { namespace UI { funcdef void InputTextCallback(int value); } UI::InputTextCallback@ callback = Good; void Use() { string text; UI::InputText(\"id\", text, 0, callback); } } void Main() {}");
    let typedb = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .args(["check", "--format", "json", "--typedb-dir"])
        .arg(typedb)
        .arg(&*root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"));
    let errors: Vec<_> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(output.status.code(), Some(0));
}
