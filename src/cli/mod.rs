//! CLI `check` command: workspace diagnostics + human-readable reports.
//!
//! Pretty layout rules live in [`pretty`] (provisional — issue #12 / map #8).

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::config::{LspConfig, UserPrefs};
use crate::server::diagnostics;
use crate::typedb::{TypeDbMeta, TypeIndex};
use crate::workspace::load::{load_plugin_workspace, DependencySearch};
use crate::workspace::manifest::Manifest;
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
    --define NAME       Add a preprocessor define on top of info.toml /
                        config (short form -D; repeatable). Use to check
                        branches the staged build enables but the source
                        tree doesn't (e.g. -D DEV for #if DEV test files).
                        Any --define collapses the define matrix
                        (define_matrix in .openplanet-lsp.toml) back to a
                        single-combo run.
    --typedb-dir DIR    Load OpenplanetCore.json and OpenplanetNext.json from DIR
    --no-typedb         Run without Openplanet/Nadeo type database files
    --no-gitignore      Check .as files that .gitignore excludes. Discovery
                        honors .gitignore at or below the plugin root by
                        default; use this (or `honor_gitignore = false` in
                        .openplanet-lsp.toml) when gitignored files are still
                        compile-critical, e.g. generated sources. The flag
                        wins over both config levels.
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
        let prefs = UserPrefs::load(workspace_root.as_deref());
        self.resolve_warnings_as_errors(&prefs)
    }
}

/// GH #59: the build stamps of the type database a check run actually
/// loaded. Surfaced in check output, the JSON report, and the server init
/// log so "the DB says X" is never an invisible premise.
///
/// The two DBs carry different stamps: Core's `op` is the full Openplanet
/// build string (with the build hash), the game dump's `op` is the bare
/// game version and its `mp` is the dump timestamp.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeDbVersions {
    pub core_op: Option<String>,
    pub game_op: Option<String>,
    pub game_mp: Option<String>,
}

impl TypeDbVersions {
    /// Best-effort read of both stamps.
    ///
    /// Only called after `TypeIndex::load` already succeeded on these paths,
    /// so a probe failure means the file changed underfoot — report the
    /// stamps we have rather than failing an otherwise-complete run.
    pub fn probe(core: &Path, game: &Path) -> Self {
        let core_meta = TypeDbMeta::probe(core).unwrap_or_default();
        let game_meta = TypeDbMeta::probe(game).unwrap_or_default();
        Self {
            core_op: core_meta.op,
            game_op: game_meta.op,
            game_mp: game_meta.mp,
        }
    }

    /// One line for check stderr and the server init log.
    pub fn summary_line(&self) -> String {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "<none>".to_string());
        format!(
            "type db: op={}  game op={}  mp={}",
            show(&self.core_op),
            show(&self.game_op),
            show(&self.game_mp)
        )
    }

    /// Additive `typedb` object for the JSON report. `op`/`mp` are the
    /// headline stamps (Core build, game dump time); `game_op` is the game
    /// DB's own version string.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "op": self.core_op,
            "mp": self.game_mp,
            "game_op": self.game_op,
        })
    }
}

#[derive(Debug)]
pub struct CheckReport {
    pub root: PathBuf,
    pub diagnostics: Vec<CliDiagnostic>,
    pub type_database_loaded: bool,
    /// GH #59: stamps of the loaded DB. `None` when no DB was loaded
    /// (`--no-typedb`, or nothing resolved).
    pub type_db: Option<TypeDbVersions>,
}

#[derive(Debug)]
pub struct CliDiagnostic {
    pub path: PathBuf,
    /// GH #57: the define combos this diagnostic fired in (each combo is a
    /// sorted define list; `[]` = the base combo). Empty = single-combo
    /// (non-matrix) run — formatters render no label in that case.
    pub combos: Vec<Vec<String>>,
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
            "--watch" => {
                options.watch = true;
                i += 1;
            }
            "--warnings-as-errors" | "-Werror" => {
                options.warnings_as_errors_flag = Some(true);
                i += 1;
            }
            "--define" | "-D" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::Usage(
                        "--define requires a symbol name argument".to_string(),
                    ));
                };
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--define requires a non-empty symbol name".to_string(),
                    ));
                }
                options.extra_defines.push(value.clone());
                i += 2;
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
            _ if arg.starts_with("--define=") => {
                let value = arg.trim_start_matches("--define=");
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--define requires a non-empty symbol name".to_string(),
                    ));
                }
                options.extra_defines.push(value.to_string());
                i += 1;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::Usage(
                        "--format requires a value (plain, pretty, or auto)".to_string(),
                    ));
                };
                options.format = CheckFormat::parse(value).map_err(CliError::Usage)?;
                i += 2;
            }
            _ if arg.starts_with("--format=") => {
                let value = arg.trim_start_matches("--format=");
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--format requires a value (plain, pretty, or auto)".to_string(),
                    ));
                }
                options.format = CheckFormat::parse(value).map_err(CliError::Usage)?;
                i += 1;
            }
            "--typedb-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::Usage(
                        "--typedb-dir requires a directory argument".to_string(),
                    ));
                };
                options.typedb_dir = Some(PathBuf::from(value));
                i += 2;
            }
            "--plugins-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::Usage(
                        "--plugins-dir requires a directory argument".to_string(),
                    ));
                };
                options.plugins_dirs.push(PathBuf::from(value));
                i += 2;
            }
            "--plugin-files-search-path" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(CliError::Usage(
                        "--plugin-files-search-path requires a directory argument".to_string(),
                    ));
                };
                if options.plugin_files_search_paths == [PathBuf::from("src")] {
                    options.plugin_files_search_paths.clear();
                }
                options.plugin_files_search_paths.push(PathBuf::from(value));
                i += 2;
            }
            _ if arg.starts_with("--typedb-dir=") => {
                let value = arg.trim_start_matches("--typedb-dir=");
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--typedb-dir requires a directory argument".to_string(),
                    ));
                }
                options.typedb_dir = Some(PathBuf::from(value));
                i += 1;
            }
            _ if arg.starts_with("--plugins-dir=") => {
                let value = arg.trim_start_matches("--plugins-dir=");
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--plugins-dir requires a directory argument".to_string(),
                    ));
                }
                options.plugins_dirs.push(PathBuf::from(value));
                i += 1;
            }
            _ if arg.starts_with("--plugin-files-search-path=") => {
                let value = arg.trim_start_matches("--plugin-files-search-path=");
                if value.is_empty() {
                    return Err(CliError::Usage(
                        "--plugin-files-search-path requires a directory argument".to_string(),
                    ));
                }
                if options.plugin_files_search_paths == [PathBuf::from("src")] {
                    options.plugin_files_search_paths.clear();
                }
                options.plugin_files_search_paths.push(PathBuf::from(value));
                i += 1;
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

pub fn run_check(options: &CheckOptions) -> Result<CheckReport, CliError> {
    let root = resolve_workspace_root(&options.path)?;
    let root = root
        .canonicalize()
        .map_err(|e| CliError::Check(format!("failed to resolve {}: {e}", root.display())))?;
    let mut config = LspConfig::load(Some(&root), None);
    apply_typedb_dir(&mut config, options.typedb_dir.as_deref());

    let manifest_path = root.join("info.toml");
    if manifest_path.exists() {
        if let Ok(manifest) = Manifest::load(&manifest_path) {
            config.apply_manifest(&manifest);
        }
    }

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

    let type_index = load_type_index(&config, options.no_typedb)?;
    // GH #59: stamp the run with the DB build it judged the code against.
    let type_db = match (type_index.as_ref(), &config.core_json, &config.game_json) {
        (Some(_), Some(core), Some(game)) => Some(TypeDbVersions::probe(core, game)),
        _ => None,
    };

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
        // parsing + symbol pooling must re-run (GH #39 / GH #57).
        let snapshot = AnalysisSnapshot::from_load(&load, &combo_config);
        let pass_start = cli_diagnostics.len();
        collect_pass_diagnostics(
            &load,
            &snapshot,
            &combo_config,
            type_index.as_ref(),
            &manifest_path,
            &mut cli_diagnostics,
        );
        if matrix_run {
            for diag in &mut cli_diagnostics[pass_start..] {
                diag.combos.push(combo.clone());
            }
        }
    }

    // GH #57: a diagnostic firing in several combos is reported once,
    // labeled with every combo it fired in.
    let cli_diagnostics = matrix::merge_combo_diagnostics(cli_diagnostics);

    Ok(CheckReport {
        root: load.root,
        diagnostics: cli_diagnostics,
        type_database_loaded: type_index.is_some(),
        type_db,
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
    manifest_path: &Path,
    cli_diagnostics: &mut Vec<CliDiagnostic>,
) {
    if !snapshot.missing_required_dependencies().is_empty() {
        for diagnostic in diagnostics::missing_required_dependency_diagnostics(
            snapshot.missing_required_dependencies(),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: manifest_path.to_path_buf(),
                combos: Vec::new(),
                diagnostic,
            });
        }
    }

    if manifest_path.exists() {
        if let Ok(source) = std::fs::read_to_string(manifest_path) {
            if let Ok(uri) = Url::from_file_path(manifest_path) {
                // The manifest is not an .as file — parse on the fly with an
                // owned symbol pool.
                let manifest_analysis =
                    crate::analysis::DocumentAnalysis::analyze(&source, &config.defines);
                let mut manifest_symbols = crate::symbols::SymbolTable::new();
                let mf_fid = manifest_symbols.allocate_file_id();
                let mf_syms = crate::symbols::SymbolTable::extract_symbols(
                    mf_fid,
                    manifest_analysis.masked_source(),
                    &manifest_analysis.file,
                );
                manifest_symbols.set_file_symbols(mf_fid, mf_syms);
                for diagnostic in diagnostics::compute_diagnostics_from_analysis(
                    &uri,
                    &manifest_analysis,
                    config,
                    type_index,
                    &manifest_symbols,
                ) {
                    cli_diagnostics.push(CliDiagnostic {
                        path: manifest_path.to_path_buf(),
                        combos: Vec::new(),
                        diagnostic,
                    });
                }
            }
        }
    }

    // Unused-declaration warnings (workspace view; own-manifest export
    // files are exempt public API). ONE collect pass for the whole
    // workspace — a per-file collect here would be O(files²) check runs.
    let unused_usage = crate::analysis::unused::WorkspaceUsage::collect(snapshot, type_index);

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
        for diagnostic in diagnostics::compute_diagnostics_from_analysis(
            &uri,
            analysis,
            config,
            type_index,
            &table_for_check,
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: item.path.clone(),
                combos: Vec::new(),
                diagnostic,
            });
        }
        if let Some(file_id) = snapshot.file_id_of_path(&item.path) {
            for diagnostic in diagnostics::unused_declaration_diagnostics(
                analysis.source.as_str(),
                &unused_usage,
                snapshot,
                file_id,
                snapshot.export_paths(),
            ) {
                cli_diagnostics.push(CliDiagnostic {
                    path: item.path.clone(),
                    combos: Vec::new(),
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
    // GH #57: JSON is a data surface — no color, no human trailer.
    if format == CheckFormat::Json {
        return format_json(report);
    }
    let body = if format.use_pretty(color) {
        pretty::format_pretty(report, color)
    } else {
        format_plain(report, color)
    };
    format!("{body}{}\n", mismatch_ask(color))
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
            serde_json::json!({
                "path": rel.display().to_string(),
                "line": item.diagnostic.range.start.line + 1,
                "col": item.diagnostic.range.start.character + 1,
                "severity": severity_label(item.diagnostic.severity),
                "message": item.diagnostic.message,
                "combo": item.combos,
            })
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
            "{}:{}: {}: {}{}\n",
            term::path(color, rel.display().to_string()),
            term::loc(color, format!("{line}:{col}")),
            sev_styled,
            // GH #57: combo label ([DEV] / [seen in: {} | DEV]) — empty in
            // single-combo runs.
            matrix::combo_prefix(&item.combos),
            item.diagnostic.message
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
}
