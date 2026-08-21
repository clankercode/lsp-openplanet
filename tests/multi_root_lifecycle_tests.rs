//! Multi-root phase 4: runtime root lifecycle (t9) — spec
//! `docs/superpowers/specs/2026-08-21-multi-root-plugin-workspaces.md`
//! ("Runtime root lifecycle"). Harness mirrors `multi_root_config_tests.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use openplanet_lsp::analysis_snapshot::AnalysisSnapshot;
use openplanet_lsp::server::Backend;

fn temp_parent(name: &str) -> PathBuf {
    let base =
        std::env::temp_dir().join(format!("openplanet-lsp-p4-{}-{}", name, std::process::id()));
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

fn init_params(root: &Path) -> InitializeParams {
    #[allow(deprecated)]
    InitializeParams {
        root_uri: Some(Url::from_directory_path(root).unwrap()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: Url::from_directory_path(root).unwrap(),
            name: "folder".to_string(),
        }]),
        ..Default::default()
    }
}

async fn watched(backend: &Backend, uri: &Url, typ: FileChangeType) {
    backend
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(uri.clone(), typ)],
        })
        .await;
}

fn snapshot_of(
    snaps: &[(PathBuf, std::sync::Arc<AnalysisSnapshot>)],
    root: &Path,
) -> std::sync::Arc<AnalysisSnapshot> {
    snaps
        .iter()
        .find(|(r, _)| r == root)
        .map(|(_, s)| s.clone())
        .unwrap_or_else(|| panic!("workspace for {root:?} present"))
}

/// (a) info.toml Created on disk → re-walk adds the shard.
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_manifest_created_adds_shard() {
    let parent = temp_parent("created");
    write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    // plugin-b exists but is NOT a plugin root yet (no manifest).
    let b_root = parent.join("plugin-b");
    fs::create_dir_all(&b_root).unwrap();
    fs::write(b_root.join("main.as"), "void BFunc() {}\n").unwrap();

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let result = backend.initialize(init_params(&parent)).await.unwrap();
    assert!(
        result
            .capabilities
            .workspace
            .expect("workspace capabilities (t9)")
            .workspace_folders
            .expect("workspace-folders capability declared (t9)")
            .supported
            .unwrap_or(false),
        "workspace folders must be supported (t9)"
    );
    assert_eq!(backend.workspace_snapshots().await.len(), 1);

    // Manifest appears on disk; client sends the Created event.
    fs::write(b_root.join("info.toml"), "[meta]\n").unwrap();
    let manifest_uri = Url::from_file_path(b_root.join("info.toml")).unwrap();
    watched(&backend, &manifest_uri, FileChangeType::CREATED).await;

    let snaps = backend.workspace_snapshots().await;
    assert_eq!(snaps.len(), 2, "created manifest must add plugin-b shard");

    // Symbol union sees B now.
    let items = backend
        .symbol(WorkspaceSymbolParams {
            query: "BFunc".into(),
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
        .expect("BFunc must resolve after shard add");
    assert!(
        items
            .iter()
            .any(|i| i.location.uri.to_file_path().unwrap().starts_with(&b_root)),
        "union must hit plugin-b: {:?}",
        items.iter().map(|i| &i.location.uri).collect::<Vec<_>>()
    );
}

/// (b) info.toml Deleted (root dir removed) → re-walk drops the shard.
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_manifest_deleted_drops_shard() {
    let parent = temp_parent("deleted");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&parent)).await.unwrap();
    assert_eq!(backend.workspace_snapshots().await.len(), 2);

    fs::remove_dir_all(&a_root).unwrap();
    let manifest_uri = Url::from_file_path(a_root.join("info.toml")).unwrap();
    watched(&backend, &manifest_uri, FileChangeType::DELETED).await;

    let snaps = backend.workspace_snapshots().await;
    assert_eq!(snaps.len(), 1, "deleted root must drop its shard");
    assert!(
        !snaps.iter().any(|(r, _)| *r == a_root),
        "A must be gone; roots: {:?}",
        snaps.iter().map(|(r, _)| r).collect::<Vec<_>>()
    );
}

/// (c) info.toml Changed → owning shard reloads config (new manifest
/// define) + snapshot; sibling untouched (ptr pin).
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_manifest_changed_reloads_owning_shard_only() {
    let parent = temp_parent("changed");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let b_root = write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&parent)).await.unwrap();

    let (a_before, b_before) = {
        let ws = backend.workspace_snapshots().await;
        (snapshot_of(&ws, &a_root), snapshot_of(&ws, &b_root))
    };
    assert!(
        !backend
            .workspace_configs()
            .await
            .iter()
            .find(|(r, _)| *r == a_root)
            .map(|(_, c)| c.defines.contains("DEPENDENCY_X"))
            .unwrap(),
        "precondition: DEPENDENCY_X absent"
    );

    // Edit adds a script define to A's manifest.
    fs::write(
        a_root.join("info.toml"),
        "[meta]\n\n[script]\ndefines = [\"DEPENDENCY_X\"]\n",
    )
    .unwrap();
    let manifest_uri = Url::from_file_path(a_root.join("info.toml")).unwrap();
    watched(&backend, &manifest_uri, FileChangeType::CHANGED).await;

    let has_define = backend
        .workspace_configs()
        .await
        .iter()
        .find(|(r, _)| *r == a_root)
        .map(|(_, c)| c.defines.contains("DEPENDENCY_X"))
        .expect("A shard");
    assert!(has_define, "changed manifest define must reload A's config");

    let (a_after, b_after) = {
        let ws = backend.workspace_snapshots().await;
        (snapshot_of(&ws, &a_root), snapshot_of(&ws, &b_root))
    };
    assert!(
        !std::ptr::eq(&*a_before, &*a_after),
        "A's snapshot must rebuild on manifest change"
    );
    assert!(
        std::ptr::eq(&*b_before, &*b_after),
        "B's snapshot must be untouched"
    );
}

/// (d) Non-manifest (.as) change under A rebuilds A only.
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_as_change_rebuilds_owning_shard_only() {
    let parent = temp_parent("aschange");
    let a_root = write_plugin(&parent, "plugin-a", "void AFunc() {}\n");
    let b_root = write_plugin(&parent, "plugin-b", "void BFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&parent)).await.unwrap();

    let (a_before, b_before) = {
        let ws = backend.workspace_snapshots().await;
        (snapshot_of(&ws, &a_root), snapshot_of(&ws, &b_root))
    };

    fs::write(a_root.join("main.as"), "void AFunc() { int x; }\n").unwrap();
    let src_uri = Url::from_file_path(a_root.join("main.as")).unwrap();
    watched(&backend, &src_uri, FileChangeType::CHANGED).await;

    let (a_after, b_after) = {
        let ws = backend.workspace_snapshots().await;
        (snapshot_of(&ws, &a_root), snapshot_of(&ws, &b_root))
    };
    assert!(
        !std::ptr::eq(&*a_before, &*a_after),
        "A's snapshot must rebuild on .as change"
    );
    assert!(
        std::ptr::eq(&*b_before, &*b_after),
        "B's snapshot must be untouched"
    );
}

/// (e) didChangeWorkspaceFolders: added folder with a plugin root grows
/// the shard set; removing it shrinks back.
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_workspace_folder_add_remove_rescans() {
    let parent = temp_parent("folders");
    write_plugin(&parent, "plugin-a", "void AFunc() {}\n");

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&parent)).await.unwrap();
    assert_eq!(backend.workspace_snapshots().await.len(), 1);

    // Second folder with its own plugin root.
    let parent2 = temp_parent("folders-2");
    let c_root = write_plugin(&parent2, "plugin-c", "void CFunc() {}\n");

    backend
        .did_change_workspace_folders(DidChangeWorkspaceFoldersParams {
            event: WorkspaceFoldersChangeEvent {
                added: vec![WorkspaceFolder {
                    uri: Url::from_directory_path(&parent2).unwrap(),
                    name: "folder2".to_string(),
                }],
                removed: vec![],
            },
        })
        .await;
    let snaps = backend.workspace_snapshots().await;
    assert_eq!(snaps.len(), 2, "added folder must add its shard");
    assert!(snaps.iter().any(|(r, _)| *r == c_root));

    // Remove it again.
    backend
        .did_change_workspace_folders(DidChangeWorkspaceFoldersParams {
            event: WorkspaceFoldersChangeEvent {
                added: vec![],
                removed: vec![WorkspaceFolder {
                    uri: Url::from_directory_path(&parent2).unwrap(),
                    name: "folder2".to_string(),
                }],
            },
        })
        .await;
    let snaps = backend.workspace_snapshots().await;
    assert_eq!(snaps.len(), 1, "removed folder must drop its shard");
    assert!(!snaps.iter().any(|(r, _)| *r == c_root));
}
