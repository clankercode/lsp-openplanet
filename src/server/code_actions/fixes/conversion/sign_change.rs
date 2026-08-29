//! `SignChange` — `uint u = -1;`.
//!
//! Two fixes, and the author has to pick:
//!
//! * **Widen the declaration to the signed type** — `int u = -1;`. Keeps the
//!   value, changes the variable's type (and, in a multi-declarator
//!   statement, its siblings' type too).
//! * **Cast the initializer** — `uint u = uint(-1);`. Keeps the type, states
//!   the wrap-around out loud.
//!
//! Neither is preferred and neither is auto-applicable: both change what the
//! program means, and only the author knows which meaning was intended.
//!
//! The checker only fires on a compile-time-known negative literal, so a
//! runtime `int` → `uint` conversion never reaches this family.

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

use super::support::{primitive_type, signed_counterpart, span_text, var_decl_by_init};

pub const FAMILY: FixFamily = FixFamily {
    name: "sign-change",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::SignChange)
}

struct Target<'a> {
    /// The declared unsigned keyword, e.g. `uint` / `uint8`.
    unsigned: &'static str,
    /// Its same-width signed counterpart.
    signed: &'static str,
    /// Span of the type keyword alone, so a leading `const` survives.
    keyword_span: Span,
    /// The initializer text exactly as written.
    init_text: &'a str,
}

fn target<'a>(diag: &TypeDiagnostic, ctx: &'a FixContext<'a>) -> Option<Target<'a>> {
    let (var, _) = var_decl_by_init(&ctx.analysis.file, diag.span.start)?;
    let (unsigned, keyword_span) = primitive_type(&var.type_expr)?;
    Some(Target {
        unsigned,
        signed: signed_counterpart(unsigned)?,
        keyword_span,
        init_text: span_text(&ctx.analysis.source, diag.span)?,
    })
}

fn retype_title(signed: &str) -> String {
    format!("Change the declared type to `{signed}`")
}

fn cast_title(unsigned: &str) -> String {
    format!("Cast the initializer to `{unsigned}`")
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    let Some(target) = target(diag, ctx) else {
        return Vec::new();
    };
    // Two readings of the same code, so neither may claim the "auto fix"
    // keybinding and neither may run on save.
    [retype_title(target.signed), cast_title(target.unsigned)]
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
    let Some(target) = target(diag, ctx) else {
        debug_assert!(false, "described a sign-change fix with no target");
        return WorkspaceEdit::default();
    };
    let source = &ctx.analysis.source;
    let edit = if selected == retype_title(target.signed) {
        TextEdit {
            range: span_to_range(source, target.keyword_span),
            new_text: target.signed.to_string(),
        }
    } else if selected == cast_title(target.unsigned) {
        TextEdit {
            range: span_to_range(source, diag.span),
            new_text: format!("{}({})", target.unsigned, target.init_text),
        }
    } else {
        debug_assert!(false, "unknown sign-change title `{selected}`");
        return WorkspaceEdit::default();
    };
    let mut changes = std::collections::HashMap::new();
    changes.insert(ctx.uri.clone(), vec![edit]);
    WorkspaceEdit::new(changes)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{apply, count, flags, reparses_clean, titles};

    const FAMILY_NAME: &str = "sign-change";

    fn fix_titles(source: &str) -> Vec<String> {
        titles(source, FAMILY_NAME)
    }

    #[test]
    fn negative_literal_into_unsigned_offers_both_readings() {
        assert_eq!(
            fix_titles("void f() { uint u = -1; }"),
            vec![
                "Change the declared type to `int`".to_string(),
                "Cast the initializer to `uint`".to_string(),
            ]
        );
    }

    #[test]
    fn retype_edits_only_the_type_keyword() {
        assert_eq!(
            apply("void f() { uint u = -1; }", FAMILY_NAME, "declared type"),
            "void f() { int u = -1; }"
        );
    }

    #[test]
    fn cast_edits_only_the_initializer() {
        assert_eq!(
            apply("void f() { uint u = -1; }", FAMILY_NAME, "Cast"),
            "void f() { uint u = uint(-1); }"
        );
    }

    #[test]
    fn every_unsigned_width_is_fixable_both_ways() {
        for (unsigned, signed) in [
            ("uint", "int"),
            ("uint8", "int8"),
            ("uint16", "int16"),
            ("uint32", "int32"),
            ("uint64", "int64"),
        ] {
            let source = format!("void f() {{ {unsigned} u = -1; }}");
            assert_eq!(
                fix_titles(&source),
                vec![
                    format!("Change the declared type to `{signed}`"),
                    format!("Cast the initializer to `{unsigned}`"),
                ],
                "unexpected actions for `{unsigned}`"
            );
            assert_eq!(
                apply(&source, FAMILY_NAME, "declared type"),
                format!("void f() {{ {signed} u = -1; }}")
            );
            assert_eq!(
                apply(&source, FAMILY_NAME, "Cast"),
                format!("void f() {{ {unsigned} u = {unsigned}(-1); }}")
            );
        }
    }

    #[test]
    fn negative_hex_literal_is_fixable_too() {
        assert_eq!(
            apply("void f() { uint u = -0x1; }", FAMILY_NAME, "Cast"),
            "void f() { uint u = uint(-0x1); }"
        );
    }

    #[test]
    fn const_declaration_keeps_its_const_when_retyped() {
        let fixed = apply(
            "void f() { const uint u = -1; }",
            FAMILY_NAME,
            "declared type",
        );
        assert_eq!(fixed, "void f() { const int u = -1; }");
        assert!(reparses_clean(&fixed));
    }

    // ── Game-behaviour guards: these must stay silent ─────────────────────

    #[test]
    fn runtime_signed_to_unsigned_conversion_offers_nothing() {
        // The game does NOT warn when the negative value is only known at
        // runtime, so there is no diagnostic and must be no action.
        for source in [
            "void f(int i) { uint u = i; }",
            "void f(int64 i) { uint64 u = i; }",
            "void f(int i) { uint u = -i; }",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    #[test]
    fn signed_targets_and_positive_literals_offer_nothing() {
        for source in [
            "void f() { int i = -1; }",
            "void f() { int64 i = -1; }",
            "void f() { uint u = 1; }",
            "void f() { float x = -1; }",
        ] {
            assert_eq!(
                count(source, FAMILY_NAME),
                0,
                "expected silence for `{source}`"
            );
        }
    }

    // ── Declaration contexts ──────────────────────────────────────────────

    #[test]
    fn locals_are_fixed_in_every_nesting_shape() {
        for source in [
            "class C { void m() { uint u = -1; } }",
            "namespace N { void f() { uint u = -1; } }",
            "void f() { if (true) { uint u = -1; } }",
            "void f() { for (uint i = 0; i < 1; i++) { uint u = -1; } }",
        ] {
            assert_eq!(count(source, FAMILY_NAME), 2, "for `{source}`");
            assert!(reparses_clean(&apply(source, FAMILY_NAME, "Cast")));
        }
    }

    // ── Metadata ──────────────────────────────────────────────────────────

    #[test]
    fn neither_fix_is_preferred_or_auto_applicable() {
        let source = "void f() { uint u = -1; }";
        assert_eq!(flags(source, FAMILY_NAME, "declared type"), (false, false));
        assert_eq!(flags(source, FAMILY_NAME, "Cast"), (false, false));
    }
}
