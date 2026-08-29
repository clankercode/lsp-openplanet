use std::collections::HashSet;

use crate::lexer::Span;

use super::eval::eval_condition;

// ── Public types ──────────────────────────────────────────────────────────────

/// Error kinds that can occur while preprocessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreprocErrorKind {
    /// `#endif` without a matching `#if`.
    UnmatchedEndif,
    /// `#else` without a matching `#if`.
    UnmatchedElse,
    /// `#elif` without a matching `#if`.
    UnmatchedElif,
    /// `#if` without a closing `#endif` (reported at end of file).
    MissingEndif,
}

/// A preprocessing error, including the 1-based line number where it occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreprocError {
    pub line: usize,
    pub kind: PreprocErrorKind,
}

/// Result of preprocessing a source file.
#[derive(Debug)]
pub struct PreprocessResult {
    /// Source with inactive/directive lines replaced by spaces.
    /// `masked_source.len() == original_source.len()`.
    pub masked_source: String,
    /// Any errors encountered.
    pub errors: Vec<PreprocError>,
    /// Byte spans of code the current defines switched off, in source order.
    ///
    /// Indexes the ORIGINAL source, not [`Self::masked_source`]. One span per
    /// contiguous dead body: consecutive dead lines coalesce, the `#if` /
    /// `#elif` / `#else` / `#endif` lines that delimit the block stay out,
    /// and a nested block inside an already-dead body is folded into the
    /// outermost span rather than splitting it. B031: the LSP fades these
    /// (HINT + `DiagnosticTag::UNNECESSARY`); the CLI ignores them.
    pub inactive_regions: Vec<Span>,
}

// ── Directive parsing ─────────────────────────────────────────────────────────

enum Directive<'a> {
    If(&'a str),
    Elif(&'a str),
    Else,
    Endif,
}

/// Try to parse a preprocessor directive from a trimmed line.
/// Returns `None` if the line is not a directive.
fn parse_directive(trimmed: &str) -> Option<Directive<'_>> {
    if !trimmed.starts_with('#') {
        return None;
    }
    let rest = trimmed[1..].trim_start();
    if let Some(cond) = rest.strip_prefix("if") {
        // Must be `#if` followed by whitespace or end-of-line
        if cond.is_empty() || cond.starts_with(|c: char| c.is_whitespace()) {
            return Some(Directive::If(cond.trim()));
        }
    }
    if let Some(cond) = rest.strip_prefix("elif") {
        if cond.is_empty() || cond.starts_with(|c: char| c.is_whitespace()) {
            return Some(Directive::Elif(cond.trim()));
        }
    }
    if rest == "else" {
        return Some(Directive::Else);
    }
    if rest == "endif" {
        return Some(Directive::Endif);
    }
    None
}

// ── Main filter function ───────────────────────────────────────────────────────

/// Preprocess `source` by masking lines that are inside inactive `#if` blocks
/// and the directive lines themselves.
///
/// Each masked character is replaced with a space so that byte offsets of
/// active content are preserved exactly.
pub fn preprocess(source: &str, defines: &HashSet<String>) -> PreprocessResult {
    let mut masked = source.as_bytes().to_vec();
    let mut errors: Vec<PreprocError> = Vec::new();
    let mut inactive_regions: Vec<Span> = Vec::new();

    // Open dead region as (start, end) byte offsets, and the stack depth at
    // which inactivity began. The depth is what separates the directive that
    // CLOSES this region (same depth — a boundary, kept out of the span) from
    // one merely nested inside it (deeper — dead too, folded in).
    let mut region: Option<(usize, usize)> = None;
    let mut dead_depth: Option<usize> = None;

    // Stack entries: (parent_was_active, branch_already_taken)
    // - `parent_was_active`: whether the enclosing block is active (true at
    //   top level).
    // - `branch_already_taken`: whether a true branch has been seen in the
    //   current if/elif/else chain.
    let mut stack: Vec<(bool, bool)> = Vec::new();

    // Whether the current position is actively emitting code.
    let mut active = true;

    // Iterate over lines while tracking byte offsets.
    let mut line_start = 0usize;
    let mut line_num = 0usize;

    for raw_line in source.split_inclusive('\n') {
        line_num += 1;
        let line_end = line_start + raw_line.len();
        let trimmed = raw_line.trim();

        let parsed = parse_directive(trimmed);
        // A dead line belongs to the region unless it is this region's own
        // closing/branching directive.
        let closes_region = !active
            && dead_depth == Some(stack.len())
            && matches!(
                parsed,
                Some(Directive::Elif(_) | Directive::Else | Directive::Endif)
            );
        if !active && !closes_region {
            match &mut region {
                Some((_, end)) => *end = line_end,
                None => region = Some((line_start, line_end)),
            }
        } else {
            flush_region(source, &mut region, &mut inactive_regions);
        }

        if let Some(directive) = parsed {
            // Always mask the directive line itself.
            mask_range(&mut masked, line_start, line_end);

            match directive {
                Directive::If(cond) => {
                    let new_active = active && eval_condition(cond, defines);
                    stack.push((active, new_active));
                    active = new_active;
                }
                Directive::Elif(cond) => {
                    if let Some((parent_active, branch_taken)) = stack.last_mut() {
                        let already = *branch_taken;
                        if already || !*parent_active {
                            // Branch already taken or parent inactive: stay off.
                            active = false;
                        } else {
                            // Parent active and no branch taken yet.
                            let val = eval_condition(cond, defines);
                            if val {
                                *branch_taken = true;
                            }
                            active = val;
                        }
                    } else {
                        errors.push(PreprocError {
                            line: line_num,
                            kind: PreprocErrorKind::UnmatchedElif,
                        });
                    }
                }
                Directive::Else => {
                    if let Some((parent_active, branch_taken)) = stack.last_mut() {
                        active = *parent_active && !*branch_taken;
                        // Mark branch as taken so a hypothetical second #else would be off.
                        *branch_taken = true;
                    } else {
                        errors.push(PreprocError {
                            line: line_num,
                            kind: PreprocErrorKind::UnmatchedElse,
                        });
                    }
                }
                Directive::Endif => {
                    if let Some((parent_active, _)) = stack.pop() {
                        active = parent_active;
                    } else {
                        errors.push(PreprocError {
                            line: line_num,
                            kind: PreprocErrorKind::UnmatchedEndif,
                        });
                    }
                }
            }
        } else if !active {
            // Non-directive line in an inactive block: mask it.
            mask_range(&mut masked, line_start, line_end);
        }

        // Inactivity is anchored to the depth it began at; nesting deeper
        // inside an already-dead body does not move that anchor.
        if active {
            dead_depth = None;
        } else if dead_depth.is_none() {
            dead_depth = Some(stack.len());
        }

        line_start = line_end;
    }
    // An unterminated dead block runs to EOF.
    flush_region(source, &mut region, &mut inactive_regions);

    // Any unclosed #if blocks.
    for _ in 0..stack.len() {
        errors.push(PreprocError {
            line: line_num,
            kind: PreprocErrorKind::MissingEndif,
        });
    }

    // Safe: mask_range only replaces non-newline bytes with 0x20 (space),
    // which is valid UTF-8. The original source was valid UTF-8, and we only
    // replace individual bytes with a valid single-byte ASCII character.
    let masked_source = String::from_utf8(masked).expect("masking preserved valid UTF-8");

    PreprocessResult {
        masked_source,
        errors,
        inactive_regions,
    }
}

/// Replace every non-newline byte in `masked[start..end]` with a space,
/// preserving newlines so line numbers remain correct.
fn mask_range(masked: &mut Vec<u8>, start: usize, end: usize) {
    for b in masked[start..end].iter_mut() {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Close the open dead region, if any, into `out`.
///
/// Trims trailing line breaks so the span ends on the last real character
/// (a span ending at `\n` renders as a fade that bleeds onto the next line),
/// and drops a region that is empty once trimmed.
fn flush_region(source: &str, region: &mut Option<(usize, usize)>, out: &mut Vec<Span>) {
    let Some((start, mut end)) = region.take() else {
        return;
    };
    let bytes = source.as_bytes();
    while end > start && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
        end -= 1;
    }
    if end > start {
        out.push(Span::new(start as u32, end as u32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The source text each inactive region covers, for readable assertions.
    fn regions<'a>(src: &'a str, defines: &[&str]) -> Vec<&'a str> {
        preprocess(src, &def(defines))
            .inactive_regions
            .iter()
            .map(|span| span.text(src))
            .collect()
    }

    #[test]
    fn inactive_region_covers_the_dead_body_not_the_directives() {
        assert_eq!(
            regions("#if TMNEXT\nint x = 1;\n#endif\n", &[]),
            vec!["int x = 1;"],
            "the #if / #endif lines stay out of the region"
        );
        assert!(
            regions("#if TMNEXT\nint x = 1;\n#endif\n", &["TMNEXT"]).is_empty(),
            "a live branch has no inactive region"
        );
    }

    #[test]
    fn inactive_region_tracks_which_else_arm_is_dead() {
        let src = "#if TMNEXT\nint a;\n#else\nint b;\n#endif\n";
        assert_eq!(regions(src, &["TMNEXT"]), vec!["int b;"], "else arm dead");
        assert_eq!(regions(src, &[]), vec!["int a;"], "if arm dead");
    }

    #[test]
    fn inactive_regions_cover_every_untaken_elif_arm() {
        let src = "#if A\nint a;\n#elif B\nint b;\n#elif C\nint c;\n#else\nint d;\n#endif\n";
        assert_eq!(regions(src, &["A"]), vec!["int b;", "int c;", "int d;"]);
        assert_eq!(regions(src, &["B"]), vec!["int a;", "int c;", "int d;"]);
        assert_eq!(regions(src, &["C"]), vec!["int a;", "int b;", "int d;"]);
        // Nothing matches: every arm but the #else is dead.
        assert_eq!(regions(src, &[]), vec!["int a;", "int b;", "int c;"]);
        // A taken #elif is never a region, even with a later true arm.
        assert_eq!(
            regions(src, &["B", "C"]),
            vec!["int a;", "int c;", "int d;"]
        );
    }

    #[test]
    fn nested_dead_block_is_one_outermost_region_including_inner_directives() {
        let src = "#if OUTER\nint a;\n#if INNER\nint b;\n#endif\nint c;\n#endif\n";
        // Whole outer body is dead: ONE region, and the nested #if/#endif are
        // inside it (they are dead too) rather than splitting it in three.
        assert_eq!(
            regions(src, &["INNER"]),
            vec!["int a;\n#if INNER\nint b;\n#endif\nint c;"],
        );
    }

    #[test]
    fn nested_dead_block_inside_a_live_block_covers_the_inner_body_only() {
        let src = "#if OUTER\nint a;\n#if INNER\nint b;\n#endif\nint c;\n#endif\n";
        assert_eq!(regions(src, &["OUTER"]), vec!["int b;"]);
    }

    #[test]
    fn adjacent_dead_blocks_stay_separate_regions() {
        // Separated by live code.
        let src = "#if A\nint a;\n#endif\nint live;\n#if B\nint b;\n#endif\n";
        assert_eq!(regions(src, &[]), vec!["int a;", "int b;"]);

        // Separated only by directives — still two regions: the #endif/#if
        // between them are live directives and must not be faded.
        let src = "#if A\nint a;\n#endif\n#if B\nint b;\n#endif\n";
        assert_eq!(regions(src, &[]), vec!["int a;", "int b;"]);
    }

    #[test]
    fn consecutive_dead_lines_coalesce_into_one_region() {
        let src = "#if A\nint a;\nint b;\n\nint c;\n#endif\n";
        assert_eq!(regions(src, &[]), vec!["int a;\nint b;\n\nint c;"]);
    }

    #[test]
    fn empty_dead_body_emits_no_region() {
        assert!(regions("#if A\n#endif\n", &[]).is_empty());
        // Dead #if arm with an empty body; the #else arm is live.
        assert!(regions("#if A\n#else\nint b;\n#endif\n", &[]).is_empty());
        // Mirror: the taken arm is empty and the #else arm is dead, so the
        // #else body IS a region (an empty body must not swallow it).
        assert_eq!(
            regions("#if A\n#else\nint b;\n#endif\n", &["A"]),
            vec!["int b;"]
        );
    }

    #[test]
    fn unterminated_dead_block_runs_to_end_of_file() {
        let src = "#if A\nint a;\nint b;\n";
        let result = preprocess(src, &def(&[]));
        assert!(
            !result.errors.is_empty(),
            "missing #endif is still an error"
        );
        assert_eq!(
            result
                .inactive_regions
                .iter()
                .map(|s| s.text(src))
                .collect::<Vec<_>>(),
            vec!["int a;\nint b;"],
        );
    }

    #[test]
    fn inactive_regions_index_the_original_source_and_are_fully_masked() {
        // Multi-byte content and CRLF: spans must stay on char boundaries,
        // slice out of the ORIGINAL source, and every byte inside must be
        // masked in masked_source (guards offset drift).
        let src = "#if A\r\nint \u{00e9}\u{00e9}\u{00e9} = 1; // \u{4e2d}\u{6587}\r\nint b;\r\n#endif\r\nint live;\r\n";
        let result = preprocess(src, &def(&[]));
        assert_eq!(result.masked_source.len(), src.len());
        assert_eq!(result.inactive_regions.len(), 1);
        for span in &result.inactive_regions {
            let (start, end) = (span.start as usize, span.end as usize);
            assert!(
                src.is_char_boundary(start) && src.is_char_boundary(end),
                "span {start}..{end} must land on char boundaries"
            );
            assert!(
                src[start..end].contains("\u{4e2d}\u{6587}"),
                "region must slice the original source, not the mask"
            );
            assert!(
                result.masked_source.as_bytes()[start..end]
                    .iter()
                    .all(|b| b.is_ascii_whitespace()),
                "every byte inside an inactive region must be masked"
            );
        }
    }

    #[test]
    fn simple_if_active() {
        let src = "#if TMNEXT\nint x = 1;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        // Byte offsets preserved
        assert_eq!(result.masked_source.len(), src.len());
        // Directive lines are masked (spaces), content is preserved
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(
            lines[0].chars().all(|c| c == ' '),
            "directive line should be spaces"
        );
        assert_eq!(lines[1], "int x = 1;");
        assert!(
            lines[2].chars().all(|c| c == ' '),
            "endif line should be spaces"
        );
    }

    #[test]
    fn simple_if_inactive() {
        let src = "#if TMNEXT\nint x = 1;\n#endif\n";
        let result = preprocess(src, &def(&[]));
        assert!(result.errors.is_empty());
        assert_eq!(result.masked_source.len(), src.len());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(lines[0].chars().all(|c| c == ' '));
        assert!(
            lines[1].chars().all(|c| c == ' '),
            "inactive content should be masked"
        );
        assert!(lines[2].chars().all(|c| c == ' '));
    }

    #[test]
    fn if_else_inactive_branch() {
        // Without TMNEXT: `int a` masked, `int b` visible
        let src = "#if TMNEXT\nint a;\n#else\nint b;\n#endif\n";
        let result = preprocess(src, &def(&[]));
        assert!(result.errors.is_empty());
        assert_eq!(result.masked_source.len(), src.len());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        // line 0: #if → masked
        assert!(lines[0].chars().all(|c| c == ' '));
        // line 1: int a; → masked (inactive branch)
        assert!(lines[1].chars().all(|c| c == ' '));
        // line 2: #else → masked
        assert!(lines[2].chars().all(|c| c == ' '));
        // line 3: int b; → visible
        assert_eq!(lines[3], "int b;");
        // line 4: #endif → masked
        assert!(lines[4].chars().all(|c| c == ' '));
    }

    #[test]
    fn byte_offsets_preserved() {
        let src = "#if TMNEXT\nint x = 1;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert_eq!(result.masked_source.len(), src.len());

        // "int x = 1;" starts at offset 11 in the original
        let offset = src.find("int x").unwrap();
        assert_eq!(&result.masked_source[offset..offset + 5], "int x");
    }

    #[test]
    fn nested_if_inner_inactive() {
        // Only TMNEXT active, not SIG_DEVELOPER
        let src = "#if TMNEXT\n#if SIG_DEVELOPER\nint debug;\n#endif\nint normal;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        assert_eq!(result.masked_source.len(), src.len());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        // line 0: #if TMNEXT → masked
        assert!(lines[0].chars().all(|c| c == ' '));
        // line 1: #if SIG_DEVELOPER → masked
        assert!(lines[1].chars().all(|c| c == ' '));
        // line 2: int debug; → masked (inner inactive)
        assert!(lines[2].chars().all(|c| c == ' '));
        // line 3: #endif → masked
        assert!(lines[3].chars().all(|c| c == ' '));
        // line 4: int normal; → visible (outer active)
        assert_eq!(lines[4], "int normal;");
        // line 5: #endif → masked
        assert!(lines[5].chars().all(|c| c == ' '));
    }

    #[test]
    fn class_body_preprocessor() {
        // Real pattern from tm-dashboard
        let src = "class WheelState {\n    float m_slipCoef;\n#if TMNEXT\n    float m_breakCoef;\n#endif\n}";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        assert_eq!(result.masked_source.len(), src.len());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert_eq!(lines[0], "class WheelState {");
        assert_eq!(lines[1], "    float m_slipCoef;");
        assert!(
            lines[2].chars().all(|c| c == ' '),
            "#if line should be masked"
        );
        assert_eq!(lines[3], "    float m_breakCoef;");
        assert!(
            lines[4].chars().all(|c| c == ' '),
            "#endif line should be masked"
        );
        assert_eq!(lines[5], "}");
    }

    #[test]
    fn unmatched_endif_error() {
        let src = "int x;\n#endif\n";
        let result = preprocess(src, &def(&[]));
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].line, 2);
        assert_eq!(result.errors[0].kind, PreprocErrorKind::UnmatchedEndif);
    }

    #[test]
    fn missing_endif_error() {
        let src = "#if TMNEXT\nint x;\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].kind, PreprocErrorKind::MissingEndif);
    }

    #[test]
    fn elif_selects_when_if_false() {
        let src = "#if MP4\nint a;\n#elif TMNEXT\nint b;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(
            lines[1].chars().all(|c| c == ' '),
            "#if branch should be masked"
        );
        assert_eq!(lines[3], "int b;");
    }

    #[test]
    fn elif_skipped_when_if_true() {
        let src = "#if TMNEXT\nint a;\n#elif MP4\nint b;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT", "MP4"]));
        assert!(result.errors.is_empty());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert_eq!(lines[1], "int a;");
        assert!(
            lines[3].chars().all(|c| c == ' '),
            "#elif branch should be masked when #if was true"
        );
    }

    #[test]
    fn elif_false_falls_to_else() {
        let src = "#if MP4\nint a;\n#elif TURBO\nint b;\n#else\nint c;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(
            lines[1].chars().all(|c| c == ' '),
            "#if branch should be masked"
        );
        assert!(
            lines[3].chars().all(|c| c == ' '),
            "#elif branch should be masked"
        );
        assert_eq!(lines[5], "int c;");
    }

    #[test]
    fn if_else_active_branch() {
        // With TMNEXT: `int a` visible, `int b` masked
        let src = "#if TMNEXT\nint a;\n#else\nint b;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(lines[0].chars().all(|c| c == ' '));
        assert_eq!(lines[1], "int a;");
        assert!(lines[2].chars().all(|c| c == ' '));
        assert!(
            lines[3].chars().all(|c| c == ' '),
            "else branch should be masked"
        );
        assert!(lines[4].chars().all(|c| c == ' '));
    }
}
