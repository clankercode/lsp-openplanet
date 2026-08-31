//! A read-only merged view of globally visible names, combining the
//! workspace-level symbol table with the external Openplanet / Nadeo type
//! database.
//!
//! This is the lookup surface the type resolver consults when deciding
//! whether a referenced name is known. It does NOT own either data source —
//! callers build one on demand by borrowing both.

use super::access_position::AccessPosition;
use super::repr::{param_mode_from_text, ParamMode, TypeRepr};
use crate::symbols::scope::{Symbol, SymbolKind};
use crate::symbols::SymbolTable;
use crate::typedb::index::{ParamDirection, ParamInfo};
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
/// B152: the typedb's parameter type text WITH its `const` restored.
///
/// Core stores a parameter's `const` in the `typeflags` bits, not in the
/// `typedecl` text: `CoroutineFuncUserdataString` arrives as
/// `typedecl="string", typeflags=5` while its own `decl` reads
/// `void CoroutineFuncUserdataString(const string&in)`. Reading `typedecl`
/// alone therefore loses the qualifier, and the loss is silent as long as
/// the comparison peels `const` — which it did until B152.
///
/// It stops being silent the moment the comparison is const-EXACT. MEASURED,
/// not predicted: an instrumented build that kept the neutral answer and
/// printed a marker wherever an exact rule would disagree flagged **38 sites
/// across the plugin corpus** on this one funcdef, every one of them a
/// correctly-written `void H(const string &in)` that the game compiles. Had
/// the tightening landed without this, those 38 would have been new errors
/// on shipped code.
///
/// The `const` is placed as a LEADING qualifier. That is exact for every
/// funcdef the shipped database actually contains, because none of them has
/// a const HANDLE parameter — where a single flag bit could not say whether
/// it meant `const T@` or `T@ const`. That is not an assumption:
/// `every_core_funcdef_param_const_agrees_with_its_own_decl` reconstructs
/// each parameter from the bits, checks it against the database's own `decl`
/// string, and FAILS if a future dump introduces a const handle here, so the
/// ambiguity becomes a deliberate decision rather than a wrong guess.
fn typedb_param_type_text(p: &ParamInfo) -> String {
    match p.is_const {
        // `None` is Nadeo's "the dump does not say", and it is not a synonym
        // for false (B142). Funcdefs are Core-only today, so this arm is
        // unreachable; it is written as "add nothing" rather than "add
        // const" so an unknown can never manufacture a qualifier.
        Some(true) => format!("const {}", p.type_name),
        Some(false) | None => p.type_name.clone(),
    }
}

fn typedb_param_mode(p: &ParamInfo) -> ParamMode {
    let from_text = param_mode_from_text(&p.type_name);
    if from_text != ParamMode::Value {
        return from_text;
    }
    // B146: THE UNKNOWN POLICY, written down rather than defaulted. The Nadeo
    // half states no direction at all (its dump has no per-parameter key —
    // the eight member keys are `n,i,t,c,m,r,e,a`), so `None` arrives here
    // and must stay PERMISSIVE.
    //
    // MEASURED, 163-plugin fleet sweep with the binaries hashed and proven to
    // differ: substituting `Out` for `None` breaks 19 plugins with 57 fresh
    // false-positive errors and flips 9 exit statuses, all on shipped code
    // that compiles in the game — they arrive through the GH #47 rule below
    // ("a bool literal cannot bind an `&out` slot"), which reads `param_out`.
    // Substituting `Inout` changes nothing at all.
    //
    // So the corpus proves `Out` wrong and cannot separate `None` from
    // `Inout`. That is exactly why the field is `Option` and not a guess: a
    // future rule that starts consuming direction would find `Inout` a
    // landmine and `None` a stop sign. B130 nearly shipped a false positive
    // on every const Nadeo call by consuming a hardcoded `false` the same
    // way, which is the same story one field over.
    //
    // `Value` here is what the old all-`false` triple produced, so this is
    // behaviour-identical; the difference is that the fallback is now visible
    // and attributed instead of looking like data.
    match p.direction {
        Some(ParamDirection::Inout) => ParamMode::Inout,
        Some(ParamDirection::Out) => ParamMode::Out,
        Some(ParamDirection::In) => ParamMode::In,
        Some(ParamDirection::Value) => ParamMode::Value,
        None => ParamMode::Value,
    }
}

/// Shadowing key for one member declaration in an inheritance walk.
///
/// A declaration on a nearer ancestor OVERRIDES a same-signature one
/// further away — `class C : I { void Go() {} }` implementing
/// `interface I { void Go(); }` is one method, not a choice of two, and
/// the same holds for a plain `class B : A` override. Both inheritance
/// walkers hand the first declaration they meet the key and drop later
/// claimants.
///
/// The key is the parameter TYPE list, not the name: a parent overload the
/// child does not override (`A::Run(int)` beside `B::Run()`) is a genuine
/// second choice and must survive. Whitespace is stripped so `const
/// string &in` and `const string&in` — the same declaration written two
/// ways — still match.
fn override_key(param_types: &[String]) -> String {
    let mut key = String::from("(");
    for ty in param_types {
        key.extend(ty.chars().filter(|c| !c.is_whitespace()));
        key.push(',');
    }
    key.push(')');
    key
}

/// B079: proof that a spelling was accepted as an enum by the checker.
///
/// The point is that it cannot be constructed any other way — the field is
/// private, so only [`GlobalScope::accepts_enum`] can mint one. A caller
/// holding it has necessarily run the acceptance test, so the B070 near-miss —
/// calling the canonicalizer and reading its answer AS acceptance — stops
/// being a discipline question and becomes a compile error.
///
/// A witness rather than a visibility keyword because this crate has `typedb`,
/// `typecheck` and `server` as SIBLING top-level modules: `pub(crate)`
/// restricts nothing between them, so types are the only mechanism that binds
/// across them.
#[derive(Debug, Clone, Copy)]
pub struct Accepted(());

/// B081: a type name resolved SHADOW-AWARE — proof that the workspace does
/// not declare a type of this name, so it really denotes the engine type the
/// typedb holds under it.
///
/// The field is private, so only [`GlobalScope::external_type_name`] can mint
/// one, and that is the shadow-aware resolver. A caller holding this has
/// necessarily asked whether the workspace shadows the name.
///
/// # Which hole this closes, and which one the NAME closes
///
/// This closes SHADOW-BLINDNESS. `is_external_derived_from` used to take two
/// `&str`s and resolve them against the type index whether or not the
/// workspace shadowed them, which made it indistinguishable at the call site
/// from the shadow-aware `is_external_type`. B063 picked the shadow-blind one
/// while actively reasoning about B079's duplicate-predicate hazard, and a
/// plugin declaring `class CSmPlayer {}` was offered the engine class's
/// injected methods. Nothing was missing; the right predicate existed, one
/// identifier away.
///
/// It does NOT close argument ORDER — the rename to
/// [`GlobalScope::external_type_derives_from`] does that, by making a
/// transposed call read wrong.
///
/// # Why only the DESCENDANT position is newtyped
///
/// Deliberate, not an oversight. The ancestor is an engine IDENTITY, not a
/// name the plugin author wrote: six of the eight original call sites pass a
/// hardcoded `"CMwNod"` or a string from `INJECTED_METHODS`. Making the
/// ancestor shadow-aware would be actively wrong — a plugin declaring
/// `class CMwNod {}` must not switch off `MwAddRef` for genuine engine types.
///
/// The asymmetry pays a second dividend the design ruling did not expect. It
/// observed that one newtype applied to BOTH parameters leaves them mutually
/// substitutable and so cannot close transposition. Applied to ONE position it
/// does: the two arguments now have different types, so a transposed call is a
/// compile error as well as a misreading.
///
/// The POSITIVE CONTROL for the two `compile_fail` blocks below, and it is
/// not decoration: a `compile_fail` doctest that fails for the wrong reason —
/// a mistyped import path, a renamed method — passes just as green as one that
/// fails for the intended reason. This block differs from each of them by
/// exactly the offending line, so if the path or the signature were wrong,
/// this one goes red and says so.
///
/// ```
/// use openplanet_lsp::typecheck::global_scope::ExternalTypeName;
/// fn wants(n: ExternalTypeName<'_>) -> &str { n.as_str() }
/// let _ = wants;
/// ```
///
/// ```compile_fail
/// // A bare `&str` cannot stand in for a resolved name.
/// use openplanet_lsp::typecheck::global_scope::ExternalTypeName;
/// fn wants(n: ExternalTypeName<'_>) -> &str { n.as_str() }
/// wants("CSmPlayer");
/// ```
///
/// ```compile_fail
/// // Nor can one be forged: the field is private, so the shadow-aware
/// // resolver is the only source.
/// use openplanet_lsp::typecheck::global_scope::ExternalTypeName;
/// fn wants(n: ExternalTypeName<'_>) -> &str { n.as_str() }
/// wants(ExternalTypeName("CSmPlayer"));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ExternalTypeName<'a>(&'a str);

impl<'a> ExternalTypeName<'a> {
    /// The underlying spelling. Read-only on purpose: handing the `&str` back
    /// is fine, since what the witness proves is about how it was OBTAINED.
    pub fn as_str(self) -> &'a str {
        self.0
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
        // B107: delegated, so the checker and completion cannot disagree
        // about what counts as a namespace.
        self.workspace.namespace_is_declared(qualified)
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
    /// B107: a PROJECTION of [`SymbolTable::namespace_member_symbols`], which
    /// is now the single walk. This used to do the walk itself and throw the
    /// symbols away, which is why completion had no workspace producer to
    /// share — the kinds were discarded one level too early.
    pub fn workspace_namespace_member_names(&self, ns: &str) -> Vec<String> {
        self.workspace
            .namespace_member_symbols(ns)
            .into_iter()
            .filter_map(|sym| sym.name.rsplit("::").next())
            .map(str::to_string)
            .collect()
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
            // B056: NARROWER than the old exact-key test, and narrower on
            // purpose. The Nadeo dump is keyed `Module::Rest` and the module
            // is this crate's invention, so the key was accepted verbatim and
            // `Game::CGameCtnApp` resolved — the game answers `Namespace
            // 'Game' doesn't exist.`
            //
            // Deliberately NOT widened to source names here. A bare Nadeo
            // class name is a real spelling, but it arrives through
            // `resolve_unqualified`'s short-name rescue, which already
            // handled it. Accepting source names in `has_type` would also
            // admit `MwFoundations::Bool` as bare `Bool`, and the b053 corpus
            // shows the game has no such type: `No matching symbol 'Bool'`,
            // where a type it really has answers `Expression 'X' is a data
            // type` (as `RGBAColor` does).
            // Kind matters: `source_names` spans types, enums and funcdefs,
            // and `Bool` is a CLASS key (`MwFoundations::Bool`). Asking only
            // "is this a source name" here would let an enum answer a type
            // question and vice versa.
            if ext.spelling_is_source_name(qualified)
                && (ext.lookup_type(qualified).is_some() || ext.lookup_funcdef(qualified).is_some())
            {
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
            param_types: info.params.iter().map(typedb_param_type_text).collect(),
            param_modes: info.params.iter().map(typedb_param_mode).collect(),
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
            // B056: the SOURCE name, which for a nested Nadeo enum is the key
            // minus its module (`CGameCtnBlock::ECardinalDirections`) and for
            // a Core enum is the key in full (`UI::Plot::Marker`).
            //
            // This replaces two lookups. The first accepted the index KEY
            // verbatim, so `Game::CGameCtnBlock::ECardinalDirections`
            // resolved — `Namespace 'Game::CGameCtnBlock' doesn't exist.` in
            // the game. The second (B007) accepted any trailing PATH, so
            // `Plot::Marker` resolved against `UI::Plot::Marker` —
            // `Namespace 'Plot' doesn't exist.` The rule is asymmetric by
            // origin and a single trailing-path test cannot express it:
            // Nadeo must lose exactly its module, Core must lose nothing.
            if ext
                .source_name_key(qualified)
                .is_some_and(|key| ext.key_is_source_enum(key))
            {
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
                // B056: `global_enum_key`, not `lookup_source_enum(tail)`.
                // The latter only answered when the KEY was bare, which is
                // true of a header enum and a namespace-less Core one but NOT
                // of a Nadeo module-level enum — `EPlugSurfaceGameplayId` is
                // keyed `Meta::EPlugSurfaceGameplayId`. Those used to resolve
                // by accident, because the loader's module name `Meta`
                // happens to coincide with a real Core namespace and the
                // exact-key test matched the spelling. Removing the exact-key
                // test exposed that, and the walk has to do the work
                // honestly: paired probe `Meta::ZZZNoSuchEnumName` answers
                // `Identifier 'ZZZNoSuchEnumName' is not a data type in
                // namespace 'Meta' or parent`, so `Meta` IS a scope and
                // `Meta::EPlugSurfaceGameplayId` is a genuine engine accept.
                if self.scope_exists(head) && ext.global_enum_key(tail).is_some() {
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
    fn canonical_enum_key_unchecked(&self, qualified: &str) -> Option<String> {
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

        // `tail` is a complete SOURCE NAME here, so this can only select a
        // global enum — and it must be asked the same question `has_enum`'s
        // walk asks. `lookup_source_enum` answers only when the key is
        // already bare, so it silently missed every global enum the loader
        // filed under a synthetic module (`Game::EDummyCollectionIdent`),
        // leaving `has_enum` accepting a spelling this hands back unchanged.
        // `global_enum_key` is that same index, and it returns the KEY.
        // The global-enum index excludes anonymous synthetic keys, so
        // acceptance is still never inferred from a matching tail alone.
        ext.global_enum_key(tail).map(|key| key.to_string())
    }

    /// Acceptance test for an enum spelling, returning proof on success.
    ///
    /// Same answer as [`Self::has_enum`]; the difference is that the `Some`
    /// carries a token [`Accepted`] which [`Self::canonical_enum_key`]
    /// demands.
    pub fn accepts_enum(&self, qualified: &str) -> Option<Accepted> {
        self.has_enum(qualified).then_some(Accepted(()))
    }

    /// The canonical index key for an ACCEPTED enum spelling.
    ///
    /// Requires an [`Accepted`] witness because this is a CANONICALIZER, not
    /// an acceptance test, and it is deliberately wider than acceptance: it
    /// still carries the trailing-suffix match `has_enum` dropped for B007.
    /// Called on arbitrary text it answers for spellings the game rejects —
    /// `Plot::Axis` matches the real key `UI::Plot::Axis`, which the game
    /// refuses with `Namespace 'Plot' doesn't exist.` That is exactly what
    /// hover did before B070, for 24 corpus spellings.
    ///
    /// Note `None` here means "already canonical", NOT "not found": a caller
    /// reading it as absence loses precisely the inputs that needed no work.
    /// Ten `Meta::*` spellings regressed that way during B079 Move 1.
    pub fn canonical_enum_key(&self, qualified: &str, _proof: Accepted) -> Option<String> {
        self.canonical_enum_key_unchecked(qualified)
    }

    /// B071: the declared member names of the enum spelled `qualified`, for
    /// a spelling this scope ACCEPTS.
    ///
    /// Deliberately separate from [`Self::has_enum`], and the split carries
    /// the whole safety property. Acceptance answers *does the game resolve
    /// this spelling*; this answers *and then which members does it have*.
    /// The two are not the same question, and a caller that conflated them
    /// would report every member of an enum it can name but cannot enumerate
    /// as misspelt.
    ///
    /// So `None` means **the member list is not knowable**, never "this enum
    /// has no members" — an anonymous member enum (B016) is the live case:
    /// `lookup_source_enum` refuses its synthetic key, so acceptance already
    /// says no and nothing downstream may judge its members. Callers must
    /// treat `None` as *stay silent*.
    pub fn enum_member_names(&self, qualified: &str) -> Option<Vec<String>> {
        let proof = self.accepts_enum(qualified)?;
        // `canonical_enum_key` returns `None` for an already-canonical
        // spelling, which is the common case here — reading that as "not
        // found" would drop every workspace enum.
        let key = self
            .canonical_enum_key(qualified, proof)
            .unwrap_or_else(|| qualified.to_string());
        // Workspace first: a plugin's own declaration outranks an engine
        // enum of the same name, exactly as it does for `has_enum`.
        for symbol in self.lookup_workspace(&key) {
            if let SymbolKind::Enum { values } = &symbol.kind {
                return Some(values.iter().map(|(name, _)| name.clone()).collect());
            }
        }
        let info = self.external?.lookup_source_enum(&key)?;
        Some(info.values.iter().map(|(name, _)| name.clone()).collect())
    }

    /// True when `qualified` is a type's own key, exactly as stored — no
    /// module-stripped spelling (B056). Callers that need a canonical name,
    /// rather than "does this resolve", ask this.
    fn is_canonical_type_key(&self, qualified: &str) -> bool {
        if self.lookup_workspace(qualified).iter().any(|s| {
            matches!(
                s.kind,
                SymbolKind::Class { .. }
                    | SymbolKind::Interface { .. }
                    | SymbolKind::Funcdef { .. }
            )
        }) {
            return true;
        }
        self.external.is_some_and(|ext| {
            ext.lookup_type(qualified).is_some() || ext.lookup_funcdef(qualified).is_some()
        })
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
    /// game accepts it — the ancestor walk reaches global from any real scope.
    ///
    /// PROVENANCE, corrected. An earlier version of this comment cited a probe
    /// "on a plugin declaring `namespace Foo { namespace Bar { … } }`", which
    /// reads as the probe fixture's own declaration and was taken that way.
    /// It was not: `scripts/probe_enum_corpus.py` emits no `namespace` in
    /// either template, and the b052 re-probe flipped every `Foo::*` row to
    /// `Namespace 'Foo' doesn't exist.` once a plugin declaring it was no
    /// longer installed. So those rows measured a FOREIGN plugin's namespace,
    /// an environment fact, not this rule.
    ///
    /// What supports this rule is the Core-namespace evidence, which did not
    /// flip: `Audio::VirtualKey`, `Crypto::VirtualKey`, `Dev::VirtualKey`,
    /// `Display::VirtualKey`, `Fids::VirtualKey`. A namespace the workspace
    /// itself declares is a real scope in its own compilation unit and reaches
    /// global by the same walk. Do not re-cite the `Foo::*` rows for it: the
    /// b052 provenance sidecar records `Foo` as HOST — a foreign plugin's —
    /// precisely so those rows must NOT resolve when nothing declares them.
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
        // External index first, by SOURCE name (B056). `find_by_short_name`
        // matched any key whose TAIL was `short`, which rescued names the
        // game does not have: bare `EMenuOrder` against
        // `Game::CGameMenu::EMenuOrder`, bare `Align` against `nvg::Align`,
        // bare `Cond` against `UI::Cond`. The game answers `Identifier 'X' is
        // not a data type in global namespace` for all three.
        //
        // The class rescue this exists for is UNAFFECTED, because a Nadeo
        // class's source name IS its bare name — `CGameCtnEditorFree` is the
        // source spelling of the key `Game::CGameCtnEditorFree`, and the game
        // takes it. What goes away is only the tail match against a name the
        // game requires a qualifier for.
        if let Some(ext) = self.external {
            if let Some(key) = ext.source_name_key(short) {
                return Some(key.to_string());
            }
            // Not a source name. The short-name rescue still applies to
            // CLASSES and funcdefs — that is what this method exists for, and
            // the game takes `CGameCtnEditorFree` bare — but NOT to enums: a
            // nested or namespaced enum needs its qualifier, and the game
            // answers `Identifier 'EMenuOrder' is not a data type in global
            // namespace`. Same for `Align` (`nvg::Align`) and `Cond`
            // (`UI::Cond`), 436 names in all.
            for candidate in ext.find_by_short_name(short) {
                if !ext.key_is_source_enum(candidate) {
                    return Some(candidate.clone());
                }
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

        // B056: the SOURCE name, not any trailing path. A trailing path
        // accepted `Plot::Marker` against `UI::Plot::Marker`, and the game
        // answers `Namespace 'Plot' doesn't exist.` — a Core namespace has to
        // be written in full. The case this method exists for is unchanged,
        // because dropping the Nadeo module IS the source name:
        // `CGameEditorPluginMap::ECardinalDirections` still finds
        // `Game::CGameEditorPluginMap::ECardinalDirections`.
        if let Some(ext) = self.external {
            if let Some(key) = ext.source_name_key(qualified) {
                return Some(key.to_string());
            }
            // As in `resolve_unqualified`: the trailing-path rescue survives
            // for classes and funcdefs, and goes away for enums, where it
            // accepted `Plot::Marker` against `UI::Plot::Marker` and the game
            // answers `Namespace 'Plot' doesn't exist.`
            for candidate in ext.find_by_short_name(short) {
                if candidate.ends_with(&needle) && !ext.key_is_source_enum(candidate) {
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
    /// The external enum-value fallback covers *globally visible* enums only
    /// (B057, generalising B044): a nested, namespaced or anonymous engine
    /// enum does not put its values in the global value scope, so a bare
    /// name that is only one of those is not a symbol — the game answers
    /// `No matching symbol`.
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
        // External index: is `name` a bare value of a GLOBALLY VISIBLE engine
        // enum? Those are the only enums whose values AngelScript puts in the
        // global value scope, so this is the last way a bare identifier can
        // be a symbol.
        //
        // B057: `global_enum_value_owners`, NOT a scan over
        // `source_enums_iter`. Being *named* is what lets source write the
        // enum's own name; it is not what makes the enum's values reachable
        // without a qualifier. An enum is globally visible iff its index key
        // is depth <= 1 with the loader's synthetic module prefix stripped —
        // see `TypeIndex::enum_is_global`, which owns that rule. A nested
        // enum (`Game::CGameCtnBlock::ECardinalDirections`) and a namespaced
        // Core enum (`UI::Cond`) never are, and neither contributes a bare
        // name.
        //
        // Game ground truth, and it is exhaustive rather than sampled: all
        // 2989 bare value names of the shipped DB's named enums were
        // submitted to the live compiler (RemoteBuild, Trackmania +
        // Openplanet 1.29.5, 2026-08-30) and the verdicts are committed at
        // `docs/probes/b053-bare-enum-values.tsv`. The scan this replaced
        // accepted 2373 names the game answers `No matching symbol` for —
        // `North`, `Appearing`, `HyperTnL`'s named siblings, and so on.
        // `every_probed_bare_value_matches_the_games_verdict` checks every
        // row of that corpus against this call.
        //
        // Two or more owners is NOT a rejection here: the name is a symbol,
        // and the compiler's refusal to choose between them is B053's
        // separate `AmbiguousEnumValue` diagnostic, raised by the checker
        // before it ever reaches this lookup.
        //
        // B044 is subsumed, not undone. An anonymous enum has no name for
        // `enum_is_global` to place, so it is not globally visible and its
        // values still contribute nothing — the 258 anonymous-only names of
        // `tests/fixtures/issue-repros/b044-anon-enum-value-names` stay
        // rejected, now as a corollary of a wider rule instead of a rule of
        // their own.
        //
        // What keeps this from over-rejecting is unchanged: every other way
        // a name can be global — type, function, virtual property, workspace
        // variable or enum value — was already tried above, so a name that
        // merely collides with a non-global enum value keeps resolving
        // through its real declaration.
        //
        // Member access is unaffected: `lookup_member_enum` reaches a nested
        // or anonymous enum by its declaring context, never through here.
        if let Some(ext) = self.external {
            if !ext.global_enum_value_owners(name).is_empty() {
                return true;
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

    /// The chain walk itself: does `descendant`'s external parent chain reach
    /// `ancestor`? Walks with a cycle guard, same as `ext_lookup_member`.
    ///
    /// B081: SHADOW-BLIND BY CONSTRUCTION, and PRIVATE for that reason. It
    /// resolves both names against the type index whether or not the workspace
    /// declares a type of that name, which is right for an engine identity in
    /// the ancestor position and wrong for a name the plugin author wrote.
    /// Public callers go through [`Self::external_type_derives_from`] or
    /// [`Self::external_type_is`], both of which demand an
    /// [`ExternalTypeName`] for the descendant.
    ///
    /// This used to be `pub fn is_external_derived_from(type_name, base)`.
    /// Two `&str`s said nothing about which was the ancestor, and nothing
    /// about whether either had been shadow-resolved; see [`ExternalTypeName`]
    /// for what each half of the fix closes.
    fn external_chain_contains(&self, descendant: &str, ancestor: &str) -> bool {
        let Some(ext) = self.external else {
            return false;
        };
        let Some(ancestor_resolved) = Self::resolve_external_type_name(ext, ancestor) else {
            return false;
        };
        let mut current: Option<String> = Self::resolve_external_type_name(ext, descendant);
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            if name == ancestor_resolved {
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

    /// B130: the trailing-`const` qualifier of an ENGINE method, from the
    /// typedb. `None` means "we do not know", and that covers four distinct
    /// situations which all want the same answer from a caller — decline:
    ///
    /// * no such method is known,
    /// * the type is not an external type at all,
    /// * a workspace class shadows the name (same provenance discipline
    ///   `workspace_own_method_is_const` applies to the other half of the
    ///   merged view),
    /// * or the DATABASE itself does not say — every Nadeo method (B137).
    ///
    /// The data reaching here is B125's: `CoreMethod::declared_const` reads
    /// the trailing ` const` off the `decl` string, which is the only place
    /// the dumps carry it. 301 of 1002 Core class methods have one. The Nadeo
    /// dump carries no signature text at all, measured, so its methods arrive
    /// as `None`.
    ///
    /// B137 note on how that exclusion is made: this used to check the type's
    /// `TypeSource` and refuse anything not Core. That was a PROXY — right
    /// answer, wrong reason — and it would have gone quietly wrong the moment
    /// a Core-sourced method arrived without constness, or a Nadeo one with
    /// it. `MethodInfo::is_const` is now `Option<bool>`, so the datum states
    /// its own absence and this reads the fact instead of guessing from
    /// provenance.
    ///
    /// `Some(true)` when ANY overload of the name is const, matching
    /// `workspace_own_method_is_const` — on a const receiver the engine
    /// selects a const overload if one exists, so one const declaration makes
    /// the call legal. The `get_V`/`set_V` accessor spellings are folded in
    /// for the same reason they are there.
    ///
    /// Parents ARE walked, unlike the workspace own-class helper. The engine
    /// hierarchy is the typedb's own and `ext_lookup_method_return` already
    /// walks it to find the return type, so stopping short here would answer
    /// `None` for a method this very module can otherwise resolve.
    pub fn external_method_is_const(&self, type_name: &str, method: &str) -> Option<bool> {
        if self.workspace_type_shadows(type_name) {
            return None;
        }
        let ext = self.external?;
        let getter = format!("get_{method}");
        let setter = format!("set_{method}");
        let mut current: Option<String> = Self::resolve_external_type_name(ext, type_name);
        let mut hops = 0usize;
        while let Some(name) = current.take() {
            hops += 1;
            if hops > 32 {
                break;
            }
            let info = ext.lookup_type(&name)?;
            // B137: three-valued, aggregated over the overload set.
            //
            //   any `Some(true)`  -> `Some(true)`  a const overload exists, so
            //                       the engine selects it and the call is legal
            //   else any `None`   -> `None`        at least one overload's
            //                       constness is unknown, so a const one cannot
            //                       be ruled out; decline rather than guess
            //   else              -> `Some(false)` every overload is known
            //                       non-const
            //
            // The `None` arm is what keeps every Nadeo method out. It reads the
            // DATA's own "I don't know" instead of inferring it from the type's
            // source, which is what this did before — a provenance proxy that
            // happened to line up, rather than the fact itself.
            let mut found = false;
            let mut any_const = false;
            let mut any_unknown = false;
            for m in &info.methods {
                if m.name == method || m.name == getter || m.name == setter {
                    found = true;
                    match m.is_const {
                        Some(true) => any_const = true,
                        Some(false) => {}
                        None => any_unknown = true,
                    }
                }
            }
            if found {
                if any_const {
                    return Some(true);
                }
                if any_unknown {
                    return None;
                }
                return Some(false);
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

    /// B081: the shadow-aware resolver, and the ONLY way to mint an
    /// [`ExternalTypeName`].
    ///
    /// Same question as [`Self::is_external_type`] — the `Some` just carries
    /// the proof that it was asked. Where a caller previously wrote
    /// `if !scope.is_external_type(&t) { return; }` and then passed the bare
    /// `&t` onward, the guard and the use are now one expression and cannot
    /// drift apart. That drift is the whole ticket: both `injected_method`
    /// callers in `checker.rs` WERE correct, but only because an
    /// `is_external_type` guard fifteen lines earlier, written for a different
    /// purpose ("only emit when the type is a known external type"), happened
    /// to dominate them. Correct by coincidence of control flow is not the
    /// same as correct by construction, and it is not visible to a reader at
    /// the call site.
    pub fn external_type_name<'n>(&self, name: &'n str) -> Option<ExternalTypeName<'n>> {
        self.is_external_type(name)
            .then_some(ExternalTypeName(name))
    }

    /// True when `descendant` is, or derives from, the engine type spelled
    /// `ancestor`.
    ///
    /// Named so a transposed call reads wrong, which is the half of B081 the
    /// witness type cannot do: `is_external_derived_from(a, b)` gave a reader
    /// nothing to check the order against.
    ///
    /// `ancestor` stays a `&str` deliberately — it is an engine identity
    /// (`"CMwNod"`, a row from `INJECTED_METHODS`), not a name the plugin
    /// author wrote, and shadow-resolving it would let a plugin's
    /// `class CMwNod {}` switch off a rule about genuine engine types. See
    /// [`ExternalTypeName`] for why that asymmetry also closes transposition.
    pub fn external_type_derives_from(
        &self,
        descendant: ExternalTypeName<'_>,
        ancestor: &str,
    ) -> bool {
        self.external_chain_contains(descendant.as_str(), ancestor)
    }

    /// True when `resolved` and the engine type spelled `engine_name` are the
    /// SAME external type, tolerating short vs. namespace-qualified spellings
    /// on either side.
    ///
    /// The typedb parent chain is a tree, so mutual derivation can only mean
    /// the two names resolve to one type. It lives here rather than in
    /// `checker.rs` because that is where the name resolver is: the checker's
    /// own `same_external_type` helper existed only to work around this being
    /// private, and `builtins.rs` open-coded the same pair of calls. One
    /// concept, three spellings, which is the B079 shape that B081 is the
    /// fifth instance of — so B081 removes two of them rather than adding a
    /// fourth.
    pub fn external_type_is(&self, resolved: ExternalTypeName<'_>, engine_name: &str) -> bool {
        self.external_chain_contains(resolved.as_str(), engine_name)
            && self.external_chain_contains(engine_name, resolved.as_str())
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
        // Only an EXACT key is already canonical. `has_type` and `has_enum`
        // are both deliberately wider than that — since B056 they answer for
        // the SOURCE spelling, which for a Nadeo entry is the key minus its
        // module (`CGameCtnApp`, not `Game::CGameCtnApp`), and `has_enum`
        // also answers for a global enum behind a real scope's qualifier
        // (B052). None of those is the canonical key, so asking `has_type`
        // here would canonicalize `CGameCtnApp` to itself and leave two
        // spellings of one type unequal.
        if self.is_canonical_type_key(name) || self.is_canonical_enum_key(name) {
            return name.to_string();
        }
        // Deliberately the UNCHECKED variant. This is a canonicalizer calling
        // a canonicalizer: its contract is "leave the input unchanged when
        // nothing matches", so it does not need — and must not require —
        // acceptance. Gating it on `has_enum` would change type-equality for
        // the spellings where the two disagree (`Plot::Axis` matching the real
        // key `UI::Plot::Axis`), which is a behaviour change this ticket has
        // no business making.
        if let Some(key) = self.canonical_enum_key_unchecked(name) {
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
                    param_out: m
                        .params
                        .iter()
                        .map(|p| p.direction.is_some_and(ParamDirection::binds_by_reference))
                        .collect(),
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
                        param_out: f
                            .params
                            .iter()
                            .map(|p| p.direction.is_some_and(ParamDirection::binds_by_reference))
                            .collect(),
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
        for name in &workspace_function_property_candidates(qualified) {
            out.extend(self.lookup_function_overloads_exact(name));
        }
        out
    }

    /// B060: [`Self::lookup_function_overloads`] WITHOUT the virtual-property
    /// alternates — only functions literally named `qualified`.
    ///
    /// The `X` -> `get_X` / `set_X` widening is what makes a property READ
    /// resolve to a value; a caller asking "is this expression a function
    /// reference?" must not get that widening, or `Text::Format("%d", Off)`
    /// classifies the `uint` property read `Off` as function-shaped and
    /// every non-funcdef overload is rejected. `get_X` is a function when
    /// WRITTEN `get_X`; `X` is a value.
    pub fn lookup_function_overloads_exact(&self, qualified: &str) -> Vec<OverloadSig> {
        let mut out = Vec::new();
        for s in self.lookup_workspace(qualified) {
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

    /// Look up the declared base types of a workspace class or interface by
    /// fully qualified name. Returns an empty vec if no workspace type with
    /// that name exists, or it has no bases. Only consults
    /// the workspace symbol table — external (typedb) types use their
    /// own parent walker via `ext_lookup_member`.
    ///
    /// Interfaces are included (B054): `interface B : A` is real
    /// AngelScript, and an inheritance walk that stops at an interface
    /// cannot find a member declared on its base.
    pub fn workspace_class_parents(&self, class_name: &str) -> Vec<String> {
        for s in self.lookup_workspace(class_name) {
            match &s.kind {
                SymbolKind::Class { parents, .. } | SymbolKind::Interface { parents } => {
                    return parents.clone()
                }
                _ => {}
            }
        }
        Vec::new()
    }

    /// True when the checker knows `qualified` as a TYPE at all — a
    /// workspace class / interface / enum / funcdef, or a typedb type, enum
    /// or funcdef.
    ///
    /// Deliberately spans all three of [`Self::has_type`],
    /// [`Self::has_enum`] and [`Self::workspace_declares_type`], because
    /// each omits something the others carry: `has_type` excludes enums
    /// entirely, `has_enum` carries the asymmetric Nadeo/Core source-name
    /// rules that make a Core `UI::Plot::Marker` resolvable, and
    /// `workspace_declares_type` carries workspace enums. Asking only one of
    /// them is how a Core enum came to be treated as an unknown shape.
    pub fn names_a_known_type(&self, qualified: &str) -> bool {
        self.has_type(qualified)
            || self.has_enum(qualified)
            || self.workspace_declares_type(qualified)
    }

    /// True when `qualified` names a type the checker knows AND that type is
    /// not a funcdef — a value of it can never be a function reference.
    /// `false` for a name the checker does not know, which keeps GH #47's
    /// guard for shapes it cannot see.
    pub fn names_a_known_non_callable_type(&self, qualified: &str) -> bool {
        if !self.names_a_known_type(qualified) {
            return false;
        }
        // `lookup_funcdef` alone is not enough, and the reason is the very
        // shadowing this ticket already fixed once: it falls through a
        // workspace CLASS of that name to the Core funcdef, so a plugin's
        // `class CoroutineFunc` read as a property looked "possibly
        // callable" and its rejection was lost. The workspace declaration
        // wins here exactly as it does everywhere else.
        self.workspace_shadows_funcdef(qualified) || self.lookup_funcdef(qualified).is_none()
    }

    /// GH #38 shadowing, funcdef edition: true when the workspace declares
    /// `name` as something that is NOT a real funcdef.
    ///
    /// [`Self::lookup_funcdef`] walks workspace symbols but only *returns*
    /// on a real funcdef, then falls through to the typedb — so a plugin
    /// class, interface, enum or `typedef` named after a Core funcdef
    /// (`CoroutineFunc`, `CoroutineFuncUserdataBool`, ...) silently resolves
    /// to the ENGINE funcdef. Callers deciding "is this value a callable?"
    /// must ask this first: the plugin's own declaration wins, and a
    /// `typedef` counts as shadowing even though it reuses
    /// `SymbolKind::Funcdef`, because it is a type alias rather than a
    /// callable signature.
    /// The tail half matters as much as the exact one: the resolver already
    /// finds an unqualified name through [`Self::lookup_workspace_tail`]
    /// (see `resolve_unqualified`), so `namespace Dep { class CoroutineFunc }`
    /// declared in another file shadows a bare `CoroutineFunc` too. Tail hits
    /// that are really CLASS MEMBERS are skipped, exactly as
    /// [`Self::has_global_ident`] skips them (GH #30).
    pub fn workspace_shadows_funcdef(&self, name: &str) -> bool {
        self.workspace_shadowing_type_name(name).is_some()
    }

    /// The same question as [`Self::workspace_shadows_funcdef`], answered
    /// with the WINNER's fully qualified name instead of a bool — the
    /// `Nz::CoroutineFunc` behind a bare `CoroutineFunc`, or `name` itself
    /// when the declaration is exact.
    ///
    /// A caller that has decided the workspace wins then has to resolve the
    /// name, and the shadowing declaration may not be spelled the way the
    /// reference is (B073). Deriving the winner separately from the predicate
    /// is how the two would drift, so the predicate is expressed in terms of
    /// this rather than beside it.
    pub fn workspace_shadowing_type_name(&self, name: &str) -> Option<String> {
        self.first_workspace_symbol(name, |kind| {
            declares_type(kind)
                && !matches!(
                    kind,
                    SymbolKind::Funcdef {
                        is_typedef: false,
                        ..
                    }
                )
        })
    }

    /// True when the workspace declares `name` as a TYPE, by exact name or
    /// through the tail table. The candidate test behind
    /// [`Self::workspace_shadows_funcdef`], split out because resolving a
    /// value's written type name needs the same "is this a type at all?"
    /// question without the funcdef half.
    pub fn workspace_declares_type(&self, name: &str) -> bool {
        self.any_workspace_symbol(name, declares_type)
    }

    /// Exact-then-tail symbol scan for `name`, skipping tail hits that are
    /// really CLASS MEMBERS exactly as [`Self::has_global_ident`] skips them
    /// (GH #30).
    fn any_workspace_symbol(&self, name: &str, pred: impl Fn(&SymbolKind) -> bool) -> bool {
        self.first_workspace_symbol(name, pred).is_some()
    }

    /// [`Self::any_workspace_symbol`] with the matching symbol's qualified
    /// name, which is `name` itself for an exact hit and the declaration's
    /// own spelling for a tail hit.
    fn first_workspace_symbol(
        &self,
        name: &str,
        pred: impl Fn(&SymbolKind) -> bool,
    ) -> Option<String> {
        if self.lookup_workspace(name).iter().any(|s| pred(&s.kind)) {
            return Some(name.to_string());
        }
        let suffix = format!("::{}", name);
        self.lookup_workspace_tail(name)
            .iter()
            .filter(|s| !self.tail_prefix_is_class_member(&s.name, &suffix))
            .find(|s| pred(&s.kind))
            .map(|s| s.name.clone())
    }

    /// True when `name` names a workspace interface symbol. The interface
    /// counterpart of [`Self::is_workspace_class`]; kept separate so
    /// callers that mean "a class, specifically" keep meaning that.
    pub fn is_workspace_interface(&self, name: &str) -> bool {
        self.lookup_workspace(name)
            .iter()
            .any(|s| s.name == name && matches!(s.kind, SymbolKind::Interface { .. }))
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
        self.method_overloads_walk(class_name, method, Self::lookup_function_overloads)
    }

    /// B060: [`Self::lookup_method_overloads_with_inheritance`] WITHOUT the
    /// virtual-property alternates — `C::Amt` no longer resolves through
    /// `C::get_Amt`, so a property read on a receiver (`c.Amt`) is a value
    /// rather than a method reference. See
    /// [`Self::lookup_function_overloads_exact`].
    pub fn lookup_method_overloads_exact_with_inheritance(
        &self,
        class_name: &str,
        method: &str,
    ) -> Vec<OverloadSig> {
        self.method_overloads_walk(class_name, method, Self::lookup_function_overloads_exact)
    }

    /// Shared inheritance walk behind the two method-overload lookups: the
    /// class itself first, then parents breadth-first, normalized in the
    /// child's namespace context and cycle-guarded. `resolve` decides
    /// whether `Class::method` widens to its `get_`/`set_` accessors.
    fn method_overloads_walk(
        &self,
        class_name: &str,
        method: &str,
        resolve: fn(&Self, &str) -> Vec<OverloadSig>,
    ) -> Vec<OverloadSig> {
        let mut out: Vec<OverloadSig> = Vec::new();
        let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(class_name)]);
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let qualified = format!("{}::{}", name, method);
            for sig in resolve(self, &qualified) {
                if claimed.insert(override_key(&sig.param_types)) {
                    out.push(sig);
                }
            }
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
        let mut out: Vec<&Symbol> = Vec::new();
        let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(class_name)]);
        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let qualified = format!("{}::{}", name, member);
            for sym in self.lookup_workspace(&qualified) {
                let key = match &sym.kind {
                    SymbolKind::Function { params, .. } => {
                        let types: Vec<String> = params.iter().map(|(_, ty)| ty.clone()).collect();
                        override_key(&types)
                    }
                    // A field hides a same-named ancestor field outright —
                    // there is no signature to distinguish them by.
                    _ => String::new(),
                };
                if claimed.insert(key) {
                    out.push(sym);
                }
            }
            for parent in self.workspace_class_parents(&name) {
                queue.push_back(self.normalize_workspace_class_name_in_context(&parent, &name));
            }
        }
        out
    }

    /// Virtual-property accessor symbols for `member` on `class_name`,
    /// walking the inheritance chain exactly as
    /// [`Self::lookup_member_symbols_with_inheritance`] does (which is what
    /// makes inherited and interface-declared accessors resolve).
    ///
    /// AngelScript exposes a `get_X`/`set_X` pair as a property `X`, so an
    /// access matching NO declared member may still be a real reference —
    /// to the accessors. Which ones is decided by `position`: a read wants
    /// `get_X`, a write wants `set_X`, and a compound assign genuinely
    /// touches both (B103).
    ///
    /// Returned nearest-declaration-first in `get_` before `set_` order —
    /// the order the access performs them in. At most one symbol per
    /// accessor name. Empty when the chain declares neither, which is the
    /// caller's cue that the name is simply unknown.
    ///
    /// This is deliberately NOT folded into
    /// [`Self::lookup_member_symbols_with_inheritance`]: a real member
    /// spelled `X` must always win over an accessor pair, so callers try
    /// the exact lookup first and only widen when it comes back empty.
    pub fn lookup_virtual_property_accessor_symbols(
        &self,
        class_name: &str,
        member: &str,
        position: AccessPosition,
    ) -> Vec<&Symbol> {
        let mut names: Vec<String> = Vec::new();
        if position.reads() {
            names.push(format!("get_{}", member));
        }
        if position.writes() {
            names.push(format!("set_{}", member));
        }
        // A write to a get-only property is a mistake the checker reports
        // on its own; navigation still has somewhere useful to go, because
        // the getter is where the property is declared.
        if position == AccessPosition::Write {
            names.push(format!("get_{}", member));
        }
        let mut out: Vec<&Symbol> = Vec::new();
        for name in &names {
            let found = self
                .lookup_member_symbols_with_inheritance(class_name, name)
                .into_iter()
                .find(|s| matches!(s.kind, SymbolKind::Function { .. }));
            if let Some(sym) = found {
                if !out.iter().any(|s| std::ptr::eq(*s, sym)) {
                    out.push(sym);
                }
            }
            // The fallback getter is only wanted when the setter missed.
            if position == AccessPosition::Write && !out.is_empty() {
                break;
            }
        }
        out
    }

    /// Workspace types that reach `type_name` through their transitive
    /// base/interface chain AND declare `member` themselves — the workspace
    /// CLASS implementations and overrides of a method the receiver's static
    /// type declares (B104). Derived interfaces are declarations, not runtime
    /// implementations, and are deliberately excluded.
    ///
    /// The pool carries no reverse (base → derived) index, so the question
    /// is asked forwards of every workspace class and interface. Two traps
    /// this has to step around:
    ///
    /// * [`SymbolTable::all_symbols`] iterates a `HashMap`, so callers must
    ///   not use this result's iteration order as workspace order. Navigation
    ///   sorts resolved locations by URI, because snapshot-local file ids are
    ///   not stable when open documents originate in a concurrent map.
    /// * [`Self::workspace_class_parents`] returns parent names AS WRITTEN,
    ///   so `class C : I` inside `namespace N` yields `"I"`. Every link is
    ///   normalized in its own class's context before comparison, or
    ///   namespaced implementers are silently missed.
    ///
    /// Each implementing class contributes its EFFECTIVE class definition.
    /// That may be inherited from a separate class branch (`C : Base, I`), in
    /// which case navigation lands on `Base::member`, never on C's class body.
    /// Locations are deduplicated because several implementers may inherit the
    /// same definition.
    pub fn workspace_member_implementations(&self, type_name: &str, member: &str) -> Vec<&Symbol> {
        let target = self.normalize_workspace_class_name(type_name);
        let mut candidates: Vec<String> = self
            .workspace()
            .all_symbols()
            .filter(|s| matches!(s.kind, SymbolKind::Class { .. }))
            .map(|s| s.name.clone())
            .collect();
        candidates.sort();
        candidates.dedup();

        let mut out: Vec<&Symbol> = Vec::new();
        let mut seen: std::collections::HashSet<(usize, u32, u32)> =
            std::collections::HashSet::new();
        for candidate in candidates {
            if candidate == target || !self.workspace_type_has_ancestor(&candidate, &target) {
                continue;
            }
            for sym in self.lookup_effective_class_member_symbols(&candidate, member) {
                if seen.insert((sym.file_id, sym.span.start, sym.span.end)) {
                    out.push(sym);
                }
            }
        }
        out
    }

    /// Function definitions that are effective for `class_name`, walking only
    /// concrete class ancestry. Interface declarations are deliberately
    /// skipped: in `class C : I, Base`, I appearing first must not hide the
    /// actual inherited implementation on Base.
    fn lookup_effective_class_member_symbols(
        &self,
        class_name: &str,
        member: &str,
    ) -> Vec<&Symbol> {
        let mut out = Vec::new();
        let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(class_name)]);

        while let Some(name) = queue.pop_front() {
            if !visited.insert(name.clone()) || !self.is_workspace_class(&name) {
                continue;
            }
            for sym in self.lookup_workspace(&format!("{}::{}", name, member)) {
                let SymbolKind::Function { params, .. } = &sym.kind else {
                    continue;
                };
                let types: Vec<String> = params.iter().map(|(_, ty)| ty.clone()).collect();
                if claimed.insert(override_key(&types)) {
                    out.push(sym);
                }
            }
            for parent in self.workspace_class_parents(&name) {
                let resolved = self.normalize_workspace_class_name_in_context(&parent, &name);
                if self.is_workspace_class(&resolved) {
                    queue.push_back(resolved);
                }
            }
        }
        out
    }

    /// True when `target` is in `type_name`'s transitive base/interface
    /// chain. Cycle-guarded and depth-capped like the other inheritance
    /// walks here.
    fn workspace_type_has_ancestor(&self, type_name: &str, target: &str) -> bool {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue =
            std::collections::VecDeque::from([self.normalize_workspace_class_name(type_name)]);
        while let Some(name) = queue.pop_front() {
            if visited.len() > 256 {
                break;
            }
            if !visited.insert(name.clone()) {
                continue;
            }
            for parent in self.workspace_class_parents(&name) {
                let resolved = self.normalize_workspace_class_name_in_context(&parent, &name);
                if resolved == target {
                    return true;
                }
                queue.push_back(resolved);
            }
        }
        false
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

    /// B116: the trailing-`const` qualifier of a METHOD declared directly
    /// on the workspace class `class_name`. `None` when the pooled table
    /// holds no such method on that class — which is also the answer for a
    /// FIELD, for an unknown class, and for an ENGINE type, because the
    /// lookup is against the workspace half of the merged view only.
    ///
    /// That provenance restriction is the point, not an accident. The
    /// const-receiver rule reads this to decide whether to diagnose, and
    /// the typedb carries no const-qualifier metadata at all
    /// (`lookup_method_return`'s own comment says so) — so an external
    /// receiver must come back `None` and stay unchecked rather than look
    /// uniformly non-const and fire on every engine call. Measured on the
    /// 151-plugin fleet: of 827 const-receiver call sites that consult
    /// this, 807 are engine or builtin receivers this `None` excludes.
    ///
    /// Parents are deliberately NOT walked. `index_file_classes` records
    /// only a class's OWN members, so the same-file rule has always been
    /// own-class-only, and this is the surface that has to match it — an
    /// inherited non-const method reached through a derived const receiver
    /// is a separate false negative, identical on both sides of the file
    /// boundary. Walking parents here would have changed exactly one
    /// verdict across the fleet (`Editor::SetSkinSpec::WriteToNetworkBuffer`,
    /// inherited from `NetworkSerializable`), which is its own question.
    ///
    /// `Some(true)` when ANY overload of the name is const: on a const
    /// receiver the engine selects a const overload if one exists, so one
    /// const declaration makes the call legal. Virtual-property accessors
    /// contribute under their stripped spelling too, and are aggregated with
    /// a directly named method; this is the retired sidecar's name-keyed
    /// behavior. B116 intentionally shares this answer across files, keeping
    /// its cross-file verdict aligned with the same-file rule.
    ///
    /// That aggregation PRESERVES A KNOWN FALSE NEGATIVE, recorded here so
    /// it does not read as intent. For
    /// `class C { void M() {} int get_M() const {} }`, the call `c.M()` on a
    /// const receiver invokes the DIRECT, non-const `M()` — the game rejects
    /// it — but the const `get_M` makes this name-keyed answer `Some(true)`
    /// and nothing is diagnosed. The accessor is the property `c.M`, not the
    /// call `c.M()`, so strictly the two spellings should not pool their
    /// constness at a call site.
    ///
    /// It is kept because `file_const_methods` behaved exactly this way —
    /// one set keyed by name, holding both spellings — and B114 is a
    /// RETIREMENT: the pool must answer what the sidecar answered, or the
    /// refactor is smuggling a behaviour change into a diagnostic that fires
    /// on shipped plugins. Splitting the call spelling from the property
    /// spelling is a real improvement and a separate, measurable one.
    /// Pinned by `b114_accessor_names_and_direct_collisions_preserve_name_keyed_constness`,
    /// which states the collision cases explicitly rather than leaving them
    /// to be rediscovered.
    pub fn workspace_own_method_is_const(&self, class_name: &str, method: &str) -> Option<bool> {
        let owner = self.normalize_workspace_class_name(class_name);
        // `index_file_classes` inserted a const `get_V`/`set_V` under `V`
        // even when a direct `V` existed. Keep all three spellings in one
        // candidate set: name-keyed constness is true iff any declaration is
        // const, not whichever spelling happens to be found first.
        let candidates = [
            format!("{owner}::{method}"),
            format!("{owner}::get_{method}"),
            format!("{owner}::set_{method}"),
        ];
        let mut found = false;
        let mut any_const = false;
        for qualified in &candidates {
            for s in self.lookup_workspace(qualified) {
                // B114: the qualifier moved from `SymbolKind::Function`
                // onto `Symbol::modifiers`, so there is ONE home for every
                // declaration modifier rather than one per kind.
                if matches!(s.kind, SymbolKind::Function { .. }) {
                    found = true;
                    any_const |= s.modifiers.is_const;
                }
            }
        }
        found.then_some(any_const)
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

/// True when a symbol DECLARES A TYPE — the only kind of declaration that
/// can shadow a type name. A variable or function that happens to share the
/// name is a VALUE: `namespace Inner { int MaskedCb = 3; }` does not stop
/// `MaskedCb` naming the global funcdef, and treating it as if it did
/// silently swallows the diagnostics that depend on knowing the type.
fn declares_type(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class { .. }
            | SymbolKind::Interface { .. }
            | SymbolKind::Enum { .. }
            | SymbolKind::Funcdef { .. }
    )
}

pub(crate) fn qualified_virtual_name(name: &str, prefix: &str) -> String {
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
    use crate::symbols::scope::{DeclModifiers, Symbol};

    fn empty_span() -> Span {
        Span::new(0, 0)
    }

    fn make_symbol(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            modifiers: DeclModifiers::default(),
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

    /// B081: the mint is the shadow-aware resolver, and both directions are
    /// asserted because the reject half alone is satisfied by a mint that
    /// never returns `Some`.
    ///
    /// `CSmPlayer` is the exact name B063's defect turned on: a plugin
    /// declaring `class CSmPlayer {}` was offered the engine class's injected
    /// methods, because the predicate that got picked resolved the name
    /// against the index whether or not the workspace shadowed it.
    #[test]
    fn the_mint_refuses_a_shadowed_name_and_accepts_an_unshadowed_one() {
        let index = typedb_fixture_index();

        let ws = SymbolTable::new();
        let unshadowed = GlobalScope::new(&ws, Some(&index));
        assert!(
            unshadowed
                .external_type_name("CSmPlayer")
                .is_some_and(|n| n.as_str() == "CSmPlayer"),
            "the engine CSmPlayer must mint when nothing shadows it, or the \
             reject half below proves nothing"
        );

        let mut shadowing = SymbolTable::new();
        let fid = shadowing.allocate_file_id();
        shadowing.set_file_symbols(
            fid,
            vec![make_symbol(
                "CSmPlayer",
                SymbolKind::Class {
                    parents: Vec::new(),
                },
            )],
        );
        let shadowed = GlobalScope::new(&shadowing, Some(&index));
        assert!(
            shadowed.external_type_name("CSmPlayer").is_none(),
            "a workspace `class CSmPlayer {{}}` shadows the engine type, so no \
             witness may be minted for that name"
        );
        // The derivation question is therefore unreachable for a shadowed
        // name: no witness, no way to ask. Paired with the positive control
        // below so this is not the vacuous half — on the UNSHADOWED scope the
        // very same chain walk answers `true`, so `is_none()` above is the
        // shadow check biting rather than the predicate being broken.
        assert!(
            unshadowed
                .external_type_name("CSmPlayer")
                .is_some_and(|n| unshadowed.external_type_derives_from(n, "CMwNod")),
            "control: the engine CSmPlayer really does derive from CMwNod, so \
             the refusal above is about shadowing and nothing else"
        );
        assert!(
            !shadowed
                .external_type_name("CSmPlayer")
                .is_some_and(|n| shadowed.external_type_derives_from(n, "CMwNod")),
            "and with the workspace declaration in place the same question \
             cannot even be put"
        );
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
        assert_eq!(scope.canonical_enum_key_unchecked(anonymous), None);
        assert_eq!(
            scope.canonicalize_type_name(anonymous),
            anonymous,
            "an anonymous key must remain unresolved rather than canonicalized"
        );

        // A NAMED nested enum, to show the anonymous filter is not what
        // hides things here. B056 changed which SPELLING of it resolves: the
        // index key carries the loader's synthetic `Game::` module, and the
        // game answers `Namespace 'Game::CGameCtnBlock' doesn't exist.` for
        // it (corpus row in `docs/probes/b052-qualified-enum-spellings.tsv`).
        // The source spelling — the key minus its module — is what compiles,
        // and this used to assert the opposite.
        let named = "Game::CGameCtnBlock::ECardinalDirections";
        let source_spelling = "CGameCtnBlock::ECardinalDirections";
        assert!(index.lookup_source_enum(named).is_some());
        assert!(
            !scope.has_enum(named) && !scope.resolves(named),
            "the module-prefixed KEY is not a spelling the game accepts"
        );
        assert!(
            scope.has_enum(source_spelling) && scope.resolves(source_spelling),
            "the source spelling must resolve — the anonymous filter must not \
             hide named enums"
        );
        assert_eq!(
            scope.canonicalize_type_name(source_spelling),
            named,
            "…and it must canonicalize back to the stored key, or two \
             spellings of one enum compare unequal"
        );
        assert_eq!(
            scope.canonical_enum_key_unchecked("CGameCtnBlock::ECardinalDirections"),
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

    /// B044's effect set, pinned against the shipped type DB, and B057's
    /// correction to the half of it that was over-general.
    ///
    /// B044 could see only the anonymous/named split, so it asserted two
    /// halves: the 258 names reachable ONLY through an anonymous enum are
    /// rejected, and every NAMED enum's value keeps resolving. The first
    /// half still holds and is checked below unchanged — the game rejects
    /// all 258, with no residual. The second half did not: the exhaustive
    /// probe (`docs/probes/b053-bare-enum-values.tsv`) found the game
    /// rejecting 2373 named-enum value names, because what puts a value in
    /// the global scope is the enum's VISIBILITY, not its having a name.
    /// B057 replaced that half with the assertion below, and
    /// `every_probed_bare_value_matches_the_games_verdict` is where the full
    /// rule is checked against the game.
    ///
    /// 258 is the exact size of the anonymous-only set, and it is the list
    /// `scripts/gen_b044_probe.py` generates and the game was probed against.
    /// Refreshing the type DB moves it: regenerate the fixture and re-probe.
    ///
    /// It was 261 until B046. `ESceneCharVisNotice` is a module-level enum
    /// no dump member is typed by, so only `Openplanet.h` names it; once the
    /// loader reads the header, its 3 values that the game accepts
    /// (`DoubleJump`, `Jump`, `TechJump`) land in the globally visible half
    /// and leave the narrowed set.
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

        // B057: a GLOBALLY VISIBLE enum's value keeps resolving — that is
        // the surviving half of the scan, and it is what stops the narrowing
        // from swallowing the 592 names the game accepts and the 22 it calls
        // ambiguous.
        let mut global = 0usize;
        for value in &named {
            if index.global_enum_value_owners(value).is_empty() {
                continue;
            }
            global += 1;
            assert!(
                scope.has_global_ident(value),
                "`{value}` is a globally visible enum's value and must keep \
                 resolving"
            );
        }
        assert!(
            global > 500,
            "fixture guard: the DB must carry a substantial globally visible \
             set, got {global}"
        );

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

    /// B057 fence, correcting B044's: a value name that also appears in a
    /// GLOBALLY VISIBLE enum keeps resolving. Sharing a name with a merely
    /// *named* enum is not enough, and the game is the one that says so.
    ///
    /// B044 pinned this with `VCenter2` — shared between the anonymous
    /// `Control::CControlBase::AlignVertical` and the named
    /// `Game::CGameManialinkControl::EAlignVertical` — and asserted it
    /// resolved. The probe answered `No matching symbol 'VCenter2'`: the
    /// enum it is shared with is NESTED, so it never put the name in the
    /// global scope. Both directions are pinned here now, on the 32 names
    /// the DB shares between an anonymous enum and a global one.
    #[test]
    fn bare_value_shared_with_a_global_enum_still_resolves() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let is_anonymous_value = |value: &str| {
            index
                .enums_iter()
                .any(|(_, info)| info.anonymous && info.values.iter().any(|(v, _)| v == value))
        };

        // Shared with a GLOBAL enum → resolves. `Border` and `Jump` are game
        // ACCEPTED, `Medium` and `None` are game AMBIGUOUS — both are
        // symbols, and only the second pair reaches B053's diagnostic.
        for (value, owner) in [
            ("Border", "EGxTexAddress"),
            ("Jump", "ESceneCharVisNotice"),
            ("Medium", "EPlugRoadDensity"),
            ("None", "EGameItemWaypointType"),
        ] {
            assert!(
                is_anonymous_value(value),
                "fixture guard: `{value}` must also be an anonymous enum value"
            );
            assert!(
                index
                    .global_enum_value_owners(value)
                    .iter()
                    .any(|o| o == owner),
                "fixture guard: the global `{owner}` must carry `{value}`"
            );
            assert!(
                scope.has_global_ident(value),
                "`{value}` is shared with a globally visible enum and must \
                 keep resolving"
            );
        }

        // Shared with a merely NAMED enum → does not. This is the case B044
        // got wrong; the game's answer is `No matching symbol 'VCenter2'`.
        let nested = "Game::CGameManialinkControl::EAlignVertical";
        assert!(
            index
                .lookup_source_enum(nested)
                .is_some_and(|info| info.values.iter().any(|(v, _)| v == "VCenter2")),
            "fixture guard: the named nested enum must still carry `VCenter2`"
        );
        assert!(
            is_anonymous_value("VCenter2"),
            "fixture guard: `VCenter2` must also be an anonymous enum value"
        );
        assert!(
            !scope.has_global_ident("VCenter2"),
            "`{nested}` is nested, so it does not make `VCenter2` a symbol"
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
                assert_eq!(scope.canonical_enum_key_unchecked(&qualified), None);
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
                assert_eq!(scope.canonical_enum_key_unchecked(&qualified), None);
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
                    scope.canonical_enum_key_unchecked(&qualified),
                    Some(tail.to_string()),
                    "`{qualified}` canonicalizes to its global enum key"
                );
                assert_eq!(scope.canonicalize_type_name(&qualified), tail);
            }
        }
    }

    /// B056 fence: `has_enum` and `canonicalize_type_name` must agree, and
    /// they did not. The ancestor walk in `has_enum` was widened to
    /// `global_enum_key` so it would answer for a global enum whose KEY
    /// carries a synthetic module (`Game::EDummyCollectionIdent`), but the
    /// canonicalizer's matching arm was left on `lookup_source_enum`, which
    /// is true only when the key is already bare. The result: 57 enums that
    /// `has_enum` accepts behind a scope and the canonicalizer hands back
    /// unchanged, so two spellings of one enum stay unequal.
    ///
    /// The law is the general fence — every accepted spelling canonicalizes
    /// to a real key — and the pinned rows below are the ones that were
    /// wrong.
    ///
    /// This test deliberately does NOT rest on a game verdict, and that is
    /// worth stating because the obvious supporting rows are no longer safe
    /// to lean on: `Meta::EGmSurfType`, `Meta::EPlugSurfaceMaterialId` and a
    /// bare `EDummyCollectionIdent` are all ACCEPTED in the B052 corpus, but
    /// B068 found that `probe_enum_corpus.py` batches names into one
    /// compilation unit, so one parse error marks the whole rest of a batch
    /// ACCEPTED. REJECTED rows there are solid; ACCEPTED rows are provisional.
    ///
    /// What is asserted instead is INTERNAL CONSISTENCY: whatever the game's
    /// answer turns out to be, `has_enum` and `canonicalize_type_name` must
    /// give the same one. This holds either way. If the ACCEPTED rows are
    /// sound, the fix is also the game-correct behaviour; if B068 shows they
    /// are corrupt and these spellings are really rejected, the defect moves
    /// to `has_enum` accepting them — B056's merged ancestor arm — and this
    /// test still holds, because it pins agreement rather than acceptance.
    #[test]
    fn an_accepted_enum_spelling_always_canonicalizes_to_a_real_key() {
        let index = typedb_fixture_index();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(fid, vec![make_symbol("Foo", SymbolKind::Namespace)]);
        let scope = GlobalScope::new(&ws, Some(&index));

        // Left: the enum's stored key. Right: whether the key is bare. The
        // module-prefixed rows are the ones the old arm could not reach.
        let cases = [
            ("EDummyCollectionIdent", "Game::EDummyCollectionIdent"),
            ("EGhostNameLogoType", "Game::EGhostNameLogoType"),
            ("EPlugSurfaceMaterialId", "Meta::EPlugSurfaceMaterialId"),
            ("EGmSurfType", "Meta::EGmSurfType"),
            // A bare-key global, to hold the pre-existing behaviour still.
            ("VirtualKey", "VirtualKey"),
        ];

        // Scopes the game accepts as qualifiers: a Core namespace, an engine
        // class scope, and a workspace-declared namespace.
        let prefixes = ["Meta", "UI", "CGameMenu", "Foo"];

        for (name, key) in cases {
            assert_eq!(
                index.global_enum_key(name),
                Some(key),
                "fixture guard: `{name}` must be a globally visible enum keyed at `{key}`"
            );

            for prefix in prefixes {
                let qualified = format!("{prefix}::{name}");
                assert!(
                    scope.has_enum(&qualified),
                    "`{qualified}` is a global enum behind a real scope"
                );
                let canonical = scope.canonicalize_type_name(&qualified);
                assert_eq!(
                    canonical, key,
                    "`{qualified}` must canonicalize to the stored key, not to its spelling"
                );
                assert!(
                    index.lookup_source_enum(&canonical).is_some(),
                    "`{canonical}` must be a real enum key"
                );
            }
        }

        // The law, over every real scope the fixture knows: an accepted
        // spelling never canonicalizes to something that is not a key.
        for prefix in index.source_scopes() {
            for (name, key) in cases {
                let qualified = format!("{prefix}::{name}");
                if !scope.has_enum(&qualified) {
                    continue;
                }
                assert_eq!(
                    scope.canonicalize_type_name(&qualified),
                    key,
                    "`{qualified}` resolves, so it must canonicalize to `{key}`"
                );
            }
        }
    }

    /// B052: declaring `A::B` declares `A`, so an ANCESTOR of a known scope
    /// is a scope. The Core database only ever names `Internal::NadeoServices`
    /// — never `Internal` alone — and the deepest engine scopes are two
    /// segments deep, so without this the LSP rejected paths the game
    /// compiles. That is a false POSITIVE, the damaging direction.
    ///
    /// Every name here was probed. The ancestors accept a global enum, and
    /// each answers `Identifier 'ZZZNoSuch' is not a data type in namespace
    /// '<ancestor>' or parent` for an absent name — the reply only a scope
    /// that EXISTS gives, as against `Namespace 'X' doesn't exist.`
    #[test]
    fn an_ancestor_of_a_known_scope_is_a_scope() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        for prefix in [
            // Core: only `Internal::NadeoServices` is ever named.
            "Internal",
            "Internal::NadeoServices",
            // Engine: ancestors of the two deepest class scopes.
            "NSysCfgVision",
            "NSysCfgVision::SDeferred",
            "CHmsSolidVisCst_TmCar",
            "CHmsSolidVisCst_TmCar::SPrestige",
        ] {
            assert!(
                index.scope_exists(prefix),
                "`{prefix}` is a scope the game resolves through"
            );
            let qualified = format!("{prefix}::VirtualKey");
            assert!(
                scope.has_enum(&qualified),
                "the game accepts `{qualified}`; rejecting it is a false positive"
            );
        }

        // The ancestor rule takes whole PATH prefixes, never a short name
        // lifted out of the middle: `SDeferred` alone is not a scope and the
        // game answers `Namespace 'SDeferred' doesn't exist.`
        assert!(!index.scope_exists("SDeferred"));
        assert!(!scope.has_enum("SDeferred::VirtualKey"));
        assert!(!index.scope_exists("SPrestige"));
        assert!(!scope.has_enum("SPrestige::VirtualKey"));
    }

    /// B052/B053 determinism: both new indexes are order-stable, so two loads
    /// of identical bytes answer identically.
    ///
    /// Worth pinning rather than assuming: both are built by iterating
    /// `HashMap`s, whose order is not stable across runs. The scope index is a
    /// set, so order cannot leak; the global-enum-value index builds a `Vec`
    /// of owner names and must therefore SORT, because that vector is printed
    /// verbatim in the B053 diagnostic and an unsorted one would give the same
    /// file different messages on different runs.
    #[test]
    fn the_scope_and_global_enum_indexes_are_deterministic() {
        let a = typedb_fixture_index();
        let b = typedb_fixture_index();

        for path in [
            "UI",
            "Internal",
            "Internal::NadeoServices",
            "CGameMenu",
            "NSysCfgVision::SDeferred",
            "Scene",
            "Bogus",
        ] {
            assert_eq!(
                a.scope_exists(path),
                b.scope_exists(path),
                "`{path}` must answer the same on both loads"
            );
        }

        for value in ["Turbo", "None", "WallJump", "GlassSmash", "Ordered"] {
            assert_eq!(
                a.global_enum_value_owners(value),
                b.global_enum_value_owners(value),
                "`{value}`'s candidate list must be identical, and in the same \
                 order — it is printed verbatim in the diagnostic"
            );
            let owners = a.global_enum_value_owners(value);
            let mut sorted = owners.to_vec();
            sorted.sort();
            assert_eq!(
                owners,
                sorted.as_slice(),
                "`{value}`'s owners must be sorted"
            );
        }
    }

    /// B052: the workspace half of `scope_exists` cannot act at a distance.
    ///
    /// A plugin's `namespace Foo` makes `Foo::VirtualKey` resolve, which is
    /// correct — the game compiles a plugin as ONE module, so a namespace
    /// declared in any of its files is in scope in all of them, and that was
    /// probed. The risk is the other direction: that declaring or deleting a
    /// namespace silently changes a file that never mentions it.
    ///
    /// It cannot, and this pins why. `scope_exists` is only ever reached
    /// through a qualified name the source itself wrote, so a file that spells
    /// no `Foo::` path never asks about `Foo`.
    #[test]
    fn a_workspace_namespace_only_affects_names_that_spell_it() {
        let index = typedb_fixture_index();

        let without = SymbolTable::new();
        let mut with = SymbolTable::new();
        let fid = with.allocate_file_id();
        with.set_file_symbols(fid, vec![make_symbol("Foo", SymbolKind::Namespace)]);

        let a = GlobalScope::new(&without, Some(&index));
        let b = GlobalScope::new(&with, Some(&index));

        // The ONE name whose answer may differ is the one that spells `Foo`.
        assert!(!a.has_enum("Foo::VirtualKey"));
        assert!(b.has_enum("Foo::VirtualKey"));

        // Everything else is unmoved by the namespace's presence.
        for name in [
            "VirtualKey",
            "ESceneCharVisNotice",
            "UI::VirtualKey",
            "CGameMenu::VirtualKey",
            "CGameMenu::EMenuOrder",
            "Scene::ESceneCharVisNotice",
            "Game::CGameMenu::VirtualKey",
            "Bogus::VirtualKey",
            "Quux::VirtualKey",
        ] {
            assert_eq!(
                a.has_enum(name),
                b.has_enum(name),
                "`{name}` does not spell `Foo`, so declaring `namespace Foo` \
                 must not change it"
            );
            assert_eq!(a.resolves(name), b.resolves(name), "`{name}` likewise");
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

    /// B056: a Nadeo MODULE name is a live AngelScript scope exactly when
    /// Core also declares it as a namespace — with one recorded exception.
    ///
    /// Driven from the paired probes already committed in
    /// `docs/probes/b052-qualified-enum-spellings.tsv`: every one of the 20
    /// module names was submitted as `<Module>::ZZZNoSuchEnumName`, and the
    /// compiler's two answers separate the cases cleanly.
    ///
    /// * `Namespace 'X' doesn't exist.` — not a scope. 16 of them.
    /// * `Identifier ... is not a data type in namespace 'X' or parent` — it
    ///   IS a scope. 4 of them: `Audio`, `Meta`, `Net`, `GameData`.
    ///
    /// Three of the four are Core namespaces, which is why `scope_exists`
    /// already answers for them and why `Meta::ESceneCharVisNotice` resolves
    /// while `Game::ESceneCharVisNotice` does not. `GameData` is the sole
    /// exception: a live engine scope that neither shipped database records,
    /// so this crate answers false for it and the corpus test excludes that
    /// one row. See `UNEXPLAINED_ENGINE_SCOPES` in the resolver tests.
    ///
    /// The point of pinning it here is that the exception stays exactly one
    /// name. If a second module name ever starts behaving this way, or if a
    /// module name that is NOT a Core namespace starts answering true, this
    /// goes red rather than quietly widening what a qualifier may be.
    #[test]
    fn a_module_name_is_a_scope_only_when_core_declares_it() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // The loader's own module names — the first segment of every Nadeo
        // key — taken from the index rather than re-derived.
        let modules: std::collections::HashSet<String> =
            index.nadeo_module_names().into_iter().collect();
        assert_eq!(
            modules.len(),
            20,
            "fixture guard: the dump must carry its 20 module names"
        );

        // The paired-probe verdicts, read out of the committed corpus.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs/probes/b052-qualified-enum-spellings.tsv");
        let text = std::fs::read_to_string(&path).expect("the probe corpus must be committed");
        let mut game_says_scope: Vec<String> = Vec::new();
        let mut game_says_not: Vec<String> = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#')).skip(1) {
            let mut cols = line.split('\t');
            let spelling = cols.next().unwrap_or_default();
            let _verdict = cols.next();
            let message = cols.next().unwrap_or_default();
            let Some(prefix) = spelling.strip_suffix("::ZZZNoSuchEnumName") else {
                continue;
            };
            // MODULE names only. The corpus also pairs engine CLASS prefixes,
            // which follow a different rule (B052: a class is a namespace iff
            // it declares an enum) and are not what this test is about. Note
            // in passing that `CMwNod` is a live game scope `scope_exists`
            // answers false for — the same shape as `GameData`, in class
            // position. No corpus row depends on it, so it stays an
            // observation rather than a change.
            if !modules.contains(prefix) {
                continue;
            }
            if message.contains("doesn't exist") {
                game_says_not.push(prefix.to_string());
            } else if message.contains("is not a data type in namespace") {
                game_says_scope.push(prefix.to_string());
            }
        }
        assert_eq!(
            game_says_scope.len() + game_says_not.len(),
            20,
            "fixture guard: all 20 module names must have a paired probe"
        );
        assert_eq!(
            game_says_scope.len(),
            4,
            "exactly four module names are live scopes: Audio, Meta, Net, GameData"
        );

        for prefix in &game_says_not {
            assert!(
                !scope.scope_exists(prefix),
                "the game answers `Namespace '{prefix}' doesn't exist.`, so it \
                 must not qualify anything"
            );
        }
        for prefix in &game_says_scope {
            // `GameData` is the one scope neither database records. Keeping it
            // false is a known, documented under-approximation, not a bug —
            // and it must stay the ONLY one.
            if prefix == "GameData" {
                assert!(
                    !scope.scope_exists(prefix),
                    "`GameData` is not recorded by either DB; if this starts \
                     answering true, the exclusion in the resolver's corpus \
                     test is now wrong and must be removed"
                );
                continue;
            }
            assert!(
                scope.scope_exists(prefix),
                "the game treats `{prefix}` as a scope and Core declares it, \
                 so `scope_exists` must agree"
            );
        }
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

    /// B057: the values of a NAMED but NOT globally visible enum are not
    /// symbols. Being named is what lets source write the enum's own NAME;
    /// it is not what puts the enum's values in the global value scope.
    ///
    /// This replaces the B044 fence `named_nested_enum_values_still_resolve_bare`,
    /// which asserted the opposite for this very enum. B044 could only see
    /// the anonymous/named split and generalised from it; the exhaustive
    /// probe of all 2989 bare value names (`docs/probes/b053-bare-enum-values.tsv`)
    /// showed the split that actually governs is global visibility. The game
    /// answers `No matching symbol 'North'` — and the same for `East`,
    /// `South` and `West` — for the nested enum this test used to pin.
    #[test]
    fn named_nested_enum_values_do_not_resolve_bare() {
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
        // The enum's own name stays writable — the narrowing is about the
        // enum's VALUES entering the global value scope, not about the enum
        // disappearing from the type index.
        assert!(
            index.lookup_source_enum(named).is_some(),
            "the nested enum itself stays indexed"
        );
        for value in &values {
            assert!(
                !scope.has_global_ident(value),
                "`{value}` belongs to a nested enum; the game answers \
                 `No matching symbol`"
            );
        }
    }

    /// B057: a namespaced Core enum's values are not symbols either — the
    /// same rule, reached through the other branch of `enum_is_global`.
    ///
    /// `UI::Cond` is named, source-writable and namespaced. The game rejects
    /// the four values only it carries (`No matching symbol 'Appearing'`,
    /// and likewise `Always`, `Once`, `FirstUseEver`); it also rejects
    /// `UI::Appearing`, because value lookup does not walk ancestor scopes
    /// the way type lookup does. See the README's third trap.
    ///
    /// Its fifth value, `None`, keeps resolving — fifteen globally visible
    /// enums carry it, so the game answers `Found multiple matching enum
    /// values` rather than `No matching symbol`. That is the fence: the
    /// narrowing is a statement about which ENUM a name reaches the scope
    /// through, never about the name.
    #[test]
    fn namespaced_core_enum_values_do_not_resolve_bare() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let named = "UI::Cond";
        let info = index
            .lookup_source_enum(named)
            .expect("fixture guard: the namespaced Core enum must exist");
        assert!(!info.anonymous, "fixture guard: it is source-writable");
        let values: Vec<&str> = info.values.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(
            values,
            ["None", "Always", "Once", "FirstUseEver", "Appearing"],
            "fixture guard: `UI::Cond`'s values changed — re-read the corpus"
        );

        for value in ["Always", "Once", "FirstUseEver", "Appearing"] {
            assert!(
                index.global_enum_value_owners(value).is_empty(),
                "fixture guard: only `{named}` carries `{value}`"
            );
            assert!(
                !scope.has_global_ident(value),
                "`{value}` belongs to the namespaced `{named}`; the game \
                 answers `No matching symbol`"
            );
        }
        assert!(
            scope.has_global_ident("None"),
            "`None` is carried by globally visible enums too and must keep \
             resolving — the game reports ambiguity, not an unknown symbol"
        );
        assert!(
            !index
                .global_enum_value_owners("None")
                .iter()
                .any(|o| o == "Cond"),
            "`UI::Cond` is namespaced, so it is not what makes `None` visible"
        );
    }

    /// B057 fence: the narrowing must not hide a plugin's OWN declaration of
    /// a name the engine rejects. The false-POSITIVE direction, and the
    /// damaging one for this tool.
    ///
    /// The scan runs LAST in `has_global_ident`, after the type, function,
    /// virtual-property, workspace-variable and workspace-enum-value lookups.
    /// This pins that ordering at the seam, on names the committed corpus
    /// records as `NO_SYMBOL` — so it cannot pass by picking a name nothing
    /// carries, which is exactly how the first version of this fence went
    /// wrong (it used `North2`, `Northward`, `Southward`, names no engine enum
    /// has, and so would have passed on the merge-base too).
    ///
    /// The same four shapes are checked end to end, against the live game, by
    /// the fixture `b057-nonglobal-enum-values-workspace`.
    #[test]
    fn a_workspace_declaration_outranks_a_non_global_engine_enum_value() {
        let index = typedb_fixture_index();

        // Guard first: with an empty workspace every one of these is rejected,
        // and rejected because NO globally visible enum carries it. Without
        // this the test could pass on a name the engine never had.
        let empty = SymbolTable::new();
        let empty_scope = GlobalScope::new(&empty, Some(&index));
        for (name, owner) in [
            ("North", "Game::CGameCtnBlock::ECardinalDirections"),
            ("VCenter2", "Game::CGameManialinkControl::EAlignVertical"),
            ("Once", "UI::Cond"),
            ("Appearing", "UI::Cond"),
        ] {
            assert!(
                index
                    .lookup_source_enum(owner)
                    .is_some_and(|i| i.values.iter().any(|(v, _)| v == name)),
                "fixture guard: `{owner}` must carry `{name}`"
            );
            assert!(
                index.global_enum_value_owners(name).is_empty(),
                "fixture guard: no globally visible enum may carry `{name}`"
            );
            assert!(
                !empty_scope.has_global_ident(name),
                "fixture guard: `{name}` is `NO_SYMBOL` in the corpus with \
                 nothing declaring it"
            );
        }

        // Each of the four lookups that run before the scan, one at a time, so
        // a regression names the shape that broke rather than just failing.
        let variable = SymbolKind::Variable {
            type_name: "int".into(),
        };
        let function = SymbolKind::Function {
            return_type: "void".into(),
            params: Vec::new(),
            min_args: 0,
            imported: false,
        };
        for (name, symbols) in [
            // A global variable.
            ("North", vec![make_symbol("North", variable.clone())]),
            // The plugin's own enum carrying it as a value.
            (
                "VCenter2",
                vec![
                    make_symbol("LocalAlignment", SymbolKind::Enum { values: Vec::new() }),
                    make_symbol(
                        "LocalAlignment::VCenter2",
                        SymbolKind::EnumValue {
                            enum_name: "LocalAlignment".into(),
                            value: Some(0),
                        },
                    ),
                ],
            ),
            // A global function.
            ("Once", vec![make_symbol("Once", function.clone())]),
            // A virtual property, reached through its `get_` accessor.
            (
                "Appearing",
                vec![make_symbol(
                    "get_Appearing",
                    SymbolKind::Function {
                        return_type: "int".into(),
                        params: Vec::new(),
                        min_args: 0,
                        imported: false,
                    },
                )],
            ),
        ] {
            let mut ws = SymbolTable::new();
            let fid = ws.allocate_file_id();
            ws.set_file_symbols(fid, symbols);
            let scope = GlobalScope::new(&ws, Some(&index));
            assert!(
                scope.has_global_ident(name),
                "a workspace declaration of `{name}` must outrank the \
                 narrowing — the game compiles this"
            );
        }
    }

    /// B057: the whole rule, checked against the live game's own verdict on
    /// every bare value name in the shipped type DB.
    ///
    /// `docs/probes/b053-bare-enum-values.tsv` is the compiler's verbatim
    /// answer for all 2989 bare value names of the DB's named enums
    /// (RemoteBuild, Trackmania + Openplanet 1.29.5, 2026-08-30). This is
    /// the gate the ticket asks for: no name may change status without a
    /// recorded game verdict behind it.
    ///
    /// The mapping from verdict to `has_global_ident`:
    ///
    /// * `ACCEPTED`  (592) → true. Exactly one globally visible enum carries it.
    /// * `AMBIGUOUS` ( 22) → true. Two or more do; the name IS a symbol, and
    ///   the refusal to choose is B053's separate diagnostic. Answering
    ///   false here would report `undefined identifier` for a name the
    ///   compiler can see perfectly well.
    /// * `NO_SYMBOL` (2373) → false. This ticket's effect set.
    /// * `OTHER`     (  2) → decided per name, because neither reached the
    ///   engine's enums in the game either. Both are named below with the
    ///   compiler's own words; the corpus is asserted to contain no third.
    #[test]
    fn every_probed_bare_value_matches_the_games_verdict() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let corpus = b053_probe_corpus();
        assert_eq!(
            corpus.len(),
            2989,
            "the probe corpus changed size — re-probe the game before \
             touching this rule"
        );

        let mut wrong: Vec<String> = Vec::new();
        let mut counts = std::collections::BTreeMap::new();
        for (value, verdict) in &corpus {
            *counts.entry(verdict.as_str()).or_insert(0usize) += 1;
            let expected = match verdict.as_str() {
                "NO_SYMBOL" => false,
                "ACCEPTED" | "AMBIGUOUS" => true,
                // The probe plugin's own `Main()` entry point answered, not
                // the engine — hence a conversion error rather than `No
                // matching symbol`. An empty workspace declares no `Main`,
                // so `false` is the honest answer here; a real plugin's
                // `Main()` is found by the function lookup at the top of
                // `has_global_ident`, well before any enum.
                "OTHER" if value == "Main" => false,
                // `Expression 'RGBAColor' is a data type` — the game's own
                // words. It is a Core TYPE, and the type lookup answers
                // first here exactly as it does in-game.
                "OTHER" if value == "RGBAColor" => true,
                "OTHER" => panic!(
                    "`{value}` is a new OTHER residual — read the game's \
                     message in the corpus and decide it explicitly"
                ),
                other => panic!("unknown verdict `{other}` for `{value}`"),
            };
            if scope.has_global_ident(value) != expected {
                wrong.push(format!("{value} ({verdict})"));
            }
        }
        assert_eq!(
            counts.get("NO_SYMBOL").copied().unwrap_or(0),
            2373,
            "the corpus no longer carries the 2373 rejects the ticket cites"
        );
        assert_eq!(
            counts.get("OTHER").copied().unwrap_or(0),
            2,
            "the corpus has a residual the arms above do not name"
        );
        assert!(
            wrong.is_empty(),
            "{} of {} probed names disagree with the game: {:?}",
            wrong.len(),
            corpus.len(),
            &wrong[..wrong.len().min(25)]
        );
    }

    /// `(value, verdict)` for every row of the committed B053 probe corpus.
    fn b053_probe_corpus() -> Vec<(String, String)> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs/probes/b053-bare-enum-values.tsv");
        let text = std::fs::read_to_string(&path).expect("the probe corpus must be committed");
        text.lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .skip(1) // column header
            .map(|line| {
                let mut cols = line.split('\t');
                let value = cols.next().expect("a value column").to_string();
                let verdict = cols.next().expect("a verdict column").to_string();
                (value, verdict)
            })
            .collect()
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
                SymbolKind::Class { parents: vec![] },
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
                SymbolKind::Interface { parents: vec![] },
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
                SymbolKind::Class { parents: vec![] },
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
            vec![make_symbol("mat3", SymbolKind::Class { parents: vec![] })],
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

    /// B146: the permissive unknown policy, pinned at the boundary that
    /// produces it, and PAIRED against a Core method that really is `&out` so
    /// the test cannot pass by the typedb simply being empty.
    ///
    /// Nadeo states no direction, so every Nadeo slot must come out `Value` /
    /// not-out. Core states one, so a genuine `&out` slot must come out `Out`
    /// / out. Asserting only the first half would be satisfied by a lookup
    /// that returns nothing at all.
    #[test]
    fn nadeo_slots_are_permissive_and_core_out_slots_are_not() {
        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).unwrap();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));

        // The Nadeo half: `EditReplay(MwFastBuffer<wstring>& ReplayList)` is
        // one of the 61 `&`-carrying params. Unknown direction must not
        // become an `&out` slot.
        let nadeo = idx
            .find_by_short_name("CGameManiaTitleControlScriptAPI")
            .iter()
            .find(|h| h.ends_with("::CGameManiaTitleControlScriptAPI"))
            .cloned()
            .expect("fixture must carry the class");
        let sigs = scope
            .lookup_external_method_param_overloads(&nadeo, "EditReplay")
            .expect("EditReplay must resolve on the Nadeo class");
        assert!(!sigs.is_empty(), "no EditReplay overload found");
        let mut checked = 0usize;
        for sig in &sigs {
            for (i, ty) in sig.param_types.iter().enumerate() {
                assert!(
                    !sig.param_out[i],
                    "unknown direction must not become an `&out` slot: {ty}"
                );
                assert_eq!(
                    sig.param_modes[i],
                    ParamMode::Value,
                    "unknown direction must stay permissive: {ty}"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "EditReplay has no params to check");

        // The Core half: a real `&out` must still be recognised, or the
        // assertions above prove nothing about the mechanism. `UI::Begin`'s
        // `bool &out open` is the exact slot GH #47 documents.
        let core = scope
            .lookup_external_function_param_overloads("UI::Begin")
            .expect("UI::Begin must exist in the fixture");
        assert!(!core.is_empty(), "UI::Begin has no overloads");
        let saw_core_out = core.iter().any(|sig| sig.param_out.iter().any(|o| *o));
        assert!(
            saw_core_out,
            "Core must still report genuine `&out` params, or the Nadeo \
             assertions above are vacuous"
        );
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
                    SymbolKind::Class { parents: vec![] },
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
                make_symbol("ItemSpec", SymbolKind::Class { parents: vec![] }),
                make_symbol("Editor::ItemSpec", SymbolKind::Class { parents: vec![] }),
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
    /// B080: b053's answer to "which rows are environment-dependent?" — and it
    /// is already in the file, in a column nobody had named as provenance.
    ///
    /// `global_enum_owners` records, per row, WHICH globally visible enums of
    /// the shipped type DB carry that value. It was written at probe time,
    /// beside the game's verdict, by a different instrument. So the two
    /// together say more than either alone:
    ///
    /// * ACCEPTED with owners — the model and the game agree, and the model's
    ///   reason is recorded. Environment-INDEPENDENT as far as this corpus can
    ///   tell.
    /// * ACCEPTED with NO owners — the game took a bare name the shipped DB
    ///   cannot explain. That is the value-position twin of b052's `Foo::`
    ///   rows: a plugin declaring a GLOBAL enum with that value makes it
    ///   compile, and the row measures that machine.
    ///
    /// **There are none of the second kind**, which is why b053 re-probed
    /// 592/592 unchanged while b052 moved three rows. Asserting it here turns
    /// that from a fact someone noticed once into a gate: a corpus regenerated
    /// on a machine with such a plugin fails and NAMES the values, instead of
    /// quietly adding rows this crate would then be required to accept.
    ///
    /// b052 needs a whole sidecar for the same question because its rows turn
    /// on a SCOPE, and a scope's origin is not recorded anywhere in that file.
    #[test]
    fn every_accepted_bare_value_is_explained_by_the_shipped_type_db() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs/probes/b053-bare-enum-values.tsv");
        let text = std::fs::read_to_string(&path).expect("the probe corpus must be committed");

        let mut accepted = 0usize;
        let mut unexplained: Vec<String> = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() || line.starts_with("value\t") {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            let (value, verdict) = (cols[0], cols[1]);
            if verdict != "ACCEPTED" {
                continue;
            }
            accepted += 1;
            if cols.get(3).map(|c| c.trim()).unwrap_or("").is_empty() {
                unexplained.push(value.to_string());
            }
        }

        assert_eq!(
            accepted, 592,
            "anti-vacuity: the ACCEPTED rows are still found"
        );
        assert!(
            unexplained.is_empty(),
            "{} bare value(s) the game accepted have no globally visible enum in \
             the shipped DB, so the probing machine's plugins supplied them: {:?}",
            unexplained.len(),
            &unexplained[..unexplained.len().min(20)]
        );
    }
}
