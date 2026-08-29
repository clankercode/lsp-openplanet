//! Live reload of workspace `.openplanet-lsp.toml` and user-global
//! `$HOME/.config/openplanet-lsp/config.toml` without a process restart.
//!
//! `$HOME` is never mutated in-process (lib + integration tests share a
//! process). User-path classification lives in `config_watch`; this file
//! drives `Backend` through workspace-file events and the public reload
//! entry point.

use std::fs;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use openplanet_lsp::server::Backend;

fn temp_plugin(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "openplanet-lsp-cfgreload-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("generated")).unwrap();
    fs::create_dir_all(root.join("extra")).unwrap();
    fs::write(root.join("info.toml"), "[meta]\n").unwrap();
    fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    fs::write(root.join("src/Main.as"), "void MainFunc() {}\n").unwrap();
    fs::write(root.join("generated/Gen.as"), "void GeneratedFunc() {}\n").unwrap();
    fs::write(root.join("extra/Extra.as"), "void ExtraFunc() {}\n").unwrap();
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

async fn watched(backend: &Backend, path: &Path, typ: FileChangeType) {
    backend
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(Url::from_file_path(path).unwrap(), typ)],
        })
        .await;
}

async fn next_show_message(socket: &mut tower_lsp::ClientSocket) -> ShowMessageParams {
    loop {
        let request = socket.next().await.expect("client socket remains open");
        if request.method() != "window/showMessage" {
            continue;
        }
        return serde_json::from_value(
            request
                .params()
                .expect("showMessage carries params")
                .clone(),
        )
        .expect("valid showMessage params");
    }
}

fn assert_whole_file_toml_warning(warning: &ShowMessageParams, config_file: &Path) {
    assert_eq!(warning.typ, MessageType::WARNING);
    assert!(
        warning.message.contains(&config_file.display().to_string()),
        "warning must carry the structured issue file: {warning:?}"
    );
    assert!(
        warning
            .message
            .contains("expected valid TOML (the whole file was skipped)"),
        "warning must carry the whole-file expected field: {warning:?}"
    );
    assert!(
        warning.message.contains("found: invalid array"),
        "warning must carry the parse error in the found field: {warning:?}"
    );
    assert!(
        !warning.message.contains("key ``"),
        "whole-file issues must render without a key: {warning:?}"
    );
}

async fn config_of(backend: &Backend) -> openplanet_lsp::config::LspConfig {
    backend
        .workspace_configs()
        .await
        .into_iter()
        .next()
        .map(|(_, c)| c)
        .expect("one shard")
}

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

/// Create / modify / delete of the workspace file must reload the ladder.
#[tokio::test(flavor = "current_thread")]
async fn workspace_config_create_modify_delete_reloads() {
    let root = temp_plugin("ws-lifecycle");
    let cfg = root.join(".openplanet-lsp.toml");
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();

    let before = config_of(backend).await;
    assert_eq!(before.game_target, "TMNEXT");
    assert!(!before.defines.contains("RELOAD_DEF"));
    assert!(before.source_paths.is_none());

    // Create: path setting + define + target.
    fs::write(
        &cfg,
        "game_target = \"TURBO\"\ndefines = [\"RELOAD_DEF\"]\nsource_paths = [\"src\"]\n",
    )
    .unwrap();
    watched(backend, &cfg, FileChangeType::CREATED).await;

    let created = config_of(backend).await;
    assert_eq!(created.game_target, "TURBO");
    assert!(
        created.defines.contains("RELOAD_DEF"),
        "create must apply defines; got {:?}",
        created.defines
    );
    assert_eq!(
        created.source_paths,
        Some(vec![PathBuf::from("src")]),
        "create must apply source_paths"
    );
    assert!(resolves(backend, "MainFunc").await);
    assert!(
        !resolves(backend, "ExtraFunc").await,
        "source_paths = [src] must drop extra/ from the snapshot"
    );

    // Modify: bool setting that changes discovery.
    fs::write(&cfg, "honor_gitignore = false\n").unwrap();
    watched(backend, &cfg, FileChangeType::CHANGED).await;

    let modified = config_of(backend).await;
    assert_eq!(modified.honor_gitignore, Some(false));
    assert_eq!(modified.game_target, "TMNEXT", "modify replaces the file");
    assert!(
        resolves(backend, "GeneratedFunc").await,
        "honor_gitignore = false must restore the gitignored file"
    );

    // Delete: fall back to defaults.
    fs::remove_file(&cfg).unwrap();
    watched(backend, &cfg, FileChangeType::DELETED).await;

    let deleted = config_of(backend).await;
    assert_eq!(deleted.honor_gitignore, None);
    assert_eq!(deleted.game_target, "TMNEXT");
    assert!(!deleted.defines.contains("RELOAD_DEF"));
}

/// Invalid TOML must not crash and must keep the last-good config.
#[tokio::test(flavor = "current_thread")]
async fn workspace_config_invalid_toml_keeps_last_good() {
    let root = temp_plugin("ws-invalid");
    let cfg = root.join(".openplanet-lsp.toml");
    fs::write(&cfg, "defines = [\"KEEP_ME\"]\nhonor_gitignore = false\n").unwrap();

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();
    assert!(config_of(backend).await.defines.contains("KEEP_ME"));
    assert!(resolves(backend, "GeneratedFunc").await);

    fs::write(&cfg, "defines = [\n").unwrap();
    watched(backend, &cfg, FileChangeType::CHANGED).await;

    let after = config_of(backend).await;
    assert!(
        after.defines.contains("KEEP_ME"),
        "invalid TOML must keep last-good defines; got {:?}",
        after.defines
    );
    assert_eq!(after.honor_gitignore, Some(false));
    assert!(
        resolves(backend, "GeneratedFunc").await,
        "invalid TOML must keep last-good discovery"
    );
}

/// Client-driven watched-file notifications must surface malformed TOML in
/// the editor, preserve the cached config, and deduplicate repeated events.
#[tokio::test(flavor = "current_thread")]
async fn watched_file_invalid_toml_warns_client_once_and_keeps_last_good() {
    let root = temp_plugin("ws-invalid-warning");
    let cfg = root.join(".openplanet-lsp.toml");
    fs::write(&cfg, "game_target = \"TURBO\"\nhonor_gitignore = false\n").unwrap();

    let (service, mut socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();

    fs::write(&cfg, "game_target = [\n").unwrap();
    watched(backend, &cfg, FileChangeType::CHANGED).await;
    let warning = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        next_show_message(&mut socket),
    )
    .await
    .expect("invalid watched file must warn the LSP client");
    assert_whole_file_toml_warning(&warning, &cfg);
    let after = config_of(backend).await;
    assert_eq!(after.game_target, "TURBO");
    assert_eq!(after.honor_gitignore, Some(false));

    watched(backend, &cfg, FileChangeType::CHANGED).await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(300),
            next_show_message(&mut socket),
        )
        .await
        .is_err(),
        "the same invalid TOML warning must be deduplicated"
    );
}

/// B017 positive guard for a REJECTED review finding.
///
/// A Codex review pass proposed suppressing the reload whenever ANY config in
/// a watched-file batch was invalid. That moves "one bad file blocks
/// everything" from the parser up into the watcher: a half-saved user-global
/// config would silently discard an unrelated, valid workspace edit, while
/// every B017 unit test stayed green. The batch must do BOTH — warn about the
/// broken file AND apply the valid one.
///
/// Without this test the rejection lives only in review history, and the next
/// reviewer proposes the same change unopposed.
#[tokio::test(flavor = "current_thread")]
async fn invalid_config_in_a_batch_does_not_suppress_a_valid_sibling_reload() {
    let root = temp_plugin("batch-mixed-validity");
    let workspace_cfg = root.join(".openplanet-lsp.toml");
    let broken_cfg = root.join("sub/.openplanet-lsp.toml");
    fs::create_dir_all(broken_cfg.parent().unwrap()).unwrap();
    fs::write(&workspace_cfg, "game_target = \"TURBO\"\n").unwrap();

    let (service, mut socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();
    assert_eq!(config_of(backend).await.game_target, "TURBO");

    // One VALID edit and one BROKEN file arrive in the same batch.
    fs::write(&workspace_cfg, "game_target = \"MP4\"\n").unwrap();
    fs::write(&broken_cfg, "game_target = [\n").unwrap();
    backend
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![
                FileEvent::new(
                    Url::from_file_path(&broken_cfg).unwrap(),
                    FileChangeType::CHANGED,
                ),
                FileEvent::new(
                    Url::from_file_path(&workspace_cfg).unwrap(),
                    FileChangeType::CHANGED,
                ),
            ],
        })
        .await;

    let warning = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        next_show_message(&mut socket),
    )
    .await
    .expect("the broken file in the batch must still warn");
    assert_whole_file_toml_warning(&warning, &broken_cfg);

    assert_eq!(
        config_of(backend).await.game_target,
        "MP4",
        "a broken sibling must NOT swallow the valid edit in the same batch"
    );
}

/// The server-owned native notify watcher has the same visible warning and
/// last-good contract as `workspace/didChangeWatchedFiles`.
#[tokio::test(flavor = "current_thread")]
async fn native_config_watcher_invalid_toml_warns_client_and_keeps_last_good() {
    let root = temp_plugin("native-invalid-warning");
    let cfg = root.join(".openplanet-lsp.toml");
    fs::write(&cfg, "game_target = \"MP4\"\nsource_paths = [\"src\"]\n").unwrap();

    let (service, mut socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();
    backend.initialized(InitializedParams {}).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    fs::write(&cfg, "source_paths = [\n").unwrap();
    let warning = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        next_show_message(&mut socket),
    )
    .await
    .expect("native config watcher must warn the LSP client");
    assert_whole_file_toml_warning(&warning, &cfg);
    let after = config_of(backend).await;
    assert_eq!(after.game_target, "MP4");
    assert_eq!(after.source_paths, Some(vec![PathBuf::from("src")]));
}

/// Completion shortcuts live on the cached shard config; a reload must
/// replace that object, not just rebuild the snapshot with stale config.
#[tokio::test(flavor = "current_thread")]
async fn workspace_config_reload_updates_shortcuts() {
    let root = temp_plugin("ws-shortcuts");
    let cfg = root.join(".openplanet-lsp.toml");
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();

    assert_eq!(config_of(backend).await.shortcut_namespaces, None);

    fs::write(&cfg, "shortcut_namespaces = [\"NetX\"]\n").unwrap();
    watched(backend, &cfg, FileChangeType::CHANGED).await;

    assert_eq!(
        config_of(backend).await.shortcut_namespaces,
        Some(vec!["NetX".into()])
    );
}

/// A user-global config URI (outside the workspace) must also reload.
/// The file is not `$HOME` — this only pins event routing; apply is
/// covered by workspace cases + `LspConfig::load`.
#[tokio::test(flavor = "current_thread")]
async fn user_config_watched_uri_triggers_reload() {
    let root = temp_plugin("user-uri");
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();

    // Workspace file written first so a reload has something to apply.
    let cfg = root.join(".openplanet-lsp.toml");
    fs::write(&cfg, "game_target = \"MP4\"\n").unwrap();

    // Path *outside* the plugin, but with the user-global suffix so routing
    // treats it as a config event (contents are irrelevant — load() reads HOME).
    let user = root.parent().unwrap().join(format!(
        "cfgreload-user-{}-{}/.config/openplanet-lsp/config.toml",
        "uri",
        std::process::id()
    ));
    fs::create_dir_all(user.parent().unwrap()).unwrap();
    fs::write(&user, "game_target = \"TURBO\"\n").unwrap();
    watched(backend, &user, FileChangeType::CHANGED).await;

    assert_eq!(
        config_of(backend).await.game_target,
        "MP4",
        "user-config URI must force a ladder reload (workspace file already on disk)"
    );
}

/// Server-side notify (not a client watched-file event) on the user-global
/// path must reload. The apply itself is the workspace file already on disk;
/// writing the injected-HOME user file is only the trigger.
#[tokio::test(flavor = "current_thread")]
async fn user_config_notify_watch_reloads() {
    let root = temp_plugin("user-notify");
    let home = root
        .parent()
        .unwrap()
        .join(format!("cfgreload-notify-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".config/openplanet-lsp")).unwrap();

    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    backend.initialize(init_params(&root)).await.unwrap();
    backend.start_user_config_file_watch(Some(home.clone()));

    fs::write(
        root.join(".openplanet-lsp.toml"),
        "game_target = \"TURBO\"\n",
    )
    .unwrap();
    // Let inotify subscribe before the create.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    fs::write(
        home.join(".config/openplanet-lsp/config.toml"),
        "warnings_as_errors = true\n",
    )
    .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if config_of(backend).await.game_target == "TURBO" {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "notify watch did not reload; target={}",
                config_of(backend).await.game_target
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let _ = fs::remove_dir_all(&home);
}
