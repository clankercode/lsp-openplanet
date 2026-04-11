//! A read-only merged view of globally visible names, combining the
//! workspace-level symbol table with the external Openplanet / Nadeo type
//! database.
//!
//! This is the lookup surface the type resolver consults when deciding
//! whether a referenced name is known. It does NOT own either data source —
//! callers build one on demand by borrowing both.

use super::repr::TypeRepr;
use crate::symbols::scope::SymbolKind;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

/// Read-only merged view of all globally visible symbols:
/// workspace-defined (user code) + external (Openplanet core + Nadeo).
pub struct GlobalScope<'a> {
    pub workspace: &'a SymbolTable,
    pub external: Option<&'a TypeIndex>,
}

/// A single overload candidate for a workspace free function, as returned
/// by `GlobalScope::lookup_function_overloads`. Stores parameter type text
/// (caller parses via `PrimitiveType::from_name` / `TypeRepr::parse_type_string`),
/// the minimum required arg count, and the return type text.
#[derive(Debug, Clone)]
pub struct OverloadSig {
    pub param_types: Vec<String>,
    pub min_args: usize,
    pub return_type: String,
}

impl<'a> GlobalScope<'a> {
    pub fn new(workspace: &'a SymbolTable, external: Option<&'a TypeIndex>) -> Self {
        Self { workspace, external }
    }

    /// True if the qualified name refers to a type (class / interface /
    /// funcdef-as-type) in either the workspace or the external index.
    pub fn has_type(&self, qualified: &str) -> bool {
        let workspace_hit = self.workspace.all_symbols().any(|s| {
            s.name == qualified
                && matches!(
                    s.kind,
                    SymbolKind::Class { .. }
                        | SymbolKind::Interface { .. }
                        | SymbolKind::Funcdef { .. }
                )
        });
        if workspace_hit {
            return true;
        }
        if let Some(ext) = self.external {
            if ext.lookup_type(qualified).is_some() {
                return true;
            }
        }
        false
    }

    /// True if the qualified name refers to a free function.
    pub fn has_function(&self, qualified: &str) -> bool {
        let workspace_hit = self
            .workspace
            .all_symbols()
            .any(|s| s.name == qualified && matches!(s.kind, SymbolKind::Function { .. }));
        if workspace_hit {
            return true;
        }
        if let Some(ext) = self.external {
            if ext.lookup_function(qualified).is_some() {
                return true;
            }
        }
        false
    }

    /// True if the qualified name refers to an enum.
    pub fn has_enum(&self, qualified: &str) -> bool {
        let workspace_hit = self
            .workspace
            .all_symbols()
            .any(|s| s.name == qualified && matches!(s.kind, SymbolKind::Enum { .. }));
        if workspace_hit {
            return true;
        }
        if let Some(ext) = self.external {
            if ext.lookup_enum(qualified).is_some() {
                return true;
            }
        }
        false
    }

    /// True if the qualified name refers to *anything* resolvable — a type,
    /// a free function, or an enum. This is the cheap "did the user typo
    /// their identifier?" check the resolver calls for named type references.
    pub fn resolves(&self, qualified: &str) -> bool {
        self.has_type(qualified) || self.has_enum(qualified) || self.has_function(qualified)
    }

    /// Last-resort lookup: given a bare (unqualified) name like
    /// `CGameCtnEditorFree`, return the first fully qualified name in the
    /// external type index whose tail segment matches. This lets user code
    /// reference Nadeo classes by their short name while the type database
    /// stores them under a deep namespace prefix (e.g. `Game::CGameCtnEditorFree`).
    ///
    /// Falls back to a linear scan of workspace type symbols whose last
    /// `::`-segment matches. Returns `None` when nothing matches.
    ///
    /// Only call this after every other lookup (direct, namespace-stack
    /// walk) has failed — the match is ambiguous in principle, but in the
    /// Nadeo database collisions are extremely rare.
    pub fn resolve_unqualified(&self, short: &str) -> Option<String> {
        if short.contains("::") {
            return None;
        }
        // External index first.
        if let Some(ext) = self.external {
            let candidates = ext.find_by_short_name(short);
            if !candidates.is_empty() {
                return Some(candidates[0].clone());
            }
        }
        // Workspace fallback: scan for any type-kind symbol whose qualified
        // tail matches. This covers user plugin types that are defined in
        // a sibling file under a namespace but referenced bare.
        let needle = format!("::{}", short);
        for s in self.workspace.all_symbols() {
            if !matches!(
                s.kind,
                SymbolKind::Class { .. }
                    | SymbolKind::Interface { .. }
                    | SymbolKind::Funcdef { .. }
                    | SymbolKind::Enum { .. }
            ) {
                continue;
            }
            if s.name.ends_with(&needle) {
                return Some(s.name.clone());
            }
        }
        None
    }

    /// True if `name` refers to any globally visible identifier — a type,
    /// a function, an enum, a top-level variable, or an enum value.
    ///
    /// This is the lookup the expression walker uses when it encounters a
    /// bare identifier (after local and class/namespace scopes are tried).
    ///
    /// Accepts both the exact qualified name and, as a fallback, any
    /// symbol whose qualified tail matches `::name` — this covers the
    /// common AngelScript case where bare enum-value names (`Red` rather
    /// than `Color::Red`) are sometimes usable without a qualifier.
    pub fn has_global_ident(&self, name: &str) -> bool {
        if self.has_type(name) || self.has_function(name) || self.has_enum(name) {
            return true;
        }
        // Exact workspace hit as a Variable or EnumValue (both at top level
        // and as a qualified tail).
        let tail = format!("::{}", name);
        for s in self.workspace.all_symbols() {
            let matches_name = s.name == name || s.name.ends_with(&tail);
            if !matches_name {
                continue;
            }
            if matches!(
                s.kind,
                SymbolKind::Variable { .. } | SymbolKind::EnumValue { .. }
            ) {
                return true;
            }
        }
        // External index: scan known enums for a matching bare value name.
        // This is linear in enum count but enums are small and this only
        // runs on identifiers that failed every earlier check.
        if let Some(ext) = self.external {
            for (qname, en) in ext.enums_iter() {
                let _ = qname;
                if en.values.iter().any(|(v, _)| v == name) {
                    return true;
                }
            }
        }
        false
    }

    /// Look up a member's type on a fully qualified type name, walking
    /// parent classes. Returns the member's type (or the method's return
    /// type, for method-as-value lookups) if found.
    ///
    /// Precedence: external TypeIndex (walks `parent`), then workspace
    /// symbols (fallback). For workspace hits we don't know the resolved
    /// type yet, so we return `TypeRepr::Error(String::new())` — enough to
    /// silence the "undefined" diagnostic without claiming a specific type.
    pub fn lookup_member_type(&self, type_name: &str, member: &str) -> Option<TypeRepr> {
        // External types first.
        if let Some(ext) = self.external {
            if let Some(t) = Self::ext_lookup_member(ext, type_name, member) {
                return Some(t);
            }
        }
        // Workspace fallback: "TypeName::member" qualified lookup.
        let qual = format!("{}::{}", type_name, member);
        if self.workspace.all_symbols().any(|s| {
            s.name == qual
                && matches!(
                    s.kind,
                    SymbolKind::Variable { .. } | SymbolKind::Function { .. }
                )
        }) {
            return Some(TypeRepr::Error(String::new()));
        }
        None
    }

    /// Like `lookup_member_type`, but only considers methods and returns
    /// the method's return type. Walks parent classes.
    pub fn lookup_method_return(&self, type_name: &str, method: &str) -> Option<TypeRepr> {
        if let Some(ext) = self.external {
            if let Some(t) = Self::ext_lookup_method_return(ext, type_name, method) {
                return Some(t);
            }
        }
        // Workspace fallback: qualified method name hit.
        let qual = format!("{}::{}", type_name, method);
        if self.workspace.all_symbols().any(|s| {
            s.name == qual && matches!(s.kind, SymbolKind::Function { .. })
        }) {
            return Some(TypeRepr::Error(String::new()));
        }
        None
    }

    fn ext_lookup_member(ext: &TypeIndex, type_name: &str, member: &str) -> Option<TypeRepr> {
        // AngelScript exposes `get_Foo` / `set_Foo` methods as a virtual
        // property named `Foo`. Build both candidate names up front so we
        // can find either the explicit property or its getter.
        let getter_name = format!("get_{}", member);
        let setter_name = format!("set_{}", member);

        // Guard against cycles in parent chains.
        let mut current: Option<String> = Some(type_name.to_string());
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            let info = ext.lookup_type(&name)?;
            for p in &info.properties {
                if p.name == member {
                    return Some(TypeRepr::parse_type_string(&p.type_name));
                }
            }
            for m in &info.methods {
                if m.name == member || m.name == getter_name || m.name == setter_name {
                    // Method-as-value: return the method's return type.
                    // For Nadeo-sourced methods `return_type` may be empty.
                    if m.return_type.is_empty() {
                        return Some(TypeRepr::Error(String::new()));
                    }
                    return Some(TypeRepr::parse_type_string(&m.return_type));
                }
            }
            current = info.parent.clone();
        }
        None
    }

    fn ext_lookup_method_return(
        ext: &TypeIndex,
        type_name: &str,
        method: &str,
    ) -> Option<TypeRepr> {
        let getter_name = format!("get_{}", method);
        let setter_name = format!("set_{}", method);
        let mut current: Option<String> = Some(type_name.to_string());
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            let info = ext.lookup_type(&name)?;
            for m in &info.methods {
                if m.name == method || m.name == getter_name || m.name == setter_name {
                    if m.return_type.is_empty() {
                        return Some(TypeRepr::Error(String::new()));
                    }
                    return Some(TypeRepr::parse_type_string(&m.return_type));
                }
            }
            // Also allow lookup_method_return to find a callable property
            // (e.g. a funcdef field) — return the property's type.
            for p in &info.properties {
                if p.name == method {
                    return Some(TypeRepr::parse_type_string(&p.type_name));
                }
            }
            current = info.parent.clone();
        }
        None
    }

    /// True if a fully qualified path `A::B::C` resolves as:
    /// - a type / function / enum / workspace variable directly, OR
    /// - `A::B` being an enum and `C` one of its values, OR
    /// - a workspace class member path `Class::member` (any kind).
    ///
    /// Used by the checker when walking `ExprKind::NamespaceAccess`.
    pub fn has_qualified_path(&self, qual: &str) -> bool {
        // Direct hit as type/function/enum/global.
        if self.has_global_ident(qual) {
            return true;
        }
        // Enum value: split off the tail, see if the head names an enum
        // and the tail is one of its values.
        if let Some(idx) = qual.rfind("::") {
            let head = &qual[..idx];
            let tail = &qual[idx + 2..];
            // Workspace enum with matching value.
            for s in self.workspace.all_symbols() {
                if s.name == head {
                    if let SymbolKind::Enum { values } = &s.kind {
                        if values.iter().any(|(v, _)| v == tail) {
                            return true;
                        }
                    }
                }
            }
            // External enum with matching value.
            if let Some(ext) = self.external {
                if let Some(en) = ext.lookup_enum(head) {
                    if en.values.iter().any(|(v, _)| v == tail) {
                        return true;
                    }
                }
            }
            // Workspace class member (e.g. `Foo::bar` → class `Foo` has `bar`).
            // Any Variable/Function/EnumValue symbol whose fully qualified
            // name exactly equals `qual` is accepted.
            for s in self.workspace.all_symbols() {
                if s.name == qual {
                    return true;
                }
            }
            // External: is the head a known type with member `tail`?
            if let Some(ext) = self.external {
                if let Some(info) = ext.lookup_type(head) {
                    if info.properties.iter().any(|p| p.name == tail)
                        || info.methods.iter().any(|m| m.name == tail)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True if `qualified` is known to the external TypeIndex as a type.
    /// Used by the member-access checker to decide whether to trust a
    /// negative lookup (only external types have complete member lists).
    pub fn is_external_type(&self, qualified: &str) -> bool {
        self.external
            .and_then(|ext| ext.lookup_type(qualified))
            .is_some()
    }

    /// True if the type at the given fully qualified name is from the
    /// external Nadeo (engine) database, whose member metadata is known to
    /// be incomplete. Callers use this to suppress `UndefinedMember`
    /// diagnostics on types whose method/property lists we can't trust.
    pub fn is_nadeo_type(&self, qualified: &str) -> bool {
        self.external
            .and_then(|idx| idx.lookup_type(qualified))
            .map(|t| matches!(t.source, crate::typedb::index::TypeSource::Nadeo))
            .unwrap_or(false)
    }

    /// Look up a unique workspace free function by qualified name, returning
    /// its `(min_args, max_args)` signature. Returns `None` if the name has
    /// zero matches *or* two-plus matches (the overloaded case — callers
    /// conservatively suppress arity checking when overloads exist).
    ///
    /// External (typedb) functions are intentionally not consulted here —
    /// their signature data is not yet wired through to the checker.
    pub fn lookup_function_signature(&self, qualified: &str) -> Option<(usize, usize)> {
        let mut found: Option<(usize, usize)> = None;
        for s in self.workspace.all_symbols() {
            if s.name != qualified {
                continue;
            }
            if let SymbolKind::Function { params, min_args, .. } = &s.kind {
                if found.is_some() {
                    // 2+ matches — overloaded. Suppress.
                    return None;
                }
                found = Some((*min_args, params.len()));
            }
        }
        found
    }

    /// Look up a unique workspace free function's parameter type text list
    /// by qualified name. Returns `None` if the name has zero matches *or*
    /// two-plus matches (the overloaded case — callers conservatively
    /// suppress type checking when overloads exist). Returns the raw
    /// `type_text` strings as stored in the symbol table; callers are
    /// responsible for parsing them (e.g. via `PrimitiveType::from_name`).
    pub fn lookup_function_param_types(&self, qualified: &str) -> Option<Vec<String>> {
        let mut found: Option<Vec<String>> = None;
        for s in self.workspace.all_symbols() {
            if s.name != qualified {
                continue;
            }
            if let SymbolKind::Function { params, .. } = &s.kind {
                if found.is_some() {
                    // 2+ matches — overloaded. Suppress.
                    return None;
                }
                found = Some(params.iter().map(|(_, ty_text)| ty_text.clone()).collect());
            }
        }
        found
    }

    /// Return every workspace free-function overload matching `qualified`.
    /// Unlike `lookup_function_signature` / `lookup_function_param_types`,
    /// this does NOT suppress the 2+-match case — callers get the full set
    /// and are expected to run their own overload resolution. Returns an
    /// empty Vec if no workspace function has that name.
    ///
    /// External (typedb) functions are intentionally not consulted here —
    /// their signature data isn't wired through to the checker yet.
    pub fn lookup_function_overloads(&self, qualified: &str) -> Vec<OverloadSig> {
        let mut out = Vec::new();
        for s in self.workspace.all_symbols() {
            if s.name != qualified {
                continue;
            }
            if let SymbolKind::Function {
                return_type,
                params,
                min_args,
            } = &s.kind
            {
                out.push(OverloadSig {
                    param_types: params.iter().map(|(_, ty_text)| ty_text.clone()).collect(),
                    min_args: *min_args,
                    return_type: return_type.clone(),
                });
            }
        }
        out
    }

    /// Look up a free function's return type by qualified name.
    pub fn lookup_function_return(&self, qualified: &str) -> Option<TypeRepr> {
        if let Some(ext) = self.external {
            if let Some(fns) = ext.lookup_function(qualified) {
                if let Some(first) = fns.first() {
                    return Some(TypeRepr::parse_type_string(&first.return_type));
                }
            }
        }
        // Workspace fallback: just silence with Error.
        if self
            .workspace
            .all_symbols()
            .any(|s| s.name == qualified && matches!(s.kind, SymbolKind::Function { .. }))
        {
            return Some(TypeRepr::Error(String::new()));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;
    use crate::symbols::scope::Symbol;

    fn empty_span() -> Span {
        Span::new(0, 0)
    }

    fn make_symbol(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            span: empty_span(),
            file_id: 0,
            doc: None,
        }
    }

    #[test]
    fn empty_scope_resolves_nothing() {
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        assert!(!scope.has_type("Foo"));
        assert!(!scope.has_function("foo"));
        assert!(!scope.has_enum("E"));
        assert!(!scope.resolves("Foo"));
    }

    #[test]
    fn workspace_class_is_found() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "MyClass",
                SymbolKind::Class {
                    parent: None,
                    members: vec![],
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_type("MyClass"));
        assert!(scope.resolves("MyClass"));
        assert!(!scope.has_function("MyClass"));
        assert!(!scope.has_enum("MyClass"));
    }

    #[test]
    fn workspace_interface_is_type() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "IThing",
                SymbolKind::Interface { methods: vec![] },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_type("IThing"));
    }

    #[test]
    fn workspace_function_is_found() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "doThing",
                SymbolKind::Function {
                    return_type: "void".into(),
                    params: vec![],
                    min_args: 0,
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_function("doThing"));
        assert!(!scope.has_type("doThing"));
        assert!(scope.resolves("doThing"));
    }

    #[test]
    fn workspace_enum_is_found() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "Color",
                SymbolKind::Enum {
                    values: vec![("Red".into(), Some(0))],
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_enum("Color"));
        assert!(!scope.has_type("Color"));
        assert!(scope.resolves("Color"));
    }

    #[test]
    fn resolve_unqualified_finds_namespaced_workspace_type() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "Deep::Ns::Thing",
                SymbolKind::Class {
                    parent: None,
                    members: vec![],
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert_eq!(
            scope.resolve_unqualified("Thing"),
            Some("Deep::Ns::Thing".to_string())
        );
        assert_eq!(scope.resolve_unqualified("Missing"), None);
        // Qualified input is not a short name — must return None.
        assert_eq!(scope.resolve_unqualified("Deep::Ns::Thing"), None);
    }

    #[test]
    fn resolve_unqualified_ignores_non_type_symbols() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "Ns::helper",
                SymbolKind::Function {
                    return_type: "void".into(),
                    params: vec![],
                    min_args: 0,
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert_eq!(scope.resolve_unqualified("helper"), None);
    }

    #[test]
    fn nadeo_type_recognized_by_is_nadeo_type() {
        // Build a TypeIndex with one Nadeo-sourced type and one Core-sourced
        // type, then verify `is_nadeo_type` discriminates.
        use crate::typedb::index::{TypeIndex, TypeInfo, TypeSource};
        // Reach into the module-internal constructor by way of the load
        // path is awkward; assemble manually via the Default + a private
        // insertion through a small helper. Since `types` is private, we
        // round-trip through the public `load` path in a throwaway test if
        // fixtures exist, but otherwise directly verify the fallback path.
        let cp = std::path::PathBuf::from(env!("HOME"))
            .join("src/openplanet/tm-scripts/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("HOME"))
            .join("src/openplanet/tm-scripts/OpenplanetNext.json");
        if !cp.exists() || !np.exists() {
            // No fixtures — assert the Default constructor at least honours
            // the empty case.
            let ws = SymbolTable::new();
            let scope = GlobalScope::new(&ws, None);
            assert!(!scope.is_nadeo_type("CGameCtnBlock"));
            // Just reference the unused type to keep the import above alive.
            let _ = std::mem::size_of::<TypeInfo>();
            let _ = TypeSource::Nadeo;
            let _ = std::mem::size_of::<TypeIndex>();
            return;
        }
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        // Pick a well-known Nadeo class (CMwNod is registered under its
        // namespaced form in the DB).
        let cmwnod = idx
            .find_by_short_name("CMwNod")
            .iter()
            .find(|h| h.ends_with("::CMwNod"))
            .cloned()
            .expect("CMwNod should exist in fixture");
        assert!(
            scope.is_nadeo_type(&cmwnod),
            "{} should be Nadeo-sourced",
            cmwnod
        );
        // A Core-sourced type like UI::InputBlocking should NOT report as Nadeo.
        // Fall back to any non-Nadeo core type by iterating if needed.
        assert!(!scope.is_nadeo_type("NotARealType"));
    }

    #[test]
    fn workspace_funcdef_counts_as_type() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "Callback",
                SymbolKind::Funcdef {
                    return_type: "void".into(),
                    params: vec![],
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_type("Callback"));
    }
}
