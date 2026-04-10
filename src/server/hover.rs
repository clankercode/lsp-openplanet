use tower_lsp::lsp_types::*;
use crate::typedb::TypeIndex;

pub fn hover(
    _source: &str,
    _position: Position,
    _type_index: Option<&TypeIndex>,
) -> Option<Hover> {
    None // Task 16
}
