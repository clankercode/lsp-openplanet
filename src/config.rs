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
    /// Raw `defines` entries from every config layer (user global, parent
    /// config, per-root config, init options), in the order they were read. They
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
    /// list. Resolved through the shared config ladder (B017) — empty
    /// when no config file sets the key.
    pub unused_callbacks: Vec<String>,
    /// How unused-declaration findings are reported. Default warn.
    /// Workspace `.openplanet-lsp.toml` wins over the user global; CLI
    /// `--unused` / `--no-unused` wins over both.
    pub unused: UnusedLevel,
    /// How preprocessor-dead `#if` branches are surfaced. Default hint.
    /// LSP-only: `check` and the TUI never emit these. Workspace
    /// `.openplanet-lsp.toml` wins over the user global; initializationOptions
    /// win over both.
    pub inactive_regions: InactiveLevel,
    /// How one-shot `check` groups diagnostics for human formats.
    /// Default last (non-errors, then errors). JSON is never regrouped.
    /// Workspace `.openplanet-lsp.toml` wins over the user global; CLI
    /// `--errors` wins over both.
    pub errors: ErrorGroup,
    /// B017: config-file keys the ladder could not use (wrong type, unknown
    /// key, unparseable file). Recorded rather than silently dropped so the
    /// CLI can warn on stderr and the LSP can surface a `window/showMessage`.
    /// Never affects analysis; empty on a clean load.
    pub config_issues: Vec<ConfigIssue>,
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
            inactive_regions: InactiveLevel::Hint,
            errors: ErrorGroup::Last,
            config_issues: Vec::new(),
        }
    }
}

/// Every key the config ladder reads out of a workspace
/// `.openplanet-lsp.toml` or the user-global
/// `~/.config/openplanet-lsp/config.toml` (B017).
///
/// Both files share one schema, so this is also the typo detector: a key that
/// is not here is reported as an unknown key and ignored. Keep it in lockstep
/// with [`ConfigFile`] and `crate::user_config::SETTINGS` (guarded by a test).
pub const CONFIG_KEYS: &[&str] = &[
    "openplanet_dir",
    "plugins_dir",
    "game_target",
    "signature_mode",
    "os_target",
    "arch_target",
    "developer_build",
    "has_dev",
    "logs",
    "server",
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
    // Written by the TUI first-run prompt (`src/tui/first_run.rs`), not read
    // by this ladder — listed so the tool never warns about its own key.
    "first_run_prompt_done",
];

/// One config-file key the ladder could not use (B017).
///
/// Before B017 a single mistyped key discarded the WHOLE file — `game_target
/// = 7` silently dropped `source_paths`, `defines` and the entire matrix
/// config with no diagnostic. Now the offending key is skipped and recorded
/// here so the loader can name the file, the key, and the expected type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    /// The config file the key came from.
    pub file: PathBuf,
    /// The offending key. Empty for a whole-file problem (unparseable TOML).
    pub key: String,
    /// What the loader wanted, e.g. `an array of strings`.
    pub expected: String,
    /// What was actually there, e.g. `an integer` / `an unknown key`, or the
    /// TOML parse error for an unparseable file.
    pub found: String,
}

impl ConfigIssue {
    fn new(file: &Path, key: &str, expected: impl Into<String>, found: impl Into<String>) -> Self {
        Self {
            file: file.to_path_buf(),
            key: key.to_string(),
            expected: expected.into(),
            found: found.into(),
        }
    }

    /// A key whose value had the wrong type; the key is skipped, its siblings
    /// in the same file stay effective.
    fn wrong_type(file: &Path, key: &str, expected: &str, value: &toml::Value) -> Self {
        Self::new(file, key, expected, describe_value(value))
    }

    /// A value with the right TOML shape but outside the key's accepted
    /// vocabulary. Preserve the value itself so the diagnostic is actionable.
    fn invalid_value(file: &Path, key: &str, expected: &str, value: &str) -> Self {
        Self::new(file, key, expected, format!("the string {value:?}"))
    }

    /// A key this loader does not read — almost always a typo.
    fn unknown_key(file: &Path, key: &str) -> Self {
        Self::new(file, key, "a known config key", "an unknown key")
    }

    /// The file is not TOML at all; it is skipped whole, but never silently.
    fn unparseable(file: &Path, err: &toml::de::Error) -> Self {
        Self::new(
            file,
            "",
            "valid TOML (the whole file was skipped)",
            err.message().to_string(),
        )
    }
}

impl std::fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.key.is_empty() {
            write!(
                f,
                "{}: expected {}, found: {}",
                self.file.display(),
                self.expected,
                self.found
            )
        } else {
            write!(
                f,
                "{}: key `{}`: expected {}, found {}",
                self.file.display(),
                self.key,
                self.expected,
                self.found
            )
        }
    }
}

/// Human description of a TOML value's shape, for [`ConfigIssue::found`].
/// Arrays describe their element types so `source_paths = [1, 2]` reads as
/// "found an array of integers", not the useless "found an array".
fn describe_value(value: &toml::Value) -> String {
    fn article(kind: &str) -> &'static str {
        if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        }
    }
    match value {
        toml::Value::Array(items) if items.is_empty() => "an empty array".to_string(),
        toml::Value::Array(items) => {
            let first = items[0].type_str();
            if items.iter().all(|i| i.type_str() == first) {
                format!("an array of {first}s")
            } else {
                "an array of mixed types".to_string()
            }
        }
        other => format!("{} {}", article(other.type_str()), other.type_str()),
    }
}

/// One config file's keys, read leniently (B017).
///
/// Deliberately NOT `#[derive(Deserialize)]`: deserializing the struct in one
/// go discards the WHOLE file on any type error, so `game_target = 7` used to
/// silently drop `source_paths`, `defines` and the entire matrix config with
/// no diagnostic. [`ConfigFile::from_table`] reads key by key instead and
/// records each bad key as a [`ConfigIssue`].
#[derive(Debug, Default)]
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
    /// GH #56: extra runtime-invoked callback names. Folded back onto this
    /// shared path by B017 — it had its own resolver only to escape the
    /// inverted ladder.
    unused_callbacks: Option<Vec<String>>,
    /// How unused-declaration findings are reported (B012). Accepts a level
    /// name or a bool; folded back onto this path by B017.
    unused: Option<UnusedLevel>,
    /// How preprocessor-dead `#if` branches are surfaced. Level name or bool.
    inactive_regions: Option<InactiveLevel>,
    /// How `check` groups diagnostics for human formats. Name or `false`.
    errors: Option<ErrorGroup>,
}

/// Reads one config file's keys off a raw [`toml::Table`], recording a
/// [`ConfigIssue`] per unusable key instead of discarding the file (B017).
struct FileReader<'a> {
    file: &'a Path,
    table: &'a toml::Table,
    issues: &'a mut Vec<ConfigIssue>,
}

impl FileReader<'_> {
    fn typed<T: serde::de::DeserializeOwned>(&mut self, key: &str, expected: &str) -> Option<T> {
        let value = self.table.get(key)?;
        match value.clone().try_into::<T>() {
            Ok(parsed) => Some(parsed),
            Err(_) => {
                self.issues
                    .push(ConfigIssue::wrong_type(self.file, key, expected, value));
                None
            }
        }
    }

    fn string(&mut self, key: &str) -> Option<String> {
        self.typed(key, "a string")
    }

    /// A string whose accepted vocabulary is known at file-parse time.
    /// Both wrong TOML shapes and unknown string values make this one key
    /// unusable, leaving lower config layers intact.
    fn validated_string<T>(
        &mut self,
        key: &str,
        expected: &str,
        parse: fn(&str) -> Option<T>,
    ) -> Option<String> {
        let value = self.table.get(key)?;
        let Some(token) = value.as_str() else {
            self.issues
                .push(ConfigIssue::wrong_type(self.file, key, expected, value));
            return None;
        };
        if parse(token).is_none() {
            self.issues
                .push(ConfigIssue::invalid_value(self.file, key, expected, token));
            return None;
        }
        Some(token.to_string())
    }

    fn boolean(&mut self, key: &str) -> Option<bool> {
        self.typed(key, "a boolean")
    }

    fn size(&mut self, key: &str) -> Option<usize> {
        self.typed(key, "a non-negative integer")
    }

    /// An ordinary array-of-strings config key. Any unusable entry makes the
    /// whole key unusable, allowing the valid lower-precedence layer to
    /// survive unchanged. An explicit empty array remains a valid value.
    fn string_list(&mut self, key: &str) -> Option<Vec<String>> {
        self.typed(key, ARRAY_OF_STRINGS)
    }

    /// The deliberately lenient legacy contract for `unused_callbacks`:
    /// retain usable callback names from a mixed array, but treat an
    /// all-unusable array as a bad key so it falls through to a lower layer.
    fn callback_list(&mut self, key: &str) -> Option<Vec<String>> {
        let value = self.table.get(key)?;
        let Some(entries) = value.as_array() else {
            self.issues.push(ConfigIssue::wrong_type(
                self.file,
                key,
                ARRAY_OF_STRINGS,
                value,
            ));
            return None;
        };
        let kept: Vec<String> = entries
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if kept.len() != entries.len() {
            self.issues.push(ConfigIssue::wrong_type(
                self.file,
                key,
                ARRAY_OF_STRINGS,
                value,
            ));
            if kept.is_empty() {
                return None;
            }
        }
        Some(kept)
    }

    fn nested_string_list(&mut self, key: &str) -> Option<Vec<Vec<String>>> {
        self.typed(key, "an array of arrays of strings")
    }

    /// A key whose value is parsed by a hand-written reader (a level name or
    /// a bool). An unusable value reports `expected` verbatim.
    fn parsed<T>(
        &mut self,
        key: &str,
        expected: &str,
        parse: fn(&toml::Value) -> Option<T>,
    ) -> Option<T> {
        let value = self.table.get(key)?;
        match parse(value) {
            Some(parsed) => Some(parsed),
            None => {
                self.issues
                    .push(ConfigIssue::wrong_type(self.file, key, expected, value));
                None
            }
        }
    }
}

const ARRAY_OF_STRINGS: &str = "an array of strings";

impl ConfigFile {
    /// Read every known key off `table`, appending one [`ConfigIssue`] per
    /// unusable or unknown key. Never fails: unusable keys are skipped and
    /// their siblings in the same file stay effective.
    fn from_table(file: &Path, table: &toml::Table, issues: &mut Vec<ConfigIssue>) -> Self {
        for key in table.keys() {
            if !CONFIG_KEYS.contains(&key.as_str()) {
                issues.push(ConfigIssue::unknown_key(file, key));
            }
        }
        let mut r = FileReader {
            file,
            table,
            issues,
        };
        Self {
            openplanet_dir: r.string("openplanet_dir"),
            plugins_dir: r.string("plugins_dir"),
            game_target: r.string("game_target"),
            signature_mode: r.validated_string(
                "signature_mode",
                "one of developer, school, regular, official",
                SignatureMode::parse,
            ),
            os_target: r.validated_string(
                "os_target",
                "one of windows, wine, linux",
                OsTarget::parse,
            ),
            arch_target: r.validated_string("arch_target", "one of 64, 32", ArchTarget::parse),
            developer_build: r.boolean("developer_build"),
            has_dev: r.boolean("has_dev"),
            logs: r.boolean("logs"),
            server: r.boolean("server"),
            defines: r.string_list("defines"),
            default_mode: r.validated_string(
                "default_mode",
                "one of tui, lsp (aliases: watch, check, server, stdio)",
                DefaultMode::parse,
            ),
            source_paths: r.string_list("source_paths"),
            ignore_paths: r.string_list("ignore_paths"),
            honor_gitignore: r.boolean("honor_gitignore"),
            shortcut_namespaces: r.string_list("shortcut_namespaces"),
            shortcut_disable_default_namespaces: r.boolean("shortcut_disable_default_namespaces"),
            shortcut_match_enums: r.boolean("shortcut_match_enums"),
            warnings_as_errors: r.boolean("warnings_as_errors"),
            define_matrix: r.string_list("define_matrix"),
            define_groups: r.nested_string_list("define_groups"),
            define_combinations: r.nested_string_list("define_combinations"),
            define_matrix_max_combinations: r.size("define_matrix_max_combinations"),
            unused_callbacks: r.callback_list("unused_callbacks"),
            unused: r.parsed(
                "unused",
                "one of ignore, hint, info, warn, error (or a boolean)",
                parse_unused_toml,
            ),
            inactive_regions: r.parsed(
                "inactive_regions",
                "one of ignore, hint, info (or a boolean)",
                parse_inactive_toml,
            ),
            errors: r.parsed(
                "errors",
                "one of last, first, off (or false)",
                parse_errors_toml,
            ),
        }
    }
}

/// Read one config file leniently (B017).
///
/// `None` when the file does not exist. Otherwise the parsed keys plus every
/// issue found: a wrong-typed or unknown key is skipped, and TOML that does
/// not parse at all yields an empty [`ConfigFile`] and one whole-file issue —
/// skipped, but never silently.
fn read_config_file(path: &Path) -> Option<(ConfigFile, Vec<ConfigIssue>)> {
    let contents = std::fs::read_to_string(path).ok()?;
    let mut issues = Vec::new();
    match contents.parse::<toml::Table>() {
        Ok(table) => {
            let cfg = ConfigFile::from_table(path, &table, &mut issues);
            Some((cfg, issues))
        }
        Err(err) => {
            issues.push(ConfigIssue::unparseable(path, &err));
            Some((ConfigFile::default(), issues))
        }
    }
}

/// Render config issues consistently on CLI/UserPrefs-only paths. Kept as a
/// writer seam so tests can prove reporting without redirecting process-wide
/// stderr or mutating HOME.
fn write_config_issues(
    writer: &mut impl std::io::Write,
    issues: &[ConfigIssue],
) -> std::io::Result<()> {
    for issue in issues {
        writeln!(writer, "warning: config: {issue}")?;
    }
    Ok(())
}

/// How unused-declaration findings are reported.
///
/// Default is [`UnusedLevel::Warn`] (today's behavior). `ignore` drops the
/// pass entirely (it is noisy on large plugins). `error` emits ERROR
/// diagnostics so `check` fails without `--warnings-as-errors`.
///
/// The two quiet levels exist because "never used" is advisory, not a
/// defect: `info` keeps findings listed (VS Code shows INFORMATION in the
/// Problems panel) while `hint` drops them out of the panel entirely and
/// leaves only the editor's grey-out. Both stay non-fatal — `check` exits 0
/// even under `--warnings-as-errors`, which promotes WARNING only.
///
/// Severity is only half the story: every unused finding also carries
/// [`tower_lsp::lsp_types::DiagnosticTag::UNNECESSARY`], which is what makes
/// a client fade the range (see `server::diagnostics::unused_to_diagnostic`).
/// That tag is what keeps `hint` visible at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnusedLevel {
    Ignore,
    Hint,
    Info,
    #[default]
    Warn,
    Error,
}

impl UnusedLevel {
    /// Parse a config / CLI token. Aliases: off/false/none/disable → ignore;
    /// faded/grey/gray → hint; information/note → info; on/true/warning →
    /// warn; err/errors → error.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ignore" | "off" | "false" | "none" | "disable" | "disabled" => Some(Self::Ignore),
            "hint" | "faded" | "grey" | "gray" => Some(Self::Hint),
            "info" | "information" | "note" => Some(Self::Info),
            "warn" | "warning" | "on" | "true" => Some(Self::Warn),
            "error" | "err" | "errors" => Some(Self::Error),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Hint => "hint",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// How preprocessor-dead code (`#if` branches the current defines switch
/// off) is surfaced.
///
/// Default is [`InactiveLevel::Hint`]: a HINT diagnostic tagged
/// [`tower_lsp::lsp_types::DiagnosticTag::UNNECESSARY`] over each dead body,
/// which fades the range in the editor while staying out of the problems
/// list. `info` lists them as well; `ignore` emits nothing.
///
/// There is deliberately no warn/error level. A branch that is off for the
/// current target is dead BY DESIGN, not a defect, and an `error` level would
/// fail `check` on every `#if MP4`. For the same reason this is an LSP-only
/// surface — `check` and the TUI never emit it (see
/// `server::diagnostics::inactive_region_diagnostics`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InactiveLevel {
    Ignore,
    #[default]
    Hint,
    Info,
}

impl InactiveLevel {
    /// Parse a config token. Aliases: off/false/none/disable → ignore;
    /// on/true/grey/gray/faded → hint; information/note → info.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ignore" | "off" | "false" | "none" | "disable" | "disabled" => Some(Self::Ignore),
            "hint" | "on" | "true" | "grey" | "gray" | "faded" => Some(Self::Hint),
            "info" | "information" | "note" => Some(Self::Info),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Hint => "hint",
            Self::Info => "info",
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
    /// Resolve `default_mode` / `warnings_as_errors` through the same
    /// ladder as [`LspConfig`] (B017): user global
    /// `~/.config/openplanet-lsp/config.toml` first, then the workspace
    /// `.openplanet-lsp.toml` — project config wins. The CLI flag, where
    /// there is one, still beats both.
    pub fn load(workspace_root: Option<&Path>) -> Self {
        let (prefs, issues) = Self::load_with_issues(workspace_root);
        let _ = write_config_issues(&mut std::io::stderr().lock(), &issues);
        prefs
    }

    /// Load the CLI preferences without choosing how to report config issues.
    /// Callers that also load [`LspConfig`] use this to avoid reporting the
    /// shared config-reader issues twice.
    pub(crate) fn load_silent(workspace_root: Option<&Path>) -> Self {
        Self::load_with_issues(workspace_root).0
    }

    fn load_with_issues(workspace_root: Option<&Path>) -> (Self, Vec<ConfigIssue>) {
        Self::from_layers_with_issues(LspConfig::user_config_path().as_deref(), workspace_root)
    }

    /// Pure variant of [`Self::load`]: `user` is the user-global config
    /// FILE, `root` the workspace DIRECTORY. Explicit paths so precedence
    /// tests never mutate `$HOME`.
    pub fn from_layers(user: Option<&Path>, root: Option<&Path>) -> Self {
        Self::from_layers_with_issues(user, root).0
    }

    /// Pure issue-carrying variant used by tests and callers that need to
    /// surface config diagnostics through a channel other than stderr.
    pub fn from_layers_with_issues(
        user: Option<&Path>,
        root: Option<&Path>,
    ) -> (Self, Vec<ConfigIssue>) {
        let mut prefs = Self::default();
        let mut issues = Vec::new();
        if let Some(user) = user {
            prefs.apply_file(user, &mut issues);
        }
        if let Some(root) = root {
            prefs.apply_workspace_config(root, &mut issues);
        }
        (prefs, issues)
    }

    fn apply_workspace_config(&mut self, root: &Path, issues: &mut Vec<ConfigIssue>) {
        self.apply_file(
            &root.join(crate::config_watch::WORKSPACE_CONFIG_FILE),
            issues,
        );
    }

    /// One layer. Reads through the shared lenient reader (B017), so a
    /// mistyped `game_target` no longer takes `default_mode` down with it.
    fn apply_file(&mut self, path: &Path, issues: &mut Vec<ConfigIssue>) {
        let Some((cfg, mut file_issues)) = read_config_file(path) else {
            return;
        };
        issues.append(&mut file_issues);
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

    /// Build config in ascending precedence: auto-detect → user global →
    /// parent (opened-folder) config → per-root config → init params. The
    /// parent layer only applies when it differs from the per-root path
    /// (multi-root t5).
    pub fn load(workspace_root: Option<&Path>, init_options: Option<&serde_json::Value>) -> Self {
        Self::load_with_parent(workspace_root, None, init_options)
    }

    /// Resolve the FILE layers of the ladder, and nothing else — no
    /// auto-detect, no manifest, no init options.
    ///
    /// `user` is the user-global config FILE; `parent` and `root` are
    /// DIRECTORIES whose `.openplanet-lsp.toml` is read. Every layer path is
    /// explicit, so precedence tests never mutate `$HOME` (which races other
    /// tests in-process).
    pub fn from_layers(
        user: Option<&Path>,
        parent: Option<&Path>,
        root: Option<&Path>,
    ) -> (Self, Vec<ConfigIssue>) {
        let mut config = Self::default();
        let issues = config.apply_config_layers(user, parent, root);
        (config, issues)
    }

    /// Multi-root aware load: `parent` is the opened folder hosting multiple
    /// plugin roots. Ladder (lowest→highest): auto-detect defaults < user
    /// global < parent config < per-root config < init options. Manifest and
    /// accumulated config/init define overrides are applied after target
    /// derivation. typedb settings stay global (resolved init > user > first
    /// root) — per-root: defines, plugins_dir, source_paths, ignore_paths.
    pub fn load_with_parent(
        workspace_root: Option<&Path>,
        parent: Option<&Path>,
        init_options: Option<&serde_json::Value>,
    ) -> Self {
        // Every field starts at its documented default; the layers below
        // overwrite in ascending precedence. (Kept as `default()` rather than
        // a second struct literal so a new field cannot be added to one and
        // forgotten in the other.)
        let mut config = Self::default();

        // Layer 1: Auto-detect
        config.auto_detect();

        // Layers 2a-2c: the config files, lowest precedence FIRST (B017) —
        // user global, then the opened-folder parent (multi-root t5), then
        // the per-root file. Every key obeys this one order; the per-key
        // resolvers that `unused_callbacks` (GH #56), `unused`,
        // `inactive_regions`, `errors` and `honor_gitignore` (GH #60) used
        // to need existed only to escape the old inverted order and are gone.
        let issues = config.apply_config_layers(
            Self::user_config_path().as_deref(),
            parent.filter(|p| Some(*p) != workspace_root),
            workspace_root,
        );
        let _ = write_config_issues(&mut std::io::stderr().lock(), &issues);
        config.config_issues = issues;

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

    /// Apply the three config-file layers in ASCENDING precedence — user
    /// global, opened-folder parent, per-root workspace — each one an
    /// unconditional overwrite of the keys it sets (B017).
    ///
    /// Before B017 this ran the user global LAST, so a machine-wide
    /// `~/.config/openplanet-lsp/config.toml` silently overrode every
    /// plugin's own `.openplanet-lsp.toml` with no way to override it
    /// locally — while `UserPrefs` resolved the opposite way for
    /// `default_mode` / `warnings_as_errors` in the same files. One order
    /// now, for every key: project config beats machine config.
    ///
    /// `user` is a FILE path; `parent` and `root` are DIRECTORIES.
    /// Returns every [`ConfigIssue`] the layers produced, in layer order.
    fn apply_config_layers(
        &mut self,
        user: Option<&Path>,
        parent: Option<&Path>,
        root: Option<&Path>,
    ) -> Vec<ConfigIssue> {
        let layers = [
            user.map(Path::to_path_buf),
            parent.map(|p| p.join(crate::config_watch::WORKSPACE_CONFIG_FILE)),
            root.map(|r| r.join(crate::config_watch::WORKSPACE_CONFIG_FILE)),
        ];
        let mut issues = Vec::new();
        for path in layers.iter().flatten() {
            if let Some((cfg, mut file_issues)) = read_config_file(path) {
                issues.append(&mut file_issues);
                self.apply_config_file(cfg);
            }
        }
        issues
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
        // Folded back onto the shared ladder by B017 — lists never merge, so
        // the winning layer's list is the whole list.
        if let Some(callbacks) = cfg.unused_callbacks {
            self.unused_callbacks = callbacks;
        }
        if let Some(level) = cfg.unused {
            self.unused = level;
        }
        if let Some(level) = cfg.inactive_regions {
            self.inactive_regions = level;
        }
        if let Some(group) = cfg.errors {
            self.errors = group;
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
        if let Some(level) = opts.get("inactive_regions").and_then(parse_inactive_json) {
            self.inactive_regions = level;
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

fn parse_inactive_toml(value: &toml::Value) -> Option<InactiveLevel> {
    match value {
        toml::Value::String(s) => InactiveLevel::parse(s),
        toml::Value::Boolean(false) => Some(InactiveLevel::Ignore),
        toml::Value::Boolean(true) => Some(InactiveLevel::Hint),
        _ => None,
    }
}

fn parse_inactive_json(value: &serde_json::Value) -> Option<InactiveLevel> {
    if let Some(s) = value.as_str() {
        return InactiveLevel::parse(s);
    }
    match value.as_bool() {
        Some(false) => Some(InactiveLevel::Ignore),
        Some(true) => Some(InactiveLevel::Hint),
        None => None,
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
            ("info", UnusedLevel::Info),
            ("INFO", UnusedLevel::Info),
            ("information", UnusedLevel::Info),
            ("note", UnusedLevel::Info),
            ("hint", UnusedLevel::Hint),
            ("HINT", UnusedLevel::Hint),
            ("faded", UnusedLevel::Hint),
            ("grey", UnusedLevel::Hint),
            ("gray", UnusedLevel::Hint),
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
            LspConfig::from_layers(None, None, None).0.unused,
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
            LspConfig::from_layers(Some(&user), None, Some(&ws_ignore))
                .0
                .unused,
            UnusedLevel::Ignore,
            "workspace ignore must beat user error"
        );

        let ws_bool = tmp.join("ws-bool");
        std::fs::create_dir_all(&ws_bool).unwrap();
        std::fs::write(ws_bool.join(".openplanet-lsp.toml"), "unused = false\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws_bool))
                .0
                .unused,
            UnusedLevel::Ignore,
            "unused = false is ignore"
        );

        assert_eq!(
            LspConfig::from_layers(Some(&user), None, None).0.unused,
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
    fn inactive_level_parse_and_round_trip() {
        for (input, want) in [
            ("ignore", InactiveLevel::Ignore),
            ("IGNORE", InactiveLevel::Ignore),
            ("off", InactiveLevel::Ignore),
            ("false", InactiveLevel::Ignore),
            ("none", InactiveLevel::Ignore),
            ("hint", InactiveLevel::Hint),
            ("HINT", InactiveLevel::Hint),
            ("on", InactiveLevel::Hint),
            ("true", InactiveLevel::Hint),
            ("grey", InactiveLevel::Hint),
            ("gray", InactiveLevel::Hint),
            ("faded", InactiveLevel::Hint),
            ("info", InactiveLevel::Info),
            ("information", InactiveLevel::Info),
            ("note", InactiveLevel::Info),
        ] {
            assert_eq!(InactiveLevel::parse(input), Some(want), "parse {input:?}");
        }
        // Deliberately NOT levels: a dead branch is dead by design, and
        // `error` would fail `check` on every `#if MP4`.
        assert!(InactiveLevel::parse("warn").is_none());
        assert!(InactiveLevel::parse("error").is_none());
        assert!(InactiveLevel::parse("banana").is_none());
        assert!(InactiveLevel::parse("").is_none());

        for level in [
            InactiveLevel::Ignore,
            InactiveLevel::Hint,
            InactiveLevel::Info,
        ] {
            assert_eq!(
                InactiveLevel::parse(level.as_str()),
                Some(level),
                "round-trip {level:?}"
            );
        }
    }

    #[test]
    fn inactive_regions_default_to_hint() {
        assert_eq!(LspConfig::default().inactive_regions, InactiveLevel::Hint);
        assert_eq!(
            LspConfig::from_layers(None, None, None).0.inactive_regions,
            InactiveLevel::Hint
        );
    }

    #[test]
    fn inactive_regions_resolve_through_the_whole_ladder() {
        let tmp = std::env::temp_dir().join(format!("oplsp-inactive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let user = tmp.join("user.toml");
        std::fs::write(&user, "inactive_regions = \"info\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, None)
                .0
                .inactive_regions,
            InactiveLevel::Info,
            "user global applies when nothing else sets the key"
        );

        for (toml_value, want) in [
            ("\"ignore\"", InactiveLevel::Ignore),
            ("false", InactiveLevel::Ignore),
            ("true", InactiveLevel::Hint),
            ("\"grey\"", InactiveLevel::Hint),
        ] {
            let ws = tmp.join(format!("ws-{want:?}-{}", toml_value.replace('"', "")));
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(
                ws.join(".openplanet-lsp.toml"),
                format!("inactive_regions = {toml_value}\n"),
            )
            .unwrap();
            assert_eq!(
                LspConfig::from_layers(Some(&user), None, Some(&ws))
                    .0
                    .inactive_regions,
                want,
                "workspace inactive_regions = {toml_value} must beat the user global"
            );
        }

        let init_root = tmp.join("init");
        std::fs::create_dir_all(&init_root).unwrap();
        std::fs::write(
            init_root.join(".openplanet-lsp.toml"),
            "inactive_regions = \"ignore\"\n",
        )
        .unwrap();
        for (json_value, want) in [("hint", InactiveLevel::Hint), ("info", InactiveLevel::Info)] {
            let opts = serde_json::json!({ "inactive_regions": json_value });
            let cfg = LspConfig::load(Some(&init_root), Some(&opts));
            assert_eq!(
                cfg.inactive_regions, want,
                "initializationOptions must beat the workspace file"
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unused_level_as_str_round_trips() {
        for level in [
            UnusedLevel::Ignore,
            UnusedLevel::Hint,
            UnusedLevel::Info,
            UnusedLevel::Warn,
            UnusedLevel::Error,
        ] {
            assert_eq!(
                UnusedLevel::parse(level.as_str()),
                Some(level),
                "as_str/parse round-trip for {level:?}"
            );
        }
    }

    /// The quiet levels are ordinary ladder citizens: user global, workspace
    /// file, and initializationOptions all resolve them, with the same
    /// precedence as ignore/warn/error.
    #[test]
    fn unused_quiet_levels_resolve_through_the_whole_ladder() {
        let tmp = std::env::temp_dir().join(format!("oplsp-unused-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let user = tmp.join("user.toml");
        std::fs::write(&user, "unused = \"hint\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, None).0.unused,
            UnusedLevel::Hint,
            "user-global hint applies when nothing else sets the key"
        );

        for (toml_value, want) in [
            ("\"info\"", UnusedLevel::Info),
            ("\"note\"", UnusedLevel::Info),
            ("\"hint\"", UnusedLevel::Hint),
            ("\"grey\"", UnusedLevel::Hint),
        ] {
            let ws = tmp.join(format!("ws-{want:?}-{toml_value}").replace(['"', ':'], ""));
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(
                ws.join(".openplanet-lsp.toml"),
                format!("unused = {toml_value}\n"),
            )
            .unwrap();
            assert_eq!(
                LspConfig::from_layers(Some(&user), None, Some(&ws))
                    .0
                    .unused,
                want,
                "workspace unused = {toml_value} must beat the user global"
            );
        }

        // initializationOptions sit above the workspace file.
        let init_root = tmp.join("init");
        std::fs::create_dir_all(&init_root).unwrap();
        std::fs::write(
            init_root.join(".openplanet-lsp.toml"),
            "unused = \"error\"\n",
        )
        .unwrap();
        for (json_value, want) in [("hint", UnusedLevel::Hint), ("info", UnusedLevel::Info)] {
            let opts = serde_json::json!({ "unused": json_value });
            let cfg = LspConfig::load(Some(&init_root), Some(&opts));
            assert_eq!(
                cfg.unused, want,
                "initializationOptions unused = {json_value} must beat the workspace file"
            );
        }

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
            LspConfig::from_layers(None, None, None).0.errors,
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
            LspConfig::from_layers(Some(&user), None, Some(&ws_last))
                .0
                .errors,
            ErrorGroup::Last,
            "workspace last must beat user first"
        );

        let ws_end = tmp.join("ws-end");
        std::fs::create_dir_all(&ws_end).unwrap();
        std::fs::write(ws_end.join(".openplanet-lsp.toml"), "errors = \"end\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws_end))
                .0
                .errors,
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
            LspConfig::from_layers(Some(&user), None, Some(&ws_start))
                .0
                .errors,
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
            LspConfig::from_layers(Some(&user), None, Some(&ws_begin))
                .0
                .errors,
            ErrorGroup::First,
            "begin is first"
        );

        let ws_off = tmp.join("ws-off");
        std::fs::create_dir_all(&ws_off).unwrap();
        std::fs::write(ws_off.join(".openplanet-lsp.toml"), "errors = \"off\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws_off))
                .0
                .errors,
            ErrorGroup::Off
        );

        let ws_bool = tmp.join("ws-bool");
        std::fs::create_dir_all(&ws_bool).unwrap();
        std::fs::write(ws_bool.join(".openplanet-lsp.toml"), "errors = false\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws_bool))
                .0
                .errors,
            ErrorGroup::Off,
            "errors = false is off"
        );

        let ws_none = tmp.join("ws-none");
        std::fs::create_dir_all(&ws_none).unwrap();
        std::fs::write(ws_none.join(".openplanet-lsp.toml"), "errors = \"none\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws_none))
                .0
                .errors,
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
            LspConfig::from_layers(Some(&user), None, Some(&ws_interleaved))
                .0
                .errors,
            ErrorGroup::Off
        );

        assert_eq!(
            LspConfig::from_layers(Some(&user), None, None).0.errors,
            ErrorGroup::First,
            "user-global errors = first when workspace does not set it"
        );

        let ws_bad = tmp.join("ws-bad");
        std::fs::create_dir_all(&ws_bad).unwrap();
        std::fs::write(ws_bad.join(".openplanet-lsp.toml"), "errors = \"banana\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws_bad))
                .0
                .errors,
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
            LspConfig::from_layers(Some(&user), None, None)
                .0
                .unused_callbacks,
            vec!["FromUser".to_string()]
        );

        // Workspace wins outright — lists do not merge.
        std::fs::write(
            ws.join(".openplanet-lsp.toml"),
            "unused_callbacks = [\"FromWorkspace\"]\n",
        )
        .unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws))
                .0
                .unused_callbacks,
            vec!["FromWorkspace".to_string()]
        );

        // An explicitly EMPTY workspace list still wins.
        std::fs::write(ws.join(".openplanet-lsp.toml"), "unused_callbacks = []\n").unwrap();
        assert!(LspConfig::from_layers(Some(&user), None, Some(&ws))
            .0
            .unused_callbacks
            .is_empty());

        // Workspace file that does not mention the key falls through.
        std::fs::write(ws.join(".openplanet-lsp.toml"), "game_target = \"MP4\"\n").unwrap();
        assert_eq!(
            LspConfig::from_layers(Some(&user), None, Some(&ws))
                .0
                .unused_callbacks,
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
            LspConfig::from_layers(Some(&user), Some(&parent), Some(&ws))
                .0
                .unused_callbacks,
            vec!["FromRoot".to_string()]
        );
        assert_eq!(
            LspConfig::from_layers(Some(&user), Some(&parent), None)
                .0
                .unused_callbacks,
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
                LspConfig::from_layers(None, None, Some(&dir))
                    .0
                    .unused_callbacks,
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
        cfg.apply_config_layers(None, None, Some(&dir));
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

    // ---------------------------------------------------------------------
    // B017: ONE ladder — init options > CLI flag > per-root workspace config
    // > parent config > user global > auto-detect — plus per-key-tolerant
    // parsing so a single mistyped key cannot discard the whole file.
    // ---------------------------------------------------------------------

    /// Fresh temp dir for a B017 case, with `user.toml` + `parent/` + `root/`
    /// laid out. Returns `(tmp, user_file, parent_dir, root_dir)`.
    fn b017_layers(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("oplsp-b017-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let parent = tmp.join("parent");
        let root = tmp.join("root");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        (tmp.clone(), tmp.join("user.toml"), parent, root)
    }

    fn write_ws(dir: &Path, body: &str) {
        std::fs::write(dir.join(".openplanet-lsp.toml"), body).unwrap();
    }

    /// One row of the ladder table: a key, a user-global value, a workspace
    /// value, and how to render the resolved value for comparison.
    struct LadderCase {
        key: &'static str,
        user: &'static str,
        workspace: &'static str,
        render: fn(&LspConfig) -> String,
        want_workspace: &'static str,
        want_user: &'static str,
    }

    fn ladder_cases() -> Vec<LadderCase> {
        vec![
            LadderCase {
                key: "game_target",
                user: "game_target = \"MP4\"",
                workspace: "game_target = \"TURBO\"",
                render: |c| c.game_target.clone(),
                want_workspace: "TURBO",
                want_user: "MP4",
            },
            LadderCase {
                key: "openplanet_dir",
                user: "openplanet_dir = \"/from/user\"",
                workspace: "openplanet_dir = \"/from/workspace\"",
                render: |c| format!("{:?}", c.openplanet_dir),
                want_workspace: "Some(\"/from/workspace\")",
                want_user: "Some(\"/from/user\")",
            },
            LadderCase {
                key: "plugins_dir",
                user: "plugins_dir = \"/user/plugins\"",
                workspace: "plugins_dir = \"/ws/plugins\"",
                render: |c| format!("{:?}", c.plugins_dir),
                want_workspace: "Some(\"/ws/plugins\")",
                want_user: "Some(\"/user/plugins\")",
            },
            LadderCase {
                key: "source_paths",
                user: "source_paths = [\"b\"]",
                workspace: "source_paths = [\"a\"]",
                render: |c| format!("{:?}", c.source_paths),
                want_workspace: "Some([\"a\"])",
                want_user: "Some([\"b\"])",
            },
            LadderCase {
                key: "ignore_paths",
                user: "ignore_paths = [\"vendor\"]",
                workspace: "ignore_paths = [\"OtherPacks\"]",
                render: |c| format!("{:?}", c.ignore_paths),
                want_workspace: "Some([\"OtherPacks\"])",
                want_user: "Some([\"vendor\"])",
            },
            LadderCase {
                key: "honor_gitignore",
                user: "honor_gitignore = true",
                workspace: "honor_gitignore = false",
                render: |c| format!("{:?}", c.honor_gitignore),
                want_workspace: "Some(false)",
                want_user: "Some(true)",
            },
            LadderCase {
                key: "shortcut_namespaces",
                user: "shortcut_namespaces = [\"FromUser\"]",
                workspace: "shortcut_namespaces = [\"FromWorkspace\"]",
                render: |c| format!("{:?}", c.shortcut_namespaces),
                want_workspace: "Some([\"FromWorkspace\"])",
                want_user: "Some([\"FromUser\"])",
            },
            LadderCase {
                key: "shortcut_disable_default_namespaces",
                user: "shortcut_disable_default_namespaces = true",
                workspace: "shortcut_disable_default_namespaces = false",
                render: |c| c.shortcut_disable_default_namespaces.to_string(),
                want_workspace: "false",
                want_user: "true",
            },
            LadderCase {
                key: "shortcut_match_enums",
                user: "shortcut_match_enums = true",
                workspace: "shortcut_match_enums = false",
                render: |c| c.shortcut_match_enums.to_string(),
                want_workspace: "false",
                want_user: "true",
            },
            LadderCase {
                key: "define_matrix",
                user: "define_matrix = [\"FROM_USER\"]",
                workspace: "define_matrix = [\"FROM_WORKSPACE\"]",
                render: |c| format!("{:?}", c.define_matrix),
                want_workspace: "[\"FROM_WORKSPACE\"]",
                want_user: "[\"FROM_USER\"]",
            },
            LadderCase {
                key: "define_groups",
                user: "define_groups = [[\"U1\", \"U2\"]]",
                workspace: "define_groups = [[\"W1\", \"W2\"]]",
                render: |c| format!("{:?}", c.define_groups),
                want_workspace: "[[\"W1\", \"W2\"]]",
                want_user: "[[\"U1\", \"U2\"]]",
            },
            LadderCase {
                key: "define_combinations",
                user: "define_combinations = [[\"U\"]]",
                workspace: "define_combinations = [[\"W\"]]",
                render: |c| format!("{:?}", c.define_combinations),
                want_workspace: "Some([[\"W\"]])",
                want_user: "Some([[\"U\"]])",
            },
            LadderCase {
                key: "define_matrix_max_combinations",
                user: "define_matrix_max_combinations = 4",
                workspace: "define_matrix_max_combinations = 32",
                render: |c| c.define_matrix_max_combinations.to_string(),
                want_workspace: "32",
                want_user: "4",
            },
            LadderCase {
                key: "signature_mode",
                user: "signature_mode = \"official\"",
                workspace: "signature_mode = \"school\"",
                render: |c| c.signature_mode.as_str().to_string(),
                want_workspace: "school",
                want_user: "official",
            },
            LadderCase {
                key: "os_target",
                user: "os_target = \"linux\"",
                workspace: "os_target = \"wine\"",
                render: |c| c.os_target.as_str().to_string(),
                want_workspace: "wine",
                want_user: "linux",
            },
            LadderCase {
                key: "arch_target",
                user: "arch_target = \"64\"",
                workspace: "arch_target = \"32\"",
                render: |c| c.arch_target.as_str().to_string(),
                want_workspace: "32",
                want_user: "64",
            },
            LadderCase {
                key: "developer_build",
                user: "developer_build = true",
                workspace: "developer_build = false",
                render: |c| c.build_flags.developer_build.to_string(),
                want_workspace: "false",
                want_user: "true",
            },
            LadderCase {
                key: "has_dev",
                user: "has_dev = true",
                workspace: "has_dev = false",
                render: |c| c.build_flags.has_dev.to_string(),
                want_workspace: "false",
                want_user: "true",
            },
            LadderCase {
                key: "logs",
                user: "logs = true",
                workspace: "logs = false",
                render: |c| c.build_flags.logs.to_string(),
                want_workspace: "false",
                want_user: "true",
            },
            LadderCase {
                key: "server",
                user: "server = true",
                workspace: "server = false",
                render: |c| c.build_flags.server.to_string(),
                want_workspace: "false",
                want_user: "true",
            },
            // The four keys that needed their own per-key resolver before
            // B017 (B012 / B015 / GH #56 / GH #60) must keep workspace-wins
            // once they are folded back onto the shared ladder.
            LadderCase {
                key: "unused",
                user: "unused = \"error\"",
                workspace: "unused = \"ignore\"",
                render: |c| c.unused.as_str().to_string(),
                want_workspace: "ignore",
                want_user: "error",
            },
            LadderCase {
                key: "unused_callbacks",
                user: "unused_callbacks = [\"FromUser\"]",
                workspace: "unused_callbacks = [\"FromWorkspace\"]",
                render: |c| format!("{:?}", c.unused_callbacks),
                want_workspace: "[\"FromWorkspace\"]",
                want_user: "[\"FromUser\"]",
            },
            LadderCase {
                key: "inactive_regions",
                user: "inactive_regions = \"info\"",
                workspace: "inactive_regions = \"ignore\"",
                render: |c| c.inactive_regions.as_str().to_string(),
                want_workspace: "ignore",
                want_user: "info",
            },
            LadderCase {
                key: "errors",
                user: "errors = \"first\"",
                workspace: "errors = \"off\"",
                render: |c| c.errors.as_str().to_string(),
                want_workspace: "off",
                want_user: "first",
            },
        ]
    }

    /// B017 defect 1: a machine-wide `~/.config/openplanet-lsp/config.toml`
    /// must NEVER override a plugin's own `.openplanet-lsp.toml`. Checked for
    /// every key the shared ladder carries, not just `source_paths`.
    #[test]
    fn per_root_workspace_config_beats_the_user_global_for_every_ladder_key() {
        let (tmp, user, _parent, root) = b017_layers("ws-over-user");
        for case in ladder_cases() {
            std::fs::write(&user, format!("{}\n", case.user)).unwrap();
            write_ws(&root, &format!("{}\n", case.workspace));
            let (cfg, issues) = LspConfig::from_layers(Some(&user), None, Some(&root));
            assert_eq!(
                (case.render)(&cfg),
                case.want_workspace,
                "workspace `{}` must beat the user global",
                case.key
            );
            assert!(issues.is_empty(), "clean files must report no issues");

            // ...and the user global still applies when the workspace file
            // does not mention the key at all.
            write_ws(&root, "# nothing relevant here\n");
            let (cfg, _) = LspConfig::from_layers(Some(&user), None, Some(&root));
            assert_eq!(
                (case.render)(&cfg),
                case.want_user,
                "user global `{}` must apply when the workspace file is silent",
                case.key
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Multi-root: the per-root file beats the opened-folder parent, and the
    /// parent beats the user global.
    #[test]
    fn per_root_beats_parent_which_beats_user_global() {
        let (tmp, user, parent, root) = b017_layers("root-parent-user");
        for case in ladder_cases() {
            std::fs::write(&user, format!("{}\n", case.user)).unwrap();
            write_ws(&parent, &format!("{}\n", case.user));
            write_ws(&root, &format!("{}\n", case.workspace));
            let (cfg, _) = LspConfig::from_layers(Some(&user), Some(&parent), Some(&root));
            assert_eq!(
                (case.render)(&cfg),
                case.want_workspace,
                "per-root `{}` must beat the parent config",
                case.key
            );

            write_ws(&parent, &format!("{}\n", case.workspace));
            write_ws(&root, "# silent\n");
            let (cfg, _) = LspConfig::from_layers(Some(&user), Some(&parent), Some(&root));
            assert_eq!(
                (case.render)(&cfg),
                case.want_workspace,
                "parent `{}` must beat the user global",
                case.key
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// initializationOptions stay at the top of the ladder after the flip.
    #[test]
    fn init_options_still_beat_every_config_file_layer() {
        let tmp = std::env::temp_dir().join(format!("oplsp-b017-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".openplanet-lsp.toml"),
            "game_target = \"TURBO\"\nshortcut_match_enums = false\nunused = \"ignore\"\n\
             errors = \"off\"\ninactive_regions = \"ignore\"\n",
        )
        .unwrap();
        let opts = serde_json::json!({
            "game_target": "MP4",
            "shortcut_match_enums": true,
            "unused": "error",
            "errors": "first",
            "inactive_regions": "info",
        });
        let cfg = LspConfig::load(Some(&tmp), Some(&opts));
        assert_eq!(cfg.game_target, "MP4");
        assert!(cfg.shortcut_match_enums);
        assert_eq!(cfg.unused, UnusedLevel::Error);
        assert_eq!(cfg.errors, ErrorGroup::First);
        assert_eq!(cfg.inactive_regions, InactiveLevel::Info);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// B017 defect 2: ONE mistyped key must not take its siblings down with
    /// it. Every type class the schema uses gets a variant.
    #[test]
    fn one_mistyped_key_leaves_sibling_keys_in_the_same_file_effective() {
        let (tmp, _user, _parent, root) = b017_layers("bad-key-siblings");
        // (case name, the bad line, the key that must be reported)
        let bad_lines: &[(&str, &str, &str)] = &[
            ("wrong-scalar-type", "game_target = 7", "game_target"),
            (
                "string-instead-of-array",
                "source_paths = \"src\"",
                "source_paths",
            ),
            (
                "wrong-array-element-type",
                "ignore_paths = [1, 2]",
                "ignore_paths",
            ),
            (
                "bool-instead-of-string",
                "openplanet_dir = true",
                "openplanet_dir",
            ),
            (
                "string-instead-of-bool",
                "shortcut_match_enums = \"yes\"",
                "shortcut_match_enums",
            ),
            (
                "string-instead-of-int",
                "define_matrix_max_combinations = \"lots\"",
                "define_matrix_max_combinations",
            ),
            (
                "flat-instead-of-nested",
                "define_groups = [\"A\", \"B\"]",
                "define_groups",
            ),
            (
                "table-instead-of-array",
                "unused_callbacks = { a = 1 }",
                "unused_callbacks",
            ),
            ("unknown-enum-shape", "unused = 7", "unused"),
            ("unknown-key", "sorce_paths = [\"src\"]", "sorce_paths"),
        ];
        for (name, bad, reported_key) in bad_lines {
            write_ws(
                &root,
                &format!("{bad}\ndefine_matrix = [\"SIBLING\"]\nplugins_dir = \"/sibling\"\n"),
            );
            let (cfg, issues) = LspConfig::from_layers(None, None, Some(&root));
            assert_eq!(
                cfg.define_matrix,
                vec!["SIBLING".to_string()],
                "case {name}: sibling array key must survive a bad key"
            );
            assert_eq!(
                cfg.plugins_dir,
                Some(PathBuf::from("/sibling")),
                "case {name}: sibling scalar key must survive a bad key"
            );
            assert!(
                issues.iter().any(|i| i.key == *reported_key),
                "case {name}: expected an issue naming `{reported_key}`, got {issues:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A recorded issue names the file, the key, the expected type and what
    /// was actually there — enough to fix the file without reading the source.
    #[test]
    fn config_issues_name_the_file_key_expected_and_found() {
        let (tmp, _user, _parent, root) = b017_layers("issue-shape");
        write_ws(&root, "source_paths = 7\n");
        let (_cfg, issues) = LspConfig::from_layers(None, None, Some(&root));
        assert_eq!(issues.len(), 1, "one bad key, one issue: {issues:?}");
        let issue = &issues[0];
        assert_eq!(issue.file, root.join(".openplanet-lsp.toml"));
        assert_eq!(issue.key, "source_paths");
        assert!(
            issue.expected.contains("array") && issue.expected.contains("string"),
            "expected should name the wanted type, got {:?}",
            issue.expected
        );
        assert!(
            issue.found.contains("integer"),
            "found should name the actual type, got {:?}",
            issue.found
        );
        let rendered = issue.to_string();
        assert!(
            rendered.contains("source_paths") && rendered.contains(".openplanet-lsp.toml"),
            "rendered issue must name key + file: {rendered}"
        );

        // Element-type mismatches say what the array actually held.
        write_ws(&root, "source_paths = [1, 2]\n");
        let (_cfg, issues) = LspConfig::from_layers(None, None, Some(&root));
        assert!(
            issues[0].found.contains("integer"),
            "array element type should be described: {:?}",
            issues[0].found
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Genuinely malformed TOML stays a whole-file skip — but it must warn,
    /// naming the file, and must not panic or poison the other layers.
    #[test]
    fn malformed_toml_degrades_to_defaults_and_names_the_file() {
        let (tmp, user, _parent, root) = b017_layers("malformed");
        std::fs::write(&user, "source_paths = [\"from-user\"]\n").unwrap();
        write_ws(&root, "this is not toml =\n");
        let (cfg, issues) = LspConfig::from_layers(Some(&user), None, Some(&root));
        assert_eq!(
            cfg.source_paths,
            Some(vec![PathBuf::from("from-user")]),
            "an unparseable per-root file must fall through to the layer below"
        );
        assert_eq!(issues.len(), 1, "expected one whole-file issue: {issues:?}");
        assert_eq!(issues[0].file, root.join(".openplanet-lsp.toml"));
        assert!(
            issues[0].key.is_empty(),
            "a whole-file problem has no single key"
        );
        assert!(
            issues[0].to_string().contains(".openplanet-lsp.toml"),
            "the warning must name the file: {}",
            issues[0]
        );

        // Missing files are not a problem at all.
        let (_cfg, issues) = LspConfig::from_layers(None, None, Some(&tmp.join("nonexistent")));
        assert!(issues.is_empty(), "a missing file is not an issue");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Unknown keys are reported (they are nearly always typos) and ignored.
    #[test]
    fn unknown_keys_are_reported_and_never_drop_the_file() {
        let (tmp, _user, _parent, root) = b017_layers("unknown-key");
        write_ws(&root, "definitely_not_a_key = 1\ngame_target = \"MP4\"\n");
        let (cfg, issues) = LspConfig::from_layers(None, None, Some(&root));
        assert_eq!(cfg.game_target, "MP4");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].key, "definitely_not_a_key");
        assert!(
            issues[0].found.contains("unknown"),
            "issue should say the key is unknown: {:?}",
            issues[0].found
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// B024 must not regress: `defines` ACCUMULATE across layers rather than
    /// obeying the last-file-wins ladder, and `-NAME` still removes.
    #[test]
    fn defines_still_accumulate_across_every_layer_after_the_ladder_flip() {
        let (tmp, user, parent, root) = b017_layers("defines-accumulate");
        std::fs::write(&user, "defines = [\"FROM_USER\"]\n").unwrap();
        write_ws(&parent, "defines = [\"FROM_PARENT\"]\n");
        write_ws(&root, "defines = [\"FROM_ROOT\", \"-FROM_USER\"]\n");
        let (mut cfg, _) = LspConfig::from_layers(Some(&user), Some(&parent), Some(&root));
        cfg.apply_define_overrides();
        assert!(cfg.defines.contains("FROM_PARENT"), "{:?}", cfg.defines);
        assert!(cfg.defines.contains("FROM_ROOT"), "{:?}", cfg.defines);
        assert!(
            !cfg.defines.contains("FROM_USER"),
            "an explicit removal in a higher layer still wins: {:?}",
            cfg.defines
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A malformed replacement list is an unusable KEY, not a partially
    /// usable value. Every ordinary string-list key therefore falls through
    /// to the lower layer as a unit. Empty arrays remain valid, explicit
    /// values; `defines` is the B024 exception whose valid layers accumulate.
    #[test]
    fn strict_string_list_keys_fall_through_as_a_unit_and_accept_empty() {
        struct Case {
            key: &'static str,
            lower: &'static str,
            mixed: &'static str,
            all_invalid: &'static str,
            empty: &'static str,
            render: fn(&LspConfig) -> String,
            lower_rendered: &'static str,
            empty_rendered: &'static str,
        }

        let cases = [
            Case {
                key: "source_paths",
                lower: "source_paths = [\"lower\"]",
                mixed: "source_paths = [\"partial\", 7]",
                all_invalid: "source_paths = [7, true]",
                empty: "source_paths = []",
                render: |c| format!("{:?}", c.source_paths),
                lower_rendered: "Some([\"lower\"])",
                empty_rendered: "Some([])",
            },
            Case {
                key: "ignore_paths",
                lower: "ignore_paths = [\"lower\"]",
                mixed: "ignore_paths = [\"partial\", 7]",
                all_invalid: "ignore_paths = [7, true]",
                empty: "ignore_paths = []",
                render: |c| format!("{:?}", c.ignore_paths),
                lower_rendered: "Some([\"lower\"])",
                empty_rendered: "Some([])",
            },
            Case {
                key: "shortcut_namespaces",
                lower: "shortcut_namespaces = [\"Lower\"]",
                mixed: "shortcut_namespaces = [\"Partial\", 7]",
                all_invalid: "shortcut_namespaces = [7, true]",
                empty: "shortcut_namespaces = []",
                render: |c| format!("{:?}", c.shortcut_namespaces),
                lower_rendered: "Some([\"Lower\"])",
                empty_rendered: "Some([])",
            },
            Case {
                key: "define_matrix",
                lower: "define_matrix = [\"LOWER\"]",
                mixed: "define_matrix = [\"PARTIAL\", 7]",
                all_invalid: "define_matrix = [7, true]",
                empty: "define_matrix = []",
                render: |c| format!("{:?}", c.define_matrix),
                lower_rendered: "[\"LOWER\"]",
                empty_rendered: "[]",
            },
            Case {
                key: "defines",
                lower: "defines = [\"LOWER\"]",
                mixed: "defines = [\"PARTIAL\", 7]",
                all_invalid: "defines = [7, true]",
                empty: "defines = []",
                render: |c| format!("{:?}", c.define_overrides),
                lower_rendered: "[\"LOWER\"]",
                // B024: a valid empty higher layer contributes nothing; it
                // does not erase accumulated lower-layer overrides.
                empty_rendered: "[\"LOWER\"]",
            },
        ];

        for case in cases {
            let (tmp, user, _parent, root) = b017_layers(case.key);
            std::fs::write(&user, format!("{}\n", case.lower)).unwrap();

            for (variant, higher) in [("mixed", case.mixed), ("all-invalid", case.all_invalid)] {
                write_ws(&root, &format!("{higher}\n"));
                let (cfg, issues) = LspConfig::from_layers(Some(&user), None, Some(&root));
                assert_eq!(
                    (case.render)(&cfg),
                    case.lower_rendered,
                    "{} {variant} must fall through as one bad key",
                    case.key
                );
                assert!(
                    issues.iter().any(|issue| issue.key == case.key),
                    "{} {variant} must report its unusable key: {issues:?}",
                    case.key
                );
            }

            write_ws(&root, &format!("{}\n", case.empty));
            let (cfg, issues) = LspConfig::from_layers(Some(&user), None, Some(&root));
            assert_eq!(
                (case.render)(&cfg),
                case.empty_rendered,
                "{} empty array is a valid explicit value",
                case.key
            );
            assert!(issues.is_empty(), "{} empty array: {issues:?}", case.key);
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    /// String-shaped selector values can still be semantically unusable.
    /// They are bad keys at their file layer (with attribution), so each
    /// selector falls through independently and valid siblings still apply.
    #[test]
    fn semantic_selector_errors_report_and_fall_through_per_key() {
        let (tmp, user, _parent, root) = b017_layers("semantic-selectors");
        std::fs::write(
            &user,
            "signature_mode = \"official\"\nos_target = \"linux\"\narch_target = \"32\"\n",
        )
        .unwrap();

        let variants = [
            (
                "unknown-values",
                "signature_mode = \"banana-sig\"\nos_target = \"beos\"\narch_target = \"128\"\ngame_target = \"TURBO\"\n",
                ["banana-sig", "beos", "128"],
            ),
            (
                "wrong-scalars",
                "signature_mode = 7\nos_target = true\narch_target = [\"32\"]\ngame_target = \"TURBO\"\n",
                ["integer", "boolean", "array"],
            ),
        ];

        for (name, body, found_fragments) in variants {
            write_ws(&root, body);
            let (cfg, issues) = LspConfig::from_layers(Some(&user), None, Some(&root));
            assert_eq!(cfg.signature_mode, SignatureMode::Official, "{name}");
            assert_eq!(cfg.os_target, OsTarget::Linux, "{name}");
            assert_eq!(cfg.arch_target, ArchTarget::Arch32, "{name}");
            assert_eq!(
                cfg.game_target, "TURBO",
                "{name}: a valid sibling must still apply"
            );

            for (key, found) in ["signature_mode", "os_target", "arch_target"]
                .into_iter()
                .zip(found_fragments)
            {
                let issue = issues
                    .iter()
                    .find(|issue| issue.key == key)
                    .unwrap_or_else(|| panic!("{name}: missing issue for {key}: {issues:?}"));
                assert_eq!(issue.file, root.join(".openplanet-lsp.toml"));
                assert!(
                    issue.expected.contains("one of"),
                    "{name} {key}: expected choices, got {issue:?}"
                );
                assert!(
                    issue.found.contains(found),
                    "{name} {key}: expected found value/type {found:?}, got {issue:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `unused_callbacks` keeps its pre-B017 legacy tolerance: usable names
    /// survive a mixed list. An all-unusable higher list is still a bad key
    /// and falls through; an explicit empty list wins outright.
    #[test]
    fn unused_callbacks_mixed_all_invalid_and_empty_keep_legacy_contract() {
        let (tmp, user, _parent, root) = b017_layers("callbacks-list-contract");
        std::fs::write(&user, "unused_callbacks = [\"Lower\"]\n").unwrap();

        let variants = [
            (
                "mixed",
                "unused_callbacks = [\"Kept\", 7, true]\n",
                vec!["Kept".to_string()],
                true,
            ),
            (
                "all-invalid",
                "unused_callbacks = [7, true]\n",
                vec!["Lower".to_string()],
                true,
            ),
            ("empty", "unused_callbacks = []\n", vec![], false),
        ];
        for (name, body, expected, has_issue) in variants {
            write_ws(&root, body);
            let (cfg, issues) = LspConfig::from_layers(Some(&user), None, Some(&root));
            assert_eq!(cfg.unused_callbacks, expected, "case {name}");
            assert_eq!(
                issues.iter().any(|issue| issue.key == "unused_callbacks"),
                has_issue,
                "case {name}: {issues:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The second ladder (`UserPrefs`) obeys the same order, and a bad key
    /// no longer discards its siblings either.
    #[test]
    fn user_prefs_workspace_beats_user_global_and_survives_a_bad_key() {
        let (tmp, user, _parent, root) = b017_layers("user-prefs");
        std::fs::write(&user, "default_mode = \"lsp\"\nwarnings_as_errors = true\n").unwrap();
        write_ws(
            &root,
            "default_mode = \"tui\"\nwarnings_as_errors = false\n",
        );
        let prefs = UserPrefs::from_layers(Some(&user), Some(&root));
        assert_eq!(prefs.default_mode, DefaultMode::Tui);
        assert!(!prefs.warnings_as_errors);

        // User global applies when the workspace file is silent.
        write_ws(&root, "game_target = \"MP4\"\n");
        let prefs = UserPrefs::from_layers(Some(&user), Some(&root));
        assert_eq!(prefs.default_mode, DefaultMode::Lsp);
        assert!(prefs.warnings_as_errors);

        // One bad key must not discard the sibling in the same file.
        write_ws(&root, "game_target = 7\nwarnings_as_errors = false\n");
        let prefs = UserPrefs::from_layers(Some(&user), Some(&root));
        assert!(
            !prefs.warnings_as_errors,
            "a bad `game_target` must not take `warnings_as_errors` with it"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The bare-TTY/UserPrefs-only loader must diagnose semantic and scalar
    /// `default_mode` errors itself: there may be no later LspConfig load to
    /// report them. This pure seam avoids mutating process-global HOME.
    #[test]
    fn user_prefs_reports_default_mode_issues_and_keeps_valid_siblings() {
        let (tmp, user, _parent, root) = b017_layers("user-prefs-issues");
        std::fs::write(&user, "default_mode = \"lsp\"\nwarnings_as_errors = true\n").unwrap();

        for (name, bad_mode, found) in [
            ("unknown-value", "default_mode = \"graphical\"", "graphical"),
            ("wrong-scalar", "default_mode = 7", "integer"),
        ] {
            write_ws(&root, &format!("{bad_mode}\nwarnings_as_errors = false\n"));
            let (prefs, issues) = UserPrefs::from_layers_with_issues(Some(&user), Some(&root));
            assert_eq!(
                prefs.default_mode,
                DefaultMode::Lsp,
                "{name}: invalid higher key must fall through"
            );
            assert!(
                !prefs.warnings_as_errors,
                "{name}: valid sibling must still apply"
            );
            let issue = issues
                .iter()
                .find(|issue| issue.key == "default_mode")
                .unwrap_or_else(|| panic!("{name}: missing default_mode issue: {issues:?}"));
            assert_eq!(issue.file, root.join(".openplanet-lsp.toml"));
            assert!(issue.expected.contains("one of"), "{name}: {issue:?}");
            assert!(issue.found.contains(found), "{name}: {issue:?}");

            let mut stderr = Vec::new();
            write_config_issues(&mut stderr, &issues).unwrap();
            let stderr = String::from_utf8(stderr).unwrap();
            assert!(stderr.contains("warning: config:"), "{name}: {stderr:?}");
            assert!(stderr.contains("default_mode"), "{name}: {stderr:?}");
            assert!(
                stderr.contains(".openplanet-lsp.toml"),
                "{name}: {stderr:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The known-key catalog must cover every key the tool itself writes —
    /// otherwise openplanet-lsp would warn about its own config files.
    #[test]
    fn config_key_catalog_covers_every_settings_key() {
        for spec in crate::user_config::SETTINGS {
            assert!(
                CONFIG_KEYS.contains(&spec.key),
                "SETTINGS key `{}` missing from CONFIG_KEYS",
                spec.key
            );
        }
        // Written by the TUI first-run prompt (src/tui/first_run.rs), never
        // read here — but it lives in the same file, so it must not warn.
        assert!(CONFIG_KEYS.contains(&"first_run_prompt_done"));
        let mut sorted = CONFIG_KEYS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), CONFIG_KEYS.len(), "duplicate CONFIG_KEYS");
    }
}
