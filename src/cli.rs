use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use crate::config::LspConfig;
use crate::server::diagnostics;
use crate::typecheck::build_plugin_symbol_table;
use crate::typedb::TypeIndex;
use crate::workspace::project;

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub path: PathBuf,
    pub typedb_dir: Option<PathBuf>,
    pub no_typedb: bool,
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
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--no-typedb" => {
                options.no_typedb = true;
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
            _ if arg.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown check option: {arg}")));
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
        [] => Err(CliError::Usage(
            "check requires a plugin path or a file inside a plugin".to_string(),
        )),
        _ => Err(CliError::Usage(
            "check accepts exactly one plugin path".to_string(),
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
    let type_index = load_type_index(&config, options.no_typedb)?;

    let source_paths = project::discover_source_files(&root);
    let mut loaded = Vec::new();
    for path in source_paths {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Check(format!("failed to read {}: {e}", path.display())))?;
        loaded.push((path, source));
    }

    let workspace = build_plugin_symbol_table(&loaded, &config);
    let mut cli_diagnostics = Vec::new();

    let manifest_path = root.join("info.toml");
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
        for diagnostic in diagnostics::compute_diagnostics(
            &uri,
            &source,
            &config,
            type_index.as_ref(),
            Some(&workspace),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: manifest_path.clone(),
                diagnostic,
            });
        }
    }

    for (path, source) in &loaded {
        let uri = Url::from_file_path(path).map_err(|_| {
            CliError::Check(format!("failed to create file URI for {}", path.display()))
        })?;
        for diagnostic in diagnostics::compute_diagnostics(
            &uri,
            source,
            &config,
            type_index.as_ref(),
            Some(&workspace),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: path.clone(),
                diagnostic,
            });
        }
    }

    Ok(CheckReport {
        root,
        diagnostics: cli_diagnostics,
        type_database_loaded: type_index.is_some(),
    })
}

pub fn format_check_report(report: &CheckReport) -> String {
    let mut out = String::new();

    for item in &report.diagnostics {
        let rel = item.path.strip_prefix(&report.root).unwrap_or(&item.path);
        let range = item.diagnostic.range;
        let line = range.start.line + 1;
        let col = range.start.character + 1;
        let severity = severity_label(item.diagnostic.severity);
        out.push_str(&format!(
            "{}:{}:{}: {}: {}\n",
            rel.display(),
            line,
            col,
            severity,
            item.diagnostic.message
        ));
    }

    out.push_str(&format!(
        "{} diagnostics in {}\n",
        report.diagnostics.len(),
        report.root.display()
    ));
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

fn severity_label(severity: Option<DiagnosticSeverity>) -> &'static str {
    match severity {
        Some(DiagnosticSeverity::ERROR) => "error",
        Some(DiagnosticSeverity::WARNING) => "warning",
        Some(DiagnosticSeverity::INFORMATION) => "info",
        Some(DiagnosticSeverity::HINT) => "hint",
        _ => "diagnostic",
    }
}
