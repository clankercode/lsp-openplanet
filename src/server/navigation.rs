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
use crate::symbols::scope::{Symbol, SymbolKind};
use crate::typecheck::access_position::{access_position_at, AccessPosition};
use crate::typecheck::global_scope::qualified_virtual_name;
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
/// Returns an EMPTY vec if the cursor is not on an identifier, no matching
/// symbol exists, or the owning file is not in `files`.
///
/// The result is a list rather than a single location because one gesture
/// can legitimately have several answers: a compound assign to a virtual
/// property touches both of its accessors (B103). Callers that need the
/// single best answer take the first — the list is ordered best-first.
pub fn goto_definition(
    analysis: &DocumentAnalysis,
    checked: Option<&CheckedFile<'_>>,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Vec<Location> {
    let Some((tok_idx, tok_span, qual)) = ident_index_at(analysis, position) else {
        return Vec::new();
    };
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
            if !members.is_empty() {
                return prefer_definition(&members)
                    .and_then(|sym| symbol_location(sym, files))
                    .into_iter()
                    .collect();
            }
            // B103: no member is spelled `qual`, but the type may still
            // declare it as a VIRTUAL PROPERTY — a `get_`/`set_` pair. The
            // exact lookup above runs first so a real field named `ID`
            // always beats an accessor named for it.
            let position = access_position_at(&analysis.file, tok_span);
            return scope
                .lookup_virtual_property_accessor_symbols(&owner, &qual, position)
                .into_iter()
                .filter_map(|sym| symbol_location(sym, files))
                .collect();
        }
    }
    // B103, the namespace/global half. Each lexical namespace level is an
    // exact scope: first its real `Thing`, then its `get_Thing`/`set_Thing`
    // pair. Only after both miss do we move outward. Explicitly-qualified
    // names have one level and never degrade to a root or unrelated tail.
    //
    // Guarded to non-member sites — a member access whose receiver type is
    // unknown must not go hunting for a same-named global accessor, which
    // is the over-reach B054 removed from this ladder.
    if !is_member_access(analysis, tok_idx) {
        let position = access_position_at(&analysis.file, tok_span);
        if let Some(symbols) =
            lookup_workspace_property_site(scope, analysis, &qual, tok_span.start, position)
        {
            return symbols
                .into_iter()
                .filter_map(|sym| symbol_location(sym, files))
                .collect();
        }
        if qual.contains("::") {
            return Vec::new();
        }
    }
    if let Some(sym) = lookup_workspace_symbol(scope, analysis, &qual, tok_span.start) {
        return symbol_location(sym, files).into_iter().collect();
    }

    // Member fallback: only for unqualified names (no `::` — `Ns::F` never
    // resolves as a member) inside a method of some class.
    if qual.contains("::") {
        return Vec::new();
    }
    let is_this_access = is_this_member_access(analysis, tok_idx);
    // Implicit-`this` call: a call callee with no receiver (`Func()`), i.e.
    // not preceded by `.` — `other.Func()` must NOT resolve against the
    // enclosing class's chain.
    let is_implicit_this_call =
        is_call_callee(analysis, tok_idx) && !is_member_access(analysis, tok_idx);
    if !is_this_access && !is_implicit_this_call {
        return Vec::new();
    }
    let file = &analysis.file;
    let offset = tok_span.start;
    let Some(enclosing) = enclosing_class_name(file, source, offset) else {
        return Vec::new();
    };
    let members = scope.lookup_member_symbols_with_inheritance(&enclosing, &qual);
    prefer_definition(&members)
        .and_then(|sym| symbol_location(sym, files))
        .into_iter()
        .collect()
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

/// Resolve a workspace-level property site through exact lexical scopes.
///
/// At every level, an ordinary declaration wins over virtual accessors. A
/// name (bare or partially qualified) walks nearest namespace to outer
/// namespace and finally its root spelling. The use of
/// [`GlobalScope::lookup_workspace`] is deliberate: `lookup_reference`
/// widens an exact miss to root/tail matches, which is invalid for a virtual
/// property whose namespace is part of its identity.
///
/// `None` means there was no exact ordinary declaration or accessor at any
/// valid level. Callers may retain conventional fallback for a bare name;
/// qualified names must treat that miss as final.
fn lookup_workspace_property_site<'a>(
    scope: &'a GlobalScope<'_>,
    analysis: &DocumentAnalysis,
    name: &str,
    use_offset: u32,
    position: AccessPosition,
) -> Option<Vec<&'a Symbol>> {
    let explicitly_qualified = name.contains("::");
    let mut candidates = Vec::new();
    let source = analysis.masked_source();
    let namespace = enclosing_namespace_path(&analysis.file, source, use_offset);
    for depth in (1..=namespace.len()).rev() {
        candidates.push(format!("{}::{name}", namespace[..depth].join("::")));
    }
    candidates.push(name.to_owned());

    for qualified in candidates {
        let ordinary = scope.lookup_workspace(&qualified);
        if let Some(symbol) = prefer_definition(&ordinary) {
            return Some(vec![symbol]);
        }
        let has_property = ["get_", "set_"].into_iter().any(|prefix| {
            lookup_exact_workspace_function(scope, &qualified_virtual_name(&qualified, prefix))
                .is_some()
        });
        if !has_property {
            continue;
        }
        let accessors = lookup_exact_workspace_property_accessors(scope, &qualified, position);
        // A property at this lexical level owns the spelling even when the
        // requested access mode has no usable accessor (for example, reading
        // a setter-only property). Do not widen that miss to an outer scope.
        return Some(accessors);
    }
    explicitly_qualified.then(Vec::new)
}

/// Select an exact workspace FUNCTION, preferring a definition over an import.
/// Variables, types, namespaces, and other symbols that merely use an
/// accessor-shaped name do not create virtual properties.
fn lookup_exact_workspace_function<'a>(
    scope: &'a GlobalScope<'_>,
    qualified: &str,
) -> Option<&'a Symbol> {
    let functions: Vec<&Symbol> = scope
        .lookup_workspace(qualified)
        .into_iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Function { .. }))
        .collect();
    prefer_definition(&functions)
}

/// Exact accessor lookup for one already-qualified property candidate.
/// Getter/setter ordering and write fallback mirror the member-access seam.
fn lookup_exact_workspace_property_accessors<'a>(
    scope: &'a GlobalScope<'_>,
    qualified: &str,
    position: AccessPosition,
) -> Vec<&'a Symbol> {
    let mut prefixes: Vec<&str> = Vec::new();
    if position.reads() {
        prefixes.push("get_");
    }
    if position.writes() {
        prefixes.push("set_");
    }
    if position == AccessPosition::Write {
        prefixes.push("get_");
    }
    let mut out: Vec<&Symbol> = Vec::new();
    for prefix in prefixes {
        let accessor = qualified_virtual_name(qualified, prefix);
        if let Some(sym) = lookup_exact_workspace_function(scope, &accessor) {
            if !out.iter().any(|s| std::ptr::eq(*s, sym)) {
                out.push(sym);
            }
        }
        if position == AccessPosition::Write && !out.is_empty() {
            break;
        }
    }
    out
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

    /// [`goto_definition`] for a site that must have exactly ONE answer.
    ///
    /// Same arguments as the real entry point, so the tests below read as
    /// they did when it returned an `Option`. The `<= 1` assertion is the
    /// point: it makes each of them a standing guard that the multi-target
    /// paths (B103 accessor pairs, B104 implementation lists) never leak
    /// into a site that resolves to a single definition.
    fn goto_one(
        analysis: &DocumentAnalysis,
        checked: Option<&CheckedFile<'_>>,
        position: Position,
        scope: &GlobalScope<'_>,
        files: &WorkspaceFiles,
    ) -> Option<Location> {
        let all = goto_definition(analysis, checked, position, scope, files);
        assert!(
            all.len() <= 1,
            "expected a single definition target, got {}: {all:?}",
            all.len()
        );
        all.into_iter().next()
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
        let loc = goto_one(analysis, None, pos(3, col + 2), &scope, &ws_files);
        assert!(loc.is_some(), "variable in namespace did not resolve");
        let loc = loc.unwrap();
        assert_eq!(loc.uri, tw.uri());
        assert_eq!(loc.range.start.line, 1, "should point at `int target` line");

        // Cursor on `greet` inside main.
        let col = src.lines().nth(3).unwrap().find("greet").unwrap() as u32;
        let loc = goto_one(analysis, None, pos(3, col + 2), &scope, &ws_files);
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
            let loc = goto_one(analysis, None, pos(0, col + 1), &scope, &ws_files);
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
        let loc = goto_one(analysis, None, pos(1, 26), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(1, 14), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(1, 14), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(1, 33), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(1, 29), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(1, 47), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(1, 43), &scope, &ws_files);
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
        let loc = goto_one(analysis, None, pos(0, 33), &scope, &ws_files).unwrap();
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
        let loc = goto_one(analysis, None, pos(2, col), &scope, &ws_files);
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
    ///
    /// Returns EVERY target, in the order navigation offers them.
    fn goto_all(
        tw: &crate::server::test_support::TestWorkspace,
        file: usize,
        line: usize,
        needle: &str,
        offset: u32,
    ) -> Vec<Location> {
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

    /// [`goto_all`] for a site that must have exactly ONE answer.
    ///
    /// The `<= 1` assertion is deliberate: it makes every precise-
    /// navigation test below a standing guard that the multi-target paths
    /// (B103 accessors, B104 implementations) never leak into a call site
    /// that resolves to a single definition.
    fn goto_at(
        tw: &crate::server::test_support::TestWorkspace,
        file: usize,
        line: usize,
        needle: &str,
        offset: u32,
    ) -> Option<Location> {
        let all = goto_all(tw, file, line, needle, offset);
        assert!(
            all.len() <= 1,
            "expected a single definition target, got {}: {all:?}",
            all.len()
        );
        all.into_iter().next()
    }

    /// Line numbers of every target, for order-sensitive assertions.
    fn lines_of(locs: &[Location]) -> Vec<u32> {
        locs.iter().map(|l| l.range.start.line).collect()
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

    // === B103: virtual-property accessors (`obj.ID` -> get_ID/set_ID) =====
    //
    // AngelScript exposes a `get_X`/`set_X` pair as a property `X`. Before
    // B103 goto-definition had no accessor step at all, so `obj.ID`
    // resolved nothing whenever the class declared only the accessors.
    // Which accessor a site means depends on the position it is written
    // in, which is the checker's existing read/write rule
    // (`check_readonly_prop_assign`'s `compound` flag), reused here rather
    // than re-derived from the token stream.

    #[test]
    fn goto_definition_virtual_property_read_resolves_to_the_getter() {
        let src = "class C { uint get_ID() { return 1; } }\nvoid f(C@ c) { uint x = c.ID; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "c.ID", 2).expect("c.ID must resolve to C::get_ID");
        assert_eq!(loc.range.start.line, 0, "must land on get_ID");
    }

    #[test]
    fn goto_definition_virtual_property_write_resolves_to_the_setter() {
        let src = "class C {\n    uint get_ID() { return 1; }\n    void set_ID(uint v) {}\n}\nvoid f(C@ c) { c.ID = 5; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 4, "c.ID", 2).expect("c.ID = 5 must resolve to C::set_ID");
        assert_eq!(
            loc.range.start.line, 2,
            "a write position must land on set_ID (line 2), not get_ID (line 1)"
        );
    }

    #[test]
    fn goto_definition_virtual_property_write_without_a_setter_falls_back_to_the_getter() {
        // Get-only property: the write is a mistake the checker reports
        // separately, but the getter is still where the property is
        // declared, so navigation must not go silent.
        let src = "class C { uint get_ID() { return 1; } }\nvoid f(C@ c) { c.ID = 5; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "c.ID", 2).expect("must fall back to the getter");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_virtual_property_compound_assign_offers_both_accessors() {
        // `+=` is `a = a OP b`: it reads AND writes, so both accessors are
        // real targets. Order is pinned — getter first, the order the
        // access performs them in.
        let src = "class C {\n    uint get_ID() { return 1; }\n    void set_ID(uint v) {}\n}\nvoid f(C@ c) { c.ID += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 4, "c.ID", 2);
        assert_eq!(
            lines_of(&locs),
            vec![1, 2],
            "a compound assign must offer get_ID then set_ID, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_virtual_property_comparison_is_a_read_not_a_write() {
        // `c.ID == 5` contains an `=` character but is a READ. A token-level
        // scan for a following `=` gets this wrong; the AST does not.
        let src = "class C {\n    uint get_ID() { return 1; }\n    void set_ID(uint v) {}\n}\nvoid f(C@ c) { if (c.ID == 5) {} }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 4, "c.ID", 2).expect("a comparison must resolve");
        assert_eq!(
            loc.range.start.line, 1,
            "`c.ID == 5` reads the property, so it must land on get_ID"
        );
    }

    #[test]
    fn goto_definition_virtual_property_indexed_write_is_a_read_of_the_property() {
        // `c.Items[0] = v` WRITES the element and READS the property to
        // reach it — the assignment's LHS is the index, not the member.
        let src = "class C {\n    array<uint>@ get_Items() { return null; }\n}\nvoid f(C@ c) { c.Items[0] = 5; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 3, "c.Items", 2).expect("must resolve to the getter");
        assert_eq!(loc.range.start.line, 1);
    }

    /// Receiver-shape variants (repo convention): the accessor step must not
    /// depend on how the receiver is spelled.
    #[test]
    fn goto_definition_virtual_property_through_receiver_type_variants() {
        for decl in [
            "C@ c",
            "C c",
            "C@ &in c",
            "const C@ c",
            "C &in c",
            "const C &in c",
        ] {
            let src = format!(
                "class C {{ uint get_ID() {{ return 1; }} }}\nvoid f({decl}) {{ uint x = c.ID; }}\n"
            );
            let tw = build_single_file_workspace("file:///t/a.as", &src);
            let loc = goto_at(&tw, 0, 1, "c.ID", 2)
                .unwrap_or_else(|| panic!("`{decl}` receiver must resolve c.ID"));
            assert_eq!(loc.range.start.line, 0, "for receiver `{decl}`");
        }
    }

    #[test]
    fn goto_definition_virtual_property_inherited_from_a_base_class() {
        let src = "class A { uint get_ID() { return 1; } }\nclass B : A {}\nvoid f(B@ b) { uint x = b.ID; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "b.ID", 2).expect("inherited accessor must resolve");
        assert_eq!(loc.range.start.line, 0, "must land on A::get_ID");
    }

    #[test]
    fn goto_definition_virtual_property_declared_on_an_interface() {
        let src = "interface I { uint get_ID(); }\nvoid f(I@ i) { uint x = i.ID; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "i.ID", 2).expect("interface accessor must resolve");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_virtual_property_across_files() {
        let tw = crate::server::test_support::TestWorkspace::files(&[
            ("use", "void f(C@ c) { uint x = c.ID; }\n"),
            ("decl", "class C { uint get_ID() { return 1; } }\n"),
        ]);
        let loc = goto_all(&tw, 0, 0, "c.ID", 2);
        assert_eq!(loc.len(), 1, "expected one target, got {loc:?}");
        assert_eq!(loc[0].uri, tw.uri_of(1), "must jump to decl.as");
    }

    #[test]
    fn goto_definition_namespace_level_virtual_property_read() {
        let src =
            "namespace Ns { uint get_Thing() { return 1; } }\nvoid f() { uint x = Ns::Thing; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc =
            goto_at(&tw, 0, 1, "Ns::Thing", 4).expect("Ns::Thing must resolve to Ns::get_Thing");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_namespace_level_virtual_property_write() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint v) {}\n}\nvoid f() { Ns::Thing = 5; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 4, "Ns::Thing", 4).expect("must resolve");
        assert_eq!(
            loc.range.start.line, 2,
            "a write must land on Ns::set_Thing (line 2)"
        );
    }

    #[test]
    fn goto_definition_namespace_level_virtual_property_compound_assign_offers_both() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint v) {}\n}\nvoid f() { Ns::Thing += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 4, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&locs),
            vec![1, 2],
            "must offer Ns::get_Thing then Ns::set_Thing, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_qualified_virtual_property_ignores_global_and_unrelated_collisions() {
        // Neither a root declaration nor an unrelated namespace tail is a
        // candidate for the explicitly-qualified `Ns::Thing` spelling.
        let src = "uint Thing;\nnamespace Other { uint get_Thing() { return 9; } }\nnamespace Ns { uint get_Thing() { return 1; } }\nvoid f() { uint x = Ns::Thing; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 3, "Ns::Thing", 4).expect("Ns::Thing must resolve");
        assert_eq!(
            loc.range.start.line, 2,
            "the exact Ns::get_Thing accessor must beat root Thing and Other::get_Thing"
        );
    }

    #[test]
    fn goto_definition_qualified_virtual_property_never_uses_an_unrelated_tail() {
        let src = "namespace Ns {}\nnamespace Other { uint get_Thing() { return 9; } }\nvoid f() { uint x = Ns::Thing; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "Ns::Thing", 4);
        assert!(
            locs.is_empty(),
            "absent Ns::Thing/Ns::get_Thing must not land on Other::get_Thing: {locs:?}"
        );
    }

    #[test]
    fn goto_definition_relative_qualified_property_uses_the_nearest_namespace_prefix() {
        let src = "namespace Inner {\n    uint get_Thing() { return 0; }\n}\nnamespace Outer {\n    namespace Inner {\n        uint get_Thing() { return 1; }\n    }\n    namespace Deep {\n        namespace Inner {\n            uint get_Thing() { return 2; }\n        }\n        void f() { uint x = Inner::Thing; }\n    }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc =
            goto_at(&tw, 0, 11, "Inner::Thing", 7).expect("relative Inner::Thing must resolve");
        assert_eq!(
            loc.range.start.line, 9,
            "Outer::Deep::Inner must beat Outer::Inner and root Inner"
        );
    }

    #[test]
    fn goto_definition_relative_qualified_real_field_beats_conflicting_root_symbols() {
        let src = "namespace Inner {\n    uint Thing;\n    uint get_Thing() { return 0; }\n}\nnamespace Outer {\n    namespace Inner {\n        uint Thing;\n        uint get_Thing() { return 1; }\n    }\n    void f() { uint x = Inner::Thing; }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 9, "Inner::Thing", 7);
        assert_eq!(locs.len(), 1, "a real field has one target: {locs:?}");
        assert_eq!(
            locs[0].range.start.line, 6,
            "Outer::Inner::Thing must beat both its accessor and root Inner::Thing"
        );
    }

    #[test]
    fn goto_definition_namespace_property_rejects_non_function_accessor_shapes() {
        for source in [
            "namespace Ns { uint get_Thing; uint set_Thing; }\nvoid f() { uint x = Ns::Thing; }\n",
            "namespace Ns { class get_Thing {} class set_Thing {} }\nvoid f() { uint x = Ns::Thing; }\n",
        ] {
            let tw = build_single_file_workspace("file:///t/a.as", source);
            let locs = goto_all(&tw, 0, 1, "Ns::Thing", 4);
            assert!(
                locs.is_empty(),
                "only functions can define a virtual property, got {locs:?} for {source}"
            );
        }
    }

    #[test]
    fn goto_definition_namespace_property_write_ignores_non_function_setter() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    uint set_Thing;\n}\nvoid f() { Ns::Thing = 2; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 4, "Ns::Thing", 4)
            .expect("write with an invalid setter must fall back to the getter");
        assert_eq!(
            loc.range.start.line, 1,
            "the set_Thing variable is not an accessor function"
        );
    }

    #[test]
    fn goto_definition_bare_virtual_property_uses_the_nearest_lexical_namespace() {
        let src = "uint get_Thing() { return 0; }\nnamespace Outer {\n    uint get_Thing() { return 1; }\n    namespace Inner {\n        uint get_Thing() { return 2; }\n        void f() { uint x = Thing; }\n    }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 5, "= Thing", 2).expect("bare Thing must resolve");
        assert_eq!(
            loc.range.start.line, 4,
            "the nearest Inner accessor must beat Outer and root accessors"
        );
    }

    #[test]
    fn goto_definition_bare_virtual_property_ignores_a_root_accessor_in_a_namespace() {
        let src = "uint get_Thing() { return 0; }\nnamespace Ns {\n    uint get_Thing() { return 1; }\n    void f() { uint x = Thing; }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 3, "= Thing", 2).expect("bare Thing must resolve");
        assert_eq!(
            loc.range.start.line, 2,
            "Ns::get_Thing must beat the root get_Thing"
        );
    }

    #[test]
    fn goto_definition_namespace_real_field_precedes_its_accessors() {
        let src = "namespace Ns {\n    uint Thing;\n    uint get_Thing() { return 1; }\n    void set_Thing(uint value) {}\n}\nvoid f() { Ns::Thing += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 5, "Ns::Thing", 4);
        assert_eq!(locs.len(), 1, "a real field has one target: {locs:?}");
        assert_eq!(locs[0].range.start.line, 1, "the exact real field must win");
    }

    #[test]
    fn goto_definition_classifies_virtual_property_access_in_omitted_initializers() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint value) {}\n}\nuint global = (Ns::Thing = 4);\nnamespace Outer {\n    class Holder { uint field = (Ns::Thing += 1); }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);

        let write = goto_all(&tw, 0, 4, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&write),
            vec![2],
            "a global initializer write must navigate to set_Thing"
        );

        let compound = goto_all(&tw, 0, 6, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&compound),
            vec![1, 2],
            "a class field initializer compound access must offer get then set"
        );
    }

    #[test]
    fn goto_definition_classifies_virtual_property_write_in_free_function_default() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint value) {}\n}\nvoid f(uint value = (Ns::Thing = 4)) {}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 4, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&locs),
            vec![2],
            "a write in a free-function default must navigate only to set_Thing"
        );
    }

    #[test]
    fn goto_definition_classifies_virtual_property_compound_in_class_method_default() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint value) {}\n}\nclass Holder {\n    void f(uint value = (Ns::Thing += 1)) {}\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 5, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&locs),
            vec![1, 2],
            "a compound class-method default must offer get_Thing then set_Thing"
        );
    }

    #[test]
    fn goto_definition_classifies_virtual_property_access_in_property_bodies() {
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint value) {}\n}\nnamespace Outer {\n    namespace Inner {\n        class Holder {\n            uint Value {\n                get { Ns::Thing += 1; return 0; }\n                set { Ns::Thing = value; }\n            }\n        }\n    }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);

        let compound = goto_all(&tw, 0, 8, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&compound),
            vec![1, 2],
            "a property getter compound access must offer get then set"
        );

        let write = goto_all(&tw, 0, 9, "Ns::Thing", 4);
        assert_eq!(
            lines_of(&write),
            vec![2],
            "a property setter-body write must navigate to set_Thing"
        );
    }

    #[test]
    fn goto_definition_bare_virtual_property_written_inside_its_own_namespace() {
        // The write half of the bare-name case: `Thing = 5` inside
        // `namespace Ns` parses as a plain identifier, not a
        // `NamespaceAccess`, so the classifier has to recognise that shape
        // too or the write silently resolves to the getter.
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void set_Thing(uint v) {}\n    void f() { Thing = 5; }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 3, "Thing", 0).expect("bare property write must resolve");
        assert_eq!(
            loc.range.start.line, 2,
            "must land on Ns::set_Thing (line 2), not the getter"
        );
    }

    #[test]
    fn goto_definition_bare_virtual_property_inside_its_own_namespace() {
        // Written bare inside `namespace Ns`, the property still resolves
        // through the namespace ladder to `Ns::get_Thing`.
        let src = "namespace Ns {\n    uint get_Thing() { return 1; }\n    void f() { uint x = Thing; }\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "= Thing", 2).expect("bare property must resolve");
        assert_eq!(loc.range.start.line, 1);
    }

    /// NEGATIVE CONTROL: a REAL field must keep navigating to the field.
    /// A same-named accessor on another class exists precisely so that a
    /// bare-name accessor fallback would be caught landing on it.
    #[test]
    fn goto_definition_a_real_field_wins_over_a_same_named_accessor() {
        let src = "class Other { uint get_ID() { return 9; } }\nclass C { uint ID; }\nvoid f(C@ c) { uint x = c.ID; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "c.ID", 2);
        assert_eq!(
            locs.len(),
            1,
            "a real field has ONE definition, got {locs:?}"
        );
        assert_eq!(
            locs[0].range.start.line, 1,
            "must land on C's field (line 1), never Other::get_ID (line 0)"
        );
    }

    /// The same control in a write position: writing a real field must not
    /// start hunting for a setter.
    #[test]
    fn goto_definition_a_real_field_written_to_still_resolves_to_the_field() {
        let src = "class Other { void set_ID(uint v) {} }\nclass C { uint ID; }\nvoid f(C@ c) { c.ID = 5; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "c.ID", 2);
        assert_eq!(locs.len(), 1, "expected the field only, got {locs:?}");
        assert_eq!(locs[0].range.start.line, 1);
    }

    #[test]
    fn goto_definition_unknown_virtual_property_resolves_nothing() {
        // No `get_Nope`/`set_Nope` anywhere: the accessor step must not
        // invent a target, and must not fall through to a same-named
        // global.
        let src = "uint get_Nope() { return 0; }\nclass C { uint get_ID() { return 1; } }\nvoid f(C@ c) { uint x = c.Nope; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "c.Nope", 2);
        assert!(
            locs.is_empty(),
            "a property the class does not declare must not fall through to the \
             same-named free accessor, got {locs:?}"
        );
    }
}
