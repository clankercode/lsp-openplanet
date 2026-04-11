//! Folding range provider.
//!
//! Computes `textDocument/foldingRange` results for OpenPlanet AngelScript
//! sources. Two complementary passes run over the same source:
//!
//! 1. **AST pass** walks `SourceFile` items collecting brace-to-brace folds
//!    for namespaces, classes, interfaces, enums, functions, methods, and
//!    nested statement blocks (`if`/`for`/`while`/`do-while`/`switch`/`try`).
//! 2. **Byte pass** scans the raw source for things the AST doesn't model as
//!    spans: multi-line block comments (`/* ... */`) and `#if`/`#endif`
//!    preprocessor regions.
//!
//! Single-line folds (`start_line == end_line`) are filtered out — they add
//! nothing for the editor — and the final list is sorted by `(start_line,
//! end_line)` so the output is deterministic across runs.

use tower_lsp::lsp_types::{FoldingRange, FoldingRangeKind};

use crate::lexer;
use crate::lexer::Span;
use crate::parser::Parser;
use crate::parser::ast::{
    ClassDecl, ClassMember, FunctionBody, FunctionDecl, Item, Stmt, StmtKind,
};
use crate::server::diagnostics::offset_to_position;

/// Compute all folding ranges for a single source file.
pub fn folding_ranges(source: &str) -> Vec<FoldingRange> {
    let mut out: Vec<FoldingRange> = Vec::new();

    // AST pass.
    let tokens = lexer::tokenize_filtered(source);
    let mut parser = Parser::new(&tokens, source);
    let file = parser.parse_file();
    collect_items(&file.items, source, &mut out);

    // Byte pass (comments + preprocessor regions).
    let skip = compute_skip_mask(source.as_bytes());
    collect_block_comments(source, &mut out);
    collect_preprocessor_regions(source, &skip, &mut out);

    // Drop any useless single-line folds just in case something slipped
    // through the per-collector guards.
    out.retain(|f| f.end_line > f.start_line);

    // Deterministic order.
    out.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| a.end_line.cmp(&b.end_line))
    });

    out
}

// ---------------------------------------------------------------------------
// AST pass.
// ---------------------------------------------------------------------------

fn collect_items(items: &[Item], source: &str, out: &mut Vec<FoldingRange>) {
    for item in items {
        match item {
            Item::Namespace(ns) => {
                push_span_fold(ns.span, source, None, out);
                collect_items(&ns.items, source, out);
            }
            Item::Class(cls) => {
                push_span_fold(cls.span, source, None, out);
                collect_class_members(cls, source, out);
            }
            Item::Interface(iface) => {
                push_span_fold(iface.span, source, None, out);
                for m in &iface.methods {
                    collect_function(m, source, out);
                }
            }
            Item::Enum(en) => {
                push_span_fold(en.span, source, None, out);
            }
            Item::Function(f) => {
                collect_function(f, source, out);
            }
            _ => {}
        }
    }
}

fn collect_class_members(cls: &ClassDecl, source: &str, out: &mut Vec<FoldingRange>) {
    for member in &cls.members {
        match member {
            ClassMember::Method(f)
            | ClassMember::Constructor(f)
            | ClassMember::Destructor(f) => collect_function(f, source, out),
            ClassMember::Property(p) => {
                if let Some(body) = &p.getter {
                    push_body_fold(body, source, out);
                }
                if let Some((_, body)) = &p.setter {
                    push_body_fold(body, source, out);
                }
            }
            ClassMember::Field(_) => {}
        }
    }
}

fn collect_function(f: &FunctionDecl, source: &str, out: &mut Vec<FoldingRange>) {
    if let Some(body) = &f.body {
        push_body_fold(body, source, out);
        for stmt in &body.stmts {
            collect_stmt(stmt, source, out);
        }
    }
}

fn push_body_fold(body: &FunctionBody, source: &str, out: &mut Vec<FoldingRange>) {
    push_span_fold(body.span, source, None, out);
}

fn collect_stmt(stmt: &Stmt, source: &str, out: &mut Vec<FoldingRange>) {
    match &stmt.kind {
        StmtKind::Block(stmts) => {
            push_span_fold(stmt.span, source, None, out);
            for s in stmts {
                collect_stmt(s, source, out);
            }
        }
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            push_span_fold(then_branch.span, source, None, out);
            collect_stmt(then_branch, source, out);
            if let Some(eb) = else_branch {
                push_span_fold(eb.span, source, None, out);
                collect_stmt(eb, source, out);
            }
        }
        StmtKind::For { body, .. }
        | StmtKind::While { body, .. }
        | StmtKind::DoWhile { body, .. } => {
            push_span_fold(body.span, source, None, out);
            collect_stmt(body, source, out);
        }
        StmtKind::Switch { cases, .. } => {
            push_span_fold(stmt.span, source, None, out);
            for case in cases {
                for s in &case.stmts {
                    collect_stmt(s, source, out);
                }
            }
        }
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            push_span_fold(try_body.span, source, None, out);
            collect_stmt(try_body, source, out);
            push_span_fold(catch_body.span, source, None, out);
            collect_stmt(catch_body, source, out);
        }
        _ => {}
    }
}

fn push_span_fold(
    span: Span,
    source: &str,
    kind: Option<FoldingRangeKind>,
    out: &mut Vec<FoldingRange>,
) {
    let start_line = offset_to_position(source, span.start as usize).line;
    let end_offset = (span.end as usize).saturating_sub(1);
    let end_line = offset_to_position(source, end_offset).line;
    if end_line <= start_line {
        return;
    }
    out.push(FoldingRange {
        start_line,
        start_character: None,
        end_line,
        end_character: None,
        kind,
        collapsed_text: None,
    });
}

// ---------------------------------------------------------------------------
// Byte pass.
// ---------------------------------------------------------------------------

/// Bitmap marking bytes inside string / char literals (so `/*` inside a
/// string does NOT start a comment, and `#if` inside a string is NOT a
/// directive). Modelled after `signature::compute_skip_mask` but only flags
/// literals — comments are tracked separately by `collect_block_comments`.
fn compute_skip_mask(bytes: &[u8]) -> Vec<bool> {
    let mut mask = vec![false; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Line comment: `//...\n` — not strictly a string, but also a region
        // where `#if` should not count as a directive.
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            mask[start..i].fill(true);
            continue;
        }
        // Block comment.
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

/// Scan for `/* ... */` block comments spanning two or more lines. Strings
/// are NOT skipped here because we walk comments first — but the scanner
/// does track literals inline so `"/*"` inside a string doesn't trip it.
fn collect_block_comments(source: &str, out: &mut Vec<FoldingRange>) {
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Skip line comments so we don't mistake `// /*` for the start of
        // a block comment.
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Skip string / char literals inline.
        if b == b'"' || b == b'\'' {
            let quote = b;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == quote {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            let end = if i + 1 < bytes.len() {
                i += 2;
                i
            } else {
                i = bytes.len();
                i
            };
            let start_line = offset_to_position(source, start).line;
            let end_line = offset_to_position(source, end.saturating_sub(1)).line;
            if end_line > start_line {
                out.push(FoldingRange {
                    start_line,
                    start_character: None,
                    end_line,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                });
            }
            continue;
        }
        i += 1;
    }
}

/// Scan for `#if` / `#ifdef` / `#endif` directives and pair them up using a
/// simple stack. Each matched pair becomes one `Region` fold. `#else` is
/// NOT treated as a fold boundary — the outer `#if ... #endif` still folds
/// as a single region.
fn collect_preprocessor_regions(source: &str, skip: &[bool], out: &mut Vec<FoldingRange>) {
    let bytes = source.as_bytes();
    let mut stack: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Only a `#` at the start of a line (optionally after whitespace)
        // counts as a directive.
        if bytes[i] == b'\n' {
            i += 1;
            continue;
        }
        let line_start = if i == 0 || bytes[i - 1] == b'\n' {
            Some(i)
        } else {
            None
        };
        if let Some(ls) = line_start {
            let mut j = ls;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'#' && !skip.get(j).copied().unwrap_or(false) {
                let dir_start = j;
                j += 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
                    j += 1;
                }
                let name = &bytes[name_start..j];
                if name == b"if" || name == b"ifdef" || name == b"ifndef" {
                    stack.push(dir_start);
                } else if name == b"endif" {
                    if let Some(open) = stack.pop() {
                        let start_line = offset_to_position(source, open).line;
                        let end_line = offset_to_position(source, dir_start).line;
                        if end_line > start_line {
                            out.push(FoldingRange {
                                start_line,
                                start_character: None,
                                end_line,
                                end_character: None,
                                kind: Some(FoldingRangeKind::Region),
                                collapsed_text: None,
                            });
                        }
                    }
                }
                // Advance past the directive line.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
        }
        i += 1;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_body_folds() {
        let src = "void f() {\n  int x = 0;\n  x = 1;\n}\n";
        let folds = folding_ranges(src);
        assert!(
            folds.iter().any(|f| f.start_line == 0 && f.end_line >= 2),
            "expected a fold covering the function body, got {:?}",
            folds
        );
    }

    #[test]
    fn test_single_line_function_no_fold() {
        let src = "void f() { return; }\n";
        let folds = folding_ranges(src);
        assert!(
            folds.is_empty(),
            "expected no folds for single-line function, got {:?}",
            folds
        );
    }

    #[test]
    fn test_nested_class_and_method_fold() {
        let src = "class Foo {\n  void m() {\n    int y = 0;\n  }\n}\n";
        let folds = folding_ranges(src);
        assert!(
            folds.len() >= 2,
            "expected class body + method body folds, got {:?}",
            folds
        );
        assert!(folds.iter().any(|f| f.start_line == 0));
        assert!(folds.iter().any(|f| f.start_line == 1));
    }

    #[test]
    fn test_block_comment_multiline_fold() {
        let src = "/*\n  hi\n*/\nvoid f() {}\n";
        let folds = folding_ranges(src);
        let comment_folds: Vec<_> = folds
            .iter()
            .filter(|f| f.kind == Some(FoldingRangeKind::Comment))
            .collect();
        assert_eq!(comment_folds.len(), 1, "got {:?}", folds);
        assert_eq!(comment_folds[0].start_line, 0);
        assert_eq!(comment_folds[0].end_line, 2);
    }

    #[test]
    fn test_block_comment_single_line_no_fold() {
        let src = "/* inline */\n";
        let folds = folding_ranges(src);
        assert!(
            folds
                .iter()
                .all(|f| f.kind != Some(FoldingRangeKind::Comment)),
            "expected no comment folds for single-line block, got {:?}",
            folds
        );
    }

    #[test]
    fn test_preprocessor_if_endif_fold() {
        let src = "#if MY_FLAG\nvoid f() {}\n#endif\n";
        let folds = folding_ranges(src);
        let regions: Vec<_> = folds
            .iter()
            .filter(|f| f.kind == Some(FoldingRangeKind::Region))
            .collect();
        assert_eq!(regions.len(), 1, "got {:?}", folds);
        assert_eq!(regions[0].start_line, 0);
        assert_eq!(regions[0].end_line, 2);
    }

    #[test]
    fn test_nested_preprocessor_if_fold() {
        let src = "#if OUTER\n#if INNER\nvoid f() {}\n#endif\n#endif\n";
        let folds = folding_ranges(src);
        let regions: Vec<_> = folds
            .iter()
            .filter(|f| f.kind == Some(FoldingRangeKind::Region))
            .collect();
        assert_eq!(regions.len(), 2, "got {:?}", folds);
    }
}
