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
    /// Which signature level's `SIG_*` defines are live (GH #64). Resolved
    /// through the same ladder as `game_target`; the defines are derived
    /// from it in [`LspConfig::defines_for_target`], never hand-listed.
    pub signature_mode: SignatureMode,
    /// Which host OS's defines are live (B028): `windows` (default) |
    /// `wine` | `linux`, via the same ladder as `game_target`. Wine keeps
    /// plain `WINDOWS` and adds `WINDOWS_WINE` (verified in the live game,
    /// 2026-08-29). Defaults changed in B028: `LINUX` / `WINDOWS_WINE` are
    /// no longer always-on.
    pub os_target: OsTarget,
    /// Which game architecture's define is live (B028): `64` (default) |
    /// `32` — exactly one of `MANIA64` / `MANIA32`, never both.
    pub arch_target: ArchTarget,
    /// Build-shape flags (B028): `DEVELOPER` / `HAS_DEV` / `LOGS` /
    /// `SERVER`, all default TRUE (decision (a) — no default churn);
    /// narrow via config / CLI to model a real client.
    pub build_flags: BuildFlags,
    pub defines: HashSet<String>,
    /// Raw `defines` entries from every config layer (parent config, per-root
    /// config, user global, init options), in the order they were read. They
    /// are ADDITIVE over the target-derived set and are applied last — after
    /// `defines_for_target` and the manifest — by
    /// [`LspConfig::apply_define_overrides`] (B024). An entry prefixed `-` or
    /// `!` removes that define instead of adding it.
    #[doc(hidden)]
    pub define_overrides: Vec<String>,
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
    /// GH #56: extra names the unused-declaration pass treats as
    /// runtime-invoked callbacks (`unused_callbacks`), on top of the
    /// built-in `OPENPLANET_CALLBACKS`. Frameworks that dispatch by name
    /// reflection (MLHook's `OnCounterUpdate`) are invisible to a fixed
    /// list. Resolved by [`LspConfig::resolve_unused_callbacks`] — empty
    /// when no config file sets the key.
    pub unused_callbacks: Vec<String>,
    /// How unused-declaration findings are reported. Default warn.
    /// Workspace `.openplanet-lsp.toml` wins over the user global; CLI
    /// `--unused` / `--no-unused` wins over both.
    pub unused: UnusedLevel,
    /// How one-shot `check` groups diagnostics for human formats.
    /// Default last (non-errors, then errors). JSON is never regrouped.
    /// Workspace `.openplanet-lsp.toml` wins over the user global; CLI
    /// `--errors` wins over both.
    pub errors: ErrorGroup,
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
            signature_mode: SignatureMode::default(),
            os_target: OsTarget::default(),
            arch_target: ArchTarget::default(),
            build_flags: BuildFlags::default(),
            defines: Self::default_defines(),
            define_overrides: Vec::new(),
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
            unused_callbacks: Vec::new(),
            unused: UnusedLevel::Warn,
            errors: ErrorGroup::Last,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    openplanet_dir: Option<String>,
    plugins_dir: Option<String>,
    game_target: Option<String>,
    /// GH #64: `official` / `regular` / `school` / `developer` (default).
    signature_mode: Option<String>,
    /// B028: `windows` (default) | `wine` | `linux`. Wine defines BOTH
    /// `WINDOWS` and `WINDOWS_WINE` (verified in the live game).
    os_target: Option<String>,
    /// B028: `64` (default) | `32` — exactly one `MANIA*` arch is live.
    arch_target: Option<String>,
    /// B028 build-shape flags, all default true (decision (a)). `false`
    /// narrows the profile toward a real client build.
    developer_build: Option<bool>,
    has_dev: Option<bool>,
    logs: Option<bool>,
    server: Option<bool>,
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

/// How unused-declaration findings are reported.
///
/// Default is [`UnusedLevel::Warn`] (today's behavior). `ignore` drops the
/// pass entirely (it is noisy on large plugins). `error` emits ERROR
/// diagnostics so `check` fails without `--warnings-as-errors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnusedLevel {
    Ignore,
    #[default]
    Warn,
    Error,
}

impl UnusedLevel {
    /// Parse a config / CLI token. Aliases: off/false/none/disable → ignore;
    /// on/true/warning → warn; err/errors → error.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ignore" | "off" | "false" | "none" | "disable" | "disabled" => Some(Self::Ignore),
            "warn" | "warning" | "on" | "true" => Some(Self::Warn),
            "error" | "err" | "errors" => Some(Self::Error),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Which Openplanet signature level the analysis models (GH #64).
///
/// Openplanet defines a signature macro "when the current signature level is
/// equal or below their respective level"
/// (<https://openplanet.dev/docs/reference/preprocessor>), so the levels are
/// CUMULATIVE rather than exclusive: Official defines `SIG_OFFICIAL` alone,
/// Developer defines all four. Default is [`SignatureMode::Developer`] —
/// the all-four set the LSP always used, so nothing moves until you opt in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignatureMode {
    Official,
    Regular,
    School,
    #[default]
    Developer,
}

impl SignatureMode {
    /// Parse a config / CLI / init-option token, case-insensitively.
    /// Aliases: `dev` → developer, `reg` → regular, and the define token
    /// itself (`SIG_SCHOOL` → school) since that is how the docs name them.
    /// Deliberately no `off` alias — that token means "disabled" for
    /// `unused` / `errors` and would read as the opposite of a level here.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "official" | "sig_official" => Some(Self::Official),
            "regular" | "reg" | "sig_regular" => Some(Self::Regular),
            "school" | "sig_school" => Some(Self::School),
            "developer" | "dev" | "sig_developer" => Some(Self::Developer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Regular => "regular",
            Self::School => "school",
            Self::Developer => "developer",
        }
    }

    /// The signature defines live at this level, lowest level first.
    pub fn defines(self) -> &'static [&'static str] {
        const ALL: &[&str] = &["SIG_OFFICIAL", "SIG_REGULAR", "SIG_SCHOOL", "SIG_DEVELOPER"];
        match self {
            Self::Official => &ALL[..1],
            Self::Regular => &ALL[..2],
            Self::School => &ALL[..3],
            Self::Developer => ALL,
        }
    }

    /// Every accepted canonical name, for help text and the config TUI.
    pub const CHOICES: &'static [&'static str] = &["developer", "school", "regular", "official"];
}

/// Which host OS the analysis models (B028). A real process defines exactly
/// one OS family. Empirically verified against the live game under Wine
/// (RemoteBuild probe, 2026-08-29): a Wine run defines BOTH `WINDOWS` and
/// `WINDOWS_WINE`; the probe fixtures
/// `tests/fixtures/issue-repros/b028-os-arch-defines-probe` pin this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OsTarget {
    #[default]
    Windows,
    Wine,
    Linux,
}

impl OsTarget {
    /// Parse a config / CLI / init-option token, case-insensitively.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "windows" => Some(Self::Windows),
            "wine" => Some(Self::Wine),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Wine => "wine",
            Self::Linux => "linux",
        }
    }

    /// The OS defines live for this target. Wine keeps plain `WINDOWS` —
    /// the game defines it alongside `WINDOWS_WINE` (probe 2026-08-29).
    pub fn defines(self) -> &'static [&'static str] {
        match self {
            Self::Windows => &["WINDOWS"],
            Self::Wine => &["WINDOWS", "WINDOWS_WINE"],
            Self::Linux => &["LINUX"],
        }
    }

    /// Every accepted canonical name, for help text and warnings.
    pub const CHOICES: &'static [&'static str] = &["windows", "wine", "linux"];
}

/// Which game architecture the analysis models (B028): a real process has
/// exactly one of `MANIA64` / `MANIA32` live, never both — defining both
/// made both `#if` branches live and manufactured phantom duplicate
/// declarations (the reported failure mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArchTarget {
    #[default]
    Arch64,
    Arch32,
}

impl ArchTarget {
    /// Parse a config / CLI / init-option token.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "64" => Some(Self::Arch64),
            "32" => Some(Self::Arch32),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arch64 => "64",
            Self::Arch32 => "32",
        }
    }

    /// The arch define live for this target.
    pub fn defines(self) -> &'static [&'static str] {
        match self {
            Self::Arch64 => &["MANIA64"],
            Self::Arch32 => &["MANIA32"],
        }
    }

    /// Every accepted canonical name, for help text and warnings.
    pub const CHOICES: &'static [&'static str] = &["64", "32"];
}

/// B028 build-shape flags: what kind of build the analysis models. All four
/// default TRUE — coordinator decision (a) keeps today's analysis set so no
/// user's code silently stops being checked; narrow via config / CLI toward
/// a real client profile (verified 2026-08-29: a normal player's client has
/// NONE of these defined — see
/// `tests/fixtures/issue-repros/b028-build-flag-probe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildFlags {
    /// `DEVELOPER` — "the current Openplanet build is a debug build".
    pub developer_build: bool,
    /// `HAS_DEV` — "the current game is a Nadeo development build".
    pub has_dev: bool,
    /// `LOGS` — "a Logs-configured build".
    pub logs: bool,
    /// `SERVER` — "a dedicated server build".
    pub server: bool,
}

impl Default for BuildFlags {
    /// Decision (a): all on — exactly the always-on set the LSP used, so
    /// the default analysis is unchanged by B028 for these four.
    fn default() -> Self {
        Self {
            developer_build: true,
            has_dev: true,
            logs: true,
            server: true,
        }
    }
}

impl BuildFlags {
    /// The build-flag defines live with this shape.
    pub fn defines(self) -> impl Iterator<Item = &'static str> {
        [
            ("DEVELOPER", self.developer_build),
            ("HAS_DEV", self.has_dev),
            ("LOGS", self.logs),
            ("SERVER", self.server),
        ]
        .into_iter()
        .filter_map(|(name, on)| on.then_some(name))
    }
}

/// How `check` groups diagnostics for human (plain/pretty) output.
///
/// Default is [`ErrorGroup::Last`]: warnings/info/hints (and unknown)
/// keep their relative order, then errors (missing severity counts as
/// error). `first` puts errors first. `off` keeps analysis order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorGroup {
    #[default]
    Last,
    First,
    Off,
}

impl ErrorGroup {
    /// Parse a config / CLI token. Aliases: end → last; start/begin → first;
    /// none/false/interleaved → off.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "last" | "end" => Some(Self::Last),
            "first" | "start" | "begin" => Some(Self::First),
            "off" | "none" | "false" | "interleaved" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Last => "last",
            Self::First => "first",
            Self::Off => "off",
        }
    }
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
    /// Preprocessor defines for a game target and signature level.
    ///
    /// Platform selectors are target-specific so the LSP only compiles the
    /// `#if` branches the real game would (GH #36): defining every platform
    /// at once made dead `#if MP4` / `#elif TURBO` branches live and flooded
    /// undefined-identifier false positives. Default target is TMNEXT
    /// (TM2020); overridable via `game_target` (config file / init options /
    /// repo-local config).
    ///
    /// The `SIG_*` names come from `signature` (GH #64) on the same
    /// reasoning — they used to be unconditionally on, which is exactly
    /// [`SignatureMode::Developer`], still the default.
    ///
    /// B028: the OS / arch / build-shape flags are selector keys too,
    /// resolved through the same ladder — never hand-listed. `os` / `arch`
    /// default to a realistic client (`windows` + `64`: the pairs are
    /// mutually exclusive in a real process, so defining both sides made
    /// both `#if` branches live and manufactured phantom duplicate
    /// declarations); the `build` flags default ALL ON (decision (a) — no
    /// default churn), see [`BuildFlags`].
    pub fn defines_for_target(
        target: &str,
        signature: SignatureMode,
        os: OsTarget,
        arch: ArchTarget,
        build: BuildFlags,
    ) -> HashSet<String> {
        // OS + arch + build-shape defines — independent of the game the
        // plugin targets and of how it is signed.
        let mut set: HashSet<String> = os
            .defines()
            .iter()
            .chain(arch.defines().iter())
            .copied()
            .chain(build.defines())
            .chain(signature.defines().iter().copied())
            .map(String::from)
            .collect();
        // Platform selector(s) for the requested game. The game defines
        // exactly one ManiaPlanet-generation family; the others stay off so
        // their `#if` branches are dead (matching the game). Evidence (GH #36,
        // tm-skids-magician): TM2020 treats `#if MP4` and `#elif TURBO` as
        // DEAD — so TMNEXT does NOT imply MP4/TURBO.
        let Some(platform) = Self::platform_family_for_target(target) else {
            // Unknown target: define the target token itself so its `#if`
            // branch is live; everything else off (conservative).
            set.insert(target.to_string());
            return set;
        };
        set.extend(platform.iter().map(|s| s.to_string()));
        set
    }

    /// Every accepted `game_target` token, for CLI validation, the config
    /// TUI's choice list and help text. Kept in lockstep with
    /// [`Self::PLATFORM_FAMILIES`] by `game_targets_match_platform_families`.
    pub const GAME_TARGETS: &'static [&'static str] = &[
        "TMNEXT",
        "MP4",
        "MP40",
        "MP41",
        "TURBO",
        "MP3",
        "UNITED_FOREVER",
        "UNITED",
        "NATIONS_FOREVER",
    ];

    /// The ManiaPlanet-generation families. Exactly one family may be live
    /// at a time (GH #36) — the members of the others must stay undefined so
    /// their `#if` branches are dead, matching the game.
    const PLATFORM_FAMILIES: &'static [&'static [&'static str]] = &[
        &["TMNEXT"],
        &["MP4", "MP40", "MP41"],
        &["TURBO"],
        &["MP3"],
        &["UNITED_FOREVER", "UNITED"],
        &["NATIONS_FOREVER"],
    ];

    /// The platform selectors a game target implies, or `None` for a target
    /// outside the known families.
    fn platform_family_for_target(target: &str) -> Option<&'static [&'static str]> {
        Self::PLATFORM_FAMILIES
            .iter()
            .copied()
            .find(|family| family.contains(&target))
    }

    /// True for any known platform selector (`TMNEXT`, `MP41`, `TURBO`, …).
    fn is_platform_selector(name: &str) -> bool {
        Self::platform_family_for_target(name).is_some()
    }

    /// Standard defines for the default target (TMNEXT / TM2020) at the
    /// default signature level (developer — every `SIG_*` on), the default
    /// OS/arch (windows / 64) and the default build flags (all on).
    pub fn default_defines() -> HashSet<String> {
        Self::defines_for_target(
            "TMNEXT",
            SignatureMode::default(),
            OsTarget::default(),
            ArchTarget::default(),
            BuildFlags::default(),
        )
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
            signature_mode: SignatureMode::default(),
            os_target: OsTarget::default(),
            arch_target: ArchTarget::default(),
            build_flags: BuildFlags::default(),
            defines: Self::default_defines(),
            define_overrides: Vec::new(),
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
            unused_callbacks: Vec::new(),
            unused: UnusedLevel::Warn,
            errors: ErrorGroup::Last,
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

        // GH #56: `unused_callbacks` does NOT follow the ladder above.
        // That ladder applies the user-global file LAST, so user-global
        // wins; this key follows the `warnings_as_errors` precedence
        // instead (workspace overrides user-global), which is what a
        // per-project exemption list has to do.
        config.unused_callbacks = Self::resolve_unused_callbacks(
            Self::user_config_path().as_deref(),
            parent.filter(|p| Some(*p) != workspace_root),
            workspace_root,
        );
        // Same workspace-over-user precedence as unused_callbacks: a
        // per-plugin silence/error pin must beat the machine default.
        config.unused = Self::resolve_unused(
            Self::user_config_path().as_deref(),
            parent.filter(|p| Some(*p) != workspace_root),
            workspace_root,
        );
        config.errors = Self::resolve_errors(
            Self::user_config_path().as_deref(),
            parent.filter(|p| Some(*p) != workspace_root),
            workspace_root,
        );

        // Layer 3: Init params (highest priority)
        if let Some(opts) = init_options {
            config.apply_init_options(opts);
        }

        // GH #36: `game_target` is now final — re-derive the base define set
        // from it so a non-default target (repo-local config / init options)
        // gets matching platform defines and dead `#if <other-platform>`
        // branches stay dead. B024: custom `defines` no longer suppress this
        // step; they are layered on top further down, after the manifest.
        // B028: the same finality holds for the os/arch/build selectors —
        // all resolved through the ladder above by now.
        config.defines = Self::defines_for_target(
            &config.game_target,
            config.signature_mode,
            config.os_target,
            config.arch_target,
            config.build_flags,
        );

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

        // Layer 5: the user's own `defines` entries, applied LAST so an
        // explicit `-NAME` can drop anything a lower layer turned on (B024).
        config.apply_define_overrides();

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

    /// Set the signature level from a config / init-option token (GH #64).
    /// An unparseable value warns and leaves the previous level in place —
    /// the default is the permissive one, and narrowing on a typo would mask
    /// code out and manufacture undefined-identifier errors.
    fn set_signature_mode(&mut self, token: &str) {
        match SignatureMode::parse(token) {
            Some(mode) => self.signature_mode = mode,
            None => eprintln!(
                "warning: unknown signature_mode `{token}` (expected {}); \
                 keeping `{}`",
                SignatureMode::CHOICES.join(", "),
                self.signature_mode.as_str()
            ),
        }
    }

    /// Set the OS target from a config / init-option token (B028). An
    /// unparseable value warns and keeps the previous one — the same
    /// warn-and-keep contract as `signature_mode`.
    fn set_os_target(&mut self, token: &str) {
        match OsTarget::parse(token) {
            Some(os) => self.os_target = os,
            None => eprintln!(
                "warning: unknown os_target `{token}` (expected {}); keeping `{}`",
                OsTarget::CHOICES.join(", "),
                self.os_target.as_str()
            ),
        }
    }

    /// Set the arch target from a config / init-option token (B028), with
    /// [`Self::set_os_target`]'s warn-and-keep contract.
    fn set_arch_target(&mut self, token: &str) {
        match ArchTarget::parse(token) {
            Some(arch) => self.arch_target = arch,
            None => eprintln!(
                "warning: unknown arch_target `{token}` (expected {}); keeping `{}`",
                ArchTarget::CHOICES.join(", "),
                self.arch_target.as_str()
            ),
        }
    }

    /// Apply the user's `defines` entries (B024) on top of the target-derived
    /// and manifest defines.
    ///
    /// * Plain entries ADD — `defines = ["DEV"]` no longer wipes `TMNEXT` and
    ///   the always-on flags, which killed every `#if TMNEXT` branch.
    /// * `-NAME` / `!NAME` REMOVES, so an unwanted define can still be turned
    ///   off (the only thing the old replace semantics were good for).
    ///   Removals are applied after additions, so a removal wins.
    /// * A hand-listed platform selector from another generation than
    ///   `game_target` replaces the target's selectors instead of stacking on
    ///   them: exactly one `#if <platform>` family stays live (GH #36).
    fn apply_define_overrides(&mut self) {
        if self.define_overrides.is_empty() {
            return;
        }
        let mut adds: Vec<String> = Vec::new();
        let mut removes: Vec<String> = Vec::new();
        for entry in &self.define_overrides {
            let entry = entry.trim();
            let (bucket, name) = match entry.strip_prefix(['-', '!']) {
                Some(rest) => (&mut removes, rest.trim()),
                None => (&mut adds, entry),
            };
            if !name.is_empty() {
                bucket.push(name.to_string());
            }
        }

        // Foreign platform selector → the listed one(s) win outright.
        let target_family = Self::platform_family_for_target(&self.game_target).unwrap_or(&[]);
        let foreign: Vec<&String> = adds
            .iter()
            .filter(|d| Self::is_platform_selector(d) && !target_family.contains(&d.as_str()))
            .collect();
        if !foreign.is_empty() {
            eprintln!(
                "warning: `defines` lists platform selector(s) {}; they replace the \
                 `game_target` ({}) selectors — set `game_target` instead to pick a game",
                foreign
                    .iter()
                    .map(|d| d.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                self.game_target
            );
            for selector in Self::PLATFORM_FAMILIES.iter().flat_map(|f| f.iter()) {
                if !adds.iter().any(|a| a == selector) {
                    self.defines.remove(*selector);
                }
            }
        }

        for add in adds {
            self.defines.insert(add);
        }
        for remove in removes {
            self.defines.remove(&remove);
        }
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

    /// `~/.config/openplanet-lsp/config.toml`, when `$HOME` is set.
    fn user_config_path() -> Option<PathBuf> {
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".config/openplanet-lsp/config.toml"))
    }

    /// GH #56: resolve `unused_callbacks` across the config ladder in
    /// ASCENDING precedence — user global, then the opened-folder parent,
    /// then the plugin root. The last file that *sets* the key wins;
    /// lists never merge, so the winning file's list is the whole list
    /// (there is deliberately no "disable the defaults" switch — dropping
    /// engine callbacks from the exemption list only manufactures false
    /// positives).
    fn resolve_unused_callbacks(
        user: Option<&Path>,
        parent: Option<&Path>,
        root: Option<&Path>,
    ) -> Vec<String> {
        let ladder = [
            user.map(Path::to_path_buf),
            parent.map(|p| p.join(".openplanet-lsp.toml")),
            root.map(|r| r.join(".openplanet-lsp.toml")),
        ];
        ladder
            .iter()
            .flatten()
            .filter_map(|p| Self::unused_callbacks_in_file(p))
            .next_back()
            .unwrap_or_default()
    }

    fn resolve_unused(
        user: Option<&Path>,
        parent: Option<&Path>,
        root: Option<&Path>,
    ) -> UnusedLevel {
        let ladder = [
            user.map(Path::to_path_buf),
            parent.map(|p| p.join(".openplanet-lsp.toml")),
            root.map(|r| r.join(".openplanet-lsp.toml")),
        ];
        ladder
            .iter()
            .flatten()
            .filter_map(|p| Self::unused_in_file(p))
            .next_back()
            .unwrap_or(UnusedLevel::Warn)
    }

    fn unused_in_file(path: &Path) -> Option<UnusedLevel> {
        let contents = std::fs::read_to_string(path).ok()?;
        let table: toml::Table = toml::from_str(&contents).ok()?;
        parse_unused_toml(table.get("unused")?)
    }

    fn resolve_errors(
        user: Option<&Path>,
        parent: Option<&Path>,
        root: Option<&Path>,
    ) -> ErrorGroup {
        let ladder = [
            user.map(Path::to_path_buf),
            parent.map(|p| p.join(".openplanet-lsp.toml")),
            root.map(|r| r.join(".openplanet-lsp.toml")),
        ];
        ladder
            .iter()
            .flatten()
            .filter_map(|p| Self::errors_in_file(p))
            .next_back()
            .unwrap_or(ErrorGroup::Last)
    }

    fn errors_in_file(path: &Path) -> Option<ErrorGroup> {
        let contents = std::fs::read_to_string(path).ok()?;
        let table: toml::Table = toml::from_str(&contents).ok()?;
        parse_errors_toml(table.get("errors")?)
    }

    /// Read `unused_callbacks` out of one TOML file, leniently: `None`
    /// when the file is missing, unparseable, or the key is absent or not
    /// an array. Non-string entries are dropped rather than rejecting the
    /// list. Parsed off a raw `toml::Table` on purpose — deserializing
    /// `ConfigFile` discards the WHOLE file on any type error, so a typo
    /// here would silently drop every other key in it.
    fn unused_callbacks_in_file(path: &Path) -> Option<Vec<String>> {
        let contents = std::fs::read_to_string(path).ok()?;
        let table: toml::Table = toml::from_str(&contents).ok()?;
        let entries = table.get("unused_callbacks")?.as_array()?;
        Some(
            entries
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
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
        if let Some(mode) = cfg.signature_mode {
            self.set_signature_mode(&mode);
        }
        if let Some(os) = cfg.os_target {
            self.set_os_target(&os);
        }
        if let Some(arch) = cfg.arch_target {
            self.set_arch_target(&arch);
        }
        if let Some(v) = cfg.developer_build {
            self.build_flags.developer_build = v;
        }
        if let Some(v) = cfg.has_dev {
            self.build_flags.has_dev = v;
        }
        if let Some(v) = cfg.logs {
            self.build_flags.logs = v;
        }
        if let Some(v) = cfg.server {
            self.build_flags.server = v;
        }
        if let Some(defines) = cfg.defines {
            self.define_overrides.extend(defines);
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
        if let Some(mode) = opts.get("signature_mode").and_then(|v| v.as_str()) {
            self.set_signature_mode(mode);
        }
        if let Some(os) = opts.get("os_target").and_then(|v| v.as_str()) {
            self.set_os_target(os);
        }
        if let Some(arch) = opts.get("arch_target").and_then(|v| v.as_str()) {
            self.set_arch_target(arch);
        }
        if let Some(v) = opts.get("developer_build").and_then(|v| v.as_bool()) {
            self.build_flags.developer_build = v;
        }
        if let Some(v) = opts.get("has_dev").and_then(|v| v.as_bool()) {
            self.build_flags.has_dev = v;
        }
        if let Some(v) = opts.get("logs").and_then(|v| v.as_bool()) {
            self.build_flags.logs = v;
        }
        if let Some(v) = opts.get("server").and_then(|v| v.as_bool()) {
            self.build_flags.server = v;
        }
        if let Some(defines) = opts.get("defines").and_then(|v| v.as_array()) {
            self.define_overrides
                .extend(defines.iter().filter_map(|v| v.as_str().map(String::from)));
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
        if let Some(level) = opts.get("unused").and_then(parse_unused_json) {
            self.unused = level;
        }
        if let Some(group) = opts.get("errors").and_then(parse_errors_json) {
            self.errors = group;
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

fn parse_unused_toml(value: &toml::Value) -> Option<UnusedLevel> {
    match value {
        toml::Value::String(s) => UnusedLevel::parse(s),
        toml::Value::Boolean(false) => Some(UnusedLevel::Ignore),
        toml::Value::Boolean(true) => Some(UnusedLevel::Warn),
        _ => None,
    }
}

fn parse_unused_json(value: &serde_json::Value) -> Option<UnusedLevel> {
    if let Some(s) = value.as_str() {
        return UnusedLevel::parse(s);
    }
    match value.as_bool() {
        Some(false) => Some(UnusedLevel::Ignore),
        Some(true) => Some(UnusedLevel::Warn),
        None => None,
    }
}

fn parse_errors_toml(value: &toml::Value) -> Option<ErrorGroup> {
    match value {
        toml::Value::String(s) => ErrorGroup::parse(s),
        toml::Value::Boolean(false) => Some(ErrorGroup::Off),
        _ => None,
    }
}

fn parse_errors_json(value: &serde_json::Value) -> Option<ErrorGroup> {
    if let Some(s) = value.as_str() {
        return ErrorGroup::parse(s);
    }
    match value.as_bool() {
        Some(false) => Some(ErrorGroup::Off),
        _ => None,
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
    fn unused_level_parse_accepts_canonical_and_alias_variants() {
        for (input, want) in [
            ("ignore", UnusedLevel::Ignore),
            ("IGNORE", UnusedLevel::Ignore),
            ("off", UnusedLevel::Ignore),
            ("false", UnusedLevel::Ignore),
            ("none", UnusedLevel::Ignore),
            ("disable", UnusedLevel::Ignore),
            ("disabled", UnusedLevel::Ignore),
            ("warn", UnusedLevel::Warn),
            ("warning", UnusedLevel::Warn),
            ("on", UnusedLevel::Warn),
            ("true", UnusedLevel::Warn),
            ("error", UnusedLevel::Error),
            ("err", UnusedLevel::Error),
            ("errors", UnusedLevel::Error),
        ] {
            assert_eq!(UnusedLevel::parse(input), Some(want), "parse {input:?}");
        }
        assert!(UnusedLevel::parse("banana").is_none());
        assert!(UnusedLevel::parse("").is_none());
    }

    #[test]
    fn unused_defaults_to_warn() {
        assert_eq!(LspConfig::default().unused, UnusedLevel::Warn);
        assert_eq!(
            LspConfig::resolve_unused(None, None, None),
            UnusedLevel::Warn
        );
    }

    #[test]
    fn unused_workspace_overrides_user_and_accepts_bool() {
        let tmp = std::env::temp_dir().join(format!("oplsp-unused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let user = tmp.join("user.toml");
        std::fs::write(&user, "unused = \"error\"\n").unwrap();

        let ws_ignore = tmp.join("ws-ignore");
        std::fs::create_dir_all(&ws_ignore).unwrap();
        std::fs::write(
            ws_ignore.join(".openplanet-lsp.toml"),
            "unused = \"ignore\"\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::resolve_unused(Some(&user), None, Some(&ws_ignore)),
            UnusedLevel::Ignore,
            "workspace ignore must beat user error"
        );

        let ws_bool = tmp.join("ws-bool");
        std::fs::create_dir_all(&ws_bool).unwrap();
        std::fs::write(ws_bool.join(".openplanet-lsp.toml"), "unused = false\n").unwrap();
        assert_eq!(
            LspConfig::resolve_unused(Some(&user), None, Some(&ws_bool)),
            UnusedLevel::Ignore,
            "unused = false is ignore"
        );

        assert_eq!(
            LspConfig::resolve_unused(Some(&user), None, None),
            UnusedLevel::Error,
            "user-global unused = error when workspace does not set it"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unused_init_options_override_config_file() {
        let tmp = std::env::temp_dir().join(format!("oplsp-unused-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".openplanet-lsp.toml"), "unused = \"ignore\"\n").unwrap();
        let opts = serde_json::json!({ "unused": "error" });
        let cfg = LspConfig::load(Some(&tmp), Some(&opts));
        assert_eq!(cfg.unused, UnusedLevel::Error);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn error_group_parse_accepts_canonical_and_alias_variants() {
        for (input, want) in [
            ("last", ErrorGroup::Last),
            ("LAST", ErrorGroup::Last),
            ("end", ErrorGroup::Last),
            ("first", ErrorGroup::First),
            ("start", ErrorGroup::First),
            ("begin", ErrorGroup::First),
            ("off", ErrorGroup::Off),
            ("none", ErrorGroup::Off),
            ("false", ErrorGroup::Off),
            ("interleaved", ErrorGroup::Off),
        ] {
            assert_eq!(ErrorGroup::parse(input), Some(want), "parse {input:?}");
        }
        assert!(ErrorGroup::parse("banana").is_none());
        assert!(ErrorGroup::parse("").is_none());
    }

    #[test]
    fn errors_defaults_to_last() {
        assert_eq!(LspConfig::default().errors, ErrorGroup::Last);
        assert_eq!(
            LspConfig::resolve_errors(None, None, None),
            ErrorGroup::Last
        );
    }

    #[test]
    fn errors_workspace_overrides_user_and_accepts_aliases() {
        let tmp = std::env::temp_dir().join(format!("oplsp-errors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let user = tmp.join("user.toml");
        std::fs::write(&user, "errors = \"first\"\n").unwrap();

        let ws_last = tmp.join("ws-last");
        std::fs::create_dir_all(&ws_last).unwrap();
        std::fs::write(ws_last.join(".openplanet-lsp.toml"), "errors = \"last\"\n").unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_last)),
            ErrorGroup::Last,
            "workspace last must beat user first"
        );

        let ws_end = tmp.join("ws-end");
        std::fs::create_dir_all(&ws_end).unwrap();
        std::fs::write(ws_end.join(".openplanet-lsp.toml"), "errors = \"end\"\n").unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_end)),
            ErrorGroup::Last,
            "end is last"
        );

        let ws_start = tmp.join("ws-start");
        std::fs::create_dir_all(&ws_start).unwrap();
        std::fs::write(
            ws_start.join(".openplanet-lsp.toml"),
            "errors = \"start\"\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_start)),
            ErrorGroup::First,
            "start is first"
        );

        let ws_begin = tmp.join("ws-begin");
        std::fs::create_dir_all(&ws_begin).unwrap();
        std::fs::write(
            ws_begin.join(".openplanet-lsp.toml"),
            "errors = \"begin\"\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_begin)),
            ErrorGroup::First,
            "begin is first"
        );

        let ws_off = tmp.join("ws-off");
        std::fs::create_dir_all(&ws_off).unwrap();
        std::fs::write(ws_off.join(".openplanet-lsp.toml"), "errors = \"off\"\n").unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_off)),
            ErrorGroup::Off
        );

        let ws_bool = tmp.join("ws-bool");
        std::fs::create_dir_all(&ws_bool).unwrap();
        std::fs::write(ws_bool.join(".openplanet-lsp.toml"), "errors = false\n").unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_bool)),
            ErrorGroup::Off,
            "errors = false is off"
        );

        let ws_none = tmp.join("ws-none");
        std::fs::create_dir_all(&ws_none).unwrap();
        std::fs::write(ws_none.join(".openplanet-lsp.toml"), "errors = \"none\"\n").unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_none)),
            ErrorGroup::Off
        );

        let ws_interleaved = tmp.join("ws-interleaved");
        std::fs::create_dir_all(&ws_interleaved).unwrap();
        std::fs::write(
            ws_interleaved.join(".openplanet-lsp.toml"),
            "errors = \"interleaved\"\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_interleaved)),
            ErrorGroup::Off
        );

        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, None),
            ErrorGroup::First,
            "user-global errors = first when workspace does not set it"
        );

        let ws_bad = tmp.join("ws-bad");
        std::fs::create_dir_all(&ws_bad).unwrap();
        std::fs::write(ws_bad.join(".openplanet-lsp.toml"), "errors = \"banana\"\n").unwrap();
        assert_eq!(
            LspConfig::resolve_errors(Some(&user), None, Some(&ws_bad)),
            ErrorGroup::First,
            "invalid workspace value is ignored; user first remains"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn errors_init_options_override_config_file() {
        let tmp = std::env::temp_dir().join(format!("oplsp-errors-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".openplanet-lsp.toml"), "errors = \"off\"\n").unwrap();
        let opts = serde_json::json!({ "errors": "first" });
        let cfg = LspConfig::load(Some(&tmp), Some(&opts));
        assert_eq!(cfg.errors, ErrorGroup::First);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_default_defines() {
        // Default target is TMNEXT (TM2020): TMNEXT on, other ManiaPlanet
        // generations OFF (GH #36 — dead `#if` branches). B028: default
        // profile is a windows/64 client with the build flags ON (decision
        // (a)) — so the OTHER os/arch side is now off, matching the game.
        let defs = LspConfig::default_defines();
        assert!(defs.contains("TMNEXT"));
        assert!(defs.contains("SIG_DEVELOPER"));
        assert!(defs.contains("WINDOWS"));
        assert!(defs.contains("MANIA64"));
        for flag in ["SERVER", "LOGS", "HAS_DEV", "DEVELOPER"] {
            assert!(defs.contains(flag), "build flag {flag} on by default");
        }
        assert!(!defs.contains("MANIA32"), "arch pair is exclusive");
        assert!(!defs.contains("LINUX"), "os pair is exclusive");
        assert!(!defs.contains("WINDOWS_WINE"), "wine flag needs --os wine");
        assert!(!defs.contains("TURBO"));
        assert!(!defs.contains("MP4"));
        assert!(!defs.contains("MP3"));
        assert!(!defs.contains("UNITED"));
    }

    #[test]
    fn test_defines_for_target_selects_platform() {
        let turbo = LspConfig::defines_for_target(
            "TURBO",
            SignatureMode::default(),
            OsTarget::default(),
            ArchTarget::default(),
            BuildFlags::default(),
        );
        assert!(turbo.contains("TURBO"));
        assert!(turbo.contains("WINDOWS")); // os/arch/build defaults still present
        assert!(!turbo.contains("TMNEXT"));
        assert!(!turbo.contains("MP4"));

        let mp4 = LspConfig::defines_for_target(
            "MP4",
            SignatureMode::default(),
            OsTarget::default(),
            ArchTarget::default(),
            BuildFlags::default(),
        );
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
    fn test_init_options_override() {
        // B024: explicit defines are additive, so the MP4 target keeps its
        // whole platform family and the always-on flags; listing `MP4` and
        // `WINDOWS` by hand is a no-op on top of that.
        let opts = serde_json::json!({
            "game_target": "MP4",
            "defines": ["MP4", "WINDOWS"]
        });
        let config = LspConfig::load(None, Some(&opts));
        assert_eq!(config.game_target, "MP4");
        assert!(config.defines.contains("MP4"));
        assert!(config.defines.contains("MP40"));
        assert!(config.defines.contains("WINDOWS"));
        assert!(!config.defines.contains("TMNEXT"));
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

    /// GH #56: `unused_callbacks` precedence. Tested through the pure
    /// resolver rather than `load_with_parent` so the "user global" leg
    /// is a temp file — mutating $HOME in-process races the tests above.
    #[test]
    fn unused_callbacks_workspace_overrides_user_global() {
        let tmp = std::env::temp_dir().join(format!("oplsp-uc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let user = tmp.join("user.toml");
        let ws = tmp.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(&user, "unused_callbacks = [\"FromUser\"]\n").unwrap();

        // User global alone.
        assert_eq!(
            LspConfig::resolve_unused_callbacks(Some(&user), None, None),
            vec!["FromUser".to_string()]
        );

        // Workspace wins outright — lists do not merge.
        std::fs::write(
            ws.join(".openplanet-lsp.toml"),
            "unused_callbacks = [\"FromWorkspace\"]\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::resolve_unused_callbacks(Some(&user), None, Some(&ws)),
            vec!["FromWorkspace".to_string()]
        );

        // An explicitly EMPTY workspace list still wins.
        std::fs::write(ws.join(".openplanet-lsp.toml"), "unused_callbacks = []\n").unwrap();
        assert!(LspConfig::resolve_unused_callbacks(Some(&user), None, Some(&ws)).is_empty());

        // Workspace file that does not mention the key falls through.
        std::fs::write(ws.join(".openplanet-lsp.toml"), "game_target = \"MP4\"\n").unwrap();
        assert_eq!(
            LspConfig::resolve_unused_callbacks(Some(&user), None, Some(&ws)),
            vec!["FromUser".to_string()]
        );

        // Plugin root beats the opened-folder parent.
        let parent = tmp.join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(
            parent.join(".openplanet-lsp.toml"),
            "unused_callbacks = [\"FromParent\"]\n",
        )
        .unwrap();
        std::fs::write(
            ws.join(".openplanet-lsp.toml"),
            "unused_callbacks = [\"FromRoot\"]\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::resolve_unused_callbacks(Some(&user), Some(&parent), Some(&ws)),
            vec!["FromRoot".to_string()]
        );
        assert_eq!(
            LspConfig::resolve_unused_callbacks(Some(&user), Some(&parent), None),
            vec!["FromParent".to_string()]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Malformed values are ignored, never panic, and never poison the
    /// other keys in the same file.
    #[test]
    fn unused_callbacks_malformed_values_are_ignored() {
        let tmp = std::env::temp_dir().join(format!("oplsp-ucbad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let cases: &[(&str, &str, Vec<String>)] = &[
            ("not-an-array", "unused_callbacks = \"OnThing\"\n", vec![]),
            ("a-number", "unused_callbacks = 7\n", vec![]),
            ("a-table", "unused_callbacks = { a = 1 }\n", vec![]),
            (
                "mixed-entries",
                "unused_callbacks = [\"Keep\", 7, true, \"Also\"]\n",
                vec!["Keep".to_string(), "Also".to_string()],
            ),
            ("absent", "game_target = \"MP4\"\n", vec![]),
            ("unparseable", "this is not toml =\n", vec![]),
            ("empty-list", "unused_callbacks = []\n", vec![]),
        ];
        for (name, body, want) in cases {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".openplanet-lsp.toml"), body).unwrap();
            assert_eq!(
                LspConfig::resolve_unused_callbacks(None, None, Some(&dir)),
                *want,
                "case {name}"
            );
        }

        // A malformed `unused_callbacks` must not drop sibling keys.
        let dir = tmp.join("sibling-keys-survive");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".openplanet-lsp.toml"),
            "game_target = \"MP4\"\nunused_callbacks = 7\n",
        )
        .unwrap();
        let mut cfg = LspConfig::default();
        cfg.load_config_file(&dir);
        assert_eq!(cfg.game_target, "MP4");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- B024: custom `defines` are ADDITIVE over the target-derived set ---

    /// Workspace `.openplanet-lsp.toml` with `defines = ["DEV"]` must ADD DEV
    /// to the target-derived set, not replace it (B024: replacing wiped
    /// TMNEXT/WINDOWS and killed every `#if TMNEXT` branch).
    #[test]
    fn config_file_defines_add_to_target_defines() {
        let tmp = std::env::temp_dir().join(format!("oplsp-defines-add-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".openplanet-lsp.toml"), "defines = [\"DEV\"]\n").unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert!(cfg.defines.contains("DEV"), "custom define missing");
        assert!(
            cfg.defines.contains("TMNEXT"),
            "target platform define wiped by custom defines: {:?}",
            cfg.defines
        );
        assert!(cfg.defines.contains("WINDOWS"), "always-on define wiped");
        assert!(
            cfg.defines.contains("SIG_DEVELOPER"),
            "always-on define wiped"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Same rule for `initializationOptions` (the editor-side layer).
    #[test]
    fn init_option_defines_add_to_target_defines() {
        let opts = serde_json::json!({ "defines": ["DEV", "DEBUG_BROWSER"] });
        let cfg = LspConfig::load(None, Some(&opts));
        assert!(cfg.defines.contains("DEV"));
        assert!(cfg.defines.contains("DEBUG_BROWSER"));
        assert!(cfg.defines.contains("TMNEXT"), "{:?}", cfg.defines);
        assert!(cfg.defines.contains("MANIA64"));
    }

    /// A non-default `game_target` keeps deriving its own platform defines
    /// even when custom defines are listed (GH #36 stays satisfied).
    #[test]
    fn custom_defines_keep_the_selected_game_target_platform() {
        let opts = serde_json::json!({ "game_target": "TURBO", "defines": ["CUSTOM"] });
        let cfg = LspConfig::load(None, Some(&opts));
        assert!(cfg.defines.contains("CUSTOM"));
        assert!(cfg.defines.contains("TURBO"), "{:?}", cfg.defines);
        assert!(
            !cfg.defines.contains("TMNEXT"),
            "other platform must stay dead"
        );
        assert!(!cfg.defines.contains("MP4"));
    }

    /// Opt-out syntax: `-NAME` / `!NAME` removes a define the target (or a
    /// lower layer) turned on. Removals win over additions in the same list.
    #[test]
    fn define_overrides_can_remove_with_dash_or_bang() {
        let tmp = std::env::temp_dir().join(format!("oplsp-defines-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "defines = [\"DEV\", \"-SIG_DEVELOPER\", \"!LOGS\", \"DEAD\", \"-DEAD\"]\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert!(cfg.defines.contains("DEV"));
        assert!(cfg.defines.contains("TMNEXT"));
        assert!(!cfg.defines.contains("SIG_DEVELOPER"), "-NAME must remove");
        assert!(!cfg.defines.contains("LOGS"), "!NAME must remove");
        assert!(
            !cfg.defines.contains("DEAD"),
            "a removal must beat an addition of the same name"
        );
        assert!(cfg.defines.contains("SIG_REGULAR"), "siblings untouched");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A hand-listed platform selector still leaves exactly ONE platform
    /// family live: it replaces the target's selectors rather than stacking
    /// on top of them (GH #36 invariant, preserved from the old replace
    /// semantics).
    #[test]
    fn explicit_platform_selector_replaces_target_platform() {
        for (listed, dead) in [("MP4", "TMNEXT"), ("TURBO", "TMNEXT"), ("UNITED", "TMNEXT")] {
            let opts = serde_json::json!({ "defines": [listed, "DEV"] });
            let cfg = LspConfig::load(None, Some(&opts));
            assert!(cfg.defines.contains(listed), "listed selector missing");
            assert!(
                !cfg.defines.contains(dead),
                "target selector {dead} must go dead when {listed} is listed: {:?}",
                cfg.defines
            );
            assert!(cfg.defines.contains("DEV"));
            assert!(cfg.defines.contains("WINDOWS"), "always-on flags survive");
        }
    }

    /// Listing a selector the target already implies is a plain no-op: the
    /// rest of that family stays defined.
    #[test]
    fn same_family_platform_selector_keeps_the_family() {
        let opts = serde_json::json!({ "game_target": "MP4", "defines": ["MP4", "DEV"] });
        let cfg = LspConfig::load(None, Some(&opts));
        assert!(cfg.defines.contains("MP4"));
        assert!(cfg.defines.contains("MP40"), "{:?}", cfg.defines);
        assert!(cfg.defines.contains("MP41"));
        assert!(cfg.defines.contains("DEV"));
        assert!(!cfg.defines.contains("TMNEXT"));
    }

    /// Multi-root: parent and per-root `defines` accumulate instead of the
    /// per-root file discarding the parent's list, and a per-root removal
    /// can still drop a parent-added define.
    #[test]
    fn parent_and_root_defines_accumulate() {
        let tmp = std::env::temp_dir().join(format!("oplsp-defines-multi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("PluginA");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "defines = [\"PARENT_X\", \"PARENT_DROPPED\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".openplanet-lsp.toml"),
            "defines = [\"ROOT_Y\", \"-PARENT_DROPPED\"]\n",
        )
        .unwrap();
        let cfg = LspConfig::load_with_parent(Some(&root), Some(&tmp), None);
        assert!(cfg.defines.contains("PARENT_X"), "{:?}", cfg.defines);
        assert!(cfg.defines.contains("ROOT_Y"));
        assert!(!cfg.defines.contains("PARENT_DROPPED"));
        assert!(cfg.defines.contains("TMNEXT"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Manifest `[script].defines` stay additive, and a config-file removal
    /// gets the last word over them (user overrides apply after the manifest).
    #[test]
    fn config_defines_apply_after_manifest_defines() {
        let tmp =
            std::env::temp_dir().join(format!("oplsp-defines-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("info.toml"),
            "[meta]\nname = \"T\"\nauthor = \"A\"\n\n[script]\ndefines = [\"MANIFEST_ON\", \"MANIFEST_OFF\"]\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "defines = [\"DEV\", \"-MANIFEST_OFF\"]\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert!(cfg.defines.contains("MANIFEST_ON"), "{:?}", cfg.defines);
        assert!(
            !cfg.defines.contains("MANIFEST_OFF"),
            "removal must beat manifest"
        );
        assert!(cfg.defines.contains("DEV"));
        assert!(cfg.defines.contains("TMNEXT"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- GH #64: signature-mode selector (which SIG_* defines are live) ---

    /// Openplanet defines a signature define "when the current signature
    /// level is equal or below their respective level" — the levels are
    /// CUMULATIVE, not exclusive (openplanet.dev/docs/reference/preprocessor).
    #[test]
    fn signature_mode_defines_are_cumulative() {
        let cases = [
            (SignatureMode::Official, vec!["SIG_OFFICIAL"]),
            (SignatureMode::Regular, vec!["SIG_OFFICIAL", "SIG_REGULAR"]),
            (
                SignatureMode::School,
                vec!["SIG_OFFICIAL", "SIG_REGULAR", "SIG_SCHOOL"],
            ),
            (
                SignatureMode::Developer,
                vec!["SIG_OFFICIAL", "SIG_REGULAR", "SIG_SCHOOL", "SIG_DEVELOPER"],
            ),
        ];
        for (mode, want) in cases {
            let got = mode.defines();
            assert_eq!(got, want.as_slice(), "{} defines", mode.as_str());
        }
    }

    /// The default set is unchanged by the feature: all four SIG_* on, which
    /// is exactly Developer. Nobody's analysis moves unless they opt in.
    #[test]
    fn default_signature_mode_is_developer_and_keeps_every_sig_define() {
        assert_eq!(
            LspConfig::default().signature_mode,
            SignatureMode::Developer
        );
        let defs = LspConfig::default_defines();
        for d in ["SIG_OFFICIAL", "SIG_REGULAR", "SIG_SCHOOL", "SIG_DEVELOPER"] {
            assert!(defs.contains(d), "{d} missing from the default set");
        }
    }

    #[test]
    fn signature_mode_parses_names_aliases_and_define_tokens() {
        for (input, want) in [
            ("developer", SignatureMode::Developer),
            ("Developer", SignatureMode::Developer),
            ("  DEV  ", SignatureMode::Developer),
            ("SIG_DEVELOPER", SignatureMode::Developer),
            ("school", SignatureMode::School),
            ("sig_school", SignatureMode::School),
            ("regular", SignatureMode::Regular),
            ("reg", SignatureMode::Regular),
            ("official", SignatureMode::Official),
            ("sig_official", SignatureMode::Official),
        ] {
            assert_eq!(SignatureMode::parse(input), Some(want), "parse {input:?}");
        }
        assert!(SignatureMode::parse("banana").is_none());
        assert!(SignatureMode::parse("").is_none());
        // `off` belongs to `errors`/`unused`; it must not silently mean
        // "official" here.
        assert!(SignatureMode::parse("off").is_none());
    }

    /// A workspace `signature_mode` narrows the SIG_* set without touching
    /// the platform or always-on defines.
    #[test]
    fn config_file_signature_mode_narrows_sig_defines() {
        let tmp = std::env::temp_dir().join(format!("oplsp-sigmode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "signature_mode = \"regular\"\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(cfg.signature_mode, SignatureMode::Regular);
        assert!(cfg.defines.contains("SIG_OFFICIAL"));
        assert!(cfg.defines.contains("SIG_REGULAR"));
        assert!(!cfg.defines.contains("SIG_SCHOOL"), "{:?}", cfg.defines);
        assert!(!cfg.defines.contains("SIG_DEVELOPER"));
        assert!(cfg.defines.contains("TMNEXT"), "platform defines untouched");
        assert!(cfg.defines.contains("WINDOWS"), "always-on flags untouched");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn init_options_signature_mode_wins_over_config_file() {
        let tmp = std::env::temp_dir().join(format!("oplsp-sigmode-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "signature_mode = \"official\"\n",
        )
        .unwrap();
        let opts = serde_json::json!({ "signature_mode": "school" });
        let cfg = LspConfig::load(Some(&tmp), Some(&opts));
        assert_eq!(cfg.signature_mode, SignatureMode::School);
        assert!(cfg.defines.contains("SIG_SCHOOL"));
        assert!(!cfg.defines.contains("SIG_DEVELOPER"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// An unknown value falls back PERMISSIVE (developer): masking code out
    /// is what manufactures false "undefined identifier" errors, so the safe
    /// direction is more defines, not fewer. Sibling keys keep working.
    #[test]
    fn invalid_signature_mode_keeps_developer_and_sibling_keys() {
        let tmp = std::env::temp_dir().join(format!("oplsp-sigmode-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "signature_mode = \"banana\"\ngame_target = \"TURBO\"\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(cfg.signature_mode, SignatureMode::Developer);
        assert!(cfg.defines.contains("SIG_DEVELOPER"));
        assert_eq!(cfg.game_target, "TURBO", "sibling key must still apply");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `defines` overrides still get the last word over the derived SIG_ set,
    /// in both directions (B024 ordering).
    #[test]
    fn define_overrides_beat_the_signature_mode_set() {
        let tmp = std::env::temp_dir().join(format!("oplsp-sigmode-ovr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let add = tmp.join("add-back");
        std::fs::create_dir_all(&add).unwrap();
        std::fs::write(
            add.join(".openplanet-lsp.toml"),
            "signature_mode = \"official\"\ndefines = [\"SIG_DEVELOPER\"]\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&add), None);
        assert!(
            cfg.defines.contains("SIG_DEVELOPER"),
            "explicit add ignored"
        );
        assert!(!cfg.defines.contains("SIG_SCHOOL"), "level still official");

        let drop = tmp.join("drop-one");
        std::fs::create_dir_all(&drop).unwrap();
        std::fs::write(
            drop.join(".openplanet-lsp.toml"),
            "signature_mode = \"developer\"\ndefines = [\"-SIG_DEVELOPER\"]\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&drop), None);
        assert!(!cfg.defines.contains("SIG_DEVELOPER"), "removal ignored");
        assert!(cfg.defines.contains("SIG_SCHOOL"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- B028: OS / arch / build-flag selectors ---

    /// Parse accepts canonical names case-insensitively and rejects junk,
    /// empty, and the ambiguous `off` token (that belongs to
    /// `errors`/`unused`, like in `SignatureMode::parse`).
    #[test]
    fn os_target_parses_canonical_and_rejects_unknown() {
        for (input, want) in [
            ("windows", OsTarget::Windows),
            ("Windows", OsTarget::Windows),
            (" WINDOWS ", OsTarget::Windows),
            ("wine", OsTarget::Wine),
            ("WINE", OsTarget::Wine),
            ("linux", OsTarget::Linux),
            ("Linux", OsTarget::Linux),
        ] {
            assert_eq!(OsTarget::parse(input), Some(want), "parse {input:?}");
        }
        for bad in ["banana", "", "off", "win", "mac"] {
            assert!(OsTarget::parse(bad).is_none(), "parse {bad:?} must fail");
        }
    }

    /// Same contract for the arch pair: canonical tokens only.
    #[test]
    fn arch_target_parses_canonical_and_rejects_unknown() {
        for (input, want) in [("64", ArchTarget::Arch64), (" 32 ", ArchTarget::Arch32)] {
            assert_eq!(ArchTarget::parse(input), Some(want), "parse {input:?}");
        }
        for bad in ["banana", "", "x64", "x86", "6", "3"] {
            assert!(ArchTarget::parse(bad).is_none(), "parse {bad:?} must fail");
        }
    }

    /// Exactly one OS family is live per target — never two, and `wine`
    /// carries BOTH `WINDOWS` and `WINDOWS_WINE` (probe 2026-08-29: the
    /// game under Wine defines both).
    #[test]
    fn os_target_defines_are_exclusive_per_target() {
        for (os, on, off) in [
            (
                OsTarget::Windows,
                &["WINDOWS"][..],
                &["WINDOWS_WINE", "LINUX"][..],
            ),
            (
                OsTarget::Wine,
                &["WINDOWS", "WINDOWS_WINE"][..],
                &["LINUX"][..],
            ),
            (
                OsTarget::Linux,
                &["LINUX"][..],
                &["WINDOWS", "WINDOWS_WINE"][..],
            ),
        ] {
            let defs = LspConfig::defines_for_target(
                "TMNEXT",
                SignatureMode::default(),
                os,
                ArchTarget::default(),
                BuildFlags::default(),
            );
            for &d in on {
                assert!(defs.contains(d), "{:?} must define {d}", os);
            }
            for &d in off {
                assert!(!defs.contains(d), "{:?} must NOT define {d}", os);
            }
        }
    }

    /// Exactly one arch is live per target, in BOTH directions (the
    /// all-on behavior was the reported bug).
    #[test]
    fn arch_target_defines_are_exclusive() {
        for (arch, on, off) in [
            (ArchTarget::Arch64, "MANIA64", "MANIA32"),
            (ArchTarget::Arch32, "MANIA32", "MANIA64"),
        ] {
            let defs = LspConfig::defines_for_target(
                "TMNEXT",
                SignatureMode::default(),
                OsTarget::default(),
                arch,
                BuildFlags::default(),
            );
            assert!(defs.contains(on), "{:?} must define {on}", arch);
            assert!(!defs.contains(off), "{:?} must NOT define {off}", arch);
        }
    }

    /// Decision (a): the four build flags default ON (the old always-on
    /// set, so nobody's analysis moves), and narrowing one drops exactly
    /// that define — its siblings stay live.
    #[test]
    fn build_flags_default_on_and_narrow_individually() {
        assert_eq!(
            BuildFlags::default(),
            BuildFlags {
                developer_build: true,
                has_dev: true,
                logs: true,
                server: true,
            }
        );
        for (name, narrowed) in [
            (
                "DEVELOPER",
                BuildFlags {
                    developer_build: false,
                    ..BuildFlags::default()
                },
            ),
            (
                "HAS_DEV",
                BuildFlags {
                    has_dev: false,
                    ..BuildFlags::default()
                },
            ),
            (
                "LOGS",
                BuildFlags {
                    logs: false,
                    ..BuildFlags::default()
                },
            ),
            (
                "SERVER",
                BuildFlags {
                    server: false,
                    ..BuildFlags::default()
                },
            ),
        ] {
            let defs = LspConfig::defines_for_target(
                "TMNEXT",
                SignatureMode::default(),
                OsTarget::default(),
                ArchTarget::default(),
                narrowed,
            );
            assert!(!defs.contains(name), "narrowing must drop {name}");
            for other in ["DEVELOPER", "HAS_DEV", "LOGS", "SERVER"] {
                if other != name {
                    assert!(defs.contains(other), "narrowing {name} must keep {other}");
                }
            }
        }
    }

    /// The ladder derives the base set from the os/arch keys like it does
    /// from `game_target`.
    #[test]
    fn config_file_os_and_arch_keys_derive_defines() {
        let tmp = std::env::temp_dir().join(format!("oplsp-osarch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "os_target = \"wine\"\narch_target = \"32\"\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(cfg.os_target, OsTarget::Wine);
        assert_eq!(cfg.arch_target, ArchTarget::Arch32);
        assert!(cfg.defines.contains("WINDOWS"), "{:?}", cfg.defines);
        assert!(cfg.defines.contains("WINDOWS_WINE"));
        assert!(cfg.defines.contains("MANIA32"));
        assert!(!cfg.defines.contains("LINUX"), "{:?}", cfg.defines);
        assert!(!cfg.defines.contains("MANIA64"));
        assert!(cfg.defines.contains("SERVER"), "build flags stay on");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// An unknown value warns and keeps the default (permissive direction:
    /// manufacturing a dead live branch is worse than a stale branch), and
    /// sibling keys still apply — same contract as `signature_mode`.
    #[test]
    fn invalid_os_or_arch_config_value_keeps_default_and_siblings() {
        let tmp = std::env::temp_dir().join(format!("oplsp-osarch-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "os_target = \"banana\"\narch_target = \"banana\"\ngame_target = \"TURBO\"\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert_eq!(cfg.os_target, OsTarget::Windows);
        assert_eq!(cfg.arch_target, ArchTarget::Arch64);
        assert_eq!(cfg.game_target, "TURBO", "sibling key must still apply");
        assert!(cfg.defines.contains("TURBO"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Init options sit above the config-file layer in the ladder.
    #[test]
    fn init_options_os_and_arch_win_over_config_file() {
        let tmp = std::env::temp_dir().join(format!("oplsp-osarch-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "os_target = \"wine\"\narch_target = \"32\"\n",
        )
        .unwrap();
        let opts = serde_json::json!({ "os_target": "linux", "arch_target": "64" });
        let cfg = LspConfig::load(Some(&tmp), Some(&opts));
        assert_eq!(cfg.os_target, OsTarget::Linux);
        assert_eq!(cfg.arch_target, ArchTarget::Arch64);
        assert!(cfg.defines.contains("LINUX"));
        assert!(!cfg.defines.contains("WINDOWS_WINE"));
        assert!(cfg.defines.contains("MANIA64"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Init options also carry the build-flag booleans, above the
    /// config-file layer — both directions (B028 review pin: this ladder
    /// rung had no test for the four booleans).
    #[test]
    fn init_options_build_flags_win_over_config_file() {
        let tmp = std::env::temp_dir().join(format!("oplsp-bflags-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "server = false\nlogs = true\n",
        )
        .unwrap();
        let opts = serde_json::json!({ "server": true, "logs": false });
        let cfg = LspConfig::load(Some(&tmp), Some(&opts));
        assert!(cfg.build_flags.server, "init option beats config false");
        assert!(cfg.defines.contains("SERVER"));
        assert!(!cfg.build_flags.logs, "init option beats config true");
        assert!(!cfg.defines.contains("LOGS"));
        assert!(cfg.build_flags.has_dev, "untouched flag stays on");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The build-flag config keys narrow the derived set without touching
    /// the os/arch pairs.
    #[test]
    fn build_flag_config_keys_narrow_defines() {
        let tmp = std::env::temp_dir().join(format!("oplsp-bflags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "server = false\nlogs = false\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&tmp), None);
        assert!(!cfg.build_flags.server);
        assert!(!cfg.build_flags.logs);
        assert!(!cfg.defines.contains("SERVER"), "{:?}", cfg.defines);
        assert!(!cfg.defines.contains("LOGS"));
        assert!(cfg.defines.contains("DEVELOPER"), "untouched flag stays on");
        assert!(cfg.defines.contains("HAS_DEV"));
        assert!(cfg.defines.contains("WINDOWS"), "os/arch pairs untouched");
        assert!(cfg.defines.contains("MANIA64"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// B024 ordering is preserved: `defines` gets the last word over the
    /// derived selector set, in both directions.
    #[test]
    fn define_overrides_last_word_over_os_arch_selectors() {
        let tmp = std::env::temp_dir().join(format!("oplsp-osarch-ovr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let drop = tmp.join("drop-wine");
        std::fs::create_dir_all(&drop).unwrap();
        std::fs::write(
            drop.join(".openplanet-lsp.toml"),
            "os_target = \"wine\"\ndefines = [\"-WINDOWS_WINE\"]\n",
        )
        .unwrap();
        let cfg = LspConfig::load(Some(&drop), None);
        assert!(!cfg.defines.contains("WINDOWS_WINE"), "removal ignored");
        assert!(cfg.defines.contains("WINDOWS"), "plain WINDOWS survives");

        let add = tmp.join("add-linux");
        std::fs::create_dir_all(&add).unwrap();
        std::fs::write(add.join(".openplanet-lsp.toml"), "defines = [\"LINUX\"]\n").unwrap();
        let cfg = LspConfig::load(Some(&add), None);
        assert!(cfg.defines.contains("LINUX"), "explicit add ignored");
        assert!(cfg.defines.contains("WINDOWS"), "default os stays live");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The CLI/TUI target list and the platform-family table must not drift
    /// apart — a target missing from one silently loses validation or its
    /// family.
    #[test]
    fn game_targets_match_platform_families() {
        let mut from_families: Vec<&str> = LspConfig::PLATFORM_FAMILIES
            .iter()
            .flat_map(|f| f.iter().copied())
            .collect();
        from_families.sort_unstable();
        let mut listed: Vec<&str> = LspConfig::GAME_TARGETS.to_vec();
        listed.sort_unstable();
        assert_eq!(listed, from_families);
    }

    // NOTE: the user-global layer ($HOME/.config/openplanet-lsp/config.toml)
    // is deliberately NOT unit-tested here — mutating $HOME in-process races
    // with `test_game_json_derivation_uses_variant_folder` above (lib tests
    // share one process). It's covered end-to-end by the subprocess tests in
    // tests/cli_check_tests.rs, which own their environment.
}
