use openplanet_lsp::testing::TempDir;
use std::{fs, path::Path, process::Command};

fn check(source: &str) -> Vec<serde_json::Value> {
    let root = TempDir::new("gh68-operator-columns");
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Operator columns\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(root.join("Main.as"), source).unwrap();
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
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["severity"] != "error"),
        "{report}"
    );
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["message"] == "Signed/Unsigned mismatch")
        .cloned()
        .collect()
}

#[test]
fn gh68_reported_string_comparisons_match_game_operator_columns() {
    let warnings = check("void Main() {\n    string s = \"abc\";\n    uint n = 3;\n    if (s.Length < n) print(\"lt\");\n    if (s.Length == n) print(\"eq\");\n    if (s.Length != n) print(\"ne\");\n    for (uint i = 0; i < s.Length; i++) print(\"loop\");\n}\n");
    let locations: Vec<_> = warnings
        .iter()
        .map(|d| (d["line"].as_u64().unwrap(), d["col"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        locations,
        [(4, 18), (5, 18), (6, 18), (7, 24)],
        "{warnings:?}"
    );
}

#[tokio::test]
#[allow(deprecated)]
async fn gh68_lsp_operator_diagnostics_keep_both_operand_cast_actions() {
    use openplanet_lsp::server::Backend;
    use tower_lsp::lsp_types::*;
    use tower_lsp::{LanguageServer, LspService};
    let root = TempDir::new("gh68-lsp");
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Operator LSP\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(InitializeParams {
            root_uri: Some(Url::from_directory_path(&*root).unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    let uri = Url::from_file_path(root.join("Main.as")).unwrap();
    let source =
        "void Main() { /* 🙂 */ int i; uint u; int j; uint v; bool b = (i != u) == (j < v); }\n";
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "angelscript".into(),
                version: 1,
                text: source.into(),
            },
        })
        .await;
    for (operator, left, right) in [("!=", "i", "u"), ("<", "j", "v")] {
        let start = source[..source.find(operator).unwrap()]
            .encode_utf16()
            .count() as u32;
        let range = Range::new(
            Position::new(0, start),
            Position::new(0, start + operator.len() as u32),
        );
        let actions = backend
            .code_action(CodeActionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range,
                context: CodeActionContext {
                    diagnostics: vec![],
                    only: Some(vec![CodeActionKind::QUICKFIX]),
                    trigger_kind: None,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .unwrap()
            .unwrap();
        let casts: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(a)
                    if a.diagnostics.as_ref().is_some_and(|ds| {
                        ds.iter().any(|d| d.message == "Signed/Unsigned mismatch")
                    }) =>
                {
                    Some(a)
                }
                _ => None,
            })
            .collect();
        assert_eq!(casts.len(), 2, "{operator}: {actions:?}");
        for action in &casts {
            assert_eq!(action.diagnostics.as_ref().unwrap()[0].range, range);
            assert_ne!(action.is_preferred, Some(true));
        }
        for (operand, cast) in [
            (left, format!("uint({left})")),
            (right, format!("int({right})")),
        ] {
            let expected_byte = if operand == left {
                source.find(&format!("({left} ")).unwrap() + 1
            } else {
                source.find(&format!(" {right})")).unwrap() + 1
            };
            let expected_start = source[..expected_byte].encode_utf16().count() as u32;
            let edits: Vec<_> = casts
                .iter()
                .flat_map(|a| a.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri].iter())
                .filter(|e| e.new_text == cast)
                .collect();
            assert_eq!(edits.len(), 1, "{casts:?}");
            assert_eq!(
                edits[0].range,
                Range::new(
                    Position::new(0, expected_start),
                    Position::new(0, expected_start + 1)
                )
            );
        }
    }
}

#[test]
fn gh68_each_comparison_finds_its_operator_through_comments_and_parentheses() {
    for (signed, unsigned) in [
        ("int8", "uint8"),
        ("int16", "uint16"),
        ("int", "uint"),
        ("int64", "uint64"),
    ] {
        for op in ["<", "<=", ">", ">=", "==", "!="] {
            for (left, right) in [("i", "u"), ("u", "i")] {
                let prefix = format!("    /* é🙂 */ bool b = (({left})) /* < > == != */ ");
                let source = format!(
                    "void Main() {{\n    {signed} i; {unsigned} u;\n{prefix}{op} ({right});\n}}\n"
                );
                let warnings = check(&source);
                assert_eq!(warnings.len(), 1, "{source}: {warnings:?}");
                assert_eq!(warnings[0]["line"], 3);
                // CLI JSON columns count UTF-8 bytes; LSP uses UTF-16.
                assert_eq!(
                    warnings[0]["col"],
                    prefix.len() + 1,
                    "{source}: {warnings:?}"
                );
            }
        }
    }
}

#[test]
fn gh68_keeps_literal_conversion_and_same_signedness_suppression() {
    for op in ["<", "<=", ">", ">=", "==", "!="] {
        let source = format!("void Main() {{ int i; int j; uint u; uint v; bool a = i {op} 0; bool b = 0 {op} u; bool c = int(u) {op} i; bool d = u {op} uint(i); bool e = i {op} j; bool f = u {op} v; }}");
        assert!(check(&source).is_empty(), "{source}");
    }
    let source = "void Main() { int i; uint u; bool b = i + u; }";
    let warnings = check(source);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["col"], 39);
}
