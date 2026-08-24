//! `openplanet-lsp config` — TUI editor for workspace or user-global config.

use std::path::{Path, PathBuf};

use crate::user_config::{user_config_path, workspace_config_path, SETTINGS};
use crate::workspace::project;

pub const CONFIG_HELP: &str = "\
openplanet-lsp config - Edit a config file in a TUI

USAGE:
    openplanet-lsp config [OPTIONS]

OPTIONS:
    -h, --help      Show this help message
    --global        Edit ~/.config/openplanet-lsp/config.toml
    --path FILE     Config file to edit (overrides workspace / --global)

BEHAVIOR:
    With no flags, edits .openplanet-lsp.toml at the plugin root
    (walks up from the current directory for info.toml). Creates the
    file on first save if it is missing. Pass --global to edit the
    user-wide file instead. --path FILE wins over both.

    Not inside a plugin and no --global/--path: error (exit 2).
    cd into a plugin directory, or pass --global.

    Changing a value writes the file immediately (comments and
    unknown keys are kept). Unset a key with `d` to fall back to
    the built-in default.

    Keys:
";

pub fn help_text() -> String {
    let mut out = CONFIG_HELP.to_string();
    for spec in SETTINGS {
        out.push_str(&format!("    {:<38} {}\n", spec.key, spec.help));
    }
    out.push('\n');
    out.push_str(
        "KEYS:\n    ↑/k ↓/j move   enter/space edit   ←/→ cycle enums\n    d unset   q quit\n",
    );
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOptions {
    pub path: PathBuf,
}

pub fn parse_config_args(args: &[String]) -> Result<ConfigOptions, String> {
    let cwd = std::env::current_dir().map_err(|err| {
        format!(
            "cannot resolve current directory: {err}\n\
             cd into a plugin directory, or pass --global / --path FILE."
        )
    })?;
    parse_config_args_from(args, &cwd)
}

pub fn parse_config_args_from(args: &[String], cwd: &Path) -> Result<ConfigOptions, String> {
    let mut path = None;
    let mut global = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print!("{}", help_text());
                std::process::exit(0);
            }
            "--global" => {
                global = true;
                i += 1;
            }
            "--path" => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "--path requires a file argument".to_string())?;
                path = Some(PathBuf::from(value));
                i += 2;
            }
            other if let Some(rest) = other.strip_prefix("--path=") => {
                if rest.is_empty() {
                    return Err("--path requires a file argument".into());
                }
                path = Some(PathBuf::from(rest));
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown config option: {other}\nRun `openplanet-lsp config --help` for usage."
                ));
            }
            other => {
                return Err(format!(
                    "unexpected argument: {other}\nRun `openplanet-lsp config --help` for usage."
                ));
            }
        }
    }
    let path = if let Some(p) = path {
        p
    } else if global {
        user_config_path()
            .ok_or_else(|| "cannot resolve config path: set HOME or pass --path FILE".to_string())?
    } else {
        match project::find_workspace_root(cwd) {
            Some(root) => workspace_config_path(&root),
            None => {
                return Err(format!(
                    "no Openplanet plugin found (no info.toml above {})\n\
                     cd into a plugin directory, or pass --global to edit \
                     the user config (or --path FILE).\n\
                     Run `openplanet-lsp config --help` for usage.",
                    cwd.display()
                ));
            }
        }
    };
    Ok(ConfigOptions { path })
}

pub fn run_config_command(args: &[String]) -> i32 {
    let options = match parse_config_args(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("{err}");
            return 2;
        }
    };
    match crate::tui::config::run(options.path) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("config TUI failed: {err}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opl-config-cmd-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plugin_dir(name: &str) -> PathBuf {
        let dir = temp_dir(name);
        std::fs::write(dir.join("info.toml"), "name = \"CfgTest\"\n").unwrap();
        dir
    }

    fn parse_in(args: &[&str], cwd: &Path) -> Result<ConfigOptions, String> {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        parse_config_args_from(&args, cwd)
    }

    #[test]
    fn parse_path_flag_and_equals() {
        let cwd = temp_dir("path-cwd");
        let opts = parse_in(&["--path", "/tmp/c.toml"], &cwd).unwrap();
        assert_eq!(opts.path, PathBuf::from("/tmp/c.toml"));
        let opts = parse_in(&["--path=/tmp/d.toml"], &cwd).unwrap();
        assert_eq!(opts.path, PathBuf::from("/tmp/d.toml"));
    }

    #[test]
    fn parse_rejects_unknown_and_missing_path_value() {
        let cwd = temp_dir("reject-cwd");
        assert!(parse_in(&["--wat"], &cwd).is_err());
        assert!(parse_in(&["--path"], &cwd).is_err());
        assert!(parse_in(&["--path="], &cwd).is_err());
        assert!(parse_in(&["extra"], &cwd).is_err());
        assert!(parse_in(&["--global=nope"], &cwd).is_err());
    }

    #[test]
    fn parse_default_in_plugin_uses_workspace_dotfile() {
        let root = plugin_dir("ws-root");
        let nested = root.join("src/nested");
        std::fs::create_dir_all(&nested).unwrap();

        let expected = crate::user_config::workspace_config_path(&root);
        assert_eq!(parse_in(&[], &root).unwrap().path, expected);
        assert_eq!(parse_in(&[], &root.join("src")).unwrap().path, expected);
        assert_eq!(parse_in(&[], &nested).unwrap().path, expected);

        let as_file = nested.join("Main.as");
        std::fs::write(&as_file, "").unwrap();
        assert_eq!(parse_in(&[], &as_file).unwrap().path, expected);
    }

    #[test]
    fn parse_global_uses_user_config_even_inside_plugin() {
        let expected = user_config_path().expect("HOME must be set for --global tests");
        let empty = temp_dir("global-empty");
        let plugin = plugin_dir("global-plugin");

        assert_eq!(parse_in(&["--global"], &empty).unwrap().path, expected);
        assert_eq!(parse_in(&["--global"], &plugin).unwrap().path, expected);
    }

    #[test]
    fn parse_path_wins_over_workspace_and_global() {
        let plugin = plugin_dir("path-wins");
        let empty = temp_dir("path-wins-empty");
        let file = "/tmp/explicit-config.toml";

        assert_eq!(
            parse_in(&["--path", file], &plugin).unwrap().path,
            PathBuf::from(file)
        );
        assert_eq!(
            parse_in(&["--path", file, "--global"], &plugin)
                .unwrap()
                .path,
            PathBuf::from(file)
        );
        assert_eq!(
            parse_in(&["--global", "--path", file], &empty)
                .unwrap()
                .path,
            PathBuf::from(file)
        );
        assert_eq!(
            parse_in(&["--global", "--path", file], &plugin)
                .unwrap()
                .path,
            PathBuf::from(file)
        );
        assert_eq!(
            parse_in(&["--path", file, "--global"], &empty)
                .unwrap()
                .path,
            PathBuf::from(file)
        );
        assert_eq!(
            parse_in(&["--path=/tmp/via-eq.toml"], &plugin)
                .unwrap()
                .path,
            PathBuf::from("/tmp/via-eq.toml")
        );
    }

    #[test]
    fn parse_missing_plugin_errors_without_global_or_path() {
        let empty = temp_dir("no-plugin");
        let nested = empty.join("deeper/still");
        std::fs::create_dir_all(&nested).unwrap();

        let stray = empty.join("notes.txt");
        std::fs::write(&stray, "").unwrap();

        for cwd in [empty.as_path(), nested.as_path(), stray.as_path()] {
            let err = parse_in(&[], cwd).unwrap_err();
            assert!(
                err.contains("cd") && err.contains("--global"),
                "missing-plugin error must tell the user to cd or pass --global: {err}"
            );
        }
    }

    #[test]
    fn help_lists_every_setting() {
        let help = help_text();
        for spec in SETTINGS {
            assert!(help.contains(spec.key), "help missing {}: {help}", spec.key);
        }
    }

    #[test]
    fn help_describes_workspace_default_and_global() {
        let help = help_text();
        assert!(
            help.contains("--global"),
            "help must advertise --global: {help}"
        );
        assert!(
            help.contains(".openplanet-lsp.toml"),
            "help must name the workspace file: {help}"
        );
        assert!(help.contains("--path"), "help must keep --path: {help}");
    }
}
