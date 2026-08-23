//! GH #60: the LSP server honors `honor_gitignore` too, not just CLI `check`.
//!
//! CLI coverage lives in `gitignore_discovery_tests.rs` (subprocesses, so they
//! own `HOME` and can exercise the user-global layer). These tests drive
//! `Backend` in-process instead, which means `$HOME` must NOT be touched — lib
//! and integration tests share a process, and mutating it races (same reason
//! documented in `src/config.rs`). So both cases pin `honor_gitignore`
//! explicitly in the workspace `.openplanet-lsp.toml`, which wins over any
//! user-global config the developer happens to have.

use std::fs;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use openplanet_lsp::server::Backend;

/// A plugin root with `src/Main.as` plus a gitignored `generated/Gen.as`, and
/// `honor_gitignore` pinned to `honor` in the workspace config.
fn write_plugin(name: &str, honor: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "openplanet-lsp-gitignore-srv-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("generated")).unwrap();
    fs::write(root.join("info.toml"), "[meta]\n").unwrap();
    fs::write(
        root.join(".openplanet-lsp.toml"),
        format!("honor_gitignore = {honor}\n"),
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    fs::write(root.join("src/Main.as"), "void MainFunc() {}\n").unwrap();
    fs::write(root.join("generated/Gen.as"), "void GeneratedFunc() {}\n").unwrap();
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

/// Does the server's symbol union know `query`?
async fn resolves(backend: &Backend, query: &str) -> bool {
    backend
        .symbol(WorkspaceSymbolParams {
            query: query.into(),
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
        .is_some_and(|items| !items.is_empty())
}

#[tokio::test(flavor = "current_thread")]
async fn server_excludes_gitignored_files_when_honoring() {
    let root = write_plugin("honoring", true);
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();

    assert!(
        resolves(backend, "MainFunc").await,
        "src/Main.as must be part of the server's checked set"
    );
    assert!(
        !resolves(backend, "GeneratedFunc").await,
        "a gitignored file must not contribute symbols to the server's workspace"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn server_includes_gitignored_files_when_not_honoring() {
    let root = write_plugin("not-honoring", false);
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();

    assert!(
        resolves(backend, "MainFunc").await,
        "src/Main.as must be part of the server's checked set"
    );
    assert!(
        resolves(backend, "GeneratedFunc").await,
        "honor_gitignore = false must restore the gitignored file for the server too"
    );
}
