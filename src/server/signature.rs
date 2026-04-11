//! Signature help provider.
//!
//! Given a cursor position inside an unfinished call expression, returns a
//! `SignatureHelp` with one `SignatureInformation` per overload candidate.
//!
//! The flow is:
//!
//! 1. Walk the raw source backwards from the cursor at the byte level, skipping
//!    string and char literals plus line/block comments, counting matched
//!    parens until we find an unmatched `(`. That `(` introduces the enclosing
//!    call. Everything to the right of it up to the cursor is the arg list
//!    so far — we count top-level commas to derive `active_parameter`.
//! 2. Extract the callee expression by walking backwards from just before the
//!    unmatched `(`, collecting an identifier chain (including `.` and `::`
//!    segments). Trailing whitespace is skipped.
//! 3. Resolve the callee against:
//!      - `GlobalScope::lookup_function_overloads` for bare / qualified free
//!        functions (workspace),
//!      - `TypeIndex::lookup_function` for external Openplanet / Nadeo free
//!        functions,
//!      - workspace class methods via `workspace_class_member` (with a local
//!        receiver lookup via `scope_query::local_type_at`) for `recv.m(...)`,
//!      - `TypeIndex::lookup_type` → `methods` for external receivers.
//! 4. Convert each resolved signature to `SignatureInformation`, pick the
//!    active signature by exact-arity-match-then-shortest, return.

use tower_lsp::lsp_types::*;

use crate::lexer;
use crate::parser::Parser;
use crate::parser::ast::SourceFile;
use crate::server::diagnostics::position_to_offset;
use crate::server::scope_query;
use crate::symbols::SymbolTable;
use crate::symbols::scope::SymbolKind;
use crate::typecheck::global_scope::{GlobalScope, OverloadSig};
use crate::typedb::TypeIndex;

/// One enclosing call located at the cursor.
#[derive(Debug, Clone)]
struct CallSite {
    callee_text: String,
    #[allow(dead_code)]
    open_paren_byte: usize,
    active_param: u32,
}

/// A resolved callable — either sourced from the workspace or from the
/// external TypeIndex. Kept as a minimal common shape so the downstream
/// `active_signature` picker and `SignatureInformation` builder don't need
/// to care where the candidate came from.
struct ResolvedSignature {
    label_name: String,
    return_type: String,
    /// `(type, name)` pairs. Name may be empty.
    params: Vec<(String, String)>,
    doc: Option<String>,
    /// The minimum required arg count (for resolving `active_signature`).
    #[allow(dead_code)]
    min_args: usize,
}

pub fn signature_help(
    source: &str,
    position: Position,
    type_index: Option<&TypeIndex>,
    workspace: Option<&SymbolTable>,
) -> Option<SignatureHelp> {
    let cursor = position_to_offset(source, position);
    let call = find_enclosing_call(source, cursor)?;

    // Parse once for receiver-type lookup on member calls.
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file: SourceFile = parser.parse_file();

    let scope = workspace.map(|ws| GlobalScope::new(ws, type_index));

    let sigs = resolve_callee(
        &call.callee_text,
        source,
        &file,
        cursor as u32,
        scope.as_ref(),
        type_index,
    );
    if sigs.is_empty() {
        return None;
    }

    let active_signature = pick_active_signature(&sigs, call.active_param);
    let signatures: Vec<SignatureInformation> =
        sigs.iter().map(to_signature_information).collect();

    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature as u32),
        active_parameter: Some(call.active_param),
    })
}

// ---------------------------------------------------------------------------
// Part 1: locate the enclosing call at the cursor.
// ---------------------------------------------------------------------------

/// Walk `source` backwards from `cursor_byte` looking for the innermost
/// unmatched `(`. Returns `None` if the cursor is not inside any call. Strings
/// and comments are skipped; nested parens inside arg expressions don't count.
fn find_enclosing_call(source: &str, cursor_byte: usize) -> Option<CallSite> {
    let prefix = &source[..cursor_byte.min(source.len())];
    let bytes = prefix.as_bytes();

    // First pass (forward): mark every byte in the prefix that lives inside
    // a string/char literal or a comment. These bytes are invisible when we
    // walk backwards counting parens. This is simpler and more robust than
    // trying to detect strings/comments while walking backwards.
    let skip = compute_skip_mask(bytes);

    // Walk backwards counting parens/brackets/braces. When paren_depth goes
    // below zero we've found our unmatched `(`.
    let mut paren_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut brace_depth: i32 = 0;
    let mut open_paren_byte = None;
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        if skip[i] {
            continue;
        }
        match bytes[i] {
            b')' => paren_depth += 1,
            b'(' => {
                if paren_depth == 0 {
                    open_paren_byte = Some(i);
                    break;
                }
                paren_depth -= 1;
            }
            b']' => bracket_depth += 1,
            b'[' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                }
                // Unmatched `[` at depth 0 would mean we're inside something
                // like an array literal — keep walking.
            }
            b'}' => brace_depth += 1,
            b'{' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                } else {
                    // Unmatched `{` — we hit a block boundary. There is no
                    // enclosing call.
                    return None;
                }
            }
            b';' => {
                // Statement terminators at depth 0 also mean no enclosing
                // call (cursor is past the end of some earlier statement).
                if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 {
                    return None;
                }
            }
            _ => {}
        }
    }

    let open = open_paren_byte?;

    // Count top-level commas between `open+1` and `cursor_byte`.
    let active_param = count_top_level_commas(bytes, open + 1, cursor_byte.min(bytes.len()), &skip);

    // Extract the callee text by walking backwards from `open` (skipping
    // whitespace) collecting identifier characters, `.`, `::` segments.
    let callee_text = extract_callee_before(bytes, open)?;

    Some(CallSite {
        callee_text,
        open_paren_byte: open,
        active_param,
    })
}

/// Build a bitmap (Vec<bool>) where `mask[i] == true` means byte `i` is
/// inside a string literal, char literal, line comment, or block comment and
/// must be ignored by the backwards walker.
fn compute_skip_mask(bytes: &[u8]) -> Vec<bool> {
    let mut mask = vec![false; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Line comment: `//...\n`
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            mask[start..i].fill(true);
            continue;
        }
        // Block comment: `/* ... */`
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                i = bytes.len();
            }
            mask[start..i].fill(true);
            continue;
        }
        // String / char literal.
        if b == b'"' || b == b'\'' {
            let quote = b;
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    // Unterminated — bail out of the literal at the newline
                    // so we don't swallow the rest of the file.
                    break;
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == quote {
                i += 1;
            }
            mask[start..i].fill(true);
            continue;
        }
        i += 1;
    }
    mask
}

fn count_top_level_commas(
    bytes: &[u8],
    from: usize,
    to: usize,
    skip: &[bool],
) -> u32 {
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut commas: u32 = 0;
    let mut i = from;
    while i < to {
        if !skip[i] {
            match bytes[i] {
                b'(' => depth_paren += 1,
                b')' => depth_paren -= 1,
                b'[' => depth_bracket += 1,
                b']' => depth_bracket -= 1,
                b'{' => depth_brace += 1,
                b'}' => depth_brace -= 1,
                b',' => {
                    if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 {
                        commas += 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    commas
}

/// Walk backwards from `open_paren_byte` (exclusive), skip whitespace, then
/// collect an identifier chain: `A-Za-z0-9_` bytes plus `.` and `::`. Returns
/// the collected text in forward order. Returns `None` if no identifier
/// character precedes the paren (e.g. `(1 + 2)` — a parenthesised expression,
/// not a call).
fn extract_callee_before(bytes: &[u8], open_paren_byte: usize) -> Option<String> {
    let mut i = open_paren_byte;
    // Skip whitespace just before the `(`.
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    let end = i;
    // Walk backwards collecting identifier + `.` + `::` bytes.
    while i > 0 {
        let c = bytes[i - 1];
        if is_ident_byte(c) || c == b'.' {
            i -= 1;
            continue;
        }
        if c == b':' && i >= 2 && bytes[i - 2] == b':' {
            i -= 2;
            continue;
        }
        break;
    }
    if i == end {
        return None;
    }
    // Trim a trailing dot/colon we might have left behind if the identifier
    // chain ends weirdly; also trim a leading one for safety.
    let slice = &bytes[i..end];
    let text = std::str::from_utf8(slice).ok()?.to_string();
    let trimmed = text
        .trim_start_matches(['.', ':'])
        .trim_end_matches(['.', ':'])
        .to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ---------------------------------------------------------------------------
// Part 2: resolve the callee to `ResolvedSignature` entries.
// ---------------------------------------------------------------------------

fn resolve_callee(
    callee: &str,
    source: &str,
    file: &SourceFile,
    cursor_offset: u32,
    scope: Option<&GlobalScope<'_>>,
    type_index: Option<&TypeIndex>,
) -> Vec<ResolvedSignature> {
    // Member / method call: `recv.method` (split on the LAST dot).
    if let Some(last_dot) = callee.rfind('.') {
        let receiver = &callee[..last_dot];
        let method = &callee[last_dot + 1..];
        return resolve_member_call(receiver, method, source, file, cursor_offset, scope, type_index);
    }

    // Free function, bare or qualified (`::` -> namespaced).
    let mut out: Vec<ResolvedSignature> = Vec::new();

    // Workspace overloads first.
    if let Some(scope) = scope {
        for ov in scope.lookup_function_overloads(callee) {
            out.push(overload_to_signature(callee.to_string(), ov, None));
        }
        // If unqualified and no workspace hit, also try the last `::`
        // segment as a bare name for workspace lookup.
        if out.is_empty() {
            if let Some(idx) = callee.rfind("::") {
                let bare = &callee[idx + 2..];
                for ov in scope.lookup_function_overloads(bare) {
                    out.push(overload_to_signature(bare.to_string(), ov, None));
                }
            }
        }
    }

    // External type index free functions.
    if let Some(index) = type_index {
        if let Some(fns) = index.lookup_function(callee) {
            for f in fns {
                out.push(function_info_to_signature(f));
            }
        } else if let Some(idx) = callee.rfind("::") {
            // Try the bare tail against the external index too.
            let bare = &callee[idx + 2..];
            if let Some(fns) = index.lookup_function(bare) {
                for f in fns {
                    out.push(function_info_to_signature(f));
                }
            }
        }
    }

    // Constructor-as-call: `Foo(args)` → TypeIndex type lookup.
    if out.is_empty() {
        if let Some(index) = type_index {
            if let Some(ty) = index.lookup_type(callee) {
                // Expose type as a zero-arg "constructor" placeholder — the
                // external DB rarely carries ctors explicitly so this mostly
                // serves to confirm the name resolves.
                out.push(ResolvedSignature {
                    label_name: ty.name.clone(),
                    return_type: ty.name.clone(),
                    params: Vec::new(),
                    doc: ty.doc.clone(),
                    min_args: 0,
                });
            }
        }
    }

    out
}

fn resolve_member_call(
    receiver: &str,
    method: &str,
    source: &str,
    file: &SourceFile,
    cursor_offset: u32,
    scope: Option<&GlobalScope<'_>>,
    type_index: Option<&TypeIndex>,
) -> Vec<ResolvedSignature> {
    // MVP: only resolve simple-identifier receivers whose type is a local
    // var or a same-file class field. Anything else falls through to empty.
    let receiver_trimmed = receiver.trim();
    if receiver_trimmed.is_empty() || !is_simple_ident(receiver_trimmed) {
        return Vec::new();
    }

    let mut receiver_type: Option<String> =
        scope_query::local_type_at(source, file, cursor_offset, receiver_trimmed);

    if receiver_type.is_none() {
        if let Some(cls) = scope_query::find_enclosing_class(file, cursor_offset) {
            receiver_type = scope_query::class_member_type(cls, source, receiver_trimmed);
        }
    }

    // `this` → enclosing class name.
    if receiver_type.is_none() && receiver_trimmed == "this" {
        if let Some(cls) = scope_query::find_enclosing_class(file, cursor_offset) {
            receiver_type = Some(cls.name.text(source).to_string());
        }
    }

    let Some(ty_text) = receiver_type else {
        return Vec::new();
    };
    // Strip trailing `@`, generic args, and leading `const `.
    let bare_type = strip_type_decoration(&ty_text);
    let mut out: Vec<ResolvedSignature> = Vec::new();

    // Workspace method lookup via `workspace_class_member` — returns just a
    // return type, which isn't enough for signature help. We need the full
    // parameter list, so walk the SymbolTable directly for `Class::method`
    // (and its parents).
    if let Some(scope) = scope {
        collect_workspace_method_overloads(&bare_type, method, scope, &mut out);
    }

    // External type index method lookup.
    if let Some(index) = type_index {
        if let Some(ty) = index.lookup_type(&bare_type) {
            for m in &ty.methods {
                if m.name == method {
                    let params: Vec<(String, String)> = m
                        .params
                        .iter()
                        .map(|p| (p.type_name.clone(), p.name.clone().unwrap_or_default()))
                        .collect();
                    let min_args = m.params.iter().filter(|p| p.default.is_none()).count();
                    out.push(ResolvedSignature {
                        label_name: m.name.clone(),
                        return_type: m.return_type.clone(),
                        params,
                        doc: m.doc.clone(),
                        min_args,
                    });
                }
            }
        }
    }

    out
}

/// Walk the workspace class-inheritance chain looking for `Class::method`
/// entries and push each matching `SymbolKind::Function` onto `out`.
fn collect_workspace_method_overloads(
    class_name: &str,
    method: &str,
    scope: &GlobalScope<'_>,
    out: &mut Vec<ResolvedSignature>,
) {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current: Option<String> = Some(class_name.to_string());
    while let Some(name) = current.take() {
        if !visited.insert(name.clone()) {
            break;
        }
        let qualified = format!("{}::{}", name, method);
        let mut any = false;
        for s in scope.workspace.all_symbols() {
            if s.name != qualified {
                continue;
            }
            if let SymbolKind::Function {
                return_type,
                params,
                min_args,
            } = &s.kind
            {
                any = true;
                // Storage order: `(name, type_text)`.
                let sig_params: Vec<(String, String)> =
                    params.iter().map(|(n, t)| (t.clone(), n.clone())).collect();
                out.push(ResolvedSignature {
                    label_name: method.to_string(),
                    return_type: if return_type.is_empty() {
                        "void".to_string()
                    } else {
                        return_type.clone()
                    },
                    params: sig_params,
                    doc: s.doc.clone(),
                    min_args: *min_args,
                });
            }
        }
        if any {
            // First class in the chain that defines the method wins —
            // matches standard override semantics. Keep walking only if we
            // found nothing on the current class.
            return;
        }
        current = scope.workspace_class_parent(&name);
    }
}

fn is_simple_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip `const`, trailing `@`, and generic args (`array<int>` → `array`).
fn strip_type_decoration(ty: &str) -> String {
    let t = ty.trim();
    let t = t.strip_prefix("const ").unwrap_or(t).trim();
    let t = t.strip_suffix('@').unwrap_or(t).trim();
    if let Some(lt) = t.find('<') {
        return t[..lt].trim().to_string();
    }
    t.to_string()
}

// ---------------------------------------------------------------------------
// `OverloadSig` / `FunctionInfo` → `ResolvedSignature` conversion.
// ---------------------------------------------------------------------------

fn overload_to_signature(
    name: String,
    ov: OverloadSig,
    doc: Option<String>,
) -> ResolvedSignature {
    let return_type = if ov.return_type.is_empty() {
        "void".to_string()
    } else {
        ov.return_type
    };
    ResolvedSignature {
        label_name: name,
        return_type,
        // Workspace params come from `OverloadSig` as raw type text only
        // (no names). Use an empty name so the label falls back to "type".
        params: ov.param_types.into_iter().map(|t| (t, String::new())).collect(),
        doc,
        min_args: ov.min_args,
    }
}

fn function_info_to_signature(f: &crate::typedb::index::FunctionInfo) -> ResolvedSignature {
    let params: Vec<(String, String)> = f
        .params
        .iter()
        .map(|p| (p.type_name.clone(), p.name.clone().unwrap_or_default()))
        .collect();
    let min_args = f.params.iter().filter(|p| p.default.is_none()).count();
    ResolvedSignature {
        label_name: f.name.clone(),
        return_type: f.return_type.clone(),
        params,
        doc: f.doc.clone(),
        min_args,
    }
}

// ---------------------------------------------------------------------------
// Part 3: build `SignatureInformation` + pick active signature.
// ---------------------------------------------------------------------------

fn to_signature_information(sig: &ResolvedSignature) -> SignatureInformation {
    let param_texts: Vec<String> = sig
        .params
        .iter()
        .map(|(ty, name)| {
            if name.is_empty() {
                ty.clone()
            } else {
                format!("{} {}", ty, name)
            }
        })
        .collect();
    let label = format!(
        "{}({}) -> {}",
        sig.label_name,
        param_texts.join(", "),
        sig.return_type,
    );
    let parameters: Vec<ParameterInformation> = param_texts
        .into_iter()
        .map(|pt| ParameterInformation {
            label: ParameterLabel::Simple(pt),
            documentation: None,
        })
        .collect();
    let documentation = sig.doc.as_ref().map(|d| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: d.clone(),
        })
    });
    SignatureInformation {
        label,
        documentation,
        parameters: Some(parameters),
        active_parameter: None,
    }
}

/// Pick the active signature index. Prefer the smallest overload that can
/// still accept `active_param + 1` arguments; if none qualify, fall back to
/// the first overload whose arity is at least `active_param`, else signature 0.
fn pick_active_signature(sigs: &[ResolvedSignature], active_param: u32) -> usize {
    let ap = active_param as usize;
    let mut best: Option<(usize, usize)> = None; // (index, params.len())
    for (i, s) in sigs.iter().enumerate() {
        if ap < s.params.len() {
            let plen = s.params.len();
            match best {
                None => best = Some((i, plen)),
                Some((_, cur)) if plen < cur => best = Some((i, plen)),
                _ => {}
            }
        }
    }
    if let Some((i, _)) = best {
        return i;
    }
    // Nothing fits the current arg position — fall back to the first overload
    // whose arity >= active_param, else 0.
    for (i, s) in sigs.iter().enumerate() {
        if s.params.len() >= ap {
            return i;
        }
    }
    0
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use crate::parser::Parser;

    fn ws_from(source: &str) -> SymbolTable {
        let mut table = SymbolTable::new();
        let tokens = lexer::tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        table.set_file_symbols(fid, syms);
        table
    }

    /// Offset-based helper: returns the `Position` corresponding to a
    /// placeholder `|` in `src`. The placeholder is stripped before parsing.
    /// This makes it easier to write cursor-at-this-point tests.
    fn split_cursor(src: &str) -> (String, Position) {
        let idx = src.find('|').expect("test source must contain `|`");
        let before = &src[..idx];
        let after = &src[idx + 1..];
        let line = before.matches('\n').count() as u32;
        let col = before.rfind('\n').map_or(before.len(), |nl| before.len() - nl - 1) as u32;
        let mut joined = String::with_capacity(before.len() + after.len());
        joined.push_str(before);
        joined.push_str(after);
        (joined, Position::new(line, col))
    }

    #[test]
    fn test_single_overload_basic_param_zero() {
        let src = "void f(int a, string b) {}\nvoid main() { f(| }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help =
            signature_help(&source, position, None, Some(&ws)).expect("signature help");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.active_signature, Some(0));
        assert_eq!(help.active_parameter, Some(0));
        assert!(help.signatures[0].label.contains("int"));
        assert!(help.signatures[0].label.contains("string"));
    }

    #[test]
    fn test_single_overload_param_one_after_comma() {
        let src = "void f(int a, string b) {}\nvoid main() { f(42,| }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help =
            signature_help(&source, position, None, Some(&ws)).expect("signature help");
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(help.active_signature, Some(0));
    }

    #[test]
    fn test_three_overloads_active_by_arity() {
        let src = "\
void f() {}
void f(int a) {}
void f(int a, string b) {}
void main() { f(1,| }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help =
            signature_help(&source, position, None, Some(&ws)).expect("signature help");
        assert_eq!(help.signatures.len(), 3);
        assert_eq!(help.active_parameter, Some(1));
        // Only the third overload has 2+ params, so active_signature must be
        // whichever index in `help.signatures` corresponds to the 2-param
        // overload. Identify it by its label.
        let active = help
            .active_signature
            .expect("active_signature should be set") as usize;
        assert!(
            help.signatures[active].label.contains("int")
                && help.signatures[active].label.contains("string"),
            "expected 2-param overload active, got {:?}",
            help.signatures[active].label
        );
    }

    #[test]
    fn test_active_parameter_through_trailing_comma_whitespace() {
        let src = "void f(int a, string b) {}\nvoid main() { f(1, | }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help =
            signature_help(&source, position, None, Some(&ws)).expect("signature help");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn test_nested_call_returns_inner() {
        let src = "\
void outer(int a) {}
void inner(string s) {}
void main() { outer(inner(| ) }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help =
            signature_help(&source, position, None, Some(&ws)).expect("signature help");
        let active = help.active_signature.unwrap_or(0) as usize;
        assert!(
            help.signatures[active].label.contains("inner"),
            "expected inner signature, got {:?}",
            help.signatures[active].label
        );
    }

    #[test]
    fn test_returns_none_outside_any_call() {
        let src = "void f() {}\nvoid main() { int x = 5;| }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help = signature_help(&source, position, None, Some(&ws));
        assert!(help.is_none(), "expected None outside a call, got {:?}", help);
    }

    #[test]
    fn test_method_call_via_receiver() {
        let src = "\
class Foo { void m(int x) {} }
void main() { Foo f; f.m(| }";
        let (source, position) = split_cursor(src);
        let ws = ws_from(&source);
        let help =
            signature_help(&source, position, None, Some(&ws)).expect("signature help");
        assert_eq!(help.active_parameter, Some(0));
        assert!(
            help.signatures[0].label.contains("int"),
            "expected `int` param in method label, got {:?}",
            help.signatures[0].label
        );
    }

    #[test]
    fn test_find_enclosing_call_skips_strings() {
        // The `)` inside the string must NOT close the call.
        let src = r#"void f(int a) {}
void main() { f("hello)world", | "#;
        let (source, position) = split_cursor(src);
        let cursor = position_to_offset(&source, position);
        let call = find_enclosing_call(&source, cursor).expect("should find call");
        assert_eq!(call.callee_text, "f");
        assert_eq!(call.active_param, 1);
    }

    #[test]
    fn test_find_enclosing_call_skips_line_comments() {
        let src = "void f(int a) {}\nvoid main() { f(/* ) */ | ";
        let (source, position) = split_cursor(src);
        let cursor = position_to_offset(&source, position);
        let call = find_enclosing_call(&source, cursor).expect("should find call");
        assert_eq!(call.callee_text, "f");
        assert_eq!(call.active_param, 0);
    }

}
