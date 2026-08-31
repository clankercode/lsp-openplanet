//! Goto-definition for the LSP, and the position → symbol-name lookup its
//! neighbours share.
//!
//! [`ident_index_at`] walks the token stream at the cursor, reconstructing
//! any `Ns::Name` prefix, and definitions are resolved against the
//! checker's resolution surface ([`GlobalScope`]) built from the open
//! documents.
//!
//! # B135: navigation does NOT filter on member visibility, deliberately
//!
//! B117 taught member COMPLETION to hide `private` and `protected` members at
//! access sites where the engine refuses them. Goto-definition is deliberately
//! NOT given the same treatment, and the absence of a filter here is a decision
//! rather than an oversight — recorded so nobody "finishes the job" by adding
//! one.
//!
//! A user navigating to a private member is READING code, not calling it, and
//! the request is most valuable in exactly the case a filter would break: they
//! could not use the member, and want to see its declaration to find out why.
//! Refusing to resolve would answer a question they did not ask by withholding
//! the one they did, and would leave them with no in-editor way to reach a
//! declaration that is plainly in the workspace.
//!
//! The same reasoning covers workspace symbol search
//! ([`lookup_workspace_symbols`] and friends): it is a code-READING tool over
//! declarations that exist.
//!
//! Hover reaches the same conclusion by the same argument and goes one step
//! further, ANNOTATING the signature with the keyword rather than suppressing
//! it — see `hover::visibility_keyword`. The document outline likewise shows
//! every member and annotates via `detail` — see `symbols::visibility_detail`.
//! Completion is the only surface that hides, because it is the only one that
//! PROPOSES code to write rather than describing code that exists.
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
use crate::typecheck::call_site::arity_admits;
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
                return member_targets(scope, &owner, &qual, &members, analysis, tok_span, files);
            }
            // B103: no member is spelled `qual`, but the type may still
            // declare it as a VIRTUAL PROPERTY — a `get_`/`set_` pair. The
            // exact lookup above runs first so a real field named `ID`
            // always beats an accessor named for it.
            let position = access_position_at(&analysis.file, tok_span);
            let accessors = scope.lookup_virtual_property_accessor_symbols(&owner, &qual, position);
            // B103 + B104: an accessor declared on an interface has
            // implementations too. Each accessor keeps its own
            // implementations directly after it, so a compound assign still
            // reads as get-then-its-impls, set-then-its-impls.
            let mut out: Vec<Location> = Vec::new();
            for accessor in accessors {
                let accessor_name = accessor
                    .name
                    .rsplit("::")
                    .next()
                    .unwrap_or(&accessor.name)
                    .to_string();
                for loc in declaration_with_implementations(
                    accessor,
                    &scope.workspace_member_implementations(&owner, &accessor_name),
                    files,
                ) {
                    if !out.contains(&loc) {
                        out.push(loc);
                    }
                }
            }
            return out;
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
        match lookup_workspace_property_site(scope, analysis, &qual, tok_span.start, position) {
            Some(PropertySite::Accessors(accessors)) => {
                return accessors
                    .into_iter()
                    .filter_map(|sym| symbol_location(sym, files))
                    .collect();
            }
            Some(PropertySite::Ordinary(ordinary)) if !ordinary.is_empty() => {
                return overload_targets(&ordinary, analysis, tok_span, files);
            }
            Some(PropertySite::Ordinary(_)) => return Vec::new(),
            None => {}
        }
        if qual.contains("::") {
            return Vec::new();
        }
    }
    let by_name = lookup_workspace_symbols(scope, analysis, &qual, tok_span.start);
    if !by_name.is_empty() {
        return overload_targets(&by_name, analysis, tok_span, files);
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
    member_targets(
        scope, &enclosing, &qual, &members, analysis, tok_span, files,
    )
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
) -> Option<PropertySite<'a>> {
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
        if prefer_definition(&ordinary).is_some() {
            return Some(PropertySite::Ordinary(ordinary));
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
        return Some(PropertySite::Accessors(accessors));
    }
    explicitly_qualified.then(|| PropertySite::Ordinary(Vec::new()))
}

/// Virtual-property accessors for a NON-member site — a bare `Thing` or a
/// qualified `Ns::Thing` — resolved through the very ladder
/// [`goto_definition`] uses, so hover and definition can never disagree about
/// which property a spelling names (B113).
///
/// `None` when the site is not a virtual property: no accessor at any valid
/// lexical level, or an ordinary declaration owns the spelling — which must
/// keep winning, and which the caller renders by its own ordinary path. An
/// empty vec means a property owns the spelling at that level but declares no
/// accessor for this access position (reading a setter-only property); the
/// caller has nothing to show and must NOT widen the search outward.
///
/// Deliberately excludes member accesses: `obj.Thing` belongs to the
/// receiver's type and nowhere else, so it goes through
/// [`GlobalScope::lookup_virtual_property_accessor_symbols`] instead.
pub(crate) fn workspace_property_accessors<'a>(
    scope: &'a GlobalScope<'_>,
    analysis: &DocumentAnalysis,
    name: &str,
    use_offset: u32,
    position: AccessPosition,
) -> Option<Vec<&'a Symbol>> {
    match lookup_workspace_property_site(scope, analysis, name, use_offset, position) {
        Some(PropertySite::Accessors(accessors)) => Some(accessors),
        Some(PropertySite::Ordinary(_)) | None => None,
    }
}

/// Which kind of thing a workspace-level site resolved to.
///
/// The distinction matters at the call site and nowhere else: an ORDINARY
/// declaration may be one of several overloads, so it still has to go
/// through [`overload_targets`] (B104), whereas ACCESSORS have already been
/// selected by access position and must be emitted as-is — running overload
/// narrowing over them would collapse a compound assign's `get_`+`set_`
/// pair back into a single target.
enum PropertySite<'a> {
    Ordinary(Vec<&'a Symbol>),
    Accessors(Vec<&'a Symbol>),
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

/// Argument count of the call whose callee is the identifier spanning
/// `name_span`; `None` when the site is not a call callee.
///
/// Read off the AST rather than the token stream so nesting, string
/// literals and comments are handled for free — the same reason
/// `call_hierarchy` walks expressions to find the enclosing call.
fn call_argc_at(
    file: &crate::parser::ast::SourceFile,
    name_span: crate::lexer::Span,
) -> Option<usize> {
    let mut found = None;
    crate::parser::walk::walk_file_exprs_complete(file, &mut |expr: &crate::parser::ast::Expr| {
        let crate::parser::ast::ExprKind::Call { callee, args } = &expr.kind else {
            return;
        };
        let callee_span = match &callee.kind {
            crate::parser::ast::ExprKind::Ident(id) => Some(id.span),
            crate::parser::ast::ExprKind::Member { member, .. } => Some(member.span),
            crate::parser::ast::ExprKind::NamespaceAccess { path } => {
                path.segments.last().map(|s| s.span)
            }
            _ => None,
        };
        if callee_span.is_some_and(|s| s.start == name_span.start && s.end == name_span.end) {
            found = Some(args.len());
        }
    });
    found
}

/// One target when the site names a single definition or the call site
/// narrows an overload set to one signature; every overload when it cannot
/// (B104).
///
/// `unique_overload_for_argc` answers `None` for BOTH "no signature takes
/// this many arguments" and "several do". Neither lets us honestly claim
/// one target, so both list the whole set — which is also why a call site
/// that DOES narrow keeps its single precise answer, the regression this
/// must not cause.
fn overload_targets(
    candidates: &[&Symbol],
    analysis: &DocumentAnalysis,
    tok_span: crate::lexer::Span,
    files: &WorkspaceFiles,
) -> Vec<Location> {
    let functions: Vec<&Symbol> = candidates
        .iter()
        .copied()
        .filter(|s| matches!(s.kind, SymbolKind::Function { .. }) && !s.is_import_alias())
        .collect();
    if functions.len() > 1 {
        if let Some(argc) = call_argc_at(&analysis.file, tok_span) {
            let mut matching = functions.iter().filter(|s| match &s.kind {
                SymbolKind::Function {
                    params, min_args, ..
                } => arity_admits(*min_args, params.len(), argc),
                _ => false,
            });
            let unique = matching.next().filter(|_| matching.next().is_none());
            match unique {
                Some(sym) => return symbol_location(sym, files).into_iter().collect(),
                None => return symbols_in_workspace_order(functions, files),
            }
        }
    }
    prefer_definition(candidates)
        .and_then(|s| symbol_location(s, files))
        .into_iter()
        .collect()
}

/// Resolve a method/member against the receiver's static type, applying the
/// same arity rule as free-function overload navigation (B104).
///
/// A call that narrows a genuine overload set to exactly one declaration uses
/// that declaration, but still includes its implementations/overrides — the
/// receiver union is independent of overload precision. With several or zero
/// arity matches, arity cannot justify picking one, so every static overload is
/// shown. Each declaration is immediately followed by implementations of THAT
/// signature.
fn member_targets(
    scope: &GlobalScope<'_>,
    owner: &str,
    member: &str,
    candidates: &[&Symbol],
    analysis: &DocumentAnalysis,
    tok_span: crate::lexer::Span,
    files: &WorkspaceFiles,
) -> Vec<Location> {
    let functions: Vec<&Symbol> = candidates
        .iter()
        .copied()
        .filter(|s| matches!(s.kind, SymbolKind::Function { .. }) && !s.is_import_alias())
        .collect();

    let declarations = if functions.len() > 1 {
        if let Some(argc) = call_argc_at(&analysis.file, tok_span) {
            let matching: Vec<&Symbol> = functions
                .iter()
                .copied()
                .filter(|s| match &s.kind {
                    SymbolKind::Function {
                        params, min_args, ..
                    } => arity_admits(*min_args, params.len(), argc),
                    _ => false,
                })
                .collect();
            if matching.len() == 1 {
                matching
            } else {
                functions
            }
        } else {
            prefer_definition(candidates).into_iter().collect()
        }
    } else {
        prefer_definition(candidates).into_iter().collect()
    };

    let implementations = scope.workspace_member_implementations(owner, member);
    let mut out = Vec::new();
    for declaration in declarations {
        for location in declaration_with_implementations(declaration, &implementations, files) {
            if !out.contains(&location) {
                out.push(location);
            }
        }
    }
    out
}

/// The declaration first, then implementations/overrides with the same
/// parameter signature, ordered by workspace URI rather than snapshot-local
/// file id. Symbols whose file is not open are dropped.
///
/// The declaration leads because it is what the receiver's STATIC type
/// guarantees — the one answer that is certainly correct. The
/// implementations are suggestions layered after it, which is why an
/// incomplete dependency closure is tolerable here in a way it would not
/// be for a diagnostic: a missing entry costs a peek-list row, not a false
/// error.
fn declaration_with_implementations(
    declaration: &Symbol,
    implementations: &[&Symbol],
    files: &WorkspaceFiles,
) -> Vec<Location> {
    let mut out = symbol_location(declaration, files)
        .into_iter()
        .collect::<Vec<_>>();
    let matching = implementations
        .iter()
        .copied()
        .filter(|implementation| same_function_signature(declaration, implementation));
    for location in symbols_in_workspace_order(matching, files) {
        if !out.contains(&location) {
            out.push(location);
        }
    }
    out
}

fn same_function_signature(left: &Symbol, right: &Symbol) -> bool {
    let (
        SymbolKind::Function {
            return_type: left_return,
            params: left_params,
            ..
        },
        SymbolKind::Function {
            return_type: right_return,
            params: right_params,
            ..
        },
    ) = (&left.kind, &right.kind)
    else {
        return false;
    };
    same_type_spelling(left_return, right_return)
        && left_params.len() == right_params.len()
        && left_params
            .iter()
            .zip(right_params)
            .all(|((_, left), (_, right))| same_type_spelling(left, right))
}

fn same_type_spelling(left: &str, right: &str) -> bool {
    left.chars()
        .filter(|c| !c.is_whitespace())
        .eq(right.chars().filter(|c| !c.is_whitespace()))
}

fn symbols_in_workspace_order<'s>(
    symbols: impl IntoIterator<Item = &'s Symbol>,
    files: &WorkspaceFiles,
) -> Vec<Location> {
    let mut locations: Vec<Location> = symbols
        .into_iter()
        .filter_map(|symbol| symbol_location(symbol, files))
        .collect();
    locations.sort_by(|left, right| {
        left.uri
            .as_str()
            .cmp(right.uri.as_str())
            .then_with(|| left.range.start.line.cmp(&right.range.start.line))
            .then_with(|| left.range.start.character.cmp(&right.range.start.character))
            .then_with(|| left.range.end.line.cmp(&right.range.end.line))
            .then_with(|| left.range.end.character.cmp(&right.range.end.character))
    });
    locations.dedup();
    locations
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

    pub(super) fn build_single_file_workspace(
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
    pub(super) fn goto_one(
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
        // B104 CHANGED THIS CONTRACT. It used to assert the interface
        // declaration was the ONLY answer ("not the class implementation").
        // The declaration still comes FIRST — that part is unchanged and
        // still pinned — but the implementations now follow it instead of
        // being suppressed.
        let locs = goto_all(&tw, 0, 2, "i.Go", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1],
            "the interface declaration must lead, with C::Go after it, got {locs:?}"
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

    // === B104: every implementation, not one location =====================
    //
    // A method reached through an interface- or base-typed receiver has
    // more than one honest answer: the declaration the static type owns,
    // and each workspace class that implements or overrides it. Definition
    // returns them all, declaration FIRST, implementations after in
    // workspace file order (deterministic, so it can be pinned here).
    //
    // The single-target contract is NOT relaxed generally: `goto_at` below
    // asserts exactly one answer, and every precise-navigation test still
    // uses it.

    #[test]
    fn goto_definition_interface_method_lists_its_one_implementer_after_the_interface() {
        let src =
            "interface I { void Go(); }\nclass C : I { void Go() {} }\nvoid f(I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "i.Go", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1],
            "interface declaration first, then the implementation, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_interface_method_lists_several_implementers_in_file_order() {
        let src = "interface I { void Go(); }\nclass A : I { void Go() {} }\nclass B : I { void Go() {} }\nvoid f(I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 3, "i.Go", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1, 2],
            "must list I::Go, then A::Go, then B::Go in declaration order, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_interface_method_with_no_implementer_stays_a_single_target() {
        // Zero implementers: the interface declaration is the only answer,
        // and it must NOT be wrapped into a list of one oddity — `goto_at`
        // asserts exactly one.
        let src = "interface I { void Go(); }\nvoid f(I@ i) { i.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 1, "i.Go", 2).expect("must resolve");
        assert_eq!(loc.range.start.line, 0);
    }

    #[test]
    fn goto_definition_implementers_are_listed_in_workspace_file_order() {
        // Ordering across files must be deterministic. The symbol pool is a
        // hash map, so this fails intermittently if the implementation does
        // not sort.
        let tw = crate::server::test_support::TestWorkspace::files(&[
            ("a_iface", "interface I { void Go(); }\n"),
            ("b_first", "class B1 : I { void Go() {} }\n"),
            ("c_second", "class C2 : I { void Go() {} }\n"),
            ("d_use", "void f(I@ i) { i.Go(); }\n"),
        ]);
        let locs = goto_all(&tw, 3, 0, "i.Go", 2);
        let uris: Vec<_> = locs.iter().map(|l| l.uri.clone()).collect();
        assert_eq!(
            uris,
            vec![tw.uri_of(0), tw.uri_of(1), tw.uri_of(2)],
            "declaration file first, then implementers in file order, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_diamond_two_interfaces_sharing_a_method_name() {
        // `C` implements both `A` and `B`, which both declare `Go`. Through
        // an `A@` receiver the answer is A::Go plus C's implementation —
        // B::Go is NOT an answer, it is a different declaration.
        let src = "interface A { void Go(); }\ninterface B { void Go(); }\nclass C : A, B { void Go() {} }\nvoid f(A@ a) { a.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 3, "a.Go", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 2],
            "must be A::Go then C::Go, never B::Go (line 1), got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_override_chain_lists_base_then_both_derived() {
        let src = "class Base { void Go() {} }\nclass D1 : Base { void Go() {} }\nclass D2 : Base { void Go() {} }\nvoid f(Base@ b) { b.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 3, "b.Go", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1, 2],
            "base declaration then each override, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_transitive_override_is_listed() {
        // D2 derives from D1 derives from Base. A `Base@` receiver must
        // reach the grandchild override too — the walk is transitive.
        let src = "class Base { void Go() {} }\nclass D1 : Base { void Go() {} }\nclass D2 : D1 { void Go() {} }\nvoid f(Base@ b) { b.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 3, "b.Go", 2);
        assert_eq!(lines_of(&locs), vec![0, 1, 2], "got {locs:?}");
    }

    #[test]
    fn goto_definition_a_derived_class_that_does_not_override_is_not_listed() {
        // `D` inherits `Go` without redeclaring it, so it has no `Go`
        // definition of its own to jump to. Listing it would send the user
        // to a class body with nothing in it.
        let src = "class Base { void Go() {} }\nclass D : Base {}\nvoid f(Base@ b) { b.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "b.Go", 2).expect("must resolve");
        assert_eq!(loc.range.start.line, 0, "only Base::Go declares Go");
    }

    #[test]
    fn goto_definition_class_typed_receiver_still_lists_its_own_overrides() {
        // The rule is about the receiver's STATIC type, not about
        // interfaces specifically: a concrete base works the same way.
        let src =
            "class C { void Go() {} }\nclass Sub : C { void Go() {} }\nvoid f(C@ c) { c.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "c.Go", 2);
        assert_eq!(lines_of(&locs), vec![0, 1], "got {locs:?}");
    }

    /// NEGATIVE CONTROL: a leaf type nobody extends keeps exactly one
    /// precise target. This is the regression B104 must not cause.
    #[test]
    fn goto_definition_leaf_class_method_keeps_a_single_precise_target() {
        let src =
            "interface I { void Go(); }\nclass C : I { void Go() {} }\nvoid f(C@ c) { c.Go(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "c.Go", 2).expect("must resolve");
        assert_eq!(
            loc.range.start.line, 1,
            "nothing extends C, so C::Go is the only answer"
        );
    }

    /// B103 + B104 together: an interface declares the ACCESSOR, classes
    /// implement it, and a property read through the interface offers the
    /// interface's getter first and each implementation after.
    #[test]
    fn goto_definition_interface_declared_accessor_lists_implementations() {
        let src = "interface I { uint get_ID(); }\nclass C : I { uint get_ID() { return 1; } }\nvoid f(I@ i) { uint x = i.ID; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "i.ID", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1],
            "I::get_ID then C::get_ID, got {locs:?}"
        );
    }

    // === B104: overload sets ==============================================

    /// NEGATIVE CONTROL: a call site whose argument count narrows the
    /// overload set to exactly one signature keeps its single precise
    /// target. Regressing this would make every ordinary call ambiguous.
    #[test]
    fn goto_definition_resolvable_overload_keeps_a_single_target() {
        let src = "void F() {}\nvoid F(int a) {}\nvoid F(int a, int b) {}\nvoid main() { F(1); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 3, "F(1)", 0).expect("F(1) must resolve");
        assert_eq!(
            loc.range.start.line, 1,
            "one argument narrows to the single-parameter overload"
        );
    }

    #[test]
    fn goto_definition_unresolvable_overload_lists_every_overload() {
        // Two overloads both accept one argument, so the call site cannot
        // be narrowed — every candidate is a legitimate target.
        let src = "void F(int a) {}\nvoid F(float a) {}\nvoid main() { F(1); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "F(1)", 0);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1],
            "an unnarrowable call must offer both overloads, got {locs:?}"
        );
    }

    #[test]
    fn goto_definition_overload_with_no_matching_arity_lists_every_overload() {
        // No signature takes three arguments. `unique_overload_for_argc`
        // answers `None` for zero-match exactly as it does for multi-match,
        // and neither lets us claim one target honestly.
        let src = "void F(int a) {}\nvoid F(int a, int b) {}\nvoid main() { F(1, 2, 3); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 2, "F(1", 0);
        assert_eq!(lines_of(&locs), vec![0, 1], "got {locs:?}");
    }

    #[test]
    fn goto_definition_overloads_are_listed_in_workspace_file_order() {
        let tw = crate::server::test_support::TestWorkspace::files(&[
            ("a_one", "void F(int a) {}\n"),
            ("b_two", "void F(float a) {}\n"),
            ("c_use", "void main() { F(1); }\n"),
        ]);
        let locs = goto_all(&tw, 2, 0, "F(1)", 0);
        let uris: Vec<_> = locs.iter().map(|l| l.uri.clone()).collect();
        assert_eq!(
            uris,
            vec![tw.uri_of(0), tw.uri_of(1)],
            "overloads must be listed in workspace file order, got {locs:?}"
        );
    }

    /// A non-call reference to an overloaded name (a function-pointer
    /// decay, `startnew(Worker)`) has no argument list to narrow with, so
    /// it keeps the existing single-target behaviour rather than becoming
    /// a peek list.
    #[test]
    fn goto_definition_non_call_reference_to_an_overload_stays_single() {
        let src = "void F(int a) {}\nvoid F(float a) {}\nvoid main() { startnew(F); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let loc = goto_at(&tw, 0, 2, "(F)", 1);
        assert!(loc.is_some(), "a bare reference must still resolve");
    }

    #[test]
    fn goto_definition_b104_second_review_receiver_overloads_keep_the_union() {
        let src = "interface I {\nvoid Go();\nvoid Go(int x);\nvoid Go(float x);\nvoid Go(int x, int y);\n}\nclass C : I {\nvoid Go() {}\nvoid Go(int x) {}\nvoid Go(float x) {}\nvoid Go(int x, int y) {}\nvoid Go(int x, int y, int z) {}\n}\nclass D : I {\nvoid Go(int x, int y) {}\n}\nvoid f(I@ i) {\ni.Go(1, 2);\ni.Go(1);\ni.Go(1, 2, 3, 4);\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);

        let precise = goto_all(&tw, 0, 17, "i.Go", 2);
        assert_eq!(
            lines_of(&precise),
            vec![4, 10, 14],
            "a unique static overload still offers every matching workspace implementation"
        );

        let ambiguous = goto_all(&tw, 0, 18, "i.Go", 2);
        assert_eq!(
            lines_of(&ambiguous),
            vec![1, 7, 2, 8, 3, 9, 4, 10, 14],
            "several arity matches make the call ambiguous, so every static overload and its matching implementations must appear"
        );

        let no_arity_match = goto_all(&tw, 0, 19, "i.Go", 2);
        assert_eq!(
            lines_of(&no_arity_match),
            vec![1, 7, 2, 8, 3, 9, 4, 10, 14],
            "zero matches must list every declared overload and its matching implementation, never C's unrelated three-arg overload"
        );
    }

    #[test]
    fn goto_definition_b104_second_review_finds_effective_inherited_implementations() {
        let src = "interface I { void M(); }\nclass Direct { void M() {} }\nclass Mid : Direct {}\nclass Left : Direct, I {}\nclass Right : I, Direct {}\nclass Deep : Mid, I {}\nclass Own : I { void M() {} }\nvoid f(I@ i) { i.M(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 7, "i.M", 2);
        assert_eq!(
            lines_of(&locs),
            vec![0, 1, 6],
            "Direct::M is the effective definition for both base-list orders and the transitive case; Own::M remains a direct implementation"
        );
    }

    #[test]
    fn goto_definition_b104_second_review_requires_exact_return_and_parameter_modes() {
        let src = "class T {}\ninterface I {\nint A(const T &in x);\nint B(T &out x);\nint C(T x);\nint D(T@ x);\nint E(T@ &in x);\n}\nclass Exact : I {\nint A(const T &in x) { return 1; }\nint B(T &out x) { return 1; }\nint C(T x) { return 1; }\nint D(T@ x) { return 1; }\nint E(T@ &in x) { return 1; }\n}\nclass WrongMode : I {\nint A(T &in x) { return 1; }\nint B(T &in x) { return 1; }\nint C(T@ x) { return 1; }\nint D(T x) { return 1; }\nint E(T@ &out x) { return 1; }\n}\nclass WrongReturn : I {\nuint A(const T &in x) { return 1; }\n}\nvoid f(I@ i, T@ t) {\ni.A(t);\ni.B(t);\ni.C(t);\ni.D(t);\ni.E(t);\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        for (line, name, declaration, implementation) in [
            (26, "i.A", 2, 9),
            (27, "i.B", 3, 10),
            (28, "i.C", 4, 11),
            (29, "i.D", 5, 12),
            (30, "i.E", 6, 13),
        ] {
            let locs = goto_all(&tw, 0, line, name, 2);
            assert_eq!(
                lines_of(&locs),
                vec![declaration, implementation],
                "{name} must include only the exact return/parameter-mode implementation"
            );
        }
    }

    #[test]
    fn goto_definition_b104_review_implicit_this_matches_explicit_member_navigation() {
        let src = "class Base {\nvoid Go() {}\nvoid Pick(int x) {}\nvoid Pick(float x) {}\nvoid Use() {\nGo();\nthis.Go();\nPick(1);\n}\n}\nclass Derived : Base {\nvoid Go() {}\nvoid Pick(int x) {}\nvoid Pick(float x) {}\n}\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);

        for (line, needle) in [(5, "Go()"), (6, "this.Go")] {
            let locs = goto_all(&tw, 0, line, needle, if line == 5 { 0 } else { 5 });
            assert_eq!(
                lines_of(&locs),
                vec![1, 11],
                "bare and this. calls must both return Base::Go then Derived::Go"
            );
        }

        let overloaded = goto_all(&tw, 0, 7, "Pick(1)", 0);
        assert_eq!(
            lines_of(&overloaded),
            vec![2, 12, 3, 13],
            "an ambiguous implicit-this overload must use the same declaration/implementation grouping"
        );
    }

    #[test]
    fn goto_definition_b104_review_orders_implementations_by_uri_not_file_id() {
        let tw = crate::server::test_support::TestWorkspace::files(&[
            ("z_impl", "class Z : I { void Go() {} }\n"),
            ("m_iface", "interface I { void Go(); }\n"),
            ("a_impl", "class A : I { void Go() {} }\n"),
            ("use", "void f(I@ i) { i.Go(); }\n"),
        ]);
        let locs = goto_all(&tw, 3, 0, "i.Go", 2);
        let uris: Vec<_> = locs.iter().map(|loc| loc.uri.clone()).collect();
        assert_eq!(
            uris,
            vec![tw.uri_of(1), tw.uri_of(2), tw.uri_of(0)],
            "the static declaration leads; implementations are URI-lexical even when file IDs are not"
        );
    }

    #[test]
    fn goto_definition_b104_review_excludes_subinterfaces_and_handles_namespaced_cycles() {
        let src = "namespace N {\ninterface I { void Go(); }\ninterface J : I { void Go(); }\nclass C : J { void Go() {} }\ninterface A : B { void Run(); }\ninterface B : A {}\nclass D : A { void Run() {} }\n}\nvoid f(N::I@ i, N::A@ a) { i.Go(); a.Run(); }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);

        let go = goto_all(&tw, 0, 8, "i.Go", 2);
        assert_eq!(
            lines_of(&go),
            vec![1, 3],
            "only workspace classes are implementations: N::J must not appear"
        );

        let run = goto_all(&tw, 0, 8, "a.Run", 2);
        assert_eq!(
            lines_of(&run),
            vec![4, 6],
            "cyclic interface inheritance must terminate and still find N::D"
        );
    }

    #[test]
    fn goto_definition_b104_review_compound_accessor_keeps_signature_pairs_grouped() {
        let src = "interface I {\nuint get_ID();\nvoid set_ID(uint value);\n}\nclass C : I {\nuint get_ID() { return 1; }\nvoid set_ID(uint value) {}\nvoid set_ID(uint value, uint extra) {}\n}\nvoid f(I@ i) { i.ID += 1; }\n";
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let locs = goto_all(&tw, 0, 9, "i.ID", 2);
        assert_eq!(
            lines_of(&locs),
            vec![1, 5, 2, 6],
            "getter + matching implementation must precede setter + matching implementation"
        );
    }
}

// ── B135: goto-definition does NOT filter on visibility ─────────────────────
//
// The decision is argued in the module docs; these tests make it a pinned
// behaviour rather than an absence. B117 taught member COMPLETION to hide
// private and protected members at sites where the engine refuses them, and
// the obvious-looking follow-up is to do the same here. It is wrong:
// navigation is a code-READING tool, and the request is most valuable in
// exactly the case a filter would break — the user cannot use the member and
// wants to see its declaration to find out why.
//
// Without these tests, a later change that added a filter here would break
// nothing and look like consistency.
#[cfg(test)]
mod b135_navigation_visibility_tests {
    use super::tests::{build_single_file_workspace, goto_one};
    use super::*;

    fn resolves(src: &str, needle: &str, occurrence: usize) -> bool {
        let tw = build_single_file_workspace("file:///t/a.as", src);
        let analysis = tw.analysis();
        let files = tw.uri_map();
        let ws_files = WorkspaceFiles { files: &files };
        let scope = tw.scope();
        let byte = {
            let mut idx = 0usize;
            let mut found = 0usize;
            loop {
                let Some(rel) = src[idx..].find(needle) else {
                    panic!("needle `{needle}` #{occurrence} not found");
                };
                found += 1;
                idx += rel;
                if found == occurrence {
                    break idx;
                }
                idx += needle.len();
            }
        };
        let prefix = &src[..byte];
        let line = prefix.matches('\n').count() as u32;
        let col = prefix.rfind('\n').map_or(byte, |nl| byte - nl - 1) as u32;
        // The checked view is what resolves a member on a typed receiver;
        // without it every row below fails identically, which the public
        // control caught on the first run.
        let checked = tw.snapshot.checked_file(&tw.uri(), &scope);
        goto_one(
            analysis,
            checked.as_ref(),
            Position::new(line, col),
            &scope,
            &ws_files,
        )
        .is_some()
    }

    /// A private member resolves from an UNRELATED function — the site where
    /// completion hides it. This is the whole decision in one row.
    #[test]
    fn a_private_member_still_resolves_from_an_unrelated_function() {
        let src = "class C {\n    private int mem;\n}\nvoid f(C@ c) { c.mem = 1; }";
        assert!(
            resolves(src, "mem", 2),
            "goto-definition must reach a private member: reading code is not calling it"
        );
    }

    /// And from a DERIVED class, the site that distinguishes the keywords for
    /// completion. Navigation makes no distinction at all, which is the point.
    #[test]
    fn private_and_protected_both_resolve_from_a_derived_class() {
        for vis in ["private", "protected"] {
            let src = format!(
                "class C {{\n    {vis} int mem;\n}}\nclass D : C {{\n    void T(C@ c) {{ c.mem = 1; }}\n}}"
            );
            assert!(
                resolves(&src, "mem", 2),
                "`{vis}` must still resolve from a derived class"
            );
        }
    }

    /// The public control, so the rows above are about visibility rather than
    /// about member resolution working at all.
    #[test]
    fn a_public_member_resolves_too() {
        let src = "class C {\n    int mem;\n}\nvoid f(C@ c) { c.mem = 1; }";
        assert!(resolves(src, "mem", 2), "the control must resolve");
    }
}
