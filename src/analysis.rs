//! Document analysis snapshot — one seam for preprocess → lex → parse.
//!
//! Callers (diagnostics, completion, hover, signature help, …) should consume
//! [`DocumentAnalysis`] instead of each re-running the pipeline with divergent
//! inputs. Workspace/dependency symbols stay outside this module (see
//! [`crate::workspace::load`]); pair them at the call site.

use std::collections::HashSet;

use crate::lexer::{self, Token};
use crate::parser::ast::SourceFile;
use crate::parser::error::ParseError;
use crate::parser::Parser;
use crate::preprocessor::{self, PreprocError, PreprocessResult};

/// Parsed view of a single document under a define set.
#[derive(Debug)]
pub struct DocumentAnalysis {
    /// Original source text (as provided by the editor / CLI).
    pub source: String,
    /// Preprocessor output (masked source + errors).
    pub preprocess: PreprocessResult,
    /// Filtered tokens over the masked source.
    pub tokens: Vec<Token>,
    /// Parsed AST (may be partial when parse errors exist).
    pub file: SourceFile,
    /// Parser errors accumulated during parse.
    pub parse_errors: Vec<ParseError>,
}

impl DocumentAnalysis {
    /// Analyze `source` with the given preprocessor defines.
    pub fn analyze(source: &str, defines: &HashSet<String>) -> Self {
        let preprocess = preprocessor::preprocess(source, defines);
        let tokens = lexer::tokenize_filtered(&preprocess.masked_source);
        let mut parser = Parser::new(&tokens, &preprocess.masked_source);
        let file = parser.parse_file();
        let parse_errors = parser.errors;
        Self {
            source: source.to_string(),
            preprocess,
            tokens,
            file,
            parse_errors,
        }
    }

    /// Analyze with no defines (empty set).
    pub fn analyze_plain(source: &str) -> Self {
        Self::analyze(source, &HashSet::new())
    }

    /// Convenience: preprocess errors only.
    pub fn preprocess_errors(&self) -> &[PreprocError] {
        &self.preprocess.errors
    }

    /// Masked source used for spans/AST (post-preprocessor).
    pub fn masked_source(&self) -> &str {
        &self.preprocess.masked_source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_parses_simple_function() {
        let a = DocumentAnalysis::analyze_plain("void Main() {}");
        assert!(a.parse_errors.is_empty());
        assert!(a.preprocess_errors().is_empty());
        assert!(!a.file.items.is_empty());
    }

    #[test]
    fn analyze_respects_defines() {
        let src = r#"
#if FEATURE
void Enabled() {}
#endif
"#;
        let off = DocumentAnalysis::analyze_plain(src);
        let mut defs = HashSet::new();
        defs.insert("FEATURE".into());
        let on = DocumentAnalysis::analyze(src, &defs);
        assert!(on.masked_source().contains("Enabled"));
        assert!(!off.masked_source().contains("Enabled") || off.file.items.is_empty());
    }
}
