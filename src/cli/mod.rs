//! CLI `check` command: workspace diagnostics + human-readable reports.
//!
//! Pretty layout rules live in [`pretty`] (provisional — issue #12 / map #8).

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::config::LspConfig;
use crate::server::diagnostics;
use crate::typedb::TypeIndex;
use crate::workspace::load::{load_plugin_workspace, DependencySearch};
use crate::workspace::manifest::Manifest;
use crate::workspace::project;

mod pretty;
pub mod watch;

const CHECK_HELP: &str = "\
openplanet-lsp check - Run workspace diagnostics for an OpenPlanet plugin

USAGE:
    openplanet-lsp check [OPTIONS] [PATH]
    openplanet-lsp check --watch [OPTIONS] [PATH]

ARGS:
    PATH                Plugin root or a file inside a plugin (walks up for
                        info.toml). Defaults to the current directory.

OPTIONS:
    -h, --help          Show this help message
    --watch             Live TUI: re-check on file changes (q quit · j/k · PgUp/PgDn · g/G · c density · r refresh)
    --format FMT        One-shot output only: plain | pretty | auto (default: auto)
                        auto → pretty when stdout is color-capable, else plain
                        plain → gcc-style path:line:col: severity: message
                        pretty → source excerpt + carets
                        Ignored with --watch (TUI owns presentation)
    --typedb-dir DIR    Load OpenplanetCore.json and OpenplanetNext.json from DIR
    --no-typedb         Run without Openplanet/Nadeo type database files
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
}

impl CheckFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "plain" => Ok(Self::Plain),
            "pretty" => Ok(Self::Pretty),
            other => Err(format!(
                "unknown --format value '{other}' (expected plain, pretty, or auto)"
            )),
        }
    }

    /// Whether the pretty layout should be used given a color-capability bit.
    pub fn use_pretty(self, color_capable: bool) -> bool {
        match self {
            Self::Plain => false,
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
}

#[derive(Debug)]
pub struct CheckReport {
    pub root: PathBuf,
    pub diagnostics: Vec<CliDiagnostic>,
    pub type_database_loaded: bool,
}

#[derive(Debug)]
pub struct CliDiagnostic {
    pub path: PathBuf,
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
            "--watch" => {
                options.watch = true;
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

    let type_index = load_type_index(&config, options.no_typedb)?;

    let search = DependencySearch {
        plugins_dirs: options.plugins_dirs.clone(),
        plugin_files_search_paths: options.plugin_files_search_paths.clone(),
        // Filled from config by finalize_with_config below.
        source_paths: None,
        ignore_paths: None,
    }
    .finalize_with_config(&config);

    let load = load_plugin_workspace(&root, &search).map_err(CliError::Check)?;
    // One snapshot: parse each file once, pool symbols (GH #39).
    let snapshot = AnalysisSnapshot::from_load(&load, &config);

    let mut cli_diagnostics = Vec::new();

    if !snapshot.missing_required_dependencies().is_empty() {
        let manifest_path = load.root.join("info.toml");
        for diagnostic in diagnostics::missing_required_dependency_diagnostics(
            snapshot.missing_required_dependencies(),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: manifest_path.clone(),
                diagnostic,
            });
        }
    }

    if manifest_path.exists() {
        let source = std::fs::read_to_string(&manifest_path).map_err(|e| {
            CliError::Check(format!("failed to read {}: {e}", manifest_path.display()))
        })?;
        let uri = Url::from_file_path(&manifest_path).map_err(|_| {
            CliError::Check(format!(
                "failed to create file URI for {}",
                manifest_path.display()
            ))
        })?;
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
            &config,
            type_index.as_ref(),
            &manifest_symbols,
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: manifest_path.clone(),
                diagnostic,
            });
        }
    }

    for item in &load.files {
        if !item.report_diagnostics {
            continue;
        }

        let uri = Url::from_file_path(&item.path).map_err(|_| {
            CliError::Check(format!(
                "failed to create file URI for {}",
                item.path.display()
            ))
        })?;
        // Reuse the snapshot's per-file analysis instead of re-parsing.
        let analysis = snapshot
            .analysis_at_path(&item.path)
            .expect("snapshot contains every loaded file");
        for diagnostic in diagnostics::compute_diagnostics_from_analysis(
            &uri,
            analysis,
            &config,
            type_index.as_ref(),
            snapshot.symbols(),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: item.path.clone(),
                diagnostic,
            });
        }
    }

    Ok(CheckReport {
        root: load.root,
        diagnostics: cli_diagnostics,
        type_database_loaded: type_index.is_some(),
    })
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
            "{}:{}: {}: {}\n",
            term::path(color, rel.display().to_string()),
            term::loc(color, format!("{line}:{col}")),
            sev_styled,
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
        assert!(CheckFormat::parse("json").is_err());
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

    fn trailer_report(diags: usize) -> CheckReport {
        use tower_lsp::lsp_types::Diagnostic;
        let diagnostics = (0..diags)
            .map(|_| CliDiagnostic {
                path: PathBuf::from("/tmp/plugin/src/Main.as"),
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
