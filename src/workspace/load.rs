//! Shared plugin workspace loading: plugin sources + dependency exports.
//!
//! One path used by CLI `check` and the LSP Backend so dependency symbols
//! stay consistent across tools.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::LspConfig;
use crate::symbols::SymbolTable;
use crate::typecheck::build_plugin_symbol_table;
use crate::workspace::deps::{resolve_dependency, resolve_plugin_file};
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
    /// True only when [`load_plugin_workspace`] walked a real plugin root,
    /// every declared dependency resolved — required, optional, and the
    /// transitive `export_dependencies` of a resolved dep alike — every
    /// resolved dependency contributed all of its declared exports, no source
    /// inclusion/exclusion filter was configured, AND gitignore honoring
    /// dropped no `.as` file. Diagnostics that read the absence of a name as
    /// a game-compiler rejection must gate on this (GH #58): an unreadable,
    /// filtered-out or gitignored module file, or an unloaded dependency
    /// export, may declare the very symbol being called (gitignore is a git
    /// concept, not a compile concept — the game compiles
    /// generated-but-gitignored sources like any other), and
    /// `LspConfig::apply_manifest` defines `DEPENDENCY_<ID>` for declared
    /// optional deps whether or not they resolved, so `#if`-guarded code the
    /// game would mask out stays live here. Every open-documents-only or
    /// filtered-source fallback leaves it `false`.
    ///
    /// Dependency files are NOT affected by gitignore: a resolved dep
    /// contributes only its explicit `[script] exports`/`shared_exports`
    /// paths, read directly by `deps::collect_export_files`, never through
    /// the discovery walk.
    pub dependency_closure_complete: bool,
    /// Canonicalized absolute paths of files listed in this plugin's own
    /// info.toml `[script]` `exports` + `shared_exports`; decls in these
    /// files are public API and exempt from unused-declaration warnings.
    pub export_paths: HashSet<PathBuf>,
    /// Canonicalized absolute paths of files listed in `[script]` EXPORTS
    /// ONLY (not `shared_exports`). Game model: a strict export file is
    /// compiled into CONSUMERS only — its declarations do not exist in
    /// the defining plugin's own module (bodies there = game compile
    /// error "No matching symbol"). `shared_exports` compile into the
    /// defining plugin too, so they stay out of this set.
    pub strict_export_paths: HashSet<PathBuf>,
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
    /// GH #60: skip `.as` files excluded by `.gitignore` rules at or below the
    /// plugin root. `None` = the default (enabled); `Some(false)` is the
    /// `honor_gitignore = false` config or a `--no-gitignore` CLI flag.
    /// Orthogonal to the two lists above — the checked set is
    /// discovered − gitignored − `ignore_paths` (or ∩ `source_paths`).
    pub honor_gitignore: Option<bool>,
}

impl DependencySearch {
    pub fn with_defaults() -> Self {
        Self {
            plugins_dirs: Vec::new(),
            plugin_files_search_paths: vec![PathBuf::from("src")],
            source_paths: None,
            ignore_paths: None,
            honor_gitignore: None,
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
        // Multi-root t6: extra dirs (e.g. the opened folder) go LAST so
        // installed/configured plugins dirs keep priority and declared
        // sibling deps still resolve.
        for extra in &config.extra_plugins_dirs {
            if !self.plugins_dirs.iter().any(|p| p == extra) {
                self.plugins_dirs.push(extra.clone());
            }
        }
        if self.plugin_files_search_paths.is_empty() {
            self.plugin_files_search_paths.push(PathBuf::from("src"));
        }
        // Carry source inclusion/exclusion from config (allowlist wins).
        self.source_paths = config.source_paths.clone();
        self.ignore_paths = config.ignore_paths.clone();
        // GH #60: gitignore honoring rides the same config ladder. A CLI
        // `--no-gitignore` is applied by the caller, after this.
        self.honor_gitignore = config.honor_gitignore;
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

/// Canonicalized paths of this plugin's own `[script]` `exports` +`
/// `shared_exports` files. Entries resolve by the same rule as dep
/// exports (`resolve_plugin_file`): root-join first, then the plugin
/// files search paths (`src/` etc.). A listed file missing everywhere
/// still contributes its root-joined path (best-effort) so future file
/// creation doesn't silently lose the exemption.
fn own_export_paths(
    root: &Path,
    search: &DependencySearch,
) -> (HashSet<PathBuf>, HashSet<PathBuf>) {
    let mut paths = HashSet::new();
    let mut strict = HashSet::new();
    let manifest_path = root.join("info.toml");
    if !manifest_path.exists() {
        return (paths, strict);
    }
    let Ok(manifest) = Manifest::load(&manifest_path) else {
        return (paths, strict);
    };
    let Some(script) = &manifest.script else {
        return (paths, strict);
    };
    let insert = |rel: &String, set: &mut HashSet<PathBuf>| {
        let resolved = resolve_plugin_file(root, rel, &search.plugin_files_search_paths)
            .unwrap_or_else(|| root.join(rel));
        let canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
        set.insert(canonical);
    };
    for rel in &script.exports {
        insert(rel, &mut paths);
        insert(rel, &mut strict);
    }
    for rel in &script.shared_exports {
        insert(rel, &mut paths);
    }
    (paths, strict)
}

/// Load plugin `.as` sources under `root` and dependency export files.
pub fn load_plugin_workspace(
    root: &Path,
    search: &DependencySearch,
) -> Result<PluginWorkspaceLoad, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("failed to resolve {}: {e}", root.display()))?;

    let export_paths = own_export_paths(&root, search);
    let (export_paths, strict_export_paths) = (export_paths.0, export_paths.1);

    let discovery = project::DiscoveryOptions::from_honor_gitignore(search.honor_gitignore);
    let discovered = project::discover(&root, discovery);
    let mut files = Vec::new();
    for path in discovered.files {
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
    // A configured source filter deliberately omits own-module files, and
    // gitignore honoring drops them a step earlier still, at discovery. Since
    // AngelScript namespaces are open and additive across those files, the
    // pooled table cannot authoritatively prove an absent member is invalid.
    // Note the gitignore condition is "at least one file was actually
    // dropped", NOT "honoring is enabled" — honoring is the default, so the
    // latter would disarm the rule in nearly every repo.
    let dependency_closure_complete = dep.unresolved.is_empty()
        && dep.incomplete_export_surfaces.is_empty()
        && search.source_paths.is_none()
        && search.ignore_paths.is_none()
        && discovered.gitignore_excluded == 0;
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
        dependency_closure_complete,
        export_paths,
        strict_export_paths,
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
        dependency_closure_complete: base.dependency_closure_complete,
        export_paths: base.export_paths.clone(),
        strict_export_paths: base.strict_export_paths.clone(),
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
    /// Every declared dependency that failed to resolve, whatever its kind
    /// — drives `PluginWorkspaceLoad::dependency_closure_complete`.
    unresolved: Vec<String>,
    /// Resolved dependencies with manifest-declared exports that were missing
    /// or unreadable. Preserve the exact export names as evidence that their
    /// namespace surface is incomplete.
    incomplete_export_surfaces: Vec<(String, Vec<String>)>,
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
    queue.extend(script.dependencies.iter().map(|d| (d.clone(), true, None)));

    while let Some((dep_id, is_required, required_by)) = queue.pop() {
        if !seen_ids.insert(dep_id.clone()) {
            continue;
        }
        match resolve_in_dirs(&dep_id, plugins_dirs, plugin_files_search_paths) {
            Some(resolved) => {
                if !resolved.unloaded_exports.is_empty() {
                    result
                        .incomplete_export_surfaces
                        .push((dep_id.clone(), resolved.unloaded_exports.clone()));
                }
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
                // Any unresolved dependency, whatever its kind, means the
                // pooled symbol set is no longer the complete module
                // closure (GH #58).
                result.unresolved.push(dep_id.clone());
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
    use crate::analysis_snapshot::AnalysisSnapshot;
    use crate::typecheck::{Checker, GlobalScope, TypeDiagnosticKind};
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

    /// GH #60: `honor_gitignore` rides the config ladder into the shared
    /// discovery seam. This is the LSP server's exact path — the Backend
    /// builds its search with `with_defaults().finalize_with_config(...)`, so
    /// server and CLI cannot diverge on which files get checked.
    #[test]
    fn finalize_with_config_carries_honor_gitignore() {
        let mut config = LspConfig::default();
        assert_eq!(config.honor_gitignore, None, "default is unset");
        assert!(
            DependencySearch::with_defaults()
                .finalize_with_config(&config)
                .honor_gitignore
                .is_none(),
            "unset config must leave the search at the default"
        );

        for honor in [Some(true), Some(false)] {
            config.honor_gitignore = honor;
            assert_eq!(
                DependencySearch::with_defaults()
                    .finalize_with_config(&config)
                    .honor_gitignore,
                honor,
                "config honor_gitignore = {honor:?} must reach DependencySearch"
            );
        }
    }

    /// The tri-state resolves to the product default when unset, and the
    /// discovery seam agrees with `LspConfig::honor_gitignore_enabled`.
    #[test]
    fn honor_gitignore_tri_state_resolves_to_enabled_by_default() {
        let mut config = LspConfig::default();
        assert!(config.honor_gitignore_enabled());
        assert!(project::DiscoveryOptions::from_honor_gitignore(None).honor_gitignore);

        config.honor_gitignore = Some(false);
        assert!(!config.honor_gitignore_enabled());
        assert!(!project::DiscoveryOptions::from_honor_gitignore(Some(false)).honor_gitignore);
    }

    /// A gitignored file is dropped from the loaded workspace, and
    /// `honor_gitignore: Some(false)` puts it back — asserted through
    /// `load_plugin_workspace` (the seam both CLI and server call).
    #[test]
    fn load_plugin_workspace_applies_gitignore() {
        let base = std::env::temp_dir().join(format!(
            "openplanet-lsp-load-gitignore-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::create_dir_all(base.join("generated")).unwrap();
        std::fs::write(base.join("info.toml"), "[meta]\n").unwrap();
        std::fs::write(base.join(".gitignore"), "generated/\n").unwrap();
        std::fs::write(base.join("src/Main.as"), "void Main() {}\n").unwrap();
        std::fs::write(base.join("generated/Gen.as"), "void Gen() {}\n").unwrap();

        let loaded_names = |honor_gitignore: Option<bool>| {
            let search = DependencySearch {
                honor_gitignore,
                ..DependencySearch::with_defaults()
            };
            let mut names: Vec<String> = load_plugin_workspace(&base, &search)
                .unwrap()
                .files
                .iter()
                .filter_map(|f| f.path.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            names.sort();
            names
        };

        assert_eq!(loaded_names(None), vec!["Main.as".to_string()]);
        assert_eq!(loaded_names(Some(true)), vec!["Main.as".to_string()]);
        assert_eq!(
            loaded_names(Some(false)),
            vec!["Gen.as".to_string(), "Main.as".to_string()]
        );
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
        let load =
            load_plugin_workspace(&base.join("consumer"), &DependencySearch::with_defaults())
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

    /// A source filter deliberately drops own-module files from the pooled
    /// table, so an absent namespace member no longer proves the game would
    /// reject it. Presence of either config key is enough to break authority,
    /// including an explicitly empty list.
    #[test]
    fn source_filter_config_breaks_dependency_closure_authority() {
        let base = temp_tree("sp-closure-authority");
        multi_dir_consumer(&base);

        let cases = [
            ("unfiltered", None, None, true),
            (
                "source allowlist",
                Some(vec![PathBuf::from("src")]),
                None,
                false,
            ),
            ("empty source allowlist", Some(Vec::new()), None, false),
            (
                "ignore blocklist",
                None,
                Some(vec![PathBuf::from("OtherPacks")]),
                false,
            ),
            ("empty ignore blocklist", None, Some(Vec::new()), false),
        ];

        for (name, source_paths, ignore_paths, expected) in cases {
            let search = DependencySearch {
                source_paths,
                ignore_paths,
                ..DependencySearch::with_defaults()
            };
            let load = load_plugin_workspace(&base.join("consumer"), &search).unwrap();
            assert_eq!(
                load.dependency_closure_complete, expected,
                "{name}: source filtering must make the module surface non-authoritative"
            );
        }

        let _ = fs::remove_dir_all(base);
    }

    /// Build a `.op` ZIP archive on disk containing `info.toml` + export files.
    fn write_op_archive(op_path: &Path, info_toml: &str, files: &[(&str, &str)]) {
        let byte_files: Vec<_> = files
            .iter()
            .map(|(name, contents)| (*name, contents.as_bytes()))
            .collect();
        write_op_archive_bytes(op_path, info_toml, &byte_files);
    }

    fn write_op_archive_bytes(op_path: &Path, info_toml: &str, files: &[(&str, &[u8])]) {
        use std::io::Write;
        let f = fs::File::create(op_path).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("info.toml", opts).unwrap();
        zip.write_all(info_toml.as_bytes()).unwrap();
        for (name, contents) in files {
            zip.start_file(name, opts).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum DepExportShape {
        Directory,
        OpArchive,
    }

    #[derive(Clone, Copy, Debug)]
    enum SecondExportState {
        Complete,
        Missing,
        Unreadable,
    }

    fn no_matching_symbols(load: &PluginWorkspaceLoad, main_path: &Path) -> Vec<String> {
        let config = LspConfig::default();
        let snapshot = AnalysisSnapshot::from_load(load, &config);
        let main_path = main_path.canonicalize().unwrap();
        let analysis = snapshot.analysis_at_path(&main_path).unwrap();
        let scope = GlobalScope::new(snapshot.symbols(), None);
        let mut checker = Checker::new(analysis.masked_source(), &scope);
        checker.check_file(&analysis.file);
        checker
            .diagnostics
            .into_iter()
            .filter_map(|diagnostic| match diagnostic.kind {
                TypeDiagnosticKind::NoMatchingSymbol { qualified, .. } => Some(qualified),
                _ => None,
            })
            .collect()
    }

    /// GH #58: resolving a dependency is not enough to make its namespace
    /// surface authoritative. Every declared export must also be readable.
    /// A valid sibling export establishes `Ns`, while a missing or undecodable
    /// export may contain `Ns::Fn`; B013 must therefore stay silent. The
    /// complete control proves the same workspace remains armed when every
    /// declared export was loaded.
    #[test]
    fn dependency_export_surface_must_be_complete_before_namespaces_are_authoritative() {
        let shapes = [DepExportShape::Directory, DepExportShape::OpArchive];
        let states = [
            SecondExportState::Complete,
            SecondExportState::Missing,
            SecondExportState::Unreadable,
        ];

        for shape in shapes {
            for state in states {
                let base = temp_tree(&format!("dep-export-authority-{shape:?}-{state:?}"));
                let deps = base.join("deps");
                let consumer = base.join("consumer");
                fs::create_dir_all(&deps).unwrap();
                fs::create_dir_all(consumer.join("src")).unwrap();

                let dep_manifest = r#"
[meta]
name = "Dependency Plugin"
version = "0.1.0"
[script]
module = "DepPlugin"
exports = ["Namespace.as", "PotentialFn.as"]
"#;
                match shape {
                    DepExportShape::Directory => {
                        let dep_root = deps.join("DepPlugin");
                        fs::create_dir_all(dep_root.join("src")).unwrap();
                        fs::write(dep_root.join("info.toml"), dep_manifest).unwrap();
                        fs::write(
                            dep_root.join("src/Namespace.as"),
                            "namespace Ns { void Present() {} }\n",
                        )
                        .unwrap();
                        match state {
                            SecondExportState::Complete => fs::write(
                                dep_root.join("src/PotentialFn.as"),
                                "namespace Elsewhere { void Present() {} }\n",
                            )
                            .unwrap(),
                            SecondExportState::Missing => {}
                            SecondExportState::Unreadable => {
                                fs::create_dir_all(dep_root.join("src/PotentialFn.as")).unwrap();
                            }
                        }
                    }
                    DepExportShape::OpArchive => {
                        let namespace = b"namespace Ns { void Present() {} }\n".as_slice();
                        let complete = b"namespace Elsewhere { void Present() {} }\n".as_slice();
                        let unreadable = b"\xff\xfe\xfd".as_slice();
                        let files = match state {
                            SecondExportState::Complete => {
                                vec![("Namespace.as", namespace), ("PotentialFn.as", complete)]
                            }
                            SecondExportState::Missing => vec![("Namespace.as", namespace)],
                            SecondExportState::Unreadable => {
                                vec![("Namespace.as", namespace), ("PotentialFn.as", unreadable)]
                            }
                        };
                        write_op_archive_bytes(&deps.join("DepPlugin.op"), dep_manifest, &files);
                    }
                }

                fs::write(
                    consumer.join("info.toml"),
                    "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n[script]\ndependencies = [\"DepPlugin\"]\n",
                )
                .unwrap();
                let main_path = consumer.join("src/Main.as");
                fs::write(
                    &main_path,
                    "void Main() {\n    Ns::Present();\n    Ns::Fn();\n}\n",
                )
                .unwrap();

                let search = DependencySearch {
                    plugins_dirs: vec![deps],
                    plugin_files_search_paths: vec![PathBuf::from("src")],
                    ..DependencySearch::with_defaults()
                };
                let load = load_plugin_workspace(&consumer, &search).unwrap();
                let no_matches = no_matching_symbols(&load, &main_path);
                let complete = matches!(state, SecondExportState::Complete);
                assert_eq!(
                    load.dependency_closure_complete, complete,
                    "{shape:?}/{state:?}: authority must require every declared export"
                );
                assert!(
                    !no_matches.iter().any(|name| name == "Ns::Present"),
                    "{shape:?}/{state:?}: the loaded namespace member must resolve: {no_matches:?}"
                );
                assert_eq!(
                    no_matches.iter().any(|name| name == "Ns::Fn"),
                    complete,
                    "{shape:?}/{state:?}: B013 must be armed exactly for the complete surface: {no_matches:?}"
                );

                let _ = fs::remove_dir_all(base);
            }
        }
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

    /// own exports: a plugin's own info.toml `exports` land in
    /// `export_paths` as canonicalized absolute paths — the public-API
    /// exemption source for the upcoming unused-declaration analysis.
    #[test]
    fn own_manifest_exports_collected_as_export_paths() {
        let base = temp_tree("own-exp");
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
exports = ["src/Export.as"]
"#,
        )
        .unwrap();
        fs::write(
            base.join("consumer/src/Export.as"),
            "namespace Consumer { class PubApi { int x; } }\n",
        )
        .unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

        let load =
            load_plugin_workspace(&base.join("consumer"), &DependencySearch::with_defaults())
                .unwrap();
        let expected = base.join("consumer/src/Export.as").canonicalize().unwrap();
        assert!(
            load.export_paths.contains(&expected),
            "export_paths={:?} expected {expected:?}",
            load.export_paths
        );
        let _ = fs::remove_dir_all(base);
    }

    /// No info.toml → no own export paths (empty set, load still succeeds).
    #[test]
    fn no_manifest_means_empty_export_paths() {
        let base = temp_tree("own-none");
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

        let load =
            load_plugin_workspace(&base.join("consumer"), &DependencySearch::with_defaults())
                .unwrap();
        assert!(
            load.export_paths.is_empty(),
            "export_paths={:?} expected empty",
            load.export_paths
        );
        let _ = fs::remove_dir_all(base);
    }

    /// shared_exports entries are collected too, alongside plain exports.
    #[test]
    fn shared_exports_also_collected() {
        let base = temp_tree("own-shared");
        fs::create_dir_all(base.join("consumer/src")).unwrap();
        fs::write(
            base.join("consumer/info.toml"),
            r#"
[meta]
name = "Consumer"
version = "0.1.0"
[script]
exports = ["src/Export.as"]
shared_exports = ["src/SharedExport.as"]
"#,
        )
        .unwrap();
        fs::write(
            base.join("consumer/src/Export.as"),
            "namespace Consumer { class PubApi { int x; } }\n",
        )
        .unwrap();
        fs::write(
            base.join("consumer/src/SharedExport.as"),
            "namespace Consumer { shared class HookApi { int y; } }\n",
        )
        .unwrap();
        fs::write(base.join("consumer/src/Main.as"), "void Main() {}\n").unwrap();

        let load =
            load_plugin_workspace(&base.join("consumer"), &DependencySearch::with_defaults())
                .unwrap();
        let canon_root = base.join("consumer").canonicalize().unwrap();
        let expected_export = canon_root.join("src/Export.as").canonicalize().unwrap();
        let expected_shared = canon_root
            .join("src/SharedExport.as")
            .canonicalize()
            .unwrap();
        assert!(
            load.export_paths.contains(&expected_export),
            "export_paths={:?} expected {expected_export:?}",
            load.export_paths
        );
        assert!(
            load.export_paths.contains(&expected_shared),
            "export_paths={:?} expected {expected_shared:?}",
            load.export_paths
        );
        let _ = fs::remove_dir_all(base);
    }
    /// GH #58 x GH #60: an open buffer arriving through the LSP overlay must
    /// not restore a claim of authority the disk load already gave up. A
    /// gitignored own-module file is missing from the pool whether or not the
    /// editor happens to have one of them open, so `merge_open_documents`
    /// carries the flag through untouched in both directions.
    #[test]
    fn open_document_overlay_cannot_restore_lost_authority() {
        let base = temp_tree("overlay-authority");
        let root = base.join("plugin");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("info.toml"),
            "[meta]\nname = \"Overlay\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
        fs::create_dir_all(root.join("generated")).unwrap();
        fs::write(root.join("generated/Gen.as"), "void Gen() {}\n").unwrap();

        // No .gitignore yet: the disk load is the complete closure, and an
        // overlay for a brand-new unsaved file keeps it complete (the pool is
        // a superset of the disk one).
        let search = DependencySearch::with_defaults();
        let clean = load_plugin_workspace(&root, &search).unwrap();
        assert!(clean.dependency_closure_complete, "no filtering configured");
        let merged = merge_open_documents(
            &clean,
            &[(root.join("src/Unsaved.as"), "void Unsaved() {}".to_string())],
        );
        assert!(
            merged.dependency_closure_complete,
            "an extra open buffer cannot make a complete closure incomplete"
        );

        // Now gitignore the generated dir. Authority is lost, and opening the
        // excluded file in the editor must not win it back.
        fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        let filtered = load_plugin_workspace(&root, &search).unwrap();
        assert!(
            !filtered.dependency_closure_complete,
            "a gitignore exclusion must break authority"
        );
        let merged = merge_open_documents(
            &filtered,
            &[(root.join("generated/Gen.as"), "void Gen() {}".to_string())],
        );
        assert!(
            !merged.dependency_closure_complete,
            "opening one excluded file does not restore the missing surface"
        );

        let _ = fs::remove_dir_all(base);
    }
}
