//! Multi-root phase 2: N isolated plugin workspaces (spec
//! `docs/superpowers/specs/2026-08-21-multi-root-plugin-workspaces.md`).
//!
//! Same harness shape as the smoke test in `integration_tests.rs`: real
//! `LspService` + `Backend`, trait-method calls; `publish_diagnostics` is
//! suppressed by tower-lsp 0.20 until the service transitions to
//! `Initialized` (we never do), so nothing drains the socket.

use std::fs;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use openplanet_lsp::analysis_snapshot::AnalysisSnapshot;
use openplanet_lsp::server::Backend;

fn temp_parent(name: &str) -> PathBuf {
    let base =
        std::env::temp_dir().join(format!("openplanet-lsp-p2-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    base
}

fn write_plugin(parent: &Path, dir: &str, source: &str) -> PathBuf {
    let root = parent.join(dir);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("info.toml"), "[meta]\n").unwrap();
    fs::write(root.join("main.as"), source).unwrap();
    root
}

fn init_params(root: &Path, folders: Option<Vec<PathBuf>>) -> InitializeParams {
    #[allow(deprecated)]
    InitializeParams {
        root_uri: Some(Url::from_directory_path(root).unwrap()),
        workspace_folders: folders.map(|fs| {
            fs.into_iter()
                .map(|uri| WorkspaceFolder {
                    uri: Url::from_directory_path(uri).unwrap(),
                    name: "folder".to_string(),
                })
                .collect()
        }),
        ..Default::default()
    }
}

async fn open_doc(backend: &Backend, uri: &Url, text: &str) {
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "angelscript".into(),
                version: 1,
                text: text.into(),
            },
        })
        .await;
}

fn pos(line: u32, character: u32) -> Position {
    Position { line, character }
}

/// (a) Isolation pin: sibling B's symbols are unresolvable from A's file.
#[tokio::test(flavor = "current_thread")]
async fn multi_root_isolation_sibling_symbols_do_not_resolve() {
    let parent = temp_parent("isolation");
    let a_root = write_plugin(&parent, "plugin-a", "void BFunc() {}\n");
    write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.clone()])))
        .await
        .unwrap();

    let a_main = Url::from_file_path(a_root.join("main.as")).unwrap();
    //              0123456789012345678
    let src = "void Call() { BFunc(); }\n";
    open_doc(backend, &a_main, src).await;

    // hover over `BFunc` at line 0, character 15
    let hover = backend
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: a_main.clone(),
                },
                position: pos(0, 15),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    // Isolation: BFunc must NOT resolve to plugin-b's definition. Hover on
    // an unresolved name reports the error-type hover (`<error:BFunc>`),
    // never sibling-b's `void BFunc()` signature.
    match hover {
        None => {}
        Some(h) => {
            let text = match h.contents {
                HoverContents::Markup(m) => m.value,
                HoverContents::Scalar(MarkedString::String(s)) => s,
                HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
                HoverContents::Array(items) => items
                    .into_iter()
                    .map(|i| match i {
                        MarkedString::String(s) => s,
                        MarkedString::LanguageString(ls) => ls.value,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            assert!(
                !text.contains("void BFunc"),
                "hover leaked sibling-b's signature: {text}"
            );
        }
    }

    let goto = backend
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: a_main.clone(),
                },
                position: pos(0, 15),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(
        goto.is_none(),
        "goto-definition of sibling-defined BFunc must return None (isolation)"
    );
}

/// (b) t8 union: workspace/symbol query matching both roots returns
/// locations from both.
#[tokio::test(flavor = "current_thread")]
async fn multi_root_workspace_symbol_union() {
    let parent = temp_parent("union");
    write_plugin(&parent, "plugin-a", "void SharedName() {}\n");
    write_plugin(&parent, "plugin-b", "void SharedName() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.clone()])))
        .await
        .unwrap();

    let items = backend
        .symbol(WorkspaceSymbolParams {
            query: "SharedName".into(),
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("union query must return results");
    let a_dir = parent.join("plugin-a");
    let b_dir = parent.join("plugin-b");
    let hits_a = items
        .iter()
        .any(|i| i.location.uri.to_file_path().unwrap().starts_with(&a_dir));
    let hits_b = items
        .iter()
        .any(|i| i.location.uri.to_file_path().unwrap().starts_with(&b_dir));
    assert!(
        hits_a && hits_b,
        "symbol query must hit both roots; got {:?}",
        items.iter().map(|i| &i.location.uri).collect::<Vec<_>>()
    );
}

/// (c) Per-workspace rebuild: a change in A replaces A's snapshot Arc while
/// B's stays pointer-identical.
#[tokio::test(flavor = "current_thread")]
async fn multi_root_per_workspace_rebuild_pin() {
    let parent = temp_parent("rebuild");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let b_root = write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.clone()])))
        .await
        .unwrap();

    let snapshot_of = |snaps: &[(PathBuf, std::sync::Arc<AnalysisSnapshot>)],
                       root: &Path|
     -> std::sync::Arc<AnalysisSnapshot> {
        snaps
            .iter()
            .find(|(r, _)| r == root)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| panic!("workspace for {root:?} present"))
    };
    let (before_a, before_b) = {
        let ws = backend.workspace_snapshots().await;
        (snapshot_of(&ws, &a_root), snapshot_of(&ws, &b_root))
    };

    let a_uri = Url::from_file_path(a_root.join("main.as")).unwrap();
    backend
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: a_uri,
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                text: "void AFunc() { int x; }\n".into(),
                range: None,
                range_length: None,
            }],
        })
        .await;

    let (after_a, after_b) = {
        let ws = backend.workspace_snapshots().await;
        (snapshot_of(&ws, &a_root), snapshot_of(&ws, &b_root))
    };
    assert!(
        !std::ptr::eq(&*before_a, &*after_a),
        "A's snapshot Arc must be replaced by the rebuild"
    );
    assert!(
        std::ptr::eq(&*before_b, &*after_b),
        "B's snapshot Arc must remain untouched by A's edit"
    );
}

/// (d) Stray `.as` at the parent level (under no plugin root): parse-level
/// fallback only — no workspace symbols resolve.
#[tokio::test(flavor = "current_thread")]
async fn multi_root_stray_file_gets_fallback_parse_only() {
    let parent = temp_parent("stray");
    write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let stray = parent.join("stray.as");
    fs::write(&stray, "void Call() { AFunc(); }\n").unwrap();

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.clone()])))
        .await
        .unwrap();

    let stray_uri = Url::from_file_path(&stray).unwrap();
    open_doc(backend, &stray_uri, "void Call() { AFunc(); }\n").await;

    let hover = backend
        .hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: stray_uri.clone(),
                },
                position: pos(0, 15),
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap();
    assert!(
        hover.is_none(),
        "stray file must not resolve plugin workspace symbols"
    );
}

/// (e) Manifest routing: `info.toml` requests are served by the owning
/// workspace's snapshot (helper for phase-4 diagnostics fan-out; here we pin
/// that opening A's manifest doesn't blow up and the workspace set stays
/// stable).
#[tokio::test(flavor = "current_thread")]
async fn multi_root_manifest_open_is_routed_to_owner() {
    let parent = temp_parent("manifest");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.clone()])))
        .await
        .unwrap();

    let manifest_uri = Url::from_file_path(a_root.join("info.toml")).unwrap();
    open_doc(backend, &manifest_uri, "[meta]\n").await;

    let ws = backend.workspace_snapshots().await;
    assert_eq!(ws.len(), 2, "workspace set must stay stable at 2");
    assert!(
        ws.iter().any(|(r, _)| *r == a_root),
        "A's workspace still present after opening its manifest"
    );
}
