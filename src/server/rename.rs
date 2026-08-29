//! Scope-aware `textDocument/rename`, `textDocument/prepareRename` and
//! `textDocument/references` (B049).
//!
//! Before this module, both rename and find-references were the same
//! workspace-wide **textual** token scan: every `Ident` token in every open
//! file whose text matched the cursor's bare name was rewritten (or
//! reported). Renaming a local `x` therefore also rewrote the outer binding
//! it shadowed, every unrelated local/parameter/global named `x`, every
//! `obj.x`, both sides of `A::x`, and every named-argument label `x:` — in
//! every open file. On a loop counter that silently corrupts a plugin.
//!
//! ## One strategy per symbol class
//!
//! A local's scope is lexical; a field's is its class plus every `obj.field`
//! access; a free function's is the workspace. There is no single occurrence
//! rule, so [`classify`] decides which of three applies:
//!
//! | class | strategy |
//! |---|---|
//! | local variable, `for`-init, block/case local | [`Target::Local`] — the lexical range `[declarator, innermost_scope_end)`, single file |
//! | function parameter | [`Target::Local`] — the parameter name plus its function body, single file |
//! | free function, global variable | [`Target::Workspace`] with [`Shape::Value`] — every file, never adjacent to `::`, never after `.` |
//! | class, interface, enum, funcdef/typedef | [`Target::Workspace`] with [`Shape::Type`] — as above, but `Name::` **is** the symbol so a left-hand qualifier is rewritten |
//! | class field, method, `obj.member` | refused — needs the type of every receiver |
//! | namespaced name, either side of `::` | refused — needs qualifier resolution |
//! | enum value | refused — visible both bare and as `E::V` |
//! | named-argument label | refused — names one of the *callee's* parameters |
//! | anything not declared in the open workspace | refused |
//!
//! Refusal is the deliberate default: a refused rename is a papercut, a
//! wrong one corrupts a plugin. Every refusal carries a message, which
//! `prepareRename` hands the client before the user types a new name.
//!
//! [`find_references`] shares the classification but is read-only, so for
//! the refused classes it degrades to a *filtered* scan
//! ([`Target::Approximate`]) rather than to nothing — still a large
//! improvement on the old scan, and it cannot corrupt anything.

use std::collections::HashMap;

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::lexer::{Span, Token, TokenKind};
use crate::parser::ast::Item;
use crate::server::diagnostics::span_to_range;
use crate::server::lexical_scope::{self as lexical, Binding};
use crate::server::navigation::{ident_index_at, WorkspaceFiles};
use crate::symbols::scope::SymbolKind;
use crate::typecheck::GlobalScope;

/// Why a rename was declined. The message reaches the user: VS Code shows
/// it in the rename box for `prepareRename` and as a notification for
/// `rename`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
}

impl Refusal {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// How a workspace symbol may appear next to `::`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Functions and globals: a bare name, never a qualifier and never a
    /// member, so every `::`-adjacent occurrence is a different symbol.
    Value,
    /// Classes, interfaces, enums and funcdefs: `Name::` **is** this
    /// symbol, so a left-hand qualifier is rewritten; a right-hand one
    /// (`Other::Name`) is still someone else's member.
    Type,
}

/// Which occurrences a read-only reference scan should keep for a class
/// rename cannot handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApproxKind {
    /// A class member: keep `.name` accesses and the bare uses inside a
    /// class that could be implicit-`this`, but drop anything sitting in
    /// the scope of a local or parameter of the same name — that is
    /// certainly not the member.
    Member,
    /// A namespaced name or enum value: keep bare and `::`-adjacent
    /// occurrences, drop `.name` accesses and argument labels.
    Qualified,
}

/// The binding the cursor names, and therefore the occurrence rule.
#[derive(Debug, Clone)]
enum Target {
    /// A local declarator or a function parameter: lexical, single file.
    Local { name: String, binding: Binding },
    /// A symbol the table registers at workspace top level.
    Workspace { name: String, shape: Shape },
    /// Not renameable. `find_references` still answers, approximately.
    Approximate {
        name: String,
        kind: ApproxKind,
        reason: &'static str,
    },
}

impl Target {
    fn name(&self) -> &str {
        match self {
            Target::Local { name, .. }
            | Target::Workspace { name, .. }
            | Target::Approximate { name, .. } => name,
        }
    }
}

/// `textDocument/prepareRename`: the range the client should put in its
/// rename box, or the reason this position cannot be renamed.
///
/// Everything [`rename`] can refuse for a reason that does not depend on
/// the new name is refused here too, so the user learns "not here" before
/// typing.
pub fn prepare_rename(
    analysis: &DocumentAnalysis,
    uri: &Url,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Result<Range, Refusal> {
    let (span, target) = classify(analysis, position, scope, files)
        .ok_or_else(|| Refusal::new("there is no identifier at this position"))?;
    match &target {
        Target::Approximate { reason, .. } => return Err(Refusal::new(*reason)),
        Target::Local { name, binding } => {
            local_occurrences(analysis, name, *binding)?;
            if binding.is_parameter {
                named_argument_label_exists(analysis, name, files)?;
            }
        }
        Target::Workspace { name, shape } => {
            workspace_occurrences(analysis, uri, name, *shape, files)?;
        }
    }
    Ok(span_to_range(analysis.masked_source(), span))
}

/// `textDocument/rename`: a workspace edit renaming exactly the
/// occurrences that bind to the symbol under `position`.
pub fn rename(
    analysis: &DocumentAnalysis,
    uri: &Url,
    position: Position,
    new_name: &str,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Result<WorkspaceEdit, Refusal> {
    let (_, target) = classify(analysis, position, scope, files)
        .ok_or_else(|| Refusal::new("there is no identifier at this position"))?;
    if !lexical::is_plain_identifier(new_name) {
        return Err(Refusal::new(format!(
            "`{new_name}` is not a valid AngelScript identifier"
        )));
    }
    if new_name == target.name() {
        return Err(Refusal::new("the new name is the same as the old one"));
    }

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    match &target {
        Target::Approximate { reason, .. } => return Err(Refusal::new(*reason)),
        Target::Local { name, binding } => {
            if binding.is_parameter {
                named_argument_label_exists(analysis, name, files)?;
            }
            let spans = local_occurrences(analysis, name, *binding)?;
            local_new_name_conflict(analysis, scope, *binding, new_name)?;
            changes.insert(uri.clone(), edits_for(analysis, &spans, new_name));
        }
        Target::Workspace { name, shape } => {
            let found = workspace_occurrences(analysis, uri, name, *shape, files)?;
            workspace_new_name_conflict(scope, files, &found, new_name)?;
            for file in &found {
                changes.insert(
                    file.uri.clone(),
                    edits_for(file.analysis, &file.spans, new_name),
                );
            }
        }
    }
    Ok(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

/// `textDocument/references`: the read-only twin of [`rename`], sharing its
/// classification.
///
/// For the classes [`rename`] refuses this falls back to a filtered scan
/// (see [`ApproxKind`]) rather than to nothing — reporting a superset is
/// acceptable for a read-only feature in a way that rewriting one is not.
/// `include_declaration` is honoured on the precise paths, where the
/// declaration is known.
pub fn find_references(
    analysis: &DocumentAnalysis,
    uri: &Url,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
    include_declaration: bool,
) -> Vec<Location> {
    let Some((_, target)) = classify(analysis, position, scope, files) else {
        return Vec::new();
    };
    match &target {
        Target::Local { name, binding } => match local_occurrences(analysis, name, *binding) {
            Ok(spans) => locations_for(
                analysis,
                uri,
                spans
                    .into_iter()
                    .filter(|span| include_declaration || span.start != binding.start),
            ),
            // The precise set refused (a lambda in scope, a same-scope
            // redeclaration). Reporting the filtered scan beats reporting
            // nothing, and nothing is rewritten.
            Err(_) => approximate_locations(name, ApproxKind::Qualified, files),
        },
        Target::Workspace { name, shape } => {
            match workspace_occurrences(analysis, uri, name, *shape, files) {
                Ok(found) => found
                    .iter()
                    .flat_map(|file| {
                        let declarations = declaration_spans(scope, name, file);
                        locations_for(
                            file.analysis,
                            &file.uri,
                            file.spans
                                .iter()
                                .copied()
                                .filter(|span| {
                                    include_declaration || !declarations.contains(&span.start)
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect(),
                Err(_) => approximate_locations(name, ApproxKind::Qualified, files),
            }
        }
        Target::Approximate { name, kind, .. } => approximate_locations(name, *kind, files),
    }
}

// -------------------------------------------------------------------
// Classification
// -------------------------------------------------------------------

/// The identifier span under `position` and what it names, or `None` when
/// the cursor is not on an identifier at all.
fn classify(
    analysis: &DocumentAnalysis,
    position: Position,
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Option<(Span, Target)> {
    let (idx, span, _) = ident_index_at(analysis, position)?;
    let tokens = &analysis.tokens;
    let source = analysis.masked_source();
    let name = span.text(source).to_string();

    let approximate = |kind, reason| {
        Some((
            span,
            Target::Approximate {
                name: name.clone(),
                kind,
                reason,
            },
        ))
    };

    // Syntactic position first: these say what the name is regardless of
    // what the symbol table knows about the spelling.
    if idx > 0 && tokens[idx - 1].kind == TokenKind::Dot {
        return approximate(
            ApproxKind::Member,
            "this is a class member; renaming one needs the type of every receiver, \
             which this server cannot resolve",
        );
    }
    if idx > 0 && tokens[idx - 1].kind == TokenKind::ColonColon {
        return approximate(
            ApproxKind::Qualified,
            "this is a qualified name; renaming the right-hand side of `::` needs \
             qualifier resolution this server does not do",
        );
    }
    if lexical::is_named_argument_label(tokens, idx) {
        return approximate(
            ApproxKind::Qualified,
            "this is a named-argument label; it names one of the callee's parameters, \
             not a binding here",
        );
    }

    // A local or parameter shadows anything above it, so it is checked
    // before the workspace. Innermost (largest declarator offset) wins.
    let binding = lexical::bindings_named(&analysis.file, tokens, source, &name)
        .into_iter()
        .filter(|binding| binding.start <= span.start && span.start < binding.scope_end)
        .max_by_key(|binding| binding.start);
    if let Some(binding) = binding {
        return Some((span, Target::Local { name, binding }));
    }

    let candidates = scope.lookup_workspace(&name);
    if candidates.is_empty() {
        // Say *why* when the spelling is visibly owned by something.
        if declares_class_member(scope, files, &name) {
            return approximate(
                ApproxKind::Member,
                "this is a class member; renaming one needs the type of every receiver, \
                 which this server cannot resolve",
            );
        }
        if declares_enum_value(scope, &name) {
            return approximate(
                ApproxKind::Qualified,
                "this is an enum value; it is visible both bare and as `Enum::Value`, \
                 which this server cannot tell apart from an unrelated name",
            );
        }
        if declares_namespaced(scope, &name) {
            return approximate(
                ApproxKind::Qualified,
                "this is a namespaced name; renaming one needs qualifier resolution \
                 this server does not do",
            );
        }
        return approximate(
            ApproxKind::Qualified,
            "no declaration for this name in the open workspace",
        );
    }

    // Every candidate must be a real definition of the same class, living
    // in a file this request can edit.
    let mut shape = None;
    for symbol in &candidates {
        if files.get(symbol.file_id).is_none() {
            return approximate(
                ApproxKind::Qualified,
                "this symbol is declared outside the open workspace",
            );
        }
        let candidate = match &symbol.kind {
            SymbolKind::Function { imported: true, .. } => {
                return approximate(
                    ApproxKind::Qualified,
                    "this name is an `import ... from` alias for a function defined \
                     in another plugin",
                )
            }
            SymbolKind::Function { .. } | SymbolKind::Variable { .. } => Shape::Value,
            SymbolKind::Class { .. }
            | SymbolKind::Interface { .. }
            | SymbolKind::Enum { .. }
            | SymbolKind::Funcdef { .. } => Shape::Type,
            SymbolKind::EnumValue { .. } => {
                return approximate(
                    ApproxKind::Qualified,
                    "this is an enum value; it is visible both bare and as `Enum::Value`, \
                     which this server cannot tell apart from an unrelated name",
                )
            }
            SymbolKind::Namespace => {
                return approximate(
                    ApproxKind::Qualified,
                    "renaming a namespace would have to rewrite every qualifier that \
                     names it, which this server cannot resolve",
                )
            }
        };
        if shape.get_or_insert(candidate) != &candidate {
            // A class and a function sharing one spelling: a bare use may
            // be either.
            return approximate(
                ApproxKind::Qualified,
                "this name is declared as more than one kind of symbol",
            );
        }
    }
    let shape = shape?;

    // A bare use inside a class or namespace that owns this spelling
    // resolves *there*, not to the top-level symbol, and telling the two
    // apart needs the resolution we do not have.
    if declares_class_member(scope, files, &name) {
        return approximate(
            ApproxKind::Member,
            "a class in this workspace declares a member with the same name, so a bare \
             use inside that class may be the member",
        );
    }
    if declares_enum_value(scope, &name) || declares_namespaced(scope, &name) {
        return approximate(
            ApproxKind::Qualified,
            "an enum value or namespaced declaration in this workspace shares this name, \
             so a bare use may resolve there",
        );
    }
    Some((span, Target::Workspace { name, shape }))
}

// -------------------------------------------------------------------
// Occurrence sets
// -------------------------------------------------------------------

/// Spans of every reference to a local or parameter, declarator included.
///
/// Refuses rather than guess when the range contains something the scope
/// model cannot see through.
fn local_occurrences(
    analysis: &DocumentAnalysis,
    name: &str,
    binding: Binding,
) -> Result<Vec<Span>, Refusal> {
    let source = analysis.masked_source();
    let tokens = &analysis.tokens;

    // A lambda opens its own frame, and neither its parameters nor its
    // locals are reachable from the AST walk (they live inside an
    // expression), so an identifier inside one may not be ours.
    if tokens.iter().any(|token| {
        token.kind == TokenKind::KwFunction
            && token.span.start > binding.start
            && token.span.end <= binding.scope_end
    }) {
        return Err(Refusal::new(
            "an anonymous function in this scope may declare its own binding with this \
             name, which this server cannot see",
        ));
    }

    let mut excluded = Vec::new();
    for redeclaration in lexical::redeclarations_in_range(
        &analysis.file,
        source,
        name,
        binding.start,
        binding.scope_end,
    ) {
        let Some(end) = lexical::innermost_scope_end(&analysis.file, tokens, redeclaration) else {
            return Err(Refusal::new(
                "a redeclaration of this name has no resolvable scope",
            ));
        };
        if end >= binding.scope_end {
            return Err(Refusal::new(
                "this name is declared twice in the same scope, so its uses may bind to \
                 either declaration",
            ));
        }
        // The nested declaration's initialiser is evaluated before its own
        // name is bound, so references there still resolve to us.
        if let Some(init) = lexical::own_initializer_span(&analysis.file, redeclaration) {
            excluded.push((redeclaration, init.start));
            excluded.push((init.end, end));
        } else {
            excluded.push((redeclaration, end));
        }
    }

    // `int x = x + 1;` initialises from the OUTER binding. Rewriting that
    // read would change which variable it names, and leaving it alone
    // would leave a dangling reference, so neither answer is safe.
    if let Some(init) = lexical::own_initializer_span(&analysis.file, binding.start) {
        if tokens.iter().any(|token| {
            token.kind == TokenKind::Ident
                && token.span.start >= init.start
                && token.span.end <= init.end
                && token.span.text(source) == name
        }) {
            return Err(Refusal::new(
                "this declaration's own initialiser reads a different binding with the \
                 same name",
            ));
        }
    }

    let spans = lexical::local_occurrences(
        tokens,
        source,
        name,
        binding.start,
        binding.scope_end,
        &excluded,
    );
    // Rewriting uses without rewriting the declaration would be worse than
    // refusing. The declarator is always the first span in range.
    if spans.first().map(|span| span.start) != Some(binding.start) {
        return Err(Refusal::new(
            "the declaration of this name could not be located in its own scope",
        ));
    }
    Ok(spans)
}

/// One file's share of a workspace symbol's occurrences.
struct FileOccurrences<'a> {
    /// `None` for a requesting document the snapshot does not hold, whose
    /// symbols are therefore not in the pool either.
    file_id: Option<usize>,
    uri: Url,
    analysis: &'a DocumentAnalysis,
    spans: Vec<Span>,
}

/// Per-file occurrence spans of a workspace symbol, over every open file.
///
/// The requesting document is covered even when the snapshot does not hold
/// it — `Backend` falls back to an analysis built on the fly for a buffer
/// the snapshot has not caught up with, and renaming every *other* file
/// while silently skipping the one under the cursor would leave the edited
/// buffer calling a name that no longer exists.
fn workspace_occurrences<'a>(
    analysis: &'a DocumentAnalysis,
    uri: &Url,
    name: &str,
    shape: Shape,
    files: &WorkspaceFiles<'a>,
) -> Result<Vec<FileOccurrences<'a>>, Refusal> {
    let mut out = Vec::new();
    let mut saw_requesting_document = false;
    for (file_id, (file_uri, file_analysis)) in files.files.iter() {
        if file_uri == uri {
            saw_requesting_document = true;
        }
        let spans = occurrences_in_file(file_analysis, name, shape)?;
        if !spans.is_empty() {
            out.push(FileOccurrences {
                file_id: Some(*file_id),
                uri: file_uri.clone(),
                analysis: file_analysis,
                spans,
            });
        }
    }
    if !saw_requesting_document {
        let spans = occurrences_in_file(analysis, name, shape)?;
        if !spans.is_empty() {
            out.push(FileOccurrences {
                file_id: None,
                uri: uri.clone(),
                analysis,
                spans,
            });
        }
    }
    if out.is_empty() {
        return Err(Refusal::new("no references to this symbol were found"));
    }
    Ok(out)
}

/// Occurrences of a workspace symbol within one file.
fn occurrences_in_file(
    analysis: &DocumentAnalysis,
    name: &str,
    shape: Shape,
) -> Result<Vec<Span>, Refusal> {
    let source = analysis.masked_source();
    let tokens = &analysis.tokens;

    // Locals and parameters spelled the same shadow us wholesale.
    let mut shadowed: Vec<(u32, u32)> = Vec::new();
    for binding in lexical::bindings_named(&analysis.file, tokens, source, name) {
        if let Some(init) = lexical::own_initializer_span(&analysis.file, binding.start) {
            if tokens.iter().any(|token| {
                token.kind == TokenKind::Ident
                    && token.span.start >= init.start
                    && token.span.end <= init.end
                    && token.span.text(source) == name
            }) {
                // `int g = g + 1;` reads US inside a range we are about to
                // skip wholesale, so skipping it would strand a reference.
                return Err(Refusal::new(
                    "a local declaration shadowing this symbol initialises itself from it, \
                     so the two cannot be separated",
                ));
            }
        }
        shadowed.push((binding.start, binding.scope_end));
    }

    let lambdas = lambda_body_regions(tokens);
    let mut spans = Vec::new();
    for (idx, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::Ident || token.span.text(source) != name {
            continue;
        }
        if idx > 0 && tokens[idx - 1].kind == TokenKind::Dot {
            continue;
        }
        let follows_colons = idx > 0 && tokens[idx - 1].kind == TokenKind::ColonColon;
        let precedes_colons = matches!(
            tokens.get(idx + 1).map(|next| next.kind),
            Some(TokenKind::ColonColon)
        );
        match shape {
            // `A::name` and `name::A` are both someone else's symbol.
            Shape::Value if follows_colons || precedes_colons => continue,
            // `Name::` IS this type; `Other::Name` is not.
            Shape::Type if follows_colons => continue,
            _ => {}
        }
        if lexical::is_named_argument_label(tokens, idx) {
            continue;
        }
        if shadowed
            .iter()
            .any(|(start, end)| token.span.start >= *start && token.span.start < *end)
        {
            continue;
        }
        if lambdas
            .iter()
            .any(|(start, end)| token.span.start >= *start && token.span.start < *end)
        {
            // A lambda's own parameters and locals are invisible to the
            // AST walk, so this may be a binding of its own.
            return Err(Refusal::new(
                "this name appears inside an anonymous function, which may declare its \
                 own binding with the same name",
            ));
        }
        spans.push(token.span);
    }
    Ok(spans)
}

/// Byte ranges of every anonymous-function body (`function(…) { … }`).
///
/// The AST walk does not descend into expressions, so a lambda's own
/// parameters and locals are invisible to [`lexical::bindings_named`].
/// Callers treat these regions as opaque.
fn lambda_body_regions(tokens: &[Token]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (idx, token) in tokens.iter().enumerate() {
        if token.kind != TokenKind::KwFunction {
            continue;
        }
        // `funcdef void CB();` has no body; a lambda's `{` is the first
        // brace before any `;`.
        let Some(open) = tokens[idx + 1..]
            .iter()
            .position(|next| matches!(next.kind, TokenKind::LBrace | TokenKind::Semi))
            .map(|offset| idx + 1 + offset)
            .filter(|open| tokens[*open].kind == TokenKind::LBrace)
        else {
            continue;
        };
        let mut depth = 0usize;
        for next in &tokens[open..] {
            match next.kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    // A truncated source can close more braces than it
                    // opened; `checked_sub` keeps a request handler from
                    // panicking on one.
                    let Some(remaining) = depth.checked_sub(1) else {
                        break;
                    };
                    depth = remaining;
                    if depth == 0 {
                        out.push((tokens[open].span.start, next.span.end));
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

// -------------------------------------------------------------------
// New-name collision checks — refuse, never shadow
// -------------------------------------------------------------------

fn local_new_name_conflict(
    analysis: &DocumentAnalysis,
    scope: &GlobalScope<'_>,
    binding: Binding,
    new_name: &str,
) -> Result<(), Refusal> {
    if scope.has_global_ident(new_name) {
        return Err(Refusal::new(format!(
            "`{new_name}` is already declared in this workspace or by the engine API"
        )));
    }
    let source = analysis.masked_source();
    let taken = analysis.tokens.iter().any(|token| {
        token.kind == TokenKind::Ident
            && token.span.start >= binding.start
            && token.span.end <= binding.scope_end
            && token.span.text(source) == new_name
    });
    if taken {
        return Err(Refusal::new(format!(
            "`{new_name}` is already used in this scope; renaming would capture it"
        )));
    }
    Ok(())
}

fn workspace_new_name_conflict(
    scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
    found: &[FileOccurrences<'_>],
    new_name: &str,
) -> Result<(), Refusal> {
    if scope.has_global_ident(new_name) || declares_class_member(scope, files, new_name) {
        return Err(Refusal::new(format!(
            "`{new_name}` is already declared in this workspace or by the engine API"
        )));
    }
    for file in found {
        let analysis = file.analysis;
        let source = analysis.masked_source();
        for binding in lexical::bindings_named(&analysis.file, &analysis.tokens, source, new_name) {
            if file
                .spans
                .iter()
                .any(|span| span.start >= binding.start && span.start < binding.scope_end)
            {
                return Err(Refusal::new(format!(
                    "a local named `{new_name}` is in scope at one of these references; \
                     renaming would capture it"
                )));
            }
        }
    }
    Ok(())
}

// -------------------------------------------------------------------
// Symbol-table queries
// -------------------------------------------------------------------

/// Owner and kind of every workspace symbol registered as `Owner::name`.
fn qualified_owners<'a>(scope: &'a GlobalScope<'_>, name: &str) -> Vec<(String, &'a SymbolKind)> {
    let suffix = format!("::{name}");
    scope
        .workspace()
        .all_symbols()
        .filter(|symbol| symbol.name.ends_with(&suffix))
        .map(|symbol| {
            (
                symbol.name[..symbol.name.len() - suffix.len()].to_string(),
                &symbol.kind,
            )
        })
        .collect()
}

/// True when some workspace class or interface declares a member spelled
/// `name` — then a bare use inside that class is the member, not a global.
///
/// A class's own constructor and destructor are registered as
/// `Thing::Thing`, and those *are* the class: they have to be renamed with
/// it, and treating them as an unrelated member would make every class
/// with a constructor unrenameable. So an owner spelled like the name
/// itself does not count.
fn declares_class_member(scope: &GlobalScope<'_>, files: &WorkspaceFiles, name: &str) -> bool {
    let owned = qualified_owners(scope, name).iter().any(|(owner, kind)| {
        owner != name
            && !matches!(kind, SymbolKind::EnumValue { .. })
            && scope.is_workspace_class(owner)
    });
    owned || declares_interface_method(files, name)
}

/// True when any open file's interface declares a method spelled `name`.
///
/// Interface methods reach the symbol table as *nothing at all*: an
/// interface is registered as `Interface { methods: [] }` and its methods
/// get no `I::name` symbols of their own, so neither [`qualified_owners`]
/// nor the symbol pool can see them. Without this, renaming a free
/// function that shares a spelling with an interface method rewrote the
/// interface's own declaration — a wrong rename. Read the ASTs instead.
fn declares_interface_method(files: &WorkspaceFiles, name: &str) -> bool {
    files.files.values().any(|(_, analysis)| {
        interface_method_named(&analysis.file.items, analysis.masked_source(), name)
    })
}

/// Recursive half of [`declares_interface_method`], carrying the source
/// each `Ident` span indexes into.
fn interface_method_named(items: &[Item], source: &str, name: &str) -> bool {
    items.iter().any(|item| match item {
        Item::Interface(iface) => iface
            .methods
            .iter()
            .any(|method| method.name.text(source) == name),
        Item::Namespace(ns) => interface_method_named(&ns.items, source, name),
        _ => false,
    })
}

fn declares_enum_value(scope: &GlobalScope<'_>, name: &str) -> bool {
    qualified_owners(scope, name)
        .iter()
        .any(|(_, kind)| matches!(kind, SymbolKind::EnumValue { .. }))
}

fn declares_namespaced(scope: &GlobalScope<'_>, name: &str) -> bool {
    qualified_owners(scope, name)
        .iter()
        .any(|(owner, _)| scope.workspace_namespace_declared(owner))
}

/// Occurrence offsets in `file` that are declarations rather than uses,
/// for `include_declaration: false`.
///
/// A `Symbol`'s span covers the WHOLE declaration — a function's body
/// included — so it cannot be compared against an identifier offset
/// directly. The declaring identifier is the first occurrence inside each
/// symbol's span (`void Helper() { Helper(); }` declares at the first and
/// recurses at the second).
fn declaration_spans(scope: &GlobalScope<'_>, name: &str, file: &FileOccurrences<'_>) -> Vec<u32> {
    let Some(file_id) = file.file_id else {
        return Vec::new();
    };
    scope
        .lookup_workspace(name)
        .iter()
        .filter(|symbol| symbol.file_id == file_id)
        .filter_map(|symbol| {
            file.spans
                .iter()
                .filter(|span| span.start >= symbol.span.start && span.end <= symbol.span.end)
                .map(|span| span.start)
                .min()
        })
        .collect()
}

// -------------------------------------------------------------------
// Read-only fallback
// -------------------------------------------------------------------

/// Filtered token scan for the classes [`rename`] refuses.
///
/// Deliberately approximate — it can over-report — but it never reports the
/// occurrences that are certainly a different symbol, which is what made
/// the old scan useless.
fn approximate_locations(name: &str, kind: ApproxKind, files: &WorkspaceFiles) -> Vec<Location> {
    let mut out = Vec::new();
    for (uri, analysis) in files.files.values() {
        let source = analysis.masked_source();
        let tokens = &analysis.tokens;
        // A local or parameter of the same name is definitely not the
        // symbol under the cursor, whichever approximate class this is.
        let shadowed: Vec<(u32, u32)> =
            lexical::bindings_named(&analysis.file, tokens, source, name)
                .into_iter()
                .map(|binding| (binding.start, binding.scope_end))
                .collect();
        let spans: Vec<Span> = tokens
            .iter()
            .enumerate()
            .filter_map(|(idx, token)| {
                if token.kind != TokenKind::Ident || token.span.text(source) != name {
                    return None;
                }
                if lexical::is_named_argument_label(tokens, idx) {
                    return None;
                }
                let after_dot = idx > 0 && tokens[idx - 1].kind == TokenKind::Dot;
                if !after_dot
                    && shadowed
                        .iter()
                        .any(|(start, end)| token.span.start >= *start && token.span.start < *end)
                {
                    return None;
                }
                match kind {
                    // A member is reached through `.`, or used bare inside
                    // its own class, which we cannot pin down further.
                    ApproxKind::Member => Some(token.span),
                    // A qualified name is never a `.member`.
                    ApproxKind::Qualified if after_dot => None,
                    ApproxKind::Qualified => Some(token.span),
                }
            })
            .collect();
        out.extend(locations_for(analysis, uri, spans));
    }
    out
}

// -------------------------------------------------------------------
// Range plumbing
// -------------------------------------------------------------------

fn edits_for(analysis: &DocumentAnalysis, spans: &[Span], new_name: &str) -> Vec<TextEdit> {
    let source = analysis.masked_source();
    spans
        .iter()
        .map(|span| TextEdit {
            range: span_to_range(source, *span),
            new_text: new_name.to_string(),
        })
        .collect()
}

fn locations_for(
    analysis: &DocumentAnalysis,
    uri: &Url,
    spans: impl IntoIterator<Item = Span>,
) -> Vec<Location> {
    let source = analysis.masked_source();
    spans
        .into_iter()
        .map(|span| Location {
            uri: uri.clone(),
            range: span_to_range(source, span),
        })
        .collect()
}

/// Refuse a parameter rename when any open file uses this spelling as a
/// named-argument label.
///
/// `f(p: 1)` *is* a reference to the parameter `p`, so the label would have
/// to move with the rename — but matching a label to its callee needs
/// overload resolution this server does not do, and rewriting the wrong
/// call's label is silent corruption.
fn named_argument_label_exists(
    _analysis: &DocumentAnalysis,
    name: &str,
    files: &WorkspaceFiles,
) -> Result<(), Refusal> {
    for (_, analysis) in files.files.values() {
        let source = analysis.masked_source();
        let tokens = &analysis.tokens;
        for (idx, token) in tokens.iter().enumerate() {
            if token.kind == TokenKind::Ident
                && token.span.text(source) == name
                && lexical::is_named_argument_label(tokens, idx)
            {
                return Err(Refusal::new(
                    "a call passes this parameter by name (`f(name: …)`); the label would \
                     have to be renamed too, and matching labels to callees needs overload \
                     resolution this server does not do",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::diagnostics::{offset_to_position, position_to_offset};
    use crate::server::test_support::TestWorkspace;

    /// Build the workspace and the per-request views the handlers build,
    /// and run `body` against them. A closure because `WorkspaceFiles`
    /// borrows a map that borrows the snapshot.
    fn with<R>(
        sources: &[(&str, &str)],
        body: impl FnOnce(&TestWorkspace, &GlobalScope<'_>, &WorkspaceFiles) -> R,
    ) -> R {
        let tw = TestWorkspace::files(sources);
        let scope = tw.scope();
        let map = tw.uri_map();
        let files = WorkspaceFiles { files: &map };
        body(&tw, &scope, &files)
    }

    /// Position of the start of the `nth` (0-based) occurrence of `needle`.
    /// Tests point at a distinctive slice (`"x = 1"`) so the cursor lands
    /// on the identifier that starts it.
    fn at(source: &str, needle: &str, nth: usize) -> Position {
        let mut from = 0usize;
        for _ in 0..nth {
            let found = source[from..]
                .find(needle)
                .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?}"));
            from += found + 1;
        }
        let found = source[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("occurrence {nth} of {needle:?}"));
        offset_to_position(source, from + found)
    }

    /// Apply a file's edits, asserting they do not overlap — an overlapping
    /// pair is a malformed `WorkspaceEdit` whatever the resulting text.
    fn apply(source: &str, edits: &[TextEdit]) -> String {
        let mut spans: Vec<(usize, usize, &str)> = edits
            .iter()
            .map(|edit| {
                (
                    position_to_offset(source, edit.range.start),
                    position_to_offset(source, edit.range.end),
                    edit.new_text.as_str(),
                )
            })
            .collect();
        spans.sort_by_key(|(start, _, _)| *start);
        for pair in spans.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "overlapping edits: {spans:?}");
        }
        let mut out = source.to_string();
        for (start, end, text) in spans.into_iter().rev() {
            out.replace_range(start..end, text);
        }
        out
    }

    /// Rename at the `nth` occurrence of `needle` in file `idx`. Returns
    /// the post-edit text of every file the edit touches, in file order.
    fn rename_at(
        sources: &[(&str, &str)],
        idx: usize,
        needle: &str,
        nth: usize,
        new_name: &str,
    ) -> Result<Vec<(usize, String)>, Refusal> {
        with(sources, |tw, scope, files| {
            let position = at(sources[idx].1, needle, nth);
            let edit = rename(
                tw.analysis_of(idx),
                &tw.uri_of(idx),
                position,
                new_name,
                scope,
                files,
            )?;
            let changes = edit.changes.expect("a rename must carry changes");
            let mut out = Vec::new();
            for (index, (_, source)) in sources.iter().enumerate() {
                if let Some(edits) = changes.get(&tw.uri_of(index)) {
                    assert!(!edits.is_empty(), "empty edit list for file {index}");
                    out.push((index, apply(source, edits)));
                }
            }
            assert_eq!(out.len(), changes.len(), "edits for a file not in sources");
            Ok(out)
        })
    }

    /// Single-file rename returning the new source text.
    fn renamed(source: &str, needle: &str, new_name: &str) -> Result<String, Refusal> {
        let changed = rename_at(&[("a", source)], 0, needle, 0, new_name)?;
        assert_eq!(changed.len(), 1, "a single-file rename touched {changed:?}");
        Ok(changed[0].1.clone())
    }

    /// The refusal message, asserting the rename was in fact declined.
    fn refused(source: &str, needle: &str, new_name: &str) -> String {
        match renamed(source, needle, new_name) {
            Ok(text) => panic!("expected a refusal, renamed to:\n{text}"),
            Err(refusal) => refusal.reason,
        }
    }

    /// Byte offsets of every reference, as `(file index, offset)`.
    fn ref_offsets(
        sources: &[(&str, &str)],
        idx: usize,
        needle: &str,
        nth: usize,
        include_declaration: bool,
    ) -> Vec<(usize, usize)> {
        with(sources, |tw, scope, files| {
            let position = at(sources[idx].1, needle, nth);
            let locations = find_references(
                tw.analysis_of(idx),
                &tw.uri_of(idx),
                position,
                scope,
                files,
                include_declaration,
            );
            let mut out: Vec<(usize, usize)> = locations
                .iter()
                .map(|location| {
                    let index = sources
                        .iter()
                        .enumerate()
                        .find(|(index, _)| tw.uri_of(*index) == location.uri)
                        .map(|(index, _)| index)
                        .expect("reference in an unknown file");
                    (
                        index,
                        position_to_offset(sources[index].1, location.range.start),
                    )
                })
                .collect();
            out.sort_unstable();
            out
        })
    }

    /// Offsets of every occurrence of `needle` in `source`.
    fn offsets_of(source: &str, needle: &str) -> Vec<usize> {
        source.match_indices(needle).map(|(at, _)| at).collect()
    }

    // ---------------------------------------------------------------
    // Locals — the ticket's headline case.
    // ---------------------------------------------------------------

    #[test]
    fn a_local_rename_leaves_the_outer_binding_it_shadows_alone() {
        let src = "int x = 1;\nvoid f() {\n    int x = 2;\n    x++;\n}\nvoid g() { x = 3; }\n";
        assert_eq!(
            renamed(src, "x = 2", "y").unwrap(),
            "int x = 1;\nvoid f() {\n    int y = 2;\n    y++;\n}\nvoid g() { x = 3; }\n"
        );
    }

    #[test]
    fn a_local_rename_leaves_a_member_access_alone() {
        let src = "void f(Thing@ obj) { int x = 1; obj.x = x; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f(Thing@ obj) { int y = 1; obj.x = y; }\n"
        );
    }

    #[test]
    fn a_local_rename_leaves_both_sides_of_a_qualified_name_alone() {
        let src = "void f() { int x = 1; A::x = x; x::A = 2; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { int y = 1; A::x = y; x::A = 2; }\n"
        );
    }

    #[test]
    fn a_local_rename_leaves_a_named_argument_label_alone() {
        let src = "void g(int w) {}\nvoid f() { int w = 1; g(w: w); }\n";
        assert_eq!(
            renamed(src, "w = 1", "v").unwrap(),
            "void g(int w) {}\nvoid f() { int v = 1; g(w: v); }\n"
        );
    }

    #[test]
    fn a_local_rename_leaves_another_functions_local_alone() {
        let src = "void f() { int x = 1; x++; }\nvoid g() { int x = 2; x++; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { int y = 1; y++; }\nvoid g() { int x = 2; x++; }\n"
        );
    }

    #[test]
    fn a_local_rename_never_leaves_its_file() {
        let changed = rename_at(
            &[
                ("a", "void f() { int x = 1; x++; }\n"),
                ("b", "void g() { int x = 2; x++; }\n"),
            ],
            0,
            "x = 1",
            0,
            "y",
        )
        .unwrap();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].0, 0);
        assert_eq!(changed[0].1, "void f() { int y = 1; y++; }\n");
    }

    #[test]
    fn a_for_init_local_stays_inside_the_for_statement() {
        let src = "void f() { for (int i = 0; i < 2; i++) { i; } }\nvoid g() { int i = 9; i++; }\n";
        assert_eq!(
            renamed(src, "i = 0", "n").unwrap(),
            "void f() { for (int n = 0; n < 2; n++) { n; } }\nvoid g() { int i = 9; i++; }\n"
        );
    }

    #[test]
    fn a_loop_body_local_stays_in_the_loop_body() {
        let src = "void f() { while (true) { int x = 1; x++; } }\nvoid g() { int x = 2; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { while (true) { int y = 1; y++; } }\nvoid g() { int x = 2; }\n"
        );
    }

    #[test]
    fn an_unbraced_if_body_reference_is_renamed() {
        let src = "void f() { int x = 1; if (x > 0) x = 2; else x = 3; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { int y = 1; if (y > 0) y = 2; else y = 3; }\n"
        );
    }

    #[test]
    fn a_switch_case_local_stays_in_its_case() {
        let src = "void f(int k) { switch (k) { case 1: { int x = 1; x++; } break; } }\nvoid g() { int x = 2; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f(int k) { switch (k) { case 1: { int y = 1; y++; } break; } }\nvoid g() { int x = 2; }\n"
        );
    }

    #[test]
    fn a_nested_redeclarations_range_is_left_alone() {
        let src = "void f() { int x = 1; { int x = 2; x++; } x++; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { int y = 1; { int x = 2; x++; } y++; }\n"
        );
    }

    #[test]
    fn a_local_rename_leaves_strings_and_comments_alone() {
        let src = "void f() { int x = 1; /* x */ string s = \"x x\"; x++; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { int y = 1; /* x */ string s = \"x x\"; y++; }\n"
        );
    }

    #[test]
    fn a_local_shadowing_a_parameter_renames_only_itself() {
        let src = "void f(int x) { { int x = 2; x++; } x++; }\n";
        assert_eq!(
            renamed(src, "x = 2", "y").unwrap(),
            "void f(int x) { { int y = 2; y++; } x++; }\n"
        );
    }

    #[test]
    fn a_local_shadowing_a_field_renames_only_itself() {
        let src =
            "class C {\n    int x;\n    void f() { int x = 1; x++; }\n    void g() { x = 2; }\n}\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "class C {\n    int x;\n    void f() { int y = 1; y++; }\n    void g() { x = 2; }\n}\n"
        );
    }

    #[test]
    fn a_local_rename_refuses_when_a_lambda_is_in_scope() {
        // A lambda opens its own frame and its parameters are invisible to
        // the AST walk, so an `x` inside one may not be ours.
        let src = "void f() { int x = 1; auto g = function() { x++; }; }\n";
        assert!(!refused(src, "x = 1", "y").is_empty());
    }

    #[test]
    fn a_local_rename_refuses_on_a_same_scope_redeclaration() {
        let src = "void f() { int x = 1; int x = 2; x++; }\n";
        assert!(!refused(src, "x = 1", "y").is_empty());
    }

    #[test]
    fn a_local_rename_refuses_when_its_own_initialiser_reads_the_outer_binding() {
        // `int x = x + 1` initialises from the OUTER x; rewriting it would
        // change which variable is read.
        let src = "int x = 1;\nvoid f() { int x = x + 1; x++; }\n";
        assert!(!refused(src, "x = x", "y").is_empty());
    }

    // ---------------------------------------------------------------
    // New-name collisions: refuse, never shadow.
    // ---------------------------------------------------------------

    #[test]
    fn a_rename_refuses_when_the_new_name_is_taken_in_the_same_scope() {
        let src = "void f() { int x = 1; int y = 2; y++; }\n";
        assert!(!refused(src, "x = 1", "y").is_empty());
    }

    #[test]
    fn a_rename_refuses_when_the_new_name_is_a_global() {
        let src = "int y = 0;\nvoid f() { int x = 1; x++; }\n";
        assert!(!refused(src, "x = 1", "y").is_empty());
    }

    #[test]
    fn a_rename_refuses_a_new_name_that_is_not_a_plain_identifier() {
        let src = "void f() { int x = 1; x++; }\n";
        for candidate in ["", "1bad", "int64", "a b", "x y", "a-b", "class"] {
            assert!(
                !refused(src, "x = 1", candidate).is_empty(),
                "accepted new name {candidate:?}"
            );
        }
    }

    // ---------------------------------------------------------------
    // Parameters.
    // ---------------------------------------------------------------

    #[test]
    fn a_parameter_rename_covers_the_parameter_and_its_body() {
        let src = "void f(int p) { p++; }\nvoid g(int p) { p--; }\n";
        assert_eq!(
            renamed(src, "p) { p++", "q").unwrap(),
            "void f(int q) { q++; }\nvoid g(int p) { p--; }\n"
        );
    }

    #[test]
    fn a_parameter_rename_never_leaves_its_file() {
        let changed = rename_at(
            &[
                ("a", "void f(int p) { p++; }\n"),
                ("b", "void g(int p) { p--; }\n"),
            ],
            0,
            "p) {",
            0,
            "q",
        )
        .unwrap();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].1, "void f(int q) { q++; }\n");
    }

    #[test]
    fn a_parameter_rename_refuses_when_a_named_argument_label_uses_the_name() {
        // `f(p: 1)` names this parameter; the label would have to move
        // with it, and matching labels to callees needs overload
        // resolution we do not have.
        let src = "void f(int p) { p++; }\nvoid g() { f(p: 1); }\n";
        assert!(!refused(src, "p) {", "q").is_empty());
    }

    #[test]
    fn a_parameter_rename_leaves_a_member_access_alone() {
        let src = "void f(Thing@ obj, int p) { obj.p = p; }\n";
        assert_eq!(
            renamed(src, "p) { obj", "q").unwrap(),
            "void f(Thing@ obj, int q) { obj.p = q; }\n"
        );
    }

    // ---------------------------------------------------------------
    // Fields and methods: refused, they need receiver types.
    // ---------------------------------------------------------------

    #[test]
    fn a_field_declaration_is_refused() {
        let src = "class C { int x; void f() { x = 1; } }\n";
        assert!(!refused(src, "x;", "y").is_empty());
    }

    #[test]
    fn a_method_declaration_is_refused() {
        let src = "class C { void Go() {} }\nvoid f(C@ c) { c.Go(); }\n";
        assert!(!refused(src, "Go() {}", "Run").is_empty());
    }

    #[test]
    fn a_member_access_is_refused() {
        let src = "class C { int x; }\nvoid f(C@ c) { c.x = 1; }\n";
        assert!(!refused(src, "x = 1", "y").is_empty());
    }

    #[test]
    fn an_implicit_this_field_reference_is_refused() {
        let src = "class C { int x; void f() { x = 1; } }\n";
        assert!(!refused(src, "x = 1", "y").is_empty());
    }

    // ---------------------------------------------------------------
    // Workspace symbols.
    // ---------------------------------------------------------------

    #[test]
    fn a_free_function_renames_across_files() {
        let changed = rename_at(
            &[
                ("a", "void Helper() {}\nvoid Main() { Helper(); }\n"),
                ("b", "void Other() { Helper(); }\n"),
            ],
            0,
            "Helper() {}",
            0,
            "Assist",
        )
        .unwrap();
        assert_eq!(changed.len(), 2, "{changed:?}");
        assert_eq!(
            changed[0].1,
            "void Assist() {}\nvoid Main() { Assist(); }\n"
        );
        assert_eq!(changed[1].1, "void Other() { Assist(); }\n");
    }

    #[test]
    fn a_free_function_rename_leaves_a_member_access_alone() {
        let src = "void Helper() {}\nvoid f(Thing@ some) { some.Helper(); Helper(); }\n";
        assert_eq!(
            renamed(src, "Helper() {}", "Assist").unwrap(),
            "void Assist() {}\nvoid f(Thing@ some) { some.Helper(); Assist(); }\n"
        );
    }

    #[test]
    fn a_free_function_rename_leaves_a_shadowing_local_alone() {
        let src =
            "void Helper() {}\nvoid f() { int Helper = 1; Helper++; }\nvoid g() { Helper(); }\n";
        assert_eq!(
            renamed(src, "Helper() {}", "Assist").unwrap(),
            "void Assist() {}\nvoid f() { int Helper = 1; Helper++; }\nvoid g() { Assist(); }\n"
        );
    }

    #[test]
    fn a_free_function_rename_refuses_when_a_class_declares_that_member() {
        // A bare `Helper()` inside C is that class's method, not ours.
        let src = "void Helper() {}\nclass C { void Helper() {} void f() { Helper(); } }\n";
        assert!(!refused(src, "Helper() {}", "Assist").is_empty());
    }

    #[test]
    fn a_global_variable_renames_across_files() {
        let changed = rename_at(
            &[
                ("a", "int counter = 0;\nvoid f() { counter += 1; }\n"),
                ("b", "void g() { counter += 2; }\n"),
            ],
            0,
            "counter = 0",
            0,
            "tally",
        )
        .unwrap();
        assert_eq!(changed.len(), 2, "{changed:?}");
        assert_eq!(changed[0].1, "int tally = 0;\nvoid f() { tally += 1; }\n");
        assert_eq!(changed[1].1, "void g() { tally += 2; }\n");
    }

    #[test]
    fn a_global_rename_refuses_when_a_shadowing_locals_initialiser_reads_it() {
        let src = "int g = 1;\nvoid f() { int g = g + 1; g++; }\n";
        assert!(!refused(src, "g = 1", "total").is_empty());
    }

    #[test]
    fn a_class_rename_rewrites_declaration_and_uses() {
        let src = "class Thing {}\nvoid f() { Thing t; Thing@ h = null; }\n";
        assert_eq!(
            renamed(src, "Thing {}", "Widget").unwrap(),
            "class Widget {}\nvoid f() { Widget t; Widget@ h = null; }\n"
        );
    }

    #[test]
    fn an_enum_rename_rewrites_the_qualifier_but_not_the_value() {
        let src = "enum E { A, B }\nvoid f() { E e = E::A; }\n";
        assert_eq!(
            renamed(src, "E { A", "Mode").unwrap(),
            "enum Mode { A, B }\nvoid f() { Mode e = Mode::A; }\n"
        );
    }

    // ---------------------------------------------------------------
    // Classes we refuse outright.
    // ---------------------------------------------------------------

    #[test]
    fn a_namespaced_function_is_refused() {
        let src = "namespace Ns { void F() {} }\nvoid g() { Ns::F(); }\n";
        assert!(!refused(src, "F() {}", "G").is_empty());
    }

    #[test]
    fn the_right_side_of_a_qualified_name_is_refused() {
        let src = "namespace Ns { void F() {} }\nvoid g() { Ns::F(); }\n";
        assert!(!refused(src, "F();", "G").is_empty());
    }

    #[test]
    fn an_enum_value_is_refused() {
        let src = "enum E { A, B }\nvoid f() { E e = E::A; int n = A; }\n";
        assert!(!refused(src, "A, B", "First").is_empty());
    }

    #[test]
    fn a_named_argument_label_under_the_cursor_is_refused() {
        let src = "void g(int w) {}\nvoid f() { g(w: 1); }\n";
        assert!(!refused(src, "w: 1", "v").is_empty());
    }

    #[test]
    fn an_unresolvable_bare_name_is_refused() {
        let src = "void f() { zzz(); }\n";
        assert!(!refused(src, "zzz", "yyy").is_empty());
    }

    #[test]
    fn a_cursor_that_is_not_on_an_identifier_is_refused() {
        let src = "void f() { int x = 1; }\n";
        assert!(!refused(src, "{ int", "y").is_empty());
    }

    // ---------------------------------------------------------------
    // prepareRename.
    // ---------------------------------------------------------------

    #[test]
    fn prepare_rename_returns_the_identifier_range() {
        let src = "void f() { int x = 1; x++; }\n";
        let range = with(&[("a", src)], |tw, scope, files| {
            prepare_rename(
                tw.analysis_of(0),
                &tw.uri_of(0),
                at(src, "x = 1", 0),
                scope,
                files,
            )
        })
        .unwrap();
        let start = position_to_offset(src, range.start);
        let end = position_to_offset(src, range.end);
        assert_eq!(&src[start..end], "x");
    }

    #[test]
    fn prepare_rename_refuses_exactly_where_rename_refuses() {
        // The point of prepareRename: the client learns "not here" before
        // the user types a new name.
        let cases: &[(&str, &str)] = &[
            ("class C { int x; void f() { x = 1; } }\n", "x;"),
            ("class C { void Go() {} }\n", "Go() {}"),
            ("namespace Ns { void F() {} }\n", "F() {}"),
            ("enum E { A, B }\n", "A, B"),
            ("void g(int w) {}\nvoid f() { g(w: 1); }\n", "w: 1"),
            ("void f() { zzz(); }\n", "zzz"),
            ("void f() { int x = 1; }\n", "{ int"),
        ];
        for (src, needle) in cases {
            let prepared = with(&[("a", *src)], |tw, scope, files| {
                prepare_rename(
                    tw.analysis_of(0),
                    &tw.uri_of(0),
                    at(src, needle, 0),
                    scope,
                    files,
                )
            });
            assert!(
                prepared.is_err(),
                "prepareRename accepted {needle:?} in {src:?}"
            );
        }
    }

    // ---------------------------------------------------------------
    // Encoding: LSP ranges are UTF-16, on BOTH ends.
    // ---------------------------------------------------------------

    #[test]
    fn ranges_are_utf16_columns_not_byte_offsets() {
        // The comment is 3 bytes per star but 1 UTF-16 unit per star, so a
        // byte-offset column would be 4 too far on each end.
        let src = "void f() { /* \u{2605}\u{2605} */ int xy = 1; xy++; }\n";
        let edits = with(&[("a", src)], |tw, scope, files| {
            rename(
                tw.analysis_of(0),
                &tw.uri_of(0),
                at(src, "xy = 1", 0),
                "ab",
                scope,
                files,
            )
        })
        .unwrap();
        let changes = edits.changes.unwrap();
        let edits = changes.values().next().unwrap();
        let decl = edits
            .iter()
            .min_by_key(|edit| edit.range.start.character)
            .unwrap();
        let byte_start = src.find("xy = 1").unwrap();
        let expected = src[..byte_start].encode_utf16().count() as u32;
        assert_eq!(decl.range.start.character, expected, "start column");
        assert_eq!(decl.range.end.character, expected + 2, "end column");
        assert_ne!(
            decl.range.start.character, byte_start as u32,
            "test is vacuous unless bytes and UTF-16 differ"
        );
    }

    #[test]
    fn crlf_sources_produce_correct_ranges() {
        let src = "void f() {\r\n    int x = 1;\r\n    x++;\r\n}\r\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() {\r\n    int y = 1;\r\n    y++;\r\n}\r\n"
        );
    }

    #[test]
    fn multi_byte_earlier_lines_do_not_shift_later_ranges() {
        let src = "// \u{e9}\u{e9}\u{e9} caf\u{e9}\nvoid f() { int x = 1; x++; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "// \u{e9}\u{e9}\u{e9} caf\u{e9}\nvoid f() { int y = 1; y++; }\n"
        );
    }

    // ---------------------------------------------------------------
    // Robustness: these run inside live request handlers.
    // ---------------------------------------------------------------

    #[test]
    fn truncated_or_unbalanced_sources_never_panic() {
        for src in [
            "void f() { int x = 1; { int x = 2; x++;",
            "void f() { int x = 1; { int x = 2; }",
            "void f() { int x = 1; { { int x = 2; x++; }",
            "void f( { int x = 1; { int x = 2; } }",
            "void f() { int x = 1; { int x = 2; x++; } } }",
            "class C { int x",
            "namespace Ns { void F(",
            "enum E { A,",
        ] {
            with(&[("a", src)], |tw, scope, files| {
                for offset in 0..src.len() {
                    let position = offset_to_position(src, offset);
                    let _ =
                        prepare_rename(tw.analysis_of(0), &tw.uri_of(0), position, scope, files);
                    let _ = rename(
                        tw.analysis_of(0),
                        &tw.uri_of(0),
                        position,
                        "zz",
                        scope,
                        files,
                    );
                    let _ = find_references(
                        tw.analysis_of(0),
                        &tw.uri_of(0),
                        position,
                        scope,
                        files,
                        true,
                    );
                }
            });
        }
    }

    // ---------------------------------------------------------------
    // find_references — the read-only twin of the same defect.
    // ---------------------------------------------------------------

    #[test]
    fn references_to_a_local_stay_in_its_scope() {
        let src = "int x = 1;\nvoid f() {\n    int x = 2;\n    x++;\n}\nvoid g() { x = 3; }\n";
        let found = ref_offsets(&[("a", src)], 0, "x = 2", 0, true);
        assert_eq!(
            found,
            vec![
                (0, src.find("x = 2").unwrap()),
                (0, src.find("x++").unwrap()),
            ]
        );
    }

    #[test]
    fn references_to_a_local_never_leave_its_file() {
        let sources: &[(&str, &str)] = &[
            ("a", "void f() { int x = 1; x++; }\n"),
            ("b", "void g() { int x = 2; x++; }\n"),
        ];
        let found = ref_offsets(sources, 0, "x = 1", 0, true);
        assert!(found.iter().all(|(index, _)| *index == 0), "{found:?}");
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn references_to_a_free_function_span_files() {
        let sources: &[(&str, &str)] = &[
            ("a", "void Helper() {}\nvoid Main() { Helper(); }\n"),
            ("b", "void Other() { Helper(); }\n"),
        ];
        let found = ref_offsets(sources, 0, "Helper() {}", 0, true);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().any(|(index, _)| *index == 1), "{found:?}");
    }

    #[test]
    fn references_skip_member_accesses_and_named_argument_labels() {
        let src = "void g(int w) {}\nvoid f(Thing@ o) { int w = 1; o.w = w; g(w: w); }\n";
        let found = ref_offsets(&[("a", src)], 0, "w = 1", 0, true);
        let labels = offsets_of(src, "w: ");
        let members = offsets_of(src, ".w");
        for (_, offset) in &found {
            assert!(!labels.contains(offset), "label at {offset} reported");
            assert!(
                !members.iter().any(|member| member + 1 == *offset),
                "member at {offset} reported"
            );
        }
        assert_eq!(found.len(), 3, "{found:?}");
    }

    // ---------------------------------------------------------------
    // Further symbol classes and scope shapes.
    // ---------------------------------------------------------------

    #[test]
    fn a_try_catch_local_stays_in_its_block() {
        let src =
            "void f() { try { int x = 1; x++; } catch { int y = 2; } }\nvoid g() { int x = 3; }\n";
        assert_eq!(
            renamed(src, "x = 1", "z").unwrap(),
            "void f() { try { int z = 1; z++; } catch { int y = 2; } }\nvoid g() { int x = 3; }\n"
        );
    }

    #[test]
    fn a_do_while_body_local_stays_in_the_body() {
        let src = "void f() { do { int x = 1; x++; } while (false); }\nvoid g() { int x = 2; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "void f() { do { int y = 1; y++; } while (false); }\nvoid g() { int x = 2; }\n"
        );
    }

    #[test]
    fn a_local_inside_a_namespaced_function_is_still_just_a_local() {
        let src = "namespace Ns {\n    void f() { int x = 1; x++; }\n}\nvoid g() { int x = 2; }\n";
        assert_eq!(
            renamed(src, "x = 1", "y").unwrap(),
            "namespace Ns {\n    void f() { int y = 1; y++; }\n}\nvoid g() { int x = 2; }\n"
        );
    }

    #[test]
    fn a_local_declared_inside_a_lambda_is_refused() {
        // Lambda bodies live inside expressions, which the AST walk does
        // not enter, so this binding is invisible to the scope model.
        let src = "void f() { auto g = function() { int q = 1; q++; }; }\n";
        assert!(!refused(src, "q = 1", "r").is_empty());
    }

    #[test]
    fn a_global_referenced_inside_a_lambda_is_refused() {
        // The lambda may declare its own `counter`, and we cannot see it.
        let src = "int counter = 0;\nvoid f() { auto g = function() { counter++; }; }\n";
        assert!(!refused(src, "counter = 0", "tally").is_empty());
    }

    #[test]
    fn an_interface_method_is_refused() {
        let src = "interface I { void Go(); }\nclass C : I { void Go() {} }\n";
        assert!(!refused(src, "Go();", "Run").is_empty());
    }

    #[test]
    fn a_funcdef_renames_like_a_type() {
        let src = "funcdef void Cb();\nvoid f(Cb@ handler) {}\n";
        assert_eq!(
            renamed(src, "Cb();", "Handler").unwrap(),
            "funcdef void Handler();\nvoid f(Handler@ handler) {}\n"
        );
    }

    #[test]
    fn a_workspace_rename_leaves_strings_and_comments_alone() {
        let src = "int counter = 0;\n// counter\nvoid f() { string s = \"counter\"; counter++; }\n";
        assert_eq!(
            renamed(src, "counter = 0", "tally").unwrap(),
            "int tally = 0;\n// counter\nvoid f() { string s = \"counter\"; tally++; }\n"
        );
    }

    #[test]
    fn a_global_rename_refuses_when_a_class_declares_that_member() {
        let src = "int counter = 0;\nclass C { int counter; void f() { counter++; } }\n";
        assert!(!refused(src, "counter = 0", "tally").is_empty());
    }

    #[test]
    fn a_workspace_rename_refuses_when_a_local_would_capture_the_new_name() {
        // `Assist` is a local at the call site: renaming `Helper` to it
        // would silently rebind that call.
        let src = "void Helper() {}\nvoid f() { int Assist = 1; Helper(); Assist++; }\n";
        assert!(!refused(src, "Helper() {}", "Assist").is_empty());
    }

    #[test]
    fn renaming_to_the_same_name_is_refused() {
        let src = "void f() { int x = 1; x++; }\n";
        assert!(!refused(src, "x = 1", "x").is_empty());
    }

    #[test]
    fn references_to_a_field_keep_member_accesses_and_drop_labels() {
        // Rename refuses here, but the read-only scan still filters: the
        // named-argument label is one of `g`'s parameters, not this field.
        let src = "class C { int w; }\nvoid g(int w) {}\nvoid f(C@ c) { c.w = 1; g(w: 2); }\n";
        let found = ref_offsets(&[("a", src)], 0, "w = 1", 0, true);
        let label = src.find("w: 2").unwrap();
        assert!(!found.contains(&(0, label)), "label reported: {found:?}");
        assert!(
            found.contains(&(0, src.find("c.w").unwrap() + 2)),
            "member access missing: {found:?}"
        );
    }

    #[test]
    fn a_class_rename_also_rewrites_its_constructor_and_destructor() {
        // Both are spelled with the class name; missing either leaves the
        // class uncompilable.
        let src = "class Thing {\n    Thing() {}\n    ~Thing() {}\n}\nvoid f() { Thing t; }\n";
        assert_eq!(
            renamed(src, "Thing {", "Widget").unwrap(),
            "class Widget {\n    Widget() {}\n    ~Widget() {}\n}\nvoid f() { Widget t; }\n"
        );
    }

    #[test]
    fn a_class_rename_rewrites_a_base_class_reference() {
        let src = "class Base {}\nclass Derived : Base {}\nvoid f(Base@ b) {}\n";
        assert_eq!(
            renamed(src, "Base {}", "Root").unwrap(),
            "class Root {}\nclass Derived : Root {}\nvoid f(Root@ b) {}\n"
        );
    }

    #[test]
    fn a_type_sharing_its_name_with_a_namespace_is_refused() {
        // `Thing::F()` qualifies the NAMESPACE. A type rename rewrites a
        // left-hand qualifier, so doing it here would repoint that call.
        let src = "class Thing {}\nnamespace Thing { void F() {} }\nvoid g() { Thing::F(); }\n";
        assert!(!refused(src, "Thing {}", "Widget").is_empty());
    }

    #[test]
    fn a_namespace_itself_is_refused() {
        let src = "namespace Ns { void F() {} }\nvoid g() { Ns::F(); }\n";
        assert!(!refused(src, "Ns {", "Other").is_empty());
    }

    #[test]
    fn an_overload_set_renames_together() {
        let src = "void f(int a) {}\nvoid f(float a) {}\nvoid g() { f(1); f(2.0f); }\n";
        assert_eq!(
            renamed(src, "f(int", "h").unwrap(),
            "void h(int a) {}\nvoid h(float a) {}\nvoid g() { h(1); h(2.0f); }\n"
        );
    }

    #[test]
    fn a_workspace_rename_covers_a_document_the_snapshot_does_not_hold() {
        // `Backend` falls back to an on-the-fly analysis when the snapshot
        // has not caught up with a buffer. Renaming every other file but
        // not that one would leave it calling a name that is gone.
        let tw = TestWorkspace::files(&[("a", "void Helper() {}\n")]);
        let scope = tw.scope();
        let map = tw.uri_map();
        let files = WorkspaceFiles { files: &map };

        let unsaved = "void Other() { Helper(); }\n";
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(unsaved);
        let unsaved_uri = Url::parse("file:///test/unsaved.as").unwrap();
        let edit = rename(
            &analysis,
            &unsaved_uri,
            at(unsaved, "Helper", 0),
            "Assist",
            &scope,
            &files,
        )
        .unwrap();
        let changes = edit.changes.unwrap();
        let edits = changes
            .get(&unsaved_uri)
            .expect("the requesting document must be edited");
        assert_eq!(apply(unsaved, edits), "void Other() { Assist(); }\n");
        assert!(
            changes.contains_key(&tw.uri_of(0)),
            "the declaring file must be edited too"
        );
    }

    #[test]
    fn a_free_function_rename_refuses_when_an_interface_declares_that_method() {
        // Interfaces are a member surface too: a bare `Go()` inside an
        // implementor is that method, not this function.
        let src = "void Go() {}\ninterface I { void Go(); }\n";
        assert!(!refused(src, "Go() {}", "Run").is_empty());
    }

    #[test]
    fn a_rename_never_edits_a_dependency_plugins_file() {
        // The snapshot holds dependency export files so they resolve. A
        // WorkspaceEdit naming one would have the editor rewrite another
        // plugin's source on disk, so the handlers hand rename only this
        // plugin's own files -- and a symbol DECLARED in a dependency is
        // then not ours to rename at all.
        let tw = TestWorkspace::files_with_flags(&[
            ("own", "void Main() { Helper(); }\n", true),
            ("dep", "void Helper() {}\n", false),
        ]);
        let scope = tw.scope();
        let map = tw.snapshot.own_uri_map();
        assert!(
            !map.values().any(|(uri, _)| uri == &tw.uri_of(1)),
            "own_uri_map must not carry the dependency file"
        );
        let files = WorkspaceFiles { files: &map };
        let own = "void Main() { Helper(); }\n";
        let refusal = rename(
            tw.analysis_of(0),
            &tw.uri_of(0),
            at(own, "Helper", 0),
            "Assist",
            &scope,
            &files,
        )
        .expect_err("a dependency's symbol is not ours to rename");
        assert!(!refusal.reason.is_empty());
    }

    #[test]
    fn references_can_exclude_a_workspace_symbols_declaration() {
        // A `Symbol`'s span covers the whole declaration, body included,
        // so the declaring identifier has to be found inside it — a
        // recursive call is a use, not the declaration.
        let sources: &[(&str, &str)] = &[
            ("a", "void Helper() { Helper(); }\n"),
            ("b", "void Other() { Helper(); }\n"),
        ];
        let with_decl = ref_offsets(sources, 0, "Helper() {", 0, true);
        let without = ref_offsets(sources, 0, "Helper() {", 0, false);
        assert_eq!(with_decl.len(), 3, "{with_decl:?}");
        assert_eq!(without.len(), 2, "{without:?}");
        let declaration = (0, sources[0].1.find("Helper").unwrap());
        assert!(with_decl.contains(&declaration), "{with_decl:?}");
        assert!(!without.contains(&declaration), "{without:?}");
        // The recursive call inside the body is a use, not the declaration.
        assert!(
            without.contains(&(0, sources[0].1.rfind("Helper").unwrap())),
            "{without:?}"
        );
    }

    #[test]
    fn approximate_references_drop_a_same_named_local() {
        // Rename refuses on a field, but the read-only scan must still not
        // report an unrelated local that merely shares the spelling.
        let src = "class C { int w; }\nvoid f(C@ c) { c.w = 1; }\nvoid g() { int w = 2; w++; }\n";
        let found = ref_offsets(&[("a", src)], 0, "w;", 0, true);
        let local_decl = src.find("w = 2").unwrap();
        assert!(
            !found.contains(&(0, local_decl)),
            "local reported: {found:?}"
        );
        assert!(
            found.contains(&(0, src.find("c.w").unwrap() + 2)),
            "member access missing: {found:?}"
        );
    }

    #[test]
    fn references_can_exclude_the_declaration() {
        let src = "void f() { int x = 1; x++; x--; }\n";
        let with_decl = ref_offsets(&[("a", src)], 0, "x = 1", 0, true);
        let without = ref_offsets(&[("a", src)], 0, "x = 1", 0, false);
        assert_eq!(with_decl.len(), 3, "{with_decl:?}");
        assert_eq!(without.len(), 2, "{without:?}");
        assert!(!without.contains(&(0, src.find("x = 1").unwrap())));
    }
}
