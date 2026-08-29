//! `SignedUnsignedMismatch` — `if (i < n)` with `int i` and `uint n`.
//!
//! The fix is a cast, but on *which* side is exactly the question the
//! warning asks. So both are offered as separate, distinguishable actions:
//! cast the left operand to the right's type, or the right to the left's.
//!
//! **Neither is auto-applicable.** The two edits touch different spans, so
//! they do not overlap, so `source.fixAll.openplanet`'s batcher would take
//! BOTH and write `uint(i) < int(n)` — a comparison nobody asked for. Even
//! one of them alone is a judgement call: only one direction matches the
//! conversion the game already performs, and which one that is has not been
//! probed against the game compiler. A save-time action must not guess at
//! that, so both stay explicit quick fixes. (B037 note for the coordinator:
//! flipping one of these to `auto_applicable` is a one-word change here once
//! the game's conversion direction is confirmed.)
//!
//! Operand types come from a second, type-recording checker pass. The
//! checker types both operands while producing this diagnostic, so the
//! lookup always hits; the cost is one extra pass per offered mismatch,
//! which is acceptable for a diagnostic this rare.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{
    Checker, GlobalScope, PrimitiveType, TypeDiagnostic, TypeDiagnosticKind, TypeRepr,
};

use super::support::{abbreviate, binary_by_span, span_text};

pub const FAMILY: FixFamily = FixFamily {
    name: "signed-unsigned-mismatch",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::SignedUnsignedMismatch)
}

/// One side of the comparison: what to replace, and what to wrap it in.
struct Side<'a> {
    span: Span,
    text: &'a str,
    /// The *other* operand's type — what this side is cast to.
    cast_to: &'static str,
}

struct Operands<'a> {
    left: Side<'a>,
    right: Side<'a>,
}

fn operands<'a>(diag: &TypeDiagnostic, ctx: &'a FixContext<'a>) -> Option<Operands<'a>> {
    let (lhs, rhs) = binary_by_span(&ctx.analysis.file, diag.span)?;
    let (left_ty, right_ty) = operand_primitives(ctx, lhs.span, rhs.span)?;
    // Belt and braces: the checker only emits this on a signed/unsigned
    // mix, but a fix that named the wrong cast target would compile and
    // mean something else.
    if signedness(left_ty)? == signedness(right_ty)? {
        return None;
    }
    let source = &ctx.analysis.source;
    Some(Operands {
        left: Side {
            span: lhs.span,
            text: span_text(source, lhs.span)?,
            cast_to: right_ty.as_str(),
        },
        right: Side {
            span: rhs.span,
            text: span_text(source, rhs.span)?,
            cast_to: left_ty.as_str(),
        },
    })
}

fn operand_primitives(
    ctx: &FixContext<'_>,
    lhs: Span,
    rhs: Span,
) -> Option<(PrimitiveType, PrimitiveType)> {
    let scope = GlobalScope::new(ctx.workspace, ctx.type_index);
    let mut checker = Checker::new(ctx.analysis.masked_source(), &scope).with_type_recording();
    checker.check_file(&ctx.analysis.file);
    Some((primitive_at(&checker, lhs)?, primitive_at(&checker, rhs)?))
}

fn primitive_at(checker: &Checker<'_>, span: Span) -> Option<PrimitiveType> {
    match checker
        .type_at_span_range(span.start, span.end)?
        .unwrap_const()
    {
        TypeRepr::Primitive(p) => Some(*p),
        _ => None,
    }
}

/// `Some(true)` signed, `Some(false)` unsigned, `None` for anything else.
fn signedness(p: PrimitiveType) -> Option<bool> {
    match p {
        PrimitiveType::Int8 | PrimitiveType::Int16 | PrimitiveType::Int | PrimitiveType::Int64 => {
            Some(true)
        }
        PrimitiveType::Uint8
        | PrimitiveType::Uint16
        | PrimitiveType::Uint
        | PrimitiveType::Uint64 => Some(false),
        _ => None,
    }
}

fn title(hand: &str, side: &Side<'_>) -> String {
    format!(
        "Cast the {hand} operand `{}` to `{}`",
        abbreviate(side.text, 24),
        side.cast_to
    )
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    let Some(operands) = operands(diag, ctx) else {
        return Vec::new();
    };
    [
        title("left", &operands.left),
        title("right", &operands.right),
    ]
    .into_iter()
    .map(|title| FixDescriptor {
        title,
        kind: CodeActionKind::QUICKFIX,
        is_preferred: None,
        auto_applicable: false,
    })
    .collect()
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, selected: &str) -> WorkspaceEdit {
    let Some(operands) = operands(diag, ctx) else {
        debug_assert!(false, "described a signed/unsigned fix with no operands");
        return WorkspaceEdit::default();
    };
    let side = if selected == title("left", &operands.left) {
        &operands.left
    } else if selected == title("right", &operands.right) {
        &operands.right
    } else {
        debug_assert!(false, "unknown signed/unsigned title `{selected}`");
        return WorkspaceEdit::default();
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        ctx.uri.clone(),
        vec![TextEdit {
            range: span_to_range(&ctx.analysis.source, side.span),
            new_text: format!("{}({})", side.cast_to, side.text),
        }],
    );
    WorkspaceEdit::new(changes)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{apply, count, flags, reparses_clean, titles};

    const FAMILY_NAME: &str = "signed-unsigned-mismatch";

    fn fix_titles(source: &str) -> Vec<String> {
        titles(source, FAMILY_NAME)
    }

    #[test]
    fn relational_mix_offers_a_cast_on_each_side() {
        assert_eq!(
            fix_titles("void f(int i, uint n) { if (i < n) {} }"),
            vec![
                "Cast the left operand `i` to `uint`".to_string(),
                "Cast the right operand `n` to `int`".to_string(),
            ]
        );
    }

    #[test]
    fn each_side_casts_only_itself() {
        let source = "void f(int i, uint n) { if (i < n) {} }";
        assert_eq!(
            apply(source, FAMILY_NAME, "left"),
            "void f(int i, uint n) { if (uint(i) < n) {} }"
        );
        assert_eq!(
            apply(source, FAMILY_NAME, "right"),
            "void f(int i, uint n) { if (i < int(n)) {} }"
        );
    }

    #[test]
    fn every_relational_operator_is_fixable() {
        for op in ["<", "<=", ">", ">="] {
            let source = format!("void f(int i, uint n) {{ if (i {op} n) {{}} }}");
            assert_eq!(count(&source, FAMILY_NAME), 2, "for `{op}`");
            assert_eq!(
                apply(&source, FAMILY_NAME, "left"),
                format!("void f(int i, uint n) {{ if (uint(i) {op} n) {{}} }}")
            );
            assert_eq!(
                apply(&source, FAMILY_NAME, "right"),
                format!("void f(int i, uint n) {{ if (i {op} int(n)) {{}} }}")
            );
        }
    }

    #[test]
    fn the_arithmetic_mix_variant_is_fixable_too() {
        // The checker also warns on a top-level `+` across signedness.
        let source = "void f(int i, uint n) { int x = i + n; }";
        assert_eq!(count(source, FAMILY_NAME), 2);
        assert_eq!(
            apply(source, FAMILY_NAME, "left"),
            "void f(int i, uint n) { int x = uint(i) + n; }"
        );
    }

    #[test]
    fn cast_target_follows_the_other_operand_width() {
        let source = "void f(int64 i, uint8 n) { if (i < n) {} }";
        assert_eq!(
            fix_titles(source),
            vec![
                "Cast the left operand `i` to `uint8`".to_string(),
                "Cast the right operand `n` to `int64`".to_string(),
            ]
        );
        assert_eq!(
            apply(source, FAMILY_NAME, "left"),
            "void f(int64 i, uint8 n) { if (uint8(i) < n) {} }"
        );
    }

    #[test]
    fn unsigned_on_the_left_is_handled_symmetrically() {
        let source = "void f(uint n, int i) { if (n < i) {} }";
        assert_eq!(
            fix_titles(source),
            vec![
                "Cast the left operand `n` to `int`".to_string(),
                "Cast the right operand `i` to `uint`".to_string(),
            ]
        );
    }

    #[test]
    fn compound_operands_are_wrapped_whole_and_still_parse() {
        for (source, fixed) in [
            (
                "uint gu() { return 1; } void f(int i) { if (i < gu()) {} }",
                "uint gu() { return 1; } void f(int i) { if (i < int(gu())) {} }",
            ),
            (
                "class C { uint x; } void f(int i, C@ c) { if (i < c.x) {} }",
                "class C { uint x; } void f(int i, C@ c) { if (i < int(c.x)) {} }",
            ),
        ] {
            let out = apply(source, FAMILY_NAME, "right");
            assert_eq!(out, fixed);
            assert!(reparses_clean(&out), "cast left unparseable text:\n{out}");
        }
    }

    #[test]
    fn parenthesised_operands_are_cast_inside_their_parens() {
        // The parser gives both a parenthesised operand and the comparison
        // that contains one a span that stops short of the closing paren.
        // The operand spans stay exact, so the cast still lands correctly —
        // assert it, because it is not obvious from the spans alone.
        for (source, fixed) in [
            (
                "void f(int i, uint n) { if (i < (n)) {} }",
                "void f(int i, uint n) { if (i < (int(n))) {} }",
            ),
            (
                "void f(int i, uint n) { if ((i) < n) {} }",
                "void f(int i, uint n) { if ((i) < int(n)) {} }",
            ),
        ] {
            let out = apply(source, FAMILY_NAME, "right");
            assert_eq!(out, fixed);
            assert!(reparses_clean(&out), "cast left unparseable text:\n{out}");
        }
    }

    #[test]
    fn a_binary_operand_is_not_diagnosed_today() {
        // Documented, not fixed: the checker does not type `n + m`, so its
        // signedness is unknown and `i < n + m` produces no diagnostic.
        assert_eq!(
            count(
                "void f(int i, uint n, uint m) { if (i < n + m) {} }",
                FAMILY_NAME
            ),
            0
        );
    }

    // ── Current-behaviour guards ──────────────────────────────────────────

    #[test]
    fn equality_operators_are_not_diagnosed_so_no_action_is_offered() {
        // Confirmed against the checker: only `<`, `<=`, `>`, `>=` (and a
        // top-level `+`) warn. `==` / `!=` stay silent, so must we.
        for op in ["==", "!="] {
            let source = format!("void f(int i, uint n) {{ if (i {op} n) {{}} }}");
            assert_eq!(
                count(&source, FAMILY_NAME),
                0,
                "expected silence for `{op}`"
            );
        }
    }

    #[test]
    fn non_primitive_and_unknown_operands_offer_nothing() {
        for source in [
            "void f(string a, string b) { if (a < b) {} }",
            "void f(int i, int j) { if (i < j) {} }",
            "void f(uint a, uint b) { if (a < b) {} }",
            "void f(float a, uint b) { if (a < b) {} }",
            "void f(int i) { if (i < unknownThing) {} }",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    #[test]
    fn explicit_conversions_and_literals_stay_silent() {
        // Game-probed exemptions the checker already honours; the fix
        // family must not resurrect an action for them.
        for source in [
            "void f(uint n) { if (n < int(3)) {} }",
            "void f(uint n) { if (n < 3) {} }",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    // ── Metadata ──────────────────────────────────────────────────────────

    #[test]
    fn neither_side_is_preferred_or_auto_applicable() {
        // Both edits are disjoint, so an auto-applicable pair would batch
        // into `uint(i) < int(n)`. Neither may run on save.
        let source = "void f(int i, uint n) { if (i < n) {} }";
        assert_eq!(flags(source, FAMILY_NAME, "left"), (false, false));
        assert_eq!(flags(source, FAMILY_NAME, "right"), (false, false));
    }
}
