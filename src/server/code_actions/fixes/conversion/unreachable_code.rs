//! `UnreachableCode` — statements after a `return` / `break` / `continue`.
//!
//! The checker warns once per block, on the FIRST dead statement, then stops
//! walking. So the fix is not "delete this statement" but "delete the run
//! from here to the end of the block": deleting only the anchor would leave
//! the rest of the dead code behind and the warning would simply move.
//!
//! Not auto-applicable. Deleting code on save is not a tidy-up, however dead
//! the compiler thinks the code is — a half-written branch is unreachable
//! too. It is also not `is_preferred`: the "auto fix" keybinding should not
//! be a shortcut for erasing lines.
//!
//! (AngelScript as parsed here has no `throw` statement, so the terminators
//! are exactly `return`, `break`, `continue` and the block/if/switch/
//! try-catch shapes that reduce to them.)

use tower_lsp::lsp_types::{CodeActionKind, TextEdit, WorkspaceEdit};

use crate::lexer::Span;
use crate::parser::ast::Stmt;
use crate::server::code_actions::fix::{FixContext, FixDescriptor, FixFamily};
use crate::server::diagnostics::span_to_range;
use crate::typecheck::{TypeDiagnostic, TypeDiagnosticKind};

use super::support::dead_tail;

pub const FAMILY: FixFamily = FixFamily {
    name: "unreachable-code",
    matches,
    describe,
    materialize,
};

fn matches(kind: &TypeDiagnosticKind) -> bool {
    matches!(kind, TypeDiagnosticKind::UnreachableCode)
}

fn title(count: usize) -> String {
    if count == 1 {
        "Delete unreachable code (1 statement)".to_string()
    } else {
        format!("Delete unreachable code ({count} statements)")
    }
}

/// What to delete for a dead run: the statements themselves, plus their own
/// indentation and line break when they occupy whole lines. Trimming the
/// line is what stops the fix leaving a ladder of blank indented rows;
/// leaving it alone when the run shares a line with live code is what stops
/// it swallowing that code's line break.
fn deletion_span(source: &str, tail: &[Stmt]) -> Span {
    let first = tail[0].span.start as usize;
    let last = tail[tail.len() - 1].span.end as usize;
    let line_start = source[..first.min(source.len())]
        .rfind('\n')
        .map_or(0, |nl| nl + 1);
    let own_line = source
        .get(line_start..first)
        .is_some_and(|indent| indent.trim().is_empty());
    if !own_line {
        return Span::new(first as u32, last as u32);
    }
    let bytes = source.as_bytes();
    let mut end = last;
    while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t' || bytes[end] == b'\r') {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
    } else {
        // No trailing newline to absorb (end of file): keep the statements'
        // own bounds so nothing after them shifts onto the wrong line.
        end = last;
    }
    Span::new(line_start as u32, end as u32)
}

fn describe(diag: &TypeDiagnostic, ctx: &FixContext<'_>) -> Vec<FixDescriptor> {
    // `materialize` indexes the tail, so establish it exists here.
    let Some(tail) = dead_tail(&ctx.analysis.file, diag.span.start) else {
        return Vec::new();
    };
    vec![FixDescriptor {
        title: title(tail.len()),
        kind: CodeActionKind::QUICKFIX,
        is_preferred: false,
        auto_applicable: false,
    }]
}

fn materialize(diag: &TypeDiagnostic, ctx: &FixContext<'_>, _selected: &str) -> WorkspaceEdit {
    let Some(tail) = dead_tail(&ctx.analysis.file, diag.span.start) else {
        debug_assert!(false, "described an unreachable-code fix with no dead tail");
        return WorkspaceEdit::default();
    };
    let source = &ctx.analysis.source;
    let mut changes = std::collections::HashMap::new();
    changes.insert(
        ctx.uri.clone(),
        vec![TextEdit {
            range: span_to_range(source, deletion_span(source, tail)),
            new_text: String::new(),
        }],
    );
    WorkspaceEdit::new(changes)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{apply, apply_all, count, flags, reparses_clean, titles};

    const FAMILY_NAME: &str = "unreachable-code";

    fn fix_titles(source: &str) -> Vec<String> {
        titles(source, FAMILY_NAME)
    }

    fn fixed(source: &str) -> String {
        let out = apply(source, FAMILY_NAME, "Delete unreachable code");
        assert!(
            reparses_clean(&out),
            "deletion left unparseable text:\n{out}"
        );
        out
    }

    // ── Every terminator ──────────────────────────────────────────────────

    #[test]
    fn dead_tail_after_return_is_deleted() {
        assert_eq!(
            fixed("void f() {\n    return;\n    int x = 1;\n}"),
            "void f() {\n    return;\n}"
        );
    }

    #[test]
    fn dead_tail_with_strings_and_comments_is_deleted_whole() {
        assert_eq!(
            fixed(
                "void f() {\n    return;\n    string marker = \"} // not syntax\"; // dead\n    // still part of the dead run\n    int x = 1;\n}"
            ),
            "void f() {\n    return;\n}"
        );
    }

    #[test]
    fn dead_tail_after_break_is_deleted() {
        assert_eq!(
            fixed("void f() {\n    while (true) {\n        break;\n        int x = 1;\n    }\n}"),
            "void f() {\n    while (true) {\n        break;\n    }\n}"
        );
    }

    #[test]
    fn dead_tail_after_continue_is_deleted() {
        assert_eq!(
            fixed(
                "void f() {\n    while (true) {\n        continue;\n        int x = 1;\n    }\n}"
            ),
            "void f() {\n    while (true) {\n        continue;\n    }\n}"
        );
    }

    #[test]
    fn dead_tail_after_a_returning_if_else_is_deleted() {
        assert_eq!(
            fixed(
                "int f(bool c) {\n    if (c) {\n        return 1;\n    } else {\n        return 2;\n    }\n    int x = 1;\n}"
            ),
            "int f(bool c) {\n    if (c) {\n        return 1;\n    } else {\n        return 2;\n    }\n}"
        );
    }

    #[test]
    fn there_is_no_throw_terminator_in_this_dialect() {
        // Documented rather than fixed: `throw` is not a keyword here, so
        // `throw;` parses as an ordinary expression statement (the checker
        // reports `undefined identifier`), nothing after it is dead, and
        // there is no action to offer. If the parser ever grows a real
        // throw statement, `Checker::stmt_terminates` decides and this
        // family needs no change.
        assert_eq!(
            count("void f() {\n    throw;\n    int x = 1;\n}", FAMILY_NAME),
            0
        );
    }

    // ── Shape of the run ──────────────────────────────────────────────────

    #[test]
    fn a_multi_statement_tail_is_one_deletion_of_the_whole_run() {
        assert_eq!(
            fix_titles("void f() {\n    return;\n    int x = 1;\n    int y = 2;\n    g();\n}"),
            vec!["Delete unreachable code (3 statements)".to_string()]
        );
        assert_eq!(
            fixed("void f() {\n    return;\n    int x = 1;\n    int y = 2;\n    g();\n}"),
            "void f() {\n    return;\n}"
        );
    }

    #[test]
    fn a_single_statement_tail_says_so() {
        assert_eq!(
            fix_titles("void f() {\n    return;\n    int x = 1;\n}"),
            vec!["Delete unreachable code (1 statement)".to_string()]
        );
    }

    #[test]
    fn a_tail_containing_a_nested_block_still_reparses() {
        assert_eq!(
            fixed(
                "void f() {\n    return;\n    if (true) {\n        int x = 1;\n    }\n    int y = 2;\n}"
            ),
            "void f() {\n    return;\n}"
        );
    }

    #[test]
    fn a_tail_containing_a_loop_and_a_switch_still_reparses() {
        assert_eq!(
            fixed(
                "void f() {\n    return;\n    for (int i = 0; i < 2; i++) {\n        g();\n    }\n    switch (1) {\n        case 1:\n            break;\n    }\n}"
            ),
            "void f() {\n    return;\n}"
        );
    }

    #[test]
    fn only_the_dead_run_is_deleted_not_the_live_statements_before_it() {
        assert_eq!(
            fixed("void f() {\n    int live = 1;\n    return;\n    int dead = 2;\n}"),
            "void f() {\n    int live = 1;\n    return;\n}"
        );
    }

    #[test]
    fn a_tail_sharing_a_line_with_live_code_keeps_that_line() {
        // The run does not own its line, so the deletion is exact rather
        // than line-based — it must not swallow the `return;`.
        assert_eq!(
            fixed("void f() { return; int x = 1; }"),
            "void f() { return;  }"
        );
    }

    #[test]
    fn a_nested_terminating_block_kills_the_outer_tail_too() {
        // The inner block terminates, so BOTH runs are dead and the
        // checker reports both — once per block. The two deletions are
        // independent, so both are offered and both apply.
        let source =
            "void f() {\n    {\n        return;\n        int x = 1;\n    }\n    int live = 2;\n}";
        assert_eq!(
            fix_titles(source),
            vec![
                "Delete unreachable code (1 statement)".to_string(),
                "Delete unreachable code (1 statement)".to_string(),
            ]
        );
        let out = apply_all(source, FAMILY_NAME);
        assert_eq!(out, "void f() {\n    {\n        return;\n    }\n}");
        assert!(reparses_clean(&out));
    }

    #[test]
    fn a_tail_inside_a_switch_case_is_deleted() {
        assert_eq!(
            fixed(
                "void f() {\n    switch (1) {\n        case 1:\n            break;\n            int x = 1;\n    }\n}"
            ),
            "void f() {\n    switch (1) {\n        case 1:\n            break;\n    }\n}"
        );
    }

    #[test]
    fn class_and_namespace_bodies_are_covered() {
        for source in [
            "class C {\n    void m() {\n        return;\n        int x = 1;\n    }\n}",
            "namespace N {\n    void f() {\n        return;\n        int x = 1;\n    }\n}",
        ] {
            assert_eq!(count(source, FAMILY_NAME), 1, "for `{source}`");
            assert!(reparses_clean(&apply(
                source,
                FAMILY_NAME,
                "Delete unreachable code"
            )));
        }
    }

    // ── Reachable code offers nothing ─────────────────────────────────────

    #[test]
    fn reachable_code_offers_nothing() {
        for source in [
            "void f() {\n    int x = 1;\n    return;\n}",
            "int f(bool c) {\n    if (c) {\n        return 1;\n    }\n    return 2;\n}",
            "void f() {\n    while (true) {\n        break;\n    }\n    int x = 1;\n}",
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
    fn deletion_is_neither_preferred_nor_auto_applicable() {
        assert_eq!(
            flags(
                "void f() {\n    return;\n    int x = 1;\n}",
                FAMILY_NAME,
                "Delete unreachable code"
            ),
            (false, false)
        );
    }
}
