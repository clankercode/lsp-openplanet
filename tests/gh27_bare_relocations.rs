use openplanet_lsp::testing::TempDir;
use std::{fs, path::Path, process::Command};

fn check(root: &Path, typedb: &Path, source: &str) -> serde_json::Value {
    fs::write(root.join("Main.as"), source).unwrap();
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Relocation\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .args(["check", "--format", "json", "--typedb-dir"])
        .arg(typedb)
        .arg(root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap();
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"))
}

#[test]
fn gh27_bare_calls_offer_the_measured_successors_on_each_recorded_version() {
    for version in ["1.29.5", "1.29.14"] {
        let root = TempDir::new("gh27-relocation");
        let typedb = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb-versions")
            .join(version);
        let report = check(
            &root,
            &typedb,
            "void Main() { StripFormatCodes(\"text\"); ColoredString(\"text\"); }",
        );
        let messages: Vec<_> = report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["severity"] == "error")
            .map(|d| d["message"].as_str().unwrap())
            .collect();
        assert_eq!(
            messages,
            [
                "No matching symbol 'StripFormatCodes' — use `Text::StripFormatCodes` instead",
                "No matching symbol 'ColoredString' — use `Text::OpenplanetFormatCodes` instead",
            ],
            "{version}: {report}"
        );
    }
}

#[test]
fn gh27_resolved_names_keep_precedence_over_relocations() {
    let typedb = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    for source in [
        "void StripFormatCodes() {} void ColoredString() {} void Main() { StripFormatCodes(); ColoredString(); }",
        "namespace N { void StripFormatCodes() {} void ColoredString() {} void Use() { StripFormatCodes(); ColoredString(); } } void Main() { N::Use(); }",
        "class C { void StripFormatCodes() {} void ColoredString() {} void Use() { StripFormatCodes(); ColoredString(); } } void Main() { C c; c.Use(); }",
        "funcdef void Action(); void Main() { Action@ StripFormatCodes = null; Action@ ColoredString = null; StripFormatCodes(); ColoredString(); }",
    ] {
        let root = TempDir::new("gh27-shadowing");
        let report = check(&root, &typedb, source);
        assert!(report["diagnostics"].as_array().unwrap().iter().all(|d| d["severity"] != "error"), "{source}: {report}");
    }
}

#[test]
fn gh27_does_not_suggest_successors_absent_from_the_selected_database() {
    let root = TempDir::new("gh27-absent-successors");
    let typedb = root.join("typedb");
    fs::create_dir_all(&typedb).unwrap();
    fs::write(
        typedb.join("OpenplanetCore.json"),
        r#"{"op":"synthetic test data","functions":[]}"#,
    )
    .unwrap();
    fs::write(
        typedb.join("OpenplanetNext.json"),
        r#"{"op":"synthetic test data","ns":{}}"#,
    )
    .unwrap();
    let report = check(
        &root,
        &typedb,
        "void Main() { StripFormatCodes(); ColoredString(); }",
    );
    let messages: Vec<_> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        messages,
        [
            "undefined identifier `StripFormatCodes`",
            "undefined identifier `ColoredString`"
        ]
    );
}

#[tokio::test]
#[allow(deprecated)]
async fn gh27_lsp_actions_replace_only_the_bare_callee_with_the_successor() {
    use openplanet_lsp::server::Backend;
    use tower_lsp::lsp_types::*;
    use tower_lsp::{LanguageServer, LspService};
    let typedb = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let root = TempDir::new("gh27-lsp");
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Relocation LSP\"\n",
    )
    .unwrap();
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(InitializeParams {
            root_uri: Some(Url::from_directory_path(&*root).unwrap()),
            initialization_options: Some(serde_json::json!({"openplanet_dir": typedb})),
            ..Default::default()
        })
        .await
        .unwrap();
    let uri = Url::from_file_path(root.join("Main.as")).unwrap();
    let source = "void Main() { StripFormatCodes(\"text\"); ColoredString(\"text\"); }\n";
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
    let actions = backend
        .code_action(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range::new(Position::new(0, 0), Position::new(1, 0)),
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
    for (old, new) in [
        ("StripFormatCodes", "Text::StripFormatCodes"),
        ("ColoredString", "Text::OpenplanetFormatCodes"),
    ] {
        let edits: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(a) => a.edit.as_ref(),
                _ => None,
            })
            .filter_map(|e| e.changes.as_ref().and_then(|c| c.get(&uri)))
            .flatten()
            .filter(|e| e.new_text == new)
            .collect();
        assert_eq!(edits.len(), 1, "{new}: {actions:?}");
        let start = source.find(old).unwrap() as u32;
        assert_eq!(
            edits[0].range,
            Range::new(
                Position::new(0, start),
                Position::new(0, start + old.len() as u32)
            )
        );
    }
}
