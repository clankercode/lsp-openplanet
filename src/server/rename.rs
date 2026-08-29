//! Scope-aware `textDocument/rename`, `textDocument/prepareRename` and
//! `textDocument/references` (B049).
//!
//! **RED PLACEHOLDER** — the bodies below still delegate to the
//! scope-blind token scan this ticket exists to replace. The tests are
//! written against the final API so they fail on behaviour, not on a
//! compile error.

use tower_lsp::lsp_types::*;

use crate::analysis::DocumentAnalysis;
use crate::server::navigation::{self, WorkspaceFiles};
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

pub fn prepare_rename(
    analysis: &DocumentAnalysis,
    position: Position,
    _scope: &GlobalScope<'_>,
    _files: &WorkspaceFiles,
) -> Result<Range, Refusal> {
    let (start, end, _) = navigation::ident_span_at(analysis, position)
        .ok_or_else(|| Refusal::new("not an identifier"))?;
    Ok(crate::server::diagnostics::span_to_range(
        analysis.masked_source(),
        crate::lexer::Span { start, end },
    ))
}

pub fn rename(
    analysis: &DocumentAnalysis,
    _uri: &Url,
    position: Position,
    new_name: &str,
    _scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
) -> Result<WorkspaceEdit, Refusal> {
    navigation::rename(analysis, position, new_name, files)
        .ok_or_else(|| Refusal::new("no matches"))
}

pub fn find_references(
    analysis: &DocumentAnalysis,
    _uri: &Url,
    position: Position,
    _scope: &GlobalScope<'_>,
    files: &WorkspaceFiles,
    include_declaration: bool,
) -> Vec<Location> {
    navigation::find_references(analysis, position, files, include_declaration)
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
                    (index, position_to_offset(sources[index].1, location.range.start))
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
        let src = "class C {\n    int x;\n    void f() { int x = 1; x++; }\n    void g() { x = 2; }\n}\n";
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
        assert_eq!(changed[0].1, "void Assist() {}\nvoid Main() { Assist(); }\n");
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
        let src = "void Helper() {}\nvoid f() { int Helper = 1; Helper++; }\nvoid g() { Helper(); }\n";
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
            prepare_rename(tw.analysis_of(0), at(src, "x = 1", 0), scope, files)
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
                prepare_rename(tw.analysis_of(0), at(src, needle, 0), scope, files)
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
                    let _ = prepare_rename(tw.analysis_of(0), position, scope, files);
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
