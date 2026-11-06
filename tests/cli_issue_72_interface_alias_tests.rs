//! GH #72: primitive aliases through CLI check and LSP diagnostics/code actions.

use futures_util::StreamExt;
use openplanet_lsp::server::{build_service, Backend};
use openplanet_lsp::testing::TempDir;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::time::Duration;
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{TextEdit, Url};
use tower_lsp::LspService;

const ALIAS_PAIRS: &[(&str, &str)] = &[
    ("uint32", "uint"),
    ("uint", "uint32"),
    ("int32", "int"),
    ("int", "int32"),
];

fn plugin(source: &str) -> TempDir {
    let root = TempDir::new("issue-72-interface-aliases");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("info.toml"),
        "[meta]\nname = \"Interface aliases\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(root.join("src/Main.as"), source).unwrap();
    let openplanet = root.join("OpenplanetNext");
    fs::create_dir_all(&openplanet).unwrap();
    fs::write(
        root.join(".openplanet-lsp.toml"),
        format!(
            "openplanet_dir = {:?}\ngame_target = \"TMNEXT\"\nunused = \"ignore\"\n",
            openplanet.to_str().unwrap()
        ),
    )
    .unwrap();
    root
}

fn check_source(source: &str) -> (bool, Value) {
    let root = plugin(source);
    let output = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .args(["check", "--no-typedb", "--unused=ignore", "--format=json"])
        .arg(&root)
        .env("HOME", &root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!(
            "invalid check JSON: {err}; stdout={stdout}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.success(), report)
}

#[test]
fn cli_accepts_an_interface_return_primitive_alias() {
    for &(required, provided) in ALIAS_PAIRS {
        let source = format!(
            "interface I {{ {required} M(); }}\n\
             class C : I {{ {provided} M() {{ return 0; }} }}\n\
             void Main() {{}}\n"
        );
        assert_clean(&source);
    }
}

fn assert_clean(source: &str) {
    let (ok, report) = check_source(source);
    assert!(ok, "implementation must pass: {report}\n{source}");
    assert_eq!(
        report["diagnostics"],
        serde_json::json!([]),
        "{report}\n{source}"
    );
}

#[test]
fn cli_accepts_primitive_aliases_in_parameters_and_combined_signatures() {
    for &(required, provided) in ALIAS_PAIRS {
        for (return_required, return_provided, body) in
            [("void", "void", ""), (required, provided, "return value;")]
        {
            let source = format!(
                "interface I {{ {return_required} M({required} value, int count); }}\n\
                 class C : I {{ {return_provided} M({provided} value, int32 count) {{ {body} }} }}\n\
                 void Main() {{}}\n"
            );
            assert_clean(&source);
        }
    }
}

#[test]
fn cli_accepts_aliases_with_matching_reference_modifiers() {
    for &(required, provided) in ALIAS_PAIRS {
        for shape in ["const T &in", "T &in", "T &out"] {
            let required = shape.replace('T', required);
            let provided = shape.replace('T', provided);
            assert_clean(&format!(
                "interface I {{ void M({required} value); }}\n\
                 class C : I {{ void M({provided} value) {{}} }}\n\
                 void Main() {{}}\n"
            ));
        }
    }
}

fn assert_missing(source: &str, signature: &str) {
    let (ok, report) = check_source(source);
    assert!(
        !ok,
        "a real signature mismatch must fail: {report}\n{source}"
    );
    assert_eq!(
        report["diagnostics"],
        serde_json::json!([{
            "path": "src/Main.as", "line": 2, "col": 7, "severity": "error", "combo": [],
            "message": format!("Missing implementation of '{signature}'"),
        }]),
        "{report}\n{source}"
    );
}

#[test]
fn cli_normalizes_nested_parameter_aliases_without_hiding_return_mismatches() {
    for &(required, provided) in ALIAS_PAIRS {
        for (shape, rendered) in [
            ("T[]", "T[]"),
            ("array<T>", "array<T>"),
            ("array<array<T>>", "array<array<T>>"),
            ("array<T>@", "array<T>@"),
            ("const array<T>@ &in", "const array<T>@&in"),
            ("array<T>@ const &in", "array<T>@const&in"),
            ("array<T> &inout", "array<T>&inout"),
        ] {
            let signature = format!("int I::M({})", rendered.replace('T', required));
            let required = shape.replace('T', required);
            let provided = shape.replace('T', provided);
            assert_clean(&format!(
                "interface I {{ void M({required} values); }}\n\
                 class C : I {{ void M({provided} values) {{}} }}\n\
                 void Main() {{}}\n"
            ));

            // A clean nested case alone is vacuous: the old checker treats
            // differing nested text as uncertain. Once the alias agrees,
            // the known return mismatch must remain visible.
            let source = format!(
                "interface I {{ int M({required} values); }}\n\
                 class C : I {{ float M({provided} values) {{ return 0.0f; }} }}\n\
                 void Main() {{}}\n"
            );
            assert_missing(&source, &signature);
        }
    }
}

#[test]
fn cli_normalizes_nested_return_aliases_without_hiding_parameter_mismatches() {
    for &(required, provided) in ALIAS_PAIRS {
        for shape in ["T[]", "array<T>", "array<array<T>>", "array<T>@"] {
            let required = shape.replace('T', required);
            let provided = shape.replace('T', provided);
            for (param, mismatch) in [("int", false), ("float", true)] {
                let source = format!(
                    "interface I {{ {required} M(int value); }}\n\
                     class C : I {{ {provided} M({param} value) {{ {provided} result; return result; }} }}\n\
                     void Main() {{}}\n"
                );
                if mismatch {
                    assert_missing(&source, &format!("{required} I::M(int)"));
                } else {
                    assert_clean(&source);
                }
            }
        }
    }
}

#[test]
fn cli_keeps_signedness_width_and_numeric_type_mismatches() {
    for (required, provided) in [
        ("int32", "uint"),
        ("int", "uint32"),
        ("uint32", "int"),
        ("uint", "int32"),
        ("int32", "int8"),
        ("int8", "int32"),
        ("int32", "int16"),
        ("int16", "int32"),
        ("int32", "int64"),
        ("int64", "int32"),
        ("uint32", "uint8"),
        ("uint8", "uint32"),
        ("uint32", "uint16"),
        ("uint16", "uint32"),
        ("uint32", "uint64"),
        ("uint64", "uint32"),
        ("int32", "float"),
        ("float", "int32"),
        ("uint32", "double"),
        ("double", "uint32"),
    ] {
        assert_missing(
            &format!(
                "interface I {{ {required} M(int count); }}\n\
                 class C : I {{ {provided} M(int32 count) {{ return 0; }} }}\n"
            ),
            &format!("{required} I::M(int)"),
        );
        assert_missing(
            &format!(
                "interface I {{ int32 M({required} value); }}\n\
                 class C : I {{ int M({provided} value) {{ return 0; }} }}\n"
            ),
            &format!("int32 I::M({required})"),
        );
    }
}

#[test]
fn cli_preserves_modifiers_arity_and_identifier_boundaries() {
    for (required, provided, signature) in [
        (
            "void M(const int32 &in value);",
            "void M(int &in value) {}",
            "void I::M(const int32&in)",
        ),
        (
            "void M(int32 &in value);",
            "void M(int &out value) {}",
            "void I::M(int32&in)",
        ),
        (
            "void M(int32 &out value);",
            "void M(int &in value) {}",
            "void I::M(int32&out)",
        ),
        (
            "int32 M(Thing &inout value);",
            "int M(Thing &in value) { return 0; }",
            "int32 I::M(Thing&inout)",
        ),
        (
            "int32 M(Thing@ value);",
            "int M(Thing value) { return 0; }",
            "int32 I::M(Thing@)",
        ),
        (
            "int32 M(const Thing@ value);",
            "int M(Thing@ const value) { return 0; }",
            "int32 I::M(const Thing@)",
        ),
        ("int32& M();", "int M() { return 0; }", "int32& I::M()"),
        (
            "uint32 M(uint32 value) const;",
            "uint M(uint value) { return value; }",
            "uint32 I::M(uint32) const",
        ),
        (
            "void M(int32 value, int count = 0);",
            "void M(int value) {}",
            "void I::M(int32,int)",
        ),
        (
            "uint32 M(uintBox value);",
            "uint M(uint32Box value) { return 0; }",
            "uint32 I::M(uintBox)",
        ),
        (
            "int32Box M(int value);",
            "intBox M(int32 value) { intBox result; return result; }",
            "int32Box I::M(int)",
        ),
    ] {
        assert_missing(
            &format!(
                "class Thing {{}} class uintBox {{}} class uint32Box {{}} class intBox {{}} class int32Box {{}} \
                 interface I {{ {required} }}\nclass C : I {{ {provided} }}\n"
            ),
            signature,
        );
    }
}

#[test]
fn cli_keeps_inherited_overloaded_and_uncertain_implementations_silent() {
    for &(required, provided) in ALIAS_PAIRS {
        assert_clean(&format!(
            "namespace N {{\n\
             interface I {{ {required} M({required} value) const; }}\n\
             interface J : I {{}}\n\
             class B {{ {provided} M({provided} value) const {{ return value; }} }}\n\
             class C : B, J {{ void M(float value) {{}} }}\n\
             }}\nvoid Main() {{}}\n"
        ));
        // The reverse constness direction and typedef differences are
        // deliberately uncertain in the existing interface checker.
        assert_clean(&format!(
            "interface I {{ {required} M(); }}\n\
             class C : I {{ {provided} M() const {{ return 0; }} }}\n"
        ));
    }
    assert_clean(
        "typedef uint32 Counter;\n\
         interface I { int M(Counter value); }\n\
         class C : I { float M(uint value) { return 0; } }\n",
    );
}

async fn rpc(
    service: &mut LspService<Backend>,
    method: &'static str,
    params: Value,
    id: Option<i64>,
) -> Value {
    let mut request = Request::build(method).params(params);
    if let Some(id) = id {
        request = request.id(id);
    }
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request.finish())
        .await
        .expect("no transport error");
    if id.is_some() {
        response
            .expect("request response")
            .into_parts()
            .1
            .unwrap_or_else(|err| panic!("{method}: {err}"))
    } else {
        assert!(response.is_none(), "notification returned {response:?}");
        Value::Null
    }
}

async fn published_diagnostics(
    notifications: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    uri: &Url,
) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(params) = notifications.recv().await {
            if params["uri"] == uri.as_str() {
                return params["diagnostics"].clone();
            }
        }
        panic!("diagnostic stream closed for {uri}");
    })
    .await
    .expect("publishDiagnostics must arrive")
}

#[tokio::test]
async fn lsp_alias_diagnostics_and_resolved_interface_stubs_round_trip() {
    for &(required, provided) in ALIAS_PAIRS {
        let mut source = format!(
            "interface I {{ {required} Done({required} value); \
             {required} Need(const {required} &in original) const; {required} Need(float scale); }}\n\
             class C : I {{ {provided} Done({provided} value) {{ return value; }} }}\n\
             void Main() {{}}\n"
        );
        let root = plugin(&source);
        let uri = Url::from_file_path(root.join("src/Main.as")).unwrap();
        let (mut service, mut socket) = build_service();
        let (sender, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(request) = socket.next().await {
                if request.method() == "textDocument/publishDiagnostics" {
                    let _ = sender.send(request.params().unwrap().clone());
                }
            }
        });
        let initialized = rpc(
            &mut service,
            "initialize",
            json!({
                "rootUri": Url::from_directory_path(&root).unwrap(),
                "capabilities": { "textDocument": { "codeAction": {
                    "dataSupport": true, "resolveSupport": { "properties": ["edit"] }
                } } }
            }),
            Some(1),
        )
        .await;
        assert_eq!(
            initialized["capabilities"]["codeActionProvider"]["resolveProvider"],
            true
        );
        rpc(
            &mut service,
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": "angelscript", "version": 1, "text": source
            } }),
            None,
        )
        .await;

        let signatures = [
            format!("{required} I::Need(const {required}&in) const"),
            format!("{required} I::Need(float)"),
        ];
        for remaining in 0..=signatures.len() {
            let diagnostics = published_diagnostics(&mut notifications, &uri).await;
            let diagnostics = diagnostics.as_array().expect("diagnostics array");
            assert_eq!(
                diagnostics.len(),
                signatures.len() - remaining,
                "{diagnostics:?}"
            );
            for (diag, signature) in diagnostics.iter().zip(&signatures[remaining..]) {
                assert_eq!(
                    diag["message"],
                    format!("Missing implementation of '{signature}'")
                );
                assert_eq!(diag["severity"], 1);
                assert_eq!(
                    diag["range"],
                    json!({
                        "start": { "line": 1, "character": 6 },
                        "end": { "line": 1, "character": 7 }
                    })
                );
            }
            let actions = rpc(
                &mut service,
                "textDocument/codeAction",
                json!({
                    "textDocument": { "uri": uri },
                    "range": {
                        "start": { "line": 1, "character": 6 },
                        "end": { "line": 1, "character": 7 }
                    },
                    "context": { "diagnostics": diagnostics, "only": ["quickfix"] }
                }),
                Some(2),
            )
            .await;
            let actions: Vec<_> = actions
                .as_array()
                .expect("code actions array")
                .iter()
                .filter(|action| action["title"].as_str().unwrap().starts_with("Implement `"))
                .collect();
            let mut titles: Vec<_> = actions
                .iter()
                .map(|a| a["title"].as_str().unwrap())
                .collect();
            titles.sort_unstable();
            let mut expected: Vec<_> = signatures[remaining..]
                .iter()
                .map(|signature| format!("Implement `{signature}`"))
                .collect();
            expected.sort();
            assert_eq!(titles, expected);
            if remaining == signatures.len() {
                break;
            }
            let action = actions
                .iter()
                .find(|action| action["title"] == format!("Implement `{}`", signatures[remaining]))
                .unwrap();
            assert!(action["edit"].is_null(), "resolve must supply the edit");
            assert!(
                action["data"].is_object(),
                "resolve needs opaque action data"
            );
            let resolved = rpc(
                &mut service,
                "codeAction/resolve",
                (*action).clone(),
                Some(3),
            )
            .await;
            let edits = resolved["edit"]["changes"][uri.as_str()]
                .as_array()
                .expect("document edits");
            assert_eq!(edits.len(), 1);
            let edit: TextEdit = serde_json::from_value(edits[0].clone()).unwrap();
            let expected_stub = if remaining == 0 {
                format!("{required} Need(const {required} &in original) const")
            } else {
                format!("{required} Need(float scale)")
            };
            assert!(edit.new_text.contains(&expected_stub), "{}", edit.new_text);
            assert_eq!(edit.range.start, edit.range.end, "stub must be inserted");
            // Fixtures are ASCII, so LSP UTF-16 columns are byte columns.
            assert!(source.is_ascii());
            let offset = source
                .split_inclusive('\n')
                .take(edit.range.start.line as usize)
                .map(str::len)
                .sum::<usize>()
                + edit.range.start.character as usize;
            source.insert_str(offset, &edit.new_text);
            rpc(
                &mut service,
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": remaining + 2 },
                    "contentChanges": [{ "text": source }]
                }),
                None,
            )
            .await;
        }
        assert_clean(&source);
        reader.abort();
    }
}
