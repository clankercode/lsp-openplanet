//! Goto-definition for the LSP, and the position → symbol-name lookup its
//! neighbours share.
//!
//! [`ident_index_at`] walks the token stream at the cursor, reconstructing
//! any `Ns::Name` prefix, and definitions are resolved against the
//! checker's resolution surface ([`GlobalScope`]) built from the open
//! documents.
//!
//! Rename and find-references used to live here as a workspace-wide token
//! scan with no scope analysis. B049 moved both to
//! [`crate::server::rename`], which classifies the symbol under the cursor
//! and picks an occurrence rule per class instead.
//!
//! Since GH #40 every entry point consumes a [`DocumentAnalysis`] — the
//! per-file view (masked source + AST) — instead of re-lexing/parsing raw
//! source on its own.

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::analysis_snapshot::CheckedFile;
use crate::lexer::TokenKind;
use crate::server::diagnostics::{position_to_offset, span_to_range};
use crate::symbols::scope::Symbol;
use crate::typecheck::repr::TypeRepr;
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
/// 0. `receiver.member` — resolved against the receiver's recorded type,
///    walking its inheritance chain. Hover has had this step since B033;
///    goto-definition had none at all, so `c.Go()` on a class-typed
///    receiver resolved nothing and `i.Go()` on an interface-typed one
///    resolved nothing twice over (B054).
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
    checked: Option<&CheckedFile<'_>>,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Option<Location> {
    let (tok_idx, tok_span, qual) = ident_index_at(analysis, position)?;
    let source = analysis.masked_source();
    // 0) `receiver.member`: the member belongs to the receiver's type, so
    //    resolve it THERE and nowhere else. Once the receiver's type is
    //    known, a miss is an answer — falling through to the by-name
    //    ladder below would jump to an unrelated global that merely
    //    shares the member's spelling, which is what `x.Nope()` did
    //    before B054. A receiver whose type is NOT known (no checked
    //    view, an unresolvable expression) still falls through, since
    //    there is nothing better to say.
    if is_member_access(analysis, tok_idx) && !qual.contains("::") {
        if let Some(owner) = receiver_type_name(analysis, checked, tok_idx, tok_span.start) {
            let members = scope.lookup_member_symbols_with_inheritance(&owner, &qual);
            return prefer_definition(&members).and_then(|sym| symbol_location(sym, files));
        }
    }
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

// === receiver.member support (shared with hover) ======================

/// Fully qualified name of the type the `.` on the left of `tok_idx` is
/// applied to. `this` resolves to the enclosing class without consulting the
/// checker; anything else uses the recorded type of the receiver expression.
pub(crate) fn receiver_type_name(
    analysis: &DocumentAnalysis,
    checked: Option<&CheckedFile<'_>>,
    tok_idx: usize,
    offset: u32,
) -> Option<String> {
    let tokens = &analysis.tokens;
    let last = tok_idx.checked_sub(2)?;
    if tokens.get(last)?.kind == TokenKind::KwThis {
        // NAMESPACE-QUALIFIED (`Ns::C`, not `C`): a bare name binds the
        // first workspace symbol spelled that way, so an unqualified
        // answer retargets every `this.` inside a namespaced class onto a
        // same-named top-level class. `lookup_member_symbols_with_inheritance`
        // normalizes either form, so qualifying costs nothing.
        return enclosing_class_name(&analysis.file, analysis.masked_source(), offset);
    }
    base_type_name(receiver_type(tokens, checked?, last)?)
}

/// Recorded type of the receiver expression whose last token is `last`.
///
/// Two shapes, because the parser spans them differently:
///
/// * A BARE parenthesised group — `(x)`, `(q ? a : b)` — is given the span of
///   the expression INSIDE it, so no recorded span contains the closing
///   paren. Query the inner token range directly.
/// * Everything else — `a`, `a.b`, `Ns::a`, `f(x)`, `items[0]`,
///   `cast<T>(x)` — is spanned through its last token, so the innermost
///   recorded expression containing that token's final byte IS the receiver.
///   Using containment rather than an exact range is what makes nested calls
///   and postfix chains work without re-parsing them.
///
/// `None` when nothing is recorded there, and deliberately no fall-back
/// between the two shapes: a miss on the group range would otherwise pick up
/// the enclosing call and report the wrong type.
fn receiver_type<'c>(
    tokens: &[crate::lexer::Token],
    checked: &'c CheckedFile<'_>,
    last: usize,
) -> Option<&'c TypeRepr> {
    let mut last = last;
    while tokens[last].kind == TokenKind::RParen {
        let Some(open) = matching_open(tokens, last) else {
            break;
        };
        // `f(...)` and `cast<T>(...)` are postfix forms, not groups — their
        // recorded span does cover the closing paren, so leave them to the
        // containment query below.
        let postfix = open
            .checked_sub(1)
            .map(|p| tokens[p].kind)
            .is_some_and(|k| {
                matches!(
                    k,
                    TokenKind::Ident | TokenKind::RParen | TokenKind::RBracket | TokenKind::Gt
                )
            });
        if postfix {
            break;
        }
        if open + 1 >= last {
            return None;
        }
        // `((x))`: the parser keeps only the innermost expression's span, so
        // a group whose entire contents are another group has to be peeled
        // again rather than queried (it would name a range nothing recorded).
        if tokens[last - 1].kind == TokenKind::RParen
            && matching_open(tokens, last - 1) == Some(open + 1)
        {
            last -= 1;
            continue;
        }
        return checked.type_at_span_range(tokens[open + 1].span.start, tokens[last - 1].span.end);
    }
    checked.type_at_offset(tokens[last].span.end.checked_sub(1)?)
}

/// Index of the bracket opening the one that closes at `close`.
fn matching_open(tokens: &[crate::lexer::Token], close: usize) -> Option<usize> {
    let (open_kind, close_kind) = match tokens[close].kind {
        TokenKind::RParen => (TokenKind::LParen, TokenKind::RParen),
        TokenKind::RBracket => (TokenKind::LBracket, TokenKind::RBracket),
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = close;
    loop {
        if tokens[i].kind == close_kind {
            depth += 1;
        } else if tokens[i].kind == open_kind {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i = i.checked_sub(1)?;
    }
}

/// Strip handle / const / array wrappers down to the named type a member
/// lookup can be run against.
pub(crate) fn base_type_name(ty: &TypeRepr) -> Option<String> {
    match ty {
        TypeRepr::Named(name) | TypeRepr::Generic { base: name, .. } | TypeRepr::Funcdef(name) => {
            Some(name.clone())
        }
        TypeRepr::Handle(inner) | TypeRepr::Const(inner) => base_type_name(inner),
        _ => None,
    }
}

/// Token index of the identifier under `position`. Hover needs the token's
/// neighbours to tell a `receiver.member` access from a bare name (B033).
pub fn ident_token_index_at(analysis: &DocumentAnalysis, position: Position) -> Option<usize> {
    ident_index_at(analysis, position).map(|(idx, _, _)| idx)
}

/// [`ident_span_at`] plus the token index — the fallback needs the token's
/// neighbours (`this.` prefix, call `(`) to classify the use site.
pub fn ident_index_at(
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
    prefer_definition(&lookup_workspace_symbols(scope, analysis, name, use_offset))
}

/// Every workspace candidate for the name at a use site, walking the same
/// ladder [`lookup_workspace_symbol`] picks its single winner from. Hover
/// renders the whole set so overloaded functions are not reduced to one
/// signature (B033).
pub fn lookup_workspace_symbols<'a>(
    scope: &'a GlobalScope<'_>,
    analysis: &DocumentAnalysis,
    name: &str,
    use_offset: u32,
) -> Vec<&'a Symbol> {
    let candidates = scope.lookup_reference(name);
    if !candidates.is_empty() {
        return candidates;
    }
    if name.contains("::") {
        return Vec::new();
    }
    let source = analysis.masked_source();
    let ns_path = enclosing_namespace_path(&analysis.file, source, use_offset);
    for depth in (1..=ns_path.len()).rev() {
        let qualified = format!("{}::{}", ns_path[..depth].join("::"), name);
        let candidates = scope.lookup_reference(&qualified);
        if !candidates.is_empty() {
            return candidates;
        }
    }
    Vec::new()
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
        let loc = goto_definition(analysis, None, pos(3, col + 2), &scope, &ws_files);
        assert!(loc.is_some(), "variable in namespace did not resolve");
        let loc = loc.unwrap();
        assert_eq!(loc.uri, tw.uri());
        assert_eq!(loc.range.start.line, 1, "should point at `int target` line");

        // Cursor on `greet` inside main.
        let col = src.lines().nth(3).unwrap().find("greet").unwrap() as u32;
        let loc = goto_definition(analysis, None, pos(3, col + 2), &scope, &ws_files);
        assert!(loc.is_some(), "function in namespace did not resolve");
        let loc = loc.unwrap();
        assert_eq!(loc.range.start.line, 2, "should point at `void greet` line");
    }

    /// B063: an Openplanet-injected method has NO AngelScript declaration
    /// anywhere — it is registered from C++ — so there is nothing to jump to.
    ///
    /// The requirement is therefore not that this resolves, but that it
    /// degrades gracefully: no location, no error, no panic. An unhandled
    /// `None` on this path surfaces as a client-side crash rather than a
    /// missing feature, which is why it is pinned rather than left to
    /// inspection. Completion now OFFERS these methods, so users will click
    /// through to them, which is exactly what makes this path newly reachable.
    #[test]
    fn goto_definition_on_an_injected_method_degrades_gracefully() {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        let idx = crate::typedb::TypeIndex::load(
            &fixtures.join("OpenplanetCore.json"),
            &fixtures.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load");
        for (src, needle) in [
            (
                "void f(CSmPlayer@ p) { p.GetCurrentEntityID(); }\n",
                "GetCurrentEntityID",
            ),
            ("void f(CMwNod@ n) { n.MwAddRef(); }\n", "MwAddRef"),
            (
                "void f(CGameNetwork@ n) { n.GetManialinkPages(); }\n",
                "GetManialinkPages",
            ),
        ] {
            let tw = build_single_file_workspace("file:///t/inj.as", src);
            let analysis = tw.analysis();
            let files = tw.uri_map();
            let ws_files = WorkspaceFiles { files: &files };
            let scope = GlobalScope::new(tw.snapshot.symbols(), Some(&idx));
            let col = src.find(needle).expect("needle") as u32;
            // Must not panic, and must not invent a location.
            let loc = goto_definition(analysis, None, pos(0, col + 1), &scope, &ws_files);
            assert!(
                loc.is_none(),
                "an injected method has no source declaration to jump to, got {loc:?} for {needle}"
            );
        }
    }

    #[test]
    fn goto_definition_resolves_single_file() {
        let src = "int target = 1;\nvoid main() { int x = target; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let loc = goto_definition(analysis, None, pos(1, 26), &scope, &ws_files).unwrap();
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
        let loc = goto_definition(analysis, None, pos(1, 14), &scope, &ws_files).unwrap();
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
        let loc = goto_definition(analysis, None, pos(1, 14), &scope, &ws_files).unwrap();
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
        let loc = goto_definition(analysis, None, pos(1, 33), &scope, &ws_files).unwrap();
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
        let loc = goto_definition(analysis, None, pos(1, 29), &scope, &ws_files).unwrap();
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
        let loc = goto_definition(analysis, None, pos(1, 47), &scope, &ws_files).unwrap();
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
        let loc = goto_definition(analysis, None, pos(1, 43), &scope, &ws_files);
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
        let loc = goto_definition(analysis, None, pos(0, 33), &scope, &ws_files).unwrap();
        assert_eq!(loc.uri, files[&1].0, "must jump to parent.as");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_receiver_call_not_resolved_against_enclosing_class() {
        // `other.Func()` — a receiver call must NOT fall back to the
        // ENCLOSING class's inheritance chain. `Other` is unrelated to the
        // enclosing `B : A`, so the only way to reach `A::Func` from here
        // is the illegitimate fallback. (Before B054 the receiver's own
        // type resolved nothing either; now it does, which is why the
        // receiver has to be an unrelated type for this to still test the
        // fallback rather than the new step 0.)
        let src = "class A { void Func() {} }\nclass Other {}\nclass B : A { void Run(Other other) { other.Func(); } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let col = src.lines().nth(2).unwrap().find("other.Func").unwrap() as u32 + 7;
        let loc = goto_definition(analysis, None, pos(2, col), &scope, &ws_files);
        assert!(
            loc.is_none(),
            "receiver call must not resolve against the enclosing class, got {loc:?}"
        );
    }

    // === B054: `receiver.member` goto-definition ==========================
    //
    // Before B054 goto-definition had NO receiver-type step at all: the
    // ladder tried the bare name as a global and then only the `this.` /
    // implicit-`this` fallback, so `c.Go()` on a class-typed receiver
    // returned None just as `i.Go()` on an interface-typed one did. Both
    // halves of that are fixed here — the interface half additionally
    // needed `I::Go` to exist in the pool at all.

    /// Build a workspace, run goto-definition at the character `needle`
    /// starts on plus `offset`, with the checker's recorded types wired in
    /// exactly as the `textDocument/definition` handler wires them.
    fn goto_at(
        tw: &crate::server::test_support::TestWorkspace,
        file: usize,
        line: usize,
        needle: &str,
        offset: u32,
    ) -> Option<Location> {
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let analysis = tw.analysis_of(file);
        let source = analysis.masked_source();
        let col = source.lines().nth(line).unwrap().find(needle).unwrap() as u32 + offset;
        let checked = tw.snapshot.checked_file(&tw.uri_of(file), &scope);
        goto_definition(
            analysis,
            checked.as_ref(),
            pos(line as u32, col),
            &scope,
            &ws_files,
        )
    }

    #[test]
    fn goto_definition_interface_method_on_an_interface_typed_receiver() {
        let src =
            "interface I { void Go(); }\nclass C : I { void Go() {} }\nvoid f(I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "i.Go", 2).expect("i.Go() must resolve to I::Go");
        assert_eq!(
            loc.range.start.line, 0,
            "must land on the interface declaration, not the class implementation"
        );
    }

    #[test]
    fn goto_definition_class_method_on_a_class_typed_receiver() {
        // The class twin of the case above: the receiver step is not
        // interface-specific.
        let src =
            "interface I { void Go(); }\nclass C : I { void Go() {} }\nvoid f(C@ c) { c.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "c.Go", 2).expect("c.Go() must resolve to C::Go");
        assert_eq!(
            loc.range.start.line, 1,
            "must land on the class implementation, not the interface declaration"
        );
    }

    #[test]
    fn goto_definition_interface_method_never_implemented() {
        // Nothing implements `I`, so the interface declaration is the only
        // definition there is — it must still be reachable.
        let src = "interface I { void Go(); }\nvoid f(I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "i.Go", 2).expect("i.Go() must resolve");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_interface_method_across_files() {
        let tw = crate::server::test_support::TestWorkspace::files(&[
            ("use", "void f(I@ i) { i.Go(); }\n"),
            ("iface", "interface I { void Go(); }\n"),
        ]);
        let loc = goto_at(&tw, 0, 0, "i.Go", 2).expect("cross-file interface method must resolve");
        assert_eq!(loc.uri, tw.uri_of(1), "must jump to iface.as");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_interface_method_inherited_from_a_base_interface() {
        let src = "interface A { void Go(); }\ninterface B : A {}\nvoid f(B@ b) { b.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "b.Go", 2).expect("inherited interface method must resolve");
        assert_eq!(loc.range.start.line, 0, "must land on A::Go");
    }

    #[test]
    fn goto_definition_interface_method_in_a_namespace() {
        let src = "namespace Ns { interface I { void Go(); } }\nvoid f(Ns::I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "i.Go", 2).expect("namespaced interface method must resolve");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_two_interfaces_same_method_name_pick_the_receivers() {
        // `A::Go` and `B::Go` share a spelling; the receiver's type decides.
        let src =
            "interface A { void Go(); }\ninterface B { void Go(); }\nvoid f(B@ b) { b.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "b.Go", 2).expect("must resolve");
        assert_eq!(loc.range.start.line, 1, "must land on B::Go, not A::Go");
    }

    #[test]
    fn goto_definition_unknown_interface_member_resolves_nothing() {
        // A same-named GLOBAL exists, so `None` here is a real assertion:
        // without it the by-name ladder would happily land on line 0 and
        // the test would pass for the wrong reason.
        let src = "void Nope() {}\ninterface I { void Go(); }\nvoid f(I@ i) { i.Nope(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "i.Nope", 2);
        assert!(
            loc.is_none(),
            "a member the interface does not declare must not fall through to the \
             same-named free function, got {loc:?}"
        );
    }

    /// A `this.` receiver must resolve against the LEXICAL class, namespace
    /// and all. Step 0 resolves the receiver's type by name, and a bare
    /// `C` binds the top-level `C` as soon as one exists — so an
    /// unqualified answer here silently retargets every `this.` inside a
    /// namespaced class.
    #[test]
    fn goto_definition_this_member_prefers_the_namespaced_enclosing_class() {
        let src = "class C { void M() {} }\nnamespace Ns { class C { void M() {} void F() { this.M(); } } }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "this.M", 5).expect("this.M() must resolve");
        assert_eq!(
            loc.range.start.line, 1,
            "must land on Ns::C::M (line 1), not the top-level C::M (line 0)"
        );
    }

    #[test]
    fn goto_definition_member_access_prefers_the_receiver_over_a_same_named_global() {
        // A free function spelled like the member must not win: the
        // receiver's type owns `.Go`.
        let src = "void Go() {}\ninterface I { void Go(); }\nvoid f(I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "i.Go", 2).expect("must resolve");
        assert_eq!(
            loc.range.start.line, 1,
            "must land on I::Go (line 1), not the free function (line 0)"
        );
    }
}
