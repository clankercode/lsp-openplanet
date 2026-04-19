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
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    openplanet_dir: Option<String>,
    plugins_dir: Option<String>,
    game_target: Option<String>,
    defines: Option<Vec<String>>,
}

impl LspConfig {
    /// Default all-permissive define set (spec Section 4.4)
    pub fn default_defines() -> HashSet<String> {
        [
            "TMNEXT",
            "MP4",
            "MP40",
            "MP41",
            "TURBO",
            "FOREVER",
            "UNITED_FOREVER",
            "NATIONS_FOREVER",
            "UNITED",
            "MP3",
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
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
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
        let defs = LspConfig::default_defines();
        assert!(defs.contains("TMNEXT"));
        assert!(defs.contains("SIG_DEVELOPER"));
        assert!(defs.contains("WINDOWS"));
        assert!(defs.contains("UNITED"));
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
