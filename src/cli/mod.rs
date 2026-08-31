//! CLI `check` command: workspace diagnostics + human-readable reports.
//!
//! Pretty layout rules live in [`pretty`] (provisional — issue #12 / map #8).

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::config::{
    ArchTarget, ErrorGroup, LspConfig, OsTarget, SignatureMode, UnusedLevel, UserPrefs,
};
use crate::server::diagnostics;
use crate::typedb::catalog::VersionCatalog;
use crate::typedb::TypeIndex;
use crate::workspace::load::{load_plugin_workspace, DependencySearch};
use crate::workspace::manifest::ManifestWorkspace;
use crate::workspace::project;

/// GH #59: `typedb status|diff|refresh` — the type-DB freshness pipeline.
pub mod config_cmd;
/// GH #57: define-matrix combo expansion, dedup, and labeling.
pub mod matrix;
mod pretty;
pub mod typedb_cmd;
pub mod watch;

const CHECK_HELP: &str = "\
openplanet-lsp check - Run workspace diagnostics for an Openplanet plugin

USAGE:
    openplanet-lsp check [OPTIONS] [PATH]
    openplanet-lsp check --watch [OPTIONS] [PATH]

ARGS:
    PATH                Plugin root or a file inside a plugin (walks up for
                        info.toml). Defaults to the current directory.

OPTIONS:
    -h, --help          Show this help message
    --watch             Live TUI: re-check on file changes (q quit · j/k · PgUp/PgDn · g/G · c density · r refresh)
    --format FMT        One-shot output only: plain | pretty | json | auto (default: auto)
                        auto → pretty when stdout is color-capable, else plain
                        plain → gcc-style path:line:col: severity: message
                        pretty → source excerpt + carets
                        json → machine-readable report; each diagnostic has a
                               combo field (define combos it fired in)
                        Ignored with --watch (TUI owns presentation)
    --warnings-as-errors
                        Exit 1 when any WARNING diagnostic is present
                        (short form -Werror; =true/=false also accepted).
                        May also be pinned via `warnings_as_errors = true`
                        in .openplanet-lsp.toml or the user config
                        (~/.config/openplanet-lsp/config.toml); workspace
                        wins over user. An explicit =false flag overrides
                        a config `true`.
                        Default exit code: 0 = clean or warnings-only,
                        1 = any error, 2 = usage failure
    --game-target NAME  Which game's platform defines are live for this run:
                        TMNEXT (default) | MP4 | MP40 | MP41 | TURBO | MP3 |
                        UNITED | UNITED_FOREVER | NATIONS_FOREVER. Case
                        -insensitive. Also picks the typedb variant and the
                        Openplanet install folder. Same key as
                        `game_target` in .openplanet-lsp.toml; the flag wins.
    --signature MODE    Signature level whose SIG_* defines are live:
                        developer (default) | school | regular | official.
                        Levels are cumulative — official defines
                        SIG_OFFICIAL, developer defines all four — so this
                        checks the plugin the way it compiles once signed.
                        Aliases: dev, reg, and the SIG_* token itself. Same
                        key as `signature_mode` in .openplanet-lsp.toml;
                        the flag wins.
    --os OS             Host OS whose OS defines are live: windows
                        (default) | wine | linux. Wine defines BOTH
                        WINDOWS and WINDOWS_WINE (as the game does under
                        Wine); linux defines LINUX. Exactly one OS is
                        live, as in a real process. Same key as `os_target`
                        in .openplanet-lsp.toml; the flag wins.
    --arch ARCH         Game architecture whose MANIA* define is live:
                        64 (default) | 32 — never both. Same key as
                        `arch_target` in .openplanet-lsp.toml; the flag
                        wins.
    --developer-build / --no-developer-build
    --has-dev / --no-has-dev
    --logs / --no-logs
    --server / --no-server
                        Build-shape defines (DEVELOPER, HAS_DEV, LOGS,
                        SERVER), all ON by default; the negative spelling
                        narrows the run toward a real client profile
                        (a client has none of them). Same keys in
                        .openplanet-lsp.toml; the flags win.
    --define NAME       Add a preprocessor define on top of info.toml /
                        config (short form -D; repeatable). Use to check
                        branches the staged build enables but the source
                        tree doesn't (e.g. -D DEV for #if DEV test files).
                        Any --define collapses the define matrix
                        (define_matrix in .openplanet-lsp.toml) back to a
                        single-combo run.
    --typedb-dir DIR    Load OpenplanetCore.json and OpenplanetNext.json from DIR
                        (plus Openplanet.h, if present)
    --openplanet-version V[,V...]
                        Check against these Openplanet versions instead of the
                        installed one. Each V must be the installed version or
                        a snapshot under --typedb-versions-dir; with several,
                        diagnostics are labeled with the versions they fire on.
    --typedb-versions-dir DIR
                        Root of recorded typedb snapshots (one subdirectory
                        per version, each holding OpenplanetCore.json and
                        OpenplanetNext.json)
    --no-typedb         Run without Openplanet/Nadeo type database files
    --no-gitignore      Check .as files that .gitignore excludes. Discovery
                        honors .gitignore at or below the plugin root by
                        default; use this (or `honor_gitignore = false` in
                        .openplanet-lsp.toml) when gitignored files are still
                        compile-critical, e.g. generated sources. The flag
                        wins over both config levels.
    --unused MODE       Unused functions/vars/etc:
                        ignore | hint | info | warn | error (aliases:
                        off/false → ignore, grey/faded → hint,
                        note/information → info, on/true → warn,
                        err → error). Default warn. `hint` and `info` report
                        the same findings at a quieter severity and never
                        fail the run, not even under --warnings-as-errors.
                        Also `unused = \"ignore\"|\"hint\"|\"info\"|\"warn\"|
                        \"error\"` in .openplanet-lsp.toml or the user config;
                        workspace wins over user. The flag wins over both.
                        `--no-unused` is `--unused ignore`.
    --no-unused         Same as `--unused ignore`
    --errors MODE       Group errors in human check output: last | first | off
                        (aliases: end → last, start/begin → first,
                        none/false/interleaved → off). Default last:
                        warnings/info/hints first, then errors. JSON is
                        never regrouped. Also `errors = \"last\"|\"first\"|\"off\"`
                        in .openplanet-lsp.toml or the user config;
                        workspace wins over user. The flag wins over both.
    --plugins-dir DIR   Directory to search for plugin dependencies
                        (may be specified multiple times)
    --plugin-files-search-path DIR
                        Additional relative search root for plugin export files
                        (may be specified multiple times; defaults to: src)

EXAMPLES:
    openplanet-lsp check ~/plugins/tm-agent
    openplanet-lsp check --format pretty ~/plugins/tm-agent
    openplanet-lsp check --watch .
    openplanet-lsp check --typedb-dir /path/to/typedb --plugins-dir ~/openplanet/my-plugins .


";

/// How `check` should render diagnostics on stdout.
///
/// Provisional (issue #12): `Auto` picks pretty when the color capability bit
/// is on (`color_stdout()`-equivalent), otherwise plain gcc-style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CheckFormat {
    #[default]
    Auto,
    Plain,
    Pretty,
    /// GH #57: machine-readable JSON (one object: root, type_database_loaded,
    /// diagnostics[]). Each diagnostic carries a "combo" array — the define
    /// combos it fired in (empty = single-combo run).
    Json,
}

impl CheckFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "plain" => Ok(Self::Plain),
            "pretty" => Ok(Self::Pretty),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "unknown --format value '{other}' (expected plain, pretty, json, or auto)"
            )),
        }
    }

    /// Whether the pretty layout should be used given a color-capability bit.
    pub fn use_pretty(self, color_capable: bool) -> bool {
        match self {
            Self::Plain | Self::Json => false,
            Self::Pretty => true,
            Self::Auto => color_capable,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub path: PathBuf,
    pub typedb_dir: Option<PathBuf>,
    pub no_typedb: bool,
    /// GH #18: target Openplanet version(s), comma-separated. Raw string;
    /// resolution against the version catalog happens in `run_check`.
    pub openplanet_version: Option<String>,
    /// GH #18: root of recorded typedb snapshots
    /// (`tests/fixtures/typedb-versions`-shaped).
    pub typedb_versions_dir: Option<PathBuf>,
    pub plugins_dirs: Vec<PathBuf>,
    pub plugin_files_search_paths: Vec<PathBuf>,
    pub format: CheckFormat,
    /// Live TUI mode (`check --watch`).
    pub watch: bool,
    /// GH #48: treat WARNING diagnostics as errors for the exit code
    /// (`--warnings-as-errors` / `-Werror`, =true/=false accepted).
    /// `None` = flag absent → falls back to the config ladder
    /// (`warnings_as_errors` in workspace `.openplanet-lsp.toml`, then the
    /// user config). An explicit `Some(false)` overrides config `true`.
    /// See [`CheckOptions::resolve_warnings_as_errors`]. Default exit
    /// contract stays: 0 = clean or warnings-only, 1 = any error,
    /// 2 = usage failure.
    pub warnings_as_errors_flag: Option<bool>,
    /// GH #53: extra preprocessor defines injected on top of the
    /// manifest/config ladder (`--define NAME`, repeatable). Lets a
    /// source-tree gate match the staged/dev compile (e.g. `build.sh dev`
    /// writes `defines = ["DEV"]` only into the staged plugin).
    pub extra_defines: Vec<String>,
    /// GH #60: `--no-gitignore` — discover `.as` files that `.gitignore`
    /// excludes. Wins over `honor_gitignore` at both config levels. There is
    /// no positive flag: honoring is already the default.
    pub no_gitignore: bool,
    /// `--unused ignore|warn|error` / `--no-unused`. `None` = flag absent
    /// → config ladder (`unused` in workspace then user config, default
    /// warn). An explicit flag wins over both.
    pub unused_flag: Option<UnusedLevel>,
    /// GH #64: `--game-target TMNEXT|MP4|…` for one run. `None` = flag
    /// absent → config ladder (`game_target`). Stored normalized to the
    /// upper-case token the config file uses, and fed to `LspConfig::load`
    /// as an init option so the define set AND the GH #45 variant-folder
    /// re-probe (typedb / game JSON) both follow it.
    pub game_target_flag: Option<String>,
    /// GH #64: `--signature developer|school|regular|official` for one run.
    /// `None` = flag absent → config ladder (`signature_mode`).
    pub signature_mode_flag: Option<SignatureMode>,
    /// B028: `--os windows|wine|linux` for one run. `None` = flag absent →
    /// config ladder (`os_target`). Wine defines WINDOWS + WINDOWS_WINE
    /// together, matching the live game (probe 2026-08-29).
    pub os_flag: Option<OsTarget>,
    /// B028: `--arch 64|32` for one run. `None` = flag absent → config
    /// ladder (`arch_target`).
    pub arch_flag: Option<ArchTarget>,
    /// B028: build-shape flag selectors (`--developer-build` /
    /// `--no-developer-build`, …). `None` = flag absent → config ladder,
    /// default true (decision (a)).
    pub developer_build_flag: Option<bool>,
    pub has_dev_flag: Option<bool>,
    pub logs_flag: Option<bool>,
    pub server_flag: Option<bool>,
    /// `--errors last|first|off`. `None` = flag absent → config ladder
    /// (`errors` in workspace then user config, default last). An explicit
    /// flag wins over both. Human formats only; JSON stays analysis order.
    pub errors_flag: Option<ErrorGroup>,
}

impl CheckOptions {
    /// Effective `warnings_as_errors` for the run's exit code, resolving
    /// the precedence CLI flag > workspace `.openplanet-lsp.toml` > user
    /// global config > default(false) (GH #48 follow-up).
    ///
    /// The flag is tracked as `Option<bool>` so an explicit
    /// `--warnings-as-errors=false` can override a config `true`; a bare
    /// `--warnings-as-errors` / `-Werror` / `=true` is `Some(true)`.
    pub fn resolve_warnings_as_errors(&self, prefs: &UserPrefs) -> bool {
        self.warnings_as_errors_flag
            .unwrap_or(prefs.warnings_as_errors)
    }

    /// [`Self::resolve_warnings_as_errors`] with the config ladder loaded
    /// from the resolved plugin root (walks up from `options.path` for
    /// info.toml, exactly like the rest of `run_check`). The user-global
    /// layer reads `$HOME/.config/openplanet-lsp/config.toml`.
    pub fn resolve_warnings_as_errors_with_config(&self) -> bool {
        let workspace_root = project::find_workspace_root(&self.path);
        // `run_check` immediately loads the full LspConfig, whose shared
        // reader owns reporting these same issues for the check invocation.
        let prefs = UserPrefs::load_silent(workspace_root.as_deref());
        self.resolve_warnings_as_errors(&prefs)
    }

    /// Effective error-grouping for human `check` output: CLI flag >
    /// workspace / user `errors` key > default last.
    pub fn resolve_errors(&self, config: &LspConfig) -> ErrorGroup {
        self.errors_flag.unwrap_or(config.errors)
    }
}

/// GH #59 / B064: the build stamps of a loaded type database.
///
/// Re-exported from [`crate::typedb`], where it lives beside the loader that
/// produces it. Moved there by B064: the stamps have to come out of the SAME
/// read that produced the index, so they belong to the index rather than to
/// the reporting layer that displays them.
pub use crate::typedb::TypeDbVersions;

#[derive(Debug)]
pub struct CheckReport {
    pub root: PathBuf,
    pub diagnostics: Vec<CliDiagnostic>,
    pub type_database_loaded: bool,
    /// GH #59: stamps of the loaded DB. `None` when no DB was loaded
    /// (`--no-typedb`, or nothing resolved).
    pub type_db: Option<TypeDbVersions>,
    /// Grouping applied to human (plain/pretty) output only. JSON keeps
    /// `diagnostics` in analysis order regardless of this field.
    pub errors: ErrorGroup,
    /// GH #18: labels of the resolved target versions, in request order.
    /// Empty on a legacy single-target run (no `--openplanet-version` /
    /// `--typedb-versions-dir`), so existing output stays byte-identical.
    pub targets: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CliDiagnostic {
    pub path: PathBuf,
    /// GH #57: the define combos this diagnostic fired in (each combo is a
    /// sorted define list; `[]` = the base combo). Empty = single-combo
    /// (non-matrix) run — formatters render no label in that case.
    pub combos: Vec<Vec<String>>,
    /// GH #18: the target Openplanet versions this diagnostic fired on.
    /// Empty on a single-target run, and on a multi-target run when it
    /// fired on EVERY target (firing everywhere is not version news) —
    /// formatters render no label in either case.
    pub versions: Vec<String>,
    pub diagnostic: Diagnostic,
}

#[derive(Debug)]
pub enum CliError {
    Usage(String),
    Check(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(msg) | CliError::Check(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CliError {}

/// Read `--flag VALUE` / `--flag=VALUE` at `args[i]`.
///
/// A separated value starting with `--` is a usage error (same policy as
/// `typedb_cmd::take_dir_arg`). When `allow_option_like_equals` is false,
/// `--flag=--x` is rejected too (`--define` / `--format`).
fn take_value_arg<'a>(
    args: &'a [String],
    i: usize,
    flag: &str,
    missing: &str,
    allow_option_like_equals: bool,
) -> Result<(&'a str, usize), CliError> {
    if let Some(value) = args[i].strip_prefix(&format!("{flag}=")) {
        if value.is_empty() {
            return Err(CliError::Usage(missing.to_string()));
        }
        if !allow_option_like_equals && value.starts_with("--") {
            return Err(CliError::Usage(missing.to_string()));
        }
        return Ok((value, 1));
    }
    let value = args
        .get(i + 1)
        .ok_or_else(|| CliError::Usage(missing.to_string()))?;
    if value.starts_with("--") {
        return Err(CliError::Usage(missing.to_string()));
    }
    Ok((value.as_str(), 2))
}

pub fn parse_check_args(args: &[String]) -> Result<CheckOptions, CliError> {
    let mut options = CheckOptions::default();
    options.plugin_files_search_paths = vec![PathBuf::from("src")];
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{}", CHECK_HELP);
                std::process::exit(0);
            }
            "--no-typedb" => {
                options.no_typedb = true;
                i += 1;
            }
            "--no-gitignore" => {
                options.no_gitignore = true;
                i += 1;
            }
            "--no-unused" => {
                options.unused_flag = Some(UnusedLevel::Ignore);
                i += 1;
            }
            _ if arg == "--unused" || arg.starts_with("--unused=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--unused",
                    "--unused requires a value (ignore, hint, info, warn, or error)",
                    true,
                )?;
                options.unused_flag = Some(UnusedLevel::parse(value).ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown --unused value '{value}' (expected ignore, hint, info, warn, or error)"
                    ))
                })?);
                i += used;
            }
            _ if arg == "--errors" || arg.starts_with("--errors=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--errors",
                    "--errors requires a value (last, first, or off)",
                    true,
                )?;
                options.errors_flag = Some(ErrorGroup::parse(value).ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown --errors value '{value}' (expected last, first, or off)"
                    ))
                })?);
                i += used;
            }
            _ if arg == "--game-target" || arg.starts_with("--game-target=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--game-target",
                    "--game-target requires a value (TMNEXT, MP4, TURBO, …)",
                    false,
                )?;
                let normalized = value.trim().to_ascii_uppercase();
                if !LspConfig::GAME_TARGETS.contains(&normalized.as_str()) {
                    return Err(CliError::Usage(format!(
                        "unknown --game-target value '{value}' (expected {})",
                        LspConfig::GAME_TARGETS.join(", ")
                    )));
                }
                options.game_target_flag = Some(normalized);
                i += used;
            }
            _ if arg == "--signature" || arg.starts_with("--signature=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--signature",
                    "--signature requires a value (developer, school, regular, or official)",
                    false,
                )?;
                options.signature_mode_flag =
                    Some(SignatureMode::parse(value).ok_or_else(|| {
                        CliError::Usage(format!(
                            "unknown --signature value '{value}' (expected {})",
                            SignatureMode::CHOICES.join(", ")
                        ))
                    })?);
                i += used;
            }
            _ if arg == "--os" || arg.starts_with("--os=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--os",
                    "--os requires a value (windows, wine, or linux)",
                    false,
                )?;
                options.os_flag = Some(OsTarget::parse(value).ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown --os value '{value}' (expected {})",
                        OsTarget::CHOICES.join(", ")
                    ))
                })?);
                i += used;
            }
            _ if arg == "--arch" || arg.starts_with("--arch=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--arch",
                    "--arch requires a value (64 or 32)",
                    false,
                )?;
                options.arch_flag = Some(ArchTarget::parse(value).ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown --arch value '{value}' (expected {})",
                        ArchTarget::CHOICES.join(", ")
                    ))
                })?);
                i += used;
            }
            "--developer-build" => {
                options.developer_build_flag = Some(true);
                i += 1;
            }
            "--no-developer-build" => {
                options.developer_build_flag = Some(false);
                i += 1;
            }
            "--has-dev" => {
                options.has_dev_flag = Some(true);
                i += 1;
            }
            "--no-has-dev" => {
                options.has_dev_flag = Some(false);
                i += 1;
            }
            "--logs" => {
                options.logs_flag = Some(true);
                i += 1;
            }
            "--no-logs" => {
                options.logs_flag = Some(false);
                i += 1;
            }
            "--server" => {
                options.server_flag = Some(true);
                i += 1;
            }
            "--no-server" => {
                options.server_flag = Some(false);
                i += 1;
            }
            "--watch" => {
                options.watch = true;
                i += 1;
            }
            "--warnings-as-errors" | "-Werror" => {
                options.warnings_as_errors_flag = Some(true);
                i += 1;
            }
            _ if arg == "--define" || arg == "-D" || arg.starts_with("--define=") => {
                if arg == "--define=" {
                    return Err(CliError::Usage(
                        "--define requires a non-empty symbol name".to_string(),
                    ));
                }
                let flag = if arg == "-D" { "-D" } else { "--define" };
                let (value, used) = take_value_arg(
                    args,
                    i,
                    flag,
                    "--define requires a symbol name argument",
                    false,
                )?;
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--define requires a non-empty symbol name".to_string(),
                    ));
                }
                options.extra_defines.push(value.to_string());
                i += used;
            }
            _ if arg.starts_with("--warnings-as-errors=") => {
                let value = arg.trim_start_matches("--warnings-as-errors=");
                options.warnings_as_errors_flag = Some(match value {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    other => {
                        return Err(CliError::Usage(format!(
                            "--warnings-as-errors accepts true or false, got `{other}`"
                        )))
                    }
                });
                i += 1;
            }
            _ if arg == "--format" || arg.starts_with("--format=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--format",
                    "--format requires a value (plain, pretty, or auto)",
                    false,
                )?;
                options.format = CheckFormat::parse(value).map_err(CliError::Usage)?;
                i += used;
            }
            _ if arg == "--typedb-dir" || arg.starts_with("--typedb-dir=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--typedb-dir",
                    "--typedb-dir requires a directory argument",
                    true,
                )?;
                options.typedb_dir = Some(PathBuf::from(value));
                i += used;
            }
            _ if arg == "--openplanet-version" || arg.starts_with("--openplanet-version=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--openplanet-version",
                    "--openplanet-version requires a version argument",
                    true,
                )?;
                options.openplanet_version = Some(value.to_string());
                i += used;
            }
            _ if arg == "--typedb-versions-dir" || arg.starts_with("--typedb-versions-dir=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--typedb-versions-dir",
                    "--typedb-versions-dir requires a directory argument",
                    true,
                )?;
                options.typedb_versions_dir = Some(PathBuf::from(value));
                i += used;
            }
            _ if arg == "--plugins-dir" || arg.starts_with("--plugins-dir=") => {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--plugins-dir",
                    "--plugins-dir requires a directory argument",
                    true,
                )?;
                options.plugins_dirs.push(PathBuf::from(value));
                i += used;
            }
            _ if arg == "--plugin-files-search-path"
                || arg.starts_with("--plugin-files-search-path=") =>
            {
                let (value, used) = take_value_arg(
                    args,
                    i,
                    "--plugin-files-search-path",
                    "--plugin-files-search-path requires a directory argument",
                    true,
                )?;
                if options.plugin_files_search_paths == [PathBuf::from("src")] {
                    options.plugin_files_search_paths.clear();
                }
                options.plugin_files_search_paths.push(PathBuf::from(value));
                i += used;
            }
            _ if arg.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown check option: {arg}\nRun `openplanet-lsp check --help` for usage."
                )));
            }
            _ => {
                positional.push(PathBuf::from(arg));
                i += 1;
            }
        }
    }

    if options.no_typedb && options.typedb_dir.is_some() {
        return Err(CliError::Usage(
            "--no-typedb cannot be combined with --typedb-dir".to_string(),
        ));
    }
    if options.no_typedb && options.openplanet_version.is_some() {
        return Err(CliError::Usage(
            "--no-typedb cannot be combined with --openplanet-version".to_string(),
        ));
    }
    if options.no_typedb && options.typedb_versions_dir.is_some() {
        return Err(CliError::Usage(
            "--no-typedb cannot be combined with --typedb-versions-dir".to_string(),
        ));
    }

    match positional.as_slice() {
        [path] => {
            options.path = path.clone();
            Ok(options)
        }
        [] => {
            // Default to cwd; resolve_workspace_root / watch walk up for info.toml.
            options.path = PathBuf::from(".");
            Ok(options)
        }
        _ => Err(CliError::Usage(
            "check accepts at most one plugin path".to_string(),
        )),
    }
}

/// `initializationOptions`-shaped overrides for the run's selector flags
/// (GH #64). `None` when neither flag was given, so the config ladder is
/// untouched.
fn check_init_options(options: &CheckOptions) -> Option<serde_json::Value> {
    let mut map = serde_json::Map::new();
    // B019: the config layer decides whether `DEPENDENCY_<ID>` is defined, so
    // it has to resolve dependencies over the same dirs the workspace loader
    // will search. `--plugins-dir` is the one search input the config ladder
    // could not otherwise see; it rides the init-options layer like the other
    // CLI flags. `finalize_with_config` dedups, so search ORDER is unchanged:
    // these dirs are still passed first via `DependencySearch.plugins_dirs`.
    if !options.plugins_dirs.is_empty() {
        map.insert(
            "plugins_dirs".into(),
            serde_json::Value::from(
                options
                    .plugins_dirs
                    .iter()
                    .filter_map(|p| p.to_str().map(String::from))
                    .collect::<Vec<_>>(),
            ),
        );
    }
    if let Some(target) = &options.game_target_flag {
        map.insert(
            "game_target".into(),
            serde_json::Value::from(target.clone()),
        );
    }
    if let Some(mode) = options.signature_mode_flag {
        map.insert(
            "signature_mode".into(),
            serde_json::Value::from(mode.as_str()),
        );
    }
    if let Some(os) = options.os_flag {
        map.insert("os_target".into(), serde_json::Value::from(os.as_str()));
    }
    if let Some(arch) = options.arch_flag {
        map.insert("arch_target".into(), serde_json::Value::from(arch.as_str()));
    }
    if let Some(v) = options.developer_build_flag {
        map.insert("developer_build".into(), serde_json::Value::from(v));
    }
    if let Some(v) = options.has_dev_flag {
        map.insert("has_dev".into(), serde_json::Value::from(v));
    }
    if let Some(v) = options.logs_flag {
        map.insert("logs".into(), serde_json::Value::from(v));
    }
    if let Some(v) = options.server_flag {
        map.insert("server".into(), serde_json::Value::from(v));
    }
    (!map.is_empty()).then_some(serde_json::Value::Object(map))
}

pub fn run_check(options: &CheckOptions) -> Result<CheckReport, CliError> {
    let root = resolve_workspace_root(&options.path)?;
    let root = root
        .canonicalize()
        .map_err(|e| CliError::Check(format!("failed to resolve {}: {e}", root.display())))?;
    // GH #64: the two selector flags ride the init-options layer — the same
    // top-of-ladder path an editor uses — so `load` re-derives the define set
    // AND re-probes the variant folder (typedb / game JSON follow the target,
    // GH #45). Mutating `config.game_target` after the fact would do neither.
    let init_options = check_init_options(options);
    let mut config = LspConfig::load(Some(&root), init_options.as_ref());
    if let Some(level) = options.unused_flag {
        config.unused = level;
    }
    if let Some(group) = options.errors_flag {
        config.errors = group;
    }
    apply_typedb_dir(&mut config, options.typedb_dir.as_deref());

    // B026: the manifest is NOT re-applied here. `LspConfig::load` already
    // applied it for this same root (Layer 4) and then ran the user's
    // `defines` overrides (Layer 5) — re-applying it re-inserted every
    // manifest/dependency define AFTER a `-NAME` removal had dropped it, so
    // `check` and the LSP server disagreed. One seam owns the ladder.

    // GH #57: any --define flag collapses the define matrix — single-combo
    // behavior (the base set plus the CLI defines), exactly like pre-matrix
    // `check`.
    let combos: Vec<Vec<String>> = if options.extra_defines.is_empty() {
        matrix::expand_combinations(
            &config.define_matrix,
            &config.define_groups,
            config.define_combinations.as_deref(),
            config.define_matrix_max_combinations,
        )
        .map_err(CliError::Check)?
    } else {
        vec![Vec::new()]
    };
    let matrix_run = combos.len() > 1;

    // GH #53: CLI --define flags land on top of the manifest/config ladder
    // so a source-tree gate can match the staged/dev compile.
    for define in &options.extra_defines {
        config.defines.insert(define.clone());
    }

    // GH #18: one target per requested Openplanet version. Without either
    // version flag this is a single unlabeled target holding exactly the
    // index `load_type_index` resolves today, so legacy runs stay
    // byte-identical.
    struct Target {
        /// `None` on the legacy path; `Some(version)` when the catalog
        /// resolved it (a lone catalog target is still labeled in the
        /// report, but only multi-target runs label diagnostics).
        label: Option<String>,
        index: Option<TypeIndex>,
        /// GH #18: what other recorded versions know about symbols this
        /// target lacks. `None` when the catalog has nothing to diff
        /// against (legacy path, or a single-entry catalog).
        notes: Option<crate::typedb::catalog::VersionNotes>,
    }
    let targets: Vec<Target> = if options.openplanet_version.is_none()
        && options.typedb_versions_dir.is_none()
    {
        vec![Target {
            label: None,
            index: load_type_index(&config, options.no_typedb)?,
            notes: None,
        }]
    } else {
        let installed = config.core_json.as_deref().zip(config.game_json.as_deref());
        let catalog =
            VersionCatalog::build(installed, options.typedb_versions_dir.as_deref())
                .map_err(CliError::Check)?;
        let resolved = catalog
            .resolve_targets(options.openplanet_version.as_deref())
            .map_err(CliError::Check)?;
        if resolved.is_empty() {
            // A versions root but no request and no installed dump: the run
            // proceeds without a typedb, exactly like discovery finding
            // nothing today.
            vec![Target {
                label: None,
                index: None,
                notes: None,
            }]
        } else {
            // With a second version to diff against, missing-symbol
            // diagnostics can say WHEN the symbol appeared or vanished.
            let diffable = catalog.entries().len() > 1;
            resolved
                .into_iter()
                .map(|entry| {
                    let index = TypeIndex::load(&entry.core_json, &entry.game_json).map_err(
                        |e| CliError::Check(format!("failed to load type database: {e}")),
                    )?;
                    let notes = diffable
                        .then(|| catalog.notes_for_target(entry))
                        .transpose()
                        .map_err(CliError::Check)?
                        .filter(|n| !n.is_empty());
                    Ok(Target {
                        label: Some(entry.label.clone()),
                        index: Some(index),
                        notes,
                    })
                })
                .collect::<Result<Vec<_>, CliError>>()?
        }
    };
    let multi_target = targets.len() > 1;
    // GH #59: stamp the run with the DB build it judged the code against.
    // B064: out of the index, not out of a second read of the files — the
    // report has to name the database the diagnostics came from, and a re-read
    // names whatever is on disk when the run finishes instead. Multi-target
    // runs keep the first target's stamps here (back-compat single slot);
    // `targets` names every version.
    let type_db = targets
        .iter()
        .find_map(|t| t.index.as_ref())
        .map(|index| index.versions().clone());
    let type_database_loaded = targets.iter().any(|t| t.index.is_some());

    let mut search = DependencySearch {
        plugins_dirs: options.plugins_dirs.clone(),
        plugin_files_search_paths: options.plugin_files_search_paths.clone(),
        // Filled from config by finalize_with_config below.
        source_paths: None,
        ignore_paths: None,
        honor_gitignore: None,
    }
    .finalize_with_config(&config);
    // GH #60: the CLI flag is the top of the ladder — it beats a workspace or
    // user-global `honor_gitignore = true`.
    if options.no_gitignore {
        search.honor_gitignore = Some(false);
    }

    // Define-independent: file discovery + source read. Shared by every
    // combo (each combo re-parses with its own define set below).
    let load = load_plugin_workspace(&root, &search).map_err(CliError::Check)?;

    let manifest_path = root.join("info.toml");
    let mut cli_diagnostics = Vec::new();

    for combo in &combos {
        // Per-combo config: base define set + this combo's axis defines.
        // Combo defines never leak into the shared base config.
        let mut combo_config = config.clone();
        combo_config.defines.extend(combo.iter().cloned());

        // One snapshot per combo: `#if` masking is define-dependent, so
        // parsing + symbol pooling must re-run (GH #39 / GH #57). The
        // snapshot is version-independent, so targets share it (GH #18).
        let snapshot = AnalysisSnapshot::from_load(&load, &combo_config);
        for target in &targets {
            let pass_start = cli_diagnostics.len();
            let version = target
                .notes
                .as_ref()
                .zip(target.label.as_deref());
            collect_pass_diagnostics(
                &load,
                &snapshot,
                &combo_config,
                target.index.as_ref(),
                version,
                &manifest_path,
                &search.plugin_files_search_paths,
                &mut cli_diagnostics,
            );
            if matrix_run {
                for diag in &mut cli_diagnostics[pass_start..] {
                    diag.combos.push(combo.clone());
                }
            }
            if multi_target {
                let label = target
                    .label
                    .clone()
                    .expect("multi-target entries come from the catalog, which labels them");
                for diag in &mut cli_diagnostics[pass_start..] {
                    diag.versions.push(label.clone());
                }
            }
        }
    }

    // GH #57: a diagnostic firing in several combos is reported once,
    // labeled with every combo it fired in. GH #18: likewise across target
    // versions.
    let mut cli_diagnostics = matrix::merge_combo_diagnostics(cli_diagnostics);
    if multi_target {
        // Firing on EVERY target is not version news — drop the label so
        // only version-specific findings carry one.
        for diag in &mut cli_diagnostics {
            if diag.versions.len() == targets.len() {
                diag.versions.clear();
            }
        }
    }

    Ok(CheckReport {
        root: load.root,
        diagnostics: cli_diagnostics,
        type_database_loaded,
        type_db,
        errors: config.errors,
        targets: targets.iter().filter_map(|t| t.label.clone()).collect(),
    })
}

/// One full analysis pass for a single define combo (GH #57): missing-dep
/// errors, info.toml diagnostics, per-file diagnostics, and the
/// workspace-wide unused-declaration warnings.
fn collect_pass_diagnostics(
    load: &crate::workspace::load::PluginWorkspaceLoad,
    snapshot: &AnalysisSnapshot,
    config: &LspConfig,
    type_index: Option<&TypeIndex>,
    version: Option<(&crate::typedb::catalog::VersionNotes, &str)>,
    manifest_path: &Path,
    plugin_files_search_paths: &[PathBuf],
    cli_diagnostics: &mut Vec<CliDiagnostic>,
) {
    if !snapshot.missing_required_dependencies().is_empty() {
        for diagnostic in diagnostics::missing_required_dependency_diagnostics(
            snapshot.missing_required_dependencies(),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: manifest_path.to_path_buf(),
                combos: Vec::new(),
                versions: Vec::new(),
                diagnostic,
            });
        }
    }

    // B040: info.toml rules (missing `version`, `exports` pointing at files
    // that are not there, deprecated keys) reach `check` through the same
    // seam the LSP publishes from — the manifest decides whether the plugin
    // loads at all, so an Error here fails the run.
    if manifest_path.exists() {
        if let Ok(source) = std::fs::read_to_string(manifest_path) {
            let workspace = ManifestWorkspace {
                root: &load.root,
                plugin_files_search_paths,
            };
            for mut diagnostic in diagnostics::manifest_rule_diagnostics(&source, Some(&workspace))
            {
                diagnostic.range = diagnostics::range_to_utf8_columns(&source, diagnostic.range);
                cli_diagnostics.push(CliDiagnostic {
                    path: manifest_path.to_path_buf(),
                    combos: Vec::new(),
                    versions: Vec::new(),
                    diagnostic,
                });
            }
        }
    }

    // Unused-declaration warnings (workspace view; own-manifest export
    // files are exempt public API). ONE collect pass for the whole
    // workspace — a per-file collect here would be O(files²) check runs.
    // `unused = ignore` skips the pass entirely (it is the noisy one).
    let unused_usage = (config.unused != crate::config::UnusedLevel::Ignore)
        .then(|| crate::analysis::unused::WorkspaceUsage::collect(snapshot, type_index));

    for item in &load.files {
        if !item.report_diagnostics {
            continue;
        }

        let Ok(uri) = Url::from_file_path(&item.path) else {
            continue;
        };
        // Reuse the snapshot's per-file analysis instead of re-parsing.
        let analysis = snapshot
            .analysis_at_path(&item.path)
            .expect("snapshot contains every loaded file");
        // Own-module symbol view: strict export files' decls are
        // consumer-only (game parity 2026-08-23) — hide them from name
        // resolution unless we're checking the export file itself (its
        // own import decls must resolve within it).
        let table_for_check =
            snapshot.table_for_own_file(crate::analysis_snapshot::FileRef::Path(&item.path));
        for mut diagnostic in diagnostics::compute_diagnostics_with_versions(
            &uri,
            analysis,
            config,
            type_index,
            &table_for_check,
            version,
        ) {
            diagnostic.range =
                diagnostics::range_to_utf8_columns(&analysis.source, diagnostic.range);
            cli_diagnostics.push(CliDiagnostic {
                path: item.path.clone(),
                combos: Vec::new(),
                versions: Vec::new(),
                diagnostic,
            });
        }
        if let (Some(file_id), Some(usage)) =
            (snapshot.file_id_of_path(&item.path), unused_usage.as_ref())
        {
            for mut diagnostic in diagnostics::unused_declaration_diagnostics(
                analysis.source.as_str(),
                usage,
                snapshot,
                file_id,
                snapshot.export_paths(),
                config.unused,
            ) {
                diagnostic.range =
                    diagnostics::range_to_utf8_columns(&analysis.source, diagnostic.range);
                cli_diagnostics.push(CliDiagnostic {
                    path: item.path.clone(),
                    combos: Vec::new(),
                    versions: Vec::new(),
                    diagnostic,
                });
            }
        }
    }
}

/// Format a check report using env/TTY color and [`CheckFormat::Auto`].
pub fn format_check_report(report: &CheckReport) -> String {
    format_check_report_with(report, crate::term::color_stdout(), CheckFormat::Auto)
}

/// Format a check report with an explicit [`CheckFormat`] (CLI entry).
pub fn format_check_report_for(report: &CheckReport, format: CheckFormat) -> String {
    format_check_report_with(report, crate::term::color_stdout(), format)
}

/// Format check diagnostics; `color` forces ANSI on/off (tests / screenshots).
///
/// `format` selects plain gcc-style vs pretty excerpts. See [`CheckFormat::use_pretty`].
///
/// Both renderers end with the Openplanet-mismatch ask: a dim trailing line
/// inviting reports whenever the game's compile errors/warnings differ from
/// this output (GH #45 follow-up).
pub fn format_check_report_with(report: &CheckReport, color: bool, format: CheckFormat) -> String {
    // GH #57: JSON is a data surface — no color, no human trailer,
    // and no error regrouping (analysis order is the contract).
    if format == CheckFormat::Json {
        return format_json(report);
    }
    let grouped = report_for_human_format(report);
    let body = if format.use_pretty(color) {
        pretty::format_pretty(&grouped, color)
    } else {
        format_plain(&grouped, color)
    };
    format!("{body}{}\n", mismatch_ask(color))
}

fn is_error_diagnostic(item: &CliDiagnostic) -> bool {
    // Missing severity is treated as an error (same as format_plain).
    matches!(
        item.diagnostic.severity,
        Some(DiagnosticSeverity::ERROR) | None
    )
}

/// Clone + regroup for human formats. Relative order inside each bucket
/// is the original analysis order.
fn group_diagnostics(diagnostics: &[CliDiagnostic], group: ErrorGroup) -> Vec<CliDiagnostic> {
    match group {
        ErrorGroup::Off => diagnostics.to_vec(),
        ErrorGroup::Last => {
            let mut out: Vec<_> = diagnostics
                .iter()
                .filter(|d| !is_error_diagnostic(d))
                .cloned()
                .collect();
            out.extend(
                diagnostics
                    .iter()
                    .filter(|d| is_error_diagnostic(d))
                    .cloned(),
            );
            out
        }
        ErrorGroup::First => {
            let mut out: Vec<_> = diagnostics
                .iter()
                .filter(|d| is_error_diagnostic(d))
                .cloned()
                .collect();
            out.extend(
                diagnostics
                    .iter()
                    .filter(|d| !is_error_diagnostic(d))
                    .cloned(),
            );
            out
        }
    }
}

fn report_for_human_format(report: &CheckReport) -> CheckReport {
    CheckReport {
        root: report.root.clone(),
        diagnostics: group_diagnostics(&report.diagnostics, report.errors),
        type_database_loaded: report.type_database_loaded,
        type_db: report.type_db.clone(),
        errors: report.errors,
        targets: report.targets.clone(),
    }
}

/// Issue tracker, shared with the top-level help/version surfaces (main.rs).
pub const ISSUE_URL: &str = "https://github.com/clankercode/lsp-openplanet/issues";

/// Trailer line appended to every one-shot `check` report (plain and pretty):
/// did Openplanet itself have different compile errors or warnings than this
/// output? False negatives (missing diagnostics, e.g. GH #28) count as
/// mismatches too, so the ask prints on clean runs as well.
fn mismatch_ask(color: bool) -> String {
    format!(
        "\n{} Did Openplanet have different compile errors or warnings compared to openplanet-lsp output? Please log an issue: {}",
        crate::term::dim(color, "›"),
        ISSUE_URL
    )
}

/// GH #57: machine-readable report. Each diagnostic carries a "combo"
/// array (the define combos it fired in — the base combo is `[]`, empty
/// array = single-combo run).
fn format_json(report: &CheckReport) -> String {
    let diagnostics: Vec<serde_json::Value> = report
        .diagnostics
        .iter()
        .map(|item| {
            let rel = item.path.strip_prefix(&report.root).unwrap_or(&item.path);
            let mut value = serde_json::json!({
                "path": rel.display().to_string(),
                "line": item.diagnostic.range.start.line + 1,
                "col": item.diagnostic.range.start.character + 1,
                "severity": severity_label(item.diagnostic.severity),
                "message": item.diagnostic.message,
                "combo": item.combos,
            });
            // GH #18: additive — absent unless version-specific.
            if !item.versions.is_empty() {
                value.as_object_mut().expect("literal object").insert(
                    "versions".to_string(),
                    serde_json::json!(item.versions),
                );
            }
            value
        })
        .collect();
    let mut doc = serde_json::json!({
        "root": report.root.display().to_string(),
        "type_database_loaded": report.type_database_loaded,
        "diagnostics": diagnostics,
    });
    // GH #59: additive — absent (not null) when no DB was loaded.
    if let (Some(versions), Some(obj)) = (&report.type_db, doc.as_object_mut()) {
        obj.insert("typedb".to_string(), versions.to_json());
    }
    // GH #18: additive — absent on legacy single-target runs.
    if !report.targets.is_empty() {
        doc.as_object_mut()
            .expect("literal object")
            .insert("targets".to_string(), serde_json::json!(report.targets));
    }
    format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    )
}

/// gcc/clang-ish plain lines: `path:line:col: severity: message`
fn format_plain(report: &CheckReport, color: bool) -> String {
    use crate::term;

    let mut out = String::new();
    let mut n_err = 0usize;
    let mut n_warn = 0usize;
    let mut n_other = 0usize;

    for item in &report.diagnostics {
        let rel = item.path.strip_prefix(&report.root).unwrap_or(&item.path);
        let range = item.diagnostic.range;
        let line = range.start.line + 1;
        let col = range.start.character + 1;
        let severity = severity_label(item.diagnostic.severity);
        match item.diagnostic.severity {
            Some(DiagnosticSeverity::ERROR) | None => n_err += 1,
            Some(DiagnosticSeverity::WARNING) => n_warn += 1,
            _ => n_other += 1,
        }

        let sev_styled = match item.diagnostic.severity {
            Some(DiagnosticSeverity::ERROR) | None => term::error(color, severity),
            Some(DiagnosticSeverity::WARNING) => term::warning(color, severity),
            Some(DiagnosticSeverity::INFORMATION) => term::info(color, severity),
            Some(DiagnosticSeverity::HINT) => term::dim(color, severity),
            _ => severity.to_string(),
        };

        out.push_str(&format!(
            "{}:{}: {}: {}{}{}\n",
            term::path(color, rel.display().to_string()),
            term::loc(color, format!("{line}:{col}")),
            sev_styled,
            // GH #57: combo label ([DEV] / [seen in: {} | DEV]) — empty in
            // single-combo runs.
            matrix::combo_prefix(&item.combos),
            item.diagnostic.message,
            // GH #18: version label ([op 1.29.5]) — empty unless the
            // diagnostic is version-specific in a multi-target run.
            matrix::version_suffix(&item.versions)
        ));
    }

    let summary = if report.diagnostics.is_empty() {
        term::ok(color, format!("0 diagnostics in {}", report.root.display()))
    } else {
        let mut parts = Vec::new();
        if n_err > 0 {
            parts.push(term::error(
                color,
                format!("{n_err} error{}", if n_err == 1 { "" } else { "s" }),
            ));
        }
        if n_warn > 0 {
            parts.push(term::warning(
                color,
                format!("{n_warn} warning{}", if n_warn == 1 { "" } else { "s" }),
            ));
        }
        if n_other > 0 {
            parts.push(term::info(color, format!("{n_other} other")));
        }
        format!(
            "{} {} in {}",
            term::bold(color, format!("{} diagnostics", report.diagnostics.len())),
            if parts.is_empty() {
                String::new()
            } else {
                format!("({})", parts.join(", "))
            },
            term::path(color, report.root.display().to_string())
        )
    };
    out.push_str(&summary);
    out.push('\n');
    out
}

fn resolve_workspace_root(path: &Path) -> Result<PathBuf, CliError> {
    if !path.exists() {
        return Err(CliError::Check(format!(
            "path does not exist: {}",
            path.display()
        )));
    }
    project::find_workspace_root(path).ok_or_else(|| {
        CliError::Check(format!(
            "could not find info.toml at or above {}",
            path.display()
        ))
    })
}

fn apply_typedb_dir(config: &mut LspConfig, typedb_dir: Option<&Path>) {
    if let Some(dir) = typedb_dir {
        config.core_json = Some(dir.join("OpenplanetCore.json"));
        config.game_json = Some(dir.join("OpenplanetNext.json"));
    }
}

fn load_type_index(config: &LspConfig, no_typedb: bool) -> Result<Option<TypeIndex>, CliError> {
    if no_typedb {
        return Ok(None);
    }

    let (Some(core), Some(game)) = (&config.core_json, &config.game_json) else {
        return Ok(None);
    };

    TypeIndex::load(core, game)
        .map(Some)
        .map_err(|e| CliError::Check(format!("failed to load type database: {e}")))
}

pub(crate) fn severity_label(severity: Option<DiagnosticSeverity>) -> &'static str {
    match severity {
        Some(DiagnosticSeverity::ERROR) => "error",
        Some(DiagnosticSeverity::WARNING) => "warning",
        Some(DiagnosticSeverity::INFORMATION) => "info",
        Some(DiagnosticSeverity::HINT) => "hint",
        _ => "diagnostic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_format_values() {
        assert_eq!(CheckFormat::parse("auto").unwrap(), CheckFormat::Auto);
        assert_eq!(CheckFormat::parse("PLAIN").unwrap(), CheckFormat::Plain);
        assert_eq!(CheckFormat::parse("Pretty").unwrap(), CheckFormat::Pretty);
        assert_eq!(CheckFormat::parse("json").unwrap(), CheckFormat::Json);
        assert!(CheckFormat::parse("yaml").is_err());
    }

    /// GH #57: JSON output labels each diagnostic with the combos it fired
    /// in ("combo" field); single-combo runs emit an empty array.
    #[test]
    fn json_output_carries_combo_field() {
        let mut report = trailer_report(1);
        report.diagnostics[0].combos = vec![vec!["DEV".to_string()]];
        let out = format_check_report_with(&report, false, CheckFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let diags = parsed["diagnostics"].as_array().unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0]["combo"], serde_json::json!([["DEV"]]));
        assert_eq!(diags[0]["severity"], "error");

        // No matrix run → "combo" is an empty array (still present).
        let report = trailer_report(1);
        let out = format_check_report_with(&report, false, CheckFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["diagnostics"][0]["combo"], serde_json::json!([]));
    }

    /// GH #57: plain output prefixes each diagnostic with its combo label.
    #[test]
    fn plain_output_labels_combos() {
        let mut report = trailer_report(2);
        report.diagnostics[0].combos = vec![vec!["DEV".to_string()]];
        report.diagnostics[1].combos = vec![vec![], vec!["DEV".to_string()]];
        let out = format_check_report_with(&report, false, CheckFormat::Plain);
        assert!(
            out.contains("error: [DEV] boom"),
            "single-combo prefix, got: {out:?}"
        );
        assert!(
            out.contains("error: [seen in: {} | DEV] boom"),
            "multi-combo dedup prefix, got: {out:?}"
        );

        // No matrix run → no prefix (byte-identical to pre-matrix output).
        let out = format_check_report_with(&trailer_report(1), false, CheckFormat::Plain);
        assert!(out.contains("error: boom"), "no prefix: {out:?}");
    }

    #[test]
    fn use_pretty_matrix() {
        assert!(!CheckFormat::Plain.use_pretty(true));
        assert!(!CheckFormat::Plain.use_pretty(false));
        assert!(CheckFormat::Pretty.use_pretty(true));
        assert!(CheckFormat::Pretty.use_pretty(false));
        assert!(CheckFormat::Auto.use_pretty(true));
        assert!(!CheckFormat::Auto.use_pretty(false));
    }

    #[test]
    fn parse_check_args_watch_flag() {
        let opts = parse_check_args(&["--watch".into(), ".".into()]).unwrap();
        assert!(opts.watch);
        assert_eq!(opts.path, PathBuf::from("."));

        let opts = parse_check_args(&["--watch".into()]).unwrap();
        assert!(opts.watch);
        assert_eq!(opts.path, PathBuf::from("."));
    }

    #[test]
    fn parse_check_args_format_flag() {
        let opts = parse_check_args(&[
            "--format".into(),
            "pretty".into(),
            "--no-typedb".into(),
            "/tmp/plugin".into(),
        ])
        .unwrap();
        assert_eq!(opts.format, CheckFormat::Pretty);
        assert!(opts.no_typedb);

        let opts = parse_check_args(&["--format=plain".into(), ".".into()]).unwrap();
        assert_eq!(opts.format, CheckFormat::Plain);

        let opts = parse_check_args(&[".".into()]).unwrap();
        assert_eq!(opts.format, CheckFormat::Auto);
    }

    /// GH #48: the parse tracks the flag as Option<bool> so an explicit
    /// `=false` is distinguishable from "flag absent" (config fallback).
    #[test]
    fn parse_check_args_warnings_as_errors_flag_tracking() {
        // Absent → None (falls back to the config ladder).
        let opts = parse_check_args(&[".".into()]).unwrap();
        assert_eq!(opts.warnings_as_errors_flag, None);

        // Bare flag and -Werror → Some(true).
        let opts = parse_check_args(&["--warnings-as-errors".into(), ".".into()]).unwrap();
        assert_eq!(opts.warnings_as_errors_flag, Some(true));
        let opts = parse_check_args(&["-Werror".into(), ".".into()]).unwrap();
        assert_eq!(opts.warnings_as_errors_flag, Some(true));

        // Explicit values round-trip (false stays distinguishable from None).
        let opts = parse_check_args(&["--warnings-as-errors=true".into(), ".".into()]).unwrap();
        assert_eq!(opts.warnings_as_errors_flag, Some(true));
        let opts = parse_check_args(&["--warnings-as-errors=false".into(), ".".into()]).unwrap();
        assert_eq!(opts.warnings_as_errors_flag, Some(false));

        // Last flag wins, matching the pre-Option behavior.
        let opts = parse_check_args(&[
            "--warnings-as-errors=true".into(),
            "--warnings-as-errors=false".into(),
            ".".into(),
        ])
        .unwrap();
        assert_eq!(opts.warnings_as_errors_flag, Some(false));

        // Invalid value is still a usage error.
        assert!(parse_check_args(&["--warnings-as-errors=banana".into(), ".".into()]).is_err());
    }

    #[test]
    fn parse_check_args_unused_flag() {
        let opts = parse_check_args(&[]).unwrap();
        assert_eq!(opts.unused_flag, None);

        let opts = parse_check_args(&["--no-unused".into()]).unwrap();
        assert_eq!(opts.unused_flag, Some(UnusedLevel::Ignore));

        for (arg, want) in [
            ("--unused=ignore", UnusedLevel::Ignore),
            ("--unused=warn", UnusedLevel::Warn),
            ("--unused=error", UnusedLevel::Error),
            ("--unused=off", UnusedLevel::Ignore),
            ("--unused=err", UnusedLevel::Error),
            ("--unused=hint", UnusedLevel::Hint),
            ("--unused=grey", UnusedLevel::Hint),
            ("--unused=info", UnusedLevel::Info),
            ("--unused=note", UnusedLevel::Info),
        ] {
            let opts = parse_check_args(&[arg.into()]).unwrap();
            assert_eq!(opts.unused_flag, Some(want), "{arg}");
        }

        let opts = parse_check_args(&["--unused".into(), "hint".into(), ".".into()]).unwrap();
        assert_eq!(opts.unused_flag, Some(UnusedLevel::Hint));
        assert_eq!(opts.path, PathBuf::from("."));

        let opts = parse_check_args(&["--unused".into(), "error".into(), ".".into()]).unwrap();
        assert_eq!(opts.unused_flag, Some(UnusedLevel::Error));
        assert_eq!(opts.path, PathBuf::from("."));

        let opts = parse_check_args(&["--watch".into(), "--no-unused".into()]).unwrap();
        assert!(opts.watch);
        assert_eq!(opts.unused_flag, Some(UnusedLevel::Ignore));

        assert!(parse_check_args(&["--unused".into()]).is_err());
        assert!(parse_check_args(&["--unused=banana".into()]).is_err());
        assert!(parse_check_args(&["--unused=".into()]).is_err());
    }

    /// GH #60: `--no-gitignore` is tracked on CheckOptions, absent by default,
    /// and reaches the watch/TUI path through the same parse.
    #[test]
    fn parse_check_args_no_gitignore_flag() {
        let opts = parse_check_args(&[]).unwrap();
        assert!(!opts.no_gitignore, "absent flag must leave honoring on");

        let opts = parse_check_args(&["--no-gitignore".to_string()]).unwrap();
        assert!(opts.no_gitignore);

        // Shared arg parsing: the watch/TUI entrypoint gets it too.
        let opts =
            parse_check_args(&["--watch".to_string(), "--no-gitignore".to_string()]).unwrap();
        assert!(opts.watch && opts.no_gitignore);

        // Order-independent, and it does not swallow the positional path.
        let opts =
            parse_check_args(&["--no-gitignore".to_string(), "some/dir".to_string()]).unwrap();
        assert!(opts.no_gitignore);
        assert_eq!(opts.path, PathBuf::from("some/dir"));
    }

    /// GH #48 follow-up: precedence contract —
    /// CLI flag > workspace config > user config > default(false).
    #[test]
    fn resolve_warnings_as_errors_precedence() {
        let prefs_true = crate::config::UserPrefs {
            warnings_as_errors: true,
            ..Default::default()
        };
        let prefs_false = crate::config::UserPrefs {
            warnings_as_errors: false,
            ..Default::default()
        };

        let opts = |flag: Option<&str>| -> CheckOptions {
            match flag {
                Some(f) => parse_check_args(&[f.to_string(), ".".into()]).unwrap(),
                None => CheckOptions::default(),
            }
        };

        // Default: no flag, no config → false.
        assert!(!opts(None).resolve_warnings_as_errors(&prefs_false));
        // Flag absent + config true → true (config wins by default).
        assert!(opts(None).resolve_warnings_as_errors(&prefs_true));
        // Flag true beats config false.
        assert!(opts(Some("--warnings-as-errors")).resolve_warnings_as_errors(&prefs_false));
        // Flag true beats config true (same result).
        assert!(opts(Some("--warnings-as-errors")).resolve_warnings_as_errors(&prefs_true));
        // Explicit =false beats config true — the reason the flag is an
        // Option<bool>.
        assert!(!opts(Some("--warnings-as-errors=false")).resolve_warnings_as_errors(&prefs_true));
        // =false with config false stays false.
        assert!(!opts(Some("--warnings-as-errors=false")).resolve_warnings_as_errors(&prefs_false));
    }

    fn trailer_report(diags: usize) -> CheckReport {
        use tower_lsp::lsp_types::Diagnostic;
        let diagnostics = (0..diags)
            .map(|_| CliDiagnostic {
                path: PathBuf::from("/tmp/plugin/src/Main.as"),
                combos: Vec::new(),
                versions: Vec::new(),
                diagnostic: Diagnostic {
                    severity: Some(DiagnosticSeverity::ERROR),
                    message: "boom".to_string(),
                    ..Default::default()
                },
            })
            .collect();
        CheckReport {
            root: PathBuf::from("/tmp/plugin"),
            diagnostics,
            type_database_loaded: false,
            type_db: None,
            errors: ErrorGroup::Off,
            targets: Vec::new(),
        }
    }

    /// One-shot check output must end with the Openplanet-mismatch ask
    /// pointing at the issue tracker (GH #45 follow-up; plain and pretty).
    #[test]
    fn check_report_ends_with_mismatch_issue_ask() {
        let report = trailer_report(1);
        for fmt in [CheckFormat::Plain, CheckFormat::Pretty] {
            let out = format_check_report_with(&report, false, fmt);
            assert!(
                out.contains("different compile errors or warnings compared to openplanet-lsp"),
                "{fmt:?} output must carry the mismatch ask: {out:?}"
            );
            assert!(
                out.ends_with(&format!("{ISSUE_URL}\n")),
                "{fmt:?} output must end with exactly the issue URL + one newline (no stray blank line): {out:?}"
            );
        }
    }

    /// GH #59: the version line names both DBs' stamps, and renders
    /// `<none>` rather than dropping a field when a stamp is absent.
    #[test]
    fn type_db_versions_summary_line_renders_stamps() {
        let full = TypeDbVersions {
            core_op: Some("1.29.5 (next, Public, 1234ad9e)".into()),
            game_op: Some("1.29.5".into()),
            game_mp: Some("2026-02-03 03:51:19".into()),
        };
        let line = full.summary_line();
        assert!(line.starts_with("type db: op=1.29.5 (next"), "{line}");
        assert!(line.contains("game op=1.29.5"), "{line}");
        assert!(line.contains("mp=2026-02-03 03:51:19"), "{line}");

        // Core-only (no game dump stamps) still names what is missing.
        let core_only = TypeDbVersions {
            core_op: Some("1.29.5".into()),
            ..Default::default()
        };
        let line = core_only.summary_line();
        assert!(line.contains("game op=<none>"), "{line}");
        assert!(line.contains("mp=<none>"), "{line}");

        // Fully unstamped DBs: a line that says so, never an empty line.
        assert_eq!(
            TypeDbVersions::default().summary_line(),
            "type db: op=<none>  game op=<none>  mp=<none>"
        );
    }

    /// The JSON object keys are the report contract: `op`/`mp` headline the
    /// Core build and the dump time, `game_op` carries the game DB version.
    #[test]
    fn type_db_versions_to_json_keys() {
        let versions = TypeDbVersions {
            core_op: Some("1.29.5 (next, Public, 1234ad9e)".into()),
            game_op: Some("1.29.5".into()),
            game_mp: Some("2026-02-03 03:51:19".into()),
        };
        let value = versions.to_json();
        assert_eq!(value["op"], "1.29.5 (next, Public, 1234ad9e)");
        assert_eq!(value["game_op"], "1.29.5");
        assert_eq!(value["mp"], "2026-02-03 03:51:19");

        // Absent stamps serialize as null (the key stays, so consumers can
        // tell "no stamp in the DB" from "no DB loaded" — the latter drops
        // the whole object).
        let value = TypeDbVersions::default().to_json();
        assert!(value["op"].is_null(), "{value}");
        assert!(value["game_op"].is_null(), "{value}");
    }

    /// GH #59: the `typedb` object is purely additive — present with a DB,
    /// entirely absent without one, and never disturbing the existing keys.
    #[test]
    fn json_report_typedb_field_is_additive() {
        let mut report = trailer_report(1);
        let out = format_check_report_with(&report, false, CheckFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(
            parsed.get("typedb").is_none(),
            "no DB loaded → no key at all: {out}"
        );
        assert_eq!(parsed["type_database_loaded"], false);
        assert!(parsed["root"].is_string());
        assert!(parsed["diagnostics"].is_array());

        report.type_database_loaded = true;
        report.type_db = Some(TypeDbVersions {
            core_op: Some("1.29.5 (next, Public, 1234ad9e)".into()),
            game_op: Some("1.29.5".into()),
            game_mp: Some("2026-02-03 03:51:19".into()),
        });
        let out = format_check_report_with(&report, false, CheckFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["typedb"]["game_op"], "1.29.5");
        // Pre-existing keys unchanged.
        assert_eq!(parsed["type_database_loaded"], true);
        assert_eq!(parsed["diagnostics"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["diagnostics"][0]["severity"], "error");
    }

    /// Clean runs keep the ask too — missing diagnostics (false negatives,
    /// e.g. GH #28) are mismatches worth reporting as much as extra ones.
    #[test]
    fn check_report_clean_still_asks_for_mismatch_reports() {
        let report = trailer_report(0);
        let out = format_check_report_with(&report, false, CheckFormat::Plain);
        assert!(
            out.ends_with(&format!("{ISSUE_URL}\n")),
            "clean output must end with the issue URL + one newline: {out:?}"
        );
    }

    fn mixed_severity_report(group: ErrorGroup) -> CheckReport {
        use tower_lsp::lsp_types::Diagnostic;
        let unknown: DiagnosticSeverity = serde_json::from_value(serde_json::json!(99)).unwrap();
        let item = |sev: Option<DiagnosticSeverity>, msg: &str| CliDiagnostic {
            path: PathBuf::from("/tmp/plugin/src/Main.as"),
            combos: Vec::new(),
            versions: Vec::new(),
            diagnostic: Diagnostic {
                severity: sev,
                message: msg.to_string(),
                ..Default::default()
            },
        };
        CheckReport {
            root: PathBuf::from("/tmp/plugin"),
            diagnostics: vec![
                item(Some(DiagnosticSeverity::ERROR), "msg-error-a"),
                item(Some(DiagnosticSeverity::WARNING), "msg-warn-a"),
                item(Some(DiagnosticSeverity::INFORMATION), "msg-info-a"),
                item(Some(DiagnosticSeverity::HINT), "msg-hint-a"),
                item(None, "msg-missing-a"),
                item(Some(unknown), "msg-unknown-a"),
                item(Some(DiagnosticSeverity::ERROR), "msg-error-b"),
                item(Some(DiagnosticSeverity::WARNING), "msg-warn-b"),
            ],
            type_database_loaded: false,
            type_db: None,
            errors: group,
            targets: Vec::new(),
        }
    }

    const MIXED_MSGS: &[&str] = &[
        "msg-error-a",
        "msg-warn-a",
        "msg-info-a",
        "msg-hint-a",
        "msg-missing-a",
        "msg-unknown-a",
        "msg-error-b",
        "msg-warn-b",
    ];

    fn message_order(out: &str) -> Vec<&'static str> {
        let mut hits: Vec<(usize, &'static str)> = MIXED_MSGS
            .iter()
            .filter_map(|msg| out.find(msg).map(|i| (i, *msg)))
            .collect();
        hits.sort_by_key(|(i, _)| *i);
        hits.into_iter().map(|(_, m)| m).collect()
    }

    fn last_idx(out: &str, needle: &str) -> usize {
        out.rfind(needle).expect(needle)
    }

    /// Human formats regroup by `report.errors`; JSON keeps analysis order.
    /// Variants: last / first / off × plain / pretty / json, plus trailer
    /// still last.
    #[test]
    fn human_check_output_groups_errors_last_first_off_variants() {
        let analysis = [
            "msg-error-a",
            "msg-warn-a",
            "msg-info-a",
            "msg-hint-a",
            "msg-missing-a",
            "msg-unknown-a",
            "msg-error-b",
            "msg-warn-b",
        ];
        let last = [
            "msg-warn-a",
            "msg-info-a",
            "msg-hint-a",
            "msg-unknown-a",
            "msg-warn-b",
            "msg-error-a",
            "msg-missing-a",
            "msg-error-b",
        ];
        let first = [
            "msg-error-a",
            "msg-missing-a",
            "msg-error-b",
            "msg-warn-a",
            "msg-info-a",
            "msg-hint-a",
            "msg-unknown-a",
            "msg-warn-b",
        ];

        for (group, want) in [
            (ErrorGroup::Last, last.as_slice()),
            (ErrorGroup::First, first.as_slice()),
            (ErrorGroup::Off, analysis.as_slice()),
        ] {
            let report = mixed_severity_report(group);
            for fmt in [CheckFormat::Plain, CheckFormat::Pretty] {
                let out = format_check_report_with(&report, false, fmt);
                assert_eq!(message_order(&out), want, "{group:?} {fmt:?} order: {out}");
                let last_msg = last_idx(&out, want[want.len() - 1]);
                assert!(
                    last_idx(&out, "8 diagnostics") > last_msg,
                    "{group:?} {fmt:?} summary after diags: {out}"
                );
                assert!(
                    out.ends_with(&format!("{ISSUE_URL}\n")),
                    "{group:?} {fmt:?} trailer last: {out:?}"
                );
            }

            // JSON is a machine contract: analysis order even when grouping
            // is last/first.
            let json = format_check_report_with(&report, false, CheckFormat::Json);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            let got: Vec<&str> = parsed["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["message"].as_str().unwrap())
                .collect();
            assert_eq!(got, analysis, "json must stay analysis order for {group:?}");
        }
    }

    #[test]
    fn parse_check_args_errors_flag() {
        let opts = parse_check_args(&[]).unwrap();
        assert_eq!(opts.errors_flag, None);

        for (arg, want) in [
            ("--errors=last", ErrorGroup::Last),
            ("--errors=end", ErrorGroup::Last),
            ("--errors=first", ErrorGroup::First),
            ("--errors=start", ErrorGroup::First),
            ("--errors=begin", ErrorGroup::First),
            ("--errors=off", ErrorGroup::Off),
            ("--errors=none", ErrorGroup::Off),
            ("--errors=false", ErrorGroup::Off),
            ("--errors=interleaved", ErrorGroup::Off),
        ] {
            let opts = parse_check_args(&[arg.into()]).unwrap();
            assert_eq!(opts.errors_flag, Some(want), "{arg}");
        }

        let opts = parse_check_args(&["--errors".into(), "first".into(), ".".into()]).unwrap();
        assert_eq!(opts.errors_flag, Some(ErrorGroup::First));
        assert_eq!(opts.path, PathBuf::from("."));

        assert!(parse_check_args(&["--errors".into()]).is_err());
        assert!(parse_check_args(&["--errors=banana".into()]).is_err());
        assert!(parse_check_args(&["--errors=".into()]).is_err());
    }

    #[test]
    fn resolve_errors_flag_beats_config() {
        let mut cfg = LspConfig::default();
        assert_eq!(cfg.errors, ErrorGroup::Last);

        let opts = CheckOptions::default();
        assert_eq!(opts.resolve_errors(&cfg), ErrorGroup::Last);

        cfg.errors = ErrorGroup::First;
        assert_eq!(opts.resolve_errors(&cfg), ErrorGroup::First);

        let opts = parse_check_args(&["--errors=off".into()]).unwrap();
        assert_eq!(opts.resolve_errors(&cfg), ErrorGroup::Off);

        cfg.errors = ErrorGroup::Off;
        let opts = parse_check_args(&["--errors".into(), "first".into()]).unwrap();
        assert_eq!(opts.resolve_errors(&cfg), ErrorGroup::First);
    }

    fn usage_msg(args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        match parse_check_args(&args) {
            Err(CliError::Usage(msg)) => msg,
            other => panic!("expected CliError::Usage, got {other:?}"),
        }
    }

    /// GH #62: `--typedb-dir --no-typedb <path>` must not treat `--no-typedb`
    /// as the directory (that used to try loading `--no-typedb/OpenplanetCore.json`).
    #[test]
    fn parse_check_args_typedb_dir_rejects_following_flag() {
        let msg = usage_msg(&["--typedb-dir", "--no-typedb", "tests/fixtures/tm-counter"]);
        assert!(
            msg.contains("--typedb-dir requires a directory argument"),
            "{msg}"
        );
    }

    /// GH #18: `--openplanet-version` captures separated and `=` values.
    #[test]
    fn parse_check_args_openplanet_version_captures_value() {
        let opts = parse_check_args(&[
            "--openplanet-version".into(),
            "1.29.5,1.29.14".into(),
            ".".into(),
        ])
        .unwrap();
        assert_eq!(opts.openplanet_version.as_deref(), Some("1.29.5,1.29.14"));

        let opts = parse_check_args(&["--openplanet-version=1.29.5".into(), ".".into()]).unwrap();
        assert_eq!(opts.openplanet_version.as_deref(), Some("1.29.5"));
    }

    /// GH #18: `--typedb-versions-dir` captures separated and `=` values.
    #[test]
    fn parse_check_args_typedb_versions_dir_captures_value() {
        let opts = parse_check_args(&[
            "--typedb-versions-dir".into(),
            "tests/fixtures/typedb-versions".into(),
            ".".into(),
        ])
        .unwrap();
        assert_eq!(
            opts.typedb_versions_dir,
            Some(PathBuf::from("tests/fixtures/typedb-versions"))
        );

        let opts =
            parse_check_args(&["--typedb-versions-dir=--weird".into(), ".".into()]).unwrap();
        assert_eq!(opts.typedb_versions_dir, Some(PathBuf::from("--weird")));
    }

    /// GH #18, same shape as GH #62: a following flag is never a value.
    #[test]
    fn parse_check_args_openplanet_version_rejects_following_flag() {
        let msg = usage_msg(&["--openplanet-version", "--no-typedb", "."]);
        assert!(
            msg.contains("--openplanet-version requires a version argument"),
            "{msg}"
        );
    }

    /// GH #18, same shape as GH #62: a following flag is never a value.
    #[test]
    fn parse_check_args_typedb_versions_dir_rejects_following_flag() {
        let msg = usage_msg(&["--typedb-versions-dir", "--no-typedb", "."]);
        assert!(
            msg.contains("--typedb-versions-dir requires a directory argument"),
            "{msg}"
        );
    }

    /// GH #18: `--no-typedb` contradicts both version flags, exactly as it
    /// contradicts `--typedb-dir`.
    #[test]
    fn parse_check_args_no_typedb_conflicts_with_version_flags() {
        let msg = usage_msg(&["--no-typedb", "--openplanet-version", "1.29.5", "."]);
        assert!(
            msg.contains("--no-typedb cannot be combined with --openplanet-version"),
            "{msg}"
        );

        let msg = usage_msg(&["--no-typedb", "--typedb-versions-dir", "dir", "."]);
        assert!(
            msg.contains("--no-typedb cannot be combined with --typedb-versions-dir"),
            "{msg}"
        );
    }

    /// GH #62: `--plugins-dir --no-typedb <path>` used to continue with a junk
    /// search path instead of a usage error.
    #[test]
    fn parse_check_args_plugins_dir_rejects_following_flag() {
        let msg = usage_msg(&["--plugins-dir", "--no-typedb", "tests/fixtures/tm-counter"]);
        assert!(
            msg.contains("--plugins-dir requires a directory argument"),
            "{msg}"
        );
    }

    /// GH #62: `--plugin-files-search-path --watch <path>` used to treat
    /// `--watch` as a search root.
    #[test]
    fn parse_check_args_plugin_files_search_path_rejects_following_flag() {
        let msg = usage_msg(&[
            "--plugin-files-search-path",
            "--watch",
            "tests/fixtures/tm-counter",
        ]);
        assert!(
            msg.contains("--plugin-files-search-path requires a directory argument"),
            "{msg}"
        );
    }

    /// GH #62: a define named `--no-typedb` is nonsense; space form is usage.
    #[test]
    fn parse_check_args_define_rejects_following_flag() {
        let msg = usage_msg(&["--define", "--no-typedb", "tests/fixtures/tm-counter"]);
        assert!(
            msg.contains("--define requires a symbol name argument"),
            "{msg}"
        );
        assert!(
            !msg.contains("unknown"),
            "must be the missing-arg path, not a later error: {msg}"
        );
    }

    /// GH #62: `--format --json` must be the flag-swallowed usage error, not
    /// `unknown --format value '--json'`.
    #[test]
    fn parse_check_args_format_rejects_following_flag() {
        let msg = usage_msg(&["--format", "--json", "tests/fixtures/tm-counter"]);
        assert!(
            msg.contains("--format requires a value"),
            "flag-swallowed usage, got: {msg}"
        );
        assert!(
            !msg.contains("unknown --format"),
            "must not take the unknown-format path: {msg}"
        );
    }

    /// Path flags keep `--flag=--weird`; only a *separated* `--` value is
    /// ambiguous. Mutation: a `---` guard would still accept these.
    #[test]
    fn parse_check_args_path_flags_accept_equals_option_like_value() {
        let opts = parse_check_args(&["--typedb-dir=--weird".into(), ".".into()]).unwrap();
        assert_eq!(opts.typedb_dir, Some(PathBuf::from("--weird")));

        let opts = parse_check_args(&["--plugins-dir=--weird".into(), ".".into()]).unwrap();
        assert_eq!(opts.plugins_dirs, vec![PathBuf::from("--weird")]);

        let opts =
            parse_check_args(&["--plugin-files-search-path=--weird".into(), ".".into()]).unwrap();
        assert_eq!(
            opts.plugin_files_search_paths,
            vec![PathBuf::from("--weird")]
        );
    }

    /// `--define` / `--format`: option-like values are rejected in the `=`
    /// form too. A define named `--x` is nonsense; `--format=--json` must
    /// not become "unknown format".
    #[test]
    fn parse_check_args_define_and_format_reject_equals_option_like_value() {
        let msg = usage_msg(&["--define=--x", "."]);
        assert!(
            msg.contains("--define requires a symbol name argument")
                || msg.contains("--define requires a non-empty symbol name"),
            "{msg}"
        );

        let msg = usage_msg(&["--format=--json", "."]);
        assert!(
            msg.contains("--format requires a value"),
            "flag-swallowed usage, got: {msg}"
        );
        assert!(
            !msg.contains("unknown --format"),
            "must not take the unknown-format path: {msg}"
        );
    }

    /// A value beginning with a single `-` (not `--`) stays accepted.
    #[test]
    fn parse_check_args_value_flags_accept_single_dash_value() {
        let opts = parse_check_args(&["--typedb-dir".into(), "-cache".into(), ".".into()]).unwrap();
        assert_eq!(opts.typedb_dir, Some(PathBuf::from("-cache")));

        let opts =
            parse_check_args(&["--plugins-dir".into(), "-plugins".into(), ".".into()]).unwrap();
        assert_eq!(opts.plugins_dirs, vec![PathBuf::from("-plugins")]);

        let opts = parse_check_args(&[
            "--plugin-files-search-path".into(),
            "-src".into(),
            ".".into(),
        ])
        .unwrap();
        assert_eq!(opts.plugin_files_search_paths, vec![PathBuf::from("-src")]);

        let opts = parse_check_args(&["--define".into(), "-DDEV".into(), ".".into()]).unwrap();
        assert_eq!(opts.extra_defines, vec!["-DDEV".to_string()]);
    }

    /// Flag as the final argument still gives the existing "requires an
    /// argument" error (not a swallow).
    #[test]
    fn parse_check_args_value_flags_require_argument_when_final() {
        for (args, needle) in [
            (
                &["--typedb-dir"][..],
                "--typedb-dir requires a directory argument",
            ),
            (
                &["--plugins-dir"][..],
                "--plugins-dir requires a directory argument",
            ),
            (
                &["--plugin-files-search-path"][..],
                "--plugin-files-search-path requires a directory argument",
            ),
            (
                &["--define"][..],
                "--define requires a symbol name argument",
            ),
            (&["--format"][..], "--format requires a value"),
            (&["--unused"][..], "--unused requires a value"),
            (&["--errors"][..], "--errors requires a value"),
        ] {
            let msg = usage_msg(args);
            assert!(msg.contains(needle), "{args:?} → {msg}");
        }
    }

    /// GH #64: `--game-target` / `--signature` select the define set for one
    /// run. Values are case-insensitive; the target normalizes to the
    /// upper-case token the config file uses.
    #[test]
    fn parse_check_args_game_target_and_signature() {
        let opts = parse_check_args(&["--game-target".into(), "MP4".into(), ".".into()]).unwrap();
        assert_eq!(opts.game_target_flag.as_deref(), Some("MP4"));

        let opts = parse_check_args(&["--game-target=turbo".into(), ".".into()]).unwrap();
        assert_eq!(
            opts.game_target_flag.as_deref(),
            Some("TURBO"),
            "target must normalize to the config-file token"
        );

        let opts = parse_check_args(&["--signature".into(), "regular".into(), ".".into()]).unwrap();
        assert_eq!(opts.signature_mode_flag, Some(SignatureMode::Regular));

        let opts = parse_check_args(&["--signature=DEV".into(), ".".into()]).unwrap();
        assert_eq!(opts.signature_mode_flag, Some(SignatureMode::Developer));

        let opts = parse_check_args(&[".".to_string()]).unwrap();
        assert_eq!(opts.game_target_flag, None);
        assert_eq!(opts.signature_mode_flag, None);
    }

    /// Unknown values are a usage error naming the accepted set — a typo'd
    /// flag must not silently analyze the wrong define set.
    #[test]
    fn parse_check_args_rejects_unknown_target_and_signature() {
        let msg = usage_msg(&["--game-target", "BANANA", "."]);
        assert!(msg.contains("unknown --game-target value"), "{msg}");
        assert!(
            msg.contains("TMNEXT"),
            "error must list valid targets: {msg}"
        );

        let msg = usage_msg(&["--signature", "banana", "."]);
        assert!(msg.contains("unknown --signature value"), "{msg}");
        assert!(
            msg.contains("developer"),
            "error must list valid modes: {msg}"
        );
    }

    /// B028: `--os` / `--arch` select the OS/arch define pairs for one run,
    /// same shape as `--game-target` / `--signature`.
    #[test]
    fn parse_check_args_os_and_arch_flags() {
        let opts = parse_check_args(&["--os".into(), "wine".into(), ".".into()]).unwrap();
        assert_eq!(opts.os_flag, Some(OsTarget::Wine));
        assert_eq!(opts.arch_flag, None, "absent flag stays None");

        let opts = parse_check_args(&["--os=linux".into(), ".".into()]).unwrap();
        assert_eq!(opts.os_flag, Some(OsTarget::Linux));

        let opts = parse_check_args(&["--os".into(), "Windows".into(), ".".into()]).unwrap();
        assert_eq!(opts.os_flag, Some(OsTarget::Windows), "case-insensitive");

        let opts = parse_check_args(&["--arch".into(), "32".into(), ".".into()]).unwrap();
        assert_eq!(opts.arch_flag, Some(ArchTarget::Arch32));

        let opts = parse_check_args(&["--arch=64".into(), ".".into()]).unwrap();
        assert_eq!(opts.arch_flag, Some(ArchTarget::Arch64));

        let opts = parse_check_args(&[".".to_string()]).unwrap();
        assert_eq!(opts.os_flag, None);
        assert_eq!(opts.arch_flag, None);

        let msg = usage_msg(&["--os", "banana", "."]);
        assert!(msg.contains("unknown --os value"), "{msg}");
        assert!(msg.contains("wine"), "error must list valid targets: {msg}");

        let msg = usage_msg(&["--arch", "banana", "."]);
        assert!(msg.contains("unknown --arch value"), "{msg}");
        assert!(msg.contains("64"), "error must list valid arches: {msg}");
    }

    /// B028: build-shape flag pairs (`--server` / `--no-server`, …). A bare
    /// positive flag exists so an explicit ON can beat a config `false`.
    #[test]
    fn parse_check_args_build_flag_pairs() {
        let opts = parse_check_args(&["--no-server".into(), "--logs".into(), ".".into()]).unwrap();
        assert_eq!(opts.server_flag, Some(false));
        assert_eq!(opts.logs_flag, Some(true));

        let opts = parse_check_args(&[
            "--server".into(),
            "--no-logs".into(),
            "--has-dev".into(),
            "--no-developer-build".into(),
            ".".into(),
        ])
        .unwrap();
        assert_eq!(opts.server_flag, Some(true));
        assert_eq!(opts.logs_flag, Some(false));
        assert_eq!(opts.has_dev_flag, Some(true));
        assert_eq!(opts.developer_build_flag, Some(false));

        let opts = parse_check_args(&[".".to_string()]).unwrap();
        for flag in [
            opts.server_flag,
            opts.logs_flag,
            opts.has_dev_flag,
            opts.developer_build_flag,
        ] {
            assert_eq!(flag, None, "absent build flags stay None");
        }
    }

    /// GH #62 guard: neither flag may swallow a following option, and both
    /// report the "requires a value" error when final.
    #[test]
    fn parse_check_args_game_target_and_signature_reject_following_flag() {
        for (args, needle) in [
            (
                &["--game-target", "--watch", "."][..],
                "--game-target requires a value",
            ),
            (
                &["--signature", "--watch", "."][..],
                "--signature requires a value",
            ),
            (&["--os", "--watch", "."][..], "--os requires a value"),
            (&["--arch", "--watch", "."][..], "--arch requires a value"),
            (&["--game-target"][..], "--game-target requires a value"),
            (&["--signature"][..], "--signature requires a value"),
            (&["--os"][..], "--os requires a value"),
            (&["--arch"][..], "--arch requires a value"),
            (
                &["--game-target=--watch", "."][..],
                "--game-target requires a value",
            ),
            (
                &["--signature=--watch", "."][..],
                "--signature requires a value",
            ),
            (&["--os=--watch", "."][..], "--os requires a value"),
            (&["--arch=--watch", "."][..], "--arch requires a value"),
        ] {
            let msg = usage_msg(args);
            assert!(msg.contains(needle), "{args:?} → {msg}");
            assert!(
                !msg.contains("unknown --"),
                "must not take the unknown-value path: {msg}"
            );
        }
    }

    /// Shared take-arg helper covers `--unused` / `--errors` too (same bug
    /// class as the five required flags).
    #[test]
    fn parse_check_args_unused_and_errors_reject_following_flag() {
        let msg = usage_msg(&["--unused", "--watch", "."]);
        assert!(msg.contains("--unused requires a value"), "{msg}");
        assert!(
            !msg.contains("unknown --unused"),
            "must not take the unknown-value path: {msg}"
        );

        let msg = usage_msg(&["--errors", "--watch", "."]);
        assert!(msg.contains("--errors requires a value"), "{msg}");
        assert!(
            !msg.contains("unknown --errors"),
            "must not take the unknown-value path: {msg}"
        );
    }

    /// `-D` is the short form of `--define` and takes a following token.
    #[test]
    fn parse_check_args_define_short_form_rejects_following_flag() {
        let msg = usage_msg(&["-D", "--no-typedb", "."]);
        assert!(
            msg.contains("--define requires a symbol name argument")
                || msg.contains("-D requires a symbol name argument"),
            "{msg}"
        );
    }
}
