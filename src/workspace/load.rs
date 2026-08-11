//! Shared plugin workspace loading: plugin sources + dependency exports.
//!
//! One path used by CLI `check` and the LSP Backend so dependency symbols
//! stay consistent across tools.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::LspConfig;
use crate::symbols::SymbolTable;
use crate::typecheck::build_plugin_symbol_table;
use crate::workspace::deps::resolve_dependency;
use crate::workspace::manifest::Manifest;
use crate::workspace::project;

/// A source file participating in a plugin workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceSourceFile {
    pub path: PathBuf,
    pub source: String,
    /// When false, file contributes symbols only (dependency exports).
    pub report_diagnostics: bool,
}

/// Result of loading a plugin root plus its dependency export files.
#[derive(Debug, Clone)]
pub struct PluginWorkspaceLoad {
    pub root: PathBuf,
    pub files: Vec<WorkspaceSourceFile>,
    /// Required dependencies that could not be resolved in any plugins dir.
    pub missing_required_dependencies: Vec<String>,
}

/// Options controlling where dependency plugins are found.
#[derive(Debug, Clone, Default)]
pub struct DependencySearch {
    pub plugins_dirs: Vec<PathBuf>,
    pub plugin_files_search_paths: Vec<PathBuf>,
}

impl DependencySearch {
    pub fn with_defaults() -> Self {
        Self {
            plugins_dirs: Vec::new(),
            plugin_files_search_paths: vec![PathBuf::from("src")],
        }
    }

    /// Merge config plugins_dir and auto-detected Openplanet Plugins path.
    pub fn finalize_with_config(mut self, config: &LspConfig) -> Self {
        if let Some(config_plugins_dir) = &config.plugins_dir {
            if !self.plugins_dirs.iter().any(|p| p == config_plugins_dir) {
                self.plugins_dirs.push(config_plugins_dir.clone());
            }
        }
        if let Some(game_plugins_dir) = detect_trackmania_openplanet_plugins_dir() {
            if !self.plugins_dirs.iter().any(|p| p == &game_plugins_dir) {
                self.plugins_dirs.push(game_plugins_dir);
            }
        }
        if self.plugin_files_search_paths.is_empty() {
            self.plugin_files_search_paths.push(PathBuf::from("src"));
        }
        self
    }
}

/// Load plugin `.as` sources under `root` and dependency export files.
pub fn load_plugin_workspace(
    root: &Path,
    search: &DependencySearch,
) -> Result<PluginWorkspaceLoad, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("failed to resolve {}: {e}", root.display()))?;

    let mut files = Vec::new();
    for path in project::discover_source_files(&root) {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        files.push(WorkspaceSourceFile {
            path,
            source,
            report_diagnostics: true,
        });
    }

    let dep = load_dependency_exports(
        &root,
        &search.plugins_dirs,
        &search.plugin_files_search_paths,
    )?;
    for (path, source) in dep.loaded_files {
        // Avoid double-including if a dep export somehow lives under root.
        if files.iter().any(|f| f.path == path) {
            continue;
        }
        files.push(WorkspaceSourceFile {
            path,
            source,
            report_diagnostics: false,
        });
    }

    Ok(PluginWorkspaceLoad {
        root,
        files,
        missing_required_dependencies: dep.missing_required,
    })
}

/// Build a pooled [`SymbolTable`] from a loaded plugin workspace.
pub fn symbol_table_from_load(load: &PluginWorkspaceLoad, config: &LspConfig) -> SymbolTable {
    let inputs: Vec<_> = load
        .files
        .iter()
        .map(|f| (f.path.clone(), f.source.clone()))
        .collect();
    build_plugin_symbol_table(&inputs, config)
}

/// Merge open-document sources on top of a disk-loaded workspace.
///
/// Open buffers win when their path matches a disk file (same canonical path
/// or URI file path). Dependency export files remain.
pub fn merge_open_documents(
    base: &PluginWorkspaceLoad,
    open: &[(PathBuf, String)],
) -> PluginWorkspaceLoad {
    let mut files = base.files.clone();
    for (path, source) in open {
        if let Some(existing) = files.iter_mut().find(|f| paths_equal(&f.path, path)) {
            existing.source = source.clone();
            existing.report_diagnostics = true;
        } else {
            files.push(WorkspaceSourceFile {
                path: path.clone(),
                source: source.clone(),
                report_diagnostics: true,
            });
        }
    }
    PluginWorkspaceLoad {
        root: base.root.clone(),
        files,
        missing_required_dependencies: base.missing_required_dependencies.clone(),
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

#[derive(Debug, Default)]
struct DepLoad {
    loaded_files: Vec<(PathBuf, String)>,
    missing_required: Vec<String>,
}

fn load_dependency_exports(
    root: &Path,
    plugins_dirs: &[PathBuf],
    plugin_files_search_paths: &[PathBuf],
) -> Result<DepLoad, String> {
    let mut result = DepLoad::default();
    let manifest_path = root.join("info.toml");
    if plugins_dirs.is_empty() || !manifest_path.exists() {
        return Ok(result);
    }

    let manifest = match Manifest::load(&manifest_path) {
        Ok(m) => m,
        Err(_) => return Ok(result),
    };
    let Some(script) = &manifest.script else {
        return Ok(result);
    };

    let mut seen_ids = HashSet::new();
    let mut seen_files = HashSet::new();

    // Required: dependencies + export_dependencies
    let mut required = Vec::new();
    required.extend(script.dependencies.iter().cloned());
    required.extend(script.export_dependencies.iter().cloned());

    for dep_id in required {
        if !seen_ids.insert(dep_id.clone()) {
            continue;
        }
        match resolve_in_dirs(&dep_id, plugins_dirs, plugin_files_search_paths) {
            Some(resolved) => {
                push_export_sources(&resolved.export_files, &mut result, &mut seen_files)?;
            }
            None => result.missing_required.push(dep_id),
        }
    }

    // Optional: load when present; never missing-error
    for dep_id in &script.optional_dependencies {
        if !seen_ids.insert(dep_id.clone()) {
            continue;
        }
        if let Some(resolved) = resolve_in_dirs(dep_id, plugins_dirs, plugin_files_search_paths) {
            push_export_sources(&resolved.export_files, &mut result, &mut seen_files)?;
        }
    }

    Ok(result)
}

fn resolve_in_dirs(
    dep_id: &str,
    plugins_dirs: &[PathBuf],
    plugin_files_search_paths: &[PathBuf],
) -> Option<crate::workspace::deps::ResolvedDependency> {
    for plugins_dir in plugins_dirs {
        if let Some(resolved) = resolve_dependency(dep_id, plugins_dir, plugin_files_search_paths) {
            return Some(resolved);
        }
    }
    None
}

fn push_export_sources(
    export_files: &[PathBuf],
    result: &mut DepLoad,
    seen_files: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    for export_path in export_files {
        let key = export_path
            .canonicalize()
            .unwrap_or_else(|_| export_path.clone());
        if !seen_files.insert(key) {
            continue;
        }
        let source = std::fs::read_to_string(export_path)
            .map_err(|e| format!("failed to read {}: {e}", export_path.display()))?;
        result.loaded_files.push((export_path.clone(), source));
    }
    Ok(())
}

/// Best-effort default Openplanet Plugins directory (Steam Trackmania).
pub fn detect_trackmania_openplanet_plugins_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home)
        .join(".local/share/Steam/steamapps/common/Trackmania/Openplanet/Plugins");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_tree(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "openplanet-lsp-ws-load-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn loads_required_dependency_exports_into_symbol_table() {
        let base = temp_tree("req");
        fs::create_dir_all(base.join("deps/DepPlugin/src")).unwrap();
        fs::create_dir_all(base.join("consumer/src")).unwrap();

        fs::write(
            base.join("deps/DepPlugin/info.toml"),
            r#"
[meta]
name = "Dep"
version = "0.1.0"
[script]
module = "DepPlugin"
exports = ["Export.as"]
"#,
        )
        .unwrap();
        fs::write(
            base.join("deps/DepPlugin/src/Export.as"),
            "namespace DepPlugin { class SharedType { int x; } }\n",
        )
        .unwrap();

        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
dependencies = ["DepPlugin"]
"#,
        )
        .unwrap();
        fs::write(
            base.join("consumer/src/Main.as"),
            "void Main() { DepPlugin::SharedType@ t; }\n",
        )
        .unwrap();

        let search = DependencySearch {
            plugins_dirs: vec![base.join("deps")],
            plugin_files_search_paths: vec![PathBuf::from("src")],
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        assert!(
            load.missing_required_dependencies.is_empty(),
            "missing={:?}",
            load.missing_required_dependencies
        );
        assert!(
            load.files.iter().any(|f| !f.report_diagnostics),
            "expected dependency export files"
        );

        let table = symbol_table_from_load(&load, &LspConfig::default());
        assert!(
            !table.lookup("SharedType").is_empty()
                || !table.lookup("DepPlugin::SharedType").is_empty(),
            "SharedType from dependency export should be in symbol table; lookup empty"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn optional_dependency_missing_is_not_an_error() {
        let base = temp_tree("opt");
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
optional_dependencies = ["MissingOptional"]
"#,
        )
        .unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

        let search = DependencySearch {
            plugins_dirs: vec![base.join("deps")],
            plugin_files_search_paths: vec![PathBuf::from("src")],
        };
        // deps dir may not exist — still ok
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        assert!(load.missing_required_dependencies.is_empty());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn required_dependency_missing_is_reported() {
        let base = temp_tree("miss");
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::create_dir_all(base.join("deps")).unwrap();
        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
dependencies = ["NoSuchPlugin"]
"#,
        )
        .unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

        let search = DependencySearch {
            plugins_dirs: vec![base.join("deps")],
            plugin_files_search_paths: vec![PathBuf::from("src")],
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        assert_eq!(load.missing_required_dependencies, vec!["NoSuchPlugin"]);
        let _ = fs::remove_dir_all(base);
    }
}
