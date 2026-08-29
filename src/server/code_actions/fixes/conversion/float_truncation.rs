//! `FloatTruncation` — `int ms = 3.7;` / `int g = f;`.
//!
//! Two fixes, and which are offered depends on the diagnostic's `literal`
//! flag:
//!
//! * **Cast** (always) — `int ms = int(3.7);`. Behaviour-preserving: the
//!   game already truncates here, this only says so out loud. Preferred and
//!   auto-applicable.
//! * **Rewrite the literal** (`literal: true` only) — `int ms = 3;`. This
//!   changes the stored VALUE, so it is neither preferred nor batched into
//!   `source.fixAll.openplanet`.
//!
//! The cast wraps the whole initializer through the construction call's own
//! parentheses (`int(a + b)`), so precedence cannot be lost.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::parser::ast::ExprKind;
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

use super::support::{abbreviate, is_integer_keyword, primitive_type, span_text, var_decl_by_init};

pub const FAMILY: FixFamily = FixFamily {
    name: "float-truncation",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::FloatTruncation { .. })
}

/// Everything both callbacks need, or `None` when the initializer cannot be
/// traced back to an integer-typed declaration. Establishing this in
/// `describe` is what keeps `materialize` total.
struct Target<'a> {
    /// The declared integer keyword, e.g. `int` / `uint8`.
    keyword: &'static str,
    /// The initializer text exactly as written.
    init_text: &'a str,
}

fn target<'a>(diag: &TypeDiagnostic, ctx: &'a FixContext<'a>) -> Option<Target<'a>> {
    let (var, _) = var_decl_by_init(&ctx.analysis.file, diag.span.start)?;
    let (keyword, _) = primitive_type(&var.type_expr)?;
    if !is_integer_keyword(keyword) {
        return None;
    }
    Some(Target {
        keyword,
        init_text: span_text(&ctx.analysis.source, diag.span)?,
    })
}

fn cast_title(keyword: &str) -> String {
    format!("Cast the initializer to `{keyword}`")
}

/// The integral literal this float literal truncates to, as source text.
///
/// `None` whenever the rewrite would not be a faithful truncation: a
/// non-literal initializer, a non-finite value, or one that does not survive
/// the trip through `i64` (`int n = 1e30;`). Refusing beats emitting a
/// literal whose value differs from the one the game would store.
fn truncated_literal(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Option<String> {
    if !matches!(
        diag.kind,
        TypeDiagnosticKind::FloatTruncation { literal: true }
    ) {
        return None;
    }
    let (_, declarator) = var_decl_by_init(&ctx.analysis.file, diag.span.start)?;
    let ExprKind::FloatLit(value) = declarator.init.as_ref()?.kind else {
        return None;
    };
    let truncated = value.trunc();
    if !truncated.is_finite() || truncated < i64::MIN as f64 || truncated > i64::MAX as f64 {
        return None;
    }
    Some((truncated as i64).to_string())
}

fn literal_title(original: &str, replacement: &str) -> String {
    format!(
        "Change the literal `{}` to `{replacement}`",
        abbreviate(original, 24)
    )
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    let Some(target) = target(diag, ctx) else {
        return Vec::new();
    };
    let mut out = vec![FixDescriptor {
        title: cast_title(target.keyword),
        kind: CodeActionKind::QUICKFIX,
        // The cast is what the game already does implicitly, so it is both
        // the right default and safe to apply on save.
        is_preferred: true,
        auto_applicable: true,
    }];
    if let Some(replacement) = truncated_literal(diag, ctx) {
        out.push(FixDescriptor {
            title: literal_title(target.init_text, &replacement),
            kind: CodeActionKind::QUICKFIX,
            // Silently changes the value the program stores.
            is_preferred: false,
            auto_applicable: false,
        });
    }
    out
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, selected: &str) -> WorkspaceEdit {
    let Some(target) = target(diag, ctx) else {
        debug_assert!(false, "described a float-truncation fix with no target");
        return WorkspaceEdit::default();
    };
    let new_text = if selected == cast_title(target.keyword) {
        format!("{}({})", target.keyword, target.init_text)
    } else {
        match truncated_literal(diag, ctx) {
            Some(replacement) => replacement,
            None => {
                debug_assert!(false, "unknown float-truncation title `{selected}`");
                return WorkspaceEdit::default();
            }
        }
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        ctx.uri.clone(),
        vec![TextEdit {
            range: span_to_range(&ctx.analysis.source, diag.span),
            new_text,
        }],
    );
    WorkspaceEdit::new(changes)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{apply, count, flags, reparses_clean, titles};

    const FAMILY_NAME: &str = "float-truncation";

    fn fix_titles(source: &str) -> Vec<String> {
        titles(source, FAMILY_NAME)
    }

    // ── Literal vs non-literal offer DIFFERENT action sets ────────────────

    #[test]
    fn literal_initializer_offers_a_cast_and_a_literal_rewrite() {
        assert_eq!(
            fix_titles("void f() { int ms = 3.7; }"),
            vec![
                "Cast the initializer to `int`".to_string(),
                "Change the literal `3.7` to `3`".to_string(),
            ]
        );
    }

    #[test]
    fn non_literal_initializer_offers_only_the_cast() {
        assert_eq!(
            fix_titles("void g(float x) { int n = x; }"),
            vec!["Cast the initializer to `int`".to_string()]
        );
    }

    #[test]
    fn cast_wraps_the_initializer() {
        assert_eq!(
            apply("void f() { int ms = 3.7; }", FAMILY_NAME, "Cast"),
            "void f() { int ms = int(3.7); }"
        );
        assert_eq!(
            apply("void g(float x) { int n = x; }", FAMILY_NAME, "Cast"),
            "void g(float x) { int n = int(x); }"
        );
    }

    #[test]
    fn literal_rewrite_truncates_toward_zero() {
        assert_eq!(
            apply(
                "void f() { int ms = 3.7; }",
                FAMILY_NAME,
                "Change the literal"
            ),
            "void f() { int ms = 3; }"
        );
    }

    // ── Every integer target width is offered ─────────────────────────────

    #[test]
    fn every_integer_target_width_is_fixable() {
        for keyword in [
            "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
        ] {
            let source = format!("void f() {{ {keyword} n = 3.7; }}");
            assert_eq!(
                fix_titles(&source),
                vec![
                    format!("Cast the initializer to `{keyword}`"),
                    "Change the literal `3.7` to `3`".to_string(),
                ],
                "unexpected actions for `{keyword}`"
            );
            assert_eq!(
                apply(&source, FAMILY_NAME, "Cast"),
                format!("void f() {{ {keyword} n = {keyword}(3.7); }}")
            );
        }
    }

    #[test]
    fn non_literal_source_is_fixable_for_every_width() {
        for keyword in [
            "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
        ] {
            let source = format!("void g(float x) {{ {keyword} n = x; }}");
            assert_eq!(count(&source, FAMILY_NAME), 1, "for `{keyword}`");
            assert_eq!(
                apply(&source, FAMILY_NAME, "Cast"),
                format!("void g(float x) {{ {keyword} n = {keyword}(x); }}")
            );
        }
    }

    #[test]
    fn double_source_truncates_the_same_way() {
        assert_eq!(
            apply("void g(double x) { int n = x; }", FAMILY_NAME, "Cast"),
            "void g(double x) { int n = int(x); }"
        );
    }

    // ── Regression guard: float/double targets are legal ──────────────────

    #[test]
    fn float_and_double_targets_offer_nothing() {
        for source in [
            "void f() { float x = 3.7; }",
            "void f() { double x = 3.7; }",
            "void g(double x) { float y = x; }",
            "void g(float x) { double y = x; }",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    #[test]
    fn integer_to_integer_initializers_offer_nothing() {
        for source in [
            "void f() { int n = 1; }",
            "void g(int x) { int64 n = x; }",
            "void f() { auto n = 3.7; }",
            "void f() { bool b = true; }",
            "void f() { string s = \"x\"; }",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    // ── Trickier initializers ─────────────────────────────────────────────

    #[test]
    fn complex_initializers_are_wrapped_whole_and_still_parse() {
        for (source, fixed) in [
            // Ternary: the classic precedence trap — an unparenthesised
            // wrap would bind the cast to the first branch only.
            (
                "void g(bool c, float a, float b) { int n = c ? a : b; }",
                "void g(bool c, float a, float b) { int n = int(c ? a : b); }",
            ),
            (
                "float gf() { return 1.0f; } void g() { int n = gf(); }",
                "float gf() { return 1.0f; } void g() { int n = int(gf()); }",
            ),
            (
                "void g(float a) { int n = -a; }",
                "void g(float a) { int n = int(-a); }",
            ),
            (
                "void g(float[] a) { int n = a[0]; }",
                "void g(float[] a) { int n = int(a[0]); }",
            ),
            (
                "class C { float x; } void g(C@ c) { int n = c.x; }",
                "class C { float x; } void g(C@ c) { int n = int(c.x); }",
            ),
        ] {
            let out = apply(source, FAMILY_NAME, "Cast");
            assert_eq!(out, fixed);
            assert!(reparses_clean(&out), "cast left unparseable text:\n{out}");
        }
    }

    #[test]
    fn an_already_parenthesised_initializer_keeps_its_parens() {
        // The parser gives a parenthesised expression its INNER span, so
        // the cast lands inside the existing parens. Still correct, still
        // parses — assert it rather than leave it to chance.
        let out = apply("void f() { int n = (3.7); }", FAMILY_NAME, "Cast");
        assert_eq!(out, "void f() { int n = (int(3.7)); }");
        assert!(reparses_clean(&out));
    }

    #[test]
    fn a_binary_arithmetic_initializer_is_not_diagnosed_today() {
        // Documented, not fixed: the checker does not type `a + b`, so
        // `int n = a + b;` produces no FloatTruncation and there is
        // nothing for this family to answer. Widening the checker would
        // change published diagnostics, which is out of scope for B037.
        assert_eq!(
            count("void g(float a, float b) { int n = a + b; }", FAMILY_NAME),
            0
        );
    }

    #[test]
    fn const_target_keeps_its_const_and_casts_to_the_bare_type() {
        assert_eq!(
            apply("void f() { const int n = 3.7; }", FAMILY_NAME, "Cast"),
            "void f() { const int n = int(3.7); }"
        );
    }

    #[test]
    fn f_suffixed_literal_is_rewritten_to_a_bare_integer() {
        assert_eq!(
            apply(
                "void f() { int n = 3.7f; }",
                FAMILY_NAME,
                "Change the literal"
            ),
            "void f() { int n = 3; }"
        );
    }

    #[test]
    fn exponent_literal_is_rewritten_to_its_integral_value() {
        assert_eq!(
            apply(
                "void f() { int n = 1e3; }",
                FAMILY_NAME,
                "Change the literal"
            ),
            "void f() { int n = 1000; }"
        );
    }

    #[test]
    fn out_of_range_literal_withholds_the_rewrite_but_keeps_the_cast() {
        // 1e30 does not survive the trip through i64, so a "truncated"
        // literal would be a different number. Only the cast is offered.
        assert_eq!(
            fix_titles("void f() { int n = 1e30; }"),
            vec!["Cast the initializer to `int`".to_string()]
        );
    }

    #[test]
    fn each_declarator_in_a_multi_declarator_statement_is_fixed_independently() {
        assert_eq!(count("void f() { int a = 3.7, b = 4.2; }", FAMILY_NAME), 4);
        assert_eq!(
            apply("void f() { int a = 3.7, b = 4.2; }", FAMILY_NAME, "`3.7`"),
            "void f() { int a = 3, b = 4.2; }"
        );
    }

    // ── Declaration contexts ──────────────────────────────────────────────

    #[test]
    fn locals_are_fixed_in_every_nesting_shape() {
        for (source, fixed) in [
            (
                "class C { void m() { int n = 3.7; } }",
                "class C { void m() { int n = int(3.7); } }",
            ),
            (
                "namespace N { void f() { int n = 3.7; } }",
                "namespace N { void f() { int n = int(3.7); } }",
            ),
            (
                "void f() { if (true) { int n = 3.7; } }",
                "void f() { if (true) { int n = int(3.7); } }",
            ),
            (
                "void f() { while (true) { int n = 3.7; } }",
                "void f() { while (true) { int n = int(3.7); } }",
            ),
        ] {
            assert_eq!(apply(source, FAMILY_NAME, "Cast"), fixed);
        }
    }

    // ── Metadata ──────────────────────────────────────────────────────────

    #[test]
    fn cast_is_preferred_and_auto_applicable_but_the_literal_rewrite_is_neither() {
        let source = "void f() { int ms = 3.7; }";
        assert_eq!(flags(source, FAMILY_NAME, "Cast"), (true, true));
        assert_eq!(
            flags(source, FAMILY_NAME, "Change the literal"),
            (false, false)
        );
    }
}
