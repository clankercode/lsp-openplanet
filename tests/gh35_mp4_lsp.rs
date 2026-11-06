//! GH35 observes initialization and diagnostics at the public LSP wire.
use std::path::Path;
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use openplanet_lsp::server::Backend;
use openplanet_lsp::testing::{EnvVars, TempDir};
use serde_json::{json, Value};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::*;
use tower_lsp::{ClientSocket, LspService};

#[path = "support/gh35_mp4.rs"]
mod support;
use support::{copy_mp4_snapshot, mp4_snapshot, plugin, MP4_SOURCE};

struct Editor {
    service: LspService<Backend>,
    socket: ClientSocket,
    published: Vec<PublishDiagnosticsParams>,
}

impl Editor {
    async fn start(plugin: &Path, options: Value) -> Self {
        let (service, socket) = LspService::new(Backend::new);
        let mut editor = Self {
            service,
            socket,
            published: Vec::new(),
        };
        editor
            .send(
                "initialize",
                json!({
                    "rootUri": Url::from_directory_path(plugin).unwrap(),
                    "capabilities": {},
                    "initializationOptions": options,
                }),
                Some(1),
            )
            .await;
        editor
    }

    async fn send(&mut self, method: &'static str, params: Value, id: Option<i64>) {
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
        .expect("LSP request completes with client publications drained");
    }

    async fn open(&mut self, plugin: &Path, source: &str) -> Vec<Diagnostic> {
        let uri = Url::from_file_path(plugin.join("src/Main.as")).unwrap();
        self.send(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "angelscript", "version": 1, "text": source,
            }}),
            None,
        )
        .await;
        self.published
            .iter()
            .rev()
            .find(|p| p.uri == uri)
            .expect("source diagnostics were published")
            .diagnostics
            .clone()
    }
}

fn record(request: Request, published: &mut Vec<PublishDiagnosticsParams>) {
    if request.method() == "textDocument/publishDiagnostics" {
        published.push(serde_json::from_value(request.params().unwrap().clone()).unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_loads_authentic_mp4_by_discovery_and_explicit_install() {
    let env = EnvVars::capture(&["HOME", "USERPROFILE"]);
    for target in ["MP4", "MP40", "MP41"] {
        for explicit in [false, true] {
            let root = TempDir::new("gh35-lsp-loading");
            env.set("HOME", &root);
            env.set("USERPROFILE", &root);
            let install = root.join(if explicit {
                "custom-install"
            } else {
                "Openplanet4"
            });
            copy_mp4_snapshot(&install, "Openplanet4.json");
            let plugin = plugin(&root, MP4_SOURCE);
            let mut options =
                json!({"game_target": target, "unused": "ignore", "inactive_regions": "ignore"});
            if explicit {
                options["openplanet_dir"] = json!(install);
            }
            let mut editor = Editor::start(&plugin, options).await;
            assert_eq!(
                editor.open(&plugin, MP4_SOURCE).await,
                Vec::<Diagnostic>::new(),
                "target={target} explicit={explicit}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_still_reports_real_unknown_mp4_members() {
    let env = EnvVars::capture(&["HOME", "USERPROFILE"]);
    let root = TempDir::new("gh35-lsp-negative-control");
    env.set("HOME", &root);
    env.set("USERPROFILE", &root);
    copy_mp4_snapshot(&root.join("Openplanet4"), "Openplanet4.json");
    let source = MP4_SOURCE.replace("script.RequestsSpectate", "script.Gh35MissingMember");
    let plugin = plugin(&root, &source);
    let mut editor =
        Editor::start(&plugin, json!({"game_target": "MP4", "unused": "ignore"})).await;
    let diagnostics = editor.open(&plugin, &source).await;
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::ERROR)
                && d.message.contains("Gh35MissingMember")),
        "{diagnostics:#?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.message.contains("unknown type")),
        "{diagnostics:#?}"
    );
}

// Recorded snapshots must resolve with the same game target as discovery.
#[tokio::test(flavor = "current_thread")]
async fn lsp_recorded_only_mp4_uses_selected_game_target() {
    let env = EnvVars::capture(&["HOME", "USERPROFILE"]);
    let root = TempDir::new("gh35-lsp-recorded-only");
    env.set("HOME", &root);
    env.set("USERPROFILE", &root);
    let plugin = plugin(&root, MP4_SOURCE);
    let mut editor = Editor::start(
        &plugin,
        json!({
            "game_target": "MP4", "unused": "ignore", "inactive_regions": "ignore",
            "openplanet_version": "1.29.14",
            "typedb_versions_dir": mp4_snapshot().parent().unwrap(),
        }),
    )
    .await;
    assert_eq!(
        editor.open(&plugin, MP4_SOURCE).await,
        Vec::<Diagnostic>::new()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_version_requests_never_substitute_an_installed_or_other_variant() {
    let env = EnvVars::capture(&["HOME", "USERPROFILE"]);
    let source = "void Probe(CGameCtnApp@ app) {}";
    for target in ["MP4", "MP40", "MP41", "TMNEXT"] {
        let root = TempDir::new("gh35-lsp-version-selection");
        env.set("HOME", &root);
        env.set("USERPROFILE", &root);
        let versions = root.join("versions");
        copy_mp4_snapshot(&versions.join("1.29.14"), "Openplanet4.json");
        let next_fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-versions/1.29.5");
        for destination in [versions.join("1.29.5"), root.join("OpenplanetNext")] {
            std::fs::create_dir_all(&destination).unwrap();
            for name in ["OpenplanetCore.json", "OpenplanetNext.json", "Openplanet.h"] {
                std::fs::copy(next_fixture.join(name), destination.join(name)).unwrap();
            }
        }
        copy_mp4_snapshot(&root.join("Openplanet4"), "Openplanet4.json");
        let (available, other_variant) = if target == "TMNEXT" {
            ("1.29.5", "1.29.14")
        } else {
            ("1.29.14", "1.29.5")
        };
        let plugin = plugin(&root, source);
        for (requested, should_load) in [
            (available, true),
            (other_variant, false),
            ("99.99.99", false),
        ] {
            let mut editor = Editor::start(
                &plugin,
                json!({
                    "game_target": target, "unused": "ignore",
                    "openplanet_version": requested,
                    "typedb_versions_dir": versions,
                }),
            )
            .await;
            let diagnostics = editor.open(&plugin, source).await;
            if should_load {
                assert!(
                    diagnostics.is_empty(),
                    "target={target} version={requested}: {diagnostics:?}"
                );
            } else {
                assert!(
                    diagnostics.iter().any(|d| d.severity == Some(DiagnosticSeverity::ERROR)
                        && d.message.contains("unknown type `CGameCtnApp`")),
                    "target={target} version={requested} must not load the installed fallback: {diagnostics:?}"
                );
            }
        }
    }
}
