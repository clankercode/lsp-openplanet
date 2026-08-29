//! Exemplar fix family: `string s` → `const string &in s`.
//!
//! Drives [`TypeDiagnosticKind::StringByValueParam`], the Openplanet sanity
//! check that a bare `string` parameter copies the string. The fix is a
//! single span replacement of the parameter's *type*, so it survives default
//! values (`string s = "x"` keeps its initialiser) and it never touches the
//! parameter name — which matters, because the diagnostic is anchored on the
//! name, and a parameter may legitimately be *called* `string`.
//!
//! The type span is recovered from the token stream rather than by scanning
//! text: tokens are comment- and whitespace-immune, so
//! `void f(string /* why */ s)` is fixed correctly.

use crate::lexer::token::TokenKind;
use crate::lexer::Span;
use crate::server::code_actions::fix::{Fix, FixContext, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};
use tower_lsp::lsp_types::CodeActionKind;

/// What a bare `string` parameter should have been.
const REPLACEMENT: &str = "const string &in";

pub const FAMILY: FixFamily = FixFamily {
    name: "string-by-value-param",
    matches,
    produce,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::StringByValueParam { .. })
}

fn produce(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<Fix> {
    let TypeDiagnosticKind::StringByValueParam { param_name } = &diag.kind else {
        return Vec::new();
    };
    let Some(type_span) = param_type_span(&ctx.analysis.tokens, diag.span) else {
        return Vec::new();
    };
    let title = if param_name.is_empty() {
        format!("Pass the string by reference (`{}`)", REPLACEMENT)
    } else {
        format!("Pass `{}` by reference (`{}`)", param_name, REPLACEMENT)
    };
    vec![Fix {
        title,
        kind: CodeActionKind::QUICKFIX,
        edits: vec![tower_lsp::lsp_types::TextEdit {
            range: span_to_range(&ctx.analysis.source, type_span),
            new_text: REPLACEMENT.to_string(),
        }],
        // Unambiguous and lossless: the game compiler asks for exactly this
        // spelling, and `const string &in` accepts every argument the
        // by-value form did.
        is_preferred: true,
        auto_applicable: true,
    }]
}

/// Span of the parameter's type, given the span the diagnostic is anchored
/// on (the parameter *name*, or the type itself for an unnamed parameter).
///
/// Returns `None` rather than guessing whenever the tokens do not have the
/// exact shape `[const] string <name>` — a wrong edit here silently rewrites
/// a signature.
fn param_type_span(tokens: &[crate::lexer::Token], diag_span: Span) -> Option<Span> {
    let idx = tokens
        .iter()
        .position(|t| t.span.start == diag_span.start)?;
    match tokens[idx].kind {
        // Unnamed parameter: the checker anchored on the type expression,
        // which already covers a leading `const`.
        TokenKind::KwString | TokenKind::KwConst => Some(diag_span),
        // Named parameter: the type is the token(s) immediately before it.
        _ => {
            let string_idx = idx.checked_sub(1)?;
            if tokens[string_idx].kind != TokenKind::KwString {
                return None;
            }
            let start = match string_idx.checked_sub(1) {
                Some(i) if tokens[i].kind == TokenKind::KwConst => tokens[i].span.start,
                _ => tokens[string_idx].span.start,
            };
            Some(Span {
                start,
                end: tokens[string_idx].span.end,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::code_actions::fix::collect_fixes;
    use crate::server::code_actions::test_fixtures::{apply_fixes, ctx_for};

    /// The full text after applying every fix this family offers for
    /// `source` — the readable form of "did the edit land correctly?".
    fn fixed(source: &str) -> String {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
        };
        let candidates = collect_fixes(&ctx);
        apply_fixes(source, &candidates)
    }

    fn fix_count(source: &str) -> usize {
        let (analysis, workspace, uri) = ctx_for(source);
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
        };
        collect_fixes(&ctx).len()
    }

    #[test]
    fn bare_string_param_is_fixed() {
        assert_eq!(
            fixed("void f(string s) {}"),
            "void f(const string &in s) {}"
        );
    }

    #[test]
    fn const_string_by_value_param_is_fixed() {
        assert_eq!(
            fixed("void f(const string s) {}"),
            "void f(const string &in s) {}"
        );
    }

    #[test]
    fn unnamed_string_param_is_fixed() {
        assert_eq!(fixed("void f(string) {}"), "void f(const string &in) {}");
    }

    // ── Parameter shapes that are already correct: offer nothing ──────────

    #[test]
    fn already_correct_and_reference_shapes_offer_no_fix() {
        for src in [
            "void f(const string &in s) {}",
            "void f(string &in s) {}",
            "void f(string &out s) {}",
            "void f(string &inout s) {}",
            "void f(string@ s) {}",
            "void f(const string@ s) {}",
            "void f(string@ &in s) {}",
            "void f(const string@ &in s) {}",
            "void f(string[] s) {}",
            "void f(string[]@ s) {}",
        ] {
            assert_eq!(fix_count(src), 0, "expected no fix for `{}`", src);
        }
    }

    #[test]
    fn underscore_prefixed_param_offers_no_fix() {
        // Openplanet's documented opt-out.
        assert_eq!(fix_count("void f(string _s) {}"), 0);
        assert_eq!(fix_count("void f(const string _s) {}"), 0);
    }

    #[test]
    fn container_types_are_not_matched() {
        // Guard against a naive substring match on "string".
        for src in [
            "void f(array<string> xs) {}",
            "void f(dictionary d) {}",
            "void f(array<string>@ xs) {}",
            "void f(dictionary@ d) {}",
        ] {
            assert_eq!(fix_count(src), 0, "expected no fix for `{}`", src);
        }
    }

    #[test]
    fn non_string_params_are_not_matched() {
        assert_eq!(fix_count("void f(int s) {}"), 0);
        assert_eq!(fix_count("void f(uint64 s) {}"), 0);
    }

    // ── Trickier shapes ───────────────────────────────────────────────────

    #[test]
    fn default_value_is_preserved_and_only_the_type_is_edited() {
        assert_eq!(
            fixed(r#"void f(string s = "x") {}"#),
            r#"void f(const string &in s = "x") {}"#
        );
    }

    #[test]
    fn a_name_containing_string_is_never_mistaken_for_the_type() {
        // The diagnostic is anchored on the *name*, so a producer that
        // searched text for "string" could edit the wrong span.
        assert_eq!(
            fixed("void f(string myString) {}"),
            "void f(const string &in myString) {}"
        );
        assert_eq!(fix_count("void f(int myString) {}"), 0);
        assert_eq!(fix_count("void f(int stringy) {}"), 0);
    }

    #[test]
    fn a_param_literally_named_string_is_not_valid_angelscript() {
        // `string` is a reserved keyword, so `void f(string string)` cannot
        // parse — there is no diagnostic to fix and we must not invent one.
        assert_eq!(fix_count("void f(string string) {}"), 0);
    }

    #[test]
    fn comment_between_type_and_name_is_handled() {
        assert_eq!(
            fixed("void f(string /* why */ s) {}"),
            "void f(const string &in /* why */ s) {}"
        );
    }

    #[test]
    fn multiple_string_params_are_all_fixed_without_corrupting_offsets() {
        assert_eq!(fix_count("void f(string a, string b, string c) {}"), 3);
        assert_eq!(
            fixed("void f(string a, string b, string c) {}"),
            "void f(const string &in a, const string &in b, const string &in c) {}"
        );
    }

    #[test]
    fn mixed_params_fix_only_the_offending_ones() {
        assert_eq!(
            fixed("void f(int n, string a, string &in b, string c) {}"),
            "void f(int n, const string &in a, string &in b, const string &in c) {}"
        );
    }

    // ── Declaration contexts ──────────────────────────────────────────────

    #[test]
    fn class_method_param_is_fixed() {
        assert_eq!(
            fixed("class C { void M(string s) {} }"),
            "class C { void M(const string &in s) {} }"
        );
    }

    #[test]
    fn namespaced_function_param_is_fixed() {
        assert_eq!(
            fixed("namespace N { void f(string s) {} }"),
            "namespace N { void f(const string &in s) {} }"
        );
    }

    #[test]
    fn interface_method_param_is_fixed() {
        assert_eq!(
            fixed("interface I { void M(string s); }"),
            "interface I { void M(const string &in s); }"
        );
    }

    #[test]
    fn shared_function_param_is_fixed() {
        assert_eq!(
            fixed("shared void f(string s) {}"),
            "shared void f(const string &in s) {}"
        );
    }

    #[test]
    fn funcdef_params_are_not_checked_today_so_no_fix_is_offered() {
        // The checker does not walk `funcdef` parameter lists, so there is
        // no `StringByValueParam` diagnostic to answer. Documented rather
        // than fixed: widening the checker would change published
        // diagnostics, which is out of scope for the seam (B035).
        assert_eq!(fix_count("funcdef void Cb(string s);"), 0);
    }

    #[test]
    fn multiline_declaration_is_fixed() {
        let src = "void f(\n    string s,\n    int n\n) {}";
        assert_eq!(
            fixed(src),
            "void f(\n    const string &in s,\n    int n\n) {}"
        );
    }

    #[test]
    fn fix_is_preferred_and_auto_applicable() {
        let (analysis, workspace, uri) = ctx_for("void f(string s) {}");
        let ctx = FixContext {
            uri: &uri,
            analysis: &analysis,
            workspace: &workspace,
            type_index: None,
        };
        let candidates = collect_fixes(&ctx);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].family, "string-by-value-param");
        assert!(candidates[0].fix.is_preferred);
        assert!(candidates[0].fix.auto_applicable);
        assert_eq!(candidates[0].fix.kind, CodeActionKind::QUICKFIX);
        assert!(candidates[0].fix.title.contains("const string &in"));
    }
}
