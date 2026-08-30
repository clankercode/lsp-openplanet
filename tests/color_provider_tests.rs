//! B041: end-to-end proof that the colour provider is REACHABLE.
//!
//! The extraction itself is unit-tested in `src/server/colors.rs`, where it
//! belongs — it is a pure function and needs no server. What those tests
//! cannot show is the thing this repo has now been bitten by three times
//! (B039, B040, and the capability half of this ticket): a handler that is
//! correct, tested, and never reachable, because the capability was not
//! advertised or the method was not routed.
//!
//! So every test here boots the real service through
//! [`openplanet_lsp::server::build_service`] — the one constructor `run_stdio`
//! uses — and dispatches raw JSON-RPC exactly as a client would. A colour
//! provider implemented on `Backend` and left off the capability list fails
//! `the_server_advertises_a_color_provider`; one advertised but not routed
//! fails the round-trip tests.
//!
//! The payload is asserted as JSON rather than as the typed struct, because
//! the wire shape is the contract with VS Code: a renamed serde field would
//! be invisible to a typed assertion.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{InitializeParams, InitializeResult, Url, WorkspaceFolder};
use tower_lsp::LspService;

use openplanet_lsp::server::{build_service, Backend};

/// A plugin root with a pinned, EMPTY `openplanet_dir`.
///
/// Nothing here needs a type database — the colour provider reads tokens, not
/// types — and pinning keeps `LspConfig` from auto-detecting the developer's
/// real `$HOME/OpenplanetNext` and loading several thousand symbols per test.
/// `game_target` is pinned too: without it a user-global config naming another
/// variant sends the GH #45 re-probe back to `$HOME` (B066).
fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("oplsp-b041-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("plugin");
    let op_dir = base.join("OpenplanetNext");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&op_dir).unwrap();
    std::fs::write(
        root.join("info.toml"),
        format!("[meta]\nname = \"B041 {name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    std::fs::write(
        root.join(".openplanet-lsp.toml"),
        format!(
            "openplanet_dir = {:?}\ngame_target = \"TMNEXT\"\n",
            op_dir.to_str().unwrap()
        ),
    )
    .unwrap();
    (base, root)
}

/// Removes its tree on drop, so a run does not leave `/tmp` littered.
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn tree(name: &str) -> Fixture {
    let (base, root) = fixture(name);
    Fixture { base, root }
}

fn init_params(root: &Path) -> InitializeParams {
    let uri = Url::from_directory_path(root).unwrap();
    #[allow(deprecated)]
    InitializeParams {
        root_uri: Some(uri.clone()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri,
            name: "b041".to_string(),
        }]),
        ..Default::default()
    }
}

async fn call(service: &mut LspService<Backend>, request: Request) -> Value {
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error")
        .expect("a request gets a response");
    let (_id, result) = response.into_parts();
    result.unwrap_or_else(|err| panic!("JSON-RPC error: {err:?}"))
}

async fn notify(service: &mut LspService<Backend>, request: Request) {
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error");
    assert!(response.is_none(), "a notification gets no response");
}

/// Boot a real service, open `source` as a document, and hand back both the
/// service and the document URI.
async fn open(root: &Path, source: &str) -> (LspService<Backend>, Url, Value) {
    let (mut service, socket) = build_service();
    tokio::spawn(async move {
        let mut socket = socket;
        while socket.next().await.is_some() {}
    });
    let initialized = call(
        &mut service,
        Request::build("initialize")
            .params(serde_json::to_value(init_params(root)).unwrap())
            .id(1)
            .finish(),
    )
    .await;

    let path = root.join("src/Main.as");
    std::fs::write(&path, source).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    notify(
        &mut service,
        Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": uri, "languageId": "angelscript", "version": 1, "text": source
                }
            }))
            .finish(),
    )
    .await;
    (service, uri, initialized)
}

async fn document_color(service: &mut LspService<Backend>, uri: &Url) -> Vec<Value> {
    let result = call(
        service,
        Request::build("textDocument/documentColor")
            .params(json!({ "textDocument": { "uri": uri } }))
            .id(2)
            .finish(),
    )
    .await;
    // Strict: `unwrap_or_default` here would let a `null` result -- a handler
    // that answered nothing at all -- pass the two empty-list tests below as
    // though it had correctly answered "no colours".
    result
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("documentColor must answer a JSON array, got {result}"))
}

#[tokio::test]
async fn the_server_advertises_a_color_provider() {
    let fx = tree("caps");
    let (_service, _uri, initialized) = open(&fx.root, "void Main() {}\n").await;
    let result: InitializeResult =
        serde_json::from_value(initialized).expect("a well-formed InitializeResult");
    assert!(
        result.capabilities.color_provider.is_some(),
        "without this capability VS Code never asks for colours, so every \
         handler below is unreachable no matter how correct it is"
    );
}

#[tokio::test]
async fn document_color_is_routed_and_finds_both_forms() {
    let fx = tree("both");
    let source = "void Main() {\n    print(\"$f00red\");\n    g(vec3(0, 1, 0));\n}\n";
    let (mut service, uri, _) = open(&fx.root, source).await;

    let found = document_color(&mut service, &uri).await;
    assert_eq!(found.len(), 2, "expected both forms: {found:?}");

    // The `$f00`, on line 1, covering exactly four characters.
    assert_eq!(found[0]["range"]["start"]["line"], json!(1));
    assert_eq!(
        found[0]["range"]["end"]["character"].as_u64().unwrap()
            - found[0]["range"]["start"]["character"].as_u64().unwrap(),
        4
    );
    assert_eq!(found[0]["color"]["red"], json!(1.0));

    // The vec3, on line 2.
    assert_eq!(found[1]["range"]["start"]["line"], json!(2));
    assert_eq!(found[1]["color"]["green"], json!(1.0));
}

#[tokio::test]
async fn color_presentation_is_routed_and_round_trips_the_form() {
    let fx = tree("present");
    let source = "void Main() {\n    print(\"$f00\");\n    g(vec3(1, 0, 0));\n}\n";
    let (mut service, uri, _) = open(&fx.root, source).await;
    let found = document_color(&mut service, &uri).await;
    assert_eq!(found.len(), 2, "{found:?}");

    // Blue is requested below, and each form must come back as blue IN THAT
    // FORM. Asserting only the `$`/`vec3(` prefix would pass even if the
    // handler ignored the requested colour and echoed the red already in the
    // source, which is the one failure this test exists to catch.
    for (site, expected) in [(&found[0], "$00f"), (&found[1], "vec3(0, 0, 1)")] {
        let result = call(
            &mut service,
            Request::build("textDocument/colorPresentation")
                .params(json!({
                    "textDocument": { "uri": uri },
                    "color": { "red": 0.0, "green": 0.0, "blue": 1.0, "alpha": 1.0 },
                    "range": site["range"],
                }))
                .id(3)
                .finish(),
        )
        .await;
        let presentations = result.as_array().cloned().unwrap_or_default();
        assert_eq!(presentations.len(), 1, "{presentations:?}");
        let label = presentations[0]["label"].as_str().unwrap_or_default();
        assert_eq!(
            label, expected,
            "the picker must write back the REQUESTED colour in the form it \
             read"
        );
        // And it must carry an edit, or the picker has nothing to apply.
        assert_eq!(presentations[0]["textEdit"]["newText"], json!(label));
    }
}

/// The blocking finding from the review of the first B041 commit: a `$xxx`
/// inside a `$l[...]` payload is part of a URL, not a colour, and offering a
/// picker there means the edit rewrites somebody's link.
#[tokio::test]
async fn a_colour_inside_a_link_payload_is_not_offered() {
    let fx = tree("link");
    let source = "void Main() {\n    print(\"$l[https://x.example/$f00]go$l\");\n}\n";
    let (mut service, uri, _) = open(&fx.root, source).await;
    let found = document_color(&mut service, &uri).await;
    assert!(
        found.is_empty(),
        "link payload is not colour text: {found:?}"
    );
}

#[tokio::test]
async fn a_document_with_no_colours_answers_an_empty_list_not_an_error() {
    let fx = tree("empty");
    let (mut service, uri, _) = open(&fx.root, "void Main() { print(\"hi\"); }\n").await;
    assert!(document_color(&mut service, &uri).await.is_empty());
}

#[tokio::test]
async fn an_unopened_document_answers_an_empty_list_not_a_panic() {
    let fx = tree("unopened");
    let (mut service, _uri, _) = open(&fx.root, "void Main() {}\n").await;
    let stray = Url::from_file_path(fx.root.join("src/NotOpen.as")).unwrap();
    assert!(document_color(&mut service, &stray).await.is_empty());
}
