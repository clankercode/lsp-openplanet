use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

use crate::config::LspConfig;
use crate::server::diagnostics;
use crate::typecheck::build_plugin_symbol_table;
use crate::typedb::TypeIndex;
use crate::workspace::deps::resolve_dependency;
use crate::workspace::manifest::Manifest;
use crate::workspace::project;

const CHECK_HELP: &str = "\
openplanet-lsp check - Run workspace diagnostics for an OpenPlanet plugin

USAGE:
    openplanet-lsp check [OPTIONS] <PATH>

OPTIONS:
    -h, --help          Show this help message
    --typedb-dir DIR    Load OpenplanetCore.json and OpenplanetNext.json from DIR
    --no-typedb         Run without Openplanet/Nadeo type database files
    --plugins-dir DIR   Directory to search for plugin dependencies
                        (may be specified multiple times; supports both directories
                        and .op archives; looks for plugins by ID)
    --plugin-files-search-path DIR
                        Additional relative search root for plugin export files
                        (may be specified multiple times; defaults to: src)

EXAMPLES:
    openplanet-lsp check ~/plugins/tm-agent
    openplanet-lsp check --plugins-dir ~/openplanet/plugins --plugins-dir ~/openplanet/my-plugins ~/plugins/tm-agent
    openplanet-lsp check --plugin-files-search-path src --plugin-files-search-path generated .
    openplanet-lsp check --typedb-dir /path/to/typedb --plugins-dir ~/openplanet/my-plugins .
";

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub path: PathBuf,
    pub typedb_dir: Option<PathBuf>,
    pub no_typedb: bool,
    pub plugins_dirs: Vec<PathBuf>,
    pub plugin_files_search_paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct CheckReport {
    pub root: PathBuf,
    pub diagnostics: Vec<CliDiagnostic>,
    pub type_database_loaded: bool,
}

#[derive(Debug)]
struct DependencyLoadResult {
    loaded_files: Vec<(PathBuf, String)>,
    missing_dependencies: Vec<String>,
}

#[derive(Debug, Clone)]
struct LoadedSource {
    path: PathBuf,
    source: String,
    report_diagnostics: bool,
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

    let manifest_path = root.join("info.toml");
    if manifest_path.exists() {
        if let Ok(manifest) = Manifest::load(&manifest_path) {
            config.apply_manifest(&manifest);
        }
    }

    let type_index = load_type_index(&config, options.no_typedb)?;

    let source_paths = project::discover_source_files(&root);
    let mut loaded = Vec::new();
    let mut cli_diagnostics = Vec::new();
    for path in source_paths {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| CliError::Check(format!("failed to read {}: {e}", path.display())))?;
        loaded.push(LoadedSource {
            path,
            source,
            report_diagnostics: true,
        });
    }

    let mut dependency_plugin_dirs = options.plugins_dirs.clone();
    if let Some(config_plugins_dir) = &config.plugins_dir {
        if !dependency_plugin_dirs
            .iter()
            .any(|p| p == config_plugins_dir)
        {
            dependency_plugin_dirs.push(config_plugins_dir.clone());
        }
    }
    if let Some(game_plugins_dir) = detect_trackmania_openplanet_plugins_dir() {
        if !dependency_plugin_dirs
            .iter()
            .any(|p| p == &game_plugins_dir)
        {
            dependency_plugin_dirs.push(game_plugins_dir);
        }
    }

    let dep_result = load_dependency_exports(
        &root,
        &dependency_plugin_dirs,
        &options.plugin_files_search_paths,
    )?;
    loaded.extend(
        dep_result
            .loaded_files
            .into_iter()
            .map(|(path, source)| LoadedSource {
                path,
                source,
                report_diagnostics: false,
            }),
    );

    if !dep_result.missing_dependencies.is_empty() {
        let manifest_path = root.join("info.toml");
        for dep_id in &dep_result.missing_dependencies {
            cli_diagnostics.push(CliDiagnostic {
                path: manifest_path.clone(),
                diagnostic: Diagnostic {
                    range: lsp_zero_range(),
                    severity: Some(DiagnosticSeverity::ERROR),
                    message: format!(
                        "dependency `{}` not found in any configured plugin directory",
                        dep_id
                    ),
                    source: Some("openplanet-lsp".to_string()),
                    ..Default::default()
                },
            });
        }
    }

    let symbol_inputs: Vec<_> = loaded
        .iter()
        .map(|item| (item.path.clone(), item.source.clone()))
        .collect();
    let workspace = build_plugin_symbol_table(&symbol_inputs, &config);

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

    for item in &loaded {
        if !item.report_diagnostics {
            continue;
        }

        let uri = Url::from_file_path(&item.path).map_err(|_| {
            CliError::Check(format!(
                "failed to create file URI for {}",
                item.path.display()
            ))
        })?;
        for diagnostic in diagnostics::compute_diagnostics(
            &uri,
            &item.source,
            &config,
            type_index.as_ref(),
            Some(&workspace),
        ) {
            cli_diagnostics.push(CliDiagnostic {
                path: item.path.clone(),
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

fn load_dependency_exports(
    root: &Path,
    plugins_dirs: &[PathBuf],
    plugin_files_search_paths: &[PathBuf],
) -> Result<DependencyLoadResult, CliError> {
    let manifest_path = root.join("info.toml");
    let mut result = DependencyLoadResult {
        loaded_files: Vec::new(),
        missing_dependencies: Vec::new(),
    };

    if plugins_dirs.is_empty() || !manifest_path.exists() {
        return Ok(result);
    }

    let manifest = match Manifest::load(&manifest_path) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(result),
    };

    let Some(script) = &manifest.script else {
        return Ok(result);
    };

    for dep_id in &script.dependencies {
        let mut resolved_dep = None;
        for plugins_dir in plugins_dirs {
            if let Some(resolved) =
                resolve_dependency(dep_id, plugins_dir, plugin_files_search_paths)
            {
                resolved_dep = Some(resolved);
                break;
            }
        }

        let Some(resolved) = resolved_dep else {
            result.missing_dependencies.push(dep_id.clone());
            continue;
        };

        for export_path in &resolved.export_files {
            let source = std::fs::read_to_string(export_path).map_err(|e| {
                CliError::Check(format!("failed to read {}: {e}", export_path.display()))
            })?;
            result.loaded_files.push((export_path.clone(), source));
        }
    }

    Ok(result)
}

fn lsp_zero_range() -> tower_lsp::lsp_types::Range {
    tower_lsp::lsp_types::Range::new(
        tower_lsp::lsp_types::Position::new(0, 0),
        tower_lsp::lsp_types::Position::new(0, 0),
    )
}

fn detect_trackmania_openplanet_plugins_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home)
        .join(".local/share/Steam/steamapps/common/Trackmania/Openplanet/Plugins");
    dir.is_dir().then_some(dir)
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
