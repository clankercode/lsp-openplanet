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
    KeepLastGood { reason: String },
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
        ConfigReloadAction::KeepLastGood { reason } => {
            tracing::warn!("keeping last-good config ({reason})");
        }
    })
}

/// Watch the user-global config parent (create/replace visible) plus any
/// extra directories (plugin roots / opened folders, non-recursive) and
/// invoke `on_action` with the batch result. If any changed config is invalid,
/// the whole batch keeps last-good and reports each invalid file; otherwise a
/// valid create/modify/delete produces one reload action.
pub fn watch_config_files_with_action(
    home: Option<&Path>,
    extra_dirs: &[PathBuf],
    on_action: impl Fn(ConfigReloadAction) + Send + Sync + 'static,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>, String> {
    let mut debouncer = new_debouncer(
        DEBOUNCE,
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| match res {
            Ok(events) => {
                let mut reload = false;
                let mut invalid = Vec::new();
                for ev in events {
                    if !is_config_path(&ev.path) {
                        continue;
                    }
                    match config_reload_action(&ev.path) {
                        ConfigReloadAction::Reload => reload = true,
                        action @ ConfigReloadAction::KeepLastGood { .. } => invalid.push(action),
                    }
                }
                if invalid.is_empty() {
                    if reload {
                        on_action(ConfigReloadAction::Reload);
                    }
                } else {
                    for action in invalid {
                        on_action(action);
                    }
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
            reason: format!("unreadable {}: {err}", path.display()),
        },
        Ok(contents) => match toml::from_str::<toml::Table>(&contents) {
            Ok(_) => ConfigReloadAction::Reload,
            Err(err) => ConfigReloadAction::KeepLastGood {
                reason: format!("invalid TOML in {}: {err}", path.display()),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_home(name: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-cfgwatch-{}-{}-{}",
            name,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
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
        let _ = fs::remove_dir_all(&home);
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
        assert!(
            matches!(
                config_reload_action(&workspace),
                ConfigReloadAction::KeepLastGood { .. }
            ),
            "invalid workspace TOML must keep last-good"
        );
        assert!(
            matches!(
                config_reload_action(&user),
                ConfigReloadAction::KeepLastGood { .. }
            ),
            "invalid user TOML must keep last-good"
        );

        // Delete after a good write → reload.
        fs::remove_file(&workspace).unwrap();
        fs::remove_file(&user).unwrap();
        assert_eq!(config_reload_action(&workspace), ConfigReloadAction::Reload);
        assert_eq!(config_reload_action(&user), ConfigReloadAction::Reload);

        let _ = fs::remove_dir_all(&home);
    }
}
