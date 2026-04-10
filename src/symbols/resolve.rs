use super::table::SymbolTable;
use super::scope::Symbol;
use crate::typedb::index::TypeIndex;

/// Resolution order (spec Section 6.3):
/// 1. Local scope (block → function → class)
/// 2. File-level globals
/// 3. Module-level globals (all files)
/// 4. Dependency-exported symbols
/// 5. OpenPlanet API symbols (type DB)
/// 6. Namespace-qualified: skip to named namespace
pub fn resolve_name<'a>(
    name: &str,
    symbol_table: &'a SymbolTable,
    type_index: &'a TypeIndex,
) -> Option<ResolvedSymbol<'a>> {
    // Module-level (workspace) symbols
    let user_symbols = symbol_table.lookup(name);
    if !user_symbols.is_empty() {
        return Some(ResolvedSymbol::UserDefined(user_symbols));
    }

    // Type DB — try as type, function, or enum
    if type_index.lookup_type(name).is_some() {
        return Some(ResolvedSymbol::ApiType(name.to_string()));
    }
    if type_index.lookup_function(name).is_some() {
        return Some(ResolvedSymbol::ApiFunction(name.to_string()));
    }
    if type_index.lookup_enum(name).is_some() {
        return Some(ResolvedSymbol::ApiEnum(name.to_string()));
    }

    None
}

#[derive(Debug)]
pub enum ResolvedSymbol<'a> {
    UserDefined(Vec<&'a Symbol>),
    ApiType(String),
    ApiFunction(String),
    ApiEnum(String),
}
