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
    /// Allowlist of root-relative dirs to check `.as` files in (`Some` → only
    /// these). From `.openplanet-lsp.toml` `source_paths`.
    pub source_paths: Option<Vec<PathBuf>>,
    /// Blocklist of root-relative dirs to skip (only when `source_paths` is
    /// `None`). From `.openplanet-lsp.toml` `ignore_paths`.
    pub ignore_paths: Option<Vec<PathBuf>>,
}

impl DependencySearch {
    pub fn with_defaults() -> Self {
        Self {
            plugins_dirs: Vec::new(),
            plugin_files_search_paths: vec![PathBuf::from("src")],
            source_paths: None,
            ignore_paths: None,
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
        // Carry source inclusion/exclusion from config (allowlist wins).
        self.source_paths = config.source_paths.clone();
        self.ignore_paths = config.ignore_paths.clone();
        self
    }
}

/// Decide whether a discovered `.as` file should be checked, given the
/// source include/exclude config. Precedence: `source_paths` (allowlist) →
/// `ignore_paths` (blocklist) → include everything. Root-relative prefix match:
/// an entry `src` matches `src/**` but not `src_old/**`. Root-level files
/// (directly under root) are only excluded by an exact-match blocklist entry
/// that is itself a file name, which we don't support — they stay included.
fn source_file_included(root: &Path, path: &Path, search: &DependencySearch) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    if let Some(allow) = &search.source_paths {
        // Allowlist: file must live under one of the listed dirs. A bare
        // root-level file (rel has a single component) is included only if the
        // allowlist contains "." (explicit opt-in for root-level sources).
        return allow.iter().any(|dir| {
            dir == Path::new(".") && rel.components().count() == 1 || rel.starts_with(dir)
        });
    }
    if let Some(ignore) = &search.ignore_paths {
        return !ignore.iter().any(|dir| rel.starts_with(dir));
    }
    true
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
        if !source_file_included(&root, &path, search) {
            continue;
        }
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
    if !manifest_path.exists() {
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

    // Required: dependencies + export_dependencies. A resolved dep's own
    // `export_dependencies` are enqueued so nested exports walk transitively
    // (MLFeed → MLHook). Optional deps load when present, never error.
    // Vec is used as a stack (pop from back), so push optional FIRST and
    // required LAST → required deps process before optional ones.
    // Tuple: (id, is_required, required_by). required_by is the dep whose
    // manifest pulled this id in transitively (None for root manifest entries)
    // — used to warn when a nested export_dependency can't be resolved.
    let mut queue: Vec<(String, bool, Option<String>)> = Vec::new();
    queue.extend(
        script
            .optional_dependencies
            .iter()
            .map(|d| (d.clone(), false, None)),
    );
    queue.extend(
        script
            .export_dependencies
            .iter()
            .map(|d| (d.clone(), true, None)),
    );
    queue.extend(
        script
            .dependencies
            .iter()
            .map(|d| (d.clone(), true, None)),
    );

    while let Some((dep_id, is_required, required_by)) = queue.pop() {
        if !seen_ids.insert(dep_id.clone()) {
            continue;
        }
        match resolve_in_dirs(&dep_id, plugins_dirs, plugin_files_search_paths) {
            Some(resolved) => {
                push_export_sources(&resolved.export_files, &mut result, &mut seen_files)?;
                // Recurse into the dep's own export_dependencies so nested
                // exports resolve. These are treated as required-transitive
                // (a missing nested dep only errors if the *root* required it).
                if let Some(dep_script) = &resolved.manifest.script {
                    for nested in &dep_script.export_dependencies {
                        if !seen_ids.contains(nested) {
                            queue.push((nested.clone(), false, Some(dep_id.clone())));
                        }
                    }
                }
            }
            None => {
                if is_required {
                    result.missing_required.push(dep_id);
                } else if let Some(parent) = required_by {
                    // A resolved dep named this as an export_dependency but it
                    // can't be found — the parent will now produce "unknown
                    // type" false positives. Surface it (pinned semantics: not
                    // a hard missing_required, but not silent either).
                    tracing::warn!(
                        "dependency `{dep_id}` (export_dependency of `{parent}`) not found in any plugins dir"
                    );
                }
            }
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
    export_files: &[(PathBuf, String)],
    result: &mut DepLoad,
    seen_files: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    for (export_path, source) in export_files {
        let key = export_path
            .canonicalize()
            .unwrap_or_else(|_| export_path.clone());
        if !seen_files.insert(key) {
            continue;
        }
        result
            .loaded_files
            .push((export_path.clone(), source.clone()));
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
            ..DependencySearch::with_defaults()
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
            ..DependencySearch::with_defaults()
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
            ..DependencySearch::with_defaults()
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        assert_eq!(load.missing_required_dependencies, vec!["NoSuchPlugin"]);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn required_dependency_missing_without_plugin_dirs_is_reported() {
        let base = temp_tree("miss-no-dirs");
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
dependencies = ["NoSearchPath"]
"#,
        )
        .unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

        let load =
            load_plugin_workspace(&base.join("consumer"), &DependencySearch::with_defaults())
                .unwrap();
        assert_eq!(load.missing_required_dependencies, vec!["NoSearchPath"]);
        let _ = fs::remove_dir_all(base);
    }

    /// Build a consumer plugin with sources in several root-relative dirs.
    fn multi_dir_consumer(base: &Path) {
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::create_dir_all(base.join("consumer/OtherPacks")).unwrap();
        fs::create_dir_all(base.join("consumer/Scripts")).unwrap();
        fs::write(
            base.join("consumer/info.toml"),
            "[meta]\nname = \"C\"\nversion = \"0.1.0\"\n[script]\n",
        )
        .unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();
        fs::write(base.join("consumer/OtherPacks/DS.as"), "void ds() {}\n").unwrap();
        fs::write(base.join("consumer/Scripts/Util.as"), "void util() {}\n").unwrap();
    }

    fn loaded_rel_paths(load: &PluginWorkspaceLoad, base: &Path) -> Vec<String> {
        let mut v: Vec<String> = load
            .files
            .iter()
            .filter(|f| f.report_diagnostics)
            .map(|f| {
                f.path
                    .strip_prefix(base.join("consumer").canonicalize().unwrap())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn no_source_config_checks_everything() {
        let base = temp_tree("sp-all");
        multi_dir_consumer(&base);
        let load = load_plugin_workspace(&base.join("consumer"), &DependencySearch::with_defaults())
            .unwrap();
        let rel = loaded_rel_paths(&load, &base);
        assert!(rel.iter().any(|p| p == "src/Main.as"), "{rel:?}");
        assert!(rel.iter().any(|p| p == "OtherPacks/DS.as"), "{rel:?}");
        assert!(rel.iter().any(|p| p == "Scripts/Util.as"), "{rel:?}");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn ignore_paths_excludes_listed_dirs() {
        let base = temp_tree("sp-ignore");
        multi_dir_consumer(&base);
        let search = DependencySearch {
            ignore_paths: Some(vec![PathBuf::from("OtherPacks"), PathBuf::from("Scripts")]),
            ..DependencySearch::with_defaults()
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        let rel = loaded_rel_paths(&load, &base);
        assert!(rel.iter().any(|p| p == "src/Main.as"), "{rel:?}");
        assert!(!rel.iter().any(|p| p.starts_with("OtherPacks/")), "{rel:?}");
        assert!(!rel.iter().any(|p| p.starts_with("Scripts/")), "{rel:?}");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn source_paths_allowlist_checks_only_listed_dirs() {
        let base = temp_tree("sp-allow");
        multi_dir_consumer(&base);
        let search = DependencySearch {
            source_paths: Some(vec![PathBuf::from("src")]),
            ..DependencySearch::with_defaults()
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        let rel = loaded_rel_paths(&load, &base);
        assert_eq!(rel, vec!["src/Main.as".to_string()], "{rel:?}");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn source_paths_takes_precedence_over_ignore_paths() {
        let base = temp_tree("sp-both");
        multi_dir_consumer(&base);
        let search = DependencySearch {
            source_paths: Some(vec![PathBuf::from("src")]),
            // ignore_paths lists src too, but the allowlist must win.
            ignore_paths: Some(vec![PathBuf::from("src")]),
            ..DependencySearch::with_defaults()
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        let rel = loaded_rel_paths(&load, &base);
        assert_eq!(rel, vec!["src/Main.as".to_string()], "{rel:?}");
        let _ = fs::remove_dir_all(base);
    }

    /// Build a `.op` ZIP archive on disk containing `info.toml` + export files.
    fn write_op_archive(op_path: &Path, info_toml: &str, files: &[(&str, &str)]) {
        use std::io::Write;
        let f = fs::File::create(op_path).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("info.toml", opts).unwrap();
        zip.write_all(info_toml.as_bytes()).unwrap();
        for (name, contents) in files {
            zip.start_file(name, opts).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    /// GH #20: an installed dependency shipped as a `.op` ZIP archive must
    /// contribute its `exports`/`shared_exports` to the workspace symbol
    /// table — the common installed-Openplanet case (MLFeedRaceData.op etc.).
    #[test]
    fn op_archive_dependency_exports_load_into_symbol_table() {
        let base = temp_tree("op-arch");
        fs::create_dir_all(base.join("deps")).unwrap();
        fs::create_dir_all(base.join("consumer/src")).unwrap();

        write_op_archive(
            &base.join("deps/MLFeedRaceData.op"),
            r#"
[meta]
name = "MLFeedRaceData"
version = "1.0.0"
[script]
module = "MLFeed"
exports = ["Export.as"]
shared_exports = ["SharedExport.as"]
"#,
            &[
                (
                    "Export.as",
                    "namespace MLFeed { class RaceData { int cp; } }\n",
                ),
                (
                    "SharedExport.as",
                    "namespace MLFeed { shared class HookData { int t; } }\n",
                ),
            ],
        );

        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
dependencies = ["MLFeedRaceData"]
"#,
        )
        .unwrap();
        fs::write(
            base.join("consumer/src/Main.as"),
            "void Main() { MLFeed::RaceData@ t; MLFeed::HookData@ h; }\n",
        )
        .unwrap();

        let search = DependencySearch {
            plugins_dirs: vec![base.join("deps")],
            plugin_files_search_paths: vec![PathBuf::from("src")],
            ..DependencySearch::with_defaults()
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        assert!(
            load.missing_required_dependencies.is_empty(),
            "missing={:?}",
            load.missing_required_dependencies
        );
        // Both export files should be present as non-diagnostic sources.
        let dep_files: Vec<_> = load
            .files
            .iter()
            .filter(|f| !f.report_diagnostics)
            .collect();
        assert!(
            dep_files.len() >= 2,
            "expected exports + shared_exports from .op, got {} dep files",
            dep_files.len()
        );

        let table = symbol_table_from_load(&load, &LspConfig::default());
        assert!(
            !table.lookup("RaceData").is_empty() || !table.lookup("MLFeed::RaceData").is_empty(),
            "RaceData from .op export should be in symbol table"
        );
        assert!(
            !table.lookup("HookData").is_empty() || !table.lookup("MLFeed::HookData").is_empty(),
            "HookData from .op shared_export should be in symbol table"
        );

        let _ = fs::remove_dir_all(base);
    }

    /// GH #20 acceptance #4: a `.op` dependency's own `export_dependencies`
    /// must still resolve transitively (MLFeed → MLHook).
    #[test]
    fn op_archive_export_dependencies_walk_transitively() {
        let base = temp_tree("op-nested");
        fs::create_dir_all(base.join("deps")).unwrap();
        fs::create_dir_all(base.join("consumer/src")).unwrap();

        // Inner dep: MLHook.op
        write_op_archive(
            &base.join("deps/MLHook.op"),
            r#"
[meta]
name = "MLHook"
version = "1.0.0"
[script]
module = "MLHook"
exports = ["HookExport.as"]
"#,
            &[(
                "HookExport.as",
                "namespace MLHook { class HookEvent { int id; } }\n",
            )],
        );
        // Outer dep: MLFeedRaceData.op depends on MLHook
        write_op_archive(
            &base.join("deps/MLFeedRaceData.op"),
            r#"
[meta]
name = "MLFeedRaceData"
version = "1.0.0"
[script]
module = "MLFeed"
exports = ["Export.as"]
export_dependencies = ["MLHook"]
"#,
            &[(
                "Export.as",
                "namespace MLFeed { class RaceData { int cp; } }\n",
            )],
        );

        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
dependencies = ["MLFeedRaceData"]
"#,
        )
        .unwrap();
        fs::write(
            base.join("consumer/src/Main.as"),
            "void Main() { MLFeed::RaceData@ t; MLHook::HookEvent@ e; }\n",
        )
        .unwrap();

        let search = DependencySearch {
            plugins_dirs: vec![base.join("deps")],
            plugin_files_search_paths: vec![PathBuf::from("src")],
            ..DependencySearch::with_defaults()
        };
        let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
        assert!(
            load.missing_required_dependencies.is_empty(),
            "missing={:?}",
            load.missing_required_dependencies
        );

        let table = symbol_table_from_load(&load, &LspConfig::default());
        assert!(
            !table.lookup("RaceData").is_empty() || !table.lookup("MLFeed::RaceData").is_empty(),
            "RaceData from outer .op export should be in symbol table"
        );
        assert!(
            !table.lookup("HookEvent").is_empty() || !table.lookup("MLHook::HookEvent").is_empty(),
            "HookEvent from nested export_dependency .op should be in symbol table"
        );

        let _ = fs::remove_dir_all(base);
    }
}
