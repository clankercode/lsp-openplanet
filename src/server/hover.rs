//! Hover information provider.
//!
//! Priority order at the cursor:
//!
//! 0. `receiver.member` — resolved against the receiver's type
//! 1. Local variable / parameter in the enclosing function
//! 2. Field/property on the enclosing class (when the cursor is inside a method)
//! 3. Workspace symbol (user-defined function / class / enum / etc.)
//! 4. External type database (Openplanet core + Nadeo)
//!
//! The first hit wins. Everything below is built on top of the existing
//! `navigation::name_at_position` helper plus a small AST walker
//! (`scope_query`) so we don't need to re-run the full type checker.
//!
//! Callables render through ONE formatter ([`overload_block`]), whichever
//! source they came from: every overload is listed, so a name with three
//! signatures no longer renders as one (B033).

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::analysis_snapshot::CheckedFile;
use crate::lexer::TokenKind;
use crate::parser::ast::SourceFile;
use crate::server::diagnostics::position_to_offset;
use crate::server::navigation;
use crate::server::scope_query;
use crate::symbols::scope::{Symbol, SymbolKind};
use crate::typecheck::repr::TypeRepr;
use crate::typecheck::GlobalScope;
use crate::typedb::index::{FunctionInfo, MethodInfo, ParamInfo, TypeInfo};
use crate::typedb::TypeIndex;

/// Members of an external type listed in full before the rest are summarised.
/// The previous `.take(6)` cut the list with no marker at all, so a user
/// could not tell a type had more members than were shown (B033).
const MEMBER_LIST_CAP: usize = 20;

/// Stand-in for a parameter default the workspace symbol table does not
/// keep. `min_args` records *which* parameters are optional but not what
/// their default expression was, so the marker says "optional" without
/// inventing a value.
const OPAQUE_DEFAULT: &str = "...";

/// Depth limit for the external inheritance walk, matching the checker's.
const MAX_PARENT_HOPS: usize = 32;

pub fn hover(
    analysis: &DocumentAnalysis,
    checked: Option<&CheckedFile<'_>>,
    position: Position,
    scope: &GlobalScope<'_>,
) -> Option<Hover> {
    let source = analysis.masked_source();
    let qualified = navigation::name_at_position(analysis, position)?;
    let bare = qualified
        .rsplit("::")
        .next()
        .unwrap_or(&qualified)
        .to_string();
    let offset = position_to_offset(source, position) as u32;

    // 0) `receiver.member`: the name belongs to the receiver's type, so
    //    resolve it there before any by-name ladder below can match an
    //    unrelated symbol that merely shares the name.
    if let Some(tok_idx) = navigation::ident_token_index_at(analysis, position) {
        if is_member_access(analysis, tok_idx) {
            if let Some(md) = member_hover(analysis, checked, scope, tok_idx, offset, &bare) {
                return Some(markdown_hover(md));
            }
            if let Some(md) = recorded_type_label(checked, offset, &bare) {
                return Some(markdown_hover(md));
            }
        }
    }

    // 1) + 2) Recorded expression type at the cursor (GH #42): the checker
    // already resolved locals, params, fields and initializers — query its
    // span→type map instead of re-walking the AST with scope_query.
    let mut recorded_fallback: Option<String> = None;
    if !qualified.contains("::") {
        if let Some(checked) = checked {
            if let Some(ty) = checked.type_at_offset(offset) {
                if is_field_name(analysis, offset, &bare) {
                    let md = fence(&format!(
                        "(field) {}:{} {}",
                        class_name_at(analysis, offset),
                        ty.display(),
                        bare
                    ));
                    return Some(markdown_hover(md));
                }
                if let Some(md) = enclosing_method_hover(analysis, scope, offset, &bare) {
                    return Some(markdown_hover(md));
                }
                if scope_query::local_type_at(source, &analysis.file, offset, &bare).is_some() {
                    let md = fence(&format!("(local) {} {}", display_or_unknown(ty), bare));
                    return Some(markdown_hover(md));
                }
                // Not a field, not a method, not a local: `(local) T name` is
                // a lie *and* it shadowed the formatters below, which is why
                // hovering a function showed no parameters at all (B033).
                // Fall through; keep the recorded type as a last resort.
                recorded_fallback = Some(fence(&format!(
                    "(local) {} {}",
                    display_or_unknown(ty),
                    bare
                )));
            }
        }
    }

    // Legacy fallback when no checked view is available (should not happen
    // in production; keeps the query surface optional for tests).
    if !qualified.contains("::") && checked.is_none() {
        let file: &SourceFile = &analysis.file;
        if let Some(ty_text) = scope_query::local_type_at(source, file, offset, &bare) {
            let ty_display = if ty_text.is_empty() {
                "?"
            } else {
                ty_text.as_str()
            };
            let md = fence(&format!("(local) {} {}", ty_display, bare));
            return Some(markdown_hover(md));
        }
        if let Some(cls) = scope_query::find_enclosing_class(file, offset) {
            if let Some(ty_text) = scope_query::class_member_type(cls, source, &bare) {
                let cls_name = cls.name.text(source);
                let md = fence(&format!("(field) {}::{}: {}", cls_name, bare, ty_text));
                return Some(markdown_hover(md));
            }
        }
    }

    // 3) Workspace symbol lookup (namespace-aware: bare names inside a
    //    namespace resolve against `Ns::name` symbol keys). Every candidate
    //    is rendered, so overloads are not silently dropped.
    let candidates = navigation::lookup_workspace_symbols(scope, analysis, &qualified, offset);
    if let Some(md) = format_workspace_symbols(&candidates) {
        return Some(markdown_hover(md));
    }

    // 4) External type database.
    if let Some(index) = scope.external() {
        if let Some(md) = lookup_external(&qualified, index) {
            return Some(markdown_hover(md));
        }
        // B070: B052's ancestor walk resolves a globally visible enum through
        // ANY real scope, including a namespace the workspace itself declares
        // — `namespace Foo {}` makes `Foo::VirtualKey` legal, and the game
        // agrees. `lookup_external` cannot see that: it reaches
        // `TypeIndex::source_reference_key`, whose scope test is
        // `TypeIndex::scope_exists`, the index-only half. The index is right
        // not to know what a workspace declared; the resolver is the half that
        // does, so ask it for the canonical key and look THAT up. Hover then
        // answers for exactly the spellings the checker accepts.
        // The gate is load-bearing, not belt-and-braces. `canonical_enum_key`
        // is a CANONICALIZER: every other caller reaches it only after the
        // checker has accepted the spelling, so it may be wider than
        // acceptance and is — it still carries the trailing-suffix match that
        // `has_enum` deliberately dropped (B007), under which `Plot::Axis` is
        // a suffix of the real key `UI::Plot::Axis`. The game rejects that
        // with `Namespace 'Plot' doesn't exist.` Calling it on arbitrary
        // hovered text without this gate made hover answer for 24 corpus
        // spellings the checker rejects — measured, see the sweep below.
        //
        // Why this is SECOND when it is the more correct path. The plain
        // lookup above is the cheap common case: almost every hover is a
        // spelling the index can key directly, and it needs no resolver
        // round-trip. This retry is the workspace-aware completion of it, not
        // a rival — so do not "simplify" by deleting either one. Deleting the
        // retry restores B070; deleting the plain lookup makes every hover pay
        // for a case that is rare.
        //
        // Ordering is safe because the primary cannot answer WRONGLY here,
        // only miss. `TypeIndex` is built from the JSON dumps alone and has no
        // workspace knowledge: `source_name_key` reads `source_names`, and the
        // walk gates on `TypeIndex::scope_exists`, which holds only Core
        // namespaces and Nadeo enum-declaring classes. So for a namespace only
        // the workspace declares it must return `None`, and when the head IS
        // also a real index scope both paths select through the same
        // `global_enum_key(tail)` and agree. Measured over the corpus plus
        // synthesized workspace spellings, including a workspace that declares
        // `namespace UI {}` to collide with the real Core scope: 591 spellings
        // where both answer, 0 disagreements, 0 cases of the primary answering
        // for a workspace-only head.
        if scope.has_enum(&qualified) {
            if let Some(key) = scope.canonical_enum_key(&qualified) {
                if let Some(md) = lookup_external(&key, index) {
                    return Some(markdown_hover(md));
                }
            }
        }
    }

    recorded_fallback.map(markdown_hover)
}

// === shared rendering =======================================================

/// One rendered parameter of a signature.
struct HoverParam {
    /// Type text including any `const` / `&in` decoration.
    ty: String,
    /// Declared name; `None` or empty renders as `_`.
    name: Option<String>,
    /// Default expression text, or [`OPAQUE_DEFAULT`] when the source only
    /// records *that* the parameter is optional.
    default: Option<String>,
}

impl HoverParam {
    fn render(&self) -> String {
        let name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("_");
        let mut out = format!("{} {}", self.ty.trim(), name);
        if let Some(default) = &self.default {
            out.push_str(" = ");
            out.push_str(default);
        }
        out
    }
}

/// One overload of a callable, ready to be listed.
struct HoverSig {
    /// Parameter count — the primary sort key, so the listing order does not
    /// depend on symbol-table or type-database insertion order.
    arity: usize,
    text: String,
    doc: Option<String>,
}

fn fence(body: &str) -> String {
    format!("```angelscript\n{}\n```", body)
}

fn display_or_unknown(ty: &TypeRepr) -> String {
    let display = ty.display();
    if display.is_empty() {
        "?".to_string()
    } else {
        display
    }
}

fn render_params(params: &[HoverParam]) -> String {
    params
        .iter()
        .map(HoverParam::render)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `rt Name(params)`, with an empty return type rendered as `void`.
fn signature(return_type: &str, name: &str, params: &[HoverParam]) -> String {
    let rt = return_type.trim();
    let rt = if rt.is_empty() { "void" } else { rt };
    format!("{} {}({})", rt, name, render_params(params))
}

/// The single formatter every callable hover goes through.
///
/// Overloads are deduplicated and ordered by (arity, text) so the same hover
/// always renders the same bytes regardless of the order the symbol table or
/// type database happened to yield them in. A lone overload renders exactly
/// as it did before — no header, no list formatting.
fn overload_block(mut sigs: Vec<HoverSig>) -> Option<String> {
    if sigs.is_empty() {
        return None;
    }
    sigs.sort_by(|a, b| a.arity.cmp(&b.arity).then_with(|| a.text.cmp(&b.text)));
    sigs.dedup_by(|a, b| a.text == b.text);

    let mut out = String::new();
    if sigs.len() > 1 {
        out.push_str(&format!("*{} overloads*\n\n", sigs.len()));
    }
    let body: Vec<&str> = sigs.iter().map(|s| s.text.as_str()).collect();
    out.push_str(&fence(&body.join("\n")));

    // Distinct docs only: overloads of the same function usually repeat one
    // description, and repeating it once per signature is noise.
    let mut docs: Vec<&str> = Vec::new();
    for sig in &sigs {
        let Some(doc) = sig.doc.as_deref().map(str::trim) else {
            continue;
        };
        if !doc.is_empty() && !docs.contains(&doc) {
            docs.push(doc);
        }
    }
    for doc in docs {
        out.push_str("\n\n");
        out.push_str(doc);
    }
    Some(out)
}

/// Shared enum rendering — the workspace and type-database paths used to
/// disagree about whether the numeric values were shown at all (B033).
fn enum_block(name: &str, values: &[(String, Option<i64>)]) -> String {
    let lines: Vec<String> = values
        .iter()
        .map(|(value_name, value)| match value {
            Some(v) => format!("  {} = {},", value_name, v),
            None => format!("  {},", value_name),
        })
        .collect();
    fence(&format!("enum {} {{\n{}\n}}", name, lines.join("\n")))
}

/// Append a doc comment / description below a rendered block.
fn with_doc(block: String, doc: Option<&String>) -> String {
    match doc.map(|d| d.trim()).filter(|d| !d.is_empty()) {
        Some(doc) => format!("{}\n\n{}", block, doc),
        None => block,
    }
}

// === workspace symbols ======================================================

/// Parameters of a workspace function symbol. The symbol table stores
/// `(name, type_text)` pairs — the type text already carries the source
/// `const` / `&in` / `&out` decoration — plus `min_args`, which is the only
/// record that a trailing parameter was optional.
fn workspace_params(params: &[(String, String)], min_args: usize) -> Vec<HoverParam> {
    params
        .iter()
        .enumerate()
        .map(|(i, (name, ty))| HoverParam {
            ty: ty.clone(),
            name: Some(name.clone()),
            default: (i >= min_args).then(|| OPAQUE_DEFAULT.to_string()),
        })
        .collect()
}

fn workspace_function_sig(sym: &Symbol, prefix: Option<&str>) -> Option<HoverSig> {
    let SymbolKind::Function {
        return_type,
        params,
        min_args,
        ..
    } = &sym.kind
    else {
        return None;
    };
    let rendered = workspace_params(params, *min_args);
    let mut text = signature(return_type, &sym.name, &rendered);
    if let Some(prefix) = prefix {
        text = format!("{} {}", prefix, text);
    }
    Some(HoverSig {
        arity: rendered.len(),
        text,
        doc: sym.doc.clone(),
    })
}

/// Render every workspace candidate for one name: the full overload set when
/// they are functions, otherwise the preferred single symbol (classes, enums
/// and variables cannot overload).
fn format_workspace_symbols(syms: &[&Symbol]) -> Option<String> {
    let sigs: Vec<HoverSig> = syms
        .iter()
        .filter_map(|s| workspace_function_sig(s, None))
        .collect();
    if !sigs.is_empty() {
        return overload_block(sigs);
    }
    format_workspace_symbol(navigation::prefer_definition(syms)?)
}

fn format_workspace_symbol(sym: &Symbol) -> Option<String> {
    let block = match &sym.kind {
        SymbolKind::Function { .. } => workspace_function_sig(sym, None).map(|s| fence(&s.text))?,
        SymbolKind::Variable { type_name } => {
            let ty = if type_name.is_empty() {
                "?"
            } else {
                type_name.as_str()
            };
            fence(&format!("{} {}", ty, sym.name))
        }
        SymbolKind::Class { parents, .. } => {
            let mut s = format!("class {}", sym.name);
            if !parents.is_empty() {
                s.push_str(&format!(" : {}", parents.join(", ")));
            }
            fence(&s)
        }
        SymbolKind::Interface { .. } => fence(&format!("interface {}", sym.name)),
        SymbolKind::Enum { values } => enum_block(&sym.name, values),
        SymbolKind::EnumValue { enum_name, value } => {
            let v = value
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            fence(&format!("{}::{} = {}", enum_name, sym.name, v))
        }
        SymbolKind::Namespace => fence(&format!("namespace {}", sym.name)),
        SymbolKind::Funcdef {
            return_type,
            params,
            is_typedef: _,
        } => {
            let rendered = workspace_params(params, params.len());
            fence(&format!(
                "funcdef {}",
                signature(return_type, &sym.name, &rendered)
            ))
        }
    };
    Some(with_doc(block, sym.doc.as_ref()))
}

// === methods ================================================================

/// True when the identifier at `tok_idx` is a member access (preceded by `.`).
fn is_member_access(analysis: &DocumentAnalysis, tok_idx: usize) -> bool {
    tok_idx >= 1 && analysis.tokens[tok_idx - 1].kind == TokenKind::Dot
}

/// The old `(local) T name` label, kept as a fallback for member accesses
/// whose receiver type could not be resolved.
fn recorded_type_label(
    checked: Option<&CheckedFile<'_>>,
    offset: u32,
    bare: &str,
) -> Option<String> {
    let ty = checked?.type_at_offset(offset)?;
    Some(fence(&format!(
        "(local) {} {}",
        display_or_unknown(ty),
        bare
    )))
}

/// Hover for `receiver.member`: resolve the receiver's recorded type and
/// render the member against *that* type.
fn member_hover(
    analysis: &DocumentAnalysis,
    checked: Option<&CheckedFile<'_>>,
    scope: &GlobalScope<'_>,
    tok_idx: usize,
    offset: u32,
    bare: &str,
) -> Option<String> {
    let owner = navigation::receiver_type_name(analysis, checked, tok_idx, offset)?;
    method_or_member_hover(scope, &owner, bare)
}

/// Render `owner::member`: workspace methods first (walking the class's
/// inheritance chain), then external methods, then an external property.
fn method_or_member_hover(scope: &GlobalScope<'_>, owner: &str, member: &str) -> Option<String> {
    let workspace: Vec<HoverSig> = scope
        .lookup_member_symbols_with_inheritance(owner, member)
        .iter()
        .filter_map(|s| workspace_function_sig(s, Some("(method)")))
        .collect();
    if let Some(md) = overload_block(workspace) {
        return Some(md);
    }
    let index = scope.external()?;
    let key = resolve_external_type_key(index, owner)?;
    let methods = external_methods(index, &key, member);
    let sigs: Vec<HoverSig> = methods
        .iter()
        .map(|m| HoverSig {
            arity: m.params.len(),
            text: format!(
                "(method) {}",
                external_signature(
                    m.decl.as_deref(),
                    &m.return_type,
                    &format!("{}::{}", key, m.name),
                    &m.params,
                )
            ),
            doc: m.doc.clone(),
        })
        .collect();
    if let Some(md) = overload_block(sigs) {
        return Some(md);
    }
    let (owner_key, prop) = external_property(index, &key, member)?;
    Some(with_doc(
        fence(&format!(
            "(property) {} {}::{}",
            prop.type_name, owner_key, prop.name
        )),
        prop.doc.as_ref(),
    ))
}

/// Hover for a bare name that resolves to a method of the class the cursor
/// is inside — including one inherited from a base class.
fn enclosing_method_hover(
    analysis: &DocumentAnalysis,
    scope: &GlobalScope<'_>,
    offset: u32,
    bare: &str,
) -> Option<String> {
    let cls = class_name_at(analysis, offset);
    if cls.is_empty() {
        return None;
    }
    let sigs: Vec<HoverSig> = scope
        .lookup_member_symbols_with_inheritance(&cls, bare)
        .iter()
        .filter_map(|s| workspace_function_sig(s, Some("(method)")))
        .collect();
    overload_block(sigs)
}

/// True when the ident at `offset` is a field of the enclosing class
/// (drives the `(field)` hover label, GH #42).
fn is_field_name(analysis: &DocumentAnalysis, offset: u32, bare: &str) -> bool {
    let file: &SourceFile = &analysis.file;
    scope_query::find_enclosing_class(file, offset)
        .and_then(|cls| scope_query::class_member_type(cls, analysis.masked_source(), bare))
        .is_some()
}

/// Qualified name of the class enclosing `offset`, when any.
fn class_name_at(analysis: &DocumentAnalysis, offset: u32) -> String {
    let file: &SourceFile = &analysis.file;
    scope_query::find_enclosing_class(file, offset)
        .map(|cls| cls.name.text(analysis.masked_source()).to_string())
        .unwrap_or_default()
}

fn markdown_hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

// === external type database =================================================

/// Parameters of a type-database callable.
fn typedb_params(params: &[ParamInfo]) -> Vec<HoverParam> {
    params
        .iter()
        .map(|p| {
            let mut ty = p.type_name.trim().to_string();
            // Core `typedecl` text carries neither `const` nor the `&`
            // decoration — the `typeflags` bits are authoritative there;
            // Nadeo signature text already carries both, so only add what is
            // missing.
            if p.is_const && !ty.contains("const") {
                ty.insert_str(0, "const ");
            }
            if !ty.contains('&') {
                if p.is_inout {
                    ty.push_str(" &inout");
                } else if p.is_out {
                    ty.push_str(" &out");
                } else if p.is_in {
                    ty.push_str(" &in");
                }
            }
            HoverParam {
                ty,
                name: p.name.clone(),
                default: p.default.clone(),
            }
        })
        .collect()
}

/// Signature text for a type-database callable.
///
/// Prefers the database's own `decl` string — the game's exact declaration,
/// including `const` qualifiers the structured `ParamInfo` flags cannot
/// express (Core marks `const T@` with no typeflags bit at all) — with the
/// declared name rewritten to the name hover was asked about. Falls back to
/// reconstruction from `ParamInfo` for the Nadeo half, which stores no
/// `decl`.
fn external_signature(
    decl: Option<&str>,
    return_type: &str,
    qualified: &str,
    params: &[ParamInfo],
) -> String {
    if let Some(text) = decl.and_then(|d| requalify_decl(d, qualified)) {
        return text;
    }
    signature(return_type, qualified, &typedb_params(params))
}

/// Rewrite the declared name in a `decl` string: `bool Button(...)` with
/// `UI::Button` gives `bool UI::Button(...)`. `None` when the text is not
/// shaped like a declaration, so callers fall back to reconstruction.
fn requalify_decl(decl: &str, qualified: &str) -> Option<String> {
    let open = decl.find('(')?;
    let (head, tail) = decl.split_at(open);
    let head = head.trim_end();
    let cut = head.rfind(' ').map(|i| i + 1).unwrap_or(0);
    if head[cut..].is_empty() {
        return None;
    }
    Some(format!("{}{}{}", &head[..cut], qualified, tail))
}

/// Resolve a type name to a key the index holds, accepting Nadeo short names.
fn resolve_external_type_key(index: &TypeIndex, name: &str) -> Option<String> {
    if index.lookup_type(name).is_some() {
        return Some(name.to_string());
    }
    if name.contains("::") {
        return None;
    }
    index
        .find_by_short_name(name)
        .iter()
        .find(|key| index.lookup_type(key).is_some())
        .cloned()
}

/// Methods named `member` on an external type, walking `parent` — the same
/// chain the checker walks, but keeping the `MethodInfo`s so hover can render
/// each overload's own `decl` and description.
fn external_methods<'i>(index: &'i TypeIndex, key: &str, member: &str) -> Vec<&'i MethodInfo> {
    let mut current = Some(key.to_string());
    for _ in 0..MAX_PARENT_HOPS {
        let Some(name) = current.take() else { break };
        let Some(info) = index.lookup_type(&name) else {
            break;
        };
        let hits: Vec<&MethodInfo> = info.methods.iter().filter(|m| m.name == member).collect();
        if !hits.is_empty() {
            return hits;
        }
        current = info
            .parent
            .as_ref()
            .and_then(|p| resolve_external_type_key(index, p));
    }
    Vec::new()
}

/// The declaring type key and property named `member`, walking `parent`.
fn external_property<'i>(
    index: &'i TypeIndex,
    key: &str,
    member: &str,
) -> Option<(String, &'i crate::typedb::index::PropertyInfo)> {
    let mut current = Some(key.to_string());
    for _ in 0..MAX_PARENT_HOPS {
        let name = current.take()?;
        let info = index.lookup_type(&name)?;
        if let Some(p) = info.properties.iter().find(|p| p.name == member) {
            return Some((name, p));
        }
        current = info
            .parent
            .as_ref()
            .and_then(|p| resolve_external_type_key(index, p));
    }
    None
}

fn format_external_type(qualified: &str, ty: &TypeInfo) -> String {
    let mut info = format!("**{}**", qualified);
    if let Some(parent) = &ty.parent {
        info.push_str(&format!(" : {}", parent));
    }
    if let Some(doc) = &ty.doc {
        info.push_str(&format!("\n\n{}", doc));
    }
    if ty.properties.is_empty() && ty.methods.is_empty() {
        return info;
    }
    let mut lines: Vec<String> = Vec::new();
    for p in ty.properties.iter().take(MEMBER_LIST_CAP) {
        lines.push(format!("{} {};", p.type_name, p.name));
    }
    if let Some(rest) = ty.properties.len().checked_sub(MEMBER_LIST_CAP) {
        if rest > 0 {
            lines.push(format!("// ... and {} more properties", rest));
        }
    }
    for m in ty.methods.iter().take(MEMBER_LIST_CAP) {
        lines.push(format!(
            "{};",
            external_signature(m.decl.as_deref(), &m.return_type, &m.name, &m.params)
        ));
    }
    if let Some(rest) = ty.methods.len().checked_sub(MEMBER_LIST_CAP) {
        if rest > 0 {
            lines.push(format!("// ... and {} more methods", rest));
        }
    }
    info.push_str("\n\n");
    info.push_str(&fence(&lines.join("\n")));
    info
}

fn lookup_external(qualified: &str, index: &TypeIndex) -> Option<String> {
    let source_key = index.source_reference_key(qualified);

    if let Some(ty) = source_key.and_then(|key| index.lookup_type(key)) {
        return Some(format_external_type(qualified, ty));
    }

    if let Some(fns) = index.lookup_source_function(qualified) {
        // Every overload, not just `fns[0]` — the type database keeps a Vec
        // precisely because AngelScript functions overload (B033).
        let sigs: Vec<HoverSig> = fns
            .iter()
            .map(|f: &FunctionInfo| HoverSig {
                arity: f.params.len(),
                text: external_signature(f.decl.as_deref(), &f.return_type, qualified, &f.params),
                doc: f.doc.clone(),
            })
            .collect();
        if let Some(md) = overload_block(sigs) {
            return Some(md);
        }
    }

    if let Some(fd) = source_key.and_then(|key| index.lookup_funcdef(key)) {
        // Funcdefs are types, not callables, so they never overload — but
        // they do have a signature, and the workspace path already showed
        // one (B033).
        let sig = external_signature(fd.decl.as_deref(), &fd.return_type, qualified, &fd.params);
        return Some(with_doc(
            fence(&format!("funcdef {}", sig)),
            fd.doc.as_ref(),
        ));
    }

    if let Some(en) = source_key.and_then(|key| index.lookup_source_enum(key)) {
        let values: Vec<(String, Option<i64>)> = en
            .values
            .iter()
            .map(|(name, v)| (name.clone(), Some(*v)))
            .collect();
        return Some(enum_block(&en.name, &values));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support::TestWorkspace;

    /// Find a 1-based column of the first occurrence of `needle` starting at
    /// position `from` (byte offset). Returns the (line, character).
    fn pos_of(source: &str, needle: &str, occurrence: usize) -> Position {
        let mut start = 0;
        for _ in 0..occurrence {
            let idx = source[start..]
                .find(needle)
                .unwrap_or_else(|| panic!("needle {:?} not found", needle));
            start += idx;
            if start + 1 <= source.len() {
                start += 1;
            }
        }
        let byte = start - 1;
        let prefix = &source[..byte];
        let line = prefix.matches('\n').count() as u32;
        let col = prefix.rfind('\n').map_or(byte, |nl| byte - nl - 1) as u32;
        Position::new(line, col)
    }

    #[test]
    fn hover_shows_local_var_type() {
        let src = "void f() { int x = 5; x; }";
        // Second occurrence of `x` — cursor sits inside it.
        let pos = pos_of(src, "x", 2);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
        assert!(m.value.contains('x'), "missing x in {:?}", m.value);
    }

    #[test]
    fn hover_shows_local_param_type() {
        let src = "void f(int arg) { arg; }";
        let pos = pos_of(src, "arg", 2);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
    }

    #[test]
    fn hover_namespaced_symbol_bare_reference() {
        // Bare reference inside a namespace must find the `Ns::greet`
        // symbol so hover works without `Ns::` qualification.
        let src = "namespace Ns {\n    void greet() {}\n    void main() { greet(); }\n}";
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let analysis = tw.analysis();
        let pos = pos_of(src, "greet", 2);
        let h = hover(analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("greet"), "missing greet in {:?}", m.value);
    }

    #[test]
    fn hover_shows_workspace_function() {
        let src = "void greet() {}\nvoid main() { greet(); }";
        let tw = TestWorkspace::one_file("hover.as", src);
        let pos = pos_of(src, "greet", 2);
        let analysis = tw.analysis();
        let scope = tw.scope();
        let h = hover(analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("greet"), "missing greet in {:?}", m.value);
    }

    #[test]
    fn hover_workspace_void_call_shows_return_type_not_error() {
        // Cross-file call of a workspace `void` function: the checker
        // records the callee span with the resolved return type — `void`,
        // never `<error>` (user report: `(local) <error> ts_log` after
        // declaring the callee in another file).
        let dir = crate::testing::TempDir::new("ols-hover-wsfn");
        let a = dir.join("A.as");
        let b = dir.join("B.as");
        let src_a = "void caller() {\n    ts_log(\"x\");\n}";
        std::fs::write(&a, src_a).unwrap();
        std::fs::write(&b, "void ts_log(const string &in s) {}").unwrap();
        let snap = crate::analysis_snapshot::AnalysisSnapshot::from_files(
            &[
                (a, src_a.to_string()),
                (b, "void ts_log(const string &in s) {}".to_string()),
            ],
            &crate::config::LspConfig::default(),
        );
        let (uri, analysis) = snap
            .uri_map()
            .into_iter()
            .find(|(_, (u, _))| u.path().ends_with("A.as"))
            .map(|(_, (u, an))| (u, an))
            .expect("A.as in snapshot");
        let scope = GlobalScope::new(snap.symbols(), None);
        let checked = snap.checked_file(&uri, &scope).expect("checked view");
        let pos = pos_of(src_a, "ts_log", 1);
        let h = hover(analysis, Some(&checked), pos, &scope).expect("hover");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(
            !m.value.contains("<error>"),
            "hover showed <error>: {:?}",
            m.value
        );
        assert!(m.value.contains("ts_log"), "missing name: {:?}", m.value);
        // Positive pin: the recorded return type is `void`, not a
        // placeholder — CHANGELOG promises `(local) void ts_log`.
        assert!(m.value.contains("void"), "missing void: {:?}", m.value);
    }

    #[test]
    fn hover_shows_class_field() {
        let src = "class C { int field; void m() { field; } }";
        let pos = pos_of(src, "field", 2);
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, pos, &scope).expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(m.value.contains("field"), "missing field in {:?}", m.value);
        assert!(m.value.contains("int"), "missing int in {:?}", m.value);
    }

    #[test]
    fn hover_returns_none_outside_ident() {
        let src = "void f() {}";
        // Column 4: the space between `void` and `f`.
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let h = hover(&analysis, None, Position::new(0, 4), &scope);
        assert!(h.is_none());
    }

    #[test]
    fn hover_implicit_this_method_call() {
        // GH: hovering a bare implicit-this call of a class method must NOT
        // show "(local) <error>" — the callee resolves to the method with
        // its return type.
        let src = "class C { void CheckInit() {} void m() { CheckInit(); } }";
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let checked = tw
            .snapshot
            .checked_file(&tw.uri(), &scope)
            .expect("checked");
        let analysis = tw.analysis();
        let pos = pos_of(src, "CheckInit", 2);
        let h = hover(analysis, Some(&checked), pos, &scope).expect("hover");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(
            !m.value.contains("<error>"),
            "hover showed <error>: {:?}",
            m.value
        );
        assert!(m.value.contains("CheckInit"), "missing name: {:?}", m.value);
    }

    #[test]
    fn hover_uses_checked_view_types() {
        // GH #42: with a checked view, hover reads the checker's recorded
        // span→type instead of the scope_query approximation.
        let dir = crate::testing::TempDir::new("ols-hover-checked-test");
        let path = dir.join("e.as");
        let src = "void main() { int x = 1; int y = x; }";
        std::fs::write(&path, src).unwrap();
        let snap = crate::analysis_snapshot::AnalysisSnapshot::from_files(
            &[(path, src.to_string())],
            &crate::config::LspConfig::default(),
        );
        let uri = snap.uri_map()[&0].0.clone();
        let symbols = snap.symbols();
        let scope = GlobalScope::new(symbols, None);
        let checked = snap.checked_file(&uri, &scope).expect("checked view");

        // Cursor over the use of `x` (offset 33 → line 0, char 33).
        let analysis = snap.analysis_of(&uri).expect("analysis");
        let h = hover(analysis, Some(&checked), Position::new(0, 33), &scope)
            .expect("hover should return");
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        assert!(
            m.value.contains("(local) int x"),
            "expected recorded int type in {:?}",
            m.value
        );
    }

    // === B033: full signatures, overload sets, unified detail ===

    fn md_of(h: Hover) -> String {
        let HoverContents::Markup(m) = h.contents else {
            panic!("expected markdown hover")
        };
        m.value
    }

    /// Hover through the production path: checked view present, no typedb.
    fn hover_checked(src: &str, needle: &str, occurrence: usize) -> String {
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let checked = tw
            .snapshot
            .checked_file(&tw.uri(), &scope)
            .expect("checked view");
        let pos = pos_of(src, needle, occurrence);
        md_of(hover(tw.analysis(), Some(&checked), pos, &scope).expect("hover"))
    }

    /// Hover with no checked view — exercises the workspace formatter.
    fn hover_plain(src: &str, needle: &str, occurrence: usize) -> String {
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let pos = pos_of(src, needle, occurrence);
        md_of(hover(tw.analysis(), None, pos, &scope).expect("hover"))
    }

    /// Build a fake Core type database from a JSON literal.
    fn typedb(json: serde_json::Value) -> crate::typedb::TypeIndex {
        let mut idx = crate::typedb::TypeIndex::new();
        let db: crate::typedb::core_format::CoreDatabase =
            serde_json::from_value(json).expect("fake core db");
        idx.add_core(&db);
        idx
    }

    /// Hover against a typedb-backed scope (no workspace symbol of that name).
    fn hover_external_md(src: &str, needle: &str, occurrence: usize, idx: &TypeIndex) -> String {
        md_of(hover_external(src, needle, occurrence, idx).expect("hover"))
    }

    fn hover_external(
        src: &str,
        needle: &str,
        occurrence: usize,
        idx: &TypeIndex,
    ) -> Option<Hover> {
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = GlobalScope::new(tw.snapshot.symbols(), Some(idx));
        let pos = pos_of(src, needle, occurrence);
        hover(tw.analysis(), None, pos, &scope)
    }

    fn fixture_typedb() -> TypeIndex {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        TypeIndex::load(
            &fixtures.join("OpenplanetCore.json"),
            &fixtures.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
    }

    #[test]
    fn hover_rejects_nadeo_class_storage_key() {
        let idx = fixture_typedb();
        let src = "void main() { Game::CGameCtnApp@ app; }";
        assert!(
            hover_external(src, "CGameCtnApp", 1, &idx).is_none(),
            "checker-rejected storage spelling must not receive authoritative hover"
        );
    }

    #[test]
    fn hover_accepts_nadeo_class_source_spelling() {
        let idx = fixture_typedb();
        let src = "void main() { CGameCtnApp@ app; }";
        let md = md_of(
            hover_external(src, "CGameCtnApp", 1, &idx)
                .expect("source spelling must receive hover"),
        );
        assert!(md.contains("CGameCtnApp"), "{md:?}");
    }

    #[test]
    fn hover_rejects_nadeo_enum_storage_key() {
        let idx = fixture_typedb();
        let src = "void main() { Game::CGameMenu::EMenuOrder value; }";
        assert!(
            hover_external(src, "EMenuOrder", 1, &idx).is_none(),
            "checker-rejected enum storage spelling must not receive authoritative hover"
        );
    }

    #[test]
    fn hover_accepts_nadeo_enum_source_spelling() {
        let idx = fixture_typedb();
        let src = "void main() { CGameMenu::EMenuOrder value; }";
        let md = md_of(
            hover_external(src, "EMenuOrder", 1, &idx).expect("source spelling must receive hover"),
        );
        assert!(md.contains("EMenuOrder"), "{md:?}");
    }

    #[test]
    fn hover_retains_global_enum_ancestor_walk_spelling() {
        let idx = fixture_typedb();
        let src = "void main() { CGameCtnApp::VirtualKey value; }";
        let md = md_of(
            hover_external(src, "VirtualKey", 1, &idx)
                .expect("B052 ancestor-walk spelling must receive hover"),
        );
        assert!(md.contains("VirtualKey"), "{md:?}");
    }

    #[test]
    fn hover_accepts_bare_header_enum_spelling() {
        let idx = fixture_typedb();
        let src = "void main() { ESceneCharVisNotice value; }";
        let md = md_of(
            hover_external(src, "ESceneCharVisNotice", 1, &idx)
                .expect("bare header enum must receive hover"),
        );
        assert!(md.contains("ESceneCharVisNotice"), "{md:?}");
    }

    #[test]
    fn hover_rejects_qualified_header_enum_spelling() {
        let idx = fixture_typedb();
        let src = "void main() { Scene::ESceneCharVisNotice value; }";
        assert!(
            hover_external(src, "ESceneCharVisNotice", 1, &idx).is_none(),
            "header enum only exists under its bare source spelling"
        );
    }

    // --- B070: hover must answer for the spellings the checker accepts ----
    //
    // B052's ancestor walk makes a WORKSPACE-declared namespace a real scope,
    // so a plugin writing `namespace Foo {}` can spell a global engine enum
    // `Foo::VirtualKey`. Hover's `lookup_external` asks
    // `TypeIndex::source_reference_key`, whose scope test is
    // `TypeIndex::scope_exists` — the INDEX-ONLY half, which cannot know a
    // workspace declared anything. `GlobalScope::scope_exists` is the half
    // that does. That split is the defect: the checker accepted a spelling
    // hover was silent on.
    //
    // WHAT GROUNDS THESE TESTS, corrected after the b052 re-probe.
    //
    // The MECHANISM — a real scope's qualifier reaching a globally visible
    // enum through the ancestor walk — is witnessed by the Core-namespace rows
    // of `docs/probes/b052-qualified-enum-spellings.tsv`: `Audio::VirtualKey`,
    // `Crypto::VirtualKey`, `Dev::VirtualKey`, `Display::VirtualKey` and
    // `Fids::VirtualKey` are all ACCEPTED and all survived the re-probe. A
    // namespace declared in one's own compilation unit is that same mechanism.
    //
    // The `Foo::*` rows are NOT evidence for the workspace case and must not
    // be cited as such. The re-probe flipped all three to `Namespace 'Foo'
    // doesn't exist.` and attributed them to the plugin-namespace effect: a
    // plugin declaring `namespace Foo` was installed on the probing machine
    // when the corpus was measured and is not now. They witness "a namespace
    // some OTHER installed plugin declares appears as a scope", which is an
    // environment fact this crate deliberately does not implement — see
    // `PLUGIN_NAMESPACE_ARTEFACTS` in `resolver.rs`, which forces exactly
    // those spellings to not resolve in a bare context.
    //
    // The `Bogus::` row (26) is REJECTED and is a genuine control: nothing
    // declares it. Note the re-probe result AGREES with it — by not declaring
    // `Foo`, the re-probe turned `Foo` into `Bogus` and got the same message.
    //
    // Recorded because an earlier version of this comment made the wrong
    // claim: it argued these ACCEPTs survived B068 because REJECTED controls
    // were interleaved with them. That argument was correct, and the re-probe
    // confirmed B068 never fired here — but it defended the rows against
    // BATCHING while they were compromised by ENVIRONMENT. Defending a claim
    // against the failure mode you have in mind, and treating that as a
    // general defence, is the error; see `docs/verification-hazards.md`.

    /// The ticket's own repro: the checker accepts this and hover was silent.
    #[test]
    fn hover_answers_for_a_global_enum_through_a_workspace_namespace() {
        let idx = fixture_typedb();
        let src = "namespace Foo {}\nvoid main() { Foo::VirtualKey value; }";
        let md = md_of(
            hover_external(src, "VirtualKey", 1, &idx)
                .expect("B070: a workspace namespace is a real scope for the ancestor walk"),
        );
        assert!(md.contains("VirtualKey"), "{md:?}");
    }

    /// The control that keeps the fix honest: an UNDECLARED namespace is not a
    /// scope, and the game says so (`Namespace 'Bogus' doesn't exist.`). A fix
    /// that answered for any qualifier at all would pass the test above and
    /// fail this one.
    #[test]
    fn hover_is_silent_for_an_enum_behind_an_undeclared_namespace() {
        let idx = fixture_typedb();
        let src = "void main() { Bogus::ESceneCharVisNotice value; }";
        assert!(
            hover_external(src, "ESceneCharVisNotice", 1, &idx).is_none(),
            "an undeclared namespace is not a scope; hover must not invent one"
        );
    }

    /// The law the ticket states — "the two must not disagree about whether a
    /// spelling names something" — pinned over every measured row, in both
    /// directions. Asserting the checker against the GAME as well means this
    /// cannot pass by both surfaces being wrong in the same way.
    #[test]
    fn hover_answers_exactly_the_enum_spellings_the_checker_accepts() {
        let idx = fixture_typedb();
        // (declarations, spelling, hovered tail, game verdict)
        let rows = [
            ("namespace Foo {}", "Foo::VirtualKey", "VirtualKey", true),
            (
                "namespace Foo {}",
                "Foo::ESceneCharVisNotice",
                "ESceneCharVisNotice",
                true,
            ),
            (
                "namespace Foo { namespace Bar {} }",
                "Foo::Bar::VirtualKey",
                "VirtualKey",
                true,
            ),
            ("namespace Foo {}", "Foo::ZZZNoSuch", "ZZZNoSuch", false),
            (
                "namespace Foo { namespace Bar {} }",
                "Foo::Bar::ZZZNoSuch",
                "ZZZNoSuch",
                false,
            ),
            (
                "",
                "Bogus::ESceneCharVisNotice",
                "ESceneCharVisNotice",
                false,
            ),
        ];
        for (decls, spelling, needle, accepted) in rows {
            let src = format!("{decls}\nvoid main() {{ {spelling} value; }}\n");
            let tw = TestWorkspace::one_file("hover.as", &src);
            let scope = GlobalScope::new(tw.snapshot.symbols(), Some(&idx));
            assert_eq!(
                scope.has_enum(spelling),
                accepted,
                "checker disagrees with the GAME on {spelling:?}"
            );
            assert_eq!(
                hover_external(&src, needle, 1, &idx).is_some(),
                accepted,
                "hover disagrees with the checker on {spelling:?}"
            );
        }
    }

    /// The ticket's law, swept over the whole corpus: hover and the checker
    /// must not disagree about whether a spelling names something.
    ///
    /// The two directions are not equally severe, and the asymmetry is why
    /// this test exists. Hover answering where the checker REJECTS is a lie —
    /// it tells the user an invalid spelling names something real. Hover
    /// silent where the checker ACCEPTS is only a missing surface. This fix
    /// introduced the severe direction and this sweep is what caught it:
    /// `canonical_enum_key` is a CANONICALIZER, free to be wider than
    /// acceptance because every other caller reaches it only after the checker
    /// has accepted, and it still carries the trailing-suffix match `has_enum`
    /// deliberately dropped (B007). Called ungated on arbitrary hovered text
    /// it answered for 24 spellings the checker rejects, every one of the form
    /// `Plot::X` matching the real key `UI::Plot::X` — which the game refuses
    /// with `Namespace 'Plot' doesn't exist.`
    ///
    /// The `has_enum` gate now makes the severe direction impossible by
    /// construction rather than merely unobserved: the fallback answers only
    /// where `has_enum` holds, and `resolves` subsumes `has_enum`. This sweep
    /// holds that closed against future edits, and asserts the mild direction
    /// too, which currently has zero exceptions.
    ///
    /// It reads no game verdict — only the corpus's list of SPELLINGS — so
    /// B068's ACCEPTED-row hazard cannot reach it. Mutation-proven: deleting
    /// the `has_enum` gate takes it from 0 disagreements to 24.
    #[test]
    fn hover_and_the_checker_never_disagree_about_what_a_spelling_names() {
        let idx = fixture_typedb();
        let corpus = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs/probes/b052-qualified-enum-spellings.tsv");
        let text = std::fs::read_to_string(&corpus).expect("b052 corpus must be readable");
        // Declared so the workspace-namespace arm of the walk is exercised too.
        let decls = "namespace Foo { namespace Bar {} }";
        let mut checked = 0usize;
        let mut accepted = 0usize;
        let mut answered = 0usize;
        // The severe direction: hover answers, the checker rejects.
        let mut lies: Vec<String> = Vec::new();
        // The mild direction: the checker accepts, hover is silent.
        let mut gaps: Vec<String> = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let spelling = line.split('\t').next().unwrap_or("");
            if spelling.is_empty() || spelling == "spelling" || !spelling.contains("::") {
                continue;
            }
            if spelling.contains('<') || spelling.contains('@') {
                continue;
            }
            let needle = spelling.rsplit("::").next().unwrap_or(spelling);
            let src = format!("{decls}\nvoid main() {{ {spelling} value; }}\n");
            let tw = TestWorkspace::one_file("hover.as", &src);
            let scope = GlobalScope::new(tw.snapshot.symbols(), Some(&idx));
            let resolves = scope.resolves(spelling);
            let hovers = hover_external(&src, needle, 1, &idx).is_some();
            checked += 1;
            accepted += usize::from(resolves);
            answered += usize::from(hovers);
            match (hovers, resolves) {
                (true, false) => lies.push(spelling.to_string()),
                (false, true) => gaps.push(spelling.to_string()),
                _ => {}
            }
        }
        // Anti-vacuity: a sweep covering nothing, or one on which hover never
        // answered, or one with no rejected spellings, satisfies this
        // trivially. Each floor is well under the current value.
        assert!(
            checked > 500,
            "sweep must cover the corpus, only reached {checked}"
        );
        assert!(
            answered > 100,
            "hover answered for only {answered} of {checked}; the law would be vacuous"
        );
        assert!(
            checked - accepted > 100,
            "only {} of {checked} spellings are rejected; the severe direction would be untested",
            checked - accepted
        );
        assert!(
            lies.is_empty(),
            "hover answers for {} spellings the checker REJECTS: {:?}",
            lies.len(),
            &lies[..lies.len().min(10)]
        );
        assert!(
            gaps.is_empty(),
            "checker accepts {} spellings hover is silent on: {:?}",
            gaps.len(),
            &gaps[..gaps.len().min(10)]
        );
    }

    // --- defect 1: the checked-view fast path printed `name()` ---

    #[test]
    fn hover_method_shows_parameters() {
        let src = "class C {\n  void Foo(int a, float b) {}\n  void m() { Foo(1, 2.0); }\n}";
        let md = hover_checked(src, "Foo", 2);
        assert!(md.contains("int a"), "missing `int a` in {:?}", md);
        assert!(md.contains("float b"), "missing `float b` in {:?}", md);
    }

    #[test]
    fn hover_free_function_call_shows_parameters() {
        // The checked fast path used to shadow the workspace formatter and
        // render `(local) void log` with no parameter list at all.
        let src = "void log(const string &in msg, int level) {}\nvoid main() { log(\"x\", 1); }";
        let md = hover_checked(src, "log", 2);
        assert!(
            md.contains("const string &in msg"),
            "missing first param in {:?}",
            md
        );
        assert!(md.contains("int level"), "missing second param in {:?}", md);
    }

    #[test]
    fn hover_inherited_method_shows_parameters() {
        let src = "class B {\n  void Foo(int a) {}\n}\nclass D : B {\n  void m() { Foo(1); }\n}";
        let md = hover_checked(src, "Foo", 2);
        assert!(md.contains("int a"), "missing inherited param in {:?}", md);
    }

    #[test]
    fn hover_method_zero_params_renders_empty_parens() {
        let src = "class C {\n  void Foo() {}\n  void m() { Foo(); }\n}";
        let md = hover_checked(src, "Foo", 2);
        assert!(md.contains("Foo()"), "expected `Foo()` in {:?}", md);
        assert!(!md.contains(", )"), "stray comma in {:?}", md);
        assert!(!md.contains("(,"), "stray comma in {:?}", md);
    }

    // --- defect 3: workspace overloads ---

    #[test]
    fn hover_shows_all_overloads() {
        let src = "void f() {}\nvoid f(int a) {}\nvoid f(int a, int b) {}\nvoid main() { f(); }";
        let md = hover_plain(src, "f", 4);
        assert!(
            md.contains("void f()"),
            "missing 0-arg overload in {:?}",
            md
        );
        assert!(
            md.contains("void f(int a)"),
            "missing 1-arg overload in {:?}",
            md
        );
        assert!(
            md.contains("void f(int a, int b)"),
            "missing 2-arg overload in {:?}",
            md
        );
    }

    #[test]
    fn hover_two_overloads_are_both_listed() {
        // `g` rather than `f`: `float` contains an `f`, which would throw
        // the occurrence count in `pos_of` off.
        let src = "void g(int a) {}\nvoid g(float a) {}\nvoid main() { g(1); }";
        let md = hover_plain(src, "g", 3);
        assert!(md.contains("void g(int a)"), "{:?}", md);
        assert!(md.contains("void g(float a)"), "{:?}", md);
    }

    #[test]
    fn hover_five_overloads_are_all_listed() {
        let src = concat!(
            "void f() {}\n",
            "void f(int a) {}\n",
            "void f(int a, int b) {}\n",
            "void f(int a, int b, int c) {}\n",
            "void f(int a, int b, int c, int d) {}\n",
            "void main() { f(); }"
        );
        let md = hover_plain(src, "f", 6);
        for n in 0..5 {
            let params: Vec<String> = (0..n)
                .map(|i| format!("int {}", (b'a' + i as u8) as char))
                .collect();
            let sig = format!("void f({})", params.join(", "));
            assert!(md.contains(&sig), "missing {:?} in {:?}", sig, md);
        }
        assert!(md.contains("5 overloads"), "missing count in {:?}", md);
    }

    #[test]
    fn hover_single_overload_has_no_overload_header() {
        let src = "void f(int a) {}\nvoid main() { f(1); }";
        let md = hover_plain(src, "f", 2);
        assert!(
            !md.contains("overload"),
            "single overload gained list formatting: {:?}",
            md
        );
    }

    #[test]
    fn hover_overload_order_is_deterministic() {
        let src = concat!(
            "void f(int a, int b) {}\n",
            "void f() {}\n",
            "void f(int a) {}\n",
            "void main() { f(); }"
        );
        let first = hover_plain(src, "f", 4);
        for _ in 0..8 {
            assert_eq!(first, hover_plain(src, "f", 4), "hover output is unstable");
        }
        let zero = first.find("void f()").expect("0-arg");
        let one = first.find("void f(int a)").expect("1-arg");
        let two = first.find("void f(int a, int b)").expect("2-arg");
        assert!(zero < one && one < two, "not arity-ordered: {:?}", first);
    }

    // --- param modifier variants (workspace path) ---

    #[test]
    fn hover_workspace_param_modifiers_render() {
        let cases = [
            ("const string &in", "const string &in p"),
            ("int &out", "int &out p"),
            ("int &inout", "int &inout p"),
            ("C@", "C@ p"),
            ("C@ &in", "C@ &in p"),
            ("const C@", "const C@ p"),
            ("int", "int p"),
        ];
        for (decl, expected) in cases {
            let src = format!(
                "class C {{}}\nvoid f({} p) {{}}\nvoid main() {{ f(); }}",
                decl
            );
            let md = hover_plain(&src, "f", 2);
            assert!(
                md.contains(expected),
                "param {:?} rendered as {:?}",
                decl,
                md
            );
        }
    }

    #[test]
    fn hover_workspace_defaulted_param_is_marked_optional() {
        let src = "void f(int a, int b = 3) {}\nvoid main() { f(1); }";
        let md = hover_plain(src, "f", 2);
        assert!(md.contains("int a"), "{:?}", md);
        assert!(
            md.contains("int b = "),
            "defaulted param not marked optional in {:?}",
            md
        );
    }

    #[test]
    fn hover_workspace_zero_param_function_has_empty_parens() {
        let src = "void f() {}\nvoid main() { f(); }";
        let md = hover_plain(src, "f", 2);
        assert!(md.contains("void f()"), "{:?}", md);
        assert!(!md.contains("f( )"), "{:?}", md);
        assert!(!md.contains("f(,"), "{:?}", md);
    }

    #[test]
    fn hover_workspace_unnamed_param_renders_placeholder() {
        let src = "void f(int) {}\nvoid main() { f(1); }";
        let md = hover_plain(src, "f", 2);
        assert!(md.contains("int _"), "unnamed param not shown in {:?}", md);
    }

    #[test]
    fn hover_namespaced_function_shows_signature() {
        let src = "namespace Ns {\n  void greet(int a) {}\n  void main() { greet(1); }\n}";
        let md = hover_plain(src, "greet", 2);
        assert!(md.contains("int a"), "{:?}", md);
        assert!(
            md.contains("Ns::greet"),
            "not namespace-qualified: {:?}",
            md
        );
    }

    #[test]
    fn hover_funcdef_shows_signature() {
        let src = "funcdef void Cb(int a, const string &in s);\nvoid f(Cb@ cb) {}";
        let md = hover_plain(src, "Cb", 2);
        assert!(md.contains("funcdef"), "{:?}", md);
        assert!(md.contains("int a"), "{:?}", md);
        assert!(md.contains("const string &in s"), "{:?}", md);
    }

    /// Workspace symbols render their doc the same way external ones do.
    ///
    /// Driven through the formatter rather than through source: the symbol
    /// extractor is handed the MASKED source (comments blanked), so no
    /// workspace symbol carries a doc today — that gap is a separate change,
    /// but the formatter must be ready for it and must not diverge from the
    /// external path (B033).
    #[test]
    fn workspace_symbol_doc_renders_below_the_signature() {
        let sym = Symbol {
            name: "greet".to_string(),
            kind: SymbolKind::Function {
                return_type: "void".to_string(),
                params: vec![("a".to_string(), "int".to_string())],
                min_args: 1,
                imported: false,
            },
            span: crate::lexer::Span::new(0, 1),
            file_id: 0,
            doc: Some("Greets the user.".to_string()),
        };
        let md = format_workspace_symbols(&[&sym]).expect("formatted");
        assert!(md.contains("void greet(int a)"), "{:?}", md);
        assert!(md.contains("Greets the user."), "{:?}", md);
    }

    /// An `import ... from` alias that duplicates a real definition must not
    /// inflate the overload count.
    ///
    /// The review pass proposed dropping every alias whenever a definition
    /// exists. Rejected: in AngelScript an imported function with a DIFFERENT
    /// signature is a genuine additional overload, so that filter would hide
    /// real ones. The case that actually matters — an alias repeating the
    /// definition it stands for — collapses through the signature dedupe
    /// `overload_block` already applies, which is what this pins.
    #[test]
    fn workspace_import_alias_does_not_duplicate_its_definition() {
        let sig = |imported: bool| Symbol {
            name: "Foo".to_string(),
            kind: SymbolKind::Function {
                return_type: "void".to_string(),
                params: vec![("a".to_string(), "int".to_string())],
                min_args: 1,
                imported,
            },
            span: crate::lexer::Span::new(0, 1),
            file_id: 0,
            doc: None,
        };
        let (def, alias) = (sig(false), sig(true));
        let md = format_workspace_symbols(&[&def, &alias]).expect("formatted");
        assert!(md.contains("void Foo(int a)"), "{:?}", md);
        assert!(
            !md.contains("overload"),
            "an alias of the same signature was counted twice: {:?}",
            md
        );

        // A genuinely different imported signature IS a second overload.
        let other = Symbol {
            kind: SymbolKind::Function {
                return_type: "void".to_string(),
                params: vec![("b".to_string(), "float".to_string())],
                min_args: 1,
                imported: true,
            },
            ..sig(true)
        };
        let md = format_workspace_symbols(&[&def, &other]).expect("formatted");
        assert!(md.contains("2 overloads"), "{:?}", md);
        assert!(md.contains("float b"), "{:?}", md);
    }

    /// Non-callable workspace symbols render their doc too.
    #[test]
    fn workspace_variable_doc_renders_below_the_declaration() {
        let sym = Symbol {
            name: "g_count".to_string(),
            kind: SymbolKind::Variable {
                type_name: "int".to_string(),
            },
            span: crate::lexer::Span::new(0, 1),
            file_id: 0,
            doc: Some("How many.".to_string()),
        };
        let md = format_workspace_symbols(&[&sym]).expect("formatted");
        assert!(md.contains("int g_count"), "{:?}", md);
        assert!(md.contains("How many."), "{:?}", md);
    }

    // --- defect 2: external (typedb) overloads ---

    fn overloaded_core_db() -> crate::typedb::TypeIndex {
        typedb(serde_json::json!({
            "op": "1.0",
            "functions": [
                {"ns": "UI", "name": "Button", "returntypedecl": "bool",
                 "decl": "bool Button(const string&in label)",
                 "args": [{"name": "label", "typedecl": "string", "typeflags": 5}],
                 "desc": "Draws a button."},
                {"ns": "UI", "name": "Button", "returntypedecl": "bool",
                 "decl": "bool Button(const string&in label, const vec2&in size)",
                 "args": [{"name": "label", "typedecl": "string", "typeflags": 5},
                          {"name": "size", "typedecl": "vec2", "typeflags": 5}]},
                {"ns": "UI", "name": "Button", "returntypedecl": "bool",
                 "decl": "bool Button(const string&in label, const vec2&in size, int flags)",
                 "args": [{"name": "label", "typedecl": "string", "typeflags": 5},
                          {"name": "size", "typedecl": "vec2", "typeflags": 5},
                          {"name": "flags", "typedecl": "int", "typeflags": 0}]},
                {"ns": "UI", "name": "Text", "returntypedecl": "void",
                 "decl": "void Text(const string&in text)",
                 "args": [{"name": "text", "typedecl": "string", "typeflags": 5}]}
            ]
        }))
    }

    #[test]
    fn hover_external_overloads_listed() {
        let idx = overloaded_core_db();
        let src = "void main() { UI::Button(\"a\"); }";
        let md = hover_external_md(src, "Button", 1, &idx);
        assert!(md.contains("3 overloads"), "missing count in {:?}", md);
        assert!(md.contains("const string&in label)"), "{:?}", md);
        assert!(md.contains("const vec2&in size)"), "{:?}", md);
        assert!(md.contains("int flags)"), "{:?}", md);
    }

    #[test]
    fn hover_external_single_overload_has_no_header() {
        let idx = overloaded_core_db();
        let src = "void main() { UI::Text(\"a\"); }";
        let md = hover_external_md(src, "Text", 1, &idx);
        assert!(!md.contains("overload"), "{:?}", md);
        assert!(md.contains("UI::Text"), "{:?}", md);
    }

    #[test]
    fn hover_external_overload_order_is_deterministic() {
        let idx = overloaded_core_db();
        let src = "void main() { UI::Button(\"a\"); }";
        let first = hover_external_md(src, "Button", 1, &idx);
        for _ in 0..8 {
            assert_eq!(first, hover_external_md(src, "Button", 1, &idx));
        }
    }

    #[test]
    fn hover_external_function_shows_doc() {
        let idx = overloaded_core_db();
        let src = "void main() { UI::Button(\"a\"); }";
        let md = hover_external_md(src, "Button", 1, &idx);
        assert!(md.contains("Draws a button."), "{:?}", md);
    }

    #[test]
    fn hover_external_param_modes_and_defaults_render() {
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "functions": [
                {"ns": "T", "name": "All", "returntypedecl": "void",
                 "args": [
                    {"name": "a", "typedecl": "string", "typeflags": 1},
                    {"name": "b", "typedecl": "int", "typeflags": 2},
                    {"name": "c", "typedecl": "int", "typeflags": 3},
                    {"name": "d", "typedecl": "int", "typeflags": 0, "default": "3"},
                    {"name": "", "typedecl": "CNod@", "typeflags": 0}
                 ]}
            ]
        }));
        let src = "void main() { T::All(); }";
        let md = hover_external_md(src, "All", 1, &idx);
        assert!(md.contains("string &in a"), "&in not rendered: {:?}", md);
        assert!(md.contains("int &out b"), "&out not rendered: {:?}", md);
        assert!(md.contains("int &inout c"), "&inout not rendered: {:?}", md);
        assert!(md.contains("int d = 3"), "default not rendered: {:?}", md);
        assert!(md.contains("CNod@ _"), "unnamed param not shown: {:?}", md);
    }

    #[test]
    fn hover_external_zero_param_function_has_empty_parens() {
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "functions": [
                {"ns": "T", "name": "Now", "returntypedecl": "uint64", "args": []}
            ]
        }));
        let src = "void main() { T::Now(); }";
        let md = hover_external_md(src, "Now", 1, &idx);
        assert!(md.contains("uint64 T::Now()"), "{:?}", md);
        assert!(!md.contains("Now( )"), "{:?}", md);
    }

    // --- silent truncation of external type members ---

    #[test]
    fn hover_external_type_marks_truncated_members() {
        let props: Vec<serde_json::Value> = (0..40)
            .map(|i| serde_json::json!({"name": format!("p{}", i), "typedecl": "int"}))
            .collect();
        let methods: Vec<serde_json::Value> = (0..40)
            .map(|i| {
                serde_json::json!({"name": format!("m{}", i), "returntypedecl": "void", "args": []})
            })
            .collect();
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "classes": [{"name": "Big", "props": props, "methods": methods}]
        }));
        let src = "void main() { Big b; }";
        let md = hover_external_md(src, "Big", 1, &idx);
        assert!(
            md.contains("more properties"),
            "no property truncation marker: {:?}",
            md
        );
        assert!(
            md.contains("more methods"),
            "no method truncation marker: {:?}",
            md
        );
    }

    #[test]
    fn hover_external_type_short_member_list_has_no_marker() {
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "classes": [{"name": "Small",
                         "props": [{"name": "x", "typedecl": "int"}],
                         "methods": [{"name": "Go", "returntypedecl": "void", "args": []}]}]
        }));
        let src = "void main() { Small s; }";
        let md = hover_external_md(src, "Small", 1, &idx);
        assert!(!md.contains("more properties"), "{:?}", md);
        assert!(!md.contains("more methods"), "{:?}", md);
    }

    #[test]
    fn hover_external_type_methods_show_parameters() {
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "classes": [{"name": "Small", "props": [],
                         "methods": [{"name": "Go", "returntypedecl": "void",
                                      "decl": "void Go(const string&in what, int n)",
                                      "args": [{"name": "what", "typedecl": "string", "typeflags": 5},
                                               {"name": "n", "typedecl": "int", "typeflags": 0}]}]}]
        }));
        let src = "void main() { Small s; }";
        let md = hover_external_md(src, "Small", 1, &idx);
        assert!(md.contains("what"), "method params missing: {:?}", md);
        assert!(md.contains("int n"), "method params missing: {:?}", md);
    }

    // --- `receiver.member` hover ---

    /// Hover with both a checked view and a type database.
    fn hover_checked_with(src: &str, needle: &str, occurrence: usize, idx: &TypeIndex) -> String {
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = GlobalScope::new(tw.snapshot.symbols(), Some(idx));
        let checked = tw
            .snapshot
            .checked_file(&tw.uri(), &scope)
            .expect("checked view");
        let pos = pos_of(src, needle, occurrence);
        md_of(hover(tw.analysis(), Some(&checked), pos, &scope).expect("hover"))
    }

    #[test]
    fn hover_member_method_shows_parameters() {
        let src =
            "class C {\n  void Foo(int a, float b) {}\n}\nvoid main() { C c; c.Foo(1, 2.0); }";
        let md = hover_checked(src, "Foo", 2);
        assert!(md.contains("int a"), "{:?}", md);
        assert!(md.contains("float b"), "{:?}", md);
    }

    #[test]
    fn hover_member_method_lists_overloads() {
        let src = concat!(
            "class C {\n",
            "  void Foo() {}\n",
            "  void Foo(int a) {}\n",
            "}\n",
            "void main() { C c; c.Foo(); }"
        );
        let md = hover_checked(src, "Foo", 3);
        assert!(md.contains("2 overloads"), "{:?}", md);
        assert!(md.contains("C::Foo()"), "{:?}", md);
        assert!(md.contains("C::Foo(int a)"), "{:?}", md);
    }

    #[test]
    fn hover_this_member_method_shows_parameters() {
        let src = "class C {\n  void Foo(int a) {}\n  void m() { this.Foo(1); }\n}";
        let md = hover_checked(src, "Foo", 2);
        assert!(md.contains("int a"), "{:?}", md);
    }

    #[test]
    fn hover_member_inherited_method_shows_parameters() {
        let src = concat!(
            "class B {\n  void Foo(int a) {}\n}\n",
            "class D : B {}\n",
            "void main() { D d; d.Foo(1); }"
        );
        let md = hover_checked(src, "Foo", 2);
        assert!(md.contains("int a"), "inherited member method: {:?}", md);
    }

    fn widget_db() -> crate::typedb::TypeIndex {
        typedb(serde_json::json!({
            "op": "1.0",
            "classes": [{
                "name": "Widget",
                "props": [{"name": "Size", "typedecl": "vec2", "desc": "Its size."}],
                "methods": [
                    {"name": "Draw", "returntypedecl": "void",
                     "decl": "void Draw(const string&in label)",
                     "args": [{"name": "label", "typedecl": "string", "typeflags": 5}]},
                    {"name": "Draw", "returntypedecl": "void",
                     "decl": "void Draw(const string&in label, int flags)",
                     "args": [{"name": "label", "typedecl": "string", "typeflags": 5},
                              {"name": "flags", "typedecl": "int", "typeflags": 0}]}
                ]
            }]
        }))
    }

    #[test]
    fn hover_external_member_method_lists_overloads() {
        let idx = widget_db();
        let src = "void main() { Widget@ w; w.Draw(\"a\"); }";
        let md = hover_checked_with(src, "Draw", 1, &idx);
        assert!(md.contains("2 overloads"), "{:?}", md);
        assert!(
            md.contains("Widget::Draw(const string&in label)"),
            "{:?}",
            md
        );
        assert!(
            md.contains("Widget::Draw(const string&in label, int flags)"),
            "{:?}",
            md
        );
    }

    #[test]
    fn hover_external_member_property_shows_type_and_doc() {
        let idx = widget_db();
        let src = "void main() { Widget@ w; w.Size; }";
        let md = hover_checked_with(src, "Size", 1, &idx);
        assert!(md.contains("vec2 Widget::Size"), "{:?}", md);
        assert!(md.contains("Its size."), "{:?}", md);
    }

    #[test]
    fn hover_unresolved_member_keeps_the_recorded_type() {
        // No receiver type to resolve against: the hover must not wander off
        // and match an unrelated `SomeClass::Name` by tail.
        let src = "class Other {\n  void Name(int a) {}\n}\nvoid main() { int x = 1; x.Name; }";
        let tw = TestWorkspace::one_file("hover.as", src);
        let scope = tw.scope();
        let checked = tw
            .snapshot
            .checked_file(&tw.uri(), &scope)
            .expect("checked view");
        let pos = pos_of(src, "Name", 2);
        let md = hover(tw.analysis(), Some(&checked), pos, &scope).map(md_of);
        if let Some(md) = md {
            assert!(
                !md.contains("Other::Name"),
                "member hover leaked an unrelated symbol: {:?}",
                md
            );
        }
    }

    #[test]
    fn hover_member_method_through_a_call_receiver() {
        // `c.Child().Run(1)`: the receiver ends in `)`, so the token walk has
        // to step back over the whole call to find the recorded span.
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "class C {\n  Inner@ Child() { return null; }\n}\n",
            "void main() { C c; c.Child().Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("int a"), "call receiver: {:?}", md);
    }

    #[test]
    fn hover_member_method_through_an_index_receiver() {
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "void main() { array<Inner@> items; items[0].Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("int a"), "index receiver: {:?}", md);
    }

    #[test]
    fn hover_member_method_through_a_parenthesised_receiver() {
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "void main() { Inner@ i; (i).Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("int a"), "parenthesised receiver: {:?}", md);
    }

    #[test]
    fn hover_member_method_binds_to_the_receiver_not_a_same_named_method() {
        // Two unrelated classes declare `Run`; the receiver decides which
        // signature hover shows.
        let src = concat!(
            "class Decoy {\n  void Run(bool wrong) {}\n}\n",
            "class Inner {\n  void Run(int a) {}\n}\n",
            "class C {\n  Inner@ Child() { return null; }\n}\n",
            "void main() { C c; c.Child().Run(1); }"
        );
        let md = hover_checked(src, "Run", 3);
        assert!(md.contains("Inner::Run(int a)"), "{:?}", md);
        assert!(!md.contains("bool wrong"), "picked the decoy: {:?}", md);
    }

    #[test]
    fn hover_member_method_through_nested_parenthesised_receivers() {
        // `((x))` and deeper: the parser records only the innermost
        // expression's span, so each redundant group has to be peeled.
        for receiver in ["(x)", "((x))", "(((x)))"] {
            let src = format!(
                "class Inner {{\n  void Run(int a) {{}}\n}}\nvoid main() {{ Inner@ x; {}.Run(1); }}",
                receiver
            );
            let md = hover_checked(&src, "Run", 2);
            assert!(
                md.contains("Inner::Run(int a)"),
                "receiver {:?}: {:?}",
                receiver,
                md
            );
        }
    }

    #[test]
    fn hover_member_method_through_a_nested_group_around_a_call() {
        // Peeling must stop at a postfix form inside the groups.
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "Inner@ g(int q) { return null; }\n",
            "void main() { ((g(1))).Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("Inner::Run(int a)"), "{:?}", md);
    }

    #[test]
    fn hover_member_walk_does_not_panic_on_unbalanced_source() {
        // Truncated/garbage input must not index out of bounds or underflow.
        // A checked view is required: the member branch returns at `checked?`
        // before the bracket walk runs without one.
        for src in [
            ").Run(1);",
            "]  .Run(1);",
            "void f() { ).Run(1); }",
            "void f() { a[.Run(1); }",
            "void f() { (.Run(1); }",
            "void f() { ().Run(1); }",
            "void f() { (((.Run(1); }",
            "void f() { )))).Run(1); }",
            "void f() { a[[[.Run(1); }",
        ] {
            let tw = TestWorkspace::one_file("hover.as", src);
            let scope = tw.scope();
            let checked = tw
                .snapshot
                .checked_file(&tw.uri(), &scope)
                .expect("checked view");
            let pos = pos_of(src, "Run", 1);
            let _ = hover(tw.analysis(), Some(&checked), pos, &scope);
        }
    }

    #[test]
    fn hover_member_method_through_a_compound_parenthesised_receiver() {
        // A group holding a whole expression, not a single name: the parser
        // gives such a group the span of its CONTENTS, so hover has to query
        // the inner token range rather than the closing paren.
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "void main() { Inner@ x; Inner@ y; bool q; (q ? x : y).Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("Inner::Run(int a)"), "{:?}", md);
    }

    #[test]
    fn hover_member_method_through_a_cast_receiver() {
        // `cast<T>(x)` is a postfix form, NOT a group: unwrapping its parens
        // would report the type of `x` (the base) instead of `T`.
        let src = concat!(
            "class Base {}\n",
            "class Inner : Base {\n  void Run(int a) {}\n}\n",
            "void main() { Base@ x; cast<Inner>(x).Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("Inner::Run(int a)"), "cast receiver: {:?}", md);
    }

    #[test]
    fn hover_member_method_through_a_grouped_index_receiver() {
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "void main() { array<Inner@> items; (items)[0].Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("Inner::Run(int a)"), "{:?}", md);
    }

    #[test]
    fn hover_member_method_through_a_nested_call_receiver() {
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "Inner@ g(int q) { return null; }\n",
            "Inner@ f(Inner@ z) { return null; }\n",
            "void main() { f(g(1)).Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("Inner::Run(int a)"), "{:?}", md);
    }

    #[test]
    fn hover_member_method_through_a_field_chain_receiver() {
        let src = concat!(
            "class Inner {\n  void Run(int a) {}\n}\n",
            "class C { Inner@ ch; }\n",
            "void main() { C c; c.ch.Run(1); }"
        );
        let md = hover_checked(src, "Run", 2);
        assert!(md.contains("Inner::Run(int a)"), "{:?}", md);
    }

    // --- external funcdefs ---

    #[test]
    fn hover_external_funcdef_shows_signature() {
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "funcdefs": [{"ns": "UI", "name": "InputTextCallback",
                          "returntypedecl": "int",
                          "decl": "int InputTextCallback(UI::InputTextCallbackData@ data)",
                          "args": [{"name": "data", "typedecl": "UI::InputTextCallbackData@",
                                    "typeflags": 0}],
                          "desc": "Called while text is edited."}]
        }));
        let src = "void main() { UI::InputTextCallback@ cb; }";
        let md = hover_external_md(src, "InputTextCallback", 1, &idx);
        assert!(md.contains("funcdef"), "{:?}", md);
        assert!(md.contains("UI::InputTextCallback("), "{:?}", md);
        assert!(md.contains("data"), "params missing: {:?}", md);
        assert!(md.contains("Called while text is edited."), "{:?}", md);
    }

    // --- typedb signatures with no `decl` text ---

    #[test]
    fn hover_external_no_decl_still_renders_const() {
        // The Nadeo half stores no `decl`, and a Core entry may omit it:
        // the reconstruction path must still show `const`.
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "functions": [
                {"ns": "T", "name": "Take", "returntypedecl": "void",
                 "args": [
                    {"name": "a", "typedecl": "string", "typeflags": 5},
                    {"name": "b", "typedecl": "C@", "typeflags": 4},
                    {"name": "c", "typedecl": "int", "typeflags": 0}
                 ]}
            ]
        }));
        let src = "void main() { T::Take(); }";
        let md = hover_external_md(src, "Take", 1, &idx);
        assert!(md.contains("const string &in a"), "{:?}", md);
        assert!(md.contains("const C@ b"), "{:?}", md);
        assert!(md.contains("int c"), "{:?}", md);
        assert!(!md.contains("const const"), "doubled const: {:?}", md);
    }

    #[test]
    fn hover_external_no_decl_does_not_double_an_existing_const() {
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "functions": [
                {"ns": "T", "name": "Take", "returntypedecl": "void",
                 "args": [{"name": "a", "typedecl": "const string&in", "typeflags": 5}]}
            ]
        }));
        let src = "void main() { T::Take(); }";
        let md = hover_external_md(src, "Take", 1, &idx);
        assert!(md.contains("const string&in a"), "{:?}", md);
        assert!(!md.contains("const const"), "{:?}", md);
    }

    // --- enum consistency across the two paths ---

    #[test]
    fn hover_workspace_enum_shows_values() {
        let src = "enum E { A = 1, B = 2 }\nvoid main() { E e; }";
        let md = hover_plain(src, "E", 2);
        assert!(
            md.contains("A = 1"),
            "workspace enum has no values: {:?}",
            md
        );
        assert!(
            md.contains("B = 2"),
            "workspace enum has no values: {:?}",
            md
        );
    }

    #[test]
    fn hover_external_enum_alias_order_is_deterministic() {
        // Two members sharing a value came out of a HashMap, so sorting by
        // value alone left their order to chance.
        let src = "void main() { Marker m; }";
        let json = serde_json::json!({
            "op": "1.0",
            "enums": [{"name": "Marker", "values": {
                "Auto": {"v": -1}, "None": {"v": -1}, "Circle": {"v": 0}
            }}]
        });
        let first = hover_external_md(src, "Marker", 1, &typedb(json.clone()));
        for _ in 0..8 {
            assert_eq!(
                first,
                hover_external_md(src, "Marker", 1, &typedb(json.clone()))
            );
        }
        let auto = first.find("Auto = -1").expect("Auto");
        let none = first.find("None = -1").expect("None");
        let circle = first.find("Circle = 0").expect("Circle");
        assert!(
            auto < none && none < circle,
            "not (value, name) ordered: {:?}",
            first
        );
    }

    #[test]
    fn hover_enum_values_render_the_same_on_both_paths() {
        let ws = hover_plain("enum E { A = 1, B = 2 }\nvoid main() { E e; }", "E", 2);
        let idx = typedb(serde_json::json!({
            "op": "1.0",
            "enums": [{"name": "E", "values": {"A": {"v": 1}, "B": {"v": 2}}}]
        }));
        let ext = hover_external_md("void main() { E e; }", "E", 1, &idx);
        assert_eq!(ws, ext, "enum hover diverges between paths");
    }

    // === B054: interface members on the pool ==============================
    //
    // Hover's `receiver.member` step has worked since B033, but it reads
    // the symbol pool — and the pool held no `I::method` symbol at all, so
    // an interface-typed receiver fell through to the `(local) <type>
    // <name>` last resort and reported `(local) <error> Go`.

    #[test]
    fn hover_interface_method_on_an_interface_typed_receiver() {
        let md = hover_checked(
            "interface I { void Go(); }\nvoid f(I@ i) { i.Go(); }\n",
            "Go",
            2,
        );
        assert!(
            md.contains("(method) void I::Go()"),
            "expected the interface method signature, got {md:?}"
        );
    }

    #[test]
    fn hover_interface_method_renders_its_parameters() {
        let md = hover_checked(
            "interface I { int Count(const string &in key, bool deep = false); }\nvoid f(I@ i) { i.Count(\"a\"); }\n",
            "Count",
            2,
        );
        assert!(
            md.contains("I::Count") && md.contains("const string &in key"),
            "expected the full interface signature, got {md:?}"
        );
    }

    #[test]
    fn hover_interface_method_inherited_from_a_base_interface() {
        let md = hover_checked(
            "interface A { void Go(); }\ninterface B : A {}\nvoid f(B@ b) { b.Go(); }\n",
            "Go",
            2,
        );
        assert!(
            md.contains("(method) void A::Go()"),
            "expected the base interface's method, got {md:?}"
        );
    }

    #[test]
    fn hover_class_implementation_wins_over_the_interface_it_satisfies() {
        // The receiver is the CLASS, so the implementation is the answer —
        // the interface declaration is reachable through an `I@` receiver.
        // And it is the ONLY answer: `C::Go()` OVERRIDES `I::Go()`, so
        // rendering both as "2 overloads" would claim the class offers a
        // choice of signatures it does not have. The inheritance walk
        // crosses into `I` (a class lists its interfaces among its bases),
        // so the shadowing has to be explicit.
        let md = hover_checked(
            "interface I { void Go(); }\nclass C : I { void Go() {} }\nvoid f(C@ c) { c.Go(); }\n",
            "Go",
            3,
        );
        assert!(
            md.contains("C::Go"),
            "expected the class implementation, got {md:?}"
        );
        assert!(
            !md.contains("I::Go") && !md.contains("overloads"),
            "an override must shadow the declaration it satisfies, got {md:?}"
        );
    }

    /// The same rule for a plain class override, which had the same shape
    /// before B054 ever touched interfaces.
    #[test]
    fn hover_a_class_override_shadows_the_parent_declaration() {
        let md = hover_checked(
            "class A { void Run() {} }\nclass B : A { void Run() {} }\nvoid f(B@ b) { b.Run(); }\n",
            "Run",
            3,
        );
        assert!(md.contains("B::Run"), "expected B::Run, got {md:?}");
        assert!(
            !md.contains("A::Run") && !md.contains("overloads"),
            "an override must shadow the parent's declaration, got {md:?}"
        );
    }

    /// Shadowing is by SIGNATURE, not by name: a parent overload the child
    /// does not override is still a real choice and must still be listed.
    #[test]
    fn hover_a_parent_overload_with_a_different_signature_survives() {
        let md = hover_checked(
            "class A { void Run(int a) {} }\nclass B : A { void Run() {} }\nvoid f(B@ b) { b.Run(); }\n",
            "Run",
            3,
        );
        assert!(md.contains("B::Run"), "expected B::Run, got {md:?}");
        assert!(
            md.contains("A::Run") && md.contains("2 overloads"),
            "a non-overriding parent overload must still be offered, got {md:?}"
        );
    }

    #[test]
    fn hover_namespaced_interface_method() {
        let md = hover_checked(
            "namespace Ns { interface I { void Go(); } }\nvoid f(Ns::I@ i) { i.Go(); }\n",
            "Go",
            2,
        );
        assert!(
            md.contains("Ns::I::Go"),
            "expected the namespaced interface method, got {md:?}"
        );
    }
}
