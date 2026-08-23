//! Plugin-wide workspace builder.
//!
//! Pre-extracts symbols from every `.as` file in a plugin and pools them into
//! one [`SymbolTable`] so that per-file checking can see sibling-file
//! declarations (classes, functions, globals, enums, etc.).

use std::path::PathBuf;

use crate::analysis::DocumentAnalysis;
use crate::config::LspConfig;
use crate::symbols::SymbolTable;

/// Build a [`SymbolTable`] from a slice of source files representing a single
/// plugin. Each `(path, source)` is preprocessed, lexed, parsed, and its
/// symbols extracted into the pooled table.
///
/// The returned table contains one file entry per input; look-ups via
/// [`SymbolTable::lookup`] will see symbols contributed by any sibling file.
pub fn build_plugin_symbol_table(files: &[(PathBuf, String)], config: &LspConfig) -> SymbolTable {
    let mut table = SymbolTable::new();
    for (_path, source) in files {
        let analysis = DocumentAnalysis::analyze(source, &config.defines);
        let fid = table.allocate_file_id();
        let symbols = SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
        table.set_file_symbols(fid, symbols);
    }
    table
}

/// Like [`build_plugin_symbol_table`], but the file paths in `skip` are
/// hidden from name resolution afterwards — the defining plugin's
/// OWN-module view (strict `[script]` exports compile into consumers
/// only, game parity 2026-08-23). Matching is by canonicalized path,
/// falling back to the raw path.
pub fn build_plugin_symbol_table_filtered(
    files: &[(PathBuf, String)],
    config: &LspConfig,
    skip: &std::collections::HashSet<PathBuf>,
) -> SymbolTable {
    let mut table = build_plugin_symbol_table(files, config);
    if skip.is_empty() {
        return table;
    }
    let canonical_skip: std::collections::HashSet<PathBuf> = skip
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    for (fid, (path, _)) in files.iter().enumerate() {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if canonical_skip.contains(&canonical) {
            table.hide_file(fid);
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_pools_sibling_symbols() {
        let files = vec![
            (PathBuf::from("a.as"), "class Foo {}".into()),
            (PathBuf::from("b.as"), "void main() { Foo@ x; }".into()),
        ];
        let table = build_plugin_symbol_table(&files, &LspConfig::default());
        assert!(
            !table.lookup("Foo").is_empty(),
            "Foo should be in pooled table"
        );
    }

    /// Game parity (2026-08-23): a file listed in `[script] exports` is
    /// compiled into CONSUMERS only — its declarations must NOT resolve
    /// in the defining plugin's own module. The own-module symbol view
    /// omits strict export files while keeping every other file.
    #[test]
    fn own_module_view_omits_strict_export_files() {
        let files = vec![
            (
                PathBuf::from("src/Exports_Test.as"),
                "string SendEvent(string a, string b) { return a; }".into(),
            ),
            (
                PathBuf::from("src/Main.as"),
                "void Main() { SendEvent(\"x\", \"y\"); }".into(),
            ),
        ];
        // Full view (consumer/hover/unused side): export decls visible.
        let full = build_plugin_symbol_table(&files, &LspConfig::default());
        assert!(
            !full.lookup("SendEvent").is_empty(),
            "full view must see export-file decls"
        );
        // Own-module view: strict export file excluded.
        let skip = [PathBuf::from("src/Exports_Test.as")].into_iter().collect();
        let own = build_plugin_symbol_table_filtered(&files, &LspConfig::default(), &skip);
        assert!(
            own.lookup("SendEvent").is_empty(),
            "own-module view must NOT see strict export decls"
        );
    }

    /// shared_exports stay visible in the own-module view — the game
    /// compiles them into the defining plugin too (fixture evidence:
    /// tm-editor-plus-plus GBX.as is core internals exported for deps).
    #[test]
    fn own_module_view_keeps_shared_exports() {
        let files = vec![
            (
                PathBuf::from("src/GBX.as"),
                "class GBXHelper { int x; }".into(),
            ),
            (
                PathBuf::from("src/Main.as"),
                "void Main() { GBXHelper@ g; }".into(),
            ),
        ];
        let skip = std::collections::HashSet::new(); // nothing strict-exported
        let own = build_plugin_symbol_table_filtered(&files, &LspConfig::default(), &skip);
        assert!(
            !own.lookup("GBXHelper").is_empty(),
            "shared exports must stay visible in own-module view"
        );
    }

    #[test]
    fn workspace_allocates_distinct_file_ids() {
        let files = vec![
            (PathBuf::from("a.as"), "class A {}".into()),
            (PathBuf::from("b.as"), "class B {}".into()),
            (PathBuf::from("c.as"), "class C {}".into()),
        ];
        let table = build_plugin_symbol_table(&files, &LspConfig::default());
        assert!(!table.lookup("A").is_empty());
        assert!(!table.lookup("B").is_empty());
        assert!(!table.lookup("C").is_empty());
    }

    #[test]
    fn workspace_symbols_respect_preprocessor_defines() {
        let files = vec![(
            PathBuf::from("feature.as"),
            "#if FEATURE\nclass EnabledOnly {}\n#endif".into(),
        )];

        let without_feature = build_plugin_symbol_table(&files, &LspConfig::default());
        assert!(without_feature.lookup("EnabledOnly").is_empty());

        let mut config = LspConfig::default();
        config.defines.insert("FEATURE".to_string());
        let with_feature = build_plugin_symbol_table(&files, &config);
        assert!(!with_feature.lookup("EnabledOnly").is_empty());
    }
}
