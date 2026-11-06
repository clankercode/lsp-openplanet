//! GH #74: completeness diagnostics and resumed checks at the public LSP wire.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use openplanet_lsp::cli::{format_check_report_with, parse_check_args, run_check, CheckFormat};
use openplanet_lsp::server::Backend;
use openplanet_lsp::testing::TempDir;
use serde_json::{json, Value};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::*;
use tower_lsp::{ClientSocket, LspService};

const MAIN: &str = "void Main() { Editor::NotExported(); }\n";
const EXPORT: &str = "namespace Editor { void Available() {} }\n";

struct Fixture {
    _temp: TempDir,
    consumer: PathBuf,
    plugins: PathBuf,
    dependency: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let temp = TempDir::new(label);
        let consumer = temp.join("consumer");
        let plugins = temp.join("plugins");
        let dependency = plugins.join("Editor");
        fs::create_dir_all(consumer.join("src")).unwrap();
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(consumer.join("info.toml"),
            "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n[script]\ndependencies = [\"Editor\"]\n").unwrap();
        fs::write(consumer.join("src/Main.as"), MAIN).unwrap();
        fs::write(
            dependency.join("info.toml"),
            dependency_manifest("Missing.as"),
        )
        .unwrap();
        fs::write(dependency.join("src/Available.as"), EXPORT).unwrap();
        Self {
            _temp: temp,
            consumer,
            plugins,
            dependency,
        }
    }
}

fn dependency_manifest(export: &str) -> String {
    format!("[meta]\nname = \"Editor\"\nversion = \"0.1.0\"\n[script]\nmodule = \"Editor\"\nexports = [\"{export}\"]\n")
}

/// Every operation passes through JSON-RPC dispatch; only client publications
/// are observed. Drain concurrently because tower-lsp applies backpressure.
struct Editor {
    service: LspService<Backend>,
    socket: ClientSocket,
    published: Vec<PublishDiagnosticsParams>,
}

impl Editor {
    async fn start(fixture: &Fixture, dependency_root: bool) -> Self {
        let (service, socket) = LspService::new(Backend::new);
        let mut editor = Self {
            service,
            socket,
            published: Vec::new(),
        };
        let mut roots = vec![json!({"uri": file_uri(&fixture.consumer), "name": "Consumer"})];
        if dependency_root {
            roots.push(json!({"uri": file_uri(&fixture.dependency), "name": "Editor"}));
        }
        editor
            .send(
                "initialize",
                json!({
                    "capabilities": {},
                    "rootUri": file_uri(&fixture.consumer),
                    "workspaceFolders": roots,
                    "initializationOptions": {
                        "plugins_dir": fixture.plugins,
                        "unused": "ignore",
                        "honor_gitignore": false,
                    }
                }),
                Some(1),
            )
            .await;
        editor.send("initialized", json!({}), None).await;
        editor
    }

    async fn send(&mut self, method: &'static str, params: Value, id: Option<i64>) {
        self.published.clear();
        let mut request = Request::build(method).params(params);
        if let Some(id) = id {
            request = request.id(id);
        }
        let call = self.service.ready().await.unwrap().call(request.finish());
        tokio::pin!(call);
        let socket = &mut self.socket;
        let published = &mut self.published;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    response = &mut call => {
                        if let Some(response) = response.unwrap() {
                            assert!(response.error().is_none(), "{response:?}");
                        }
                        break;
                    }
                    request = socket.next() => record(request.unwrap(), published),
                }
            }
            while let Some(Some(request)) = socket.next().now_or_never() {
                record(request, published);
            }
        })
        .await
        .expect("LSP operation completes while draining client publications");
    }

    async fn open_main(&mut self, fixture: &Fixture) {
        self.send(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": file_uri(&fixture.consumer.join("src/Main.as")),
                "languageId": "angelscript", "version": 1, "text": MAIN,
            }}),
            None,
        )
        .await;
    }

    async fn watched(&mut self, path: &Path, typ: FileChangeType) {
        self.send(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [{
                "uri": file_uri(path), "type": typ,
            }]}),
            None,
        )
        .await;
    }

    fn diagnostics(&self, path: &Path) -> &[Diagnostic] {
        let uri = file_uri(path);
        &self
            .published
            .iter()
            .rev()
            .find(|p| p.uri == uri)
            .unwrap_or_else(|| panic!("no diagnostics published for {uri}: {:#?}", self.published))
            .diagnostics
    }
}

fn record(request: Request, published: &mut Vec<PublishDiagnosticsParams>) {
    if request.method() == "textDocument/publishDiagnostics" {
        published.push(serde_json::from_value(request.params().unwrap().clone()).unwrap());
    }
}

fn file_uri(path: &Path) -> Url {
    Url::from_file_path(path).unwrap()
}

fn assert_incomplete(diagnostics: &[Diagnostic], reason: &str) {
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::WARNING)
                && d.message.contains("cannot be verified")
                && d.message.contains(reason)),
        "{diagnostics:#?}"
    );
}

fn assert_complete(diagnostics: &[Diagnostic]) {
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.message.contains("cannot be verified")),
        "{diagnostics:#?}"
    );
}

fn assert_resumed(diagnostics: &[Diagnostic]) {
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::ERROR)
                && d.message.contains("Editor::NotExported")),
        "{diagnostics:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn correcting_dependency_manifest_refreshes_consumers() {
    for dependency_root in [false, true] {
        let fixture = Fixture::new("gh74-reload-manifest");
        let mut editor = Editor::start(&fixture, dependency_root).await;
        assert_incomplete(
            editor.diagnostics(&fixture.consumer.join("info.toml")),
            "Missing.as",
        );
        editor.open_main(&fixture).await;
        assert!(editor
            .diagnostics(&fixture.consumer.join("src/Main.as"))
            .is_empty());

        let manifest = fixture.dependency.join("info.toml");
        fs::write(&manifest, dependency_manifest("Available.as")).unwrap();
        editor.watched(&manifest, FileChangeType::CHANGED).await;
        assert_complete(editor.diagnostics(&fixture.consumer.join("info.toml")));

        // A fresh public document event proves the consumer's checks resumed,
        // independently of the watcher republishing already-open buffers.
        editor.send("textDocument/didChange", json!({
            "textDocument": {"uri": file_uri(&fixture.consumer.join("src/Main.as")), "version": 2},
            "contentChanges": [{"text": MAIN}],
        }), None).await;
        assert_resumed(editor.diagnostics(&fixture.consumer.join("src/Main.as")));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn creating_or_repairing_dependency_export_refreshes_consumers() {
    for dependency_root in [false, true] {
        for export_key in ["exports", "shared_exports"] {
            let fixture = Fixture::new("gh74-reload-export");
            fs::write(
                fixture.dependency.join("info.toml"),
                dependency_manifest("Missing.as").replace("exports =", &format!("{export_key} =")),
            )
            .unwrap();
            let mut editor = Editor::start(&fixture, dependency_root).await;
            assert_incomplete(
                editor.diagnostics(&fixture.consumer.join("info.toml")),
                "Missing.as",
            );

            let export = fixture.dependency.join("src/Missing.as");
            fs::write(&export, EXPORT).unwrap();
            editor.watched(&export, FileChangeType::CREATED).await;
            assert_complete(editor.diagnostics(&fixture.consumer.join("info.toml")));
            editor.open_main(&fixture).await;
            assert_resumed(editor.diagnostics(&fixture.consumer.join("src/Main.as")));

            fs::remove_file(&export).unwrap();
            editor.watched(&export, FileChangeType::DELETED).await;
            assert_incomplete(
                editor.diagnostics(&fixture.consumer.join("info.toml")),
                "Missing.as",
            );
            assert!(editor
                .diagnostics(&fixture.consumer.join("src/Main.as"))
                .is_empty());

            fs::write(&export, [0xff]).unwrap();
            editor.watched(&export, FileChangeType::CREATED).await;
            assert_incomplete(
                editor.diagnostics(&fixture.consumer.join("info.toml")),
                "Missing.as",
            );
            fs::write(&export, EXPORT).unwrap();
            editor.watched(&export, FileChangeType::CHANGED).await;
            assert_complete(editor.diagnostics(&fixture.consumer.join("info.toml")));
            assert_resumed(editor.diagnostics(&fixture.consumer.join("src/Main.as")));
        }
    }
}

fn write_archive(path: &Path, source: Option<&[u8]>) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("info.toml", options).unwrap();
    zip.write_all(dependency_manifest("Missing.as").as_bytes())
        .unwrap();
    if let Some(source) = source {
        zip.start_file("Missing.as", options).unwrap();
        zip.write_all(source).unwrap();
    }
    zip.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn updating_dependency_archive_refreshes_consumers() {
    for unreadable in [false, true] {
        let fixture = Fixture::new("gh74-reload-archive");
        let archive = fixture.plugins.join("Editor.op");
        write_archive(&archive, unreadable.then_some(&[0xff][..]));
        fs::remove_dir_all(&fixture.dependency).unwrap();
        let mut editor = Editor::start(&fixture, false).await;
        assert_incomplete(
            editor.diagnostics(&fixture.consumer.join("info.toml")),
            "Missing.as",
        );

        write_archive(&archive, Some(EXPORT.as_bytes()));
        editor.watched(&archive, FileChangeType::CHANGED).await;
        assert_complete(editor.diagnostics(&fixture.consumer.join("info.toml")));
        editor.open_main(&fixture).await;
        assert_resumed(editor.diagnostics(&fixture.consumer.join("src/Main.as")));

        write_archive(&archive, None);
        editor.watched(&archive, FileChangeType::CHANGED).await;
        assert_incomplete(
            editor.diagnostics(&fixture.consumer.join("info.toml")),
            "Missing.as",
        );
        assert!(editor
            .diagnostics(&fixture.consumer.join("src/Main.as"))
            .is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn restoring_completeness_republishes_previously_suppressed_open_source_errors() {
    for repair in ["manifest", "export", "archive", "own-source"] {
        let fixture = Fixture::new("gh74-republish-source");
        let (changed_path, reason) = match repair {
            "manifest" => (fixture.dependency.join("info.toml"), "Missing.as"),
            "export" => (fixture.dependency.join("src/Missing.as"), "Missing.as"),
            "archive" => {
                let archive = fixture.plugins.join("Editor.op");
                write_archive(&archive, None);
                fs::remove_dir_all(&fixture.dependency).unwrap();
                (archive, "Missing.as")
            }
            "own-source" => {
                fs::write(fixture.dependency.join("src/Missing.as"), EXPORT).unwrap();
                let source = fixture.consumer.join("src/Unreadable.as");
                fs::write(&source, [0xff]).unwrap();
                (source, "workspace sources")
            }
            _ => unreachable!(),
        };
        let mut editor = Editor::start(&fixture, false).await;
        assert_incomplete(
            editor.diagnostics(&fixture.consumer.join("info.toml")),
            reason,
        );
        editor.open_main(&fixture).await;
        assert!(editor
            .diagnostics(&fixture.consumer.join("src/Main.as"))
            .is_empty());

        let typ = match repair {
            "manifest" => {
                fs::write(&changed_path, dependency_manifest("Available.as")).unwrap();
                FileChangeType::CHANGED
            }
            "export" => {
                fs::write(&changed_path, EXPORT).unwrap();
                FileChangeType::CREATED
            }
            "archive" => {
                write_archive(&changed_path, Some(EXPORT.as_bytes()));
                FileChangeType::CHANGED
            }
            "own-source" => {
                fs::write(&changed_path, "void Repaired() {}\n").unwrap();
                FileChangeType::CHANGED
            }
            _ => unreachable!(),
        };
        editor.watched(&changed_path, typ).await;
        assert_complete(editor.diagnostics(&fixture.consumer.join("info.toml")));
        assert_resumed(editor.diagnostics(&fixture.consumer.join("src/Main.as")));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_manifest_keeps_genuine_source_load_warnings_visible() {
    for missing in [false, true] {
        let fixture = Fixture::new("gh74-unavailable-manifest");
        fs::write(fixture.dependency.join("src/Missing.as"), EXPORT).unwrap();
        fs::write(fixture.consumer.join("src/Unreadable.as"), [0xff]).unwrap();
        let manifest = fixture.consumer.join("info.toml");
        if missing {
            fs::remove_file(&manifest).unwrap();
        } else {
            fs::write(&manifest, [0xff]).unwrap();
        }
        // No manifest buffer is open; initialized must still publish the
        // snapshot's real source-load failure against its manifest URI.
        let mut editor = Editor::start(&fixture, false).await;
        let diagnostics = editor.diagnostics(&manifest);
        assert_incomplete(diagnostics, "workspace sources");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("Unreadable.as")),
            "{diagnostics:#?}"
        );

        editor
            .watched(
                &fixture.consumer.join("src/Unreadable.as"),
                FileChangeType::CHANGED,
            )
            .await;
        assert_incomplete(editor.diagnostics(&manifest), "workspace sources");
    }
}

#[test]
fn dependency_define_matrix_labels_incomplete_checks_in_plain_and_json_output() {
    let fixture = Fixture::new("gh74-dependency-matrix-output");
    fs::write(fixture.consumer.join("info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n[script]\noptional_dependencies = [\"NotInstalled\"]\n").unwrap();
    fs::write(
        fixture.consumer.join("src/Main.as"),
        "void Main() {\n#if DEPENDENCY_NOTINSTALLED\n    NotInstalled::Missing();\n#endif\n}\n",
    )
    .unwrap();
    fs::write(
        fixture.consumer.join(".openplanet-lsp.toml"),
        "define_matrix = [\"DEPENDENCY_NOTINSTALLED\"]\nhonor_gitignore = false\n",
    )
    .unwrap();
    let options = parse_check_args(&[
        "--no-typedb".into(),
        "--unused=ignore".into(),
        "--plugins-dir".into(),
        fixture.plugins.display().to_string(),
        fixture.consumer.display().to_string(),
    ])
    .unwrap();
    let report = run_check(&options).unwrap();
    assert_eq!(report.diagnostics.len(), 1, "{:#?}", report.diagnostics);
    let warning = &report.diagnostics[0];
    assert_eq!(warning.path, fixture.consumer.join("info.toml"));
    assert_incomplete(std::slice::from_ref(&warning.diagnostic), "NotInstalled");
    assert_eq!(
        warning.combos,
        vec![vec!["DEPENDENCY_NOTINSTALLED".to_string()]]
    );

    let plain = format_check_report_with(&report, false, CheckFormat::Plain);
    assert!(
        plain.contains("info.toml:1:1: warning: [DEPENDENCY_NOTINSTALLED]"),
        "{plain}"
    );
    assert!(
        plain.contains("NotInstalled") && plain.contains("cannot be verified"),
        "{plain}"
    );
    assert!(plain.contains("1 diagnostics (1 warning)"), "{plain}");
    let json_text = format_check_report_with(&report, false, CheckFormat::Json);
    let json: Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(json["diagnostics"].as_array().unwrap().len(), 1);
    let diagnostic = &json["diagnostics"][0];
    assert_eq!(diagnostic["path"], "info.toml");
    assert_eq!(diagnostic["severity"], "warning");
    assert_eq!(diagnostic["combo"], json!([["DEPENDENCY_NOTINSTALLED"]]));
    assert!(diagnostic["message"]
        .as_str()
        .unwrap()
        .contains("cannot be verified"));
}
