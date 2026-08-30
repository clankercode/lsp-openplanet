use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::core_format::{CoreClass, CoreDatabase};
use super::header_format::HeaderDatabase;
use super::nadeo_format::{NadeoDatabase, NadeoEnumEntry, NadeoMemberKind, NadeoType};

/// The engine header, read from the same directory as the game dump for the
/// module-level enums the dump cannot express (B046).
pub const HEADER_FILENAME: &str = "Openplanet.h";

/// Merged type index combining Core API and Nadeo game engine types.
pub struct TypeIndex {
    /// All types keyed by fully qualified name (e.g., "Net::HttpRequest")
    types: HashMap<String, TypeInfo>,
    /// Global functions keyed by qualified name (e.g., "UI::Begin")
    functions: HashMap<String, Vec<FunctionInfo>>,
    /// Enums keyed by qualified name
    enums: HashMap<String, EnumInfo>,
    /// Core funcdefs keyed by qualified name (e.g. "UI::InputTextCallback")
    funcdefs: HashMap<String, FuncdefInfo>,
    /// Short-name → list of fully qualified names that end with `::<short>`
    /// (or exactly match `<short>`). Built lazily by `ensure_short_index`.
    /// Covers types, enums, and funcdefs — functions are keyed differently
    /// and this map is only consulted when resolving a referenced *type* name.
    short_type_index: HashMap<String, Vec<String>>,
    /// Every scope the GAME has, spelled the way source must spell it (B052).
    /// Built by `build_scope_index` alongside the short-name index; see
    /// [`Self::scope_exists`] for what qualifies and why the Nadeo module
    /// prefixes this crate keys the dump under are excluded.
    scopes: HashSet<String>,
    /// Bare enum-value name → the distinct NAMES of the globally visible
    /// enums that carry it (B053). Built by `build_global_enum_value_index`;
    /// see [`Self::global_enum_value_owners`] for what "globally visible"
    /// means and why the names, not the keys, are what get counted.
    global_enum_values: HashMap<String, Vec<String>>,
    /// Bare enum name → index key, for every GLOBALLY VISIBLE enum (B056).
    ///
    /// These are the enums AngelScript's ancestor walk can reach from any
    /// real scope, so this is the set `P::T` is checked against once `P` is
    /// known to be a scope. A globally visible enum's source name is bare by
    /// construction — that equivalence is pinned by
    /// `a_globally_visible_enum_is_exactly_one_with_a_bare_source_name`.
    global_enum_names: HashMap<String, String>,
    /// SOURCE NAME → index key, for every source-writable type, enum and
    /// funcdef (B056). The source name is the spelling the GAME accepts,
    /// which is not the index key: see [`Self::source_name_of`].
    ///
    /// One source name can own several keys — the loader keys one engine
    /// enum under several modules, so `Hms::EHmsLightMapQuality` and
    /// `MetaNotPersistent::EHmsLightMapQuality` both reduce to
    /// `EHmsLightMapQuality`. The lexicographically first key wins, matching
    /// `build_short_type_index`, so the mapping is deterministic.
    source_names: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub name: String,
    pub namespace: Option<String>,
    pub parent: Option<String>,
    pub methods: Vec<MethodInfo>,
    pub properties: Vec<PropertyInfo>,
    /// GH #54: parameter type of every *single-argument* construct behavior
    /// (`asBEHAVE_CONSTRUCT`) registered on this type, minus the copy
    /// constructor. AngelScript uses such a constructor as an implicit
    /// conversion, so this is exactly the set of types it can convert *into*
    /// this one in one step (`mat4` lists `mat3`, `iso4`). A registered
    /// CONSTRUCT behavior implies a value type — reference types get
    /// `asBEHAVE_FACTORY` instead — so no handle/reference type appears here.
    /// Empty for Nadeo-sourced types (that format carries no behaviors).
    pub ctor_conversions: Vec<String>,
    pub doc: Option<String>,
    pub source: TypeSource,
}

#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub name: String,
    pub namespace: Option<String>,
    pub return_type: String,
    pub params: Vec<ParamInfo>,
    /// The database's own declaration text (`bool Button(const string&in
    /// label)`), when the source format carries one. Kept verbatim because
    /// it is the only place `const` on a handle parameter survives — the
    /// `typeflags` bits do not encode it — so hover renders the game's exact
    /// signature instead of a lossy reconstruction (B033). `None` for the
    /// Nadeo half of the database, which stores no declaration text.
    pub decl: Option<String>,
    pub doc: Option<String>,
    pub source: TypeSource,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub return_type: String,
    pub params: Vec<ParamInfo>,
    pub is_const: bool,
    /// Declaration text as stored by the database — see [`FunctionInfo::decl`].
    pub decl: Option<String>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PropertyInfo {
    pub name: String,
    pub type_name: String,
    pub doc: Option<String>,
    /// True when the typedb lists a `set_X` accessor (or a bare prop,
    /// which is a real field and always writable). False for get-only
    /// virtual properties (`iso4.get_tx` with no `set_tx`) — the game
    /// rejects writes with `The property has no set accessor`.
    pub writable: bool,
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: Option<String>,
    pub type_name: String,
    pub default: Option<String>,
    /// `&in` reference parameter (typedb `typeflags` bit 1) (B027).
    pub is_in: bool,
    /// `&out` reference parameter (typedb `typeflags` bit 2): only an
    /// l-value argument can bind (GH #47).
    pub is_out: bool,
    /// `&inout` reference parameter (typedb `typeflags` bits 1|2) (B027).
    pub is_inout: bool,
    /// `typeflags` bit 3 (value 4): the parameter is const-qualified. Kept so
    /// the no-`decl` rendering path (the Nadeo half, and any Core entry that
    /// omits `decl`) can still show `const T &in` rather than `T &in` (B033).
    pub is_const: bool,
}

#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub name: String,
    pub namespace: Option<String>,
    pub values: Vec<(String, i64)>,
    pub source: TypeSource,
    /// B016: the engine dump declares this enum inline on a member and gives
    /// it no name of its own, so its qualified key is *synthesized* from the
    /// declaring context (`Module::Class::Member`) rather than read from the
    /// DB. Such a key names nothing the game will accept in source, so it is
    /// kept out of every source-facing lookup, completion, and namespace
    /// query — see `lookup_source_enum` and `build_short_type_index`.
    pub anonymous: bool,
}

/// A Core `funcdef` — a named function-pointer type that can be constructed
/// from a compatible function (`UI::InputTextCallback(fn)`).
#[derive(Debug, Clone)]
pub struct FuncdefInfo {
    pub name: String,
    pub namespace: Option<String>,
    pub return_type: String,
    pub params: Vec<ParamInfo>,
    /// Declaration text as stored by the database — see [`FunctionInfo::decl`].
    pub decl: Option<String>,
    pub doc: Option<String>,
    pub source: TypeSource,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TypeSource {
    Core,
    Nadeo,
    /// `Openplanet.h` — only ever a module-level enum the dump's shape
    /// cannot express (B046). Never a type, so the `is_nadeo_type` /
    /// `nadeo_member_list_trusted` member-completeness rules, which ask
    /// about types, are unaffected by this variant.
    Header,
}

/// Version stamps + top-level shape of a type-DB JSON file (GH #59).
///
/// Both Openplanet DB formats carry a top-level `op` build string; only the
/// Nadeo game dump carries `mp` (its own dump timestamp). Nothing else is
/// deserialized — [`TypeDbMeta::probe`] reads the top level as an untyped
/// `Value`, so a file the strict loaders would reject still yields a usable
/// version stamp (and a shape the guard can reject with a real reason).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeDbMeta {
    /// `op` — the Openplanet build this DB was dumped from
    /// (`"1.29.5 (next, Public, 1234ad9e)"` in Core, `"1.29.5"` in the dump).
    /// `None` when absent or not a string.
    pub op: Option<String>,
    /// `mp` — the game dump timestamp. Nadeo format only; `None` for Core.
    pub mp: Option<String>,
    /// Top-level entry counts (the fixture shape guard's input).
    pub counts: TypeDbCounts,
}

/// Top-level entry counts of a type-DB file. The Core format populates
/// `functions`/`classes`/`enums`/`funcdefs`/`props`; the Nadeo dump
/// populates `ns_modules` plus `classes` (summed across every module).
///
/// `props` are not part of [`TypeIndex`] — counting them here is how the
/// pipeline notices drift in entries the checker never loads. `funcdefs`
/// are indexed for lookup (GH #63); the count still flags harvest drift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypeDbCounts {
    pub functions: usize,
    pub classes: usize,
    pub enums: usize,
    pub funcdefs: usize,
    pub props: usize,
    pub ns_modules: usize,
}

impl TypeDbMeta {
    /// Read the version stamps and top-level counts of `path`.
    ///
    /// Errors name the path: unreadable file, or JSON that does not parse
    /// (including a truncated harvest, which must never look like an empty
    /// DB). A well-formed file with no `op` is *not* an error — the shape
    /// guard reports that, so `status` can still show what it found.
    pub fn probe(path: &Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        let value: serde_json::Value = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))?;
        Ok(Self::from_value(&value))
    }

    fn from_value(value: &serde_json::Value) -> Self {
        let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let array_len = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .map_or(0, Vec::len)
        };
        let ns = value.get("ns").and_then(|v| v.as_object());
        Self {
            op: text("op"),
            mp: text("mp"),
            counts: TypeDbCounts {
                functions: array_len("functions"),
                // Core lists classes flat; the Nadeo dump nests them per module.
                classes: array_len("classes")
                    + ns.map_or(0, |modules| {
                        modules
                            .values()
                            .map(|types| types.as_object().map_or(0, serde_json::Map::len))
                            .sum()
                    }),
                enums: array_len("enums"),
                funcdefs: array_len("funcdefs"),
                props: array_len("props"),
                ns_modules: ns.map_or(0, serde_json::Map::len),
            },
        }
    }
}

impl Default for TypeIndex {
    fn default() -> Self {
        Self {
            types: HashMap::new(),
            functions: HashMap::new(),
            enums: HashMap::new(),
            funcdefs: HashMap::new(),
            short_type_index: HashMap::new(),
            scopes: HashSet::new(),
            global_enum_values: HashMap::new(),
            global_enum_names: HashMap::new(),
            source_names: HashMap::new(),
        }
    }
}

/// Put enum members in a stable order: by value, then by name.
///
/// The Core dump stores members in a `HashMap`, so sorting by value alone
/// left aliases that share a value in whatever order the map yielded —
/// `UI::Plot::Marker` declares both `Auto = -1` and `None = -1`, and hover
/// could list them either way round between loads (B033).
pub(crate) fn sort_enum_values(values: &mut [(String, i64)]) {
    values.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
}

impl TypeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the type database from a co-located set of Openplanet files.
    ///
    /// `Openplanet.h` is read from the same directory as the game dump, and
    /// is OPTIONAL: it supplies only the module-level enums the dump's shape
    /// cannot express (B046), so an install without it loads exactly as it
    /// did before. Deriving the path here rather than taking a third
    /// argument keeps every existing caller — `--typedb-dir`, `typedb
    /// diff`, the server, the tests — reading the header for free, since
    /// all of them already point at a directory holding both JSONs.
    pub fn load(core_path: &Path, nadeo_path: &Path) -> Result<Self, String> {
        let header_path = nadeo_path.parent().map(|d| d.join(HEADER_FILENAME));
        let header = header_path
            .filter(|p| p.is_file())
            .and_then(|path| HeaderDatabase::load_from_file(&path).ok());
        let mut index = Self::load_with_header(core_path, nadeo_path, None)?;
        if let Some(header) = header {
            index.add_header(&header);
            index.build_short_type_index();
        }
        Ok(index)
    }

    /// [`Self::load`] with the header named explicitly. `None` skips it.
    ///
    /// A header that fails to READ is an error the caller sees; normal
    /// [`Self::load`] treats its co-located optional header as best-effort.
    /// A header
    /// that parses to nothing is not, because the parser degrades to an
    /// empty result rather than guessing (see [`HeaderDatabase::parse`]).
    pub fn load_with_header(
        core_path: &Path,
        nadeo_path: &Path,
        header_path: Option<&Path>,
    ) -> Result<Self, String> {
        let mut index = Self::new();
        let core = CoreDatabase::load_from_file(core_path)?;
        index.add_core(&core);
        let nadeo = NadeoDatabase::load_from_file(nadeo_path)?;
        index.add_nadeo(&nadeo);
        if let Some(path) = header_path {
            let header = HeaderDatabase::load_from_file(path)?;
            index.add_header(&header);
        }
        index.build_short_type_index();
        Ok(index)
    }

    /// Rebuild the short-name → FQN index. Must be called after every
    /// `add_*` path once all insertions are done.
    fn build_short_type_index(&mut self) {
        self.short_type_index.clear();
        // Anonymous member enums (B016) are deliberately excluded: their key
        // is synthesized from the declaring context, so admitting it here
        // would invent 117 new resolvable short names and hand `Type` to
        // `Control::CControlEntry::Type` instead of `Json::Type`.
        let mut names: Vec<String> = self
            .types
            .keys()
            .chain(
                self.enums
                    .iter()
                    .filter(|(_, info)| !info.anonymous)
                    .map(|(qname, _)| qname),
            )
            .chain(self.funcdefs.keys())
            .cloned()
            .collect();
        names.sort();
        for qname in names {
            let short = qname
                .rsplit("::")
                .next()
                .unwrap_or(qname.as_str())
                .to_string();
            self.short_type_index.entry(short).or_default().push(qname);
        }
        self.build_scope_index();
        self.build_global_enum_value_index();
        self.build_source_name_index();
    }

    /// Rebuild [`Self::scopes`] — every namespace-like scope the GAME has.
    ///
    /// Two sources, and the difference between them is the whole point:
    ///
    /// * **Core namespaces.** `UI`, `Meta`, `Json`, `IO`, `nvg`, `UI::Font`,
    ///   … are declared by the Core database and registered in the engine as
    ///   real AngelScript namespaces.
    ///
    /// * **Engine classes that declare an enum.** Openplanet registers a
    ///   class's enums by setting the default namespace to the class name, so
    ///   declaring one (named or anonymous — either is enough) is what makes
    ///   a class a namespace. `CGameMenu` and `CGameCtnApp` are; the game
    ///   accepts `CGameMenu::VirtualKey`. `CSceneVehicleVis`, `CTrackMania`,
    ///   `CMwNod` and `CSystemFidFile` declare none and the game rejects the
    ///   same spelling. The class is spelled WITHOUT the module prefix,
    ///   because that prefix is not part of its name.
    ///
    /// What is deliberately NOT here: the Nadeo `ns` MODULE names this crate
    /// keys the dump under — `Game`, `Scene`, `Plug`, `System`, `Hms`, …
    /// The game has no such namespaces and answers `Namespace 'Game' doesn't
    /// exist.` for every path through one. Their module-level enums are
    /// global, so they contribute an empty scope path and drop out.
    ///
    /// Nor namespaces that only some *other* installed plugin declares: a
    /// live probe sees `MLFeed::` and `MathX::` resolve purely because those
    /// plugins are installed on that machine, which is an environment fact
    /// and not an engine one.
    fn build_scope_index(&mut self) {
        self.scopes.clear();
        for info in self.types.values().filter(|i| i.source == TypeSource::Core) {
            if let Some(ns) = &info.namespace {
                self.scopes.insert(ns.clone());
            }
        }
        for overloads in self.functions.values() {
            for f in overloads.iter().filter(|f| f.source == TypeSource::Core) {
                if let Some(ns) = &f.namespace {
                    self.scopes.insert(ns.clone());
                }
            }
        }
        for info in self
            .funcdefs
            .values()
            .filter(|i| i.source == TypeSource::Core)
        {
            if let Some(ns) = &info.namespace {
                self.scopes.insert(ns.clone());
            }
        }
        for (qname, info) in &self.enums {
            match info.source {
                // A Core enum's own namespace is a real namespace.
                TypeSource::Core => {
                    if let Some(ns) = &info.namespace {
                        self.scopes.insert(ns.clone());
                    }
                }
                // A Nadeo enum's key is `Module::Scope…::Name`. Drop the
                // module — an invention of this crate — and whatever is left
                // is the class scope the game registered the enum under.
                // A module-level enum leaves nothing, which is correct: it
                // is global and its module is not a scope.
                TypeSource::Nadeo => {
                    let mut segments: Vec<&str> = qname.split("::").collect();
                    segments.pop();
                    if segments.len() > 1 {
                        self.scopes.insert(segments[1..].join("::"));
                    }
                }
                // Header enums are module-level and carry no namespace.
                TypeSource::Header => {}
            }
        }
        // Declaring `A::B` declares `A` as well, so every ANCESTOR of a
        // scope is a scope. Without this the LSP rejected
        // `Internal::VirtualKey` — the Core database only ever names
        // `Internal::NadeoServices`, never `Internal` on its own — while the
        // game accepts it, which is a false POSITIVE and the damaging
        // direction. Probed both ways: `Internal::VirtualKey` and
        // `NSysCfgVision::VirtualKey` compile, and each answers `Identifier
        // 'ZZZNoSuch' is not a data type in namespace '<ancestor>' or parent`
        // for a name that is genuinely absent, which is the reply only a
        // scope that EXISTS gives.
        let ancestors: Vec<String> = self
            .scopes
            .iter()
            .flat_map(|path| {
                let segments: Vec<&str> = path.split("::").collect();
                (1..segments.len()).map(move |n| segments[..n].join("::"))
            })
            .collect();
        self.scopes.extend(ancestors);
        self.scopes.remove("");
    }

    /// True when the game puts this enum in the GLOBAL namespace, so its
    /// values are reachable by a bare name (B053).
    ///
    /// Exactly the enums with no enclosing scope once the loader's synthetic
    /// Nadeo module prefix is dropped:
    ///
    /// * `Openplanet.h` enums — module-level by construction (B046);
    /// * a Nadeo key with ONE module segment (`Game::EGhostNameLogoType`).
    ///   The module is this crate's invention, so the enum is global;
    /// * a Core enum with no namespace (`VirtualKey`).
    ///
    /// And nothing else. A NESTED Nadeo enum (`Game::CGameMenu::EMenuOrder`)
    /// lives in the `CGameMenu` namespace and a Core enum with a namespace
    /// (`UI::Cond`) lives in `UI`; the game answers `No matching symbol` for
    /// a bare reference to either one's values.
    ///
    /// Two enums of the same NAME under different modules are one engine
    /// enum the loader keyed twice (`Hms::EHmsLightMapQuality` and
    /// `MetaNotPersistent::EHmsLightMapQuality`), which is why callers count
    /// distinct names rather than keys.
    fn enum_is_global(qname: &str, info: &EnumInfo) -> bool {
        if info.anonymous {
            return false;
        }
        match info.source {
            TypeSource::Header => true,
            TypeSource::Nadeo => qname.matches("::").count() <= 1,
            TypeSource::Core => info.namespace.is_none(),
        }
    }

    /// Rebuild [`Self::global_enum_values`].
    ///
    /// Ground truth: all 2989 bare value names of the shipped DB's named
    /// enums were submitted to the live game (RemoteBuild, Trackmania +
    /// Openplanet 1.29.5, 2026-08-30). The rule below predicted 2987 of
    /// them; the 2 it missed (`Main`, `RGBAColor`) resolve to a non-enum
    /// symbol, so they are not counterexamples.
    ///
    ///   0 owners → `No matching symbol`          (2373 names)
    ///   1 owner  → accepted                      ( 592 names)
    ///  ≥2 owners → `Found multiple matching enum values` (22 names)
    fn build_global_enum_value_index(&mut self) {
        self.global_enum_values.clear();
        self.global_enum_names.clear();
        let mut keys: Vec<&String> = self.enums.keys().collect();
        keys.sort();
        let ordered: Vec<String> = keys.into_iter().cloned().collect();
        for qname in &ordered {
            let info = &self.enums[qname];
            if !Self::enum_is_global(qname, info) {
                continue;
            }
            let name = qname.rsplit("::").next().unwrap_or(qname).to_string();
            // Deterministic on collision, like `build_short_type_index`: the
            // loader keys one engine enum under several modules, so
            // `Hms::EHmsLightMapQuality` and
            // `MetaNotPersistent::EHmsLightMapQuality` are one enum.
            self.global_enum_names
                .entry(name.clone())
                .or_insert_with(|| qname.clone());
            for (value, _) in info.values.clone() {
                let owners = self.global_enum_values.entry(value).or_default();
                if !owners.contains(&name) {
                    owners.push(name.clone());
                }
            }
        }
        for owners in self.global_enum_values.values_mut() {
            owners.sort();
        }
    }

    /// The spelling SOURCE must use for the entry keyed `qname` (B056), or
    /// `None` when the key is not source-writable at all.
    ///
    /// The index key is not the game's name for a type, and the difference is
    /// asymmetric by origin — which is the whole reason this is a function
    /// branching on [`TypeSource`] rather than segment arithmetic:
    ///
    /// * **Nadeo.** The dump is keyed `Module::Rest`, and the module —
    ///   `Game`, `Scene`, `Plug`, `Audio`, … — is this crate's invention. The
    ///   game has no such namespace and says so, so the module is DROPPED:
    ///   `Audio::CAudioScriptManager::ELibSound` is written
    ///   `CAudioScriptManager::ELibSound`, and
    ///   `Meta::NPlugAnim::EExtractMotionMode` is written
    ///   `NPlugAnim::EExtractMotionMode`.
    /// * **Core.** The namespace is real and must be written in FULL:
    ///   `UI::ButtonFlags`, `nvg::Align`, `UI::Plot::Marker`. Dropping a
    ///   segment here is exactly the bug — the game answers `Namespace 'Plot'
    ///   doesn't exist.` for `Plot::Marker`.
    /// * **Header.** `Openplanet.h` module-level enums (B046) are keyed by
    ///   their bare name already, with no namespace, and the game takes them
    ///   bare and rejects every qualifier. They get their own arm rather than
    ///   falling through the Nadeo one: a single-segment key sent through
    ///   "drop the leading module" would yield the empty string.
    ///
    /// Every one of those spellings is a recorded game verdict in
    /// `docs/probes/b052-qualified-enum-spellings.tsv`, both directions.
    fn source_name_of(qname: &str, source: TypeSource) -> Option<String> {
        match source {
            TypeSource::Core => Some(qname.to_string()),
            TypeSource::Header => {
                // Keyed bare by construction (B046). Never route this through
                // the Nadeo arm.
                debug_assert!(
                    !qname.contains("::"),
                    "a header entry must be keyed by its bare name, got `{qname}`"
                );
                Some(qname.to_string())
            }
            TypeSource::Nadeo => {
                let (module, rest) = qname.split_once("::")?;
                // A key with no module is not a shape the dump produces; if
                // one ever appears, dropping its only segment would leave
                // nothing, so refuse rather than invent an empty name.
                if module.is_empty() || rest.is_empty() {
                    return None;
                }
                Some(rest.to_string())
            }
        }
    }

    /// Rebuild [`Self::source_names`].
    ///
    /// Anonymous enums are excluded for the same reason they are excluded
    /// from `build_short_type_index`: their key is synthesized from the
    /// declaring context and names nothing source can write.
    fn build_source_name_index(&mut self) {
        self.source_names.clear();
        let mut entries: Vec<(String, TypeSource)> = self
            .types
            .iter()
            .map(|(k, i)| (k.clone(), i.source))
            .chain(
                self.enums
                    .iter()
                    .filter(|(_, i)| !i.anonymous)
                    .map(|(k, i)| (k.clone(), i.source)),
            )
            .chain(self.funcdefs.iter().map(|(k, i)| (k.clone(), i.source)))
            .collect();
        // Sorted so a source name owned by several keys resolves to the
        // lexicographically first, exactly as `build_short_type_index` does.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (qname, source) in entries {
            if let Some(name) = Self::source_name_of(&qname, source) {
                self.source_names.entry(name).or_insert(qname);
            }
        }
    }

    /// B056: the index key of the GLOBALLY VISIBLE enum named `name`, or
    /// `None` when no globally visible enum answers to it.
    ///
    /// This is the second half of AngelScript's `P::T` rule (B052): once `P`
    /// is known to be a real scope, the walk out to the global namespace
    /// finds `T` exactly when `T` is one of these.
    pub fn global_enum_key(&self, name: &str) -> Option<&str> {
        self.global_enum_names.get(name).map(|s| s.as_str())
    }

    /// B056: is `spelling` an index key that is ALSO its own source name?
    ///
    /// True for a Core entry, whose namespace is real and whose key the game
    /// accepts verbatim (`UI::Cond`, `Reflection::MwClassInfo`), and for a
    /// header enum. False for every Nadeo key, because the key carries the
    /// loader's synthetic module and the game has no such namespace.
    ///
    /// This is the NARROWING half on its own, and `has_type` wants exactly
    /// it: a bare Nadeo class name like `CGameCtnApp` is a real spelling, but
    /// it reaches the resolver through `resolve_unqualified`'s short-name
    /// rescue, not through here. Widening `has_type` to source names instead
    /// would admit `Bool`, `Real`, `Int8` and the rest of `MwFoundations`,
    /// which the dump carries but AngelScript never registers — the game
    /// answers `No matching symbol 'Bool'`, not `Expression 'Bool' is a data
    /// type` as it does for a type it really has.
    pub fn spelling_is_source_name(&self, spelling: &str) -> bool {
        self.source_names.get(spelling).map(|k| k.as_str()) == Some(spelling)
    }

    /// B056: is the entry keyed `qname` an ENUM? Used by the resolver's
    /// short-name rescues, which narrow for enums and leave classes alone.
    pub fn key_is_source_enum(&self, qname: &str) -> bool {
        self.lookup_source_enum(qname).is_some()
    }

    /// B056: the index key for the source spelling `spelling`, or `None` when
    /// no source-writable entry answers to it.
    ///
    /// This is the lookup a qualified TYPE reference goes through. It is not
    /// [`Self::lookup_type`] / [`Self::lookup_source_enum`], which take an
    /// index KEY and stay the right call for data access once a name has
    /// already resolved.
    pub fn source_name_key(&self, spelling: &str) -> Option<&str> {
        self.source_names.get(spelling).map(|s| s.as_str())
    }

    /// Every exact source spelling accepted by [`Self::source_name_key`], in
    /// deterministic order. Unlike [`Self::all_short_names`], these are
    /// complete spellings: a nested Nadeo enum is
    /// `CGameMenu::EMenuOrder`, never the rejected bare `EMenuOrder`, while a
    /// Core name keeps its full real namespace and a header enum stays bare.
    pub fn source_spellings(&self) -> Vec<String> {
        let mut names: Vec<String> = self.source_names.keys().cloned().collect();
        names.sort();
        names
    }

    /// Resolve a source-written type reference to its storage key.
    ///
    /// In addition to exact source names, this preserves B052's AngelScript
    /// ancestor walk: `P::T` can name a globally visible enum `T` when `P` is
    /// a real source scope. Synthetic Nadeo modules never pass that gate.
    pub fn source_reference_key(&self, spelling: &str) -> Option<&str> {
        if let Some(key) = self.source_name_key(spelling) {
            return Some(key);
        }
        let (scope, name) = spelling.rsplit_once("::")?;
        self.scope_exists(scope)
            .then(|| self.global_enum_key(name))
            .flatten()
    }

    /// B053: the globally visible enums that carry the bare value name
    /// `value`, by enum name and sorted. Empty when nothing global carries
    /// it; TWO OR MORE is what the game rejects with `Found multiple
    /// matching enum values`.
    pub fn global_enum_value_owners(&self, value: &str) -> &[String] {
        self.global_enum_values
            .get(value)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// B052: does `path` name a scope the game will resolve a qualified name
    /// through? See [`Self::build_scope_index`] for what counts.
    ///
    /// This is NOT [`Self::namespace_exists`], which answers the looser "does
    /// the index know this prefix at all" and is true for the synthetic Nadeo
    /// module names precisely because the index keys them that way.
    pub fn scope_exists(&self, path: &str) -> bool {
        self.scopes.contains(path)
    }

    /// Every real source scope, in deterministic order. This is the
    /// completion-facing counterpart to [`Self::scope_exists`]: Core
    /// namespaces and Nadeo enum-declaring class scopes are included, while
    /// loader-only modules such as `Game`, `Scene` and `Audio` are not.
    pub fn source_scopes(&self) -> Vec<String> {
        let mut scopes: Vec<String> = self.scopes.iter().cloned().collect();
        scopes.sort();
        scopes
    }

    /// Direct source-writable members of a real scope, in deterministic
    /// order. Names are derived from the same origin-aware spellings used by
    /// the resolver, so storage prefixes cannot leak into editor output.
    pub fn source_scope_members(&self, scope: &str) -> Vec<String> {
        if !self.scope_exists(scope) {
            return Vec::new();
        }

        let prefix = format!("{}::", scope);
        let mut members = BTreeSet::new();
        for spelling in self.source_names.keys() {
            if let Some(rest) = spelling.strip_prefix(&prefix) {
                if !rest.contains("::") {
                    members.insert(rest.to_string());
                }
            }
        }
        for (qname, overloads) in &self.functions {
            for info in overloads {
                let Some(spelling) = Self::source_name_of(qname, info.source) else {
                    continue;
                };
                if let Some(rest) = spelling.strip_prefix(&prefix) {
                    if !rest.contains("::") {
                        members.insert(rest.to_string());
                    }
                }
            }
        }
        for nested in &self.scopes {
            if let Some(rest) = nested.strip_prefix(&prefix) {
                if !rest.contains("::") {
                    members.insert(rest.to_string());
                }
            }
        }
        members.into_iter().collect()
    }

    /// The fully qualified enum key `qualified` is a genuine trailing PATH
    /// of, if there is one (B052). `CGameMenu::EMenuOrder` finds
    /// `Game::CGameMenu::EMenuOrder`; `Bogus::EMenuOrder` finds nothing.
    ///
    /// A trailing path, not a trailing SEGMENT: the leading part still has to
    /// be the enum's real declaring scope, which is what the game checks.
    pub fn lookup_source_enum_by_suffix(&self, qualified: &str) -> Option<&str> {
        if !qualified.contains("::") {
            return None;
        }
        let short = qualified.rsplit("::").next()?;
        let needle = format!("::{}", qualified);
        self.find_by_short_name(short)
            .iter()
            .find(|key| key.ends_with(&needle) && self.lookup_source_enum(key).is_some())
            .map(|key| key.as_str())
    }

    /// Return all fully qualified names of types/enums whose tail segment
    /// matches `short`. Used as a last-resort fallback when an unqualified
    /// reference fails to resolve under any active namespace prefix.
    ///
    /// The slice is empty when the short name is unknown. Callers should
    /// prefer the first match, which is the lexicographically first qualified
    /// name when the tail is ambiguous.
    pub fn find_by_short_name(&self, short: &str) -> &[String] {
        self.short_type_index
            .get(short)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Return every short name registered in the type/enum short-name index.
    ///
    /// Used by "did you mean" quick-fixes to enumerate known external type and
    /// enum names for distance comparisons. Does not include global functions.
    pub fn all_short_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.short_type_index.keys().cloned().collect();
        names.sort();
        names
    }

    fn qualify(ns: &Option<String>, name: &str) -> String {
        match ns {
            Some(ns) if !ns.is_empty() => format!("{}::{}", ns, name),
            _ => name.to_string(),
        }
    }

    pub(crate) fn add_core(&mut self, db: &CoreDatabase) {
        for func in &db.functions {
            let qname = Self::qualify(&func.ns, &func.name);
            let info = FunctionInfo {
                name: func.name.clone(),
                namespace: func.ns.clone(),
                return_type: func.returntypedecl.clone(),
                params: func
                    .args
                    .iter()
                    .map(|a| ParamInfo {
                        name: a.name.clone(),
                        type_name: a.typedecl.clone(),
                        default: a.default.clone(),
                        // typedb typeflags: bit 1 `&in`, bit 2 `&out`,
                        // both = `&inout` (B027); bit 4 is const.
                        is_in: a.typeflags & 0b1 != 0,
                        is_out: a.typeflags & 0b10 != 0,
                        is_inout: a.typeflags & 0b11 == 0b11,
                        is_const: a.typeflags & 0b100 != 0,
                    })
                    .collect(),
                decl: func.decl.clone(),
                doc: func.desc.clone(),
                source: TypeSource::Core,
            };
            self.functions.entry(qname).or_default().push(info);
        }

        for cls in &db.classes {
            let qname = Self::qualify(&cls.ns, &cls.name);
            let mut info = TypeInfo {
                name: cls.name.clone(),
                namespace: cls.ns.clone(),
                parent: cls.inherits.clone(),
                methods: cls
                    .methods
                    .iter()
                    .map(|m| MethodInfo {
                        name: m.name.clone(),
                        return_type: m.returntypedecl.clone(),
                        params: m
                            .args
                            .iter()
                            .map(|a| ParamInfo {
                                name: a.name.clone(),
                                type_name: a.typedecl.clone(),
                                default: a.default.clone(),
                                is_in: a.typeflags & 0b1 != 0,
                                is_out: a.typeflags & 0b10 != 0,
                                is_inout: a.typeflags & 0b11 == 0b11,
                                is_const: a.typeflags & 0b100 != 0,
                            })
                            .collect(),
                        is_const: m.is_const,
                        decl: m.decl.clone(),
                        doc: m.desc.clone(),
                    })
                    .collect(),
                properties: cls
                    .props
                    .iter()
                    .map(|p| PropertyInfo {
                        name: p.name.clone(),
                        type_name: p.typedecl.clone(),
                        doc: p.desc.clone(),
                        // Explicit props are real fields — always writable.
                        writable: true,
                    })
                    .collect(),
                ctor_conversions: single_arg_ctor_sources(cls),
                doc: cls.desc.clone(),
                source: TypeSource::Core,
            };
            // Get-only virtual properties: a `get_X` method with no matching
            // `set_X` synthesizes a read-only PropertyInfo entry so the
            // checker can reject `obj.X = …` / `obj.X += …` with the game's
            // `The property has no set accessor` (iso4.tx et al.).
            let has_set: std::collections::HashSet<&str> = cls
                .methods
                .iter()
                .filter_map(|m| m.name.strip_prefix("set_"))
                .collect();
            for m in &cls.methods {
                if let Some(prop) = m.name.strip_prefix("get_") {
                    if !prop.is_empty()
                        && !has_set.contains(prop)
                        && !info.properties.iter().any(|p| p.name == prop)
                    {
                        info.properties.push(PropertyInfo {
                            name: prop.to_string(),
                            type_name: m.returntypedecl.clone(),
                            doc: m.desc.clone(),
                            writable: false,
                        });
                    }
                }
            }
            self.types.insert(qname, info);
        }

        for en in &db.enums {
            let qname = Self::qualify(&en.ns, &en.name);
            let mut values: Vec<_> = en.values.iter().map(|(k, v)| (k.clone(), v.v)).collect();
            sort_enum_values(&mut values);
            self.enums.insert(
                qname,
                EnumInfo {
                    name: en.name.clone(),
                    namespace: en.ns.clone(),
                    values,
                    source: TypeSource::Core,
                    anonymous: false,
                },
            );
        }

        for fd in &db.funcdefs {
            let qname = Self::qualify(&fd.ns, &fd.name);
            self.funcdefs.insert(
                qname,
                FuncdefInfo {
                    name: fd.name.clone(),
                    namespace: fd.ns.clone(),
                    return_type: fd.returntypedecl.clone(),
                    params: fd
                        .args
                        .iter()
                        .map(|a| ParamInfo {
                            name: a.name.clone(),
                            type_name: a.typedecl.clone(),
                            default: a.default.clone(),
                            is_in: a.typeflags & 0b1 != 0,
                            is_out: a.typeflags & 0b10 != 0,
                            is_inout: a.typeflags & 0b11 == 0b11,
                            is_const: a.typeflags & 0b100 != 0,
                        })
                        .collect(),
                    decl: fd.decl.clone(),
                    doc: fd.desc.clone(),
                    source: TypeSource::Core,
                },
            );
        }
        // Rebuild the derived indexes, exactly as `add_nadeo` does. `load`
        // calls this first and `add_nadeo` second, so this used to be
        // redundant there and absent everywhere else — an index built from
        // Core alone had an empty short-name index, no scopes, no global enum
        // tables and (B056) no source names, so nothing resolved through any
        // of them. Harmless while every source-facing lookup read `types` and
        // `enums` directly; a silent footgun once they did not.
        self.build_short_type_index();
    }

    fn add_nadeo(&mut self, db: &NadeoDatabase) {
        // Deterministic registration order (GH #59): iterating the `ns`/type
        // HashMaps directly made the loaded index — and anything derived
        // from it — differ between two loads of the *same* file wherever a
        // key was contested. Sorting both levels costs one 20-key and one
        // per-module sort and makes `TypeIndex` a function of the file's
        // contents again. It also fixes the probe order in
        // `register_anon_member_enum` below.
        let mut module_names: Vec<&String> = db.ns.keys().collect();
        module_names.sort();
        for ns_name in module_names {
            let types = &db.ns[ns_name];
            let mut type_names: Vec<&String> = types.keys().collect();
            type_names.sort();
            for type_name in type_names {
                let nadeo_type = &types[type_name];
                let qname = format!("{}::{}", ns_name, type_name);
                let mut methods = Vec::new();
                let mut properties = Vec::new();

                // Anonymous member enums are registered *after* this type's
                // named nested enums, so a nested `enum` block always keeps
                // the plain `Module::Class::Name` key (B016).
                let mut anon_member_enums: Vec<(String, Vec<(String, i64)>)> = Vec::new();

                for (ordinal, member) in nadeo_type.m.iter().enumerate() {
                    match member.kind() {
                        NadeoMemberKind::Property => {
                            properties.push(PropertyInfo {
                                name: member.n.clone(),
                                type_name: member.type_name().unwrap_or("").to_string(),
                                doc: None,
                                // Nadeo format exposes no accessor info;
                                // assume writable (conservative — we only
                                // hard-error on Core's proven get-only set).
                                writable: true,
                            });
                        }
                        NadeoMemberKind::Method => {
                            let args = member.parse_args();
                            methods.push(MethodInfo {
                                name: member.n.clone(),
                                return_type: String::new(), // Nadeo format uses type IDs
                                params: args
                                    .into_iter()
                                    .map(|(ty, name)| ParamInfo {
                                        name: Some(name),
                                        type_name: ty,
                                        default: None,
                                        is_in: false,
                                        is_out: false,
                                        is_inout: false,
                                        // The Nadeo dump carries `const` in
                                        // the signature text, not in flags.
                                        is_const: false,
                                    })
                                    .collect(),
                                is_const: false,
                                // The Nadeo dump stores no declaration text.
                                decl: None,
                                doc: None,
                            });
                        }
                        NadeoMemberKind::Enum => {
                            // Members with an inline `e` block are enum-typed
                            // *properties* (e.g. `CGameCtnBlock::Direction`).
                            // Register the enum for name resolution AND keep
                            // the property so member access returns the enum type.
                            //
                            // B016: only a member that names a real enum type
                            // can be keyed by that name. An *anonymous* inline
                            // enum carries the placeholder `UnnamedEnum`, which
                            // is shared by every anonymous enum in the module —
                            // keying by it dropped 118 of the fixture's 132 to
                            // `or_insert`. Those are keyed by their declaring
                            // context instead.
                            if let Some(values) = member_inline_enum_values(member.e.as_ref()) {
                                match member.type_name() {
                                    Some(type_name) if !is_anonymous_enum_type_name(type_name) => {
                                        self.register_nadeo_enum(
                                            qualify_nadeo_member_type_name(
                                                &db.ns, ns_name, type_name,
                                            ),
                                            values,
                                        );
                                    }
                                    _ => anon_member_enums.push((
                                        anon_member_enum_key(&qname, &member.n, ordinal),
                                        values,
                                    )),
                                }
                            }
                            if let Some(type_name) = member.type_name() {
                                properties.push(PropertyInfo {
                                    name: member.n.clone(),
                                    type_name: type_name.to_string(),
                                    doc: None,
                                    // Nadeo enum members: no accessor info —
                                    // assume writable (conservative).
                                    writable: true,
                                });
                            }
                        }
                    }
                }

                // Register nested enums declared on this type. Both
                // `CGamePlaygroundUIConfig::EUISequence` (qualified) and
                // the short name `EUISequence` are resolvable downstream
                // via the short-name index.
                if let Some(enums) = &nadeo_type.e {
                    for en in enums {
                        let nested_qname = format!("{}::{}::{}", ns_name, type_name, en.n);
                        self.register_nadeo_enum(
                            nested_qname,
                            en.v.iter()
                                .enumerate()
                                .map(|(i, v)| (v.clone(), i as i64))
                                .collect(),
                        );
                    }
                }

                for (key, values) in anon_member_enums {
                    self.register_anon_member_enum(key, values);
                }

                let info = TypeInfo {
                    name: type_name.clone(),
                    namespace: Some(ns_name.clone()),
                    parent: nadeo_type.parent.clone(),
                    methods,
                    properties,
                    // The Nadeo dump carries no behaviors — engine classes are
                    // reference types with no constructor conversions.
                    ctor_conversions: Vec::new(),
                    doc: None,
                    source: TypeSource::Nadeo,
                };
                // Don't overwrite Core types — they have richer metadata
                self.types.entry(qname).or_insert(info);
            }
        }
        self.build_short_type_index();
    }

    fn register_nadeo_enum(&mut self, qualified_name: String, values: Vec<(String, i64)>) {
        self.insert_nadeo_enum(qualified_name, values, false);
    }

    /// Register an inline member enum the dump left unnamed (B016).
    ///
    /// `preferred_key` is `Module::Class::Member`. A *named* enum already
    /// holding that key always wins — a nested `enum` block and a member
    /// can share a name — and so does an earlier anonymous member of the
    /// same name, so later claimants take `<key>#2`, `<key>#3`, … `#` cannot
    /// occur in an AngelScript identifier, which keeps the disambiguated key
    /// unmistakably synthetic. Both the probe order and the members feeding
    /// it are deterministic, so identical bytes yield identical keys.
    fn register_anon_member_enum(&mut self, preferred_key: String, values: Vec<(String, i64)>) {
        let mut key = preferred_key.clone();
        let mut nth = 1usize;
        while self.enums.contains_key(&key) {
            nth += 1;
            key = format!("{}#{}", preferred_key, nth);
        }
        self.insert_nadeo_enum(key, values, true);
    }

    /// Index the module-level enums `Openplanet.h` declares that the dump
    /// could not express (B046). Must run AFTER [`Self::add_nadeo`], whose
    /// result decides what is left to add.
    ///
    /// # The header is a superset — prefer the dump
    ///
    /// 57 of the 59 module-level enums are already indexed, because some
    /// class member is typed by them and carries the enum inline, which
    /// [`Self::add_nadeo`] keys `Module::EName`. Re-adding those under a
    /// bare key would not overwrite anything — it would DUPLICATE them under
    /// a second key, inflating `enum_count`, putting two FQNs in every
    /// affected short-name bucket, and so moving the lexicographically-first
    /// pick that `resolve_unqualified` depends on. So an enum whose name
    /// already names an indexed enum is skipped, and the dump stays the
    /// authority for everything it can reach. On OP 1.29.5 that admits
    /// exactly `ESceneCharVisNotice` and `ESceneVehicleVisEvent`.
    ///
    /// Matching on the TAIL rather than the whole key is what makes the
    /// comparison meaningful: the dump keys these `Module::EName`, and the
    /// module is precisely the part the header does not know.
    ///
    /// # No namespace
    ///
    /// These are keyed by their bare name, with `namespace: None`, because
    /// that is what the game accepts. A live RemoteBuild probe (OP 1.29.5,
    /// 2026-08-30) takes `ESceneCharVisNotice` as a type and `DoubleJump`
    /// bare, and rejects both `Scene::ESceneCharVisNotice` and
    /// `Enums::ESceneCharVisNotice` with `Namespace '…' doesn't exist.`
    /// Nothing here infers a namespace from a name prefix.
    ///
    /// # Spellable, so `anonymous: false`
    ///
    /// Unlike B016's synthesized keys, this key is a name the game accepts.
    /// The entry therefore joins every source-facing surface — the
    /// short-name index, `namespace_members`, hover, completion, and the
    /// bare-value scan behind `source_enums_iter` — which is what makes the
    /// 55 false-positive value names resolve.
    fn add_header(&mut self, db: &HeaderDatabase) {
        // This is exactly the source type-name universe: `build_short_type_index`
        // combines types, non-synthetic enums, and funcdefs. Functions are
        // intentionally absent because their names are never resolved through
        // a type-name bucket (see that method's field documentation).
        let indexed_source_type_names: std::collections::HashSet<String> = self
            .types
            .keys()
            .chain(
                self.enums
                    .iter()
                    .filter(|(_, info)| !info.anonymous)
                    .map(|(key, _)| key),
            )
            .chain(self.funcdefs.keys())
            .map(|key| key.rsplit("::").next().unwrap_or(key.as_str()).to_string())
            .collect();

        for header_enum in &db.module_enums {
            if indexed_source_type_names.contains(&header_enum.name) {
                continue;
            }
            let mut values = header_enum.values.clone();
            sort_enum_values(&mut values);
            self.enums
                .entry(header_enum.name.clone())
                .or_insert(EnumInfo {
                    name: header_enum.name.clone(),
                    namespace: None,
                    values,
                    source: TypeSource::Header,
                    anonymous: false,
                });
        }
    }

    fn insert_nadeo_enum(
        &mut self,
        qualified_name: String,
        values: Vec<(String, i64)>,
        anonymous: bool,
    ) {
        let (namespace, name) = match qualified_name.rsplit_once("::") {
            Some((ns, tail)) => (Some(ns.to_string()), tail.to_string()),
            None => (None, qualified_name.clone()),
        };
        self.enums.entry(qualified_name).or_insert(EnumInfo {
            name,
            namespace,
            values,
            source: TypeSource::Nadeo,
            anonymous,
        });
    }

    // === Query API ===

    pub fn lookup_type(&self, qualified_name: &str) -> Option<&TypeInfo> {
        self.types.get(qualified_name)
    }

    pub fn lookup_function(&self, qualified_name: &str) -> Option<&[FunctionInfo]> {
        self.functions.get(qualified_name).map(|v| v.as_slice())
    }

    /// Look up a free function by its source spelling rather than its
    /// storage key. All overloads under one key share an origin.
    pub fn lookup_source_function(&self, spelling: &str) -> Option<&[FunctionInfo]> {
        let overloads = self.lookup_function(spelling)?;
        overloads
            .iter()
            .any(|info| Self::source_name_of(spelling, info.source).as_deref() == Some(spelling))
            .then_some(overloads)
    }

    pub fn lookup_enum(&self, qualified_name: &str) -> Option<&EnumInfo> {
        self.enums.get(qualified_name)
    }

    /// Source-facing enum lookup. Anonymous member enums are keyed by
    /// declaration context solely for internal data queries; that synthetic
    /// key is never a type name AngelScript accepts in user source.
    pub fn lookup_source_enum(&self, qualified_name: &str) -> Option<&EnumInfo> {
        self.lookup_enum(qualified_name)
            .filter(|info| !info.anonymous)
    }

    /// The inline enum declared *by* `member` of engine class `class_qname`
    /// (B016). The Nadeo dump gives such an enum no name of its own, so it
    /// is keyed by its declaring context (`Module::Class::Member`) and is
    /// deliberately absent from every source-facing lookup — the values are
    /// reachable in-game only through the member, never by writing the
    /// enum's name.
    ///
    /// Walks the same `#2`, `#3`, … chain [`Self::register_anon_member_enum`]
    /// assigns, because the plain key can be owned by a *named* nested enum
    /// of the same name, and only ever returns an anonymous entry — a named
    /// enum is not "the enum this member declares". When one class declares
    /// two anonymous enums under the same member name, this returns the
    /// first; the rest are reachable via [`Self::enums_iter`].
    pub fn lookup_member_enum(&self, class_qname: &str, member: &str) -> Option<&EnumInfo> {
        let base = format!("{}::{}", class_qname, member);
        let mut key = base.clone();
        let mut nth = 1usize;
        loop {
            let info = self.enums.get(&key)?;
            if info.anonymous {
                return Some(info);
            }
            nth += 1;
            key = format!("{}#{}", base, nth);
        }
    }

    /// Look up a Core funcdef by qualified name (`UI::InputTextCallback`,
    /// `CoroutineFunc`).
    pub fn lookup_funcdef(&self, qualified_name: &str) -> Option<&FuncdefInfo> {
        self.funcdefs.get(qualified_name)
    }

    /// The synthetic MODULE names the Nadeo loader keys its entries under
    /// (`Game`, `Scene`, `Audio`, …), sorted.
    ///
    /// These are this crate's own invention, not namespaces the game has —
    /// which is the whole subject of B056. Exposed because whether a given
    /// module name is nonetheless a live AngelScript scope is a question the
    /// probe corpus answers and a test pins; see
    /// `global_scope::tests::a_module_name_is_a_scope_only_when_core_declares_it`.
    pub fn nadeo_module_names(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (key, info) in &self.types {
            if info.source == TypeSource::Nadeo {
                if let Some((module, _)) = key.split_once("::") {
                    names.insert(module.to_string());
                }
            }
        }
        for (key, info) in &self.enums {
            if info.source == TypeSource::Nadeo {
                if let Some((module, _)) = key.split_once("::") {
                    names.insert(module.to_string());
                }
            }
        }
        names.into_iter().collect()
    }

    /// Iterate all known enums by qualified name.
    pub fn enums_iter(&self) -> impl Iterator<Item = (&String, &EnumInfo)> {
        self.enums.iter()
    }

    /// Iterate only enums that have a real, source-writable name.
    pub fn source_enums_iter(&self) -> impl Iterator<Item = (&String, &EnumInfo)> {
        self.enums.iter().filter(|(_, info)| !info.anonymous)
    }

    /// Iterate all function overload sets directly under `namespace`
    /// (e.g., `UI` → `UI::Begin` but not `UI::Layout::Foo`). Used by
    /// namespace-shortcut completions.
    pub fn namespace_functions(
        &self,
        namespace: &str,
    ) -> impl Iterator<Item = (&String, &Vec<FunctionInfo>)> {
        let prefix = format!("{}::", namespace);
        self.functions.iter().filter(move |(qname, _)| {
            qname
                .strip_prefix(&prefix)
                .is_some_and(|rest| !rest.contains("::"))
        })
    }

    /// Iterate source-visible enums directly under `namespace` (qualified
    /// name → info). Used by namespace-shortcut completions when enum
    /// matching is enabled; contextual anonymous enums are intentionally
    /// excluded.
    pub fn namespace_enums(&self, namespace: &str) -> impl Iterator<Item = (&String, &EnumInfo)> {
        let prefix = format!("{}::", namespace);
        self.enums.iter().filter(move |(qname, info)| {
            !info.anonymous
                && qname
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.contains("::"))
        })
    }

    /// Get source-visible member names for namespace completion (e.g., after
    /// "UI::").
    pub fn namespace_members(&self, namespace: &str) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let prefix = format!("{}::", namespace);
        for qname in self.types.keys() {
            if let Some(name) = qname.strip_prefix(&prefix) {
                if !name.contains("::") {
                    seen.insert(name.to_string());
                }
            }
        }
        for qname in self.functions.keys() {
            if let Some(name) = qname.strip_prefix(&prefix) {
                if !name.contains("::") {
                    seen.insert(name.to_string());
                }
            }
        }
        for (qname, info) in &self.enums {
            if info.anonymous {
                continue;
            }
            if let Some(name) = qname.strip_prefix(&prefix) {
                if !name.contains("::") {
                    seen.insert(name.to_string());
                }
            }
        }
        for qname in self.funcdefs.keys() {
            if let Some(name) = qname.strip_prefix(&prefix) {
                if !name.contains("::") {
                    seen.insert(name.to_string());
                }
            }
        }
        let mut members: Vec<_> = seen.into_iter().collect();
        members.sort();
        members
    }

    /// Get all source-visible namespaces.
    pub fn namespaces(&self) -> Vec<String> {
        let mut nss: std::collections::HashSet<String> = std::collections::HashSet::new();
        for info in self.types.values() {
            if let Some(ns) = &info.namespace {
                nss.insert(ns.clone());
            }
        }
        for fns in self.functions.values() {
            for f in fns {
                if let Some(ns) = &f.namespace {
                    nss.insert(ns.clone());
                }
            }
        }
        for en in self.enums.values().filter(|en| !en.anonymous) {
            if let Some(ns) = &en.namespace {
                nss.insert(ns.clone());
            }
        }
        for fd in self.funcdefs.values() {
            if let Some(ns) = &fd.namespace {
                nss.insert(ns.clone());
            }
        }
        let mut result: Vec<_> = nss.into_iter().collect();
        result.sort();
        result
    }

    pub fn type_count(&self) -> usize {
        self.types.len()
    }

    /// GH #28: does the typedb list at least one free function under `ns`?
    /// True for API namespaces like `UI`, `Text`, `Math` — a call to a
    /// `Ns::Fn` member that isn't in the DB is then a high-confidence miss.
    pub fn namespace_has_functions(&self, ns: &str) -> bool {
        let prefix = format!("{}::", ns);
        self.functions.keys().any(|k| k.starts_with(&prefix))
    }

    /// GH #28: does the typedb know `ns` at all (functions, types, or
    /// enums)? Absent + listed in the removed-API table (e.g. `Draw` on
    /// 1.29.5) diagnoses; absent + unlisted is an unknown user namespace.
    pub fn namespace_exists(&self, ns: &str) -> bool {
        self.namespaces().iter().any(|n| n == ns)
    }

    pub fn function_count(&self) -> usize {
        self.functions.values().map(|v| v.len()).sum()
    }

    pub fn enum_count(&self) -> usize {
        self.enums.len()
    }
}

// === Fixture shape guard (GH #59) ===

/// Floors the shipped Core DB must clear. Set well below the real figures
/// (854 / 75 / 71 on Openplanet 1.29.5) so a genuine API shrink does not
/// trip them, but far above anything a truncated or partial harvest yields.
pub const MIN_CORE_FUNCTIONS: usize = 700;
pub const MIN_CORE_CLASSES: usize = 70;
pub const MIN_CORE_ENUMS: usize = 70;
/// Floor for the Nadeo engine dump (2585 classes on 1.29.5).
pub const MIN_GAME_CLASSES: usize = 2000;

/// Does `op` start with a `major.minor.patch` version?
///
/// The Core stamp carries build metadata after a space
/// (`"1.29.5 (next, Public, 1234ad9e)"`), so only the first whitespace-
/// delimited token is checked. Deliberately not a version *ordering* —
/// the stamps are compared for equality only (GH #59: no semver ordering).
fn looks_like_version(op: &str) -> bool {
    let head = op.split_whitespace().next().unwrap_or_default();
    let parts: Vec<&str> = head.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// Anti-drift guard for the type-DB files this repo ships (GH #59, goal G2).
///
/// Returns every problem found rather than the first, so one CI failure
/// tells the maintainer everything that is wrong with a bad harvest. The
/// checks are deliberately cheap and offline: version stamps, top-level
/// entry floors, and a full [`TypeIndex::load`].
///
/// The dangerous failure this exists for is not a truncated file (which
/// errors loudly) but a *well-formed empty* one: it parses, it loads, and
/// it silently disables every type-aware diagnostic.
pub fn check_typedb_shape(core_path: &Path, game_path: &Path) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();

    let core = probe_for_guard(core_path, &mut problems);
    let game = probe_for_guard(game_path, &mut problems);

    if let Some(core) = &core {
        check_op(core_path, &core.op, &mut problems);
        let counts = &core.counts;
        check_floor(
            core_path,
            "functions",
            counts.functions,
            MIN_CORE_FUNCTIONS,
            &mut problems,
        );
        check_floor(
            core_path,
            "classes",
            counts.classes,
            MIN_CORE_CLASSES,
            &mut problems,
        );
        check_floor(
            core_path,
            "enums",
            counts.enums,
            MIN_CORE_ENUMS,
            &mut problems,
        );
    }
    if let Some(game) = &game {
        check_op(game_path, &game.op, &mut problems);
        check_floor(
            game_path,
            "classes",
            game.counts.classes,
            MIN_GAME_CLASSES,
            &mut problems,
        );
    }

    // The strict loaders reject shapes `probe` tolerates, so load both for
    // real — this is what the LSP itself will do.
    if core.is_some() && game.is_some() {
        if let Err(err) = TypeIndex::load(core_path, game_path) {
            problems.push(format!("TypeIndex::load failed: {err}"));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn probe_for_guard(path: &Path, problems: &mut Vec<String>) -> Option<TypeDbMeta> {
    match TypeDbMeta::probe(path) {
        Ok(meta) => Some(meta),
        Err(err) => {
            problems.push(err);
            None
        }
    }
}

fn check_op(path: &Path, op: &Option<String>, problems: &mut Vec<String>) {
    let label = file_label(path);
    match op {
        None => problems.push(format!("{label}: missing `op` version stamp")),
        Some(op) if !looks_like_version(op) => problems.push(format!(
            "{label}: `op` is not a major.minor.patch version: {op:?}"
        )),
        Some(_) => {}
    }
}

fn check_floor(path: &Path, what: &str, actual: usize, floor: usize, problems: &mut Vec<String>) {
    if actual < floor {
        problems.push(format!(
            "{}: only {actual} {what} (expected at least {floor}) — truncated or partial harvest?",
            file_label(path)
        ));
    }
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

// === Symbol-level DB diff (GH #59) ===

/// Which DB entry a diff row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DbSymbolKind {
    Class,
    Enum,
    Function,
}

impl DbSymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Enum => "enum",
            Self::Function => "function",
        }
    }
}

/// A qualified entry present in exactly one of the two DBs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DbSymbol {
    pub kind: DbSymbolKind,
    /// Fully qualified name (`Display::GetWidth`, `MwFoundations::CMwNod`).
    pub name: String,
}

/// A qualified entry present in both DBs whose contents moved. `details`
/// is the rendered symmetric difference of its facts, `-` lines (only in
/// the old DB) before `+` lines (only in the new one).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DbChange {
    pub kind: DbSymbolKind,
    pub name: String,
    pub details: Vec<String>,
}

/// Symbol-level parity report between two loaded type databases.
///
/// Every bucket is sorted, so the same pair of DBs always produces the
/// same report (`HashMap` iteration order is not stable).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbDiff {
    pub added: Vec<DbSymbol>,
    pub removed: Vec<DbSymbol>,
    pub changed: Vec<DbChange>,
}

impl DbDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// `(added, removed, changed)` entry counts.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.added.len(), self.removed.len(), self.changed.len())
    }
}

/// Diff two loaded type databases entry by entry.
///
/// Entries are keyed by qualified name across three kinds (classes, enums,
/// function overload sets). A name in only one DB is added/removed; a name
/// in both whose *facts* differ is changed.
///
/// "Facts" are the semantics the checker actually consumes — method and
/// overload signatures (including `&out` flags and parameter defaults),
/// property types and writability, inheritance, single-argument
/// constructor conversions, and enum values. Anything the checker ignores
/// (docs, ids, descriptions) is deliberately not compared, so a doc-only
/// DB refresh reports zero drift.
///
/// One limitation the caller should know about, because it means a real
/// change can go unreported: Core's top-level `funcdefs` are loaded into
/// [`TypeIndex`] for lookup (GH #63) but are not a [`DbSymbolKind`], so they
/// are not diffed here. `props` remain count-only via [`TypeDbCounts`].
///
/// Inline engine enums the dump leaves unnamed were a second such gap — all
/// 15 of `Control`'s collapsed onto `Control::UnnamedEnum` and only the
/// first was diffed — until B016 keyed them by their declaring context.
/// Each is compared on its own now.
pub fn diff_indexes(old: &TypeIndex, new: &TypeIndex) -> DbDiff {
    let mut diff = DbDiff::default();
    diff_entries(
        DbSymbolKind::Class,
        &old.types,
        &new.types,
        class_facts,
        &mut diff,
    );
    diff_entries(
        DbSymbolKind::Enum,
        &old.enums,
        &new.enums,
        enum_facts,
        &mut diff,
    );
    diff_entries(
        DbSymbolKind::Function,
        &old.functions,
        &new.functions,
        |overloads: &Vec<FunctionInfo>| overload_facts(overloads),
        &mut diff,
    );
    diff.added.sort();
    diff.removed.sort();
    diff.changed.sort();
    diff
}

/// Diff one keyed entry map, appending into the shared buckets.
fn diff_entries<T>(
    kind: DbSymbolKind,
    old: &HashMap<String, T>,
    new: &HashMap<String, T>,
    facts: impl Fn(&T) -> BTreeSet<String>,
    diff: &mut DbDiff,
) {
    for (name, entry) in old {
        match new.get(name) {
            None => diff.removed.push(DbSymbol {
                kind,
                name: name.clone(),
            }),
            Some(other) => {
                let details = fact_delta(&facts(entry), &facts(other));
                if !details.is_empty() {
                    diff.changed.push(DbChange {
                        kind,
                        name: name.clone(),
                        details,
                    });
                }
            }
        }
    }
    for name in new.keys() {
        if !old.contains_key(name) {
            diff.added.push(DbSymbol {
                kind,
                name: name.clone(),
            });
        }
    }
}

/// Rendered symmetric difference: `- fact` for old-only, `+ fact` for
/// new-only. Empty when the two fact sets match.
fn fact_delta(old: &BTreeSet<String>, new: &BTreeSet<String>) -> Vec<String> {
    let mut out: Vec<String> = old.difference(new).map(|f| format!("- {f}")).collect();
    out.extend(new.difference(old).map(|f| format!("+ {f}")));
    out
}

/// The Nadeo dump carries no return types (it uses numeric type ids), so an
/// empty return renders as `?` on both sides rather than reading as drift.
fn render_return(return_type: &str) -> &str {
    if return_type.is_empty() {
        "?"
    } else {
        return_type
    }
}

fn render_params(params: &[ParamInfo]) -> String {
    params
        .iter()
        .map(|p| {
            let mut rendered = format!(
                "{}: {}",
                p.name.as_deref().unwrap_or("<unnamed>"),
                p.type_name
            );
            if p.is_out {
                rendered.push_str(" &out");
            }
            if let Some(default) = &p.default {
                rendered.push_str(&format!(" = {default}"));
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn class_facts(info: &TypeInfo) -> BTreeSet<String> {
    let mut facts = BTreeSet::new();
    facts.insert(format!("source {:?}", info.source));
    if let Some(parent) = &info.parent {
        facts.insert(format!("inherits {parent}"));
    }
    for method in &info.methods {
        facts.insert(format!(
            "method {}({}) -> {}{}",
            method.name,
            render_params(&method.params),
            render_return(&method.return_type),
            if method.is_const { " const" } else { "" }
        ));
    }
    for prop in &info.properties {
        facts.insert(format!(
            "prop {}: {}{}",
            prop.name,
            prop.type_name,
            if prop.writable { "" } else { " (get-only)" }
        ));
    }
    for source in &info.ctor_conversions {
        facts.insert(format!("ctor from {source}"));
    }
    facts
}

fn enum_facts(info: &EnumInfo) -> BTreeSet<String> {
    let mut facts: BTreeSet<String> = info
        .values
        .iter()
        .map(|(name, value)| format!("value {name} = {value}"))
        .collect();
    facts.insert(format!(
        "source-facing {}",
        if info.anonymous { "anonymous" } else { "named" }
    ));
    facts
}

/// One fact per overload, so an added or dropped overload of an existing
/// name reads as a change to that name rather than an add/remove.
fn overload_facts(overloads: &[FunctionInfo]) -> BTreeSet<String> {
    overloads
        .iter()
        .map(|func| {
            format!(
                "overload ({}) -> {}",
                render_params(&func.params),
                render_return(&func.return_type)
            )
        })
        .collect()
}

/// AngelScript `asBEHAVE_CONSTRUCT` — the value-type constructor behavior.
/// Reference types register `asBEHAVE_FACTORY` (3) instead, so a CONSTRUCT
/// entry is itself the value-type signal.
const AS_BEHAVE_CONSTRUCT: u32 = 0;

/// GH #54: parameter types of `cls`'s single-argument constructors — the
/// conversions AngelScript can synthesise *into* `cls`.
///
/// The copy constructor (`mat4(const mat4&in)`) is dropped: an argument of
/// the type itself already matches directly, so keeping it would let a
/// "constructor conversion" stand in for an exact match and blur the two.
fn single_arg_ctor_sources(cls: &CoreClass) -> Vec<String> {
    let self_qname = TypeIndex::qualify(&cls.ns, &cls.name);
    let mut out = Vec::new();
    for beh in &cls.behaviors {
        if beh.kind != AS_BEHAVE_CONSTRUCT {
            continue;
        }
        let Some(func) = &beh.func else { continue };
        let [arg] = &func.args[..] else { continue };
        let source = arg.typedecl.trim();
        if source.is_empty() || source == self_qname || source == cls.name {
            continue;
        }
        out.push(source.to_string());
    }
    out
}

fn member_inline_enum_values(raw: Option<&serde_json::Value>) -> Option<Vec<(String, i64)>> {
    let first: NadeoEnumEntry = serde_json::from_value(raw?.clone()).ok()?;
    Some(
        first
            .v
            .iter()
            .enumerate()
            .map(|(i, v)| (v.clone(), i as i64))
            .collect(),
    )
}

/// Does the member's declared type name identify a real enum type, or is it
/// the dump's placeholder for an enum declared inline with no name? (B016)
///
/// Every anonymous inline enum in a module carries the same `UnnamedEnum`
/// placeholder, so it names nothing and cannot be a key.
fn is_anonymous_enum_type_name(type_name: &str) -> bool {
    type_name.is_empty() || type_name == "UnnamedEnum"
}

/// Key for an inline enum the dump left unnamed: its declaring context
/// (B016). `class_qname` is `Module::Class`.
///
/// The member name is what makes the key both unique and meaningful, so it
/// is used whenever the dump supplies one. A member the dump leaves unnamed
/// cannot be identified, so it falls back to its declaration ordinal — still
/// stable across loads, since the members of a type are a JSON array.
fn anon_member_enum_key(class_qname: &str, member_name: &str, ordinal: usize) -> String {
    if member_name.is_empty() {
        format!("{}::UnnamedEnum#{}", class_qname, ordinal)
    } else {
        format!("{}::{}", class_qname, member_name)
    }
}

fn qualify_nadeo_member_type_name(
    namespaces: &std::collections::HashMap<String, std::collections::HashMap<String, NadeoType>>,
    current_namespace: &str,
    type_name: &str,
) -> String {
    if let Some((head, _)) = type_name.split_once("::") {
        if namespaces.contains_key(head) {
            return type_name.to_string();
        }
    }
    format!("{}::{}", current_namespace, type_name)
}

#[cfg(test)]
mod tests {
    /// B033: aliases sharing a value must not depend on map order.
    #[test]
    fn sort_enum_values_breaks_value_ties_by_name() {
        let mut values = vec![
            ("None".to_string(), -1i64),
            ("Circle".to_string(), 0),
            ("Auto".to_string(), -1),
            ("Square".to_string(), 0),
        ];
        super::sort_enum_values(&mut values);
        assert_eq!(
            values,
            vec![
                ("Auto".to_string(), -1),
                ("None".to_string(), -1),
                ("Circle".to_string(), 0),
                ("Square".to_string(), 0),
            ]
        );
    }

    use super::*;
    use std::path::PathBuf;

    fn core_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/OpenplanetCore.json")
    }

    fn next_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/OpenplanetNext.json")
    }

    /// B056: the invariant `global_enum_names` is named for, checked both
    /// ways over the whole shipped DB.
    ///
    /// A globally visible enum is EXACTLY one whose source name is bare. That
    /// is not a coincidence — it is the same fact stated twice, once by
    /// `enum_is_global` and once by `source_name_of` — and the two are
    /// computed by separate code that could drift apart. If it ever does, the
    /// ancestor walk starts answering for enums that need a qualifier, or
    /// stops answering for ones that do not.
    #[test]
    fn a_globally_visible_enum_is_exactly_one_with_a_bare_source_name() {
        let index = TypeIndex::load(&core_path(), &next_path()).unwrap();
        let mut global = 0usize;
        let mut bare = 0usize;
        for (qname, info) in index.enums_iter() {
            if info.anonymous {
                continue;
            }
            let is_global = TypeIndex::enum_is_global(qname, info);
            let source_is_bare =
                TypeIndex::source_name_of(qname, info.source).is_some_and(|n| !n.contains("::"));
            assert_eq!(
                is_global,
                source_is_bare,
                "`{qname}` ({:?}): globally visible = {is_global}, but its \
                 source name is {}bare",
                info.source,
                if source_is_bare { "" } else { "not " }
            );
            if is_global {
                global += 1;
                assert!(
                    index.global_enum_key(&info.name).is_some(),
                    "`{qname}` is globally visible, so its bare name must be \
                     reachable through `global_enum_key`"
                );
            }
            if source_is_bare {
                bare += 1;
            }
        }
        assert_eq!(global, bare);
        assert!(
            global > 50,
            "fixture guard: the shipped DB must carry a real global set, got {global}"
        );
    }

    /// B056: the source name follows the entry's ORIGIN, not the shape of its
    /// key. Both are pinned at every depth the shipped DB actually has,
    /// because segment arithmetic alone cannot tell these apart — a Core key
    /// and a Nadeo key of the same length want opposite treatment.
    ///
    /// Every pair below is a recorded game verdict in
    /// `docs/probes/b052-qualified-enum-spellings.tsv`, both directions.
    #[test]
    fn a_source_name_follows_the_origin_not_the_key_shape() {
        let index = TypeIndex::load(&core_path(), &next_path()).unwrap();

        for (key, source_name) in [
            // Nadeo: the module is dropped, at every depth it occurs.
            ("Game::CGameCtnApp", "CGameCtnApp"),
            ("Game::CGameMenu::EMenuOrder", "CGameMenu::EMenuOrder"),
            (
                "Meta::NPlugAnim::EExtractMotionMode",
                "NPlugAnim::EExtractMotionMode",
            ),
            (
                "Meta::EPlugAnimGraphStateSelectCond",
                "EPlugAnimGraphStateSelectCond",
            ),
            // Core: the namespace is real and survives whole. Note
            // `UI::Plot::Marker` is three segments, exactly like the Nadeo
            // nested case above, and wants the opposite answer.
            ("UI::ButtonFlags", "UI::ButtonFlags"),
            ("nvg::Align", "nvg::Align"),
            ("UI::Plot::Marker", "UI::Plot::Marker"),
            // Header: bare, and it must not travel through the Nadeo arm,
            // where a single-segment key would reduce to the empty string.
            ("ESceneCharVisNotice", "ESceneCharVisNotice"),
            ("ESceneVehicleVisEvent", "ESceneVehicleVisEvent"),
        ] {
            assert!(
                index.lookup_source_enum(key).is_some() || index.lookup_type(key).is_some(),
                "fixture guard: `{key}` must be indexed"
            );
            assert_eq!(
                index.source_name_key(source_name),
                Some(key),
                "`{source_name}` must be the source spelling of `{key}`"
            );
            if key != source_name {
                assert!(
                    !index.spelling_is_source_name(key),
                    "`{key}` is the loader's key, not a spelling the game takes"
                );
            }
        }

        // The empty-name trap, stated directly: no source name is empty, and
        // none of them still carries a leading module.
        for (qname, info) in index.enums_iter() {
            if info.anonymous {
                continue;
            }
            let name = TypeIndex::source_name_of(qname, info.source)
                .unwrap_or_else(|| panic!("`{qname}` must have a source name"));
            assert!(!name.is_empty(), "`{qname}` reduced to an empty name");
        }
    }

    #[test]
    fn test_merged_index() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();
        assert!(
            index.type_count() > 100,
            "expected many types, got {}",
            index.type_count()
        );
        assert!(
            index.function_count() > 50,
            "expected many functions, got {}",
            index.function_count()
        );
    }

    #[test]
    fn test_namespace_members_ui() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let ui_members = index.namespace_members("UI");
        assert!(!ui_members.is_empty(), "expected UI namespace members");
        assert!(
            ui_members.iter().any(|m| m == "InputTextCallback"),
            "expected UI::InputTextCallback among namespace members, got {:?}",
            ui_members
        );
    }

    #[test]
    fn core_funcdefs_are_indexed() {
        let index = TypeIndex::load(&core_path(), &next_path()).unwrap();
        let input = index
            .lookup_funcdef("UI::InputTextCallback")
            .expect("UI::InputTextCallback");
        assert_eq!(input.name, "InputTextCallback");
        assert_eq!(input.namespace.as_deref(), Some("UI"));
        assert_eq!(input.return_type, "void");
        assert_eq!(input.params.len(), 1);
        assert!(
            input.params[0].type_name.contains("InputTextCallbackData"),
            "callback data param, got {:?}",
            input.params[0].type_name
        );

        assert!(index.lookup_funcdef("CoroutineFunc").is_some());
        assert!(index.lookup_funcdef("Dev::ProcIntercept").is_some());
        assert!(index.lookup_funcdef("UI::NodTreeMemberCallback").is_some());
        assert!(index.lookup_funcdef("less_nonconst").is_some());
        assert!(index.lookup_funcdef("UI::DefinitelyNotAFuncdef").is_none());
    }

    #[test]
    fn test_find_by_short_name_nadeo_class() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let hits = index.find_by_short_name("CMwNod");
        assert!(
            hits.iter().any(|h| h.ends_with("::CMwNod")),
            "expected a ::CMwNod match, got {:?}",
            hits
        );
    }

    #[test]
    fn test_find_by_short_name_editor_free() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let hits = index.find_by_short_name("CGameCtnEditorFree");
        assert!(
            !hits.is_empty(),
            "expected at least one match for CGameCtnEditorFree"
        );
    }

    /// Short-name buckets are a resolution seam: duplicated tails must have
    /// a stable first candidate across independent loads, not HashMap order.
    #[test]
    fn short_name_buckets_and_resolution_are_sorted_across_loads() {
        let core = r#"{"op":"1.0.0","classes":[
            {"ns":"Zeta","name":"Shared"},
            {"ns":"Alpha","name":"Shared"},
            {"ns":"Middle","name":"Other"},
            {"ns":"Names","name":"Zulu"},
            {"ns":"Names","name":"Alpha"},
            {"ns":"Names","name":"Echo"},
            {"ns":"Names","name":"Bravo"},
            {"ns":"Names","name":"Kilo"},
            {"ns":"Names","name":"Delta"},
            {"ns":"Names","name":"Foxtrot"}]}
        "#;
        let load = |tag: &str| {
            let dir = std::env::temp_dir().join(format!(
                "openplanet-lsp-typedb-short-order-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("OpenplanetCore.json"), core).unwrap();
            std::fs::write(dir.join("OpenplanetNext.json"), r#"{"op":"1.0.0","ns":{}}"#).unwrap();
            TypeIndex::load(
                &dir.join("OpenplanetCore.json"),
                &dir.join("OpenplanetNext.json"),
            )
            .unwrap()
        };
        let first = load("first");
        let second = load("second");
        let expected = vec!["Alpha::Shared".to_string(), "Zeta::Shared".to_string()];

        assert_eq!(first.find_by_short_name("Shared"), expected.as_slice());
        assert_eq!(second.find_by_short_name("Shared"), expected.as_slice());
        assert_eq!(
            first.find_by_short_name("Shared").first(),
            Some(&"Alpha::Shared".to_string()),
            "the first bucket entry is GlobalScope's deterministic fallback"
        );
        assert_eq!(
            first.all_short_names(),
            vec![
                "Alpha".to_string(),
                "Bravo".to_string(),
                "Delta".to_string(),
                "Echo".to_string(),
                "Foxtrot".to_string(),
                "Kilo".to_string(),
                "Other".to_string(),
                "Shared".to_string(),
                "Zulu".to_string(),
            ],
            "the user-visible suggestion names are deterministic too"
        );
    }

    #[test]
    fn test_nested_nadeo_enum_registered() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let hits = index.find_by_short_name("EUISequence");
        assert!(
            hits.iter().any(|h| h.ends_with("::EUISequence")),
            "expected EUISequence to be registered as a nested enum, got {:?}",
            hits
        );
    }

    #[test]
    fn test_nested_enums_cgame_editor_plugin_map() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();

        let nested_enums = [
            "ECardinalDirections",
            "ECardinalDirections8",
            "ERelativeDirections",
            "EPlaceMode",
            "EditMode",
            "EShadowsQuality",
            "EValidationStatus",
            "EMapElemColor",
            "EPhaseOffset",
            "EMapElemLightmapQuality",
            "EMapElemColorPalette",
        ];

        for enum_name in nested_enums {
            let hits = index.find_by_short_name(enum_name);
            let has_qualified = hits.iter().any(|h| h.contains("CGameEditorPluginMap"));
            assert!(
                has_qualified,
                "expected {} to be found as CGameEditorPluginMap::{}, got {:?}",
                enum_name, enum_name, hits
            );
        }
    }

    #[test]
    fn test_inline_member_enums_are_registered() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();

        let direct = [
            "EPlugSurfaceMaterialId",
            "EPlugSurfaceGameplayId",
            "EGameItemWaypointType",
            "EGmSurfType",
            "NPlugDyna::EAxis",
            "CGxLightFrustum::EApply",
            "CGxLightFrustum::ETechnique",
        ];

        for enum_name in direct {
            assert!(
                index.lookup_enum(enum_name).is_some()
                    || !index.find_by_short_name(enum_name).is_empty()
                    || index.enums_iter().any(|(qname, _)| qname == enum_name
                        || qname.ends_with(&format!("::{}", enum_name))),
                "expected inline enum `{}` to be registered",
                enum_name
            );
        }
    }

    #[test]
    fn test_lookup_cmwnod() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() {
            panic!("Typedb files not found at {:?} and {:?}", cp, np);
        }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let nod = index.lookup_type("MwFoundations::CMwNod");
        assert!(nod.is_some(), "expected CMwNod type");
    }
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    /// Unique tmp dir per test tag (the pid alone repeats across tests in
    /// one binary).
    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-typedb-meta-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_db(tag: &str, name: &str, contents: &str) -> std::path::PathBuf {
        let path = tmp_dir(tag).join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb")
            .join(name)
    }

    #[test]
    fn typedb_meta_probe_core_has_op_no_mp() {
        let path = write_db(
            "core-op",
            "OpenplanetCore.json",
            r#"{"op":"1.29.5 (next, Public, 1234ad9e)","functions":[],"classes":[],"enums":[]}"#,
        );
        let meta = TypeDbMeta::probe(&path).unwrap();
        assert_eq!(meta.op.as_deref(), Some("1.29.5 (next, Public, 1234ad9e)"));
        assert_eq!(meta.mp, None);
    }

    #[test]
    fn typedb_meta_probe_next_has_op_and_mp() {
        let path = write_db(
            "next-op-mp",
            "OpenplanetNext.json",
            r#"{"op":"1.29.5","mp":"2026-02-03 03:51:19","ns":{}}"#,
        );
        let meta = TypeDbMeta::probe(&path).unwrap();
        assert_eq!(meta.op.as_deref(), Some("1.29.5"));
        assert_eq!(meta.mp.as_deref(), Some("2026-02-03 03:51:19"));
    }

    #[test]
    fn typedb_meta_probe_missing_op_is_none_but_ok() {
        let path = write_db("no-op", "OpenplanetCore.json", "{}");
        let meta = TypeDbMeta::probe(&path).unwrap();
        assert_eq!(meta.op, None);
        assert_eq!(meta.mp, None);
        assert_eq!(meta.counts, TypeDbCounts::default());
    }

    /// Non-string `op`/`mp` (a number, an object) is treated as absent
    /// rather than an error — the file still parses as JSON.
    #[test]
    fn typedb_meta_probe_non_string_op_is_none() {
        let path = write_db(
            "num-op",
            "OpenplanetCore.json",
            r#"{"op":129,"mp":{"a":1}}"#,
        );
        let meta = TypeDbMeta::probe(&path).unwrap();
        assert_eq!(meta.op, None);
        assert_eq!(meta.mp, None);
    }

    #[test]
    fn typedb_meta_probe_malformed_json_errs() {
        let path = write_db("malformed", "OpenplanetCore.json", "not json");
        let err = TypeDbMeta::probe(&path).unwrap_err();
        assert!(
            err.contains("OpenplanetCore.json"),
            "error should name the file: {err}"
        );
    }

    /// A truncated (mid-array) file is malformed JSON, not a silent empty DB.
    #[test]
    fn typedb_meta_probe_truncated_json_errs() {
        let path = write_db(
            "truncated",
            "OpenplanetCore.json",
            r#"{"op":"1.29.5","functions":[{"name":"Begin""#,
        );
        assert!(TypeDbMeta::probe(&path).is_err());
    }

    #[test]
    fn typedb_meta_probe_missing_file_errs() {
        let path = tmp_dir("missing").join("OpenplanetCore.json");
        let err = TypeDbMeta::probe(&path).unwrap_err();
        assert!(
            err.contains("OpenplanetCore.json"),
            "error should name the path: {err}"
        );
    }

    #[test]
    fn typedb_meta_probe_counts_core_shape() {
        let path = write_db(
            "core-counts",
            "OpenplanetCore.json",
            r#"{"op":"1.0.0",
                "functions":[{"name":"A"},{"name":"B"}],
                "classes":[{"name":"C"}],
                "enums":[{"name":"E"},{"name":"F"},{"name":"G"}],
                "funcdefs":[{"name":"D"}],
                "props":[{"name":"P"},{"name":"Q"}]}"#,
        );
        let counts = TypeDbMeta::probe(&path).unwrap().counts;
        assert_eq!(counts.functions, 2);
        assert_eq!(counts.classes, 1);
        assert_eq!(counts.enums, 3);
        assert_eq!(counts.funcdefs, 1);
        assert_eq!(counts.props, 2);
        assert_eq!(counts.ns_modules, 0);
    }

    /// Nadeo dump: `classes` is the total across every `ns` module.
    #[test]
    fn typedb_meta_probe_counts_next_shape() {
        let path = write_db(
            "next-counts",
            "OpenplanetNext.json",
            r#"{"op":"1.0.0","mp":"2026-01-01 00:00:00",
                "ns":{"MwFoundations":{"CMwNod":{}},
                      "GameData":{"CGameCtnBlock":{},"CGameCtnChallenge":{}}}}"#,
        );
        let counts = TypeDbMeta::probe(&path).unwrap().counts;
        assert_eq!(counts.ns_modules, 2);
        assert_eq!(counts.classes, 3);
        assert_eq!(counts.functions, 0);
    }

    /// The shipped fixtures are the canonical shape both formats must keep.
    #[test]
    fn typedb_meta_probe_reads_repo_fixtures() {
        let core = TypeDbMeta::probe(&fixture("OpenplanetCore.json")).unwrap();
        assert!(
            core.op.as_deref().unwrap_or_default().starts_with("1."),
            "core op: {:?}",
            core.op
        );
        assert_eq!(core.mp, None, "Core format carries no `mp` stamp");
        assert!(core.counts.functions > 700, "{:?}", core.counts);
        assert!(core.counts.classes > 70, "{:?}", core.counts);
        assert!(core.counts.enums > 70, "{:?}", core.counts);

        let game = TypeDbMeta::probe(&fixture("OpenplanetNext.json")).unwrap();
        assert!(game.op.is_some(), "game op missing");
        assert!(game.mp.is_some(), "game mp missing");
        assert!(game.counts.classes > 2000, "{:?}", game.counts);
        assert!(game.counts.ns_modules > 10, "{:?}", game.counts);
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    const EMPTY_NADEO: &str = r#"{"op":"1.0.0","ns":{}}"#;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-typedb-diff-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn index_from(tag: &str, core: &str, nadeo: &str) -> TypeIndex {
        let dir = tmp_dir(tag);
        let core_path = dir.join("OpenplanetCore.json");
        let nadeo_path = dir.join("OpenplanetNext.json");
        std::fs::write(&core_path, core).unwrap();
        std::fs::write(&nadeo_path, nadeo).unwrap();
        TypeIndex::load(&core_path, &nadeo_path).unwrap()
    }

    fn core_index(tag: &str, core: &str) -> TypeIndex {
        index_from(tag, core, EMPTY_NADEO)
    }

    /// `{"functions": [...]}` with one overload set.
    fn core_with_function(args: &str, ret: &str) -> String {
        format!(
            r#"{{"op":"1.0.0","functions":[{{"ns":"UI","name":"Begin","returntypedecl":"{ret}","args":{args}}}]}}"#
        )
    }

    fn names(symbols: &[DbSymbol]) -> Vec<String> {
        symbols.iter().map(|s| s.name.clone()).collect()
    }

    // === no-change baseline ===

    /// The property the whole pipeline rests on: a DB diffed against an
    /// identical DB reports nothing.
    #[test]
    fn diff_identical_indexes_is_empty() {
        let core = core_with_function(r#"[{"name":"label","typedecl":"string"}]"#, "bool");
        let old = core_index("same-old", &core);
        let new = core_index("same-new", &core);
        let diff = diff_indexes(&old, &new);
        assert!(diff.is_empty(), "{diff:?}");
        assert_eq!(diff.counts(), (0, 0, 0));
    }

    /// The shipped fixtures diffed against themselves — the real-scale
    /// no-op case (2585 engine classes) `just typedb-refresh` relies on.
    #[test]
    fn diff_repo_fixture_against_itself_is_empty() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        let load = || {
            TypeIndex::load(
                &dir.join("OpenplanetCore.json"),
                &dir.join("OpenplanetNext.json"),
            )
            .unwrap()
        };
        let diff = diff_indexes(&load(), &load());
        assert_eq!(diff.counts(), (0, 0, 0), "{diff:?}");
    }

    // === added / removed ===

    #[test]
    fn diff_reports_removed_function() {
        let old = core_index("rm-fn-old", &core_with_function("[]", "bool"));
        let new = core_index("rm-fn-new", r#"{"op":"1.0.0","functions":[]}"#);
        let diff = diff_indexes(&old, &new);
        assert_eq!(names(&diff.removed), vec!["UI::Begin"]);
        assert_eq!(diff.removed[0].kind, DbSymbolKind::Function);
        assert!(diff.added.is_empty() && diff.changed.is_empty(), "{diff:?}");
    }

    #[test]
    fn diff_reports_added_function() {
        let old = core_index("add-fn-old", r#"{"op":"1.0.0","functions":[]}"#);
        let new = core_index("add-fn-new", &core_with_function("[]", "bool"));
        let diff = diff_indexes(&old, &new);
        assert_eq!(names(&diff.added), vec!["UI::Begin"]);
        assert!(
            diff.removed.is_empty() && diff.changed.is_empty(),
            "{diff:?}"
        );
    }

    #[test]
    fn diff_reports_added_and_removed_enum() {
        let with_enum =
            r#"{"op":"1.0.0","enums":[{"ns":"UI","name":"Cond","values":{"Always":{"v":0}}}]}"#;
        let without = r#"{"op":"1.0.0","enums":[]}"#;

        let diff = diff_indexes(
            &core_index("add-en-old", without),
            &core_index("add-en-new", with_enum),
        );
        assert_eq!(names(&diff.added), vec!["UI::Cond"]);
        assert_eq!(diff.added[0].kind, DbSymbolKind::Enum);

        let diff = diff_indexes(
            &core_index("rm-en-old", with_enum),
            &core_index("rm-en-new", without),
        );
        assert_eq!(names(&diff.removed), vec!["UI::Cond"]);
    }

    #[test]
    fn diff_reports_added_and_removed_class() {
        let with_class = r#"{"op":"1.0.0","classes":[{"ns":"Net","name":"HttpRequest"}]}"#;
        let without = r#"{"op":"1.0.0","classes":[]}"#;

        let diff = diff_indexes(
            &core_index("add-cls-old", without),
            &core_index("add-cls-new", with_class),
        );
        assert_eq!(names(&diff.added), vec!["Net::HttpRequest"]);
        assert_eq!(diff.added[0].kind, DbSymbolKind::Class);

        let diff = diff_indexes(
            &core_index("rm-cls-old", with_class),
            &core_index("rm-cls-new", without),
        );
        assert_eq!(names(&diff.removed), vec!["Net::HttpRequest"]);
    }

    /// Nadeo-side classes participate too — the 2585-class dump is where
    /// engine-API drift shows up.
    #[test]
    fn diff_reports_removed_nadeo_class() {
        let core = r#"{"op":"1.0.0"}"#;
        let old = index_from(
            "rm-nadeo-old",
            core,
            r#"{"op":"1.0.0","ns":{"MwFoundations":{"CMwNod":{"m":[]}}}}"#,
        );
        let new = index_from("rm-nadeo-new", core, EMPTY_NADEO);
        let diff = diff_indexes(&old, &new);
        assert_eq!(names(&diff.removed), vec!["MwFoundations::CMwNod"]);
    }

    // === changed ===

    /// An overload added under an existing name is a change, not an
    /// addition — the name still resolves, its signature set moved.
    #[test]
    fn diff_reports_added_overload_as_change() {
        let old = core_index("ovl-old", &core_with_function("[]", "bool"));
        let new = core_index(
            "ovl-new",
            r#"{"op":"1.0.0","functions":[
                {"ns":"UI","name":"Begin","returntypedecl":"bool","args":[]},
                {"ns":"UI","name":"Begin","returntypedecl":"bool","args":[{"name":"label","typedecl":"string"}]}]}"#,
        );
        let diff = diff_indexes(&old, &new);
        assert!(diff.added.is_empty() && diff.removed.is_empty(), "{diff:?}");
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].name, "UI::Begin");
        assert_eq!(diff.changed[0].kind, DbSymbolKind::Function);
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.starts_with('+') && d.contains("string")),
            "{:?}",
            diff.changed[0].details
        );
    }

    #[test]
    fn diff_reports_changed_function_return_type() {
        let old = core_index("ret-old", &core_with_function("[]", "bool"));
        let new = core_index("ret-new", &core_with_function("[]", "int"));
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        let details = &diff.changed[0].details;
        assert!(
            details
                .iter()
                .any(|d| d.starts_with('-') && d.contains("bool")),
            "{details:?}"
        );
        assert!(
            details
                .iter()
                .any(|d| d.starts_with('+') && d.contains("int")),
            "{details:?}"
        );
    }

    /// Named arguments are part of the checker-facing call contract, so a
    /// rename changes a free-function overload even when its types do not.
    #[test]
    fn diff_reports_changed_free_function_parameter_name() {
        let old = core_index(
            "fn-param-name-old",
            &core_with_function(r#"[{"name":"label","typedecl":"string"}]"#, "bool"),
        );
        let new = core_index(
            "fn-param-name-new",
            &core_with_function(r#"[{"name":"title","typedecl":"string"}]"#, "bool"),
        );
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|line| line.contains("label"))
                && diff.changed[0]
                    .details
                    .iter()
                    .any(|line| line.contains("title")),
            "{diff:?}"
        );

        let unnamed = core_index(
            "fn-param-name-unnamed",
            &core_with_function(r#"[{"typedecl":"string"}]"#, "bool"),
        );
        let absence = diff_indexes(&old, &unnamed);
        assert_eq!(absence.counts(), (0, 0, 1), "{absence:?}");
        assert!(
            absence.changed[0]
                .details
                .iter()
                .any(|line| line.contains("<unnamed>: string")),
            "{absence:?}"
        );
    }

    /// A parameter default appearing or disappearing changes which calls
    /// compile, so it counts as drift.
    #[test]
    fn diff_reports_changed_param_default() {
        let old = core_index(
            "def-old",
            &core_with_function(r#"[{"name":"n","typedecl":"int"}]"#, "bool"),
        );
        let new = core_index(
            "def-new",
            &core_with_function(r#"[{"name":"n","typedecl":"int","default":"0"}]"#, "bool"),
        );
        assert_eq!(diff_indexes(&old, &new).counts(), (0, 0, 1));
    }

    /// `&out` (typeflags bit 2) decides whether an r-value argument binds
    /// (GH #47) — a flag flip is drift.
    #[test]
    fn diff_reports_changed_out_parameter_flag() {
        let old = core_index(
            "out-old",
            &core_with_function(r#"[{"name":"n","typedecl":"int","typeflags":0}]"#, "bool"),
        );
        let new = core_index(
            "out-new",
            &core_with_function(r#"[{"name":"n","typedecl":"int","typeflags":2}]"#, "bool"),
        );
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0].details.iter().any(|d| d.contains("&out")),
            "{:?}",
            diff.changed[0].details
        );
    }

    #[test]
    fn diff_reports_changed_method_signature() {
        let class = |args: &str| {
            format!(
                r#"{{"op":"1.0.0","classes":[{{"ns":"Net","name":"HttpRequest","methods":[{{"name":"Start","returntypedecl":"void","args":{args}}}]}}]}}"#
            )
        };
        let old = core_index("meth-old", &class("[]"));
        let new = core_index("meth-new", &class(r#"[{"name":"n","typedecl":"int"}]"#));
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert_eq!(diff.changed[0].name, "Net::HttpRequest");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.contains("method Start")),
            "{:?}",
            diff.changed[0].details
        );
    }

    /// Method parameter names participate in named-argument checking just as
    /// free-function names do.
    #[test]
    fn diff_reports_changed_method_parameter_name() {
        let class = |arg_name: &str| {
            format!(
                r#"{{"op":"1.0.0","classes":[{{"ns":"Net","name":"HttpRequest","methods":[{{"name":"Start","returntypedecl":"void","args":[{{"name":"{arg_name}","typedecl":"int"}}]}}]}}]}}"#
            )
        };
        let diff = diff_indexes(
            &core_index("meth-param-name-old", &class("port")),
            &core_index("meth-param-name-new", &class("channel")),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|line| line.contains("port"))
                && diff.changed[0]
                    .details
                    .iter()
                    .any(|line| line.contains("channel")),
            "{diff:?}"
        );
    }

    /// Descriptions are deliberately outside checker semantics: a doc-only
    /// refresh must remain an empty diff even as parameter names are tracked.
    #[test]
    fn diff_ignores_doc_only_function_change() {
        let old = core_index(
            "fn-doc-old",
            r#"{"op":"1.0.0","functions":[{"ns":"UI","name":"Begin","returntypedecl":"bool","desc":"old docs","args":[{"name":"label","typedecl":"string"}]}]}"#,
        );
        let new = core_index(
            "fn-doc-new",
            r#"{"op":"1.0.0","functions":[{"ns":"UI","name":"Begin","returntypedecl":"bool","desc":"new docs","args":[{"name":"label","typedecl":"string"}]}]}"#,
        );
        assert!(diff_indexes(&old, &new).is_empty());
    }

    #[test]
    fn diff_reports_changed_property_type() {
        let class = |ty: &str| {
            format!(
                r#"{{"op":"1.0.0","classes":[{{"ns":"Net","name":"HttpRequest","props":[{{"name":"Ready","typedecl":"{ty}"}}]}}]}}"#
            )
        };
        let diff = diff_indexes(
            &core_index("prop-old", &class("bool")),
            &core_index("prop-new", &class("int")),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.contains("prop Ready")),
            "{:?}",
            diff.changed[0].details
        );
    }

    /// Checker behavior differs for Core and Nadeo classes, so ownership is
    /// a semantic fact even when the visible member shape is identical.
    #[test]
    fn diff_reports_changed_class_source() {
        let index_with_source = |source| {
            let mut index = TypeIndex::new();
            index.types.insert(
                "Net::HttpRequest".to_string(),
                TypeInfo {
                    name: "HttpRequest".to_string(),
                    namespace: Some("Net".to_string()),
                    parent: None,
                    methods: Vec::new(),
                    properties: Vec::new(),
                    ctor_conversions: Vec::new(),
                    doc: None,
                    source,
                },
            );
            index
        };
        let diff = diff_indexes(
            &index_with_source(TypeSource::Core),
            &index_with_source(TypeSource::Nadeo),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|line| line.contains("source Core"))
                && diff.changed[0]
                    .details
                    .iter()
                    .any(|line| line.contains("source Nadeo")),
            "{diff:?}"
        );
    }

    /// Losing a `set_` accessor turns a writable property read-only — the
    /// game then rejects assignment, so it is drift the diff must show.
    #[test]
    fn diff_reports_property_losing_its_setter() {
        let with_set = r#"{"op":"1.0.0","classes":[{"ns":"Math","name":"iso4","methods":[
            {"name":"get_tx","returntypedecl":"float","args":[]},
            {"name":"set_tx","returntypedecl":"void","args":[{"name":"v","typedecl":"float"}]}]}]}"#;
        let without = r#"{"op":"1.0.0","classes":[{"ns":"Math","name":"iso4","methods":[
            {"name":"get_tx","returntypedecl":"float","args":[]}]}]}"#;
        let diff = diff_indexes(
            &core_index("set-old", with_set),
            &core_index("set-new", without),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.contains("get-only")),
            "{:?}",
            diff.changed[0].details
        );
    }

    #[test]
    fn diff_reports_changed_class_parent() {
        let class = |parent: &str| {
            format!(
                r#"{{"op":"1.0.0","classes":[{{"ns":"Net","name":"HttpRequest","inherits":"{parent}"}}]}}"#
            )
        };
        let diff = diff_indexes(
            &core_index("parent-old", &class("Base")),
            &core_index("parent-new", &class("Other")),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.contains("inherits")),
            "{:?}",
            diff.changed[0].details
        );
    }

    /// Single-argument constructor behaviors are the implicit-conversion set
    /// (GH #54) — gaining or losing one changes what compiles.
    #[test]
    fn diff_reports_changed_ctor_conversions() {
        let class = |behaviors: &str| {
            format!(
                r#"{{"op":"1.0.0","classes":[{{"ns":"Math","name":"mat4","behaviors":{behaviors}}}]}}"#
            )
        };
        let diff = diff_indexes(
            &core_index("ctor-old", &class("[]")),
            &core_index(
                "ctor-new",
                &class(r#"[{"type":0,"func":{"args":[{"typedecl":"mat3"}]}}]"#),
            ),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.contains("ctor from mat3")),
            "{:?}",
            diff.changed[0].details
        );
    }

    #[test]
    fn diff_reports_changed_enum_value() {
        let en = |v: i64| {
            format!(
                r#"{{"op":"1.0.0","enums":[{{"ns":"UI","name":"Cond","values":{{"Always":{{"v":{v}}}}}}}]}}"#
            )
        };
        let diff = diff_indexes(
            &core_index("enval-old", &en(0)),
            &core_index("enval-new", &en(1)),
        );
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert_eq!(diff.changed[0].kind, DbSymbolKind::Enum);
        assert!(
            diff.changed[0].details.iter().any(|d| d.contains("Always")),
            "{:?}",
            diff.changed[0].details
        );
    }

    /// The enum's source-facing status changes the checker surface even
    /// when its key and values do not; the typedb diff must report it.
    #[test]
    fn diff_reports_changed_enum_source_facing_status() {
        let enum_db =
            r#"{"op":"1.0.0","enums":[{"ns":"UI","name":"Cond","values":{"Always":{"v":0}}}]}"#;
        let named = core_index("en-source-named", enum_db);
        let mut anonymous = core_index("en-source-anonymous", enum_db);
        anonymous.enums.get_mut("UI::Cond").unwrap().anonymous = true;

        for (old, new, removed, added) in [
            (&anonymous, &named, "anonymous", "named"),
            (&named, &anonymous, "named", "anonymous"),
        ] {
            let diff = diff_indexes(old, new);
            assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
            assert_eq!(diff.changed[0].kind, DbSymbolKind::Enum);
            assert!(
                diff.changed[0]
                    .details
                    .iter()
                    .any(|detail| detail == &format!("- source-facing {removed}")),
                "{:?}",
                diff.changed[0].details
            );
            assert!(
                diff.changed[0]
                    .details
                    .iter()
                    .any(|detail| detail == &format!("+ source-facing {added}")),
                "{:?}",
                diff.changed[0].details
            );
        }
    }

    #[test]
    fn diff_reports_added_enum_member() {
        let old = core_index(
            "enmem-old",
            r#"{"op":"1.0.0","enums":[{"ns":"UI","name":"Cond","values":{"Always":{"v":0}}}]}"#,
        );
        let new = core_index(
            "enmem-new",
            r#"{"op":"1.0.0","enums":[{"ns":"UI","name":"Cond","values":{"Always":{"v":0},"Once":{"v":1}}}]}"#,
        );
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert!(
            diff.changed[0]
                .details
                .iter()
                .any(|d| d.starts_with('+') && d.contains("Once")),
            "{:?}",
            diff.changed[0].details
        );
    }

    /// Regression guard for the load-order bug this diff surfaced: iterating
    /// the `ns`/type HashMaps directly made the loaded index differ between
    /// two loads of the same bytes wherever an enum key was contested. Same
    /// file in ⇒ same index out.
    ///
    /// The contest that exposed it — 66 inline member-enum type names
    /// colliding within a module, 15 distinct anonymous enums all keyed
    /// `Control::UnnamedEnum` — is gone since B016 keys them by declaring
    /// context, but sorted registration still decides `register_anon_member_enum`'s
    /// `#2` probe order, so the guard stays load-bearing.
    #[test]
    fn loading_the_same_db_twice_yields_identical_enums() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        let load = || {
            TypeIndex::load(
                &dir.join("OpenplanetCore.json"),
                &dir.join("OpenplanetNext.json"),
            )
            .unwrap()
        };
        let facts = |index: &TypeIndex| {
            let mut all: Vec<String> = index
                .enums_iter()
                .flat_map(|(qname, info)| {
                    info.values
                        .iter()
                        .map(move |(name, v)| format!("{qname}::{name} = {v}"))
                })
                .collect();
            all.sort();
            all
        };
        let (a, b) = (load(), load());
        let (fa, fb) = (facts(&a), facts(&b));
        assert_eq!(
            fa.len(),
            fb.len(),
            "enum value count must not depend on HashMap iteration order"
        );
        assert_eq!(fa, fb, "two loads of the same DB must agree");

        // The formerly-colliding group resolves to 15 separate entries now
        // (B016), and to the same 15 on every load.
        assert!(
            a.lookup_enum("Control::UnnamedEnum").is_none(),
            "the placeholder key must not come back"
        );
        let control_anon = |index: &TypeIndex| {
            let mut keys: Vec<String> = index
                .enums_iter()
                .filter(|(qname, info)| info.anonymous && qname.starts_with("Control::"))
                .map(|(qname, _)| qname.clone())
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(control_anon(&a).len(), 15, "{:?}", control_anon(&a));
        assert_eq!(control_anon(&a), control_anon(&b));
    }

    // === ordering / shape ===

    /// Output is sorted so a diff is stable across runs (HashMap iteration
    /// order is not).
    #[test]
    fn diff_output_is_sorted_and_deterministic() {
        let old = core_index("sort-old", r#"{"op":"1.0.0","classes":[]}"#);
        let new = core_index(
            "sort-new",
            r#"{"op":"1.0.0","classes":[{"ns":"Z","name":"Zed"},{"ns":"A","name":"Ay"},{"ns":"M","name":"Em"}]}"#,
        );
        let first = diff_indexes(&old, &new);
        assert_eq!(names(&first.added), vec!["A::Ay", "M::Em", "Z::Zed"]);
        assert_eq!(first, diff_indexes(&old, &new), "diff must be stable");
    }

    /// Removals and additions across kinds are bucketed, not merged.
    #[test]
    fn diff_buckets_mixed_kinds_separately() {
        let old = core_index(
            "mix-old",
            r#"{"op":"1.0.0","classes":[{"ns":"A","name":"Gone"}],"enums":[],"functions":[]}"#,
        );
        let new = core_index(
            "mix-new",
            r#"{"op":"1.0.0","classes":[],"enums":[{"ns":"B","name":"New","values":{}}],
                "functions":[{"ns":"C","name":"Fresh","returntypedecl":"void","args":[]}]}"#,
        );
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (2, 1, 0), "{diff:?}");
        assert_eq!(
            diff.added
                .iter()
                .map(|s| (s.kind, s.name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (DbSymbolKind::Enum, "B::New"),
                (DbSymbolKind::Function, "C::Fresh"),
            ]
        );
        assert_eq!(names(&diff.removed), vec!["A::Gone"]);
    }
}

#[cfg(test)]
mod shape_guard_tests {
    use super::*;

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb")
    }

    fn fixture_core() -> std::path::PathBuf {
        fixture_dir().join("OpenplanetCore.json")
    }

    fn fixture_game() -> std::path::PathBuf {
        fixture_dir().join("OpenplanetNext.json")
    }

    fn tmp_file(tag: &str, name: &str, contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-typedb-shape-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// The anti-drift tripwire itself: the DBs this repo ships must always
    /// pass. A failure here means a corrupt or truncated harvest landed.
    #[test]
    fn typedb_shape_guard_accepts_repo_fixtures() {
        if let Err(problems) = check_typedb_shape(&fixture_core(), &fixture_game()) {
            panic!(
                "shipped fixture DBs failed the shape guard:\n  {}",
                problems.join("\n  ")
            );
        }
    }

    #[test]
    fn typedb_shape_guard_rejects_truncated_core() {
        let core = tmp_file(
            "trunc-core",
            "OpenplanetCore.json",
            r#"{"op":"1.29.5","functions":[{"name":"Begin""#,
        );
        let problems = check_typedb_shape(&core, &fixture_game()).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("OpenplanetCore.json")),
            "{problems:?}"
        );
    }

    #[test]
    fn typedb_shape_guard_rejects_truncated_game_dump() {
        let game = tmp_file(
            "trunc-game",
            "OpenplanetNext.json",
            r#"{"op":"1.29.5","ns":{"MwFoundations":{"CMwNod""#,
        );
        let problems = check_typedb_shape(&fixture_core(), &game).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("OpenplanetNext.json")),
            "{problems:?}"
        );
    }

    /// A well-formed but empty DB is the dangerous case: it parses, loads,
    /// and silently disables every type-aware diagnostic.
    #[test]
    fn typedb_shape_guard_rejects_empty_core() {
        let core = tmp_file(
            "empty-core",
            "OpenplanetCore.json",
            r#"{"op":"1.29.5","functions":[],"classes":[],"enums":[]}"#,
        );
        let problems = check_typedb_shape(&core, &fixture_game()).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("functions")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("classes")),
            "{problems:?}"
        );
        assert!(problems.iter().any(|p| p.contains("enums")), "{problems:?}");
    }

    #[test]
    fn typedb_shape_guard_rejects_empty_game_dump() {
        let game = tmp_file(
            "empty-game",
            "OpenplanetNext.json",
            r#"{"op":"1.29.5","mp":"2026-02-03 03:51:19","ns":{}}"#,
        );
        let problems = check_typedb_shape(&fixture_core(), &game).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("classes")),
            "{problems:?}"
        );
    }

    /// A partial harvest that keeps *some* entries is still a regression —
    /// the thresholds are floors, not emptiness checks.
    #[test]
    fn typedb_shape_guard_rejects_partially_harvested_core() {
        let core = tmp_file(
            "partial-core",
            "OpenplanetCore.json",
            r#"{"op":"1.29.5",
                "functions":[{"ns":"UI","name":"Begin"}],
                "classes":[{"ns":"Net","name":"HttpRequest"}],
                "enums":[{"ns":"UI","name":"Cond","values":{}}]}"#,
        );
        let problems = check_typedb_shape(&core, &fixture_game()).unwrap_err();
        assert!(problems.len() >= 3, "{problems:?}");
    }

    #[test]
    fn typedb_shape_guard_rejects_missing_op() {
        let core = tmp_file(
            "no-op",
            "OpenplanetCore.json",
            r#"{"functions":[],"classes":[],"enums":[]}"#,
        );
        let problems = check_typedb_shape(&core, &fixture_game()).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("op")), "{problems:?}");
    }

    /// `op` must look like a version, so a placeholder or a truncated stamp
    /// does not pass as one.
    #[test]
    fn typedb_shape_guard_rejects_non_version_op() {
        for bad in ["dev", "1.29", "v1.29.5", "", "next"] {
            let core = tmp_file(
                "bad-op",
                "OpenplanetCore.json",
                &format!(r#"{{"op":"{bad}","functions":[],"classes":[],"enums":[]}}"#),
            );
            let problems = check_typedb_shape(&core, &fixture_game()).unwrap_err();
            assert!(
                problems.iter().any(|p| p.contains("op")),
                "op {bad:?} should be rejected: {problems:?}"
            );
        }
    }

    /// Both real stamps pass the version check, including Core's trailing
    /// build metadata.
    #[test]
    fn typedb_shape_guard_accepts_real_op_stamp_shapes() {
        for good in ["1.29.5", "1.29.5 (next, Public, 1234ad9e)", "2.0.10"] {
            assert!(looks_like_version(good), "{good:?} should be accepted");
        }
    }

    #[test]
    fn typedb_shape_guard_rejects_missing_file() {
        let missing = std::env::temp_dir().join("openplanet-lsp-no-such-db.json");
        let problems = check_typedb_shape(&missing, &fixture_game()).unwrap_err();
        assert!(!problems.is_empty(), "{problems:?}");
    }
}

/// B016 — inline/anonymous member enums must be keyed by their declaring
/// context, not collapsed onto `<Module>::UnnamedEnum`.
///
/// The Nadeo dump gives an inline member enum no name of its own: the member
/// carries `"t": "UnnamedEnum"` plus an `e` block of values. Keying by that
/// type name put every anonymous enum in a module on one key, and
/// `or_insert` dropped all but the first — 132 anonymous enums in the shipped
/// fixture collapsed to 14 (one per module), with `Control` losing 14 of 15.
#[cfg(test)]
mod anon_member_enum_tests {
    use super::*;
    use std::path::PathBuf;

    const CORE_MIN: &str = r#"{"op":"1.0.0","functions":[],"classes":[],"enums":[]}"#;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb")
    }

    fn load_fixture() -> TypeIndex {
        let dir = fixture_dir();
        TypeIndex::load(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
        )
        .unwrap()
    }

    /// Build an index over a synthetic Nadeo dump (Core stays empty).
    fn nadeo_index(tag: &str, nadeo: &str) -> TypeIndex {
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-anon-enum-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let core_path = dir.join("OpenplanetCore.json");
        let nadeo_path = dir.join("OpenplanetNext.json");
        std::fs::write(&core_path, CORE_MIN).unwrap();
        std::fs::write(&nadeo_path, nadeo).unwrap();
        TypeIndex::load(&core_path, &nadeo_path).unwrap()
    }

    /// Every anonymous member enum in the shipped fixture, read straight
    /// from the JSON: `(Module::Class::Member, values)`.
    fn expected_from_fixture() -> Vec<(String, Vec<(String, i64)>)> {
        let db = NadeoDatabase::load_from_file(&fixture_dir().join("OpenplanetNext.json")).unwrap();
        let mut out = Vec::new();
        let mut modules: Vec<&String> = db.ns.keys().collect();
        modules.sort();
        for ns_name in modules {
            let types = &db.ns[ns_name];
            let mut type_names: Vec<&String> = types.keys().collect();
            type_names.sort();
            for type_name in type_names {
                for member in &types[type_name].m {
                    if member.kind() != NadeoMemberKind::Enum {
                        continue;
                    }
                    if member.type_name() != Some("UnnamedEnum") {
                        continue;
                    }
                    let values = member_inline_enum_values(member.e.as_ref()).unwrap();
                    out.push((format!("{}::{}::{}", ns_name, type_name, member.n), values));
                }
            }
        }
        out
    }

    /// The headline data loss: 132 anonymous member enums in the fixture,
    /// each independently queryable under its declaring context. Before
    /// B016 only 14 survived (one per module).
    #[test]
    fn every_anonymous_member_enum_is_independently_queryable() {
        let index = load_fixture();
        let expected = expected_from_fixture();
        assert_eq!(
            expected.len(),
            132,
            "fixture shape guard: the shipped Next dump declares 132 anonymous \
             member enums — update this count deliberately on a DB refresh"
        );
        for (key, values) in &expected {
            let found = index.lookup_enum(key).unwrap_or_else(|| {
                panic!("anonymous member enum `{key}` is missing from the index")
            });
            assert_eq!(&found.values, values, "values of `{key}`");
        }
    }

    /// The 66 colliding inline member-enum type names are gone: no enum key
    /// is a bare `<Module>::UnnamedEnum` any more.
    #[test]
    fn no_module_level_unnamed_enum_key_survives() {
        let index = load_fixture();
        let leftovers: Vec<&String> = index
            .enums_iter()
            .map(|(qname, _)| qname)
            .filter(|qname| qname.ends_with("::UnnamedEnum"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "anonymous enums must not be keyed by the placeholder type name: {leftovers:?}"
        );
        assert!(index.lookup_enum("Control::UnnamedEnum").is_none());
    }

    /// The worst collision group: `Control` had 15 distinct anonymous enums
    /// on one key, so 14 were dropped. All 15 must now resolve, each with
    /// its own values.
    #[test]
    fn control_modules_fifteen_anonymous_enums_all_resolve() {
        let index = load_fixture();
        let control: Vec<String> = expected_from_fixture()
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with("Control::"))
            .collect();
        assert_eq!(control.len(), 15, "{control:?}");
        for key in &control {
            assert!(
                index.lookup_enum(key).is_some(),
                "`{key}` must resolve independently"
            );
        }
        assert_eq!(
            index
                .lookup_enum("Control::CControlColorChooser::StyleType")
                .map(|e| e.values.clone()),
            Some(vec![
                ("HueSlider".to_string(), 0),
                ("SLSquare".to_string(), 1),
                ("MinimalReadOnly".to_string(), 2),
            ])
        );
    }

    /// Variant — two previously-collided enums that share value *names*
    /// (`None`) stay distinct and separately queryable.
    #[test]
    fn collided_enums_sharing_value_names_stay_distinct() {
        let index = load_fixture();
        for (a, b, shared) in [
            (
                "Control::CControlBase::AlignHorizontal",
                "Control::CControlBase::AlignVertical",
                "None",
            ),
            (
                "Hms::CHmsViewport::TextureRender",
                "Hms::CHmsViewportPerfDbg::VisionSkip",
                "All",
            ),
            // Same member name on two classes, differing in exactly one
            // value (`Custom` vs `Multi` at 7) — the collapse used to serve
            // one enum's values for the other.
            (
                "Game::CGameCtnChallenge::Kind",
                "Game::CGameCtnChallengeInfo::Kind",
                "Campaign",
            ),
        ] {
            let ea = index.lookup_enum(a).unwrap_or_else(|| panic!("{a}"));
            let eb = index.lookup_enum(b).unwrap_or_else(|| panic!("{b}"));
            assert!(
                ea.values.iter().any(|(n, _)| n == shared),
                "{a} lacks {shared}"
            );
            assert!(
                eb.values.iter().any(|(n, _)| n == shared),
                "{b} lacks {shared}"
            );
            assert_ne!(ea.values, eb.values, "{a} and {b} must stay distinct");
        }
    }

    /// Variant — two anonymous enums with *identical* values on different
    /// classes are still two entries, because the key carries the class.
    #[test]
    fn identical_anonymous_enums_on_different_classes_are_both_kept() {
        let index = load_fixture();
        for (a, b) in [
            (
                "Control::CControlBase::AlignHorizontal",
                "Control::CControlLayout::AlignHorizontal",
            ),
            (
                "Control::CControlButton::DisplayType",
                "Control::CControlEnum::DisplayType",
            ),
        ] {
            let ea = index.lookup_enum(a).unwrap_or_else(|| panic!("{a}"));
            let eb = index.lookup_enum(b).unwrap_or_else(|| panic!("{b}"));
            assert_eq!(
                ea.values, eb.values,
                "{a} / {b} should carry the same values"
            );
        }
    }

    /// Variant — a module with exactly ONE anonymous member enum never
    /// collided, but its key must still be contextual (no regression to
    /// `Audio::UnnamedEnum`).
    #[test]
    fn single_anonymous_enum_module_is_also_keyed_by_context() {
        let index = load_fixture();
        let audio: Vec<String> = expected_from_fixture()
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with("Audio::"))
            .collect();
        assert_eq!(audio, vec!["Audio::CAudioPort::SettingQuality".to_string()]);
        assert!(index
            .lookup_enum("Audio::CAudioPort::SettingQuality")
            .is_some());
        assert!(index.lookup_enum("Audio::UnnamedEnum").is_none());
    }

    /// The member → enum link is queryable directly, which is what
    /// completion/hover need (the values are reachable in-game only through
    /// the member).
    #[test]
    fn member_enum_is_reachable_from_its_declaring_member() {
        let index = load_fixture();
        let en = index
            .lookup_member_enum("Control::CControlEntry", "Type")
            .expect("Control::CControlEntry::Type");
        assert!(en.values.iter().any(|(n, _)| n == "TimeHhMmSsMil"));
        assert!(index
            .lookup_member_enum("Control::CControlEntry", "Nope")
            .is_none());
    }

    /// Fallback — an anonymous enum whose declaring member cannot be named
    /// (empty member name) gets a stable synthesized key derived from its
    /// declaration ordinal, and two loads agree.
    #[test]
    fn unidentifiable_member_falls_back_to_a_stable_synthesized_key() {
        let nadeo = r#"{"op":"1.0.0","ns":{"X":{"C":{"i":"1","m":[
            {"n":"","i":0,"t":"UnnamedEnum","e":{"n":"UnnamedEnum","v":["A","B"]}},
            {"n":"","i":1,"t":"UnnamedEnum","e":{"n":"UnnamedEnum","v":["C"]}}
        ]}}}}"#;
        let first = nadeo_index("fallback-a", nadeo);
        let second = nadeo_index("fallback-b", nadeo);
        let keys = |ix: &TypeIndex| {
            let mut k: Vec<String> = ix.enums_iter().map(|(q, _)| q.clone()).collect();
            k.sort();
            k
        };
        assert_eq!(keys(&first), keys(&second), "fallback keys must be stable");
        assert_eq!(keys(&first).len(), 2, "{:?}", keys(&first));
        assert!(
            keys(&first).iter().all(|k| k.starts_with("X::C::")),
            "{:?}",
            keys(&first)
        );
        let values: Vec<Vec<(String, i64)>> = keys(&first)
            .iter()
            .map(|k| first.lookup_enum(k).unwrap().values.clone())
            .collect();
        assert!(values.iter().any(|v| v.len() == 2) && values.iter().any(|v| v.len() == 1));
    }

    /// Fallback — two anonymous enums declared under the SAME member name on
    /// one class must not overwrite each other either.
    #[test]
    fn duplicate_member_names_on_one_class_do_not_collide() {
        let nadeo = r#"{"op":"1.0.0","ns":{"X":{"C":{"i":"1","m":[
            {"n":"Mode","i":0,"t":"UnnamedEnum","e":{"n":"UnnamedEnum","v":["A","B"]}},
            {"n":"Mode","i":1,"t":"UnnamedEnum","e":{"n":"UnnamedEnum","v":["C"]}}
        ]}}}}"#;
        let index = nadeo_index("dup-member", nadeo);
        let mut keys: Vec<&String> = index.enums_iter().map(|(q, _)| q).collect();
        keys.sort();
        assert_eq!(keys, vec!["X::C::Mode", "X::C::Mode#2"], "{keys:?}");
        assert_eq!(
            index.lookup_enum("X::C::Mode").map(|e| e.values.len()),
            Some(2),
            "the first declaration keeps the plain key"
        );
        assert_eq!(
            index.lookup_enum("X::C::Mode#2").map(|e| e.values.len()),
            Some(1),
            "the second takes the disambiguated key"
        );
        assert_eq!(
            index
                .lookup_member_enum("X::C", "Mode")
                .map(|e| e.values.len()),
            Some(2),
            "the member lookup returns the first declaration"
        );
    }

    /// Fallback — an anonymous member whose name matches a nested enum on
    /// the same class must not displace the nested enum.
    #[test]
    fn anonymous_member_does_not_displace_a_nested_enum_of_the_same_name() {
        let nadeo = r#"{"op":"1.0.0","ns":{"X":{"C":{"i":"1",
            "e":[{"n":"Mode","v":["Nested0","Nested1"]}],
            "m":[{"n":"Mode","i":0,"t":"UnnamedEnum","e":{"n":"UnnamedEnum","v":["Anon"]}}]
        }}}}"#;
        let index = nadeo_index("nested-clash", nadeo);
        assert_eq!(
            index.lookup_enum("X::C::Mode").map(|e| e
                .values
                .iter()
                .map(|(n, _)| n.clone())
                .collect::<Vec<_>>()),
            Some(vec!["Nested0".to_string(), "Nested1".to_string()]),
            "the *named* nested enum owns the plain key"
        );
        let mut keys: Vec<&String> = index.enums_iter().map(|(q, _)| q).collect();
        keys.sort();
        assert_eq!(keys, vec!["X::C::Mode", "X::C::Mode#2"], "{keys:?}");
        // The member lookup must skip the named squatter and return the
        // enum the member actually declares.
        assert_eq!(
            index.lookup_member_enum("X::C", "Mode").map(|e| e
                .values
                .iter()
                .map(|(n, _)| n.clone())
                .collect::<Vec<_>>()),
            Some(vec!["Anon".to_string()]),
            "the member declares the ANONYMOUS enum, not the nested one"
        );
    }

    /// The name-resolution surface must not move: an anonymous enum has no
    /// name in the game, so its synthesized key must stay out of the
    /// short-name index that resolves unqualified user-written type names.
    #[test]
    fn synthesized_keys_stay_out_of_the_short_name_index() {
        let index = load_fixture();
        for short in [
            "AlignHorizontal",
            "SettingQuality",
            "StyleType",
            "UnnamedEnum",
        ] {
            assert!(
                index.find_by_short_name(short).is_empty(),
                "`{short}` must not become resolvable as a type name: {:?}",
                index.find_by_short_name(short)
            );
        }
        // `Type` is the one pre-existing short name an unfiltered index
        // would have hijacked (`Control::CControlEntry::Type` sorts before
        // `Json::Type`).
        assert_eq!(
            index.find_by_short_name("Type").first().map(String::as_str),
            Some("Json::Type"),
        );
        assert!(!index.all_short_names().iter().any(|n| n == "UnnamedEnum"));
    }

    /// B016: the contextual key's prefix is an internal lookup context, not
    /// a source namespace that can lend authority to qualified diagnostics.
    #[test]
    fn anonymous_member_enum_namespaces_stay_internal() {
        let index = load_fixture();
        for namespace in ["Control::CControlEntry", "Audio::CAudioPort"] {
            assert!(
                !index.namespace_exists(namespace),
                "anonymous enum context `{namespace}` must not become a source namespace"
            );
        }
        assert!(
            index.namespace_exists("Game::CGameCtnBlock"),
            "named nested enum namespace must stay source-visible"
        );
    }

    /// The drift the collapse used to hide. `typedb diff` is the tripwire
    /// that found B016, and it could only ever see the one anonymous enum
    /// that won its module's collision. Change a value in one that did NOT
    /// win, and the diff must now report it — otherwise `typedb refresh`
    /// could ship a changed engine enum silently.
    #[test]
    fn a_change_to_a_formerly_dropped_anonymous_enum_is_reported() {
        // `AA` sorts before `ZZ`, so pre-B016 both members keyed
        // `X::UnnamedEnum` and only `AA`'s values reached the index.
        let db = |zz_second: &str| {
            format!(
                r#"{{"op":"1.0.0","ns":{{"X":{{
                    "AA":{{"i":"1","m":[{{"n":"Mode","i":0,"t":"UnnamedEnum","e":{{"n":"UnnamedEnum","v":["Keep"]}}}}]}},
                    "ZZ":{{"i":"2","m":[{{"n":"Mode","i":0,"t":"UnnamedEnum","e":{{"n":"UnnamedEnum","v":["Same","{zz_second}"]}}}}]}}
                }}}}}}"#
            )
        };
        let old = nadeo_index("drift-old", &db("Before"));
        let new = nadeo_index("drift-new", &db("After"));
        let diff = diff_indexes(&old, &new);
        assert_eq!(diff.counts(), (0, 0, 1), "{diff:?}");
        assert_eq!(diff.changed[0].name, "X::ZZ::Mode");
        assert_eq!(diff.changed[0].kind, DbSymbolKind::Enum);
    }

    /// Determinism, restated for the new keys: identical bytes in ⇒
    /// identical anonymous-enum keys and values out.
    #[test]
    fn anonymous_enum_keys_are_identical_across_two_loads() {
        let facts = |index: &TypeIndex| {
            let mut all: Vec<String> = index
                .enums_iter()
                .flat_map(|(qname, info)| {
                    info.values
                        .iter()
                        .map(move |(name, v)| format!("{qname}::{name} = {v}"))
                })
                .collect();
            all.sort();
            all
        };
        assert_eq!(facts(&load_fixture()), facts(&load_fixture()));
    }
}

/// B046 — MODULE-LEVEL engine enums, the enums the dump's shape cannot
/// express.
///
/// The Nadeo dump's `ns` map is namespace -> type name -> type, so the only
/// enum it carries directly is one nested in a class. It reaches most
/// module-level enums by accident of shape — a member typed by one carries
/// the enum inline — but an enum nothing is typed by has no route in at all.
/// `Openplanet.h` is then the only source. Measured on OP 1.29.5: 59
/// module-level enums, 57 already reachable, 2 not.
///
/// The risk this suite exists to pin is the other direction: the header is a
/// SUPERSET of the dump, so a loader that took everything would double-index
/// 57 enums under a second key.
#[cfg(test)]
mod module_level_enum_tests {
    use super::*;
    use crate::typedb::header_format::{HeaderDatabase, HeaderEnum};
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb")
    }

    fn load_fixture() -> TypeIndex {
        let dir = fixture_dir();
        TypeIndex::load(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
        )
        .unwrap()
    }

    /// A private ignored directory to assemble a DB pair in. Keeping it below
    /// the repository makes these tests independent of a home or `/tmp`.
    /// The pid keeps concurrent test binaries apart.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("test-typedb-b046-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn load_without_header() -> TypeIndex {
        let dir = fixture_dir();
        TypeIndex::load_with_header(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
            None,
        )
        .unwrap()
    }

    fn header_sourced(index: &TypeIndex) -> Vec<String> {
        let mut keys: Vec<String> = index
            .enums_iter()
            .filter(|(_, info)| info.source == TypeSource::Header)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort();
        keys
    }

    /// The whole point: the two enums the dump cannot express are indexed,
    /// and NOTHING else is taken from the header.
    #[test]
    fn the_header_contributes_exactly_the_enums_the_dump_lacks() {
        let index = load_fixture();
        assert_eq!(
            header_sourced(&index),
            vec![
                "ESceneCharVisNotice".to_string(),
                "ESceneVehicleVisEvent".to_string()
            ]
        );
    }

    /// The header declares 59 module-level enums and the index gains 2, so
    /// 57 were recognised as already present. Asserted as a delta rather
    /// than an absolute so it keeps meaning after a type-DB refresh.
    #[test]
    fn the_other_fifty_seven_module_level_enums_are_not_double_indexed() {
        let header = HeaderDatabase::load_from_file(&fixture_dir().join("Openplanet.h")).unwrap();
        assert_eq!(header.module_enums.len(), 59, "fixture guard");
        let before = load_without_header();
        let after = load_fixture();
        assert_eq!(
            after.enum_count() - before.enum_count(),
            2,
            "the header must add only what the dump could not express"
        );
    }

    /// The dump stays the authority for an enum it already carries: the
    /// header must not shadow `Game::ESpectatorCameraType` with a bare
    /// `ESpectatorCameraType`, which would put two FQNs in one short-name
    /// bucket and move the pick `resolve_unqualified` depends on.
    #[test]
    fn an_enum_the_dump_already_carries_keeps_its_dump_key_and_gains_no_second_one() {
        let index = load_fixture();
        assert!(
            index
                .lookup_source_enum("Game::ESpectatorCameraType")
                .is_some(),
            "fixture guard: the dump carries this one"
        );
        assert!(
            index.lookup_enum("ESpectatorCameraType").is_none(),
            "the header must not add a second key for an enum the dump has"
        );
        // The dump itself already puts two keys in this bucket (a nested one
        // and the module-level one). What must not change is the bucket:
        // adding a bare third key would move the lexicographically-first
        // pick that `resolve_unqualified` treats as the answer.
        assert_eq!(
            index.find_by_short_name("ESpectatorCameraType"),
            load_without_header().find_by_short_name("ESpectatorCameraType"),
            "the header must not disturb an existing short-name bucket"
        );
    }

    /// The stronger form of the rule above, over the whole index: reading
    /// the header must leave every pre-existing short-name bucket alone.
    /// This is what keeps `resolve_unqualified`'s deterministic first pick,
    /// and with it the false-positive fixture counts, unchanged.
    #[test]
    fn the_header_disturbs_no_existing_short_name_bucket() {
        let before = load_without_header();
        let after = load_fixture();
        for short in before.all_short_names() {
            assert_eq!(
                before.find_by_short_name(&short),
                after.find_by_short_name(&short),
                "short-name bucket `{short}` changed"
            );
        }
        // The only new buckets are the two enums the dump could not express.
        let added: Vec<String> = after
            .all_short_names()
            .into_iter()
            .filter(|s| before.find_by_short_name(s).is_empty())
            .collect();
        assert_eq!(added, ["ESceneCharVisNotice", "ESceneVehicleVisEvent"]);
    }

    /// A module-level enum is spellable, so unlike B016's synthesized keys
    /// it belongs on every source-facing surface.
    #[test]
    fn a_module_level_enum_is_source_facing() {
        let index = load_fixture();
        let info = index
            .lookup_source_enum("ESceneCharVisNotice")
            .expect("must be reachable as a source-facing enum");
        assert!(!info.anonymous);
        assert_eq!(info.name, "ESceneCharVisNotice");
        assert_eq!(info.values.len(), 33);
        assert_eq!(
            index.find_by_short_name("ESceneCharVisNotice"),
            ["ESceneCharVisNotice"],
            "it must join the short-name index"
        );
    }

    /// No namespace is invented. The game rejects both `Scene::` (a real
    /// dump namespace whose name the enum shares a prefix with) and
    /// `Enums::` (the header's own bucket) with `Namespace '…' doesn't
    /// exist.`, so the entry carries neither.
    #[test]
    fn no_namespace_is_attributed_to_a_module_level_enum() {
        let index = load_fixture();
        for name in ["ESceneCharVisNotice", "ESceneVehicleVisEvent"] {
            let info = index.lookup_source_enum(name).unwrap();
            assert_eq!(info.namespace, None, "{name} must have no namespace");
        }
        assert!(index.lookup_enum("Scene::ESceneCharVisNotice").is_none());
        assert!(index.lookup_enum("Enums::ESceneCharVisNotice").is_none());
        assert!(
            !index.namespaces().iter().any(|n| n == "Enums"),
            "the header's documentation bucket must not become a namespace"
        );
    }

    /// The header carries explicit integers; the dump only has positions.
    /// Six engine enums are non-contiguous, so this is a real difference.
    #[test]
    fn header_enum_values_keep_the_engines_own_integers() {
        let mut index = TypeIndex::new();
        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "EGappy".to_string(),
                values: vec![("A".to_string(), 2), ("B".to_string(), 11)],
            }],
        });
        let info = index.lookup_source_enum("EGappy").unwrap();
        assert_eq!(
            info.values,
            vec![("A".to_string(), 2), ("B".to_string(), 11)]
        );
    }

    /// Values are sorted the same way Core's are, so a header that lists
    /// aliases in a different order still yields one index.
    #[test]
    fn header_enum_values_are_sorted_deterministically() {
        let mut index = TypeIndex::new();
        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "EAliases".to_string(),
                values: vec![
                    ("Zebra".to_string(), 1),
                    ("Second".to_string(), 2),
                    ("Alpha".to_string(), 1),
                ],
            }],
        });
        let info = index.lookup_source_enum("EAliases").unwrap();
        assert_eq!(
            info.values,
            vec![
                ("Alpha".to_string(), 1),
                ("Zebra".to_string(), 1),
                ("Second".to_string(), 2),
            ]
        );
    }

    /// `add_header` runs after the dump, so a name the dump owns is never
    /// overwritten even when the header disagrees about its values.
    #[test]
    fn the_dump_wins_a_disagreement_about_values() {
        let dir = fixture_dir();
        let mut index = TypeIndex::load_with_header(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
            None,
        )
        .unwrap();
        let before = index
            .lookup_source_enum("Game::ESpectatorCameraType")
            .unwrap()
            .values
            .clone();
        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "ESpectatorCameraType".to_string(),
                values: vec![("Bogus".to_string(), 99)],
            }],
        });
        assert_eq!(
            index
                .lookup_source_enum("Game::ESpectatorCameraType")
                .unwrap()
                .values,
            before
        );
        assert!(index.lookup_enum("ESpectatorCameraType").is_none());
    }

    /// An anonymous key must not be mistaken for a name the dump owns —
    /// B016's synthesized keys name nothing, so they cannot block a real
    /// module-level enum of the same tail.
    #[test]
    fn an_anonymous_key_does_not_block_a_module_level_enum() {
        let core = r#"{"op":"1.0.0","functions":[],"classes":[],"enums":[]}"#;
        let nadeo = r#"{"op":"1.0.0","ns":{"Game":{"CThing":{"i":"1","m":[
            {"n":"EMystery","i":0,"t":"UnnamedEnum","e":{"n":"UnnamedEnum","v":["A"]}}]}}}}"#;
        let dir = scratch_dir("anon-key-no-block");
        std::fs::write(dir.join("OpenplanetCore.json"), core).unwrap();
        std::fs::write(dir.join("OpenplanetNext.json"), nadeo).unwrap();
        let mut index = TypeIndex::load_with_header(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
            None,
        )
        .unwrap();
        assert!(index.lookup_enum("Game::CThing::EMystery").is_some());
        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "EMystery".to_string(),
                values: vec![("B".to_string(), 0)],
            }],
        });
        assert!(
            index.lookup_source_enum("EMystery").is_some(),
            "an anonymous key names nothing and must not shadow a real enum"
        );
    }

    #[test]
    fn a_header_enum_does_not_change_a_type_short_name_bucket() {
        let mut index = TypeIndex::new();
        index.types.insert(
            "Game::ECollision".to_string(),
            TypeInfo {
                name: "ECollision".to_string(),
                namespace: Some("Game".to_string()),
                parent: None,
                methods: Vec::new(),
                properties: Vec::new(),
                ctor_conversions: Vec::new(),
                doc: None,
                source: TypeSource::Nadeo,
            },
        );
        index.build_short_type_index();
        let before = index.find_by_short_name("ECollision").to_vec();

        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "ECollision".to_string(),
                values: vec![("A".to_string(), 0)],
            }],
        });
        index.build_short_type_index();

        assert!(index.lookup_enum("ECollision").is_none());
        assert_eq!(index.find_by_short_name("ECollision"), before.as_slice());
    }

    #[test]
    fn a_header_enum_does_not_change_a_funcdef_short_name_bucket() {
        let mut index = TypeIndex::new();
        index.funcdefs.insert(
            "UI::ECollision".to_string(),
            FuncdefInfo {
                name: "ECollision".to_string(),
                namespace: Some("UI".to_string()),
                return_type: "void".to_string(),
                params: Vec::new(),
                decl: None,
                doc: None,
                source: TypeSource::Core,
            },
        );
        index.build_short_type_index();
        let before = index.find_by_short_name("ECollision").to_vec();

        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "ECollision".to_string(),
                values: vec![("A".to_string(), 0)],
            }],
        });
        index.build_short_type_index();

        assert!(index.lookup_enum("ECollision").is_none());
        assert_eq!(index.find_by_short_name("ECollision"), before.as_slice());
    }

    #[test]
    fn a_header_enum_without_a_type_name_collision_is_inserted() {
        let mut index = TypeIndex::new();
        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "EFresh".to_string(),
                values: vec![("A".to_string(), 0)],
            }],
        });
        index.build_short_type_index();
        assert!(index.lookup_source_enum("EFresh").is_some());
        assert_eq!(index.find_by_short_name("EFresh"), ["EFresh"]);
    }

    /// Header entries are optional supplements, so they may not displace a
    /// source-visible type that already owns the short-name resolution bucket.
    #[test]
    fn a_header_enum_sharing_a_short_name_with_a_type_is_not_added() {
        let core = r#"{"op":"1.0.0","functions":[],"classes":[],"enums":[]}"#;
        let nadeo = r#"{"op":"1.0.0","ns":{"Game":{"CThing":{"i":"1"}}}}"#;
        let dir = scratch_dir("type-name-clash");
        std::fs::write(dir.join("OpenplanetCore.json"), core).unwrap();
        std::fs::write(dir.join("OpenplanetNext.json"), nadeo).unwrap();
        let mut index = TypeIndex::load_with_header(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
            None,
        )
        .unwrap();
        assert!(index.lookup_type("Game::CThing").is_some(), "fixture guard");

        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "CThing".to_string(),
                values: vec![("A".to_string(), 0)],
            }],
        });
        index.build_short_type_index();

        assert!(index.lookup_source_enum("CThing").is_none());
        assert_eq!(
            index.find_by_short_name("CThing"),
            ["Game::CThing"],
            "the existing source-visible bucket must stay unchanged"
        );
    }

    /// A header enum the parser found no values for is still a name the game
    /// accepts, so it is indexed; it simply contributes no bare values.
    #[test]
    fn a_header_enum_with_no_values_is_indexed_but_contributes_none() {
        let mut index = TypeIndex::new();
        index.add_header(&HeaderDatabase {
            module_enums: vec![HeaderEnum {
                name: "EEmpty".to_string(),
                values: vec![],
            }],
        });
        let info = index.lookup_source_enum("EEmpty").expect("indexed");
        assert!(info.values.is_empty());
        assert!(!info.anonymous);
    }

    /// The header is optional: an install without one must load exactly as
    /// it did before B046, not fail.
    #[test]
    fn a_missing_header_is_not_an_error() {
        let dir = scratch_dir("no-header");
        let src = fixture_dir();
        std::fs::copy(
            src.join("OpenplanetCore.json"),
            dir.join("OpenplanetCore.json"),
        )
        .unwrap();
        std::fs::copy(
            src.join("OpenplanetNext.json"),
            dir.join("OpenplanetNext.json"),
        )
        .unwrap();
        let index = TypeIndex::load(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
        )
        .expect("a dump without a header must still load");
        assert!(header_sourced(&index).is_empty());
        assert_eq!(index.enum_count(), load_without_header().enum_count());
    }

    #[test]
    fn an_invalid_utf8_adjacent_header_is_ignored_by_normal_load() {
        let dir = scratch_dir("invalid-utf8-header");
        let src = fixture_dir();
        std::fs::copy(
            src.join("OpenplanetCore.json"),
            dir.join("OpenplanetCore.json"),
        )
        .unwrap();
        std::fs::copy(
            src.join("OpenplanetNext.json"),
            dir.join("OpenplanetNext.json"),
        )
        .unwrap();
        std::fs::write(dir.join(HEADER_FILENAME), [0xff, 0xfe]).unwrap();

        let index = TypeIndex::load(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
        )
        .expect("an optional malformed header must not break normal type-db loading");
        assert!(header_sourced(&index).is_empty());
    }

    /// `load` finds the header beside the dump, so every existing caller
    /// that points at a directory gets it without changing its signature.
    #[test]
    fn load_picks_up_a_header_sitting_beside_the_dump() {
        assert_eq!(header_sourced(&load_fixture()).len(), 2);
        assert!(header_sourced(&load_without_header()).is_empty());
    }

    #[test]
    fn loading_the_same_files_twice_yields_identical_module_level_enums() {
        let a = load_fixture();
        let b = load_fixture();
        assert_eq!(header_sourced(&a), header_sourced(&b));
        for key in header_sourced(&a) {
            assert_eq!(
                a.lookup_source_enum(&key).unwrap().values,
                b.lookup_source_enum(&key).unwrap().values
            );
        }
        assert_eq!(a.enum_count(), b.enum_count());
    }

    /// `typedb diff` of the fixture against itself must stay empty — a
    /// header-sourced enum diffs on values and its source-facing bit, both
    /// of which are stable across loads.
    #[test]
    fn diff_of_the_fixture_against_itself_is_empty_with_the_header_loaded() {
        let diff = diff_indexes(&load_fixture(), &load_fixture());
        assert!(diff.is_empty(), "{diff:?}");
    }
}
