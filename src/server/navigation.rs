//! Goto-definition and find-references for the LSP.
//!
//! Both features share a position → symbol-name lookup that walks the token
//! stream at the cursor, reconstructing any `Ns::Name` prefix. Definitions
//! are resolved against a [`SymbolTable`] built from the open documents, and
//! references are located via a pragmatic token-scan (no AST-aware shadowing).

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::lexer::{self, TokenKind};
use crate::server::diagnostics::{position_to_offset, span_to_range};
use crate::symbols::SymbolTable;

/// Mapping from `file_id` → `(uri, source)` for every open document.
///
/// Used to translate a symbol's `file_id` + span into a concrete
/// `Location` in a potentially different document than the one where the
/// request originated.
pub struct WorkspaceFiles<'a> {
    pub files: &'a HashMap<usize, (Url, String)>,
}

impl<'a> WorkspaceFiles<'a> {
    pub fn get(&self, fid: usize) -> Option<&(Url, String)> {
        self.files.get(&fid)
    }
}

/// Find the qualified identifier name at the given position. Returns `None`
/// if the cursor is not over an identifier.
///
/// When the cursor is on the `Name` in `Ns::Sub::Name`, the returned value is
/// `"Ns::Sub::Name"`. When the cursor is on `Sub`, it is `"Ns::Sub"`.
pub fn name_at_position(source: &str, position: Position) -> Option<String> {
    let offset = position_to_offset(source, position);
    let tokens = lexer::tokenize_filtered(source);
    let (idx, token) = tokens.iter().enumerate().find(|(_, t)| {
        let start = t.span.start as usize;
        let end = t.span.end as usize;
        start <= offset && offset < end.max(start + 1)
    })?;
    if token.kind != TokenKind::Ident {
        return None;
    }
    let mut parts = vec![token.span.text(source).to_string()];
    let mut i = idx;
    while i >= 2
        && tokens[i - 1].kind == TokenKind::ColonColon
        && tokens[i - 2].kind == TokenKind::Ident
    {
        parts.push(tokens[i - 2].span.text(source).to_string());
        i -= 2;
    }
    parts.reverse();
    Some(parts.join("::"))
}

/// Resolve the definition location of the symbol at `position` in `source`.
///
/// Looks up the qualified name in `workspace` first, then falls back to the
/// bare (last-segment) name. Returns `None` if the cursor is not on an
/// identifier, no matching symbol exists, or the owning file is not in
/// `files`.
pub fn goto_definition(
    source: &str,
    position: Position,
    workspace: &SymbolTable,
    files: &WorkspaceFiles,
) -> Option<Location> {
    let qual = name_at_position(source, position)?;
    let qualified_hits = workspace.lookup(&qual);
    let candidates = if !qualified_hits.is_empty() {
        qualified_hits
    } else {
        let bare = qual.rsplit("::").next().unwrap_or(&qual);
        workspace.lookup(bare)
    };
    let sym = candidates.first()?;
    let (uri, def_source) = files.get(sym.file_id)?;
    Some(Location {
        uri: uri.clone(),
        range: span_to_range(def_source, sym.span),
    })
}

/// Build a workspace rename edit replacing every textual reference to the
/// identifier under `position` with `new_name`.
///
/// Uses the same token-scan strategy as [`find_references`] — every `Ident`
/// token in the open workspace whose text matches the cursor's bare name is
/// rewritten. Returns `None` if the cursor is not on an identifier or if no
/// matches were found.
pub fn rename(
    source: &str,
    position: Position,
    new_name: &str,
    files: &WorkspaceFiles,
) -> Option<WorkspaceEdit> {
    let qual = name_at_position(source, position)?;
    let bare = qual.rsplit("::").next().unwrap_or(&qual).to_string();
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for (_fid, (uri, src)) in files.files.iter() {
        let tokens = lexer::tokenize_filtered(src);
        let mut edits = Vec::new();
        for tok in &tokens {
            if tok.kind != TokenKind::Ident {
                continue;
            }
            if tok.span.text(src) == bare {
                edits.push(TextEdit {
                    range: span_to_range(src, tok.span),
                    new_text: new_name.to_string(),
                });
            }
        }
        if !edits.is_empty() {
            changes.insert(uri.clone(), edits);
        }
    }
    if changes.is_empty() {
        return None;
    }
    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// Find all references to the symbol at `position` across the workspace.
///
/// Pragmatic implementation: once the bare identifier is known, every file
/// in `files` is tokenized and every `Ident` whose text equals the bare name
/// is reported. `include_declaration` is accepted for API compatibility but
/// not acted on — clients can dedupe against the declaration site if they
/// care.
pub fn find_references(
    source: &str,
    position: Position,
    files: &WorkspaceFiles,
    _include_declaration: bool,
) -> Vec<Location> {
    let Some(qual) = name_at_position(source, position) else {
        return Vec::new();
    };
    let bare = qual.rsplit("::").next().unwrap_or(&qual);
    let mut results = Vec::new();
    for (_fid, (uri, src)) in files.files.iter() {
        let tokens = lexer::tokenize_filtered(src);
        for tok in &tokens {
            if tok.kind != TokenKind::Ident {
                continue;
            }
            if tok.span.text(src) == bare {
                results.push(Location {
                    uri: uri.clone(),
                    range: span_to_range(src, tok.span),
                });
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn build_single_file_workspace(
        uri_str: &str,
        source: &str,
    ) -> (SymbolTable, HashMap<usize, (Url, String)>) {
        let mut table = SymbolTable::new();
        let tokens = lexer::tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let symbols = SymbolTable::extract_symbols(fid, source, &file);
        table.set_file_symbols(fid, symbols);
        let mut files = HashMap::new();
        files.insert(
            fid,
            (Url::parse(uri_str).expect("url"), source.to_string()),
        );
        (table, files)
    }

    fn add_file(
        table: &mut SymbolTable,
        files: &mut HashMap<usize, (Url, String)>,
        uri_str: &str,
        source: &str,
    ) -> usize {
        let tokens = lexer::tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let symbols = SymbolTable::extract_symbols(fid, source, &file);
        table.set_file_symbols(fid, symbols);
        files.insert(
            fid,
            (Url::parse(uri_str).expect("url"), source.to_string()),
        );
        fid
    }

    #[test]
    fn name_at_position_finds_simple_ident() {
        // "void foo() {}"
        //       ^ column 6 is inside "foo"
        let src = "void foo() {}";
        let name = name_at_position(src, Position::new(0, 6));
        assert_eq!(name.as_deref(), Some("foo"));
    }

    #[test]
    fn name_at_position_returns_qualified_name() {
        // "Net::HttpRequest x;"
        //       ^ column 6 lies within "HttpRequest" (starts at col 5)
        let src = "Net::HttpRequest x;";
        let name = name_at_position(src, Position::new(0, 6));
        assert_eq!(name.as_deref(), Some("Net::HttpRequest"));
    }

    #[test]
    fn name_at_position_none_outside_ident() {
        // "void foo() {}"
        // Column 9 is the '(' character — not an identifier.
        let src = "void foo() {}";
        assert!(name_at_position(src, Position::new(0, 9)).is_none());
    }

    #[test]
    fn name_at_position_handles_triple_segment() {
        // cursor on "C" in "A::B::C"
        let src = "A::B::C x;";
        // column 6 is 'C'
        let name = name_at_position(src, Position::new(0, 6));
        assert_eq!(name.as_deref(), Some("A::B::C"));
    }

    #[test]
    fn goto_definition_finds_function_in_same_file() {
        let src = "void greet() {}\nvoid main() { greet(); }";
        let (table, files) = build_single_file_workspace("file:///tmp/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        // "greet" call is on line 1, starting at column 15
        let loc = goto_definition(src, Position::new(1, 16), &table, &ws)
            .expect("should resolve");
        assert_eq!(loc.uri.as_str(), "file:///tmp/a.as");
        // Definition span covers `void greet() {}` on line 0.
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_crosses_files() {
        let def_src = "void greet() {}\n";
        let use_src = "void main() { greet(); }\n";
        let mut table = SymbolTable::new();
        let mut files = HashMap::new();
        let _def_fid =
            add_file(&mut table, &mut files, "file:///tmp/def.as", def_src);
        let _use_fid =
            add_file(&mut table, &mut files, "file:///tmp/use.as", use_src);
        let ws = WorkspaceFiles { files: &files };
        // "greet" call in use.as starts at column 14
        let loc = goto_definition(use_src, Position::new(0, 15), &table, &ws)
            .expect("should resolve across files");
        assert_eq!(loc.uri.as_str(), "file:///tmp/def.as");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_returns_none_when_not_on_ident() {
        let src = "void greet() {}";
        let (table, files) = build_single_file_workspace("file:///tmp/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        // column 4 is the space between `void` and `greet`
        assert!(goto_definition(src, Position::new(0, 4), &table, &ws).is_none());
    }

    #[test]
    fn find_references_returns_all_uses() {
        let def_src = "void greet() {}\n";
        let use_src = "void main() { greet(); greet(); }\n";
        let mut table = SymbolTable::new();
        let mut files = HashMap::new();
        let _ = add_file(&mut table, &mut files, "file:///tmp/def.as", def_src);
        let _ = add_file(&mut table, &mut files, "file:///tmp/use.as", use_src);
        let ws = WorkspaceFiles { files: &files };
        // cursor on the definition of greet in def.as (column 6)
        let refs = find_references(def_src, Position::new(0, 6), &ws, true);
        // Expect: 1 hit in def.as + 2 hits in use.as = 3 total.
        assert_eq!(refs.len(), 3, "unexpected refs: {:?}", refs);
    }

    #[test]
    fn rename_replaces_all_references() {
        let def_src = "void greet() {}\n";
        let use_src = "void main() { greet(); greet(); }\n";
        let mut table = SymbolTable::new();
        let mut files = HashMap::new();
        let _ = add_file(&mut table, &mut files, "file:///tmp/def.as", def_src);
        let _ = add_file(&mut table, &mut files, "file:///tmp/use.as", use_src);
        let ws = WorkspaceFiles { files: &files };
        // cursor on the definition of greet in def.as (column 6)
        let edit = rename(def_src, Position::new(0, 6), "hello", &ws)
            .expect("should produce workspace edit");
        let changes = edit.changes.expect("changes present");
        // Expect edits in both files
        assert_eq!(changes.len(), 2, "expected edits in 2 files: {:?}", changes);
        let total: usize = changes.values().map(|v| v.len()).sum();
        assert_eq!(total, 3, "expected 3 total edits, got: {:?}", changes);
        for edits in changes.values() {
            for edit in edits {
                assert_eq!(edit.new_text, "hello");
            }
        }
    }

    #[test]
    fn rename_returns_none_when_not_on_ident() {
        let src = "void greet() {}";
        let (_table, files) = build_single_file_workspace("file:///tmp/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        // column 4 is the space between `void` and `greet`
        assert!(rename(src, Position::new(0, 4), "hello", &ws).is_none());
    }

    #[test]
    fn find_references_none_when_not_on_ident() {
        let src = "void greet() {}";
        let (_table, files) = build_single_file_workspace("file:///tmp/a.as", src);
        let ws = WorkspaceFiles { files: &files };
        let refs = find_references(src, Position::new(0, 4), &ws, true);
        assert!(refs.is_empty());
    }
}
