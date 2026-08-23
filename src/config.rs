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
    /// GH #60: skip `.as` files excluded by `.gitignore` rules at or below
    /// the plugin root during discovery (`honor_gitignore`). Tri-state so a
    /// workspace `.openplanet-lsp.toml` beats the user global; `None` =
    /// unset → the default (enabled). Read via
    /// [`LspConfig::honor_gitignore_enabled`].
    pub honor_gitignore: Option<bool>,
    /// Explicit plugin-root override (initializationOptions `plugin_roots`,
    /// a non-empty string array). When non-empty, replaces server-side root
    /// discovery entirely (manual override for unusual trees — t2). Init
    /// option ONLY: there is deliberately no config-file key.
    pub plugin_roots: Vec<PathBuf>,
    /// Additional LAST-priority dependency search dirs (multi-root t6:
    /// the opened folder appended so declared sibling deps resolve).
    /// Populated by the server, not config files.
    pub extra_plugins_dirs: Vec<PathBuf>,
    /// Custom namespaces for completion shortcuts (e.g. `["UX", "MathX"]`
    /// → `UX::DrawGrid` appears when typing `Dra`). `None` = default
    /// `["UX", "MathX"]`; explicit setting replaces the default. Combined
    /// with the built-in defaults `["UI", "Math"]` unless
    /// `shortcut_disable_default_namespaces` is set.
    pub shortcut_namespaces: Option<Vec<String>>,
    /// When true, only `shortcut_namespaces` (if any) get shortcut
    /// completions — the built-in `UI`/`Math` defaults are dropped.
    pub shortcut_disable_default_namespaces: bool,
    /// Also offer enum types and enum values from the shortcut namespaces
    /// (e.g. `UI::WindowFlags`, `UI::WindowFlags::NoTitleBar`). Off by
    /// default; functions are always matched.
    pub shortcut_match_enums: bool,
    /// GH #57: define-matrix axes (`.openplanet-lsp.toml` `define_matrix`).
    /// Each axis is toggled on/off on top of the base define set → the CLI
    /// `check` runs every combo through the full pipeline. Empty = single
    /// pass (current behavior). CLI-only: the LSP server keeps one live
    /// define set.
    pub define_matrix: Vec<String>,
    /// GH #57: mutually-exclusive define groups (`define_groups`). At most
    /// one member of a group is defined per combo; violating combos are
    /// skipped (never analyzed). The platform selectors {TMNEXT, MP4,
    /// TURBO} form an implicit group whenever ≥2 of them are axes.
    pub define_groups: Vec<Vec<String>>,
    /// GH #57: explicit combo pin list (`define_combinations`). When set
    /// (even `[]`), the cross-product is skipped entirely and ONLY these
    /// combos are checked.
    pub define_combinations: Option<Vec<Vec<String>>>,
    /// GH #57: matrix size cap (`define_matrix_max_combinations`).
    /// Cross-product exceeding the cap is a hard CLI error.
    pub define_matrix_max_combinations: usize,
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
            honor_gitignore: None,
            plugin_roots: Vec::new(),
            extra_plugins_dirs: Vec::new(),
            shortcut_namespaces: None,
            shortcut_disable_default_namespaces: false,
            shortcut_match_enums: false,
            define_matrix: Vec::new(),
            define_groups: Vec::new(),
            define_combinations: None,
            define_matrix_max_combinations: crate::cli::matrix::DEFAULT_MAX_COMBINATIONS,
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
    /// Blocklist of root-relative dirs to skip when checking `.as` files in
    /// (e.g. `["OtherPacks", "vendor"]`). Only used when `source_paths` is
    /// absent. Absent (and no `source_paths`) → check every `.as` under root.
    ignore_paths: Option<Vec<String>>,
    /// GH #60: honor `.gitignore` during `.as` discovery. Default `true`
    /// (absent = enabled). Workspace `.openplanet-lsp.toml` wins over the
    /// user config; CLI `--no-gitignore` wins over both.
    honor_gitignore: Option<bool>,
    /// Completion-shortcut namespaces (e.g. `["UX", "MathX", "Net"]`).
    /// Replaces the default `["UX", "MathX"]` when present.
    shortcut_namespaces: Option<Vec<String>>,
    /// Drop the built-in `UI`/`Math` shortcut namespaces.
    shortcut_disable_default_namespaces: Option<bool>,
    /// Include enums + enum values in shortcut completions.
    shortcut_match_enums: Option<bool>,
    /// GH #48 follow-up: make WARNING diagnostics fail `check` (exit 1).
    /// CLI `--warnings-as-errors`/`-Werror` (incl. `=false`) wins over this;
    /// workspace `.openplanet-lsp.toml` wins over the user config.
    warnings_as_errors: Option<bool>,
    /// GH #57: define-matrix axes (e.g. `["DEV", "DEPENDENCY_MLHOOK"]`).
    /// CLI `check` cross-multiplies them on top of the base define set.
    define_matrix: Option<Vec<String>>,
    /// GH #57: mutually-exclusive define groups (e.g.
    /// `[["TMNEXT", "MP4", "TURBO"]]`); combos defining two members of one
    /// group are skipped.
    define_groups: Option<Vec<Vec<String>>>,
    /// GH #57: explicit combo pin list; when present the cross-product is
    /// skipped and ONLY these combos are checked.
    define_combinations: Option<Vec<Vec<String>>>,
    /// GH #57: matrix size cap (default 16); exceeding it is a hard error.
    define_matrix_max_combinations: Option<usize>,
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
    /// GH #48 follow-up: `warnings_as_errors` from the config ladder
    /// (workspace `.openplanet-lsp.toml` over `~/.config/openplanet-lsp/
    /// config.toml`). The CLI flag — including an explicit `=false` —
    /// still takes precedence over this.
    pub warnings_as_errors: bool,
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
        if let Some(w) = cfg.warnings_as_errors {
            self.warnings_as_errors = w;
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
                let mut s: HashSet<String> = ALWAYS_ON.iter().map(|x| x.to_string()).collect();
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

    /// GH #45: the Openplanet install folder name for a game target.
    /// `None` for targets with no Openplanet build (MP3, NATIONS_FOREVER)
    /// — callers warn and fall back to the TMNEXT layout.
    pub fn openplanet_folder_for_target(target: &str) -> Option<&'static str> {
        match target {
            "TMNEXT" => Some("OpenplanetNext"),
            "MP4" | "MP40" | "MP41" => Some("OpenplanetMP4"),
            "TURBO" => Some("OpenplanetTurbo"),
            "UNITED" | "UNITED_FOREVER" => Some("OpenplanetUnited"),
            _ => None,
        }
    }

    /// GH #45: the game typedb filename inside the variant's install
    /// folder. Targets without an Openplanet build fall back to the
    /// TMNEXT database (warn-and-fallback per the issue).
    pub fn game_json_filename_for_target(target: &str) -> &'static str {
        match Self::openplanet_folder_for_target(target) {
            Some("OpenplanetNext") => "OpenplanetNext.json",
            Some("OpenplanetMP4") => "OpenplanetMP4.json",
            Some("OpenplanetTurbo") => "OpenplanetTurbo.json",
            Some("OpenplanetUnited") => "OpenplanetUnited.json",
            _ => "OpenplanetNext.json",
        }
    }

    /// The namespaces offered as completion shortcuts at identifier
    /// positions (old vscode-openplanet-angelscript parity): built-in
    /// `UI`/`Math` unless disabled, plus `shortcut_namespaces` (default
    /// `UX`/`MathX`).
    pub fn shortcut_namespace_list(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !self.shortcut_disable_default_namespaces {
            out.extend(["UI".to_string(), "Math".to_string()]);
        }
        match &self.shortcut_namespaces {
            Some(custom) => out.extend(custom.iter().cloned()),
            None => out.extend(["UX".to_string(), "MathX".to_string()]),
        }
        out
    }

    /// Build config from layers: auto-detect → parent (opened-folder) config
    /// → config file → init params. The parent layer only applies when the
    /// workspace root differs from the per-root path (multi-root t5).
    pub fn load(workspace_root: Option<&Path>, init_options: Option<&serde_json::Value>) -> Self {
        Self::load_with_parent(workspace_root, None, init_options)
    }

    /// Multi-root aware load: `parent` is the opened folder hosting multiple
    /// plugin roots. Ladder (lowest→highest): auto-detect defaults < parent
    /// config < per-root config < user config < init options < manifest
    /// defines. typedb settings stay global (resolved init > user > first
    /// root) — per-root: defines, plugins_dir, source_paths, ignore_paths.
    pub fn load_with_parent(
        workspace_root: Option<&Path>,
        parent: Option<&Path>,
        init_options: Option<&serde_json::Value>,
    ) -> Self {
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
            honor_gitignore: None,
            plugin_roots: Vec::new(),
            extra_plugins_dirs: Vec::new(),
            shortcut_namespaces: None,
            shortcut_disable_default_namespaces: false,
            shortcut_match_enums: false,
            define_matrix: Vec::new(),
            define_groups: Vec::new(),
            define_combinations: None,
            define_matrix_max_combinations: crate::cli::matrix::DEFAULT_MAX_COMBINATIONS,
        };

        // Layer 1: Auto-detect
        config.auto_detect();

        // Layer 2a: Parent (opened-folder) config — base for all shards.
        if let Some(parent) = parent {
            if Some(parent) != workspace_root {
                config.load_config_file(parent);
            }
        }

        // Layer 2b: Per-root config.
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

        // GH #45: the finalized target may expect a different install folder
        // than the Layer-1 TMNEXT default — re-probe before deriving paths.
        let auto_detected = config.openplanet_dir.clone();
        config.recheck_variant_folder(auto_detected.as_deref());
        if Self::openplanet_folder_for_target(&config.game_target).is_none() {
            // No-build target (MP3 / NATIONS_FOREVER): warn here too —
            // auto_detect couldn't know the final target yet.
            eprintln!(
                "warning: game target `{}` has no Openplanet build; \
                 falling back to the OpenplanetNext layout",
                config.game_target
            );
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

        // Derive JSON paths from openplanet_dir if not set explicitly.
        // The game JSON follows the target's variant (GH #45).
        if let Some(op_dir) = &config.openplanet_dir {
            if config.core_json.is_none() {
                let p = op_dir.join("OpenplanetCore.json");
                if p.exists() {
                    config.core_json = Some(p);
                }
            }
            if config.game_json.is_none() {
                let game_json_name = Self::game_json_filename_for_target(&config.game_target);
                let p = op_dir.join(game_json_name);
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
        // GH #45: auto-detection follows the game target's install folder.
        // At this layer the target is still the default (TMNEXT) — config
        // files / init options apply later and `recheck_variant_folder`
        // re-probes when they changed it.
        self.auto_detect_for_target("TMNEXT");
    }

    fn auto_detect_for_target(&mut self, target: &str) {
        let Some(folder) = Self::openplanet_folder_for_target(target) else {
            // MP3 / NATIONS_FOREVER: no Openplanet build exists. Warn and
            // fall back to the TMNEXT layout (issue GH #45 table).
            eprintln!(
                "warning: game target `{target}` has no Openplanet build; \
                 falling back to the OpenplanetNext layout"
            );
            return;
        };
        // Windows-style path via HOME
        if let Ok(home) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(&home).join(folder);
            if p.exists() {
                self.openplanet_dir = Some(p);
                return;
            }
        }
        // Linux / generic HOME
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join(folder);
            if p.exists() {
                self.openplanet_dir = Some(p);
            }
        }
    }

    /// GH #45: after the config ladder finalizes `game_target`, re-probe
    /// the variant folder when the auto-detected dir doesn't match. Only
    /// an explicit `openplanet_dir` (config / init options) wins over the
    /// variant probe; a default TMNEXT detection is replaced by the
    /// variant folder when it exists.
    fn recheck_variant_folder(&mut self, auto_detected: Option<&Path>) {
        let folder = match Self::openplanet_folder_for_target(&self.game_target) {
            Some(f) => f,
            None => return, // no-build targets already warned in auto_detect
        };
        let current_matches = self
            .openplanet_dir
            .as_deref()
            .is_some_and(|d| d.file_name().and_then(|n| n.to_str()) == Some(folder));
        if current_matches {
            return;
        }
        for var in ["USERPROFILE", "HOME"] {
            if let Ok(home) = std::env::var(var) {
                let p = PathBuf::from(&home).join(folder);
                if p.exists() {
                    // Replace the auto-detected dir only when it was the
                    // Layer-1 default (not an explicit user override).
                    if self.openplanet_dir.as_deref() == auto_detected {
                        self.openplanet_dir = Some(p);
                    }
                    return;
                }
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
        // GH #60: the user global is applied AFTER the workspace layers in
        // this ladder, so a repo-local `honor_gitignore` would otherwise lose
        // to it. Which files get checked is a per-repo decision (the same rule
        // `warnings_as_errors` follows), so a workspace-set value is restored
        // afterwards. The rest of the keys keep their existing precedence.
        let workspace_honor_gitignore = self.honor_gitignore;
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/openplanet-lsp/config.toml");
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if let Ok(file_config) = toml::from_str::<ConfigFile>(&contents) {
                    self.apply_config_file(file_config);
                }
            }
        }
        if workspace_honor_gitignore.is_some() {
            self.honor_gitignore = workspace_honor_gitignore;
        }
    }

    /// Effective `honor_gitignore` (GH #60): unset means enabled. The CLI
    /// `--no-gitignore` flag overrides this at the call site.
    pub fn honor_gitignore_enabled(&self) -> bool {
        self.honor_gitignore
            .unwrap_or(crate::workspace::project::DEFAULT_HONOR_GITIGNORE)
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
        if let Some(honor) = cfg.honor_gitignore {
            self.honor_gitignore = Some(honor);
        }
        if let Some(namespaces) = cfg.shortcut_namespaces {
            self.shortcut_namespaces = Some(namespaces);
        }
        if let Some(disable) = cfg.shortcut_disable_default_namespaces {
            self.shortcut_disable_default_namespaces = disable;
        }
        if let Some(match_enums) = cfg.shortcut_match_enums {
            self.shortcut_match_enums = match_enums;
        }
        if let Some(axes) = cfg.define_matrix {
            self.define_matrix = axes;
        }
        if let Some(groups) = cfg.define_groups {
            self.define_groups = groups;
        }
        if let Some(combos) = cfg.define_combinations {
            self.define_combinations = Some(combos);
        }
        if let Some(max) = cfg.define_matrix_max_combinations {
            self.define_matrix_max_combinations = max;
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
        // Plugin-root override (t2): non-empty string array replaces
        // server-side discovery. Empty / absent / non-string entries never
        // produce an override (empty stays empty → discovery runs).
        if let Some(namespaces) = opts.get("shortcut_namespaces").and_then(|v| v.as_array()) {
            self.shortcut_namespaces = Some(
                namespaces
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
            );
        }
        if let Some(disable) = opts
            .get("shortcut_disable_default_namespaces")
            .and_then(|v| v.as_bool())
        {
            self.shortcut_disable_default_namespaces = disable;
        }
        if let Some(match_enums) = opts.get("shortcut_match_enums").and_then(|v| v.as_bool()) {
            self.shortcut_match_enums = match_enums;
        }
        if let Some(roots) = opts.get("plugin_roots").and_then(|v| v.as_array()) {
            let parsed: Vec<PathBuf> = roots
                .iter()
                .filter_map(|v| v.as_str())
                .map(PathBuf::from)
                .collect();
            if !parsed.is_empty() {
                self.plugin_roots = parsed;
            }
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

    #[test]
    fn test_plugin_roots_absent_init_option_means_no_override() {
        // No `plugin_roots` key in initializationOptions → empty (walk wins).
        let opts = serde_json::json!({ "game_target": "MP4" });
        let config = LspConfig::load(None, Some(&opts));
        assert!(config.plugin_roots.is_empty());
    }

    #[test]
    fn test_plugin_roots_empty_init_option_means_no_override() {
        // Explicit empty array → still no override (non-empty required).
        let opts = serde_json::json!({ "plugin_roots": [] });
        let config = LspConfig::load(None, Some(&opts));
        assert!(config.plugin_roots.is_empty());
    }

    #[test]
    fn test_plugin_roots_nonempty_init_option_is_parsed() {
        let opts = serde_json::json!({
            "plugin_roots": ["/tmp/plugin-a", "/tmp/plugin-b"]
        });
        let config = LspConfig::load(None, Some(&opts));
        assert_eq!(
            config.plugin_roots,
            vec![
                PathBuf::from("/tmp/plugin-a"),
                PathBuf::from("/tmp/plugin-b")
            ]
        );
    }

    #[test]
    fn test_plugin_roots_ignores_non_string_entries() {
        // Non-string entries are skipped, not fatal.
        let opts = serde_json::json!({ "plugin_roots": ["/tmp/ok", 42, null] });
        let config = LspConfig::load(None, Some(&opts));
        assert_eq!(config.plugin_roots, vec![PathBuf::from("/tmp/ok")]);
    }

    // ── GH #57: define matrix config keys ───────────────────────────────

    #[test]
    fn define_matrix_keys_default_off() {
        let cfg = LspConfig::default();
        assert!(cfg.define_matrix.is_empty());
        assert!(cfg.define_groups.is_empty());
        assert!(cfg.define_combinations.is_none());
        assert_eq!(cfg.define_matrix_max_combinations, 16);
    }

    #[test]
    fn define_matrix_keys_parse_from_config_file() {
        let tmp = std::env::temp_dir().join(format!("opl-defmatrix-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            r#"define_matrix = ["DEV", "DEPENDENCY_MLHOOK"]
define_groups = [["A", "B"]]
define_matrix_max_combinations = 32
"#,
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(cfg.define_matrix, vec!["DEV", "DEPENDENCY_MLHOOK"]);
        assert_eq!(cfg.define_groups, vec![vec!["A", "B"]]);
        assert_eq!(cfg.define_combinations, None);
        assert_eq!(cfg.define_matrix_max_combinations, 32);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn define_combinations_parse_from_config_file() {
        let tmp = std::env::temp_dir().join(format!("opl-defcombos-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            r#"define_combinations = [[], ["DEV"], ["DEV", "DEPENDENCY_MLHOOK"]]
"#,
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(
            cfg.define_combinations,
            Some(vec![
                Vec::<String>::new(),
                vec!["DEV".to_string()],
                vec!["DEV".to_string(), "DEPENDENCY_MLHOOK".to_string()],
            ])
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── GH #45: variant-aware folder / typedb selection ──────────────────

    #[test]
    fn test_variant_folder_names() {
        // TMNEXT maps to the default layout.
        assert_eq!(
            LspConfig::openplanet_folder_for_target("TMNEXT"),
            Some("OpenplanetNext")
        );
        // MP4 family all map to the MP4 folder.
        for t in ["MP4", "MP40", "MP41"] {
            assert_eq!(
                LspConfig::openplanet_folder_for_target(t),
                Some("OpenplanetMP4"),
                "target {t}"
            );
        }
        assert_eq!(
            LspConfig::openplanet_folder_for_target("TURBO"),
            Some("OpenplanetTurbo")
        );
        assert_eq!(
            LspConfig::openplanet_folder_for_target("UNITED"),
            Some("OpenplanetUnited")
        );
        assert_eq!(
            LspConfig::openplanet_folder_for_target("UNITED_FOREVER"),
            Some("OpenplanetUnited")
        );
        // MP3 / NATIONS_FOREVER have no Openplanet build → no folder.
        assert_eq!(LspConfig::openplanet_folder_for_target("MP3"), None);
        assert_eq!(
            LspConfig::openplanet_folder_for_target("NATIONS_FOREVER"),
            None
        );
    }

    #[test]
    fn test_variant_game_json_names() {
        assert_eq!(
            LspConfig::game_json_filename_for_target("TMNEXT"),
            "OpenplanetNext.json"
        );
        assert_eq!(
            LspConfig::game_json_filename_for_target("MP4"),
            "OpenplanetMP4.json"
        );
        assert_eq!(
            LspConfig::game_json_filename_for_target("TURBO"),
            "OpenplanetTurbo.json"
        );
        assert_eq!(
            LspConfig::game_json_filename_for_target("UNITED"),
            "OpenplanetUnited.json"
        );
        // Targets without an Openplanet build fall back to the TMNEXT DB
        // (warn-and-fallback per the issue).
        assert_eq!(
            LspConfig::game_json_filename_for_target("MP3"),
            "OpenplanetNext.json"
        );
    }

    #[test]
    fn test_game_json_derivation_uses_variant_folder() {
        // Build a fake variant install: home/OpenplanetTurbo with the two
        // JSON files, plus an OpenplanetNext alongside to prove selection.
        let tmp = std::env::temp_dir().join(format!("oplsp-gh45-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let turbo = tmp.join("OpenplanetTurbo");
        let next = tmp.join("OpenplanetNext");
        std::fs::create_dir_all(&turbo).unwrap();
        std::fs::create_dir_all(&next).unwrap();
        std::fs::write(turbo.join("OpenplanetCore.json"), "{}").unwrap();
        std::fs::write(turbo.join("OpenplanetTurbo.json"), "{}").unwrap();
        std::fs::write(next.join("OpenplanetCore.json"), "{}").unwrap();
        std::fs::write(next.join("OpenplanetNext.json"), "{}").unwrap();
        std::fs::create_dir_all(turbo.join("Plugins")).unwrap();
        std::fs::create_dir_all(next.join("Plugins")).unwrap();

        // SAFETY for tests: scoped env mutation is fine here — cargo runs
        // lib tests in one process, but this test owns a unique HOME value
        // and config load is deterministic given HOME.
        std::env::set_var("HOME", &tmp);
        let opts = serde_json::json!({ "game_target": "TURBO" });
        let config = LspConfig::load(None, Some(&opts));
        std::env::remove_var("HOME");

        assert_eq!(
            config.openplanet_dir,
            Some(turbo.clone()),
            "TURBO target must pick the OpenplanetTurbo folder"
        );
        assert_eq!(
            config.game_json,
            Some(turbo.join("OpenplanetTurbo.json")),
            "game_json must be the Turbo typedb"
        );
        assert_eq!(
            config.plugins_dir,
            Some(turbo.join("Plugins")),
            "plugins_dir must derive from the variant folder"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // === Namespace-shortcut config (completion QoL) ===

    #[test]
    fn shortcut_namespace_list_default_includes_builtins_and_custom_defaults() {
        let cfg = LspConfig::default();
        assert_eq!(
            cfg.shortcut_namespace_list(),
            vec!["UI", "Math", "UX", "MathX"]
        );
        assert!(!cfg.shortcut_match_enums);
    }

    #[test]
    fn shortcut_namespace_list_custom_replaces_custom_defaults() {
        let cfg = LspConfig {
            shortcut_namespaces: Some(vec!["Net".to_string(), "Audio".to_string()]),
            ..LspConfig::default()
        };
        assert_eq!(
            cfg.shortcut_namespace_list(),
            vec!["UI", "Math", "Net", "Audio"]
        );
    }

    #[test]
    fn shortcut_namespace_list_disable_defaults_keeps_custom_only() {
        let cfg = LspConfig {
            shortcut_disable_default_namespaces: true,
            shortcut_namespaces: Some(vec!["UX".to_string()]),
            ..LspConfig::default()
        };
        assert_eq!(cfg.shortcut_namespace_list(), vec!["UX"]);
    }

    #[test]
    fn shortcut_namespace_list_disable_defaults_with_no_custom_is_empty() {
        let cfg = LspConfig {
            shortcut_disable_default_namespaces: true,
            shortcut_namespaces: Some(Vec::new()),
            ..LspConfig::default()
        };
        assert!(cfg.shortcut_namespace_list().is_empty());
    }

    #[test]
    fn shortcut_namespace_list_disable_defaults_alone_keeps_custom_defaults() {
        let cfg = LspConfig {
            shortcut_disable_default_namespaces: true,
            ..LspConfig::default()
        };
        assert_eq!(cfg.shortcut_namespace_list(), vec!["UX", "MathX"]);
    }

    #[test]
    fn shortcut_settings_parse_from_config_file() {
        let tmp = std::env::temp_dir().join(format!("opl-shortcut-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            r#"shortcut_namespaces = ["Net", "Audio"]
shortcut_disable_default_namespaces = true
shortcut_match_enums = true
"#,
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(cfg.shortcut_namespace_list(), vec!["Net", "Audio"]);
        assert!(cfg.shortcut_match_enums);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn shortcut_settings_parse_from_init_options() {
        let opts = serde_json::json!({
            "shortcut_namespaces": ["Data"],
            "shortcut_disable_default_namespaces": true,
            "shortcut_match_enums": true
        });
        let cfg = LspConfig::load(None, Some(&opts));
        assert_eq!(cfg.shortcut_namespace_list(), vec!["Data"]);
        assert!(cfg.shortcut_match_enums);
    }

    /// GH #48 follow-up: `warnings_as_errors` config key reaches UserPrefs
    /// through the workspace `.openplanet-lsp.toml` layer. Both cases are
    /// deterministic without pinning $HOME because the workspace layer is
    /// applied last — it wins over any user-global value.
    #[test]
    fn test_user_prefs_warnings_as_errors_from_workspace_toml() {
        let tmp = std::env::temp_dir().join(format!("oplsp-wae-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let ws_true = tmp.join("ws-true");
        std::fs::create_dir_all(&ws_true).unwrap();
        std::fs::write(
            ws_true.join(".openplanet-lsp.toml"),
            "warnings_as_errors = true\n",
        )
        .unwrap();
        assert!(
            UserPrefs::load(Some(&ws_true)).warnings_as_errors,
            "workspace warnings_as_errors = true must reach UserPrefs"
        );

        // Explicit false must not coerce to true (regression guard for the
        // key being honored as written).
        let ws_false = tmp.join("ws-false");
        std::fs::create_dir_all(&ws_false).unwrap();
        std::fs::write(
            ws_false.join(".openplanet-lsp.toml"),
            "warnings_as_errors = false\n",
        )
        .unwrap();
        assert!(
            !UserPrefs::load(Some(&ws_false)).warnings_as_errors,
            "workspace warnings_as_errors = false must stay false"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // NOTE: the user-global layer ($HOME/.config/openplanet-lsp/config.toml)
    // is deliberately NOT unit-tested here — mutating $HOME in-process races
    // with `test_game_json_derivation_uses_variant_folder` above (lib tests
    // share one process). It's covered end-to-end by the subprocess tests in
    // tests/cli_check_tests.rs, which own their environment.
}
