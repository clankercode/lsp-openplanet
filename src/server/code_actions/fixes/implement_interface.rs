//! B095: "Implement `void I::M()`" for a class that does not satisfy an
//! interface it declares.
//!
//! Answers [`TypeDiagnosticKind::MissingInterfaceImplementation`], which
//! B094 anchors on the class NAME — chosen partly for this: the name is a
//! stable place to hang the action, and the class body is reachable from it.
//!
//! # Where the stub's text comes from
//!
//! Not from the diagnostic. The engine-shaped signature B094 carries
//! (`void I::M(const string&in)`) is deliberately the game's spelling and
//! has no parameter NAMES in it, so writing a stub from it would invent
//! `a0`, `a1`. Instead the interface's own pooled declaration is looked up
//! and its parameters are copied verbatim — the user gets `const string &in
//! msg`, the identifier the interface author chose, which is what makes the
//! stub worth accepting rather than retyping.
//!
//! The signature is still what SELECTS the overload, so a class missing two
//! same-named methods gets two actions offering different stubs.
//!
//! # Refusals
//!
//! `describe` returns nothing — no menu entry at all — rather than offering
//! an edit that would produce code the user has to repair:
//!
//! * the class body's closing brace cannot be located;
//! * the interface declaration cannot be found, or its overload cannot be
//!   picked unambiguously;
//! * the method returns a REFERENCE, where no default-value stub is
//!   correct.
//!
//! The seam requires `materialize` to be total for everything `describe`
//! offered, so each refusal has to happen here where it is free.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::parser::ast::{Item, SourceFile};
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::symbols::scope::SymbolKind;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

pub const FAMILY: FixFamily = FixFamily {
    name: "implement-interface-method",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(
        kind,
        TypeDiagnosticKind::MissingInterfaceImplementation { .. }
    )
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    let Some(plan) = plan(diag, ctx) else {
        return Vec::new();
    };
    vec![FixDescriptor {
        title: format!("Implement `{}`", plan.signature),
        kind: CodeActionKind::QUICKFIX,
        is_preferred: true,
        // Deliberately NOT auto-applicable: a stub is a placeholder the user
        // must fill in, so `source.fixAll` must never insert one silently.
        auto_applicable: false,
    }]
}

fn materialize(
    diag: &TypeDiagnostic,
    ctx: &FixContext<'_>,
    _selected_title: &str,
) -> WorkspaceEdit {
    let Some(plan) = plan(diag, ctx) else {
        debug_assert!(false, "described an implement-interface fix with no plan");
        return WorkspaceEdit::default();
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        ctx.uri.clone(),
        vec![TextEdit {
            range: span_to_range(
                &ctx.analysis.source,
                crate::lexer::Span {
                    start: plan.insert_at,
                    end: plan.insert_at,
                },
            ),
            new_text: plan.stub,
        }],
    );
    WorkspaceEdit::new(changes)
}

/// Everything the edit needs, or `None` if any of it is uncertain.
struct Plan {
    signature: String,
    /// Byte offset to insert at: immediately before the class's `}`.
    insert_at: u32,
    stub: String,
}

fn plan(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<Plan> {
    let TypeDiagnosticKind::MissingInterfaceImplementation { signature } = &diag.kind else {
        return None;
    };
    let parsed = parse_signature(signature)?;
    let source = &ctx.analysis.source;

    // The class the diagnostic is anchored on, and its closing brace.
    let class_span = find_class_span_by_name(&ctx.analysis.file, diag.span)?;
    let close = source
        .get(..class_span.end as usize)?
        .rfind('}')
        .and_then(|i| u32::try_from(i).ok())?;
    if close < class_span.start {
        return None;
    }

    // Parameter names come from the interface's own declaration, never from
    // the engine-shaped signature (which has none).
    let params = interface_params(ctx, &parsed)?;
    if parsed.return_type.contains('&') {
        // No default-value stub is correct for a reference return.
        return None;
    }

    let indent = line_indent(source, class_span.start as usize);
    let inner = format!("{indent}    ");
    let body = if parsed.return_type == "void" {
        format!("{inner}    // TODO: implement {}\n", parsed.qualified)
    } else {
        format!(
            "{inner}    // TODO: implement {}\n{inner}    {} ret;\n{inner}    return ret;\n",
            parsed.qualified, parsed.return_type
        )
    };
    // `class C : I {}` puts the insertion point straight after `{`, so the
    // stub needs its own newline; a class whose `}` already sits on its own
    // line must not get a blank one.
    let lead = if source[..close as usize]
        .rsplit('\n')
        .next()
        .is_some_and(|tail| tail.trim().is_empty())
    {
        ""
    } else {
        "\n"
    };
    let stub = format!(
        "{lead}{inner}{} {}({}) {{\n{body}{inner}}}\n{indent}",
        parsed.return_type,
        parsed.method,
        params.join(", ")
    );
    Some(Plan {
        signature: signature.clone(),
        insert_at: close,
        stub,
    })
}

/// `void I::M(const string&in)` split into its parts.
struct ParsedSignature {
    return_type: String,
    /// `I::M`
    qualified: String,
    /// `I`
    interface: String,
    /// `M`
    method: String,
    /// Canonical (whitespace-free) parameter types, as B094 renders them.
    param_types: Vec<String>,
}

fn parse_signature(signature: &str) -> Option<ParsedSignature> {
    let (head, rest) = signature.split_once('(')?;
    let params = rest.strip_suffix(')')?;
    let (return_type, qualified) = head.trim().rsplit_once(' ')?;
    let (interface, method) = qualified.rsplit_once("::")?;
    Some(ParsedSignature {
        return_type: return_type.trim().to_string(),
        qualified: qualified.to_string(),
        interface: interface.to_string(),
        method: method.to_string(),
        param_types: if params.is_empty() {
            Vec::new()
        } else {
            params.split(',').map(|p| p.trim().to_string()).collect()
        },
    })
}

/// Parameters of the interface method, rendered as `<type> <name>` using the
/// interface author's own identifiers. `None` when the declaration cannot be
/// found or the overload cannot be picked unambiguously.
fn interface_params(ctx: &FixContext<'_>, parsed: &ParsedSignature) -> Option<Vec<String>> {
    let key = format!("{}::{}", parsed.interface, parsed.method);
    let mut hit = None;
    for sym in ctx.workspace.lookup(&key) {
        let SymbolKind::Function { params, .. } = &sym.kind else {
            continue;
        };
        let canonical: Vec<String> = params
            .iter()
            .map(|(_, t)| t.chars().filter(|c| !c.is_whitespace()).collect())
            .collect();
        if canonical != parsed.param_types {
            continue;
        }
        if hit.is_some() {
            return None; // two indistinguishable overloads — do not guess
        }
        hit = Some(params.clone());
    }
    let params = hit?;
    Some(
        params
            .iter()
            .enumerate()
            .map(|(i, (name, ty))| {
                if name.is_empty() {
                    format!("{} a{}", ty.trim(), i)
                } else {
                    format!("{} {}", ty.trim(), name)
                }
            })
            .collect(),
    )
}

/// Span of the class whose NAME is at `name_span` — the anchor B094 uses.
fn find_class_span_by_name(
    file: &SourceFile,
    name_span: crate::lexer::Span,
) -> Option<crate::lexer::Span> {
    fn walk(items: &[Item], want: crate::lexer::Span) -> Option<crate::lexer::Span> {
        for item in items {
            match item {
                Item::Class(cls) if cls.name.span == want => return Some(cls.span),
                Item::Namespace(ns) => {
                    if let Some(found) = walk(&ns.items, want) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(&file.items, name_span)
}

/// Leading whitespace of the line containing `offset`.
fn line_indent(source: &str, offset: usize) -> String {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    source[start..offset]
        .chars()
        .take_while(|c| c.is_whitespace() && *c != '\n')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::code_actions::fix::{collect_fixes, WorkspaceFileResolver};
    use crate::server::code_actions::test_fixtures::{apply_fixes, ctx_for};

    fn with_ctx<T>(source: &str, f: impl FnOnce(&FixContext<'_>) -> T) -> T {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::empty(),
        };
        f(&ctx)
    }

    /// Titles this family offers for `source`.
    fn titles(source: &str) -> Vec<String> {
        with_ctx(source, |ctx| {
            let mut t: Vec<String> = collect_fixes(ctx)
                .into_iter()
                .filter(|c| c.family == "implement-interface-method")
                .map(|c| c.fix.title.clone())
                .collect();
            t.sort();
            t
        })
    }

    /// The source after applying every fix offered.
    fn fixed(source: &str) -> String {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::empty(),
        };
        apply_fixes(source, &collect_fixes(&ctx))
    }

    #[test]
    fn a_missing_method_is_offered() {
        assert_eq!(
            titles("interface I { void M(); }\nclass C : I {}\n"),
            vec!["Implement `void I::M()`"]
        );
    }

    /// One action per missing method, matching B094's one-diagnostic-per-
    /// method granularity.
    #[test]
    fn every_missing_method_gets_its_own_action() {
        assert_eq!(
            titles("interface I { void M(); int P(float f); }\nclass C : I {}\n"),
            vec!["Implement `int I::P(float)`", "Implement `void I::M()`"]
        );
    }

    /// THE POINT OF LOOKING THE INTERFACE UP: the stub carries the
    /// interface author's own parameter names, not invented ones.
    #[test]
    fn the_stub_uses_the_interfaces_parameter_names() {
        let out = fixed(
            "interface I { void Log(const string &in message, int level); }\nclass C : I {}\n",
        );
        assert!(
            out.contains("void Log(const string &in message, int level)"),
            "parameter names must come from the interface: {out:?}"
        );
        assert!(
            !out.contains("a0"),
            "must not invent parameter names when real ones exist: {out:?}"
        );
    }

    #[test]
    fn a_void_stub_has_an_empty_body_with_a_todo() {
        assert_eq!(
            fixed("interface I { void M(); }\nclass C : I {}\n"),
            "interface I { void M(); }\nclass C : I {\n    void M() {\n        // TODO: implement I::M\n    }\n}\n"
        );
    }

    /// THE PROPERTY THAT MATTERS: applying the actions must actually SATISFY
    /// the interface. A stub that looks right but leaves the diagnostic
    /// standing is worse than no action — the user accepts it and the
    /// squiggle stays. Checked by re-running the real checker over the fixed
    /// text; string-matching the stub cannot establish this.
    ///
    /// Applied ONE AT A TIME, re-deriving after each, which is what an
    /// editor does and is not a convenience here: every action for a given
    /// class inserts at the SAME offset (just before its `}`), so applying
    /// two of them to one document text drops one. This loop is therefore
    /// also the convergence proof — it terminates only if each stub really
    /// removes its own diagnostic. Batching them instead would silently
    /// re-introduce that, which is why `auto_applicable` is false.
    #[test]
    fn applying_the_actions_one_at_a_time_clears_every_diagnostic() {
        for src in [
            "interface I { void M(); }\nclass C : I {}\n",
            "interface I { void M(); int P(float f); }\nclass C : I {}\n",
            "interface I { void Log(const string &in m, int l); }\nclass C : I {}\n",
            "namespace N {\n    interface I { void M(); }\n    class C : I {}\n}\n",
            "interface I { void M(); void M(int a); }\nclass C : I {}\n",
            "class T {}\ninterface I { T@ Get(); }\nclass C : I {}\n",
            "interface I { void M(); }\nclass C : I {\n    int f;\n}\n",
            "interface I { void M(); }\ninterface J : I { void N(); }\nclass C : J {}\n",
            "interface I { void M(); }\nabstract class C : I {}\n",
            "interface I { void A(); void B(); void C(); }\nclass K : I {}\n",
        ] {
            let mut text = src.to_string();
            let mut applied = 0;
            while let Some(edit) = first_action_edit(&text) {
                text = apply_one(&text, &edit);
                applied += 1;
                assert!(applied <= 8, "did not converge for `{src}`:\n{text}");
            }
            assert!(applied > 0, "no action was offered for `{src}`");
            let still = remaining_missing(&text);
            assert!(
                still.is_empty(),
                "after applying {applied} action(s) to `{src}` the class still \
                 does not satisfy the interface: {still:?}\n--- result ---\n{text}"
            );
        }
    }

    /// The first edit this family offers for `source`, if any.
    fn first_action_edit(source: &str) -> Option<TextEdit> {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::empty(),
        };
        collect_fixes(&ctx)
            .into_iter()
            .find(|c| c.family == "implement-interface-method")
            .and_then(|c| {
                c.fix
                    .edit
                    .changes
                    .as_ref()
                    .and_then(|m| m.values().next())
                    .and_then(|edits| edits.first())
                    .cloned()
            })
    }

    fn apply_one(source: &str, edit: &TextEdit) -> String {
        let start = crate::server::diagnostics::position_to_offset(source, edit.range.start);
        let end = crate::server::diagnostics::position_to_offset(source, edit.range.end);
        let mut out = String::with_capacity(source.len() + edit.new_text.len());
        out.push_str(&source[..start]);
        out.push_str(&edit.new_text);
        out.push_str(&source[end..]);
        out
    }

    /// `MissingInterfaceImplementation` signatures the real checker still
    /// reports for `source`.
    fn remaining_missing(source: &str) -> Vec<String> {
        use crate::lexer::tokenize_filtered;
        use crate::parser::Parser;
        use crate::symbols::SymbolTable;
        use crate::typecheck::{Checker, GlobalScope};
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, None);
        let mut checker = Checker::new(source, &scope);
        checker.check_file(&file);
        checker
            .diagnostics
            .iter()
            .filter_map(|d| match &d.kind {
                TypeDiagnosticKind::MissingInterfaceImplementation { signature } => {
                    Some(signature.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// A non-void stub must return something, or the inserted code does not
    /// compile — which would make the action worse than useless.
    #[test]
    fn a_non_void_stub_declares_and_returns_a_value() {
        let out = fixed("interface I { int Count(); }\nclass C : I {}\n");
        assert!(out.contains("int Count() {"), "{out:?}");
        assert!(out.contains("int ret;"), "{out:?}");
        assert!(out.contains("return ret;"), "{out:?}");
    }

    /// Inserting into a class that already has members must not disturb them
    /// and must land inside the braces.
    #[test]
    fn the_stub_lands_inside_a_class_that_already_has_members() {
        let out = fixed(
            "interface I { void M(); }\nclass C : I {\n    int field;\n    void Other() {}\n}\n",
        );
        assert!(out.contains("int field;"), "existing member lost: {out:?}");
        assert!(
            out.contains("void Other() {}"),
            "existing member lost: {out:?}"
        );
        let body_start = out.find("class C : I {").unwrap();
        let stub = out.find("void M() {").expect("stub missing");
        let close = out.rfind('}').unwrap();
        assert!(
            body_start < stub && stub < close,
            "stub must land inside the class body: {out:?}"
        );
    }

    /// Indentation follows the class, so a namespaced class gets a deeper
    /// stub rather than a flush-left one.
    #[test]
    fn indentation_follows_the_class_declaration() {
        let out = fixed("namespace N {\n    interface I { void M(); }\n    class C : I {}\n}\n");
        assert!(
            out.contains("        void M() {"),
            "expected 8-space indent inside a namespaced class: {out:?}"
        );
    }

    /// Two same-named overloads missing: each action offers its own stub,
    /// selected by the signature.
    #[test]
    fn same_named_overloads_get_distinct_actions() {
        let t = titles("interface I { void M(); void M(int a); }\nclass C : I {}\n");
        assert_eq!(
            t,
            vec!["Implement `void I::M()`", "Implement `void I::M(int)`"]
        );
    }

    /// A stub is a placeholder, so `source.fixAll` must never insert one.
    #[test]
    fn the_action_is_not_auto_applicable() {
        with_ctx("interface I { void M(); }\nclass C : I {}\n", |ctx| {
            let c: Vec<_> = collect_fixes(ctx)
                .into_iter()
                .filter(|c| c.family == "implement-interface-method")
                .collect();
            assert_eq!(c.len(), 1);
            assert!(!c[0].fix.auto_applicable, "a stub must not be auto-applied");
            assert!(c[0].fix.is_preferred);
            assert_eq!(c[0].fix.kind, CodeActionKind::QUICKFIX);
        });
    }

    // ── REFUSALS. `materialize` must be total for everything described. ──

    /// A reference return has no correct default-value stub.
    #[test]
    fn a_reference_return_is_refused() {
        assert!(titles("class T {}\ninterface I { T& Get(); }\nclass C : I {}\n").is_empty());
    }

    /// No diagnostic, no action.
    #[test]
    fn a_correctly_implemented_class_offers_nothing() {
        assert!(titles("interface I { void M(); }\nclass C : I { void M() {} }\n").is_empty());
    }

    #[test]
    fn a_class_with_no_interface_offers_nothing() {
        assert!(titles("interface I { void M(); }\nclass C {}\n").is_empty());
    }

    /// EVERY described action must materialize to a non-empty edit — the
    /// seam declares `materialize` total, so a describe/materialize
    /// disagreement is a panic in the language server rather than a missing
    /// menu entry. Swept over every shape above at once.
    #[test]
    fn every_described_action_materializes() {
        for src in [
            "interface I { void M(); }\nclass C : I {}\n",
            "interface I { void M(); int P(float f); }\nclass C : I {}\n",
            "interface I { void Log(const string &in m, int l); }\nclass C : I {}\n",
            "namespace N {\n    interface I { void M(); }\n    class C : I {}\n}\n",
            "interface I { void M(); void M(int a); }\nclass C : I {}\n",
            "class T {}\ninterface I { T@ Get(); }\nclass C : I {}\n",
            "interface I { void M(); }\nclass C : I {\n    int f;\n}\n",
        ] {
            with_ctx(src, |ctx| {
                for cand in collect_fixes(ctx)
                    .into_iter()
                    .filter(|c| c.family == "implement-interface-method")
                {
                    let changes = cand.fix.edit.changes.clone().unwrap_or_default();
                    let edits = changes.values().next().cloned().unwrap_or_default();
                    assert!(
                        edits.iter().any(|e| !e.new_text.is_empty()),
                        "described `{}` for `{src}` but materialized nothing",
                        cand.fix.title
                    );
                }
            });
        }
    }
}
