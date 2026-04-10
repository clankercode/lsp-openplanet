use std::collections::HashSet;

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
    let mut masked = source.to_string();
    let mut errors: Vec<PreprocError> = Vec::new();

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

        if let Some(directive) = parse_directive(trimmed) {
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

        line_start = line_end;
    }

    // Any unclosed #if blocks.
    for _ in 0..stack.len() {
        errors.push(PreprocError {
            line: line_num,
            kind: PreprocErrorKind::MissingEndif,
        });
    }

    PreprocessResult {
        masked_source: masked,
        errors,
    }
}

/// Replace every non-newline byte in `masked[start..end]` with a space,
/// preserving newlines so line numbers remain correct.
fn mask_range(masked: &mut String, start: usize, end: usize) {
    // Safety: we only replace ASCII-safe bytes (non-newline bytes with 0x20)
    // within valid UTF-8. Spaces and newlines are both single-byte ASCII, so
    // replacing any non-newline byte with 0x20 keeps the string valid UTF-8
    // only when the replaced bytes are themselves single-byte (ASCII).
    //
    // For correctness we iterate over characters. We rebuild the relevant
    // slice by operating on bytes directly for multi-byte chars: we replace
    // every byte that is not part of a newline with 0x20.  This is valid
    // because 0x20 (space) is a single ASCII byte, and any multi-byte UTF-8
    // sequence's continuation bytes all have the high bit set (>= 0x80), so
    // they cannot be confused with ASCII.  Replacing them with 0x20 reduces
    // a multi-byte char to a single-byte space—changing length would break
    // offsets.  Instead, we must preserve length: replace every byte with
    // 0x20 except newline bytes (0x0A).
    //
    // Replacing continuation bytes (0x80–0xBF) and leading bytes with 0x20
    // would break UTF-8.  However the source is known ASCII-compatible
    // AngelScript, so non-ASCII chars should not appear in directive lines or
    // code we wish to mask except inside string literals.  For now we do a
    // byte-level replacement preserving length.
    unsafe {
        let bytes = masked.as_bytes_mut();
        for b in bytes[start..end].iter_mut() {
            if *b != b'\n' && *b != b'\r' {
                *b = b' ';
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn def(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
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
        assert!(lines[0].chars().all(|c| c == ' '), "directive line should be spaces");
        assert_eq!(lines[1], "int x = 1;");
        assert!(lines[2].chars().all(|c| c == ' '), "endif line should be spaces");
    }

    #[test]
    fn simple_if_inactive() {
        let src = "#if TMNEXT\nint x = 1;\n#endif\n";
        let result = preprocess(src, &def(&[]));
        assert!(result.errors.is_empty());
        assert_eq!(result.masked_source.len(), src.len());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(lines[0].chars().all(|c| c == ' '));
        assert!(lines[1].chars().all(|c| c == ' '), "inactive content should be masked");
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
        assert!(lines[2].chars().all(|c| c == ' '), "#if line should be masked");
        assert_eq!(lines[3], "    float m_breakCoef;");
        assert!(lines[4].chars().all(|c| c == ' '), "#endif line should be masked");
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
    fn if_else_active_branch() {
        // With TMNEXT: `int a` visible, `int b` masked
        let src = "#if TMNEXT\nint a;\n#else\nint b;\n#endif\n";
        let result = preprocess(src, &def(&["TMNEXT"]));
        assert!(result.errors.is_empty());
        let lines: Vec<&str> = result.masked_source.lines().collect();
        assert!(lines[0].chars().all(|c| c == ' '));
        assert_eq!(lines[1], "int a;");
        assert!(lines[2].chars().all(|c| c == ' '));
        assert!(lines[3].chars().all(|c| c == ' '), "else branch should be masked");
        assert!(lines[4].chars().all(|c| c == ' '));
    }
}
