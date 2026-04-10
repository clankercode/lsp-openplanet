use tower_lsp::lsp_types::*;
use crate::typedb::TypeIndex;

pub fn complete(
    _source: &str,
    _position: Position,
    _type_index: Option<&TypeIndex>,
) -> Vec<CompletionItem> {
    Vec::new() // Task 16
}
