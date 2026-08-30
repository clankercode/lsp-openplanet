//! Multi-root phase 3: per-root config ladder (t5) + sibling deps via
//! opened-folder LAST-priority plugins_dir (t6). Spec:
//! `docs/superpowers/specs/2026-08-21-multi-root-plugin-workspaces.md`.
//!
//! Harness mirrors `multi_root_tests.rs` (real `LspService` + `Backend`).

use openplanet_lsp::testing::TempDir;
use std::fs;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use openplanet_lsp::server::Backend;

fn temp_parent(name: &str) -> TempDir {
    let base = TempDir::new(&format!("openplanet-lsp-p3-{}", name));
    base
}

fn write_plugin(parent: &Path, dir: &str, source: &str) -> PathBuf {
    let root = parent.join(dir);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("info.toml"), "[meta]\n").unwrap();
    fs::write(root.join("main.as"), source).unwrap();
    root
}

fn write_config(dir: &Path, body: &str) {
    fs::write(dir.join(".openplanet-lsp.toml"), body).unwrap();
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

fn goto_def(backend: &Backend, uri: &Url, ch: u32) -> Option<Vec<Location>> {
    // Synchronous facade is impossible; tests call backend.goto_definition
    // directly. This helper exists only for doc clarity — see tests below.
    let _ = (backend, uri, ch);
    None
}

/// (a-p3) Per-root config: define set in plugin-a/.openplanet-lsp.toml
/// applies to A only; B unaffected.
#[tokio::test(flavor = "current_thread")]
async fn per_root_config_define_applies_to_own_workspace_only() {
    let parent = temp_parent("cfg-own");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let b_root = write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    write_config(
        &a_root,
        "game_target = \"TURBO\"\ndefines = [\"CUSTOM_A\"]\n",
    );

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.to_path_buf()])))
        .await
        .unwrap();

    let configs = backend.workspace_configs().await;
    assert_eq!(configs.len(), 2);
    let a = configs
        .iter()
        .find(|(r, _)| *r == a_root)
        .map(|(_, c)| c)
        .expect("A config");
    let b = configs
        .iter()
        .find(|(r, _)| *r == b_root)
        .map(|(_, c)| c)
        .expect("B config");
    assert!(
        a.defines.contains("CUSTOM_A"),
        "A must carry CUSTOM_A; has {:?}",
        a.defines
    );
    assert!(!b.defines.contains("CUSTOM_A"), "B must not carry CUSTOM_A");
    assert_eq!(a.game_target, "TURBO");
    assert_eq!(b.game_target, "TMNEXT");
}

/// (b-p3) Parent (opened-folder) config is a base layer; per-root wins.
#[tokio::test(flavor = "current_thread")]
async fn parent_config_base_layer_per_root_overrides() {
    let parent = temp_parent("cfg-parent");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let b_root = write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    write_config(&parent, "game_target = \"MP4\"\ndefines = [\"PARENT_X\"]\n");
    write_config(&a_root, "game_target = \"TURBO\"\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.to_path_buf()])))
        .await
        .unwrap();

    let configs = backend.workspace_configs().await;
    let a = configs
        .iter()
        .find(|(r, _)| *r == a_root)
        .map(|(_, c)| c)
        .expect("A config");
    let b = configs
        .iter()
        .find(|(r, _)| *r == b_root)
        .map(|(_, c)| c)
        .expect("B config");
    assert_eq!(a.game_target, "TURBO", "per-root target must win");
    assert!(
        a.defines.contains("PARENT_X"),
        "A inherits parent define; has {:?}",
        a.defines
    );
    assert!(
        b.defines.contains("PARENT_X"),
        "B inherits parent define; has {:?}",
        b.defines
    );
    assert_eq!(b.game_target, "MP4", "B uses parent target");
}

/// (c-p3) Declared sibling dep resolves via opened-folder plugins_dir;
/// undeclared siblings stay isolated.
#[tokio::test(flavor = "current_thread")]
async fn declared_sibling_dependency_resolves_and_isolation_holds() {
    let parent = temp_parent("sib-dep");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let b_root = write_plugin(&parent, "plugin-b", "void BFunc() {}\n");
    fs::write(
        a_root.join("info.toml"),
        "[meta]\nname = \"A\"\n\n[script]\nmodule = \"PluginA\"\ndependencies = [\"plugin-b\"]\n",
    )
    .unwrap();
    fs::write(
        b_root.join("info.toml"),
        "[meta]\nname = \"B\"\n\n[script]\nmodule = \"PluginB\"\nexports = [\"main.as\"]\n",
    )
    .unwrap();
    write_plugin(&parent, "plugin-c", "void CFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.to_path_buf()])))
        .await
        .unwrap();

    let a_main = Url::from_file_path(a_root.join("main.as")).unwrap();

    // Declared dep export resolves from A.
    open_doc(backend, &a_main, "void Call() { BFunc(); }\n").await;
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
        goto.is_some(),
        "declared sibling dep BFunc must resolve from A"
    );

    // Undeclared plugin-c does NOT resolve from A (isolation).
    backend
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: a_main.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                text: "void Call() { CFunc(); }\n".into(),
                range: None,
                range_length: None,
            }],
        })
        .await;
    let goto2 = backend
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
        goto2.is_none(),
        "undeclared sibling CFunc must NOT resolve from A"
    );
    let _ = goto_def; // silence unused warning for the doc helper
}

/// (d-p3) N=1: shard root IS the plugin root; no parent layer when the
/// opened folder is the parent (today's semantics unchanged).
#[tokio::test(flavor = "current_thread")]
async fn single_root_config_unchanged() {
    let parent = temp_parent("cfg-n1");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend
        .initialize(init_params(&parent, Some(vec![parent.to_path_buf()])))
        .await
        .unwrap();

    let configs = backend.workspace_configs().await;
    assert_eq!(configs.len(), 1);
    let (root, c) = &configs[0];
    assert_eq!(*root, a_root, "N=1 shard root must be the plugin root");
    assert_eq!(c.game_target, "TMNEXT");
}
