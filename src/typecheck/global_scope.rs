//! A read-only merged view of globally visible names, combining the
//! workspace-level symbol table with the external Openplanet / Nadeo type
//! database.
//!
//! This is the lookup surface the type resolver consults when deciding
//! whether a referenced name is known. It does NOT own either data source —
//! callers build one on demand by borrowing both.

use super::repr::{param_mode_from_text, ParamMode, TypeRepr};
use crate::symbols::scope::{Symbol, SymbolKind};
use crate::symbols::SymbolTable;
use crate::typedb::index::ParamInfo;
use crate::typedb::TypeIndex;

/// Read-only merged view of all globally visible symbols:
/// workspace-defined (user code) + external (Openplanet core + Nadeo).
pub struct GlobalScope<'a> {
    workspace: &'a SymbolTable,
    /// Optional declarations used only while checking omitted own-module
    /// sources for usage. They participate in positive name resolution but
    /// never in closure/namespace-authority queries.
    supplemental: Option<&'a SymbolTable>,
    external: Option<&'a TypeIndex>,
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
    /// Per-parameter `&out` flag (typedb `typeflags` bit 2) aligned with
    /// `param_types`; true params bind only l-value args (GH #47).
    pub param_out: Vec<bool>,
    /// Per-parameter passing mode (B027), aligned with `param_types`:
    /// workspace sigs derive it from the stored param text, typedb sigs
    /// from the `typeflags` bits.
    pub param_modes: Vec<ParamMode>,
    pub min_args: usize,
    pub return_type: String,
}

/// A funcdef type (workspace or Core typedb) looked up by qualified name.
/// Empty `param_types` + empty `return_type` means the signature is unknown
/// (e.g. a typedef); callers then accept any single function argument.
#[derive(Debug, Clone)]
pub struct FuncdefSig {
    pub qualified: String,
    pub return_type: String,
    pub param_types: Vec<String>,
    /// Per-parameter passing mode (B027), aligned with `param_types`.
    pub param_modes: Vec<ParamMode>,
}

/// Passing mode of a typedb parameter. Core `typedecl` text never carries
/// the `&` decoration (the `typeflags` bits are authoritative there);
/// Nadeo signature text may, so prefer the text and fall back to flags.
fn typedb_param_mode(p: &ParamInfo) -> ParamMode {
    let from_text = param_mode_from_text(&p.type_name);
    if from_text != ParamMode::Value {
        return from_text;
    }
    if p.is_inout {
        ParamMode::Inout
    } else if p.is_out {
        ParamMode::Out
    } else if p.is_in {
        ParamMode::In
    } else {
        ParamMode::Value
    }
}

impl<'a> GlobalScope<'a> {
    pub fn new(workspace: &'a SymbolTable, external: Option<&'a TypeIndex>) -> Self {
        Self {
            workspace,
            supplemental: None,
            external,
        }
    }

    /// Build the usage-only resolution view: authoritative pooled symbols
    /// plus a small table containing only non-colliding declarations from
    /// omitted sources. Borrowing both avoids cloning the full pooled table;
    /// the supplemental table remains outside every snapshot surface.
    pub fn with_supplemental(
        workspace: &'a SymbolTable,
        supplemental: &'a SymbolTable,
        external: Option<&'a TypeIndex>,
    ) -> Self {
        Self {
            workspace,
            supplemental: Some(supplemental),
            external,
        }
    }

    /// Exact workspace lookup across the pooled table and the optional
    /// usage-only supplement. Namespace-authority methods deliberately do
    /// not use this surface.
    pub fn lookup_workspace(&self, name: &str) -> Vec<&Symbol> {
        let mut symbols = self.workspace.lookup(name);
        if let Some(supplemental) = self.supplemental {
            symbols.extend(supplemental.lookup(name));
        }
        symbols
    }

    /// Qualified-tail lookup across both positive-resolution tables.
    pub fn lookup_workspace_tail(&self, tail: &str) -> Vec<&Symbol> {
        let mut symbols = self.workspace.lookup_tail(tail);
        if let Some(supplemental) = self.supplemental {
            symbols.extend(supplemental.lookup_tail(tail));
        }
        symbols
    }

    /// Member-prefix lookup across both positive-resolution tables.
    pub fn lookup_workspace_members(&self, owner: &str) -> Vec<&Symbol> {
        let mut symbols = self.workspace.lookup_members(owner);
        if let Some(supplemental) = self.supplemental {
            symbols.extend(supplemental.lookup_members(owner));
        }
        symbols
    }

    /// The external type-database half of the merged view, when one was
    /// provided. Features that need typedb details beyond name resolution
    /// (hover docs, signature fallbacks) read it through here instead of
    /// carrying a second handle to the same index alongside the scope
    /// (GH #41).
    pub fn external(&self) -> Option<&'a TypeIndex> {
        self.external
    }

    /// The workspace symbol-table half of the merged view (unused-decl
    /// usage recording needs exact-qualified-name existence checks while
    /// walking inheritance chains).
    pub fn workspace(&self) -> &'a SymbolTable {
        self.workspace
    }

    /// Resolve a source-level reference through the workspace half:
    /// exact qualified name, then bare tail, then qualified-tail match
    /// (`SymbolTable::lookup_reference`). One ladder, shared by every
    /// feature instead of per-file copies (GH #41).
    pub fn lookup_reference(&self, name: &str) -> Vec<&Symbol> {
        let exact = self.lookup_workspace(name);
        if !exact.is_empty() {
            return exact;
        }
        if let Some((_, bare)) = name.rsplit_once("::") {
            let bare_hits = self.lookup_workspace(bare);
            if !bare_hits.is_empty() {
                return bare_hits;
            }
            return self.lookup_workspace_tail(bare);
        }
        Vec::new()
    }

    /// True when the workspace half is the plugin's COMPLETE module
    /// closure (see [`SymbolTable::dependency_closure_complete`]). Only
    /// then does "absent from the pool" mean "the game compiler would
    /// reject it" — GH #58.
    pub fn workspace_closure_complete(&self) -> bool {
        self.workspace.dependency_closure_complete()
    }

    /// True when the workspace declares `qualified` as a namespace
    /// (`namespace Ns { … }` in any pooled file). Namespaces are the only
    /// qualifier AngelScript resolves free calls through, so this is the
    /// gate for the workspace `No matching symbol` rule (GH #58).
    pub fn workspace_namespace_declared(&self, qualified: &str) -> bool {
        !qualified.is_empty()
            && self
                .workspace
                .lookup(qualified)
                .iter()
                .any(|s| matches!(s.kind, SymbolKind::Namespace))
    }

    /// True when the workspace pool holds ANY symbol under the exact
    /// qualified name — function, class, interface, enum, enum value,
    /// funcdef, namespace or variable. GH #58 decision 4: every kind
    /// counts as resolution for `Ns::Fn(...)`, so only a name matching
    /// nothing is a game-compiler rejection.
    pub fn workspace_declares_any(&self, qualified: &str) -> bool {
        !self.workspace.lookup(qualified).is_empty()
    }

    /// Direct child names declared under `ns` in the workspace — one
    /// `::`-segment deep only, so `Ns::Class::method` contributes `Class`
    /// and not `method`. Backs the near-name hint on the workspace
    /// `No matching symbol` rule (GH #58), mirroring the typedb
    /// `UI::GetIO` path.
    pub fn workspace_namespace_member_names(&self, ns: &str) -> Vec<String> {
        let prefix = format!("{ns}::");
        let mut names: Vec<String> = self
            .workspace
            .lookup_members(ns)
            .iter()
            .filter_map(|s| s.name.strip_prefix(&prefix))
            .filter(|rest| !rest.contains("::") && !rest.is_empty())
            .map(|rest| rest.to_string())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// True if the qualified name refers to a type (class / interface /
    /// funcdef-as-type) in either the workspace or the external index.
    pub fn has_type(&self, qualified: &str) -> bool {
        let is_type_kind = |s: &Symbol| {
            matches!(
                s.kind,
                SymbolKind::Class { .. }
                    | SymbolKind::Interface { .. }
                    | SymbolKind::Funcdef { .. }
            )
        };
        if self
            .lookup_workspace(qualified)
            .iter()
            .any(|s| is_type_kind(s))
        {
            return true;
        }
        if let Some(ext) = self.external {
            if ext.lookup_type(qualified).is_some() {
                return true;
            }
            if ext.lookup_funcdef(qualified).is_some() {
                return true;
            }
        }
        false
    }

    /// Look up a funcdef (workspace declaration or Core typedb) by
    /// qualified name. `None` when the name is not a funcdef.
    pub fn lookup_funcdef(&self, qualified: &str) -> Option<FuncdefSig> {
        for s in self.lookup_workspace(qualified) {
            if let SymbolKind::Funcdef {
                return_type,
                params,
                is_typedef,
            } = &s.kind
            {
                // Typedefs reuse `SymbolKind::Funcdef` (tagged at
                // extraction): they are type aliases, not callable
                // signatures. Real funcdefs — including parameterless
                // ones — must resolve here.
                if *is_typedef {
                    continue;
                }
                return Some(FuncdefSig {
                    qualified: s.name.clone(),
                    return_type: return_type.clone(),
                    param_types: params.iter().map(|(_, ty)| ty.clone()).collect(),
                    param_modes: params
                        .iter()
                        .map(|(_, ty)| param_mode_from_text(ty))
                        .collect(),
                });
            }
        }
        let ext = self.external?;
        let info = ext.lookup_funcdef(qualified)?;
        Some(FuncdefSig {
            qualified: qualified.to_string(),
            return_type: info.return_type.clone(),
            param_types: info.params.iter().map(|p| p.type_name.clone()).collect(),
            param_modes: info.params.iter().map(|p| typedb_param_mode(p)).collect(),
        })
    }

    /// True if the qualified name refers to a free function.
    pub fn has_function(&self, qualified: &str) -> bool {
        let workspace_hit = self
            .lookup_workspace(qualified)
            .iter()
            .any(|s| matches!(s.kind, SymbolKind::Function { .. }));
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
            .lookup_workspace(qualified)
            .iter()
            .any(|s| matches!(s.kind, SymbolKind::Enum { .. }));
        if workspace_hit {
            return true;
        }
        if let Some(ext) = self.external {
            if ext.lookup_source_enum(qualified).is_some() {
                return true;
            }
            // The typedb keys a nested engine enum under its MODULE
            // (`Game::CGameCtnBlock::ECardinalDirections`), but source spells
            // it without one (`CGameCtnBlock::ECardinalDirections`), so a
            // qualified lookup must try the trailing path before giving up
            // (B007). A trailing PATH — the leading part still has to be the
            // enum's real declaring scope, which is what the game checks.
            if ext.lookup_source_enum_by_suffix(qualified).is_some() {
                return true;
            }
            // AngelScript's ancestor walk (B052): `P::T` looks `T` up in
            // scope `P` and then in each of P's ancestors out to the global
            // namespace, so a GLOBAL enum answers to any real scope's
            // qualifier. `lookup_source_enum(tail)` only hits when `tail` is
            // a whole key, which for an enum means it has no namespace, so
            // this reaches exactly the global enums and nothing else.
            //
            // The `scope_exists` gate is the fix. Without it every prefix
            // qualified them — including the Nadeo module names this crate
            // invents, so `Scene::ESceneCharVisNotice` resolved while the
            // game answered `Namespace 'Scene' doesn't exist.` Live probe,
            // Trackmania + Openplanet 1.29.5, 2026-08-30.
            if let Some((head, tail)) = qualified.rsplit_once("::") {
                if ext.lookup_source_enum(tail).is_some() && self.scope_exists(head) {
                    return true;
                }
            }
        }
        false
    }

    /// The stored source key for an accepted, non-canonical enum spelling.
    /// This covers both a module-stripped nested enum
    /// (`CGameCtnBlock::ECardinalDirections` →
    /// `Game::CGameCtnBlock::ECardinalDirections`) and a global enum reached
    /// through a real scope's ancestor walk (`UI::VirtualKey` →
    /// `VirtualKey`). `None` when the spelling already IS the key, or names
    /// no source-visible enum at all.
    ///
    /// `has_enum` accepts the module-stripped spelling because the game does
    /// (B052), but type equality is by key, so a resolver that stops at
    /// "it resolves" would leave two spellings of one enum unequal.
    pub fn canonical_enum_key(&self, qualified: &str) -> Option<String> {
        if self.is_canonical_enum_key(qualified) {
            return None;
        }
        let ext = self.external?;

        // Preserve the declaring-scope suffix mapping before considering the
        // broader ancestor walk: it is the more specific resolution path.
        if let Some(key) = ext.lookup_source_enum_by_suffix(qualified) {
            return Some(key.to_string());
        }

        let (head, tail) = qualified.rsplit_once("::")?;
        if !self.scope_exists(head) {
            return None;
        }

        // `tail` is a complete key here, so this can only select a global
        // enum. The source-facing lookup also excludes anonymous synthetic
        // keys; acceptance is never inferred merely from a matching tail.
        ext.lookup_source_enum(tail).map(|_| tail.to_string())
    }

    /// True when `qualified` is an enum's own key, exactly as stored —
    /// no module-stripped spelling, no ancestor walk. Callers that need a
    /// canonical name, rather than "does this resolve", ask this.
    fn is_canonical_enum_key(&self, qualified: &str) -> bool {
        if self
            .lookup_workspace(qualified)
            .iter()
            .any(|s| matches!(s.kind, SymbolKind::Enum { .. }))
        {
            return true;
        }
        self.external
            .is_some_and(|ext| ext.lookup_source_enum(qualified).is_some())
    }

    /// True when `path` names a scope a qualified name resolves through —
    /// a Core namespace, an engine class that declares an enum, or a
    /// namespace the workspace itself declares (B052).
    ///
    /// The workspace half is not a nicety: a plugin that writes `namespace
    /// Foo { … }` can spell a global engine enum `Foo::VirtualKey`, and the
    /// game accepts it. Probed on a plugin declaring `namespace Foo {
    /// namespace Bar { … } }`: `Foo::VirtualKey` and `Foo::Bar::VirtualKey`
    /// both compile.
    pub fn scope_exists(&self, path: &str) -> bool {
        if path.is_empty() {
            return false;
        }
        if self.workspace_namespace_declared(path) {
            return true;
        }
        self.external.is_some_and(|ext| ext.scope_exists(path))
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
        for s in self.lookup_workspace_tail(short) {
            if !matches!(
                s.kind,
                SymbolKind::Class { .. }
                    | SymbolKind::Interface { .. }
                    | SymbolKind::Funcdef { .. }
                    | SymbolKind::Enum { .. }
            ) {
                continue;
            }
            return Some(s.name.clone());
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

        for s in self.lookup_workspace_tail(short) {
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
    ///
    /// The external enum-value fallback covers *named* enums only (B044):
    /// an inline member enum the engine dump leaves unnamed cannot be
    /// spelled in source, so a bare name that is only one of its values is
    /// not a symbol — the game answers `No matching symbol`.
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
        let mut candidates: Vec<&crate::symbols::scope::Symbol> = self.lookup_workspace(name);
        candidates.extend(self.lookup_workspace_tail(name));
        for s in candidates {
            if self.tail_prefix_is_class_member(&s.name, &format!("::{}", name)) {
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
        //
        // B044: `source_enums_iter`, NOT `enums_iter` — only an enum source
        // can NAME contributes its values here. An inline member enum the
        // engine dump leaves unnamed has no name in the game either, so no
        // source can reach its values except through the declaring member's
        // type; a bare value name of one is not a symbol. Game ground truth,
        // RemoteBuild probe, OP 1.29.5, on the exhaustive fixture
        // `tests/fixtures/issue-repros/b044-anon-enum-value-names`:
        // `No matching symbol 'HyperTnL'` and 257 siblings — all 258 of its
        // names, with no residual. The 3 the game accepted (`DoubleJump`,
        // `Jump`, `TechJump`) were never anonymous-only: they belong to the
        // module-level enum `ESceneCharVisNotice`, which only `Openplanet.h`
        // names. B046 taught the loader to read it, and they left this set.
        //
        // This is the whole seam. Two things keep it from over-rejecting:
        //   * a value name that ALSO appears in a named enum is still found
        //     on that enum (`VCenter2` lives in an anonymous enum *and* in
        //     `Game::CGameManialinkControl::EAlignVertical`), and
        //   * every other way a name can be global — type, function, virtual
        //     property, workspace variable or enum value — was already tried
        //     above, so a name that merely collides with an anonymous enum
        //     value keeps resolving through its real declaration.
        //
        // Member access is unaffected: `lookup_member_enum` reaches an
        // anonymous enum by its declaring context, never through this scan.
        //
        // Deliberately deferred by B016, which was forbidden to tighten any
        // rule: pre-B016 only the 14 anonymous enums that won their module's
        // key collision had their values accepted here, so filtering would
        // have turned 5 identifiers into fresh errors on a data-loss fix.
        // B044 is that tightening, with the game probe behind it.
        if let Some(ext) = self.external {
            for (_, en) in ext.source_enums_iter() {
                if en.values.iter().any(|(v, _)| v == name) {
                    return true;
                }
            }
        }
        false
    }

    /// B053: the bare value name `name` is carried by two or more globally
    /// visible engine enums, so the game refuses to choose — `Found multiple
    /// matching enum values`. Returns the candidate enum names, sorted, so
    /// the diagnostic can spell out how to qualify it.
    ///
    /// `None` when the name is unambiguous or something outranks the enums.
    ///
    /// The workspace cuts BOTH ways, and the game was asked which (probed
    /// with a plugin declaring each shape, OP 1.29.5, 2026-08-30):
    ///
    /// * a global `enum` of the plugin's own JOINS the pool. `GlassSmash` is
    ///   carried by one engine enum and compiles bare; add `enum Local {
    ///   GlassSmash }` to the plugin and the game starts answering `Found
    ///   multiple matching enum values`. So a workspace enum is a candidate,
    ///   not a shield.
    /// * a global `int Turbo = 1;` WINS outright — the game accepts bare
    ///   `Turbo` even though two engine enums carry it. A variable, function
    ///   or virtual property of the name is the answer, and no ambiguity is
    ///   reported.
    /// * a NAMESPACED workspace enum (`namespace N { enum E { Horn } }`) and
    ///   a class member do not take part at all, exactly as a namespaced or
    ///   nested ENGINE enum does not.
    pub fn ambiguous_global_enum_value(&self, name: &str) -> Option<Vec<String>> {
        if name.contains("::") {
            return None;
        }
        // A type or enum of this name is a type reference, never a value.
        if self.has_type(name) || self.has_enum(name) {
            return None;
        }
        let mut owners: Vec<String> = self
            .external
            .map(|ext| ext.global_enum_value_owners(name).to_vec())
            .unwrap_or_default();

        // Both tables, exactly as `lookup_global_value_type` reads them: a
        // top-level `int Down = 1;` is stored under its bare name and is
        // absent from the tail index, so scanning only tails would miss the
        // declaration that outranks the enums.
        let tail = format!("::{}", name);
        let mut candidates: Vec<&crate::symbols::scope::Symbol> = self.lookup_workspace(name);
        candidates.extend(self.lookup_workspace_tail(name));
        for s in candidates {
            if self.tail_prefix_is_class_member(&s.name, &tail) {
                // `ClassName::field` is not a global: it neither outranks the
                // engine's enums nor joins them (GH #30).
                continue;
            }
            match &s.kind {
                // Outranks the enums entirely.
                SymbolKind::Variable { .. } | SymbolKind::Function { .. } => return None,
                // Only a GLOBAL workspace enum is in scope for a bare name;
                // one inside a namespace is not.
                SymbolKind::EnumValue { enum_name, .. }
                    if !enum_name.contains("::") && !owners.contains(enum_name) =>
                {
                    owners.push(enum_name.clone());
                }
                _ => {}
            }
        }
        // A virtual property (`get_Turbo`) is a value that outranks them too.
        if self.has_function(&qualified_virtual_name(name, "get_"))
            || self.has_function(&qualified_virtual_name(name, "set_"))
        {
            return None;
        }
        if owners.len() < 2 {
            return None;
        }
        owners.sort();
        Some(owners)
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
            if self.lookup_workspace(&acc).iter().any(|s| {
                matches!(
                    s.kind,
                    SymbolKind::Class { .. } | SymbolKind::Interface { .. }
                )
            }) {
                return true;
            }
        }
        false
    }

    /// True if a *workspace* class/interface/funcdef symbol exists whose
    /// name (or `::`-tail) matches `name`. Game semantics: a plugin-declared
    /// type shadows an engine typedb type of the same short name (GH #38 —
    /// workspace `Status` beats `Discord::Status`), so external member-list
    /// trust must not apply to the colliding name.
    fn workspace_type_shadows(&self, name: &str) -> bool {
        let is_type = |s: &&crate::symbols::scope::Symbol| {
            matches!(
                s.kind,
                SymbolKind::Class { .. }
                    | SymbolKind::Interface { .. }
                    | SymbolKind::Funcdef { .. }
            )
        };
        self.lookup_workspace(name).iter().any(is_type)
            || self.lookup_workspace_tail(name).iter().any(is_type)
    }

    /// Look up a member's type on a fully qualified type name, walking
    /// parent classes. Returns the member's type (or the method's return
    /// type, for method-as-value lookups) if found.
    ///
    /// Precedence: a workspace type whose name collides with the lookup
    /// shadows the external TypeIndex entirely (GH #38); otherwise external
    /// TypeIndex first (walks `parent`), then workspace symbols (fallback).
    /// Workspace hits parse the type text stored by the symbol extractor
    /// (iter 28) into a real `TypeRepr`. An empty stored string parses to
    /// `Error("")` — still a valid silence sentinel for suppressing
    /// `UndefinedMember`.
    pub fn lookup_member_type(&self, type_name: &str, member: &str) -> Option<TypeRepr> {
        if self.workspace_type_shadows(type_name) {
            return self.workspace_class_member(type_name, member);
        }
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
        if self.workspace_type_shadows(type_name) {
            return self.workspace_class_member(type_name, method);
        }
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

    /// True when `type_name` is (or derives from) `base` in the external
    /// typedb. Walks the parent chain with a cycle guard, same as
    /// `ext_lookup_member`. Used to gate game-builtin allowances (GH #21:
    /// `MwAddRef`/`MwRelease` on CMwNod).
    pub fn is_external_derived_from(&self, type_name: &str, base: &str) -> bool {
        let Some(ext) = self.external else {
            return false;
        };
        let Some(base_resolved) = Self::resolve_external_type_name(ext, base) else {
            return false;
        };
        let mut current: Option<String> = Self::resolve_external_type_name(ext, type_name);
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            if name == base_resolved {
                return true;
            }
            let Some(info) = ext.lookup_type(&name) else {
                return false;
            };
            current = info
                .parent
                .as_ref()
                .and_then(|p| Self::resolve_external_type_name(ext, p));
        }
        false
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

    /// GH #54: true when AngelScript can implicitly convert `from` into `to`
    /// through a single-argument constructor registered on `to`.
    ///
    /// `mat4` registers `mat4(const mat3&in)`, so `mat4::Inverse(someMat3)`
    /// compiles — AngelScript instantiates the destination value type from
    /// the argument. The pairs come from the typedb's own `behaviors`
    /// (`asBEHAVE_CONSTRUCT`, copy constructor excluded when the index is
    /// built), never a hardcoded list, so every registered conversion is
    /// covered and an exact match is still an exact match rather than a
    /// "conversion".
    ///
    /// Both endpoints must be engine typedb types: a workspace class that
    /// shadows an engine name (GH #38) carries none of these behaviors.
    pub fn external_ctor_converts(&self, from: &str, to: &str) -> bool {
        let Some(ext) = self.external else {
            return false;
        };
        if self.workspace_type_shadows(from) || self.workspace_type_shadows(to) {
            return false;
        }
        let Some(info) = self.external_type_info(to) else {
            return false;
        };
        let Some(from_key) = Self::resolve_external_type_name(ext, from) else {
            return false;
        };
        info.ctor_conversions.iter().any(|source| {
            Self::resolve_external_type_name(ext, source).as_deref() == Some(from_key.as_str())
        })
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
    ///
    /// A workspace type symbol whose name collides shadows the external
    /// type (GH #38): the checker must not apply external member-list
    /// trust to the shadowed name.
    pub fn is_external_type(&self, qualified: &str) -> bool {
        if self.workspace_type_shadows(qualified) {
            return false;
        }
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

    /// True when `obj_type` is an external typedb type whose property
    /// `member` is a proven get-only virtual property (a synthesized
    /// `writable: false` entry from a `get_X`-without-`set_X` pair —
    /// e.g. `iso4.tx`). Walks the parent chain like `ext_lookup_member`.
    /// Returns `false` for anything we can't positively classify
    /// (workspace types, Nadeo props, unknown names).
    pub fn external_member_is_readonly(&self, obj_type: &str, member: &str) -> bool {
        if self.workspace_type_shadows(obj_type) {
            return false;
        }
        let Some(ext) = self.external else {
            return false;
        };
        let mut current = Self::resolve_external_type_name(ext, obj_type);
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            let Some(info) = ext.lookup_type(&name) else {
                return false;
            };
            for p in &info.properties {
                if p.name == member {
                    // Explicit props are real fields; only synthesized
                    // get-only entries are read-only.
                    return !p.writable;
                }
            }
            current = info
                .parent
                .as_ref()
                .and_then(|par| Self::resolve_external_type_name(ext, par));
        }
        false
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
        if let Some(s) = self.lookup_workspace_function_property(qualified) {
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
        // Only an EXACT key is already canonical. `has_enum` is deliberately
        // wider than that — it also answers for the module-stripped spelling
        // the game wants and for a global enum behind a real scope's
        // qualifier (B052) — and neither of those is the canonical key.
        if self.has_type(name) || self.is_canonical_enum_key(name) {
            return name.to_string();
        }
        if let Some(key) = self.canonical_enum_key(name) {
            return key;
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
                    param_out: m.params.iter().map(|p| p.is_out).collect(),
                    param_modes: m.params.iter().map(|p| typedb_param_mode(p)).collect(),
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
                        param_out: f.params.iter().map(|p| p.is_out).collect(),
                        param_modes: f.params.iter().map(|p| typedb_param_mode(p)).collect(),
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
        self.lookup_workspace_function_property(qualified)
            .map(|s| match &s.kind {
                SymbolKind::Function { params, .. } => params.clone(),
                _ => unreachable!(),
            })
    }

    /// Look up a unique workspace free function's parameter type text list
    /// by qualified name. Returns `None` if the name has zero matches *or*
    /// two-plus matches (the overloaded case — callers conservatively
    /// Return every workspace free-function overload matching `qualified`.
    /// Unlike `lookup_function_signature`, this does NOT suppress the
    /// 2+-match case — callers get the full set
    /// and are expected to run their own overload resolution. Returns an
    /// empty Vec if no workspace function has that name.
    ///
    /// External (typedb) functions are intentionally not consulted here —
    /// their signature data isn't wired through to the checker yet.
    pub fn lookup_function_overloads(&self, qualified: &str) -> Vec<OverloadSig> {
        let mut out = Vec::new();
        let alt_names = workspace_function_property_candidates(qualified);
        for name in &alt_names {
            for s in self.lookup_workspace(name) {
                if let SymbolKind::Function {
                    return_type,
                    params,
                    min_args,
                    ..
                } = &s.kind
                {
                    out.push(OverloadSig {
                        param_names: params.iter().map(|(name, _)| Some(name.clone())).collect(),
                        param_types: params.iter().map(|(_, ty_text)| ty_text.clone()).collect(),
                        // B027: workspace param text carries the source
                        // `&out` decoration (the `Reference` type span
                        // covers it) — derive the flag from it.
                        param_out: params
                            .iter()
                            .map(|(_, ty_text)| ty_text.contains("&out"))
                            .collect(),
                        param_modes: params
                            .iter()
                            .map(|(_, ty_text)| param_mode_from_text(ty_text))
                            .collect(),
                        min_args: *min_args,
                        return_type: return_type.clone(),
                    });
                }
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
        let ext = self.lookup_external_function_param_overloads(qualified);
        if !ext.as_ref().is_some_and(|v| v.is_empty()) {
            return ext.unwrap_or_default();
        }
        // Qualified miss: retry the bare tail — user code may call a
        // namespaced function by its short name (absorbs the inline
        // ladder signature-help used to carry, GH #41).
        if let Some((_, bare)) = qualified.rsplit_once("::") {
            let ws = self.lookup_function_overloads(bare);
            if !ws.is_empty() {
                return ws;
            }
            return self
                .lookup_external_function_param_overloads(bare)
                .unwrap_or_default();
        }
        Vec::new()
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
        for s in self.lookup_workspace(class_name) {
            if let SymbolKind::Class { parents, .. } = &s.kind {
                return parents.clone();
            }
        }
        Vec::new()
    }

    /// True when `class_name` names a workspace class symbol (used to decide
    /// whether a qualified `Class::Method` callee should pull inherited
    /// overloads into its arity/overload set — GH #34).
    pub fn is_workspace_class(&self, class_name: &str) -> bool {
        self.lookup_workspace(class_name)
            .iter()
            .any(|s| s.name == class_name && matches!(s.kind, SymbolKind::Class { .. }))
    }

    /// Collect every workspace method overload named `Class::method`, walking
    /// the class's inheritance chain so an overload declared on a parent counts
    /// toward arity/overload resolution on the child (GH #34). The qualified
    /// `class_name` may itself be namespace-qualified (`Ns::Child`); parents are
    /// normalized in that namespace context. Cycle-guarded.
    pub fn lookup_method_overloads_with_inheritance(
        &self,
        class_name: &str,
        method: &str,
    ) -> Vec<OverloadSig> {
        let mut out = Vec::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(class_name)]);
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let qualified = format!("{}::{}", name, method);
            out.extend(self.lookup_function_overloads(&qualified));
            for parent in self.workspace_class_parents(&name) {
                queue.push_back(self.normalize_workspace_class_name_in_context(&parent, &name));
            }
        }
        out
    }

    /// Resolve member symbols (methods) named `member` walking the class's
    /// inheritance chain: the class itself first, then parents breadth-first.
    /// Names are normalized in namespace context and the walk is
    /// cycle-guarded — same discipline as
    /// [`lookup_method_overloads_with_inheritance`]. Returns symbols in
    /// chain order (own-class first); empty when nothing matches.
    pub fn lookup_member_symbols_with_inheritance(
        &self,
        class_name: &str,
        member: &str,
    ) -> Vec<&Symbol> {
        let mut out = Vec::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(class_name)]);
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let qualified = format!("{}::{}", name, member);
            out.extend(self.lookup_workspace(&qualified));
            for parent in self.workspace_class_parents(&name) {
                queue.push_back(self.normalize_workspace_class_name_in_context(&parent, &name));
            }
        }
        out
    }

    /// External typedb enum lookup by short name: the enum's qualified
    /// name if `short` names an enum in the type database (absorbs the
    /// former `scope.external` field read in the checker, GH #41).
    pub fn external_enum_by_short_name(&self, short: &str) -> Option<String> {
        let ext = self.external?;
        ext.find_by_short_name(short)
            .iter()
            .find(|enum_name| ext.lookup_source_enum(enum_name).is_some())
            .cloned()
    }

    /// All members (fields + methods, typed) declared on `class_name`
    /// itself — NOT walking parents. Each method appears with its return
    /// type as the member type (same approximation the checker uses for
    /// implicit-this resolution). Absorbs the former pub-field prefix
    /// scans in `checker.rs` / `signature.rs` (GH #41).
    pub fn workspace_class_member_pairs(&self, class_name: &str) -> Vec<(String, TypeRepr)> {
        let prefix = format!("{}::", class_name);
        let mut out = Vec::new();
        for s in self.lookup_workspace_members(class_name) {
            let member_name = s.name.strip_prefix(&prefix).unwrap_or(&s.name);
            match &s.kind {
                crate::symbols::scope::SymbolKind::Variable { type_name } => {
                    out.push((
                        member_name.to_string(),
                        TypeRepr::parse_type_string(type_name),
                    ));
                }
                crate::symbols::scope::SymbolKind::Function { return_type, .. } => {
                    out.push((
                        member_name.to_string(),
                        TypeRepr::parse_type_string(return_type),
                    ));
                }
                _ => {}
            }
        }
        out
    }

    /// Function symbols on a class (qualified `Class::method`) as
    /// `(return_type, params, min_args, doc)` tuples — the data
    /// `signature.rs` used to walk `workspace.all_symbols()` for (GH #41).
    pub fn workspace_class_method_sigs(
        &self,
        class_name: &str,
        method: &str,
    ) -> Vec<(String, Vec<(String, String)>, usize, Option<String>)> {
        let qualified = format!("{}::{}", class_name, method);
        let mut out = Vec::new();
        for s in self.lookup_workspace(&qualified) {
            if let crate::symbols::scope::SymbolKind::Function {
                return_type,
                params,
                min_args,
                ..
            } = &s.kind
            {
                out.push((
                    return_type.clone(),
                    params.clone(),
                    *min_args,
                    s.doc.clone(),
                ));
            }
        }
        out
    }

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
            let mut candidates = self.lookup_workspace(&qualified);
            candidates.extend(self.lookup_workspace(&qualified_getter));
            candidates.extend(self.lookup_workspace(&qualified_setter));
            for s in candidates {
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

    /// Resolve a bare class name to its pooled-table qualified form
    /// (e.g. `Status` inside `namespace Repro` → `Repro::Status`). Public
    /// for the unused-decl usage recorder, which needs the same
    /// normalization when walking inheritance chains from type text that
    /// carries no namespace qualification.
    pub fn normalize_class_name(&self, name: &str) -> String {
        self.normalize_workspace_class_name(name)
    }

    fn normalize_workspace_class_name_in_context(
        &self,
        name: &str,
        context_class_name: &str,
    ) -> String {
        if !name.contains("::") {
            if let Some((ns, _)) = context_class_name.rsplit_once("::") {
                let candidate = format!("{}::{}", ns, name);
                if !self.lookup_workspace(&candidate).is_empty() {
                    return candidate;
                }
            }
        }
        if !self.lookup_workspace(name).is_empty() {
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
        let mut candidates: Vec<&crate::symbols::scope::Symbol> = self.lookup_workspace(qualified);
        candidates.extend(self.lookup_workspace_tail(qualified));
        for s in candidates {
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
        // Workspace fallback: the symbol table stores the declared return
        // type string — parse it (e.g. `void` → Primitive(Void)) instead
        // of silencing with Error, which surfaced as `(local) <error>`
        // hover on workspace function calls.
        if let Some(s) = self.lookup_workspace_function_property(qualified) {
            return match &s.kind {
                SymbolKind::Function { return_type, .. } => {
                    Some(TypeRepr::parse_type_string(return_type))
                }
                _ => Some(TypeRepr::Error(String::new())),
            };
        }
        None
    }

    fn lookup_workspace_function_property(&self, qualified: &str) -> Option<&Symbol> {
        let candidates = workspace_function_property_candidates(qualified);
        let mut found = None;
        for name in &candidates {
            for symbol in self.lookup_workspace(name) {
                if !matches!(symbol.kind, SymbolKind::Function { .. }) {
                    continue;
                }
                if found.is_some() {
                    return None;
                }
                found = Some(symbol);
            }
        }
        found
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

    /// The shipped type-DB fixtures, loaded as a `TypeIndex`. Several tests
    /// assert against real engine enums, where a hand-built stub would not
    /// prove anything about the data the checker actually sees.
    fn typedb_fixture_index() -> crate::typedb::index::TypeIndex {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        crate::typedb::index::TypeIndex::load(
            &fixtures.join("OpenplanetCore.json"),
            &fixtures.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
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

    /// B016: contextual keys preserve anonymous member-enum data in the
    /// type DB, but those synthesized keys never name a source-level type.
    #[test]
    fn anonymous_member_enum_keys_stay_internal_to_source_resolution() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let anonymous = "Control::CControlEntry::Type";
        assert!(
            index
                .lookup_enum(anonymous)
                .is_some_and(|info| info.anonymous),
            "fixture guard: this must remain an internally queryable anonymous enum"
        );
        assert!(
            index.lookup_source_enum(anonymous).is_none(),
            "the source-facing API must hide synthetic keys"
        );
        assert!(
            !scope.has_enum(anonymous) && !scope.resolves(anonymous),
            "synthesized keys must not resolve from source"
        );
        assert_eq!(scope.canonical_enum_key(anonymous), None);
        assert_eq!(
            scope.canonicalize_type_name(anonymous),
            anonymous,
            "an anonymous key must remain unresolved rather than canonicalized"
        );

        let named = "Game::CGameCtnBlock::ECardinalDirections";
        assert!(index.lookup_source_enum(named).is_some());
        assert!(scope.has_enum(named) && scope.resolves(named));
        assert_eq!(
            scope.canonicalize_type_name("CGameCtnBlock::ECardinalDirections"),
            named,
            "the anonymous filter must not hide named enums"
        );
        assert_eq!(
            scope.canonical_enum_key("CGameCtnBlock::ECardinalDirections"),
            Some(named.to_string())
        );
    }

    /// B044 (the other half of the rule above): a bare identifier that names
    /// only a VALUE of an anonymous member enum is NOT a global identifier.
    ///
    /// The enum has no name in the game, so nothing in source can reach its
    /// values except through the declaring member's type. Game ground truth,
    /// RemoteBuild probe, OP 1.29.5: `No matching symbol 'HyperTnL'`.
    ///
    /// Both classes of anonymous enum are covered on purpose. `HyperTnL`,
    /// `SlideNoControl`, `NotTimed`, `RenderPlaneReflect` and
    /// `Download_WaitingSources` belong to enums that won their module's key
    /// collision before B016 and were accepted long before it existed (set
    /// B); `MinimalReadOnly`, `TimeHhMmSsMil` and `Text_Icon` belong to enums
    /// B016 recovered (set C). One call site governs both, and the game
    /// rejects both.
    #[test]
    fn bare_anonymous_enum_value_is_not_a_global_identifier() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // Set B — accepted before B016 (their enum survived the collision).
        for name in [
            "HyperTnL",
            "SlideNoControl",
            "NotTimed",
            "RenderPlaneReflect",
            "Download_WaitingSources",
        ] {
            assert!(
                !scope.has_global_ident(name),
                "`{name}` is only an anonymous enum value; the game rejects it"
            );
        }
        // Set C — dropped before B016, recovered by it. The narrowing must
        // not be scoped to the enums B016 recovered.
        for name in ["MinimalReadOnly", "TimeHhMmSsMil", "Text_Icon"] {
            assert!(
                !scope.has_global_ident(name),
                "`{name}` is only an anonymous enum value; the game rejects it"
            );
        }
        assert!(
            !scope.has_global_ident("NoSuchEngineEnumValue"),
            "the scan must still reject genuinely unknown identifiers"
        );
    }

    /// B044: a value name carried by TWO different anonymous member enums is
    /// still not a symbol — the narrowing is a property of the enum's
    /// namelessness, not of how many nameless enums happen to share the name.
    #[test]
    fn bare_value_of_two_anonymous_enums_is_still_not_a_global_identifier() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for (name, first, second) in [
            (
                "BackFlip",
                "Scene::CSceneCharVisState::CharPhyState",
                "Scene::CSceneCharVisState::CharPhyState2",
            ),
            (
                "Campaign",
                "Game::CGameCtnChallenge::Kind",
                "Game::CGameCtnChallengeInfo::Kind",
            ),
        ] {
            for key in [first, second] {
                assert!(
                    index.lookup_enum(key).is_some_and(
                        |info| info.anonymous && info.values.iter().any(|(v, _)| v == name)
                    ),
                    "fixture guard: `{key}` must stay an anonymous enum carrying `{name}`"
                );
            }
            assert!(
                !scope.has_global_ident(name),
                "`{name}` is a value of two anonymous enums and of no named one"
            );
        }
    }

    /// B044: the change's effect set, pinned against the shipped type DB.
    ///
    /// The narrowing can only ever touch a value name that belongs to an
    /// anonymous enum and to NO named one — every other name reaches
    /// `has_global_ident` through a lookup that runs before the enum scan, or
    /// through the scan's surviving named half. This asserts both halves over
    /// the whole DB rather than over hand-picked names, so a regression
    /// anywhere in the enum index shows up here.
    ///
    /// 258 is the exact size of that set, and it is the list
    /// `scripts/gen_b044_probe.py` generates and the game was probed against.
    /// Refreshing the type DB moves it: regenerate the fixture and re-probe.
    ///
    /// It was 261 until B046. `ESceneCharVisNotice` is a module-level enum
    /// no dump member is typed by, so only `Openplanet.h` names it; once the
    /// loader reads the header, its 3 values that the game accepts
    /// (`DoubleJump`, `Jump`, `TechJump`) land in the NAMED half and leave
    /// the narrowed set. The narrowing itself is unchanged — the enum they
    /// belong to stopped being invisible.
    #[test]
    fn the_narrowing_touches_only_anonymous_only_value_names() {
        use std::collections::HashSet;

        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let mut named: HashSet<&str> = HashSet::new();
        let mut anonymous: HashSet<&str> = HashSet::new();
        for (_, info) in index.enums_iter() {
            let bucket = if info.anonymous {
                &mut anonymous
            } else {
                &mut named
            };
            for (value, _) in &info.values {
                bucket.insert(value.as_str());
            }
        }
        assert!(
            !named.is_empty() && !anonymous.is_empty(),
            "fixture guard: the DB must carry both kinds of enum"
        );

        for value in &named {
            assert!(
                scope.has_global_ident(value),
                "`{value}` is a NAMED enum's value and must keep resolving"
            );
        }

        let mut narrowed: Vec<&str> = anonymous.difference(&named).copied().collect();
        narrowed.sort_unstable();
        assert_eq!(
            narrowed.len(),
            258,
            "the narrowed set changed size — regenerate the B044 probe fixture \
             and re-probe the game"
        );
        for value in &narrowed {
            assert!(
                !scope.has_global_ident(value),
                "`{value}` is reachable only through an anonymous enum"
            );
        }

        // Cross-check the Python generator against this index: the probe the
        // GAME was run against must be this exact set, not merely a set of
        // the same size. A drift between the two would silently probe the
        // wrong names.
        assert_eq!(
            narrowed,
            b044_probe_fixture_names(),
            "the B044 probe fixture no longer lists the narrowed set — \
             regenerate it with scripts/gen_b044_probe.py"
        );
    }

    /// The identifiers `tests/fixtures/issue-repros/b044-anon-enum-value-names`
    /// probes, read back out of the generated fixture, sorted.
    fn b044_probe_fixture_names() -> Vec<String> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue-repros/b044-anon-enum-value-names/Main.as");
        let source = std::fs::read_to_string(&path).expect("the probe fixture must exist");
        let mut names: Vec<String> = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("print(\"\" + "))
            .filter_map(|rest| rest.split(')').next())
            .map(str::to_string)
            .collect();
        names.sort();
        names
    }

    /// B044 fence: a value name that ALSO appears in a NAMED enum keeps
    /// resolving. `VCenter2` is a value of the anonymous
    /// `Control::CControlBase::AlignVertical` and of the named
    /// `Game::CGameManialinkControl::EAlignVertical`, and a bare reference to
    /// it is legitimate in-game.
    #[test]
    fn bare_value_shared_with_a_named_enum_still_resolves() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let named = "Game::CGameManialinkControl::EAlignVertical";
        assert!(
            index
                .lookup_source_enum(named)
                .is_some_and(|info| info.values.iter().any(|(v, _)| v == "VCenter2")),
            "fixture guard: the named enum must still carry `VCenter2`"
        );
        assert!(
            index
                .enums_iter()
                .any(|(_, info)| info.anonymous
                    && info.values.iter().any(|(v, _)| v == "VCenter2")),
            "fixture guard: `VCenter2` must also be an anonymous enum value"
        );
        assert!(
            scope.has_global_ident("VCenter2"),
            "a value shared with a named enum must keep resolving"
        );
    }

    /// B046: a bare value of a MODULE-LEVEL engine enum resolves.
    ///
    /// The game accepts these (RemoteBuild probe, OP 1.29.5, 2026-08-30:
    /// `DoubleJump`, `TechJump`, `BulletHit`, `Land` all compile), and it
    /// accepts them because every engine namespace is `using namespace`'d —
    /// there is no namespace to qualify them with. Before B046 the LSP
    /// rejected all 55 value names these two enums carry alone.
    #[test]
    fn bare_value_of_a_module_level_enum_resolves() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for name in ["ESceneCharVisNotice", "ESceneVehicleVisEvent"] {
            let info = index
                .lookup_source_enum(name)
                .unwrap_or_else(|| panic!("fixture guard: {name} must be indexed"));
            assert!(!info.anonymous, "{name} is spellable, so not anonymous");
            for (value, _) in &info.values {
                assert!(
                    scope.has_global_ident(value),
                    "`{value}` is a value of the module-level enum {name}"
                );
            }
        }
    }

    /// B046: the enum NAME is a type the game accepts unqualified, so the
    /// resolver must take it as one.
    #[test]
    fn a_module_level_enum_name_resolves_as_a_type() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for name in ["ESceneCharVisNotice", "ESceneVehicleVisEvent"] {
            assert!(scope.has_enum(name), "`{name}` must resolve as an enum");
            assert!(scope.resolves(name), "`{name}` must resolve as a type");
        }
    }

    /// B046 fence: no namespace is invented for a module-level enum.
    ///
    /// The game rejects `Scene::ESceneCharVisNotice` with `Namespace 'Scene'
    /// doesn't exist.` and `Enums::ESceneCharVisNotice` likewise, so neither
    /// spelling may become a key. What is pinned here is that the INDEX
    /// carries no such entry, which is what a namespace-inventing fix would
    /// have added.
    ///
    /// B046 left `has_enum` answering true for both anyway, through the tail
    /// fallback it was forbidden to touch. B052 closed that; the resolution
    /// half of the rule is pinned by
    /// `a_nadeo_module_prefix_does_not_qualify_a_global_enum`.
    #[test]
    fn no_namespace_is_invented_for_a_module_level_enum() {
        let index = typedb_fixture_index();
        for qualified in [
            "Scene::ESceneCharVisNotice",
            "Enums::ESceneCharVisNotice",
            "Scene::ESceneVehicleVisEvent",
            "Enums::ESceneVehicleVisEvent",
        ] {
            assert!(
                index.lookup_enum(qualified).is_none(),
                "`{qualified}` is not a name the game accepts"
            );
        }
        assert!(!index.namespace_exists("Enums"));
    }

    // ---- B052: qualified paths to a GLOBAL enum -----------------------
    //
    // AngelScript resolves `P::T` by looking `T` up in scope `P` and then
    // walking P's ancestors out to the global namespace. Three things follow,
    // all of them measured against the live game (RemoteBuild, Trackmania +
    // Openplanet 1.29.5, 2026-08-30) rather than reasoned from the dump:
    //
    //   * `P` is not a scope at all  -> `Namespace 'P' doesn't exist.`
    //   * `P` is a scope, `T` is not reachable from it
    //                                -> `Identifier 'T' is not a data type in
    //                                    namespace 'P' or parent`
    //   * `P` is a scope and `T` is a GLOBAL enum -> accepted.
    //
    // Only three named enum keys are global (single-segment): `VirtualKey`,
    // `ESceneVehicleVisEvent`, `ESceneCharVisNotice`. Every other key is
    // `Module::Class::Enum` or `CoreNs::Enum`, so this rule reaches exactly
    // those three names — the pre-B052 fallback's whole surface, because
    // `lookup_source_enum(tail)` demands `tail` be a complete key.
    //
    // What IS a scope, and what is not:
    //   * a Core namespace (`UI`, `Meta`, `Json`, `IO`, `nvg`, `UI::Font`, …)
    //     — yes, and the walk reaches global from it;
    //   * an engine class that declares at least one enum, named or
    //     anonymous, spelled WITHOUT the loader's synthetic module prefix
    //     (`CGameMenu`, `CGameCtnApp`, `CGameCtnChallenge`) — yes;
    //   * an engine class that declares none (`CSceneVehicleVis`,
    //     `CTrackMania`, `CMwNod`, `CSystemFidFile`, `Meta::Plugin`) — no;
    //   * a namespace the workspace itself declares, nested included — yes;
    //   * the loader's Nadeo MODULE prefixes (`Scene`, `Game`, `Plug`,
    //     `System`, `Hms`, …) — no. They are this crate's invention for
    //     keying the dump and name nothing the game has.

    /// B052 red: a Nadeo module prefix is not a namespace, so a qualified
    /// path through one names nothing. Every name below was submitted to the
    /// live game and rejected with `Namespace '<prefix>' doesn't exist.`
    #[test]
    fn a_nadeo_module_prefix_does_not_qualify_a_global_enum() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // Every module the loader keys the Nadeo dump under, plus the
        // `Openplanet.h` bucket name and a control that is nothing at all.
        let prefixes = [
            "Scene",
            "Game",
            "Plug",
            "System",
            "Hms",
            "Input",
            "Control",
            "Function",
            "Graphic",
            "Script",
            "Vision",
            "Xml",
            "TrackMania",
            "ShootMania",
            "MwFoundations",
            "MetaNotPersistent",
            "Enums",
            "Bogus",
        ];
        for prefix in prefixes {
            for tail in ["VirtualKey", "ESceneVehicleVisEvent", "ESceneCharVisNotice"] {
                let qualified = format!("{prefix}::{tail}");
                assert!(
                    !scope.has_enum(&qualified),
                    "the game answers `Namespace '{prefix}' doesn't exist.` for `{qualified}`"
                );
                assert!(
                    !scope.resolves(&qualified),
                    "`{qualified}` must not resolve as a type either"
                );
                assert_eq!(scope.canonical_enum_key(&qualified), None);
                assert_eq!(scope.canonicalize_type_name(&qualified), qualified);
            }
        }
    }

    /// B052 red: a deeper path through a non-scope is no better, and neither
    /// is an engine class that declares no enum — the game reports
    /// `Namespace 'Game::CGameMenu' doesn't exist.` for the first and
    /// `Identifier 'VirtualKey' is not a data type in namespace
    /// 'CSceneVehicleVis' or parent` for the second.
    #[test]
    fn a_scopeless_or_enumless_prefix_does_not_qualify_a_global_enum() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for prefix in [
            // module-prefixed class path: the module segment is synthetic.
            "Game::CGameMenu",
            "MetaNotPersistent::NPlugAnim",
            // real engine classes that declare no enum, so no namespace of
            // their name exists and the ancestor walk never starts.
            "CSceneVehicleVis",
            "CTrackMania",
            "CMwNod",
            "CSystemFidFile",
            // a Core CLASS (not namespace) under a real Core namespace.
            "Meta::Plugin",
        ] {
            for tail in ["VirtualKey", "ESceneVehicleVisEvent", "ESceneCharVisNotice"] {
                let qualified = format!("{prefix}::{tail}");
                assert!(
                    !scope.has_enum(&qualified),
                    "the game rejects `{qualified}`"
                );
                assert!(
                    !scope.resolves(&qualified),
                    "`{qualified}` must not resolve"
                );
                assert_eq!(scope.canonical_enum_key(&qualified), None);
                assert_eq!(scope.canonicalize_type_name(&qualified), qualified);
            }
        }
    }

    /// B052 fence: the ancestor walk is real, so a REAL scope does qualify a
    /// global enum and must keep resolving. Narrowing the fallback without
    /// this test would trade a false negative for a false POSITIVE — the
    /// damaging direction. Every name below the game accepted.
    #[test]
    fn a_real_scope_qualifies_a_global_enum() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let prefixes = [
            // Core namespaces, flat and nested.
            "UI",
            "Meta",
            "Json",
            "Net",
            "Audio",
            "IO",
            "nvg",
            "Text",
            "Math",
            "Regex",
            "Dev",
            "Import",
            "XML",
            "Time",
            "Icons",
            "Fids",
            "Path",
            "Crypto",
            "Auth",
            "Display",
            "Reflection",
            "Permissions",
            "SQLite",
            "Settings",
            "Tests",
            "mat3",
            "mat4",
            "string",
            "UI::Font",
            "UI::Plot",
            // Engine classes that declare an enum, module prefix stripped:
            // `CGameMenu` a named one, `CGameCtnApp` only an anonymous one.
            "CGameMenu",
            "CGameCtnApp",
            "CGameCtnChallenge",
        ];
        for prefix in prefixes {
            for tail in ["VirtualKey", "ESceneVehicleVisEvent", "ESceneCharVisNotice"] {
                let qualified = format!("{prefix}::{tail}");
                assert!(
                    scope.has_enum(&qualified),
                    "the game accepts `{qualified}` through the ancestor walk"
                );
                assert!(scope.resolves(&qualified), "`{qualified}` must resolve");
                assert_eq!(
                    scope.canonical_enum_key(&qualified),
                    Some(tail.to_string()),
                    "`{qualified}` canonicalizes to its global enum key"
                );
                assert_eq!(scope.canonicalize_type_name(&qualified), tail);
            }
        }
    }

    /// B052 fence: the bare spellings are untouched — they are how the game
    /// wants these three written in the first place.
    #[test]
    fn a_global_enum_still_resolves_bare() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for name in ["VirtualKey", "ESceneVehicleVisEvent", "ESceneCharVisNotice"] {
            assert!(scope.has_enum(name), "`{name}` must resolve bare");
            assert!(scope.resolves(name));
            assert!(scope.has_global_ident(name));
        }
    }

    /// B052 red: a namespace the WORKSPACE declares is a scope, so the same
    /// ancestor walk applies inside plugin code. Probed with a plugin that
    /// declares `namespace Foo { namespace Bar { … } }`: the game accepts
    /// `Foo::VirtualKey` and `Foo::Bar::VirtualKey`, and answers `Identifier
    /// 'ZZZNoSuch' is not a data type in namespace 'Foo' or parent` for a
    /// name that is genuinely absent.
    #[test]
    fn a_workspace_namespace_qualifies_a_global_enum() {
        let index = typedb_fixture_index();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![
                make_symbol("Foo", SymbolKind::Namespace),
                make_symbol("Foo::Bar", SymbolKind::Namespace),
            ],
        );
        let scope = GlobalScope::new(&ws, Some(&index));

        for tail in ["VirtualKey", "ESceneVehicleVisEvent", "ESceneCharVisNotice"] {
            for prefix in ["Foo", "Foo::Bar"] {
                let qualified = format!("{prefix}::{tail}");
                assert!(
                    scope.has_enum(&qualified),
                    "`{qualified}` resolves through the workspace namespace"
                );
            }
            // A namespace nobody declares is still nothing.
            assert!(!scope.has_enum(&format!("Quux::{tail}")));
        }
    }

    /// B052: the fallback the old tail match was reaching for. A nested
    /// engine enum written the way the game wants it — class scope, no
    /// module prefix — is an enum, and `has_enum` said no. The suffix has to
    /// be a genuine trailing PATH of a key, not merely its last segment.
    #[test]
    fn a_module_stripped_nested_enum_path_is_an_enum() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // Left: what the game accepts. Right: the key the loader stores.
        for (spelling, key) in [
            ("CGameMenu::EMenuOrder", "Game::CGameMenu::EMenuOrder"),
            (
                "CGameCtnBlock::ECardinalDirections",
                "Game::CGameCtnBlock::ECardinalDirections",
            ),
            ("CGameHighScore::EMedal", "Game::CGameHighScore::EMedal"),
            (
                "CHmsViewport::EPC3Quality",
                "Hms::CHmsViewport::EPC3Quality",
            ),
        ] {
            assert!(
                index.lookup_source_enum(key).is_some(),
                "fixture guard: `{key}` must be a named enum key"
            );
            assert!(
                scope.has_enum(spelling),
                "the game accepts `{spelling}`, so it is an enum"
            );
            assert!(scope.resolves(spelling));
        }

        // The suffix must be a whole trailing path. A real enum name behind
        // a prefix that is not its declaring class is still nothing, and the
        // game agrees: `Namespace 'Bogus' doesn't exist.`
        assert!(!scope.has_enum("Bogus::EMenuOrder"));
        assert!(!scope.has_enum("CGameHighScore::EMenuOrder"));
    }

    // ---- B053: a bare value two globally visible enums carry ------------
    //
    // The game resolves a bare enum-value name against the enums in scope,
    // and when two of them carry it, it refuses to choose:
    // `Found multiple matching enum values`. openplanet-lsp took the first
    // hit and said nothing.
    //
    // Which enums are in scope for a BARE name is the whole question, and it
    // is narrower than "every named enum". Ground truth, all 2989 bare value
    // names of the shipped DB's named enums submitted to the live game
    // (RemoteBuild, Trackmania + Openplanet 1.29.5, 2026-08-30):
    //
    //     2373  No matching symbol
    //      592  accepted
    //       22  Found multiple matching enum values
    //        2  resolved to a non-enum symbol (`Main`, `RGBAColor`)
    //
    // Only enums the game puts in the GLOBAL namespace take part — 68 keys,
    // 60 distinct names. A nested enum's values (`CGameMenu::EMenuOrder`) and
    // a Core namespaced enum's values (`UI::Cond`) are not reachable bare at
    // all, which is the 2373. Counting NAMES not keys matters: the loader
    // keys one engine enum under several modules, so `EHmsLightMapQuality`
    // appears twice and is still one enum.
    //
    // The ticket predicted this set from "carried by two or more NAMED
    // enums", which is 758 names — 34x too many. The rule above predicts
    // 2987 of the 2989 verdicts, and misses only the two that resolve to
    // something that is not an enum value.
    //
    // The 2373 are a real and much larger false negative, and they are NOT
    // fixed here: narrowing the bare-value scan needs its own probe and its
    // own false-positive gate. B053 is the ambiguity half only.

    /// B053 red: each of the 22 names the game rejects as ambiguous.
    #[test]
    fn a_value_two_global_enums_carry_is_ambiguous() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // Every name the live game answered `Found multiple matching enum
        // values` for, with no residual: this list IS the game's answer.
        let ambiguous = [
            "Blend",
            "Cling",
            "Down",
            "Energy",
            "FreeWheeling",
            "Heavy",
            "High",
            "Light",
            "Medium",
            "Mesh",
            "None",
            "Player",
            "Road",
            "Static",
            "Turbo",
            "Undefined",
            "Up",
            "Vehicle",
            "WallJump",
            "Water",
            "WaterSplash",
            "XXX_Null",
        ];
        for name in ambiguous {
            let candidates = scope
                .ambiguous_global_enum_value(name)
                .unwrap_or_else(|| panic!("the game rejects `{name}` as ambiguous"));
            assert!(
                candidates.len() >= 2,
                "`{name}` needs at least two candidates to be ambiguous, got {candidates:?}"
            );
            let mut sorted = candidates.clone();
            sorted.sort();
            assert_eq!(
                sorted, candidates,
                "candidates must be sorted for a stable message"
            );
        }

        // Spot-check the candidate sets the message will print.
        assert_eq!(
            scope.ambiguous_global_enum_value("Turbo").unwrap(),
            vec![
                "EPlugSurfaceGameplayId".to_string(),
                "ESceneCharVisNotice".to_string(),
                "ESceneVehicleVisEvent".to_string(),
            ]
        );
        assert_eq!(
            scope.ambiguous_global_enum_value("WallJump").unwrap(),
            vec![
                "EPlugSurfaceMaterialId".to_string(),
                "ESceneCharVisNotice".to_string(),
            ]
        );
    }

    /// B053 fence: a value only ONE globally visible enum carries is not
    /// ambiguous and must stay silent. The game compiles all of these.
    #[test]
    fn a_value_one_global_enum_carries_is_not_ambiguous() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for name in [
            // ESceneVehicleVisEvent, carried by nothing else global.
            "GlassSmash",
            "Horn",
            "ImpactBody",
            "BeginBulletTime",
            // ESceneCharVisNotice.
            "JumpChain1",
            "DoubleJump",
            "TechJump",
            "GestureVictory",
            // VirtualKey.
            "Space",
            "Escape",
        ] {
            assert!(
                scope.ambiguous_global_enum_value(name).is_none(),
                "the game accepts `{name}`; it must not be called ambiguous"
            );
            assert!(scope.has_global_ident(name), "`{name}` must keep resolving");
        }
    }

    /// B053 fence: a value carried only by NESTED or NAMESPACED enums is not
    /// ambiguous either — those enums are not in scope for a bare name, so
    /// there is nothing to be ambiguous between. (The game rejects these with
    /// `No matching symbol`, a separate and much larger false negative that
    /// B053 deliberately does not touch; pinned here so a later fix for it
    /// cannot be mistaken for this one.)
    #[test]
    fn a_value_only_nested_enums_carry_is_not_ambiguous() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // `Ordered` belongs to the nested `Game::CGameMenu::EMenuOrder`;
        // `Object` to the nested `GameData::CGameCtnCollector::EProdState`
        // family. Neither is reachable bare in-game.
        for name in ["Ordered", "Aborted"] {
            assert!(
                scope.ambiguous_global_enum_value(name).is_none(),
                "`{name}` is carried by no globally visible enum"
            );
        }
        assert!(
            index.global_enum_value_owners("Ordered").is_empty(),
            "a nested enum's value has no globally visible owner"
        );
    }

    /// B053: a workspace VARIABLE or FUNCTION of the name outranks the
    /// engine's enums, and the game agrees — `int Turbo = 1;` in a plugin
    /// makes bare `Turbo` compile even though two engine enums carry it.
    #[test]
    fn a_workspace_variable_or_function_outranks_the_engine_enums() {
        let index = typedb_fixture_index();
        for kind in [
            SymbolKind::Variable {
                type_name: "int".into(),
            },
            SymbolKind::Function {
                return_type: "void".into(),
                params: Vec::new(),
                min_args: 0,
                imported: false,
            },
        ] {
            let mut ws = SymbolTable::new();
            let fid = ws.allocate_file_id();
            ws.set_file_symbols(fid, vec![make_symbol("Turbo", kind.clone())]);
            let scope = GlobalScope::new(&ws, Some(&index));
            assert!(
                scope.ambiguous_global_enum_value("Turbo").is_none(),
                "a workspace `{kind:?}` named `Turbo` is the answer, so there \
                 is no ambiguity to report"
            );
        }
    }

    /// B053: a plugin's own GLOBAL enum JOINS the pool rather than shielding
    /// against it — the counter-intuitive half, and the one the game settled.
    /// `GlassSmash` is carried by exactly one engine enum and compiles bare;
    /// add `enum LocalPick { GlassSmash }` to the plugin and the game starts
    /// answering `Found multiple matching enum values`.
    #[test]
    fn a_workspace_global_enum_joins_the_ambiguity_pool() {
        let index = typedb_fixture_index();
        let bare = SymbolTable::new();
        let bare_scope = GlobalScope::new(&bare, Some(&index));
        assert!(
            bare_scope
                .ambiguous_global_enum_value("GlassSmash")
                .is_none(),
            "fixture guard: one engine enum carries `GlassSmash`, so it starts \
             out unambiguous"
        );

        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![
                make_symbol("LocalPick", SymbolKind::Enum { values: Vec::new() }),
                make_symbol(
                    "LocalPick::GlassSmash",
                    SymbolKind::EnumValue {
                        enum_name: "LocalPick".into(),
                        value: Some(0),
                    },
                ),
            ],
        );
        let scope = GlobalScope::new(&ws, Some(&index));
        assert_eq!(
            scope.ambiguous_global_enum_value("GlassSmash"),
            Some(vec![
                "ESceneVehicleVisEvent".to_string(),
                "LocalPick".to_string()
            ]),
            "the plugin's own enum must appear as a candidate, by name"
        );
    }

    /// B053: a NAMESPACED workspace enum and a class member take no part —
    /// the same rule that keeps a namespaced or nested ENGINE enum out.
    /// Probed: with `namespace NsBox { enum NsEnum { Horn } }` the game still
    /// compiles bare `Horn`.
    #[test]
    fn a_namespaced_workspace_enum_or_class_member_does_not_join_the_pool() {
        let index = typedb_fixture_index();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![
                make_symbol("NsBox", SymbolKind::Namespace),
                make_symbol("NsBox::NsEnum", SymbolKind::Enum { values: Vec::new() }),
                make_symbol(
                    "NsBox::NsEnum::Horn",
                    SymbolKind::EnumValue {
                        enum_name: "NsBox::NsEnum".into(),
                        value: Some(0),
                    },
                ),
                make_symbol(
                    "Holder",
                    SymbolKind::Class {
                        parents: Vec::new(),
                        members: vec!["JumpChain1".into()],
                    },
                ),
                make_symbol(
                    "Holder::JumpChain1",
                    SymbolKind::Variable {
                        type_name: "int".into(),
                    },
                ),
            ],
        );
        let scope = GlobalScope::new(&ws, Some(&index));
        assert!(
            scope.ambiguous_global_enum_value("Horn").is_none(),
            "a namespaced workspace enum is not in scope for a bare name"
        );
        assert!(
            scope.ambiguous_global_enum_value("JumpChain1").is_none(),
            "a class member is not a global"
        );
    }

    /// B053: which enums are globally visible, pinned directly. The index
    /// half of the rule, so a loader change that starts keying enums
    /// differently fails here rather than silently changing the diagnostic.
    #[test]
    fn only_module_level_enums_are_globally_visible() {
        let index = typedb_fixture_index();

        // Global: the two header enums, and a Nadeo module-level one.
        for (value, owner) in [
            ("Jump", "ESceneCharVisNotice"),
            ("GlassSmash", "ESceneVehicleVisEvent"),
            ("Space", "VirtualKey"),
        ] {
            assert!(
                index
                    .global_enum_value_owners(value)
                    .iter()
                    .any(|o| o == owner),
                "`{value}` must be attributed to the global `{owner}`"
            );
        }

        // Not global: a nested Nadeo enum, and a namespaced Core enum.
        assert!(index
            .lookup_source_enum("Game::CGameMenu::EMenuOrder")
            .is_some());
        assert!(
            index.global_enum_value_owners("Ordered").is_empty(),
            "a nested engine enum contributes no globally visible value"
        );
        assert!(index.lookup_source_enum("UI::Cond").is_some());
        for (value, _) in &index.lookup_source_enum("UI::Cond").unwrap().values {
            assert!(
                !index
                    .global_enum_value_owners(value)
                    .iter()
                    .any(|o| o == "Cond"),
                "`UI::Cond` is namespaced, so `{value}` is not global through it"
            );
        }
    }

    /// B046 fence: a value a module-level enum SHARES with an enum the dump
    /// already carries keeps resolving, and the dump's enum is untouched.
    /// Guards against a fix that only works by shadowing.
    #[test]
    fn a_value_shared_with_a_dump_enum_still_resolves_after_b046() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // `Turbo` is carried by both module-level enums AND by enums the
        // dump has. (In-game a bare `Turbo` is actually ambiguous — "Found
        // multiple matching enum values" — which the LSP does not model.
        // That is a pre-existing false NEGATIVE, out of scope for B046.)
        for value in ["Turbo", "WallJump", "FreeWheeling"] {
            assert!(
                scope.has_global_ident(value),
                "`{value}` must keep resolving"
            );
        }
        assert!(
            index
                .lookup_source_enum("ESceneCharVisNotice")
                .is_some_and(|i| i.values.iter().any(|(v, _)| v == "Turbo")),
            "fixture guard: the module-level enum carries `Turbo`"
        );
    }

    /// B044 fence: the values of a NAMED nested enum are untouched by the
    /// narrowing, bare or qualified.
    #[test]
    fn named_nested_enum_values_still_resolve_bare() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let named = "Game::CGameCtnBlock::ECardinalDirections";
        let values: Vec<String> = index
            .lookup_source_enum(named)
            .expect("fixture guard: the named nested enum must exist")
            .values
            .iter()
            .map(|(v, _)| v.clone())
            .collect();
        assert!(!values.is_empty(), "fixture guard: it must carry values");
        for value in &values {
            assert!(
                scope.has_global_ident(value),
                "`{value}` is a named enum's value and must keep resolving"
            );
        }
    }

    /// B044 fence: a name that collides with an anonymous enum value but has
    /// a real declaration of its own keeps resolving through that
    /// declaration. This is the residual the ticket's 261 upper bound could
    /// not subtract — the checks that run BEFORE the enum scan (type,
    /// function, virtual property, workspace variable, workspace enum value)
    /// are what keep such a name a symbol.
    #[test]
    fn anonymous_enum_value_name_declared_elsewhere_still_resolves() {
        let index = typedb_fixture_index();
        let colliding = "HyperTnL";

        for kind in [
            SymbolKind::Variable {
                type_name: "int".into(),
            },
            SymbolKind::Function {
                return_type: "void".into(),
                params: Vec::new(),
                min_args: 0,
                imported: false,
            },
            SymbolKind::EnumValue {
                enum_name: "MyEnum".into(),
                value: Some(0),
            },
        ] {
            let mut ws = SymbolTable::new();
            let fid = ws.allocate_file_id();
            ws.set_file_symbols(fid, vec![make_symbol(colliding, kind)]);
            let scope = GlobalScope::new(&ws, Some(&index));
            assert!(
                scope.has_global_ident(colliding),
                "a workspace declaration of `{colliding}` must outrank the narrowing"
            );
        }

        // Same shape one level up: a virtual property's `get_` accessor.
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                &format!("get_{colliding}"),
                SymbolKind::Function {
                    return_type: "int".into(),
                    params: Vec::new(),
                    min_args: 0,
                    imported: false,
                },
            )],
        );
        let scope = GlobalScope::new(&ws, Some(&index));
        assert!(
            scope.has_global_ident(colliding),
            "a virtual property named `{colliding}` must outrank the narrowing"
        );
    }

    /// B044 fence: an anonymous member enum stays reachable through the
    /// member that declares it — the path the game does accept. Narrowing
    /// the bare-name scan must not touch `lookup_member_enum`, the property
    /// type it is keyed by, or the enum's stored values.
    #[test]
    fn member_path_to_an_anonymous_enum_value_still_resolves() {
        let index = typedb_fixture_index();

        for (class, member, value) in [
            ("Graphic::CGxLightBall", "AttenuationType", "HyperTnL"),
            (
                "Control::CControlColorChooser",
                "StyleType",
                "MinimalReadOnly",
            ),
            ("Control::CControlEntry", "Type", "TimeHhMmSsMil"),
        ] {
            let info = index
                .lookup_member_enum(class, member)
                .unwrap_or_else(|| panic!("`{class}::{member}` must still declare its enum"));
            assert!(info.anonymous, "`{class}::{member}` must stay anonymous");
            assert!(
                info.values.iter().any(|(v, _)| v == value),
                "`{value}` must stay reachable through `{class}::{member}`"
            );
            assert!(
                index
                    .lookup_type(class)
                    .is_some_and(|t| t.properties.iter().any(|p| p.name == member)),
                "`{member}` must stay a property of `{class}`"
            );
        }
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
                    imported: false,
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_function("doThing"));
        assert!(!scope.has_type("doThing"));
        assert!(scope.resolves("doThing"));
    }

    #[test]
    fn supplemental_symbols_resolve_without_gaining_namespace_authority() {
        let workspace = SymbolTable::new();
        let mut supplemental = SymbolTable::new();
        let fid = supplemental.allocate_file_id();
        supplemental.set_file_symbols(
            fid,
            vec![
                make_symbol("Generated", SymbolKind::Namespace),
                make_symbol(
                    "Generated::UseOnly",
                    SymbolKind::Function {
                        return_type: "void".into(),
                        params: vec![],
                        min_args: 0,
                        imported: false,
                    },
                ),
            ],
        );
        let scope = GlobalScope::with_supplemental(&workspace, &supplemental, None);

        assert!(scope.has_function("Generated::UseOnly"));
        assert!(!scope.workspace_closure_complete());
        assert!(!scope.workspace_namespace_declared("Generated"));
        assert!(!scope.workspace_declares_any("Generated::UseOnly"));
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
                    imported: false,
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

    /// GH #54: the conversion graph comes from the typedb's own
    /// `asBEHAVE_CONSTRUCT` behaviors — `mat4(const mat3&in)` and its
    /// symmetric partner exist, `mat4(const vec3&in)` does not.
    #[test]
    fn external_ctor_converts_follows_typedb_construct_behaviors() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));

        // Registered single-arg ctors, both directions.
        assert!(scope.external_ctor_converts("mat3", "mat4"));
        assert!(scope.external_ctor_converts("mat4", "mat3"));
        assert!(scope.external_ctor_converts("iso4", "mat4"));
        assert!(scope.external_ctor_converts("mat3", "iso3"));
        assert!(scope.external_ctor_converts("vec3", "quat"));
        // Namespaced value types resolve by short name too.
        assert!(scope.external_ctor_converts("vec4", "RGBAColor"));

        // No such constructor.
        assert!(!scope.external_ctor_converts("vec3", "mat4"));
        assert!(!scope.external_ctor_converts("mat3", "vec3"));
        assert!(!scope.external_ctor_converts("nat3", "int3"));

        // The copy constructor is NOT a conversion: an exact match must be
        // decided by type equality, never explained by this path.
        assert!(!scope.external_ctor_converts("mat4", "mat4"));
        assert!(!scope.external_ctor_converts("vec3", "vec3"));

        // Reference (engine) types have no construct behaviors at all.
        assert!(!scope.external_ctor_converts("CMwNod", "CGameCtnChallenge"));
        assert!(!scope.external_ctor_converts("mat3", "NotARealType"));
    }

    /// A workspace class shadowing an engine value type (GH #38) must not
    /// inherit the engine type's constructor conversions.
    #[test]
    fn external_ctor_converts_ignores_shadowed_types() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "mat3",
                SymbolKind::Class {
                    parents: vec![],
                    members: vec![],
                },
            )],
        );
        let scope = GlobalScope::new(&ws, Some(&idx));
        assert!(!scope.external_ctor_converts("mat3", "mat4"));
    }

    #[test]
    fn workspace_funcdef_counts_as_type() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "Callback",
                // Synthetic symbol exercising the empty-signature shape
                // the extractor can produce for a broken parse.
                SymbolKind::Funcdef {
                    return_type: String::new(),
                    params: vec![],
                    is_typedef: false,
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_type("Callback"));
        assert!(scope.lookup_funcdef("Callback").is_some());
    }

    /// B027: workspace signatures must carry parameter passing modes —
    /// `param_out` derived from the stored param text, and a funcdef
    /// signature complete enough to compare against (previously the
    /// workspace funcdef branch returned an empty skeleton).
    #[test]
    fn workspace_sigs_carry_param_passing_modes() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![
                make_symbol(
                    "Save",
                    SymbolKind::Function {
                        return_type: "void".into(),
                        params: vec![("v".into(), "int &out".into())],
                        min_args: 1,
                        imported: false,
                    },
                ),
                make_symbol(
                    "FDO",
                    SymbolKind::Funcdef {
                        return_type: "void".into(),
                        params: vec![("".into(), "int &out".into())],
                        is_typedef: false,
                    },
                ),
            ],
        );
        let scope = GlobalScope::new(&ws, None);
        let sigs = scope.lookup_function_overloads("Save");
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].param_types, vec!["int &out"]);
        assert_eq!(
            sigs[0].param_out,
            vec![true],
            "workspace sigs must derive &out from the stored param text"
        );
        let fd = scope.lookup_funcdef("FDO").expect("workspace funcdef");
        assert_eq!(fd.return_type, "void");
        assert_eq!(fd.param_types, vec!["int &out"]);
        assert_eq!(fd.param_modes, vec![ParamMode::Out]);
    }

    #[test]
    fn workspace_typedef_is_type_but_not_funcdef() {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![make_symbol(
                "MyInt",
                SymbolKind::Funcdef {
                    return_type: "int".into(),
                    params: vec![],
                    is_typedef: true,
                },
            )],
        );
        let scope = GlobalScope::new(&ws, None);
        assert!(scope.has_type("MyInt"));
        assert!(scope.lookup_funcdef("MyInt").is_none());
    }

    #[test]
    fn typedb_funcdef_counts_as_type() {
        use crate::typedb::index::TypeIndex;

        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        assert!(scope.has_type("UI::InputTextCallback"));
        let fd = scope
            .lookup_funcdef("UI::InputTextCallback")
            .expect("typedb funcdef");
        assert_eq!(fd.param_types.len(), 1);
        assert!(scope.lookup_funcdef("CoroutineFunc").is_some());
        assert!(scope.lookup_funcdef("Dev::ProcIntercept").is_some());
        assert!(scope.lookup_funcdef("UI::DefinitelyNotAFuncdef").is_none());
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
                        imported: false,
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
                        imported: false,
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
