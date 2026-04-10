use std::collections::HashSet;

/// Tokens produced by the condition tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CondToken {
    Define(String),
    Not,
    And,
    Or,
}

/// Tokenize a preprocessor condition string into `CondToken`s.
///
/// Recognized tokens: bare identifiers (define names), `!`, `&&`, `||`.
/// Whitespace is skipped. Unknown characters are silently skipped.
pub fn tokenize_condition(cond: &str) -> Vec<CondToken> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = cond.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\r' | '\n' => {
                i += 1;
            }
            '!' => {
                tokens.push(CondToken::Not);
                i += 1;
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                tokens.push(CondToken::And);
                i += 2;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                tokens.push(CondToken::Or);
                i += 2;
            }
            c if c.is_alphanumeric() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                tokens.push(CondToken::Define(name));
            }
            _ => {
                i += 1;
            }
        }
    }

    tokens
}

/// Evaluate a preprocessor condition against a set of active defines.
///
/// Operators evaluate strictly left-to-right with no precedence:
/// - `!` binds to the immediately following define name only
/// - `&&` and `||` are evaluated left-to-right
///
/// Returns `false` for empty or malformed conditions.
pub fn eval_condition(cond: &str, defines: &HashSet<String>) -> bool {
    let tokens = tokenize_condition(cond);
    if tokens.is_empty() {
        return false;
    }

    let mut iter = tokens.into_iter().peekable();

    // Parse the first value
    let mut result = match parse_value(&mut iter, defines) {
        Some(v) => v,
        None => return false,
    };

    // Process remaining && / || operators left-to-right
    loop {
        match iter.next() {
            Some(CondToken::And) => {
                let rhs = match parse_value(&mut iter, defines) {
                    Some(v) => v,
                    None => break,
                };
                result = result && rhs;
            }
            Some(CondToken::Or) => {
                let rhs = match parse_value(&mut iter, defines) {
                    Some(v) => v,
                    None => break,
                };
                result = result || rhs;
            }
            _ => break,
        }
    }

    result
}

/// Parse a single boolean value: optionally-negated define name.
fn parse_value(
    iter: &mut std::iter::Peekable<std::vec::IntoIter<CondToken>>,
    defines: &HashSet<String>,
) -> Option<bool> {
    match iter.next()? {
        CondToken::Not => {
            // `!` must be followed by a define name
            match iter.next()? {
                CondToken::Define(name) => Some(!defines.contains(&name)),
                _ => None,
            }
        }
        CondToken::Define(name) => Some(defines.contains(&name)),
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn defines(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn simple_define_true() {
        let d = defines(&["TMNEXT"]);
        assert!(eval_condition("TMNEXT", &d));
    }

    #[test]
    fn simple_define_false() {
        let d = defines(&[]);
        assert!(!eval_condition("TMNEXT", &d));
    }

    #[test]
    fn negation_true() {
        let d = defines(&[]);
        assert!(eval_condition("!TMNEXT", &d));
    }

    #[test]
    fn negation_false() {
        let d = defines(&["TMNEXT"]);
        assert!(!eval_condition("!TMNEXT", &d));
    }

    #[test]
    fn and_both_present() {
        let d = defines(&["TMNEXT", "SIG_DEVELOPER"]);
        assert!(eval_condition("TMNEXT && SIG_DEVELOPER", &d));
    }

    #[test]
    fn and_one_missing() {
        let d = defines(&["TMNEXT"]);
        assert!(!eval_condition("TMNEXT && SIG_DEVELOPER", &d));
    }

    #[test]
    fn or_first_present() {
        let d = defines(&["TMNEXT"]);
        assert!(eval_condition("TMNEXT || MP4", &d));
    }

    #[test]
    fn or_second_present() {
        let d = defines(&["MP4"]);
        assert!(eval_condition("TMNEXT || MP4", &d));
    }

    #[test]
    fn or_neither_present() {
        let d = defines(&[]);
        assert!(!eval_condition("TMNEXT || MP4", &d));
    }

    #[test]
    fn negation_with_and() {
        // "!DEV && TMNEXT" — real pattern from plugins
        let d = defines(&["TMNEXT"]);
        assert!(eval_condition("!DEV && TMNEXT", &d));

        let d2 = defines(&["TMNEXT", "DEV"]);
        assert!(!eval_condition("!DEV && TMNEXT", &d2));
    }

    #[test]
    fn left_to_right_no_precedence() {
        // "TMNEXT || MP4 && TURBO" with only TMNEXT:
        // left-to-right: (TMNEXT || MP4) = true, then true && TURBO = false
        let d = defines(&["TMNEXT"]);
        assert!(!eval_condition("TMNEXT || MP4 && TURBO", &d));
    }
}
