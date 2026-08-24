//! `openplanet-lsp config` — TUI editor for the user-global config file.

use std::path::PathBuf;

use crate::user_config::{user_config_path, SETTINGS};

pub const CONFIG_HELP: &str = "\
openplanet-lsp config - Edit the user-global config file in a TUI

USAGE:
    openplanet-lsp config [OPTIONS]

OPTIONS:
    -h, --help      Show this help message
    --path FILE     Config file to edit (default:
                    ~/.config/openplanet-lsp/config.toml)

BEHAVIOR:
    Lists every setting persisted in the user config file. Changing a
    value writes the file immediately (comments and unknown keys are
    kept). Unset a key with `d` to fall back to the built-in default.

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
    let mut path = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print!("{}", help_text());
                std::process::exit(0);
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
    let path = match path {
        Some(p) => p,
        None => user_config_path().ok_or_else(|| {
            "cannot resolve config path: set HOME or pass --path FILE".to_string()
        })?,
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

    #[test]
    fn parse_path_flag_and_equals() {
        let opts = parse_config_args(&["--path".into(), "/tmp/c.toml".into()]).unwrap();
        assert_eq!(opts.path, PathBuf::from("/tmp/c.toml"));
        let opts = parse_config_args(&["--path=/tmp/d.toml".into()]).unwrap();
        assert_eq!(opts.path, PathBuf::from("/tmp/d.toml"));
    }

    #[test]
    fn parse_rejects_unknown_and_missing_path_value() {
        assert!(parse_config_args(&["--wat".into()]).is_err());
        assert!(parse_config_args(&["--path".into()]).is_err());
        assert!(parse_config_args(&["extra".into()]).is_err());
    }

    #[test]
    fn help_lists_every_setting() {
        let help = help_text();
        for spec in SETTINGS {
            assert!(help.contains(spec.key), "help missing {}: {help}", spec.key);
        }
    }
}
