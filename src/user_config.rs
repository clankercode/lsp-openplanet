//! User-global and workspace config files (`config.toml` / `.openplanet-lsp.toml`).
//!
//! Load / mutate / save the document the `config` TUI edits. Writes are
//! atomic (temp + rename) and preserve comments / unknown keys.

use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Value};

/// `$HOME/.config/openplanet-lsp/config.toml` — same path [`crate::config`]
/// already reads. `None` when `$HOME` is unset.
pub fn user_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join("openplanet-lsp")
            .join("config.toml")
    })
}

/// `<plugin-root>/.openplanet-lsp.toml` — workspace file next to `info.toml`.
pub fn workspace_config_path(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref()
        .join(crate::config_watch::WORKSPACE_CONFIG_FILE)
}

/// In-memory view of the user config TOML document.
#[derive(Debug, Clone)]
pub struct UserConfigFile {
    path: PathBuf,
    doc: DocumentMut,
}

impl UserConfigFile {
    /// Load `path`, or an empty document if the file does not exist.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, UserConfigError> {
        let path = path.into();
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(UserConfigError::Io(path, err)),
        };
        let doc = raw
            .parse::<DocumentMut>()
            .map_err(|err| UserConfigError::Parse(err.to_string()))?;
        Ok(Self { path, doc })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.doc.get(key).and_then(Item::as_bool)
    }

    pub fn set_bool(&mut self, key: &str, value: bool) {
        assign_value(&mut self.doc, key, Value::from(value));
    }

    pub fn get_string(&self, key: &str) -> Option<String> {
        self.doc.get(key).and_then(Item::as_str).map(str::to_string)
    }

    pub fn set_string(&mut self, key: &str, value: &str) {
        assign_value(&mut self.doc, key, Value::from(value));
    }

    pub fn get_usize(&self, key: &str) -> Option<usize> {
        self.doc
            .get(key)
            .and_then(Item::as_integer)
            .and_then(|n| usize::try_from(n).ok())
    }

    pub fn set_usize(&mut self, key: &str, value: usize) {
        assign_value(
            &mut self.doc,
            key,
            Value::from(i64::try_from(value).unwrap_or(i64::MAX)),
        );
    }

    pub fn get_string_list(&self, key: &str) -> Option<Vec<String>> {
        let arr = self.doc.get(key).and_then(Item::as_array)?;
        Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
    }

    pub fn set_string_list(&mut self, key: &str, values: &[String]) {
        assign_value(&mut self.doc, key, Value::Array(string_array(values)));
    }

    pub fn get_nested_string_list(&self, key: &str) -> Option<Vec<Vec<String>>> {
        let arr = self.doc.get(key).and_then(Item::as_array)?;
        Some(
            arr.iter()
                .filter_map(|v| {
                    v.as_array().map(|inner| {
                        inner
                            .iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                })
                .collect(),
        )
    }

    pub fn set_nested_string_list(&mut self, key: &str, values: &[Vec<String>]) {
        let mut outer = Array::new();
        for group in values {
            outer.push(Value::Array(string_array(group)));
        }
        assign_value(&mut self.doc, key, Value::Array(outer));
    }

    pub fn unset(&mut self, key: &str) {
        self.doc.as_table_mut().remove(key);
    }

    /// Write the document to [`Self::path`], creating parent dirs.
    ///
    /// Same-directory unique temp + rename (Windows: remove dest and retry),
    /// matching [`crate::update::save_status`].
    pub fn save(&self) -> Result<(), UserConfigError> {
        let parent = match self.path.parent() {
            Some(p) => p,
            None => {
                return Err(UserConfigError::Parse(format!(
                    "{} has no parent directory",
                    self.path.display()
                )));
            }
        };
        std::fs::create_dir_all(parent)
            .map_err(|err| UserConfigError::Io(parent.to_path_buf(), err))?;
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".into());
        let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        if let Err(err) = std::fs::write(&tmp, self.doc.to_string()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(UserConfigError::Io(tmp, err));
        }
        if let Err(err) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&self.path);
            if let Err(err2) = std::fs::rename(&tmp, &self.path) {
                let _ = std::fs::remove_file(&tmp);
                return Err(UserConfigError::Io(self.path.clone(), err2));
            }
            let _ = err;
        }
        Ok(())
    }
}

fn assign_value(doc: &mut DocumentMut, key: &str, mut value: Value) {
    if let Some(Item::Value(old)) = doc.get(key) {
        *value.decor_mut() = old.decor().clone();
    }
    doc[key] = Item::Value(value);
}

fn string_array(values: &[String]) -> Array {
    let mut arr = Array::new();
    for value in values {
        arr.push(value.as_str());
    }
    arr
}

#[derive(Debug)]
pub enum UserConfigError {
    Io(PathBuf, std::io::Error),
    Parse(String),
}

impl std::fmt::Display for UserConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, err) => write!(f, "{}: {err}", path.display()),
            Self::Parse(msg) => write!(f, "invalid config toml: {msg}"),
        }
    }
}

impl std::error::Error for UserConfigError {}

/// Kind of value a persisted config key stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool {
        default: bool,
    },
    Enum {
        choices: &'static [&'static str],
        default: &'static str,
    },
    String,
    Path,
    /// A comma-separated list in the TUI editor. `example` is a short,
    /// real-looking value shown as a format hint next to the help text.
    StringList {
        example: &'static str,
    },
    NestedStringList,
    Usize {
        default: usize,
    },
}

/// One key the user-global config file (and this TUI) can persist.
#[derive(Debug, Clone, Copy)]
pub struct SettingSpec {
    pub key: &'static str,
    pub help: &'static str,
    pub kind: SettingKind,
}

/// Every setting [`crate::config`] reads from the user-global TOML.
pub const SETTINGS: &[SettingSpec] = &[
    SettingSpec {
        key: "default_mode",
        help: "Bare TTY launch: tui (watch) or lsp",
        kind: SettingKind::Enum {
            choices: &["tui", "lsp"],
            default: "tui",
        },
    },
    SettingSpec {
        key: "game_target",
        help: "Platform defines / typedb variant (TMNEXT, MP4, TURBO, …)",
        kind: SettingKind::Enum {
            // The server's own list — one source of truth for the CLI's
            // `--game-target` validation, this TUI, and the define tables.
            choices: crate::config::LspConfig::GAME_TARGETS,
            default: "TMNEXT",
        },
    },
    SettingSpec {
        key: "signature_mode",
        help: "Signature level whose SIG_* defines are live (cumulative)",
        kind: SettingKind::Enum {
            choices: crate::config::SignatureMode::CHOICES,
            default: "developer",
        },
    },
    SettingSpec {
        key: "warnings_as_errors",
        help: "Make WARNING diagnostics fail `check` (exit 1)",
        kind: SettingKind::Bool { default: false },
    },
    SettingSpec {
        key: "honor_gitignore",
        help: "Skip .as files excluded by .gitignore (default true)",
        kind: SettingKind::Bool { default: true },
    },
    SettingSpec {
        key: "shortcut_disable_default_namespaces",
        help: "Drop built-in UI/Math completion shortcuts",
        kind: SettingKind::Bool { default: false },
    },
    SettingSpec {
        key: "shortcut_match_enums",
        help: "Also offer enums/enum values in namespace shortcuts",
        kind: SettingKind::Bool { default: false },
    },
    SettingSpec {
        key: "openplanet_dir",
        help: "Openplanet install folder (typedb + Plugins)",
        kind: SettingKind::Path,
    },
    SettingSpec {
        key: "plugins_dir",
        help: "Directory to search for plugin dependencies",
        kind: SettingKind::Path,
    },
    SettingSpec {
        key: "defines",
        help: "Preprocessor defines added to the game-target set",
        kind: SettingKind::StringList {
            example: "DEV, DEBUG_BROWSER, -LOGS",
        },
    },
    SettingSpec {
        key: "source_paths",
        help: "Allowlist of root-relative dirs to check",
        kind: SettingKind::StringList {
            example: "src, shared",
        },
    },
    SettingSpec {
        key: "ignore_paths",
        help: "Blocklist of root-relative dirs to skip",
        kind: SettingKind::StringList {
            example: "vendor, OtherPacks",
        },
    },
    SettingSpec {
        key: "shortcut_namespaces",
        help: "Custom completion-shortcut namespaces (replaces UX/MathX)",
        kind: SettingKind::StringList {
            example: "UX, MathX",
        },
    },
    SettingSpec {
        key: "unused",
        help: "Unused functions/vars/etc: ignore, hint, info, warn (default), or error",
        kind: SettingKind::Enum {
            choices: &["ignore", "hint", "info", "warn", "error"],
            default: "warn",
        },
    },
    SettingSpec {
        key: "inactive_regions",
        help: "Grey out #if branches the defines switch off (LSP only): ignore, hint (default), or info",
        kind: SettingKind::Enum {
            choices: &["ignore", "hint", "info"],
            default: "hint",
        },
    },
    SettingSpec {
        key: "unused_callbacks",
        help: "Extra names treated as runtime-invoked callbacks",
        kind: SettingKind::StringList {
            example: "OnCounterUpdate, OnBrick",
        },
    },
    SettingSpec {
        key: "define_matrix",
        help: "CLI check matrix axes (toggled on/off on top of base defines)",
        kind: SettingKind::StringList {
            example: "DEV, DEPENDENCY_MLHOOK",
        },
    },
    SettingSpec {
        key: "define_groups",
        help: "Mutually-exclusive define groups: [A, B] | [C, D]",
        kind: SettingKind::NestedStringList,
    },
    SettingSpec {
        key: "define_combinations",
        help: "Pinned combos: [] | [DEV] | [DEV, DEPENDENCY_MLHOOK]",
        kind: SettingKind::NestedStringList,
    },
    SettingSpec {
        key: "define_matrix_max_combinations",
        help: "Hard cap on matrix cross-product size (default 16)",
        kind: SettingKind::Usize { default: 16 },
    },
    SettingSpec {
        key: "errors",
        help: "Group check errors: last (default), first, or off",
        kind: SettingKind::Enum {
            choices: &["last", "first", "off"],
            default: "last",
        },
    },
];

pub fn format_string_list(values: &[String]) -> String {
    values.join(", ")
}

pub fn parse_string_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn format_nested_list(values: &[Vec<String>]) -> String {
    values
        .iter()
        .map(|group| format!("[{}]", group.join(", ")))
        .collect::<Vec<_>>()
        .join(" | ")
}

pub fn parse_nested_list(s: &str) -> Result<Vec<Vec<String>>, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split('|')
        .map(|group| {
            let group = group.trim();
            let inner = group
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .ok_or_else(|| format!("expected [a, b] groups, got {group:?}"))?;
            Ok(parse_string_list(inner))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the config path together with its directory's guard, so the
    /// tree outlives the call (see `crate::testing::TempPath`).
    fn temp_path(name: &str) -> crate::testing::TempPath {
        crate::testing::TempDir::new(&format!("opl-user-config-{name}")).into_child("config.toml")
    }

    #[test]
    fn load_missing_file_is_empty() {
        let path = temp_path("missing");
        let file = UserConfigFile::load(&path).unwrap();
        assert_eq!(file.path(), path.as_path());
        assert_eq!(file.get_bool("warnings_as_errors"), None);
        assert_eq!(file.get_string("game_target"), None);
        assert_eq!(file.get_string_list("defines"), None);
    }

    #[test]
    fn save_replaces_existing_file() {
        let path = temp_path("replace-existing");
        std::fs::write(&path, "default_mode = \"tui\"\n").unwrap();
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_bool("warnings_as_errors", true);
        file.save().unwrap();
        file.set_string("default_mode", "lsp");
        file.save().unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("default_mode = \"lsp\""),
            "second save must replace: {written}"
        );
        assert!(
            written.contains("warnings_as_errors = true"),
            "first key kept: {written}"
        );
        let parent = path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files must not remain: {leftovers:?}"
        );
    }

    #[test]
    fn set_bool_then_save_writes_key() {
        let path = temp_path("set-bool");
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_bool("warnings_as_errors", true);
        file.save().unwrap();

        let written = std::fs::read_to_string(&path).expect("config file written on save");
        assert!(
            written.contains("warnings_as_errors") && written.contains("true"),
            "expected warnings_as_errors = true, got:\n{written}"
        );

        let reloaded = UserConfigFile::load(&path).unwrap();
        assert_eq!(reloaded.get_bool("warnings_as_errors"), Some(true));
    }

    #[test]
    fn set_bool_false_is_distinguishable_from_unset() {
        let path = temp_path("bool-false");
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_bool("honor_gitignore", false);
        file.save().unwrap();
        let reloaded = UserConfigFile::load(&path).unwrap();
        assert_eq!(reloaded.get_bool("honor_gitignore"), Some(false));
        assert_eq!(reloaded.get_bool("warnings_as_errors"), None);
    }

    #[test]
    fn set_string_and_list_variants_round_trip() {
        let path = temp_path("string-list");
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_string("game_target", "TURBO");
        file.set_string("openplanet_dir", "/tmp/OpenplanetTurbo");
        file.set_string_list(
            "defines",
            &["DEV".to_string(), "DEPENDENCY_MLHOOK".to_string()],
        );
        file.set_string_list("source_paths", &["src".to_string()]);
        file.set_string_list("ignore_paths", &["vendor".to_string()]);
        file.set_string_list(
            "shortcut_namespaces",
            &["UX".to_string(), "Net".to_string()],
        );
        file.set_string_list("unused_callbacks", &["OnCounterUpdate".to_string()]);
        file.set_string_list("define_matrix", &["DEV".to_string()]);
        file.set_usize("define_matrix_max_combinations", 32);
        file.save().unwrap();

        let reloaded = UserConfigFile::load(&path).unwrap();
        assert_eq!(reloaded.get_string("game_target").as_deref(), Some("TURBO"));
        assert_eq!(
            reloaded.get_string("openplanet_dir").as_deref(),
            Some("/tmp/OpenplanetTurbo")
        );
        assert_eq!(
            reloaded.get_string_list("defines"),
            Some(vec!["DEV".into(), "DEPENDENCY_MLHOOK".into()])
        );
        assert_eq!(
            reloaded.get_string_list("source_paths"),
            Some(vec!["src".into()])
        );
        assert_eq!(
            reloaded.get_string_list("ignore_paths"),
            Some(vec!["vendor".into()])
        );
        assert_eq!(
            reloaded.get_string_list("shortcut_namespaces"),
            Some(vec!["UX".into(), "Net".into()])
        );
        assert_eq!(
            reloaded.get_string_list("unused_callbacks"),
            Some(vec!["OnCounterUpdate".into()])
        );
        assert_eq!(
            reloaded.get_string_list("define_matrix"),
            Some(vec!["DEV".into()])
        );
        assert_eq!(
            reloaded.get_usize("define_matrix_max_combinations"),
            Some(32)
        );
    }

    #[test]
    fn nested_list_variants_round_trip() {
        let path = temp_path("nested");
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_nested_string_list(
            "define_groups",
            &[
                vec!["TMNEXT".into(), "MP4".into(), "TURBO".into()],
                vec!["FEATURE_A".into(), "FEATURE_B".into()],
            ],
        );
        file.set_nested_string_list(
            "define_combinations",
            &[
                Vec::new(),
                vec!["DEV".into()],
                vec!["DEV".into(), "DEPENDENCY_MLHOOK".into()],
            ],
        );
        file.save().unwrap();

        let reloaded = UserConfigFile::load(&path).unwrap();
        assert_eq!(
            reloaded.get_nested_string_list("define_groups"),
            Some(vec![
                vec!["TMNEXT".into(), "MP4".into(), "TURBO".into()],
                vec!["FEATURE_A".into(), "FEATURE_B".into()],
            ])
        );
        assert_eq!(
            reloaded.get_nested_string_list("define_combinations"),
            Some(vec![
                Vec::new(),
                vec!["DEV".into()],
                vec!["DEV".into(), "DEPENDENCY_MLHOOK".into()],
            ])
        );
    }

    #[test]
    fn unset_removes_key_and_save_drops_it() {
        let path = temp_path("unset");
        std::fs::write(
            &path,
            "warnings_as_errors = true\ngame_target = \"TURBO\"\n",
        )
        .unwrap();
        let mut file = UserConfigFile::load(&path).unwrap();
        file.unset("warnings_as_errors");
        file.save().unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("warnings_as_errors"),
            "unset key must leave the file: {written}"
        );
        assert!(
            written.contains("game_target"),
            "sibling key kept: {written}"
        );
        let reloaded = UserConfigFile::load(&path).unwrap();
        assert_eq!(reloaded.get_bool("warnings_as_errors"), None);
        assert_eq!(reloaded.get_string("game_target").as_deref(), Some("TURBO"));
    }

    #[test]
    fn save_preserves_comments_and_unknown_keys() {
        let path = temp_path("comments");
        std::fs::write(
            &path,
            "# bare TTY default\ndefault_mode = \"tui\"\n\n# keep me\nextra_future_key = 1\n",
        )
        .unwrap();
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_bool("warnings_as_errors", true);
        file.save().unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("# bare TTY default"),
            "header comment lost:\n{written}"
        );
        assert!(
            written.contains("# keep me"),
            "inline comment lost:\n{written}"
        );
        assert!(
            written.contains("extra_future_key"),
            "unknown key dropped:\n{written}"
        );
        assert!(
            written.contains("warnings_as_errors"),
            "new key missing:\n{written}"
        );
    }

    #[test]
    fn mutating_a_key_keeps_its_trailing_comment() {
        let path = temp_path("trailing-comment");
        std::fs::write(&path, "default_mode = \"tui\"  # trailing\n").unwrap();
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_string("default_mode", "lsp");
        file.save().unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("default_mode = \"lsp\""),
            "value not updated:\n{written}"
        );
        assert!(
            written.contains("# trailing"),
            "trailing comment lost:\n{written}"
        );
    }

    #[test]
    fn save_creates_missing_parent_dirs() {
        // The guard owns the OUTERMOST directory, so dropping it takes the
        // `nested/` levels this test exists to create with it.
        let base = crate::testing::TempDir::new("opl-user-config-parents");
        let dir = base.join("nested/openplanet-lsp");
        let path = dir.join("config.toml");
        let mut file = UserConfigFile::load(&path).unwrap();
        file.set_string("default_mode", "lsp");
        file.save().unwrap();
        assert!(path.is_file(), "parent dirs + file must exist");
        assert_eq!(
            UserConfigFile::load(&path)
                .unwrap()
                .get_string("default_mode")
                .as_deref(),
            Some("lsp")
        );
    }

    #[test]
    fn settings_catalog_covers_every_persisted_key() {
        let keys: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        for expected in [
            "openplanet_dir",
            "plugins_dir",
            "game_target",
            "defines",
            "default_mode",
            "source_paths",
            "ignore_paths",
            "honor_gitignore",
            "shortcut_namespaces",
            "shortcut_disable_default_namespaces",
            "shortcut_match_enums",
            "warnings_as_errors",
            "define_matrix",
            "define_groups",
            "define_combinations",
            "define_matrix_max_combinations",
            "unused",
            "unused_callbacks",
            "inactive_regions",
            "errors",
        ] {
            assert!(
                keys.contains(&expected),
                "SETTINGS missing persisted key {expected}"
            );
        }
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate SETTINGS keys");
    }

    #[test]
    fn nested_list_parse_formats_empty_and_multi_groups() {
        assert_eq!(parse_nested_list("").unwrap(), Vec::<Vec<String>>::new());
        assert_eq!(
            parse_nested_list("[] | [DEV] | [DEV, DEPENDENCY_MLHOOK]").unwrap(),
            vec![
                Vec::<String>::new(),
                vec!["DEV".into()],
                vec!["DEV".into(), "DEPENDENCY_MLHOOK".into()],
            ]
        );
        assert_eq!(
            format_nested_list(&[vec!["A".into(), "B".into()], vec!["C".into()]]),
            "[A, B] | [C]"
        );
        assert!(parse_nested_list("not-brackets").is_err());
    }

    #[test]
    fn workspace_config_path_is_dotfile_under_root() {
        assert_eq!(
            workspace_config_path(Path::new("/plugin")),
            PathBuf::from("/plugin/.openplanet-lsp.toml")
        );
        assert_eq!(
            workspace_config_path(Path::new("rel/root")),
            PathBuf::from("rel/root/.openplanet-lsp.toml")
        );
    }

    #[test]
    fn user_config_path_follows_home() {
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", "/tmp/opl-home-probe");
        let path = user_config_path();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/tmp/opl-home-probe/.config/openplanet-lsp/config.toml"
            ))
        );
    }
}
