//! Plugin-wide workspace builder.
//!
//! Pre-extracts symbols from every `.as` file in a plugin and pools them into
//! one [`SymbolTable`] so that per-file checking can see sibling-file
//! declarations (classes, functions, globals, enums, etc.).

use std::path::PathBuf;

use crate::config::LspConfig;
use crate::lexer;
use crate::parser::Parser;
use crate::preprocessor;
use crate::symbols::SymbolTable;

/// Build a [`SymbolTable`] from a slice of source files representing a single
/// plugin. Each `(path, source)` is preprocessed, lexed, parsed, and its
/// symbols extracted into the pooled table.
///
/// The returned table contains one file entry per input; look-ups via
/// [`SymbolTable::lookup`] will see symbols contributed by any sibling file.
pub fn build_plugin_symbol_table(
    files: &[(PathBuf, String)],
    config: &LspConfig,
) -> SymbolTable {
    let mut table = SymbolTable::new();
    for (_path, source) in files {
        let pp = preprocessor::preprocess(source, &config.defines);
        let tokens = lexer::tokenize_filtered(&pp.masked_source);
        let mut parser = Parser::new(&tokens, &pp.masked_source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let symbols = SymbolTable::extract_symbols(fid, &pp.masked_source, &file);
        table.set_file_symbols(fid, symbols);
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
}
