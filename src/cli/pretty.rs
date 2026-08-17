//! Pretty check-report renderer (source excerpts + carets).
//!
//! # Rules (locked on map #8 / issue #12)
//!
//! 1. **When pretty:** `format == Pretty`, or `format == Auto` and the caller's
//!    color capability is on (`color_stdout()`-equivalent). `format == Plain`
//!    is always gcc-style. Explicit `--format pretty` still renders pretty even
//!    when color is off (no ANSI); only the layout changes.
//! 2. **Each diagnostic block:**
//!    - header: `path:line:col` (path may be dim/cyan when color is on)
//!    - one source line with line-number gutter: `  37 │ code here`
//!    - caret line under the primary span: `     │     ^^^^^ message`
//! 3. Use `diagnostic.range` for the span. Missing / zero-width ranges get a
//!    single `^` under the start column (or column 1).
//! 4. Tab width is 4. Column math uses `chars().count` (crude; does **not**
//!    implement full Unicode width / LSP UTF-16 — acceptable for v1).
//! 5. **No outer unicode box frame** in CLI v1 — framing is the watch TUI's job.
//! 6. Summary footer: `✗ N diagnostics · E errors · W warnings · root`
//!    (or `✓ 0 diagnostics · root` when clean).
//! 7. CLI: `--format plain|pretty|auto` (default `auto`).
//! 8. Shared entry: [`super::format_check_report_with`].
//! 9. Source excerpts are read from disk via diagnostic paths.
//! 10. LSP JSON-RPC path is untouched.
//! 11. The mismatch-report trailer (super::format_check_report_with) follows
//!     the summary on one-shot output; the watch TUI does not print it.

use std::collections::HashMap;
use std::path::PathBuf;

#[cfg(test)]
use tower_lsp::lsp_types::Position;
use tower_lsp::lsp_types::{DiagnosticSeverity, Range};

use super::{severity_label, CheckReport, CliDiagnostic};
use crate::term;

/// Tab stops when expanding source lines for display / column math.
const TAB_WIDTH: usize = 4;

pub(super) fn format_pretty(report: &CheckReport, color: bool) -> String {
    let mut source_cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut n_err = 0usize;
    let mut n_warn = 0usize;
    let mut n_other = 0usize;

    let mut blocks: Vec<String> = Vec::with_capacity(report.diagnostics.len());

    for item in &report.diagnostics {
        match item.diagnostic.severity {
            Some(DiagnosticSeverity::ERROR) | None => n_err += 1,
            Some(DiagnosticSeverity::WARNING) => n_warn += 1,
            _ => n_other += 1,
        }
        blocks.push(format_diagnostic_block(
            report,
            item,
            color,
            &mut source_cache,
        ));
    }

    let body = if blocks.is_empty() {
        String::new()
    } else {
        blocks.join("\n")
    };

    let summary = format_pretty_summary(report, color, n_err, n_warn, n_other);

    if body.is_empty() {
        format!("{summary}\n")
    } else {
        format!("{body}\n{summary}\n")
    }
}

fn format_pretty_summary(
    report: &CheckReport,
    color: bool,
    n_err: usize,
    n_warn: usize,
    n_other: usize,
) -> String {
    let root = term::path(color, report.root.display().to_string());
    if report.diagnostics.is_empty() {
        return format!("{} {}", term::ok(color, "✓ 0 diagnostics"), root);
    }

    let mark = if n_err > 0 {
        term::error(color, "✗")
    } else if n_warn > 0 {
        term::warning(color, "✗")
    } else {
        term::info(color, "✗")
    };

    let mut parts = vec![term::bold(
        color,
        format!(
            "{} diagnostic{}",
            report.diagnostics.len(),
            if report.diagnostics.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
    )];
    if n_err > 0 {
        parts.push(term::error(
            color,
            format!("{n_err} error{}", if n_err == 1 { "" } else { "s" }),
        ));
    }
    if n_warn > 0 {
        parts.push(term::warning(
            color,
            format!("{n_warn} warning{}", if n_warn == 1 { "" } else { "s" }),
        ));
    }
    if n_other > 0 {
        parts.push(term::info(color, format!("{n_other} other")));
    }
    parts.push(root);

    format!("{mark} {}", parts.join(" · "))
}

fn format_diagnostic_block(
    report: &CheckReport,
    item: &CliDiagnostic,
    color: bool,
    source_cache: &mut HashMap<PathBuf, Option<String>>,
) -> String {
    let rel = item.path.strip_prefix(&report.root).unwrap_or(&item.path);
    let range = item.diagnostic.range;
    let line_no = range.start.line + 1;
    let col_no = range.start.character + 1;

    let path_s = term::path(color, rel.display().to_string());
    let loc_s = term::loc(color, format!("{line_no}:{col_no}"));
    let severity = severity_label(item.diagnostic.severity);
    let sev_styled = match item.diagnostic.severity {
        Some(DiagnosticSeverity::ERROR) | None => term::error(color, severity),
        Some(DiagnosticSeverity::WARNING) => term::warning(color, severity),
        Some(DiagnosticSeverity::INFORMATION) => term::info(color, severity),
        Some(DiagnosticSeverity::HINT) => term::dim(color, severity),
        _ => severity.to_string(),
    };

    let mut out = String::new();
    // header: path:line:col + severity
    out.push_str(&format!("{path_s}:{loc_s}: {sev_styled}\n"));

    let source = source_cache
        .entry(item.path.clone())
        .or_insert_with(|| std::fs::read_to_string(&item.path).ok());

    if let Some(src) = source.as_ref() {
        if let Some(raw_line) = line_at(src, range.start.line as usize) {
            let expanded = expand_tabs(raw_line, TAB_WIDTH);
            let gutter_w = line_no.to_string().len().max(2);
            let gutter = format!("{line_no:>gutter_w$}");
            let pipe = term::dim(color, "│");

            out.push_str(&format!("  {gutter} {pipe} {expanded}\n"));

            let (start_col, end_col) = caret_cols(&expanded, range);
            let pad = " ".repeat(start_col);
            let carets = if end_col > start_col {
                "^".repeat(end_col - start_col)
            } else {
                "^".to_string()
            };
            let carets_s = match item.diagnostic.severity {
                Some(DiagnosticSeverity::ERROR) | None => term::error(color, &carets),
                Some(DiagnosticSeverity::WARNING) => term::warning(color, &carets),
                Some(DiagnosticSeverity::INFORMATION) => term::info(color, &carets),
                _ => term::dim(color, &carets),
            };
            let blank_gutter = " ".repeat(gutter_w);
            out.push_str(&format!(
                "  {blank_gutter} {pipe} {pad}{carets_s} {}\n",
                item.diagnostic.message
            ));
            return out;
        }
    }

    // Fallback when source is missing or line is out of range: still show message.
    out.push_str(&format!("  {}\n", item.diagnostic.message));
    out
}

/// Expand tabs to spaces (tab width 4) so caret columns align with display.
fn expand_tabs(line: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = tab_width - (col % tab_width);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(ch);
            col += 1; // chars().count policy — see module docs
        }
    }
    out
}

/// Map LSP range onto expanded source-line columns (0-based, exclusive end).
fn caret_cols(expanded_line: &str, range: Range) -> (usize, usize) {
    let line_len = expanded_line.chars().count();
    let start = (range.start.character as usize).min(line_len);
    let end = if same_line(range) {
        let e = range.end.character as usize;
        if e <= range.start.character as usize {
            (start + 1).min(line_len.max(start + 1))
        } else {
            e.min(line_len.max(start + 1))
        }
    } else {
        // Multi-line span: caret through end of the start line.
        line_len.max(start + 1)
    };
    // Ensure at least one caret column even on empty lines.
    if line_len == 0 {
        return (0, 1);
    }
    let end = end.max(start + 1);
    (start, end)
}

fn same_line(range: Range) -> bool {
    range.start.line == range.end.line
}

fn line_at(source: &str, line_idx: usize) -> Option<&str> {
    source
        .lines()
        .nth(line_idx)
        .map(|l| l.trim_end_matches('\r'))
}

/// Build a synthetic diagnostic for unit tests.
#[cfg(test)]
pub(super) fn test_diag(
    path: impl Into<PathBuf>,
    line: u32,
    col: u32,
    end_col: u32,
    severity: Option<DiagnosticSeverity>,
    message: &str,
) -> CliDiagnostic {
    use tower_lsp::lsp_types::Diagnostic;
    CliDiagnostic {
        path: path.into(),
        diagnostic: Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: col,
                },
                end: Position {
                    line,
                    character: end_col,
                },
            },
            severity,
            message: message.to_string(),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{format_check_report_with, CheckFormat, CheckReport};
    use std::fs;

    fn temp_plugin(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "openplanet-lsp-pretty-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn expand_tabs_width_4() {
        assert_eq!(expand_tabs("a\tb", 4), "a   b");
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("ab\tc", 4), "ab  c");
    }

    #[test]
    fn zero_width_range_gets_single_caret() {
        let expanded = "hello world";
        let range = Range {
            start: Position {
                line: 0,
                character: 6,
            },
            end: Position {
                line: 0,
                character: 6,
            },
        };
        assert_eq!(caret_cols(expanded, range), (6, 7));
    }

    #[test]
    fn pretty_false_via_plain_is_gcc_style() {
        let root = temp_plugin("plain");
        let path = root.join("src/Main.as");
        fs::write(&path, "void Main() {\n  Missing@ x;\n}\n").unwrap();

        let report = CheckReport {
            root: root.clone(),
            diagnostics: vec![test_diag(
                &path,
                1,
                2,
                9,
                Some(DiagnosticSeverity::ERROR),
                "unknown type `Missing`",
            )],
            type_database_loaded: false,
        };

        let out = format_check_report_with(&report, false, CheckFormat::Plain);
        assert!(
            out.contains("src/Main.as:2:3: error: unknown type `Missing`"),
            "plain gcc line missing: {out:?}"
        );
        assert!(
            !out.contains('│'),
            "plain must not use unicode gutter: {out:?}"
        );
        assert!(!out.contains('^'), "plain must not draw carets: {out:?}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pretty_true_includes_source_excerpt_and_carets() {
        let root = temp_plugin("pretty");
        let path = root.join("src/Main.as");
        //            012345678901
        fs::write(&path, "void Main() {\n  Missing@ x;\n}\n").unwrap();

        let report = CheckReport {
            root: root.clone(),
            diagnostics: vec![test_diag(
                &path,
                1,
                2,
                9,
                Some(DiagnosticSeverity::ERROR),
                "unknown type `Missing`",
            )],
            type_database_loaded: false,
        };

        // color=false, format=pretty → pretty layout, no ANSI
        let out = format_check_report_with(&report, false, CheckFormat::Pretty);
        assert!(
            out.contains("src/Main.as:2:3"),
            "header path:line:col missing: {out:?}"
        );
        assert!(
            out.contains('│') && out.contains("Missing@ x;"),
            "expected source gutter line: {out:?}"
        );
        assert!(
            out.contains('^') && out.contains("unknown type `Missing`"),
            "expected caret + message: {out:?}"
        );
        // span cols 2..9 → seven carets under Missing
        assert!(
            out.contains("^^^^^^^"),
            "expected span carets under Missing: {out:?}"
        );
        assert!(
            out.contains("✗") && out.contains("1 diagnostic") && out.contains("1 error"),
            "expected pretty summary: {out:?}"
        );
        // no outer box frame in CLI pretty v1
        assert!(
            !out.contains('╭') && !out.contains('╰'),
            "CLI pretty must not box-frame: {out:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn auto_follows_color_capability() {
        let root = temp_plugin("auto");
        let path = root.join("src/Main.as");
        fs::write(&path, "int x = 1;\n").unwrap();
        let report = CheckReport {
            root: root.clone(),
            diagnostics: vec![test_diag(
                &path,
                0,
                0,
                3,
                Some(DiagnosticSeverity::WARNING),
                "demo",
            )],
            type_database_loaded: true,
        };

        let plainish = format_check_report_with(&report, false, CheckFormat::Auto);
        assert!(
            plainish.contains("src/Main.as:1:1: warning: demo"),
            "auto+!color → gcc plain: {plainish:?}"
        );

        let prettyish = format_check_report_with(&report, true, CheckFormat::Auto);
        assert!(
            prettyish.contains('│') && prettyish.contains('^'),
            "auto+color → pretty: {prettyish:?}"
        );
        assert!(
            !prettyish.contains('╭') && !prettyish.contains('╰'),
            "no CLI box frame even with color: {prettyish:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pretty_clean_summary() {
        let root = temp_plugin("clean");
        let report = CheckReport {
            root: root.clone(),
            diagnostics: vec![],
            type_database_loaded: true,
        };
        let out = format_check_report_with(&report, false, CheckFormat::Pretty);
        assert!(
            out.contains("✓ 0 diagnostics"),
            "clean pretty summary: {out:?}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
