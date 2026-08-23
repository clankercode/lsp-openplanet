use std::collections::HashMap;
use std::path::Path;

use super::core_format::{CoreClass, CoreDatabase};
use super::nadeo_format::{NadeoDatabase, NadeoEnumEntry, NadeoMemberKind, NadeoType};

/// Merged type index combining Core API and Nadeo game engine types.
pub struct TypeIndex {
    /// All types keyed by fully qualified name (e.g., "Net::HttpRequest")
    types: HashMap<String, TypeInfo>,
    /// Global functions keyed by qualified name (e.g., "UI::Begin")
    functions: HashMap<String, Vec<FunctionInfo>>,
    /// Enums keyed by qualified name
    enums: HashMap<String, EnumInfo>,
    /// Short-name → list of fully qualified names that end with `::<short>`
    /// (or exactly match `<short>`). Built lazily by `ensure_short_index`.
    /// Covers types and enums — functions are keyed differently and this
    /// map is only consulted when resolving a referenced *type* name.
    short_type_index: HashMap<String, Vec<String>>,
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
    pub doc: Option<String>,
    pub source: TypeSource,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub return_type: String,
    pub params: Vec<ParamInfo>,
    pub is_const: bool,
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
    /// `&out` reference parameter (typedb `typeflags` bit 2): only an
    /// l-value argument can bind (GH #47).
    pub is_out: bool,
}

#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub name: String,
    pub namespace: Option<String>,
    pub values: Vec<(String, i64)>,
    pub source: TypeSource,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TypeSource {
    Core,
    Nadeo,
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
/// `funcdefs`/`props` are not part of [`TypeIndex`] — counting them here is
/// how the pipeline notices drift in entries the checker never loads.
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
            short_type_index: HashMap::new(),
        }
    }
}

impl TypeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(core_path: &Path, nadeo_path: &Path) -> Result<Self, String> {
        let mut index = Self::new();
        let core = CoreDatabase::load_from_file(core_path)?;
        index.add_core(&core);
        let nadeo = NadeoDatabase::load_from_file(nadeo_path)?;
        index.add_nadeo(&nadeo);
        index.build_short_type_index();
        Ok(index)
    }

    /// Rebuild the short-name → FQN index. Must be called after every
    /// `add_*` path once all insertions are done.
    fn build_short_type_index(&mut self) {
        self.short_type_index.clear();
        for qname in self.types.keys() {
            let short = qname.rsplit("::").next().unwrap_or(qname).to_string();
            self.short_type_index
                .entry(short)
                .or_default()
                .push(qname.clone());
        }
        for qname in self.enums.keys() {
            let short = qname.rsplit("::").next().unwrap_or(qname).to_string();
            self.short_type_index
                .entry(short)
                .or_default()
                .push(qname.clone());
        }
    }

    /// Return all fully qualified names of types/enums whose tail segment
    /// matches `short`. Used as a last-resort fallback when an unqualified
    /// reference fails to resolve under any active namespace prefix.
    ///
    /// The slice is empty when the short name is unknown. Callers should
    /// prefer the first match (stable insertion order is not guaranteed
    /// because of `HashMap`, but duplicate short names in the Nadeo DB are
    /// vanishingly rare — see tests).
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
        self.short_type_index.keys().cloned().collect()
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
                        // typedb typeflags bit 2 marks `&out` reference params
                        // (GH #47): only an l-value argument can bind.
                        is_out: a.typeflags & 0b10 != 0,
                    })
                    .collect(),
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
                                is_out: a.typeflags & 0b10 != 0,
                            })
                            .collect(),
                        is_const: m.is_const,
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
            values.sort_by_key(|(_, v)| *v);
            self.enums.insert(
                qname,
                EnumInfo {
                    name: en.name.clone(),
                    namespace: en.ns.clone(),
                    values,
                    source: TypeSource::Core,
                },
            );
        }
    }

    fn add_nadeo(&mut self, db: &NadeoDatabase) {
        for (ns_name, types) in &db.ns {
            for (type_name, nadeo_type) in types {
                let qname = format!("{}::{}", ns_name, type_name);
                let mut methods = Vec::new();
                let mut properties = Vec::new();

                for member in &nadeo_type.m {
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
                                        is_out: false,
                                    })
                                    .collect(),
                                is_const: false,
                                doc: None,
                            });
                        }
                        NadeoMemberKind::Enum => {
                            // Members with an inline `e` block are enum-typed
                            // *properties* (e.g. `CGameCtnBlock::Direction`).
                            // Register the enum for name resolution AND keep
                            // the property so member access returns the enum type.
                            if let (Some(type_name), Some(values)) = (
                                member.type_name(),
                                member_inline_enum_values(member.e.as_ref()),
                            ) {
                                self.register_nadeo_enum(
                                    qualify_nadeo_member_type_name(&db.ns, ns_name, type_name),
                                    values,
                                );
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
        let (namespace, name) = match qualified_name.rsplit_once("::") {
            Some((ns, tail)) => (Some(ns.to_string()), tail.to_string()),
            None => (None, qualified_name.clone()),
        };
        self.enums.entry(qualified_name).or_insert(EnumInfo {
            name,
            namespace,
            values,
            source: TypeSource::Nadeo,
        });
    }

    // === Query API ===

    pub fn lookup_type(&self, qualified_name: &str) -> Option<&TypeInfo> {
        self.types.get(qualified_name)
    }

    pub fn lookup_function(&self, qualified_name: &str) -> Option<&[FunctionInfo]> {
        self.functions.get(qualified_name).map(|v| v.as_slice())
    }

    pub fn lookup_enum(&self, qualified_name: &str) -> Option<&EnumInfo> {
        self.enums.get(qualified_name)
    }

    /// Iterate all known enums by qualified name.
    pub fn enums_iter(&self) -> impl Iterator<Item = (&String, &EnumInfo)> {
        self.enums.iter()
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

    /// Iterate all enums directly under `namespace` (qualified name → info).
    /// Used by namespace-shortcut completions when enum matching is enabled.
    pub fn namespace_enums(&self, namespace: &str) -> impl Iterator<Item = (&String, &EnumInfo)> {
        let prefix = format!("{}::", namespace);
        self.enums.iter().filter(move |(qname, _)| {
            qname
                .strip_prefix(&prefix)
                .is_some_and(|rest| !rest.contains("::"))
        })
    }

    /// Get all member names for namespace completion (e.g., after "UI::")
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
        for qname in self.enums.keys() {
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

    /// Get all known namespaces
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
        for en in self.enums.values() {
            if let Some(ns) = &en.namespace {
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
    use super::*;
    use std::path::PathBuf;

    fn core_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/OpenplanetCore.json")
    }

    fn next_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/OpenplanetNext.json")
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
