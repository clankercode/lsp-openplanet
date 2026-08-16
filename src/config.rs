use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::workspace::manifest::Manifest;

#[derive(Debug, Clone)]
pub struct LspConfig {
    pub openplanet_dir: Option<PathBuf>,
    pub plugins_dir: Option<PathBuf>,
    pub core_json: Option<PathBuf>,
    pub game_json: Option<PathBuf>,
    pub game_target: String,
    pub defines: HashSet<String>,
    /// True when `defines` came from an explicit source (config file / init
    /// options) rather than the target-derived default. When false, `load`
    /// re-derives defines from the final `game_target` so a non-default target
    /// (set via repo-local config or init options) gets matching platform
    /// defines without the user also hand-listing them (GH #36).
    #[doc(hidden)]
    pub defines_overridden: bool,
    /// Allowlist of root-relative dirs to check `.as` files in (from
    /// `.openplanet-lsp.toml` `source_paths`). `Some` → check only these.
    pub source_paths: Option<Vec<PathBuf>>,
    /// Blocklist of root-relative dirs to skip (from `.openplanet-lsp.toml`
    /// `ignore_paths`). Only consulted when `source_paths` is `None`.
    pub ignore_paths: Option<Vec<PathBuf>>,
}

impl Default for LspConfig {
    /// A minimal config with only the standard set of preprocessor defines
    /// enabled. Used by tests and by callers that don't care about
    /// auto-detection or user config files.
    fn default() -> Self {
        Self {
            openplanet_dir: None,
            plugins_dir: None,
            core_json: None,
            game_json: None,
            game_target: "TMNEXT".to_string(),
            defines: Self::default_defines(),
            defines_overridden: false,
            source_paths: None,
            ignore_paths: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    openplanet_dir: Option<String>,
    plugins_dir: Option<String>,
    game_target: Option<String>,
    defines: Option<Vec<String>>,
    /// Bare-TTY default: `"tui"` (default) or `"lsp"`.
    default_mode: Option<String>,
    /// Allowlist of root-relative dirs to check `.as` files in (e.g.
    /// `["src"]`). When set, ONLY these are checked. Takes precedence over
    /// `ignore_paths`. Absent → fall through to `ignore_paths`, else check all.
    source_paths: Option<Vec<String>>,
    /// Blocklist of root-relative dirs to skip when checking `.as` files
    /// (e.g. `["OtherPacks", "vendor"]`). Only used when `source_paths` is
    /// absent. Absent (and no `source_paths`) → check every `.as` under root.
    ignore_paths: Option<Vec<String>>,
}

/// How bare TTY launches behave when no subcommand is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultMode {
    #[default]
    Tui,
    Lsp,
}

impl DefaultMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tui" | "watch" | "check" => Some(Self::Tui),
            "lsp" | "server" | "stdio" => Some(Self::Lsp),
            _ => None,
        }
    }
}

/// User/workspace preferences that affect CLI entrypoints (not LSP typecheck).
#[derive(Debug, Clone, Default)]
pub struct UserPrefs {
    pub default_mode: DefaultMode,
}

impl UserPrefs {
    /// Load `default_mode` from workspace `.openplanet-lsp.toml` then
    /// `~/.config/openplanet-lsp/config.toml` (later wins? workspace should win).
    /// Order: user global first, workspace overrides.
    pub fn load(workspace_root: Option<&Path>) -> Self {
        let mut prefs = Self::default();
        prefs.apply_user_config();
        if let Some(root) = workspace_root {
            prefs.apply_workspace_config(root);
        }
        prefs
    }

    fn apply_user_config(&mut self) {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/openplanet-lsp/config.toml");
            self.apply_file(&path);
        }
    }

    fn apply_workspace_config(&mut self, root: &Path) {
        self.apply_file(&root.join(".openplanet-lsp.toml"));
    }

    fn apply_file(&mut self, path: &Path) {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(cfg) = toml::from_str::<ConfigFile>(&contents) else {
            return;
        };
        if let Some(mode) = cfg.default_mode.as_deref().and_then(DefaultMode::parse) {
            self.default_mode = mode;
        }
    }
}

impl LspConfig {
    /// Default all-permissive define set (spec Section 4.4)
    /// Preprocessor defines for a game target. Platform selectors are
    /// target-specific so the LSP only compiles the `#if` branches the real
    /// game would (GH #36): defining every platform at once made dead
    /// `#if MP4` / `#elif TURBO` branches live and flooded undefined-identifier
    /// false positives. Default target is TMNEXT (TM2020); overridable via
    /// `game_target` (config file / init options / repo-local config).
    pub fn defines_for_target(target: &str) -> HashSet<String> {
        // Always-on flags (OS, signature, build caps) — independent of which
        // game the plugin targets.
        const ALWAYS_ON: &[&str] = &[
            "MANIA64",
            "MANIA32",
            "WINDOWS",
            "WINDOWS_WINE",
            "LINUX",
            "SERVER",
            "LOGS",
            "HAS_DEV",
            "DEVELOPER",
            "SIG_OFFICIAL",
            "SIG_REGULAR",
            "SIG_SCHOOL",
            "SIG_DEVELOPER",
        ];
        // Platform selector(s) for the requested game. The game defines
        // exactly one ManiaPlanet-generation family; the others stay off so
        // their `#if` branches are dead (matching the game). Evidence (GH #36,
        // tm-skids-magician): TM2020 treats `#if MP4` and `#elif TURBO` as
        // DEAD — so TMNEXT does NOT imply MP4/TURBO.
        let platform: &[&str] = match target {
            "TMNEXT" => &["TMNEXT"],
            "MP4" | "MP40" | "MP41" => &["MP4", "MP40", "MP41"],
            "TURBO" => &["TURBO"],
            "MP3" => &["MP3"],
            "UNITED_FOREVER" | "UNITED" => &["UNITED_FOREVER", "UNITED"],
            "NATIONS_FOREVER" => &["NATIONS_FOREVER"],
            // Unknown target: define the target token itself so its `#if`
            // branch is live; everything else off (conservative).
            other => {
                let mut s: HashSet<String> =
                    ALWAYS_ON.iter().map(|x| x.to_string()).collect();
                s.insert(other.to_string());
                return s;
            }
        };
        ALWAYS_ON
            .iter()
            .chain(platform.iter())
            .map(|s| s.to_string())
            .collect()
    }

    /// Standard defines for the default target (TMNEXT / TM2020).
    pub fn default_defines() -> HashSet<String> {
        Self::defines_for_target("TMNEXT")
    }

    /// Build config from layers: auto-detect → config file → init params
    pub fn load(workspace_root: Option<&Path>, init_options: Option<&serde_json::Value>) -> Self {
        let mut config = Self {
            openplanet_dir: None,
            plugins_dir: None,
            core_json: None,
            game_json: None,
            game_target: "TMNEXT".to_string(),
            defines: Self::default_defines(),
            defines_overridden: false,
            source_paths: None,
            ignore_paths: None,
        };

        // Layer 1: Auto-detect
        config.auto_detect();

        // Layer 2: Config file
        if let Some(root) = workspace_root {
            config.load_config_file(root);
        }
        config.load_user_config_file();

        // Layer 3: Init params (highest priority)
        if let Some(opts) = init_options {
            config.apply_init_options(opts);
        }

        // GH #36: `game_target` is now final. Unless defines were explicitly
        // overridden, re-derive them from the target so a non-default target
        // (repo-local config / init options) gets matching platform defines and
        // dead `#if <other-platform>` branches stay dead.
        if !config.defines_overridden {
            config.defines = Self::defines_for_target(&config.game_target);
        }

        // Layer 4: Workspace manifest-derived defines. These are additive and
        // model how Openplanet compiles the plugin with dependency and
        // script-defined preprocessor symbols enabled.
        if let Some(root) = workspace_root {
            let manifest_path = root.join("info.toml");
            if let Ok(manifest) = Manifest::load(&manifest_path) {
                config.apply_manifest(&manifest);
            }
        }

        // Derive JSON paths from openplanet_dir if not set explicitly
        if let Some(op_dir) = &config.openplanet_dir {
            if config.core_json.is_none() {
                let p = op_dir.join("OpenplanetCore.json");
                if p.exists() {
                    config.core_json = Some(p);
                }
            }
            if config.game_json.is_none() {
                let p = op_dir.join("OpenplanetNext.json");
                if p.exists() {
                    config.game_json = Some(p);
                }
            }
            if config.plugins_dir.is_none() {
                let p = op_dir.join("Plugins");
                if p.exists() {
                    config.plugins_dir = Some(p);
                }
            }
        }

        config
    }

    pub fn apply_manifest(&mut self, manifest: &Manifest) {
        let Some(script) = &manifest.script else {
            return;
        };

        for define in &script.defines {
            self.defines.insert(define.clone());
        }
        for dep in &script.dependencies {
            self.defines
                .insert(format!("DEPENDENCY_{}", dependency_define_suffix(dep)));
        }
        for dep in &script.optional_dependencies {
            self.defines
                .insert(format!("DEPENDENCY_{}", dependency_define_suffix(dep)));
        }
        for dep in &script.export_dependencies {
            let suffix = dependency_define_suffix(dep);
            self.defines.insert(format!("DEPENDENCY_{}", suffix));
            self.defines.insert(format!("EXPORT_DEPENDENCY_{}", suffix));
        }
    }

    fn auto_detect(&mut self) {
        // Windows-style path via HOME
        if let Ok(home) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(&home).join("OpenplanetNext");
            if p.exists() {
                self.openplanet_dir = Some(p);
                return;
            }
        }
        // Linux / generic HOME
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join("OpenplanetNext");
            if p.exists() {
                self.openplanet_dir = Some(p);
            }
        }
    }

    fn load_config_file(&mut self, workspace_root: &Path) {
        let path = workspace_root.join(".openplanet-lsp.toml");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(file_config) = toml::from_str::<ConfigFile>(&contents) {
                self.apply_config_file(file_config);
            }
        }
    }

    fn load_user_config_file(&mut self) {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/openplanet-lsp/config.toml");
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if let Ok(file_config) = toml::from_str::<ConfigFile>(&contents) {
                    self.apply_config_file(file_config);
                }
            }
        }
    }

    fn apply_config_file(&mut self, cfg: ConfigFile) {
        if let Some(dir) = cfg.openplanet_dir {
            self.openplanet_dir = Some(PathBuf::from(dir));
        }
        if let Some(dir) = cfg.plugins_dir {
            self.plugins_dir = Some(PathBuf::from(dir));
        }
        if let Some(target) = cfg.game_target {
            self.game_target = target;
        }
        if let Some(defines) = cfg.defines {
            self.defines = defines.into_iter().collect();
            self.defines_overridden = true;
        }
        if let Some(source_paths) = cfg.source_paths {
            self.source_paths = Some(source_paths.into_iter().map(PathBuf::from).collect());
        }
        if let Some(ignore_paths) = cfg.ignore_paths {
            self.ignore_paths = Some(ignore_paths.into_iter().map(PathBuf::from).collect());
        }
    }

    fn apply_init_options(&mut self, opts: &serde_json::Value) {
        if let Some(dir) = opts.get("openplanet_dir").and_then(|v| v.as_str()) {
            self.openplanet_dir = Some(PathBuf::from(dir));
        }
        if let Some(dir) = opts.get("plugins_dir").and_then(|v| v.as_str()) {
            self.plugins_dir = Some(PathBuf::from(dir));
        }
        if let Some(target) = opts.get("game_target").and_then(|v| v.as_str()) {
            self.game_target = target.to_string();
        }
        if let Some(defines) = opts.get("defines").and_then(|v| v.as_array()) {
            self.defines = defines
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            self.defines_overridden = true;
        }
    }
}

fn dependency_define_suffix(dep: &str) -> String {
    dep.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_defines() {
        // Default target is TMNEXT (TM2020): TMNEXT on, always-on flags on,
        // and other ManiaPlanet generations OFF (GH #36 — dead `#if` branches).
        let defs = LspConfig::default_defines();
        assert!(defs.contains("TMNEXT"));
        assert!(defs.contains("SIG_DEVELOPER"));
        assert!(defs.contains("WINDOWS"));
        assert!(!defs.contains("TURBO"));
        assert!(!defs.contains("MP4"));
        assert!(!defs.contains("MP3"));
        assert!(!defs.contains("UNITED"));
    }

    #[test]
    fn test_defines_for_target_selects_platform() {
        let turbo = LspConfig::defines_for_target("TURBO");
        assert!(turbo.contains("TURBO"));
        assert!(turbo.contains("WINDOWS")); // always-on still present
        assert!(!turbo.contains("TMNEXT"));
        assert!(!turbo.contains("MP4"));

        let mp4 = LspConfig::defines_for_target("MP4");
        assert!(mp4.contains("MP4"));
        assert!(!mp4.contains("TMNEXT"));
        assert!(!mp4.contains("TURBO"));
    }

    #[test]
    fn test_game_target_rederives_defines_when_not_overridden() {
        // Setting game_target (no explicit defines) re-derives defines.
        let opts = serde_json::json!({ "game_target": "TURBO" });
        let config = LspConfig::load(None, Some(&opts));
        assert_eq!(config.game_target, "TURBO");
        assert!(config.defines.contains("TURBO"));
        assert!(!config.defines.contains("TMNEXT"));
    }

    #[test]
    fn test_explicit_defines_win_over_target_derivation() {
        // Explicit defines override target derivation (init options).
        let opts = serde_json::json!({ "game_target": "TURBO", "defines": ["CUSTOM"] });
        let config = LspConfig::load(None, Some(&opts));
        assert!(config.defines.contains("CUSTOM"));
        assert!(!config.defines.contains("TURBO"));
    }

    #[test]
    fn test_init_options_override() {
        let opts = serde_json::json!({
            "game_target": "MP4",
            "defines": ["MP4", "WINDOWS"]
        });
        let config = LspConfig::load(None, Some(&opts));
        assert_eq!(config.game_target, "MP4");
        assert_eq!(config.defines.len(), 2);
        assert!(config.defines.contains("MP4"));
    }
}
