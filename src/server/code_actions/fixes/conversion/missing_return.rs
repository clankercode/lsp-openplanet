//! `MissingReturn` — control can fall off the end of a value-returning
//! function.
//!
//! The fix appends `return <default>;` as the function body's last
//! statement.
//!
//! **Where it goes is not a guess.** The checker reports this exactly when
//! control can reach the closing brace, so the closing brace *is* the
//! missing path — including for a function that already returns on some
//! branches (`int f(bool c) { if (c) return 1; }`). The action therefore
//! never has to choose between candidate branches, and never withholds
//! itself for ambiguity; it withholds only when the return type has no
//! obvious default (see [`default_literal`]).
//!
//! **What it returns is a guess**, and that is why the action is neither
//! preferred nor auto-applicable: a plausible-looking wrong return value is
//! worse than the compile error it replaces, because the compiler stops
//! complaining. The default table is deliberately narrow — primitives and
//! handles only. A class or array return type gets no action at all rather
//! than an invented value.
//!
//! Literal forms: `f`/`F` is AngelScript's documented explicit-float suffix
//! (the manual's auto-declaration example, `auto f = 18 + 5.f;`, resolves to
//! `float`; this repo's lexer accepts it at `lexer/scanner.rs`). The manual
//! does not pin down what an *unsuffixed* decimal literal is, so the table
//! sidesteps the question: `0.0f` is a float under either reading, and `0.0`
//! is exactly representable as a double under either.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::parser::ast::{FunctionDecl, TypeExpr, TypeExprKind};
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

use super::support::{function_by_name_span, is_integer_keyword, primitive_type};

pub const FAMILY: FixFamily = FixFamily {
    name: "missing-return",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::MissingReturn { .. })
}

/// The value to return for `ty`, or `None` when there is no obvious one.
///
/// `void` is absent by construction — the checker never reports a missing
/// return for it — and stays absent here so a hand-built diagnostic cannot
/// produce `return ;`.
fn default_literal(ty: &TypeExpr) -> Option<&'static str> {
    if let TypeExprKind::Const(inner) = &ty.kind {
        return default_literal(inner);
    }
    if matches!(ty.kind, TypeExprKind::Handle(_)) {
        return Some("null");
    }
    match primitive_type(ty)?.0 {
        "bool" => Some("false"),
        "float" => Some("0.0f"),
        "double" => Some("0.0"),
        "string" => Some("\"\""),
        keyword if is_integer_keyword(keyword) => Some("0"),
        // `void` (and anything else the keyword table grows) gets nothing.
        _ => None,
    }
}

/// Where the `return` goes and what it looks like there.
struct Insertion {
    at: u32,
    text: String,
}

fn insertion(source: &str, func: &FunctionDecl, default: &str) -> Option<Insertion> {
    let body = func.body.as_ref()?;
    let close = (body.span.end as usize).checked_sub(1)?;
    // The body span runs `{` … `}`; if the parser recovered without a real
    // closing brace there is nowhere safe to insert.
    if source.as_bytes().get(close) != Some(&b'}') {
        return None;
    }
    let line_start = source
        .get(..close)?
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let before_brace = source.get(line_start..close)?;
    if !before_brace.trim().is_empty() {
        // Single-line body: `int f() { }` → `int f() { return 0; }`.
        let lead = if before_brace.ends_with([' ', '\t']) {
            ""
        } else {
            " "
        };
        return Some(Insertion {
            at: close as u32,
            text: format!("{lead}return {default}; "),
        });
    }
    // The brace owns its line, so add a line above it. Match the last
    // statement's indentation where there is one — that is the file's own
    // unit, tabs included — and otherwise step in from the brace.
    let indent = body
        .stmts
        .last()
        .and_then(|stmt| line_indent(source, stmt.span.start as usize))
        .unwrap_or_else(|| format!("{before_brace}    "));
    Some(Insertion {
        at: line_start as u32,
        text: format!("{indent}return {default};\n"),
    })
}

/// The leading whitespace of the line `offset` sits on, when `offset` is the
/// first non-whitespace on that line.
fn line_indent(source: &str, offset: usize) -> Option<String> {
    let line_start = source
        .get(..offset)?
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let indent = source.get(line_start..offset)?;
    indent
        .trim()
        .is_empty()
        .then(|| indent.replace(['\r', '\n'], ""))
}

fn title(function_name: &str, default: &str) -> String {
    format!("Add `return {default};` at the end of `{function_name}`")
}

fn plan(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<(String, Insertion)> {
    let TypeDiagnosticKind::MissingReturn { function_name } = &diag.kind else {
        return None;
    };
    let func = function_by_name_span(&ctx.analysis.file, diag.span)?;
    let default = default_literal(&func.return_type)?;
    let insertion = insertion(&ctx.analysis.source, func, default)?;
    Some((title(function_name, default), insertion))
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    plan(diag, ctx)
        .map(|(title, _)| FixDescriptor {
            title,
            kind: CodeActionKind::QUICKFIX,
            // The value is a guess; the user must read it before taking it.
            is_preferred: false,
            auto_applicable: false,
        })
        .into_iter()
        .collect()
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, _selected: &str) -> WorkspaceEdit {
    let Some((_, insertion)) = plan(diag, ctx) else {
        debug_assert!(false, "described a missing-return fix with no insertion");
        return WorkspaceEdit::default();
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        ctx.uri.clone(),
        vec![TextEdit {
            range: span_to_range(&ctx.analysis.source, Span::new(insertion.at, insertion.at)),
            new_text: insertion.text,
        }],
    );
    WorkspaceEdit::new(changes)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{apply, count, flags, reparses_clean, titles};

    const FAMILY_NAME: &str = "missing-return";

    fn fixed(source: &str) -> String {
        let out = apply(source, FAMILY_NAME, "Add `return");
        assert!(
            reparses_clean(&out),
            "insertion left unparseable text:\n{out}"
        );
        out
    }

    // ── The default table ─────────────────────────────────────────────────

    #[test]
    fn every_primitive_return_type_gets_its_default() {
        for (return_type, default) in [
            ("int", "0"),
            ("int8", "0"),
            ("int16", "0"),
            ("int32", "0"),
            ("int64", "0"),
            ("uint", "0"),
            ("uint8", "0"),
            ("uint16", "0"),
            ("uint32", "0"),
            ("uint64", "0"),
            ("bool", "false"),
            ("float", "0.0f"),
            ("double", "0.0"),
            ("string", "\"\""),
        ] {
            let source = format!("{return_type} f() {{\n}}");
            assert_eq!(
                titles(&source, FAMILY_NAME),
                vec![format!("Add `return {default};` at the end of `f`")],
                "unexpected action for `{return_type}`"
            );
            assert_eq!(
                fixed(&source),
                format!("{return_type} f() {{\n    return {default};\n}}")
            );
        }
    }

    #[test]
    fn a_handle_return_type_returns_null() {
        for return_type in ["C@", "array<int>@", "const C@"] {
            let source = format!("class C {{}}\n{return_type} f() {{\n}}");
            assert_eq!(
                titles(&source, FAMILY_NAME),
                vec!["Add `return null;` at the end of `f`".to_string()],
                "unexpected action for `{return_type}`"
            );
        }
    }

    #[test]
    fn void_offers_no_action_at_all() {
        // There is no `MissingReturn` for `void`, and there must be no
        // action inventing `return ;`.
        assert_eq!(count("void f() {\n}", FAMILY_NAME), 0);
        assert_eq!(count("void f() {\n    g();\n}", FAMILY_NAME), 0);
    }

    #[test]
    fn return_types_with_no_obvious_default_offer_nothing() {
        // A class value, an array or a `dictionary` has no default this
        // family can name without inventing one, so it names none.
        for source in [
            "class C {}\nC f() {\n}",
            "int[] f() {\n}",
            "array<int> f() {\n}",
            "dictionary f() {\n}",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    // ── Where the return goes ─────────────────────────────────────────────

    #[test]
    fn a_single_line_body_is_completed_in_place() {
        assert_eq!(fixed("int f() { }"), "int f() { return 0; }");
        assert_eq!(fixed("int f() {}"), "int f() { return 0; }");
    }

    #[test]
    fn a_populated_body_keeps_its_statements_and_indentation() {
        assert_eq!(
            fixed("int f() {\n    g();\n}"),
            "int f() {\n    g();\n    return 0;\n}"
        );
    }

    #[test]
    fn strings_and_comments_cannot_masquerade_as_the_body_close_brace() {
        assert_eq!(
            fixed("int f() {\n    string marker = \"}\";\n    // } is not the body close\n}"),
            "int f() {\n    string marker = \"}\";\n    // } is not the body close\n    return 0;\n}"
        );
    }

    #[test]
    fn indentation_follows_the_last_statement_including_tabs() {
        assert_eq!(
            fixed("int f() {\n\t\tg();\n}"),
            "int f() {\n\t\tg();\n\t\treturn 0;\n}"
        );
    }

    #[test]
    fn a_nested_declaration_indents_from_its_own_brace() {
        assert_eq!(
            fixed("class C {\n    int m() {\n    }\n}"),
            "class C {\n    int m() {\n        return 0;\n    }\n}"
        );
        assert_eq!(
            fixed("namespace N {\n    int f() {\n    }\n}"),
            "namespace N {\n    int f() {\n        return 0;\n    }\n}"
        );
    }

    #[test]
    fn a_function_that_returns_on_only_some_paths_is_completed_at_the_fall_through() {
        // The diagnostic fires precisely because control can reach the
        // closing brace, so the closing brace IS the missing path — no
        // branch has to be chosen and nothing is ambiguous.
        assert_eq!(
            fixed("int f(bool c) {\n    if (c) {\n        return 1;\n    }\n}"),
            "int f(bool c) {\n    if (c) {\n        return 1;\n    }\n    return 0;\n}"
        );
    }

    #[test]
    fn a_loop_bodied_function_is_completed_after_the_loop() {
        assert_eq!(
            fixed("int f() {\n    while (true) {\n        g();\n    }\n}"),
            "int f() {\n    while (true) {\n        g();\n    }\n    return 0;\n}"
        );
    }

    // ── Functions that already return offer nothing ───────────────────────

    #[test]
    fn a_function_that_always_returns_offers_nothing() {
        for source in [
            "int f() {\n    return 1;\n}",
            "int f(bool c) {\n    if (c) {\n        return 1;\n    } else {\n        return 2;\n    }\n}",
        ] {
            assert_eq!(count(source, FAMILY_NAME), 0, "expected silence for `{source}`");
        }
    }

    #[test]
    fn a_bodyless_declaration_offers_nothing() {
        // No body means no insertion point — and the checker does not
        // report one either.
        assert_eq!(count("interface I {\n    int m();\n}", FAMILY_NAME), 0);
        assert_eq!(count("import int f() from \"m\";", FAMILY_NAME), 0);
    }

    // ── Metadata ──────────────────────────────────────────────────────────

    #[test]
    fn the_inserted_return_is_neither_preferred_nor_auto_applicable() {
        assert_eq!(
            flags("int f() {\n}", FAMILY_NAME, "Add `return"),
            (false, false)
        );
    }
}
