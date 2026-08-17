//! Goto-definition and find-references for the LSP.
//!
//! Both features share a position → symbol-name lookup that walks the token
//! stream at the cursor, reconstructing any `Ns::Name` prefix. Definitions
//! are resolved against the checker's resolution surface ([`GlobalScope`])
//! built from the open documents, and references are located via a pragmatic
//! token-scan (no AST-aware shadowing).
//!
//! Since GH #40 every entry point consumes a [`DocumentAnalysis`] — the
//! per-file view (masked source + AST) — instead of re-lexing/parsing raw
//! source on its own.

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::lexer::{self, TokenKind};
use crate::server::diagnostics::{position_to_offset, span_to_range};
use crate::symbols::scope::Symbol;
use crate::typecheck::GlobalScope;

/// Per-file view used by navigation / call hierarchy / workspace symbols:
/// `file_id` → `(uri, analysis)`. Built from an [`AnalysisSnapshot`] so
/// cross-file features reuse the snapshot's parse instead of re-lexing
/// every workspace file per request.
pub struct WorkspaceFiles<'a> {
    pub files: &'a HashMap<usize, (Url, &'a DocumentAnalysis)>,
}

impl<'a> WorkspaceFiles<'a> {
    pub fn get(&self, fid: usize) -> Option<&(Url, &'a DocumentAnalysis)> {
        self.files.get(&fid)
    }

    /// Source text (masked) of `fid`'s analysis.
    pub fn source_of(&self, fid: usize) -> Option<&'a str> {
        self.files.get(&fid).map(|(_, a)| a.masked_source())
    }
}

/// Find the qualified identifier name at the given position. Returns `None`
/// if the cursor is not over an identifier.
///
/// When the cursor is on the `Name` in `Ns::Sub::Name`, the returned value is
/// `"Ns::Sub::Name"`. When the cursor is on `Sub`, it is `"Ns::Sub"`.
pub fn ident_span_at(
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<(u32, u32, String)> {
    let (_, span, qual) = ident_index_at(analysis, position)?;
    Some((span.start, span.end, qual))
}

pub fn name_at_position(analysis: &DocumentAnalysis, position: Position) -> Option<String> {
    ident_span_at(analysis, position).map(|(_, _, name)| name)
}

/// Resolve the definition location of the symbol at `position`.
///
/// Resolution ladder:
/// 1. Qualified / bare workspace name via `lookup_reference` (real
///    definitions preferred over `import ... from` aliases).
/// 2. Class-member fallback for unqualified names inside a method body:
///    `this.Func` accesses and implicit-`this` calls resolve through the
///    enclosing class's inheritance chain (own class first, then
///    superclasses breadth-first).
///
/// Returns `None` if the cursor is not on an identifier, no matching symbol
/// exists, or the owning file is not in `files`.
pub fn goto_definition(
    analysis: &DocumentAnalysis,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Option<Location> {
    let (tok_idx, tok_span, qual) = ident_index_at(analysis, position)?;
    let source = analysis.masked_source();
    if let Some(sym) = lookup_workspace_symbol(scope, analysis, &qual, tok_span.start) {
        return symbol_location(sym, files);
    }

    // Member fallback: only for unqualified names (no `::` — `Ns::F` never
    // resolves as a member) inside a method of some class.
    if qual.contains("::") {
        return None;
    }
    let is_this_access = is_this_member_access(analysis, tok_idx);
    // Implicit-`this` call: a call callee with no receiver (`Func()`), i.e.
    // not preceded by `.` — `other.Func()` must NOT resolve against the
    // enclosing class's chain.
    let is_implicit_this_call =
        is_call_callee(analysis, tok_idx) && !is_member_access(analysis, tok_idx);
    if !is_this_access && !is_implicit_this_call {
        return None;
    }
    let file = &analysis.file;
    let offset = tok_span.start;
    let enclosing = enclosing_class_name(file, source, offset)?;
    let members = scope.lookup_member_symbols_with_inheritance(&enclosing, &qual);
    let sym = prefer_definition(&members)?;
    let (uri, def_analysis) = files.get(sym.file_id)?;
    Some(Location {
        uri: uri.clone(),
        range: span_to_range(def_analysis.masked_source(), sym.span),
    })
}

/// [`ident_span_at`] plus the token index — the fallback needs the token's
/// neighbours (`this.` prefix, call `(`) to classify the use site.
fn ident_index_at(
    analysis: &DocumentAnalysis,
    position: Position,
) -> Option<(usize, crate::lexer::Span, String)> {
    let source = analysis.masked_source();
    let offset = position_to_offset(source, position);
    let tokens = &analysis.tokens;
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
    Some((idx, token.span, parts.join("::")))
}

/// True when the identifier at `tok_idx` is the member of a `this.X`
/// access (preceded by `this .`).
fn is_this_member_access(analysis: &DocumentAnalysis, tok_idx: usize) -> bool {
    let tokens = &analysis.tokens;
    tok_idx >= 2
        && tokens[tok_idx - 1].kind == TokenKind::Dot
        && tokens[tok_idx - 2].kind == TokenKind::KwThis
}

/// True when the identifier at `tok_idx` is any member access
/// (preceded by `.`).
fn is_member_access(analysis: &DocumentAnalysis, tok_idx: usize) -> bool {
    tok_idx >= 1 && analysis.tokens[tok_idx - 1].kind == TokenKind::Dot
}

/// True when the identifier at `tok_idx` is a call callee (immediately
/// followed by `(`).
fn is_call_callee(analysis: &DocumentAnalysis, tok_idx: usize) -> bool {
    analysis
        .tokens
        .get(tok_idx + 1)
        .map(|t| t.kind == TokenKind::LParen)
        .unwrap_or(false)
}

/// Namespace-qualified name of the class whose body contains `offset`, when
/// the position sits inside any class (directly or inside a method).
fn enclosing_class_name(
    file: &crate::parser::ast::SourceFile,
    source: &str,
    offset: u32,
) -> Option<String> {
    /// Depth-first search for the innermost enclosing class, tracking the
    /// namespace prefix (`Ns::Outer::Inner`).
    fn walk<'a>(
        item: &'a crate::parser::ast::Item,
        offset: u32,
        source: &str,
        ns: &[String],
        best: &mut Option<(String, u32)>,
    ) {
        use crate::parser::ast::Item;
        match item {
            Item::Namespace(nsd) => {
                if nsd.span.start <= offset && offset <= nsd.span.end {
                    let mut nested = ns.to_vec();
                    nested.push(nsd.name.text(source).to_string());
                    for sub in &nsd.items {
                        walk(sub, offset, source, &nested, best);
                    }
                }
            }
            Item::Class(cls) => {
                if cls.span.start <= offset && offset <= cls.span.end {
                    let mut name = ns.join("::");
                    if !name.is_empty() {
                        name.push_str("::");
                    }
                    name.push_str(cls.name.text(source));
                    let width = cls.span.end - cls.span.start;
                    if best.as_ref().map(|(_, w)| width < *w).unwrap_or(true) {
                        *best = Some((name, width));
                    }
                }
            }
            _ => {}
        }
    }
    let mut best: Option<(String, u32)> = None;
    for item in &file.items {
        walk(item, offset, source, &[], &mut best);
    }
    best.map(|(name, _)| name)
}

/// Namespace path enclosing `offset` (`["Outer", "Inner"]`), innermost
/// last. Drives the qualified-key fallback in
/// [`goto_definition`](crate::server::navigation::goto_definition): bare
/// references inside a namespace resolve against `Ns::name` symbol keys.
fn enclosing_namespace_path(
    file: &crate::parser::ast::SourceFile,
    source: &str,
    offset: u32,
) -> Vec<String> {
    fn walk<'a>(
        items: &'a [crate::parser::ast::Item],
        offset: u32,
        source: &str,
        path: &mut Vec<String>,
    ) {
        for item in items {
            if let crate::parser::ast::Item::Namespace(nsd) = item {
                if nsd.span.start <= offset && offset <= nsd.span.end {
                    path.push(nsd.name.text(source).to_string());
                    walk(&nsd.items, offset, source, path);
                }
            }
        }
    }
    // Identifier text is byte-identical between the original and masked
    // sources (masking replaces comments/strings with same-length blanks).
    let mut path = Vec::new();
    walk(&file.items, offset, source, &mut path);
    path
}

/// Resolve a symbol to a [`Location`] when its file is open.
fn symbol_location(sym: &Symbol, files: &WorkspaceFiles) -> Option<Location> {
    let (uri, def_analysis) = files.get(sym.file_id)?;
    Some(Location {
        uri: uri.clone(),
        range: span_to_range(def_analysis.masked_source(), sym.span),
    })
}

/// Namespace-aware workspace lookup for the name at a use site: exact key,
/// then enclosing-namespace-qualified keys innermost→outermost (the symbol
/// table registers namespaced decls as `Ns::name`), preferring real
/// definitions over `import ... from` aliases.
pub fn lookup_workspace_symbol<'a>(
    scope: &'a GlobalScope<'_>,
    analysis: &DocumentAnalysis,
    name: &str,
    use_offset: u32,
) -> Option<&'a Symbol> {
    let candidates = scope.lookup_reference(name);
    if let Some(sym) = prefer_definition(&candidates) {
        return Some(sym);
    }
    if name.contains("::") {
        return None;
    }
    let source = analysis.masked_source();
    let ns_path = enclosing_namespace_path(&analysis.file, source, use_offset);
    for depth in (1..=ns_path.len()).rev() {
        let qualified = format!("{}::{}", ns_path[..depth].join("::"), name);
        let candidates = scope.lookup_reference(&qualified);
        if let Some(sym) = prefer_definition(&candidates) {
            return Some(sym);
        }
    }
    None
}

/// Pick the best candidate from a `lookup_reference` result: a real
/// definition if any exists, else the first import-site alias.
pub fn prefer_definition<'s>(candidates: &[&'s Symbol]) -> Option<&'s Symbol> {
    candidates
        .iter()
        .find(|s| !s.is_import_alias())
        .copied()
        .or_else(|| candidates.first().copied())
}

/// Build a workspace rename edit replacing every textual reference to the
/// identifier under `position` with `new_name`.
///
/// Uses the same token-scan strategy as [`find_references`] — every `Ident`
/// token in the open workspace whose text matches the cursor's bare name is
/// rewritten. Returns `None` if the cursor is not on an identifier or if no
/// matches were found.
pub fn rename(
    analysis: &DocumentAnalysis,
    position: Position,
    new_name: &str,
    files: &WorkspaceFiles,
) -> Option<WorkspaceEdit> {
    let qual = name_at_position(analysis, position)?;
    let bare = qual.rsplit("::").next().unwrap_or(&qual).to_string();
    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for (_fid, (uri, analysis)) in files.files.iter() {
        let src = analysis.masked_source();
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
    analysis: &DocumentAnalysis,
    position: Position,
    files: &WorkspaceFiles,
    _include_declaration: bool,
) -> Vec<Location> {
    let Some(qual) = name_at_position(analysis, position) else {
        return Vec::new();
    };
    let bare = qual.rsplit("::").next().unwrap_or(&qual);
    let mut results = Vec::new();
    for (_fid, (uri, analysis)) in files.files.iter() {
        let src = analysis.masked_source();
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

    fn build_single_file_workspace(
        uri_str: &str,
        source: &str,
    ) -> crate::server::test_support::TestWorkspace {
        let name = uri_str.rsplit('/').next().unwrap_or("a.as");
        crate::server::test_support::TestWorkspace::one_file(name, source)
    }

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn name_at_position_finds_bare_identifier() {
        let src = "void main() { Foo foo; }\n";
        let analysis = DocumentAnalysis::analyze_plain(src);
        // Cursor over `Foo` (decl type position).
        let name = name_at_position(&analysis, pos(0, 14)).unwrap();
        assert_eq!(name, "Foo");
    }

    #[test]
    fn name_at_position_reconstructs_qualified_name() {
        let src = "void main() { Ns::Sub::Name(); }\n";
        let analysis = DocumentAnalysis::analyze_plain(src);
        let name = name_at_position(&analysis, pos(0, 23)).unwrap();
        assert_eq!(name, "Ns::Sub::Name");
    }

    #[test]
    fn goto_definition_inside_namespace_variable_and_function() {
        // Bare reference inside a namespace must resolve to the
        // namespace-qualified symbol (Ns::target / Ns::greet), which is
        // how the symbol table registers them.
        let src = "namespace Ns {\n    int target = 1;\n    void greet() {}\n    void main() { target; greet(); }\n}";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();

        // Cursor on `target` inside main (line 3: `    void main() { target; greet(); }`).
        let col = src.lines().nth(3).unwrap().find("target").unwrap() as u32;
        let loc = goto_definition(analysis, pos(3, col + 2), &scope, &ws_files);
        assert!(loc.is_some(), "variable in namespace did not resolve");
        let loc = loc.unwrap();
        assert_eq!(loc.uri, tw.uri());
        assert_eq!(loc.range.start.line, 1, "should point at `int target` line");

        // Cursor on `greet` inside main.
        let col = src.lines().nth(3).unwrap().find("greet").unwrap() as u32;
        let loc = goto_definition(analysis, pos(3, col + 2), &scope, &ws_files);
        assert!(loc.is_some(), "function in namespace did not resolve");
        let loc = loc.unwrap();
        assert_eq!(loc.range.start.line, 2, "should point at `void greet` line");
    }

    #[test]
    fn goto_definition_resolves_single_file() {
        let src = "int target = 1;\nvoid main() { int x = target; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let loc = goto_definition(analysis, pos(1, 26), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, tw.uri());
        // Points at the declaration (line 0), not the use.
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_prefers_definition_over_import() {
        // A name declared both by an `import ... from` statement (file A) and
        // a real function definition (file B) must resolve to the definition:
        // the import is just a local alias declaration for it. File A is
        // registered first, so a naive first-match lookup lands on the import.
        let imports_src = "import void DoThing(int x) from 'Other';\nvoid main() { DoThing(1); }\n";
        let defs_src = "void DoThing(int x) { }\n";
        let snap = crate::analysis_snapshot::AnalysisSnapshot::from_files(
            &[
                (
                    std::path::PathBuf::from("/test/imports.as"),
                    imports_src.to_string(),
                ),
                (
                    std::path::PathBuf::from("/test/defs.as"),
                    defs_src.to_string(),
                ),
            ],
            &crate::config::LspConfig::default(),
        );
        let files = snap.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = crate::typecheck::GlobalScope::new(snap.symbols(), None);
        let analysis = &snap.files()[0].analysis;
        // Cursor on the `DoThing` call site (line 1, char 14).
        let loc = goto_definition(analysis, pos(1, 14), &scope, &ws_files).unwrap();
        assert_eq!(
            loc.uri, files[&1].0,
            "must jump to the definition file, not the importing file"
        );
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_falls_back_to_import_when_no_definition() {
        // When the real definition is not part of the workspace, the import
        // site is still the best (only) answer.
        let src = "import void Ext(int x) from 'Other';\nvoid main() { Ext(1); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let loc = goto_definition(analysis, pos(1, 14), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, tw.uri());
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_this_inherited_method_resolves_to_parent() {
        // F2 on `Func` in `this.Func()` inside B (inherits A) must land on
        // A::Func — the method is not declared on B itself.
        let src = "class A { void Func() {} }\nclass B : A { void Run() { this.Func(); } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        // `Func` spans chars 32..36 on line 1.
        let loc = goto_definition(analysis, pos(1, 33), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, tw.uri());
        // Line 0 is `class A { ... }` — the parent declaration.
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_implicit_this_inherited_method_resolves_to_parent() {
        // Bare `Func()` call inside B — implicit this, same resolution.
        let src = "class A { void Func() {} }\nclass B : A { void Run() { Func(); } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        // `Func` starts at char 27 on line 1.
        let loc = goto_definition(analysis, pos(1, 29), &scope, &ws_files).unwrap();
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_this_member_on_own_class_resolves() {
        // `this.Own()` — declared on B itself, also indexed `B::Own`.
        let src = "class A { }\nclass B : A { void Own() {} void Run() { this.Own(); } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        // `Own` (the call site) spans chars 46..49 on line 1.
        let loc = goto_definition(analysis, pos(1, 47), &scope, &ws_files).unwrap();
        // `Own` is declared on B (line 1).
        assert_eq!(loc.range.start.line, 1);
    }

    #[test]
    fn goto_definition_local_variable_not_mistaken_for_member() {
        // A local named like a parent method must NOT resolve to the method:
        // bare-name fallback is only for this-accesses and call sites.
        let src =
            "class A { void Func() {} }\nclass B : A { void Run() { int Func = 1; Func; } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        // Bare `Func;` expression starts at char 41 on line 1.
        let loc = goto_definition(analysis, pos(1, 43), &scope, &ws_files);
        // `int Func = 1;` declares no global symbol; the member fallback must
        // not fire for a non-call, non-this use of a shadowing local name.
        // (If locals were ever indexed, this must point at the local decl.)
        assert!(
            loc.is_none(),
            "bare local use must not resolve to A::Func, got {loc:?}"
        );
    }

    #[test]
    fn goto_definition_inherited_method_cross_file() {
        // Parent class in another file: F2 must jump across files.
        let child_src = "class B : A { void Run() { this.Func(); } }\n";
        let parent_src = "class A { void Func() {} }\n";
        let snap = crate::analysis_snapshot::AnalysisSnapshot::from_files(
            &[
                (
                    std::path::PathBuf::from("/test/child.as"),
                    child_src.to_string(),
                ),
                (
                    std::path::PathBuf::from("/test/parent.as"),
                    parent_src.to_string(),
                ),
            ],
            &crate::config::LspConfig::default(),
        );
        let files = snap.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = crate::typecheck::GlobalScope::new(snap.symbols(), None);
        let analysis = &snap.files()[0].analysis;
        // `Func` spans chars 32..36 on line 0 of child.as.
        let loc = goto_definition(analysis, pos(0, 33), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, files[&1].0, "must jump to parent.as");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_receiver_call_not_resolved_against_enclosing_class() {
        // `other.Func()` — a receiver call must NOT fall back to the
        // enclosing class's inheritance chain (the receiver's own type owns
        // resolution, which the global ladder already covers for
        // class-typed receivers registered as globals).
        let src =
            "class A { void Func() {} }\nclass B : A { void Run(B other) { other.Func(); } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        // `Func` in `other.Func()` at line 1 char 47.
        let loc = goto_definition(analysis, pos(1, 48), &scope, &ws_files);
        assert!(
            loc.is_none() || loc.unwrap().range.start.line == 1,
            "receiver call must not resolve to A::Func (line 0)"
        );
    }

    #[test]
    fn find_references_scans_open_documents() {
        let src = "int counter = 0;\nvoid main() { counter += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let refs = find_references(analysis, pos(1, 15), &ws_files, true);
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn rename_rewrites_matching_identifiers() {
        let src = "int counter = 0;\nvoid main() { counter += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let edit = rename(analysis, pos(1, 15), "tally", &ws_files).unwrap();
        let changes = edit.changes.unwrap();
        let edits = changes.get(&tw.uri()).unwrap();
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|e| e.new_text == "tally"));
    }
}
