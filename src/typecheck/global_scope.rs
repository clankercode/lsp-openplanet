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
/// by `GlobalScope::lookup_function_overloads`. Stores parameter names and
/// type text (callers parse types via `PrimitiveType::from_name` /
/// `TypeRepr::parse_type_string`); missing typedb names remain `None` to
/// preserve alignment with parameter types. Also stores the minimum required
/// arg count and return type text.
#[derive(Debug, Clone)]
pub struct OverloadSig {
    pub param_names: Vec<Option<String>>,
    pub param_types: Vec<String>,
    pub min_args: usize,
    pub return_type: String,
}

impl<'a> GlobalScope<'a> {
    pub fn new(workspace: &'a SymbolTable, external: Option<&'a TypeIndex>) -> Self {
        Self {
            workspace,
            external,
        }
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

    /// Last-resort lookup for a partially qualified path whose fully
    /// qualified external/workspace name may carry an additional leading
    /// namespace segment.
    ///
    /// Example: user code may write
    /// `CGameEditorPluginMap::ECardinalDirections` while the typedb stores
    /// `Game::CGameEditorPluginMap::ECardinalDirections`.
    pub fn resolve_qualified_suffix(&self, qualified: &str) -> Option<String> {
        if !qualified.contains("::") {
            return None;
        }

        let needle = format!("::{}", qualified);
        let short = qualified.rsplit("::").next()?;

        if let Some(ext) = self.external {
            for candidate in ext.find_by_short_name(short) {
                if candidate.ends_with(&needle) {
                    return Some(candidate.clone());
                }
            }
        }

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
        let getter = qualified_virtual_name(name, "get_");
        let setter = qualified_virtual_name(name, "set_");
        if self.has_function(&getter) || self.has_function(&setter) {
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
            if self.tail_prefix_is_class_member(&s.name, &tail) {
                // `ClassName::field` is a class member, not a global — a
                // bare name matching it stays undefined (GH #30).
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

    /// GH #30: `ClassName::field` / `ClassName::method` symbols are class
    /// members, not globals. When a *bare* name tail-matches such a symbol,
    /// the prefix chain must not be treated as a namespace — otherwise any
    /// sibling class's field silently "resolves" the bare name.
    ///
    /// Walks each `::`-separated prefix segment of the symbol name; if any
    /// prefix segment names a class/interface, the symbol is a class member.
    fn tail_prefix_is_class_member(&self, symbol_name: &str, tail: &str) -> bool {
        let Some(prefix) = symbol_name.strip_suffix(tail) else {
            return false;
        };
        // Build progressively longer prefixes; if any names a class or
        // interface, this is a member symbol. Namespaces are never classes,
        // so `A::B::var` with A/B namespaces stays a legitimate global.
        let segments: Vec<&str> = prefix.split("::").collect();
        let mut acc = String::new();
        for seg in segments {
            if seg.is_empty() {
                continue;
            }
            if !acc.is_empty() {
                acc.push_str("::");
            }
            acc.push_str(seg);
            if self.workspace.all_symbols().any(|s| {
                s.name == acc
                    && matches!(
                        s.kind,
                        SymbolKind::Class { .. } | SymbolKind::Interface { .. }
                    )
            }) {
                return true;
            }
        }
        false
    }

    /// Look up a member's type on a fully qualified type name, walking
    /// parent classes. Returns the member's type (or the method's return
    /// type, for method-as-value lookups) if found.
    ///
    /// Precedence: external TypeIndex (walks `parent`), then workspace
    /// symbols (fallback). Workspace hits parse the type text stored by
    /// the symbol extractor (iter 28) into a real `TypeRepr`. An empty
    /// stored string parses to `Error("")` — still a valid silence
    /// sentinel for suppressing `UndefinedMember`.
    pub fn lookup_member_type(&self, type_name: &str, member: &str) -> Option<TypeRepr> {
        // External types first.
        if let Some(ext) = self.external {
            if let Some(t) = Self::ext_lookup_member(ext, type_name, member) {
                return Some(t);
            }
        }
        self.workspace_class_member(type_name, member)
    }

    /// Like `lookup_member_type`, but only considers methods and returns
    /// the method's return type. Walks parent classes.
    pub fn lookup_method_return(&self, type_name: &str, method: &str) -> Option<TypeRepr> {
        if let Some(ext) = self.external {
            if let Some(t) = Self::ext_lookup_method_return(ext, type_name, method) {
                return Some(t);
            }
        }
        self.workspace_class_member(type_name, method)
    }

    /// Resolve `name` to a fully qualified external type key. Accepts either
    /// an already-qualified name or a Nadeo short name (`CMwNod`).
    fn resolve_external_type_name(ext: &TypeIndex, name: &str) -> Option<String> {
        if ext.lookup_type(name).is_some() {
            return Some(name.to_string());
        }
        if name.contains("::") {
            return None;
        }
        ext.find_by_short_name(name).first().cloned()
    }

    fn ext_lookup_member(ext: &TypeIndex, type_name: &str, member: &str) -> Option<TypeRepr> {
        // AngelScript exposes `get_Foo` / `set_Foo` methods as a virtual
        // property named `Foo`. Build both candidate names up front so we
        // can find either the explicit property or its getter.
        let getter_name = format!("get_{}", member);
        let setter_name = format!("set_{}", member);

        // Guard against cycles in parent chains. Nadeo parents are stored as
        // short names (`CMwNod`), so resolve each hop through the short-name
        // index.
        let mut current: Option<String> = Self::resolve_external_type_name(ext, type_name);
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
            current = info
                .parent
                .as_ref()
                .and_then(|p| Self::resolve_external_type_name(ext, p));
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
        let mut current: Option<String> = Self::resolve_external_type_name(ext, type_name);
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
            current = info
                .parent
                .as_ref()
                .and_then(|p| Self::resolve_external_type_name(ext, p));
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

    /// Look up external type info by fully qualified name or Nadeo short name.
    fn external_type_info(&self, name: &str) -> Option<&crate::typedb::index::TypeInfo> {
        let ext = self.external?;
        if let Some(t) = ext.lookup_type(name) {
            return Some(t);
        }
        if name.contains("::") {
            return None;
        }
        let q = ext.find_by_short_name(name).first()?;
        ext.lookup_type(q)
    }

    /// True if `qualified` is known to the external TypeIndex as a type.
    /// Used by the member-access checker to decide whether to trust a
    /// negative lookup (only external types have complete member lists).
    /// Accepts fully qualified names and Nadeo short names.
    pub fn is_external_type(&self, qualified: &str) -> bool {
        self.external_type_info(qualified).is_some()
    }

    /// True if the type at the given name is from the external Nadeo
    /// (engine) database. Accepts fully qualified names and short names.
    ///
    /// Historically callers suppressed `UndefinedMember` for all Nadeo
    /// types because member metadata can be incomplete. Prefer
    /// [`Self::nadeo_member_list_trusted`] to decide whether a negative
    /// lookup is trustworthy.
    pub fn is_nadeo_type(&self, qualified: &str) -> bool {
        self.external_type_info(qualified)
            .map(|t| matches!(t.source, crate::typedb::index::TypeSource::Nadeo))
            .unwrap_or(false)
    }

    /// True when a Nadeo type has a non-empty `properties` + `methods`
    /// list in the typedb. A failed member lookup against such a type can
    /// be reported as `UndefinedMember` (trust positive completeness of
    /// the listed API). Types with zero listed members stay silent —
    /// their metadata is treated as incomplete (B006).
    pub fn nadeo_member_list_trusted(&self, qualified: &str) -> bool {
        self.external_type_info(qualified)
            .map(|t| {
                matches!(t.source, crate::typedb::index::TypeSource::Nadeo)
                    && (!t.properties.is_empty() || !t.methods.is_empty())
            })
            .unwrap_or(false)
    }

    /// Look up a unique workspace free function by qualified name, returning
    /// its `(min_args, max_args)` signature. Returns `None` if the name has
    /// zero matches *or* two-plus matches (the overloaded case — callers
    /// conservatively suppress arity checking when overloads exist).
    ///
    /// When no workspace function matches, falls back to a unique external
    /// (typedb) free-function signature. Multi-overload external names still
    /// return `None` here — use `lookup_external_function_arity_ranges` for
    /// the "no overload accepts this arity" check.
    pub fn lookup_function_signature(&self, qualified: &str) -> Option<(usize, usize)> {
        if let Some(s) = lookup_workspace_function_property(&self.workspace, qualified) {
            return match &s.kind {
                SymbolKind::Function {
                    params, min_args, ..
                } => Some((*min_args, params.len())),
                _ => unreachable!(),
            };
        }
        let ranges = self.lookup_external_function_arity_ranges(qualified)?;
        if ranges.len() == 1 {
            Some(ranges[0])
        } else {
            None
        }
    }

    /// Return every external (typedb) free-function overload's
    /// `(min_args, max_args)` range for `qualified`. Empty/`None` when the
    /// name is not an external function. Defaults (params with `default`
    /// set) lower `min_args`.
    pub fn lookup_external_function_arity_ranges(
        &self,
        qualified: &str,
    ) -> Option<Vec<(usize, usize)>> {
        let ext = self.external?;
        let fns = ext.lookup_function(qualified)?;
        if fns.is_empty() {
            return None;
        }
        Some(
            fns.iter()
                .map(|f| {
                    let min_args = f.params.iter().filter(|p| p.default.is_none()).count();
                    (min_args, f.params.len())
                })
                .collect(),
        )
    }

    /// Return every external method overload's `(min_args, max_args)` range
    /// for `type_name::method`, walking parent classes. Stops at the first
    /// class in the chain that defines the method (override semantics).
    ///
    /// Returns `None` when the type/method is unknown or the type is
    /// Nadeo-sourced (member metadata is incomplete there).
    pub fn lookup_external_method_arity_ranges(
        &self,
        type_name: &str,
        method: &str,
    ) -> Option<Vec<(usize, usize)>> {
        if self.is_nadeo_type(type_name) {
            return None;
        }
        let ext = self.external?;
        let mut ranges = Vec::new();
        let mut current: Option<String> = Self::resolve_external_type_name(ext, type_name);
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            let info = ext.lookup_type(&name)?;
            let mut found_on_this = false;
            for m in &info.methods {
                if m.name != method {
                    continue;
                }
                found_on_this = true;
                let min_args = m.params.iter().filter(|p| p.default.is_none()).count();
                ranges.push((min_args, m.params.len()));
            }
            if found_on_this {
                break;
            }
            current = info
                .parent
                .as_ref()
                .and_then(|p| Self::resolve_external_type_name(ext, p));
        }
        if ranges.is_empty() {
            None
        } else {
            Some(ranges)
        }
    }

    /// Resolve `name` to a key that `TypeIndex::lookup_type` accepts.
    /// Accepts already-FQN names, bare short names (`CGameCtnBlock`), and
    /// partially-qualified suffixes (`CGameEditorPluginMap::ECardinalDirections`
    /// is not a type key — only class-level names are expected here).
    fn resolve_external_type_key(&self, name: &str) -> Option<String> {
        let ext = self.external?;
        if ext.lookup_type(name).is_some() {
            return Some(name.to_string());
        }
        if name.contains("::") {
            self.resolve_qualified_suffix(name)
        } else {
            self.resolve_unqualified(name)
        }
    }

    /// Canonical FQN for a type/enum name used in equality checks.
    /// Leaves the input unchanged when nothing in the index matches.
    pub fn canonicalize_type_name(&self, name: &str) -> String {
        if self.has_type(name) || self.has_enum(name) {
            return name.to_string();
        }
        if let Some(resolved) = self.resolve_qualified_suffix(name) {
            return resolved;
        }
        if !name.contains("::") {
            if let Some(resolved) = self.resolve_unqualified(name) {
                return resolved;
            }
        }
        name.to_string()
    }

    /// External method overloads with param type strings (B007).
    ///
    /// Unlike `lookup_external_method_arity_ranges`, this does **not** skip
    /// Nadeo-sourced types: Nadeo method `a` fields carry usable parameter
    /// type text (e.g. `RemoveBlockSafe`'s distinct enum params). Parent
    /// names stored bare in the Nadeo dump are re-resolved via short-name
    /// lookup so inherited methods on `CGameEditorPluginMapMapType` are found.
    ///
    /// Returns `None` when the type/method is unknown.
    pub fn lookup_external_method_param_overloads(
        &self,
        type_name: &str,
        method: &str,
    ) -> Option<Vec<OverloadSig>> {
        let ext = self.external?;
        let mut out = Vec::new();
        let mut current = self.resolve_external_type_key(type_name);
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            let info = ext.lookup_type(&name)?;
            let mut found_on_this = false;
            for m in &info.methods {
                if m.name != method {
                    continue;
                }
                found_on_this = true;
                let min_args = m.params.iter().filter(|p| p.default.is_none()).count();
                out.push(OverloadSig {
                    param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                    param_types: m.params.iter().map(|p| p.type_name.clone()).collect(),
                    min_args,
                    return_type: m.return_type.clone(),
                });
            }
            if found_on_this {
                break;
            }
            current = info
                .parent
                .as_ref()
                .and_then(|p| self.resolve_external_type_key(p));
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// External free-function overloads with param type strings (B007).
    /// Returns `None` when `qualified` is not an external function.
    pub fn lookup_external_function_param_overloads(
        &self,
        qualified: &str,
    ) -> Option<Vec<OverloadSig>> {
        let ext = self.external?;
        let fns = ext.lookup_function(qualified)?;
        if fns.is_empty() {
            return None;
        }
        Some(
            fns.iter()
                .map(|f| {
                    let min_args = f.params.iter().filter(|p| p.default.is_none()).count();
                    OverloadSig {
                        param_names: f.params.iter().map(|p| p.name.clone()).collect(),
                        param_types: f.params.iter().map(|p| p.type_name.clone()).collect(),
                        min_args,
                        return_type: f.return_type.clone(),
                    }
                })
                .collect(),
        )
    }

    /// Look up a unique workspace free function's parameter list
    /// `(name, type_text)` by qualified name. Returns `None` if the name has
    /// zero matches *or* two-plus matches (the overloaded case — callers
    /// conservatively suppress type checking when overloads exist).
    pub fn lookup_function_params(&self, qualified: &str) -> Option<Vec<(String, String)>> {
        lookup_workspace_function_property(&self.workspace, qualified).map(|s| match &s.kind {
            SymbolKind::Function { params, .. } => params.clone(),
            _ => unreachable!(),
        })
    }

    /// Look up a unique workspace free function's parameter type text list
    /// by qualified name. Returns `None` if the name has zero matches *or*
    /// two-plus matches (the overloaded case — callers conservatively
    /// suppress type checking when overloads exist). Returns the raw
    /// `type_text` strings as stored in the symbol table; callers are
    /// responsible for parsing them (e.g. via `PrimitiveType::from_name`).
    pub fn lookup_function_param_types(&self, qualified: &str) -> Option<Vec<String>> {
        self.lookup_function_params(qualified)
            .map(|params| params.into_iter().map(|(_, ty_text)| ty_text).collect())
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
        let alt_names = workspace_function_property_candidates(qualified);
        for s in self.workspace.all_symbols() {
            if !alt_names.iter().any(|name| s.name == *name) {
                continue;
            }
            if let SymbolKind::Function {
                return_type,
                params,
                min_args,
            } = &s.kind
            {
                out.push(OverloadSig {
                    param_names: params.iter().map(|(name, _)| Some(name.clone())).collect(),
                    param_types: params.iter().map(|(_, ty_text)| ty_text.clone()).collect(),
                    min_args: *min_args,
                    return_type: return_type.clone(),
                });
            }
        }
        out
    }

    /// Unified free-function callables for a name (I3).
    ///
    /// Workspace overloads win when present; otherwise external typedb
    /// free-function overloads. Empty when neither source knows the name.
    /// Signature help and the checker should share this path so arity/params
    /// stay consistent.
    pub fn callables_free(&self, qualified: &str) -> Vec<OverloadSig> {
        let ws = self.lookup_function_overloads(qualified);
        if !ws.is_empty() {
            return ws;
        }
        self.lookup_external_function_param_overloads(qualified)
            .unwrap_or_default()
    }

    /// Unified method callables on `type_name` (I3).
    ///
    /// Currently external typedb methods (with parent walk). Workspace class
    /// methods remain via `workspace_class_member` / signature's member path
    /// until those carry full overload lists in the symbol table.
    pub fn callables_method(&self, type_name: &str, method: &str) -> Vec<OverloadSig> {
        self.lookup_external_method_param_overloads(type_name, method)
            .unwrap_or_default()
    }

    /// Look up the declared base classes of a workspace class by
    /// fully qualified name. Returns an empty vec if no workspace class with
    /// that name exists, or the class has no bases. Only consults
    /// the workspace symbol table — external (typedb) types use their
    /// own parent walker via `ext_lookup_member`.
    pub fn workspace_class_parents(&self, class_name: &str) -> Vec<String> {
        for s in self.workspace.all_symbols() {
            if s.name != class_name {
                continue;
            }
            if let SymbolKind::Class { parents, .. } = &s.kind {
                return parents.clone();
            }
        }
        Vec::new()
    }

    /// Walk the workspace class inheritance chain starting from
    /// `class_name`, looking for a field or method named `member`.
    /// Returns the first match as a `TypeRepr`, parsed from the raw
    /// type-text the symbol table lifted at extraction time (iter 28).
    /// An empty type-text (destructor, unpopulated) parses to
    /// `TypeRepr::Error(String::new())` — still a valid silence sentinel
    /// for suppressing `UndefinedMember`.
    ///
    /// Uses a visited-set to prevent infinite loops on cyclic
    /// inheritance (pathological user code: `A : B, B : A`).
    pub fn workspace_class_member(&self, class_name: &str, member: &str) -> Option<TypeRepr> {
        let getter = format!("get_{}", member);
        let setter = format!("set_{}", member);
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(class_name)]);
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            // Look for `Class::member` / `Class::get_member` / `Class::set_member`
            // in the workspace symbol table.
            let qualified = format!("{}::{}", name, member);
            let qualified_getter = format!("{}::{}", name, getter);
            let qualified_setter = format!("{}::{}", name, setter);
            for s in self.workspace.all_symbols() {
                if s.name != qualified && s.name != qualified_getter && s.name != qualified_setter {
                    continue;
                }
                match &s.kind {
                    SymbolKind::Variable { type_name } => {
                        return Some(TypeRepr::parse_type_string(type_name));
                    }
                    SymbolKind::Function { return_type, .. } => {
                        return Some(TypeRepr::parse_type_string(return_type));
                    }
                    _ => {}
                }
            }
            // Not found on this class — ascend to its base classes.
            for parent in self.workspace_class_parents(&name) {
                queue.push_back(self.normalize_workspace_class_name_in_context(&parent, &name));
            }
        }
        None
    }

    fn normalize_workspace_class_name(&self, name: &str) -> String {
        self.normalize_workspace_class_name_in_context(name, name)
    }

    fn normalize_workspace_class_name_in_context(
        &self,
        name: &str,
        context_class_name: &str,
    ) -> String {
        if !name.contains("::") {
            if let Some((ns, _)) = context_class_name.rsplit_once("::") {
                let candidate = format!("{}::{}", ns, name);
                if self.workspace.all_symbols().any(|s| s.name == candidate) {
                    return candidate;
                }
            }
        }
        if self.workspace.all_symbols().any(|s| s.name == name) {
            return name.to_string();
        }
        self.resolve_qualified_suffix(name)
            .or_else(|| self.resolve_unqualified(name))
            .unwrap_or_else(|| name.to_string())
    }

    /// Resolve a workspace global used as a *value* (not a call callee).
    ///
    /// Returns:
    /// - `Variable` → parsed `type_name` (empty text → `Error("")` silence)
    /// - `Function` → `Funcdef(qualified)` so function-pointer decay sites
    ///   (`startnew(Worker)`) don't compare as `Named("Worker")` vs
    ///   `Named("CoroutineFunc")`
    /// - `EnumValue` → `Named(enum_name)`
    /// - virtual property via `get_`/`set_` → getter return type when known
    ///
    /// `None` when the name is not a workspace value symbol (caller falls
    /// through to other Ident rules).
    pub fn lookup_global_value_type(&self, qualified: &str) -> Option<TypeRepr> {
        let tail = format!("::{}", qualified);
        for s in self.workspace.all_symbols() {
            let matches_name = s.name == qualified || s.name.ends_with(&tail);
            if !matches_name {
                continue;
            }
            if self.tail_prefix_is_class_member(&s.name, &tail) {
                // `ClassName::field` is not a global value — bare-name tail
                // matches against class members must stay undefined (GH #30).
                continue;
            }
            match &s.kind {
                SymbolKind::Variable { type_name } => {
                    return Some(TypeRepr::parse_type_string(type_name));
                }
                SymbolKind::Function { .. } => {
                    return Some(TypeRepr::Funcdef(s.name.clone()));
                }
                SymbolKind::EnumValue { enum_name, .. } => {
                    return Some(TypeRepr::Named(enum_name.clone()));
                }
                _ => {}
            }
        }
        // Virtual property: bare name with get_/set_ accessors.
        let getter = qualified_virtual_name(qualified, "get_");
        if let Some(t) = self.lookup_function_return(&getter) {
            return Some(t);
        }
        let setter = qualified_virtual_name(qualified, "set_");
        if self.has_function(&setter) {
            // Setter-only: type unknown — silence rather than Named(name).
            return Some(TypeRepr::Error(String::new()));
        }
        None
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
        if lookup_workspace_function_property(&self.workspace, qualified).is_some() {
            return Some(TypeRepr::Error(String::new()));
        }
        None
    }
}

fn workspace_function_property_candidates(name: &str) -> [String; 3] {
    [
        name.to_string(),
        qualified_virtual_name(name, "get_"),
        qualified_virtual_name(name, "set_"),
    ]
}

fn qualified_virtual_name(name: &str, prefix: &str) -> String {
    if let Some((head, tail)) = name.rsplit_once("::") {
        format!("{}::{}{}", head, prefix, tail)
    } else {
        format!("{}{}", prefix, name)
    }
}

fn lookup_workspace_function_property<'a>(
    workspace: &'a SymbolTable,
    qualified: &str,
) -> Option<&'a crate::symbols::scope::Symbol> {
    let candidates = workspace_function_property_candidates(qualified);
    let mut found = None;
    for s in workspace.all_symbols() {
        if !candidates.iter().any(|name| s.name == *name) {
            continue;
        }
        if !matches!(s.kind, SymbolKind::Function { .. }) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(s);
    }
    found
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
                    parents: vec![],
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
                    parents: vec![],
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
        use crate::typedb::index::TypeIndex;
        // Reach into the module-internal constructor by way of the load
        // path is awkward; assemble manually via the Default + a private
        // insertion through a small helper. Since `types` is private, we
        // round-trip through the public `load` path in a throwaway test if
        // fixtures exist, but otherwise directly verify the fallback path.
        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
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
        // Short names also resolve for Nadeo discrimination / completeness.
        assert!(scope.is_nadeo_type("CMwNod"));
        assert!(scope.nadeo_member_list_trusted("CGameCtnCollection"));
        assert!(
            !scope.nadeo_member_list_trusted("CMwEngine"),
            "empty member list must not be trusted"
        );
        // A Core-sourced type like UI::InputBlocking should NOT report as Nadeo.
        // Fall back to any non-Nadeo core type by iterating if needed.
        assert!(!scope.is_nadeo_type("NotARealType"));
    }

    #[test]
    fn resolve_qualified_suffix_finds_nested_external_enum() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));

        assert_eq!(
            scope.resolve_qualified_suffix("CGameEditorPluginMap::ECardinalDirections"),
            Some("Game::CGameEditorPluginMap::ECardinalDirections".to_string())
        );
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

    #[test]
    fn workspace_class_member_normalizes_namespaced_parent_name() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![
                make_symbol(
                    "Editor::NetworkSerializable",
                    SymbolKind::Class {
                        parents: vec![],
                        members: vec![],
                    },
                ),
                make_symbol(
                    "Editor::NetworkSerializable::ReadFromNetworkBuffer",
                    SymbolKind::Function {
                        return_type: "NetworkSerializable@".into(),
                        params: vec![("buf".into(), "MemoryBuffer@".into())],
                        min_args: 1,
                    },
                ),
                make_symbol(
                    "Editor::Child",
                    SymbolKind::Class {
                        parents: vec!["NetworkSerializable".into()],
                        members: vec![],
                    },
                ),
            ],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(
            scope
                .workspace_class_member("Editor::Child", "ReadFromNetworkBuffer")
                .is_some(),
            "expected inherited member lookup to normalize parent names"
        );
    }

    #[test]
    fn workspace_class_member_prefers_parent_in_current_namespace() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![
                make_symbol(
                    "ItemSpec",
                    SymbolKind::Class {
                        parents: vec![],
                        members: vec![],
                    },
                ),
                make_symbol(
                    "Editor::ItemSpec",
                    SymbolKind::Class {
                        parents: vec![],
                        members: vec![],
                    },
                ),
                make_symbol(
                    "Editor::ItemSpec::ReadFromNetworkBuffer",
                    SymbolKind::Function {
                        return_type: "NetworkSerializable@".into(),
                        params: vec![("buf".into(), "MemoryBuffer@".into())],
                        min_args: 1,
                    },
                ),
                make_symbol(
                    "Editor::ItemSpecPriv",
                    SymbolKind::Class {
                        parents: vec!["ItemSpec".into()],
                        members: vec![],
                    },
                ),
            ],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(
            scope
                .workspace_class_member("Editor::ItemSpecPriv", "ReadFromNetworkBuffer")
                .is_some(),
            "expected namespaced parent lookup to beat global short-name collision"
        );
    }

    #[test]
    fn external_method_arity_ranges_indexof_and_selectable() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));

        assert_eq!(
            scope.lookup_external_method_arity_ranges("string", "IndexOf"),
            Some(vec![(1, 1)])
        );
        // SubStr is overloaded 1-arg / 2-arg.
        let substr = scope
            .lookup_external_method_arity_ranges("string", "SubStr")
            .expect("SubStr");
        assert!(substr.contains(&(1, 1)) && substr.contains(&(2, 2)));

        assert_eq!(
            scope.lookup_external_function_arity_ranges("UI::Selectable"),
            Some(vec![(2, 3)])
        );
        assert_eq!(
            scope.lookup_function_signature("UI::Selectable"),
            Some((2, 3))
        );
    }

    #[test]
    fn external_method_param_overloads_remove_block_safe() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));

        // Direct declaring type.
        let overloads = scope
            .lookup_external_method_param_overloads("CGameEditorPluginMap", "RemoveBlockSafe")
            .expect("RemoveBlockSafe on CGameEditorPluginMap");
        assert_eq!(overloads.len(), 1);
        assert_eq!(overloads[0].min_args, 3);
        assert_eq!(overloads[0].param_types.len(), 3);
        assert_eq!(
            overloads[0].param_types[2],
            "CGameEditorPluginMap::ECardinalDirections"
        );

        // Inherited via MapType subclass (bare parent walk + short-name resolve).
        let inherited = scope
            .lookup_external_method_param_overloads(
                "CGameEditorPluginMapMapType",
                "RemoveBlockSafe",
            )
            .expect("RemoveBlockSafe on MapType");
        assert_eq!(inherited[0].param_types[2], overloads[0].param_types[2]);

        assert_eq!(
            scope.canonicalize_type_name("CGameCtnBlock::ECardinalDirections"),
            "Game::CGameCtnBlock::ECardinalDirections"
        );
        assert_eq!(
            scope.canonicalize_type_name("CGameEditorPluginMap::ECardinalDirections"),
            "Game::CGameEditorPluginMap::ECardinalDirections"
        );
    }

    #[test]
    fn external_param_overloads_preserve_param_names() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));

        let table_setup_column = scope
            .lookup_external_function_param_overloads("UI::TableSetupColumn")
            .expect("UI::TableSetupColumn");
        assert_eq!(table_setup_column.len(), 1);
        assert_eq!(
            table_setup_column[0].param_names,
            vec![
                Some("label".to_string()),
                Some("flags".to_string()),
                Some("init_width_or_weight".to_string()),
                Some("user_id".to_string()),
            ]
        );

        let assert_true = scope
            .lookup_external_method_param_overloads("Context", "AssertTrue")
            .expect("Context::AssertTrue");
        assert_eq!(assert_true.len(), 1);
        assert_eq!(
            assert_true[0].param_names,
            vec![Some("condition".to_string()), Some("message".to_string())]
        );
    }
}
