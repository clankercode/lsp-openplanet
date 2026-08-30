//! Shared detection of on-disk config files that should retrigger analysis.
//!
//! Two files matter:
//! - workspace `.openplanet-lsp.toml` (plugin root or opened-folder parent)
//! - user-global `$HOME/.config/openplanet-lsp/config.toml`
//!
//! The user file lives *outside* the plugin root, so a recursive plugin-root
//! watcher never sees it. Callers watch [`user_config_watch_root`] and filter
//! events with [`is_config_path`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};

use crate::config::ConfigIssue;

const DEBOUNCE: Duration = Duration::from_millis(250);

/// Filename of the per-workspace / per-plugin config file.
pub const WORKSPACE_CONFIG_FILE: &str = ".openplanet-lsp.toml";

/// `$HOME`-relative path of the user-global config file. Must stay in lockstep
/// with `LspConfig::user_config_path` / `UserPrefs::apply_user_config`.
pub const USER_CONFIG_REL: &str = ".config/openplanet-lsp/config.toml";

/// What to do when a config path produced a filesystem event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigReloadAction {
    /// File is missing or parses; reload the config ladder.
    Reload,
    /// File exists but TOML is invalid; keep last-good config.
    KeepLastGood { issue: ConfigIssue },
}

/// `$HOME/.config/openplanet-lsp/config.toml` when `$HOME` is set.
pub fn user_config_path() -> Option<PathBuf> {
    user_config_path_from_home(std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

/// Same path given an explicit home (tests; avoids mutating process `$HOME`).
pub fn user_config_path_from_home(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|home| home.join(USER_CONFIG_REL))
}

/// Directory to pass to notify so create/replace/atomic-rename of the user
/// file is visible. Prefers the file's parent when it exists; otherwise the
/// nearest existing ancestor under `$HOME/.config`.
pub fn user_config_watch_root(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    let parent = home.join(".config/openplanet-lsp");
    if parent.exists() {
        return Some(parent);
    }
    let config_dir = home.join(".config");
    if config_dir.exists() {
        return Some(config_dir);
    }
    home.exists().then(|| home.to_path_buf())
}

/// True for workspace `.openplanet-lsp.toml` or the user-global config file.
pub fn is_config_path(path: &Path) -> bool {
    is_workspace_config_path(path) || is_user_config_path(path)
}

pub fn is_workspace_config_path(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some(WORKSPACE_CONFIG_FILE)
}

pub fn is_user_config_path(path: &Path) -> bool {
    let mut comps = path.components().rev();
    let file = comps.next().and_then(|c| c.as_os_str().to_str());
    let dir = comps.next().and_then(|c| c.as_os_str().to_str());
    let config = comps.next().and_then(|c| c.as_os_str().to_str());
    file == Some("config.toml") && dir == Some("openplanet-lsp") && config == Some(".config")
}

/// Back-compatible reload-only watcher. New LSP callers should use
/// [`watch_config_files_with_action`] so invalid TOML can be surfaced to the
/// client rather than only logged.
pub fn watch_config_files(
    home: Option<&Path>,
    extra_dirs: &[PathBuf],
    on_reload: impl Fn() + Send + Sync + 'static,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>, String> {
    watch_config_files_with_action(home, extra_dirs, move |action| match action {
        ConfigReloadAction::Reload => on_reload(),
        ConfigReloadAction::KeepLastGood { issue } => {
            tracing::warn!("keeping last-good config ({issue})");
        }
    })
}

/// Actions to emit for one debounced batch of filesystem events.
///
/// Every invalid config in the batch is reported, AND a valid change still
/// reloads. An earlier revision suppressed the reload whenever any file in
/// the batch was invalid; that is rejected deliberately — see
/// [`batch_actions`]'s tests. One broken file must not swallow an unrelated
/// valid edit, which is the whole point of B017's per-key tolerance.
pub(crate) fn batch_actions<'a>(paths: impl Iterator<Item = &'a Path>) -> Vec<ConfigReloadAction> {
    let mut reload = false;
    let mut actions = Vec::new();
    for path in paths {
        if !is_config_path(path) {
            continue;
        }
        match config_reload_action(path) {
            ConfigReloadAction::Reload => reload = true,
            action @ ConfigReloadAction::KeepLastGood { .. } => actions.push(action),
        }
    }
    if reload {
        actions.push(ConfigReloadAction::Reload);
    }
    actions
}

/// Watch the user-global config parent (create/replace visible) plus any
/// extra directories (plugin roots / opened folders, non-recursive) and
/// invoke `on_action` for each outcome in the batch: one action per invalid
/// file, plus one `Reload` when any changed config was valid.
pub fn watch_config_files_with_action(
    home: Option<&Path>,
    extra_dirs: &[PathBuf],
    on_action: impl Fn(ConfigReloadAction) + Send + Sync + 'static,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>, String> {
    let mut debouncer = new_debouncer(
        DEBOUNCE,
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| match res {
            Ok(events) => {
                for action in batch_actions(events.iter().map(|ev| ev.path.as_path())) {
                    on_action(action);
                }
            }
            Err(err) => tracing::warn!("config watch error: {err:?}"),
        },
    )
    .map_err(|e| e.to_string())?;

    if let Some(watch_root) = user_config_watch_root(home) {
        let mode = if watch_root.ends_with("openplanet-lsp") {
            RecursiveMode::NonRecursive
        } else {
            RecursiveMode::Recursive
        };
        if let Err(err) = debouncer.watcher().watch(&watch_root, mode) {
            tracing::warn!(
                "user config watch unavailable at {}: {err}",
                watch_root.display()
            );
        }
    }
    for dir in extra_dirs {
        if dir.exists() {
            if let Err(err) = debouncer.watcher().watch(dir, RecursiveMode::NonRecursive) {
                tracing::warn!("config watch unavailable at {}: {err}", dir.display());
            }
        }
    }
    Ok(debouncer)
}

/// Inspect a config path after a create/modify/delete event.
///
/// Missing (delete) → [`ConfigReloadAction::Reload`]. Valid TOML (including
/// empty/unknown keys) → Reload. Unparseable TOML → KeepLastGood.
pub fn config_reload_action(path: &Path) -> ConfigReloadAction {
    match std::fs::read_to_string(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ConfigReloadAction::Reload,
        Err(err) => ConfigReloadAction::KeepLastGood {
            issue: ConfigIssue::unreadable(path, &err),
        },
        Ok(contents) => match toml::from_str::<toml::Table>(&contents) {
            Ok(_) => ConfigReloadAction::Reload,
            Err(err) => ConfigReloadAction::KeepLastGood {
                issue: ConfigIssue::unparseable(path, &err),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_home(name: &str) -> crate::testing::TempDir {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = crate::testing::TempDir::new(&format!(
            "openplanet-lsp-cfgwatch-{}-{}",
            name,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        dir
    }

    #[test]
    fn classifies_workspace_and_user_paths() {
        assert!(is_workspace_config_path(Path::new(".openplanet-lsp.toml")));
        assert!(is_workspace_config_path(Path::new(
            "/plugin/.openplanet-lsp.toml"
        )));
        assert!(!is_workspace_config_path(Path::new("info.toml")));
        assert!(!is_workspace_config_path(Path::new("config.toml")));

        assert!(is_user_config_path(Path::new(
            "/home/dev/.config/openplanet-lsp/config.toml"
        )));
        assert!(is_user_config_path(Path::new(
            "/tmp/fake-home/.config/openplanet-lsp/config.toml"
        )));
        assert!(!is_user_config_path(Path::new(
            "/home/dev/.config/other/config.toml"
        )));
        assert!(!is_user_config_path(Path::new(
            "/plugin/.openplanet-lsp.toml"
        )));
        assert!(is_config_path(Path::new("/p/.openplanet-lsp.toml")));
        assert!(is_config_path(Path::new(
            "/h/.config/openplanet-lsp/config.toml"
        )));
    }

    #[test]
    fn user_path_matches_loader_home_layout() {
        let home = Path::new("/tmp/op-home");
        assert_eq!(
            user_config_path_from_home(Some(home)).unwrap(),
            home.join(".config/openplanet-lsp/config.toml")
        );
        assert!(user_config_path_from_home(None).is_none());
    }

    #[test]
    fn watch_root_prefers_existing_parent_then_config_then_home() {
        let home = temp_home("watch-root");
        assert_eq!(
            user_config_watch_root(Some(&home)).as_deref(),
            Some(home.as_path()),
            "bare home: watch home so .config create is visible"
        );

        fs::create_dir_all(home.join(".config")).unwrap();
        assert_eq!(
            user_config_watch_root(Some(&home)).unwrap(),
            home.join(".config")
        );

        fs::create_dir_all(home.join(".config/openplanet-lsp")).unwrap();
        assert_eq!(
            user_config_watch_root(Some(&home)).unwrap(),
            home.join(".config/openplanet-lsp")
        );
    }

    #[test]
    fn reload_action_create_modify_delete_and_invalid() {
        let home = temp_home("status");
        let workspace = home.join(".openplanet-lsp.toml");
        let user_dir = home.join(".config/openplanet-lsp");
        fs::create_dir_all(&user_dir).unwrap();
        let user = user_dir.join("config.toml");

        // Delete / missing → reload (settings gone, fall back to other layers).
        for path in [&workspace, &user] {
            assert_eq!(
                config_reload_action(path),
                ConfigReloadAction::Reload,
                "missing {} must reload",
                path.display()
            );
        }

        // Create / modify with valid TOML (bool + path settings).
        fs::write(&workspace, "honor_gitignore = false\n").unwrap();
        fs::write(&user, "source_paths = [\"src\"]\n").unwrap();
        assert_eq!(config_reload_action(&workspace), ConfigReloadAction::Reload);
        assert_eq!(config_reload_action(&user), ConfigReloadAction::Reload);

        fs::write(&workspace, "warnings_as_errors = true\n").unwrap();
        fs::write(&user, "game_target = \"TURBO\"\n").unwrap();
        assert_eq!(config_reload_action(&workspace), ConfigReloadAction::Reload);
        assert_eq!(config_reload_action(&user), ConfigReloadAction::Reload);

        // Invalid TOML on either file → keep last-good.
        fs::write(&workspace, "honor_gitignore = {\n").unwrap();
        fs::write(&user, "defines = [\n").unwrap();
        for (path, found) in [
            (&workspace, "invalid inline table"),
            (&user, "invalid array"),
        ] {
            let ConfigReloadAction::KeepLastGood { issue } = config_reload_action(path) else {
                panic!("invalid TOML in {} must keep last-good", path.display());
            };
            assert_eq!(issue.file, *path);
            assert_eq!(issue.key, "", "whole-file parse failures have no key");
            assert_eq!(issue.expected, "valid TOML (the whole file was skipped)");
            assert!(
                issue.found.contains(found),
                "{}: expected parse error containing {found:?}, got {issue:?}",
                path.display()
            );
        }

        // Other unusable files are structured whole-file issues too.
        let unreadable = home.join("unreadable/.openplanet-lsp.toml");
        fs::create_dir_all(&unreadable).unwrap();
        let ConfigReloadAction::KeepLastGood { issue } = config_reload_action(&unreadable) else {
            panic!("an unreadable config must keep last-good");
        };
        assert_eq!(issue.file, unreadable);
        assert_eq!(issue.key, "");
        assert_eq!(issue.expected, "a readable config file");
        assert!(!issue.found.is_empty());

        // --- B017: a broken file must not swallow a valid sibling edit ---
        //
        // Positive guard for a REJECTED review finding. A Codex review pass
        // proposed suppressing the reload whenever any file in a batch was
        // invalid. That reintroduces "one bad file blocks everything" one
        // layer up from the parser: a half-saved user-global config would
        // silently discard an unrelated, valid workspace edit while every
        // B017 unit test stayed green. Reload wins; invalid files warn too.
        fs::write(&workspace, "game_target = \"MP4\"\n").unwrap();
        fs::write(&user, "defines = [\n").unwrap();
        let actions = batch_actions([workspace.as_path(), user.as_path()].into_iter());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConfigReloadAction::Reload)),
            "a valid config in the batch must still reload: {actions:?}"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, ConfigReloadAction::KeepLastGood { .. })),
            "the invalid config in the same batch must still be reported: {actions:?}"
        );

        // Order matters for the consumer: warn first, then reload, so the
        // user sees why a layer was skipped by the reload they just got.
        let reload_at = actions
            .iter()
            .position(|a| matches!(a, ConfigReloadAction::Reload))
            .unwrap();
        let warn_at = actions
            .iter()
            .position(|a| matches!(a, ConfigReloadAction::KeepLastGood { .. }))
            .unwrap();
        assert!(
            warn_at < reload_at,
            "warnings precede the reload: {actions:?}"
        );

        // All-invalid batch: warnings only, no reload.
        fs::write(&workspace, "honor_gitignore = {\n").unwrap();
        let actions = batch_actions([workspace.as_path(), user.as_path()].into_iter());
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, ConfigReloadAction::Reload)),
            "no valid config means no reload: {actions:?}"
        );
        assert_eq!(
            actions.len(),
            2,
            "one warning per invalid file: {actions:?}"
        );

        // Non-config paths in the batch are ignored entirely.
        let unrelated = home.join("src/Main.as");
        assert!(
            batch_actions([unrelated.as_path()].into_iter()).is_empty(),
            "a non-config path must produce no action"
        );

        // Delete after a good write → reload.
        fs::remove_file(&workspace).unwrap();
        fs::remove_file(&user).unwrap();
        assert_eq!(config_reload_action(&workspace), ConfigReloadAction::Reload);
        assert_eq!(config_reload_action(&user), ConfigReloadAction::Reload);
    }
}
