//! CLI entrypoint routing: bare TTY → TUI, non-TTY → LSP, flags, config.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::cli::{self, CheckOptions};
use crate::config::{DefaultMode, UserPrefs};
use crate::workspace::project;

/// Result of deciding what bare / default invocation should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchAction {
    /// Start the language server on stdio.
    Lsp,
    /// Start watch TUI on this plugin root (path may be `.` or absolute).
    WatchTui { path: PathBuf },
    /// Print short help and exit 2.
    HelpNoPlugin,
}

/// Whether stdin looks like an interactive terminal (editors pipe stdio).
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Resolve plugin root walking up from `start` looking for `info.toml`.
pub fn resolve_plugin_root(start: &Path) -> Option<PathBuf> {
    project::find_workspace_root(start)
}

/// Decide bare-invocation behavior (no subcommand).
pub fn decide_bare_launch(is_tty: bool, cwd: &Path, prefs: &UserPrefs) -> LaunchAction {
    if !is_tty {
        return LaunchAction::Lsp;
    }
    match prefs.default_mode {
        DefaultMode::Lsp => LaunchAction::Lsp,
        DefaultMode::Tui => match resolve_plugin_root(cwd) {
            Some(path) => LaunchAction::WatchTui { path },
            None => LaunchAction::HelpNoPlugin,
        },
    }
}

/// Shared check options for bare-TTY / default watch launch.
pub fn watch_options_for_path(path: PathBuf) -> CheckOptions {
    let mut opts = CheckOptions::default();
    opts.path = path;
    opts.watch = true;
    opts.plugin_files_search_paths = vec![PathBuf::from("src")];
    opts
}

/// Message when TTY bare launch finds no plugin.
pub fn no_plugin_help_message() -> &'static str {
    "\
No OpenPlanet plugin found (no info.toml at or above the current directory).

  • cd into a plugin and run `openplanet-lsp` again (starts the watch TUI)
  • or: openplanet-lsp check --watch /path/to/plugin
  • or: openplanet-lsp check /path/to/plugin
  • force language server: openplanet-lsp --lsp   (or: openplanet-lsp lsp)

Config: ~/.config/openplanet-lsp/config.toml
  default_mode = \"tui\"   # or \"lsp\"

Repo:  https://github.com/clankercode/lsp-openplanet
Found a mismatch with how Openplanet behaves in-game? Please open an issue:
       https://github.com/clankercode/lsp-openplanet/issues
"
}

/// Run watch TUI for a path; returns process exit code.
pub fn run_watch_path(path: PathBuf) -> i32 {
    let opts = watch_options_for_path(path);
    match cli::watch::run_watch(opts) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("op-lsp-entry-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bare_non_tty_is_lsp() {
        let cwd = Path::new("/tmp");
        let prefs = UserPrefs::default();
        assert_eq!(decide_bare_launch(false, cwd, &prefs), LaunchAction::Lsp);
    }

    #[test]
    fn bare_tty_no_plugin_help() {
        let dir = temp_dir("empty");
        let prefs = UserPrefs::default();
        assert_eq!(
            decide_bare_launch(true, &dir, &prefs),
            LaunchAction::HelpNoPlugin
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bare_tty_plugin_is_watch() {
        let dir = temp_dir("plug");
        fs::write(dir.join("info.toml"), "[meta]\nname=\"x\"\n").unwrap();
        let prefs = UserPrefs::default();
        match decide_bare_launch(true, &dir, &prefs) {
            LaunchAction::WatchTui { path } => {
                assert_eq!(path.canonicalize().unwrap(), dir.canonicalize().unwrap());
            }
            other => panic!("expected WatchTui, got {other:?}"),
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bare_tty_config_lsp_overrides() {
        let dir = temp_dir("plug2");
        fs::write(dir.join("info.toml"), "[meta]\nname=\"x\"\n").unwrap();
        let prefs = UserPrefs {
            default_mode: DefaultMode::Lsp,
        };
        assert_eq!(decide_bare_launch(true, &dir, &prefs), LaunchAction::Lsp);
        let _ = fs::remove_dir_all(dir);
    }
}
