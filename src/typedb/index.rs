use std::collections::HashMap;
use std::path::Path;

use super::core_format::CoreDatabase;
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
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: Option<String>,
    pub type_name: String,
    pub default: Option<String>,
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

    fn add_core(&mut self, db: &CoreDatabase) {
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
                    })
                    .collect(),
                doc: func.desc.clone(),
                source: TypeSource::Core,
            };
            self.functions.entry(qname).or_default().push(info);
        }

        for cls in &db.classes {
            let qname = Self::qualify(&cls.ns, &cls.name);
            let info = TypeInfo {
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
                    })
                    .collect(),
                doc: cls.desc.clone(),
                source: TypeSource::Core,
            };
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
                                    })
                                    .collect(),
                                is_const: false,
                                doc: None,
                            });
                        }
                        NadeoMemberKind::Enum => {
                            if let (Some(type_name), Some(values)) = (
                                member.type_name(),
                                member_inline_enum_values(member.e.as_ref()),
                            ) {
                                self.register_nadeo_enum(
                                    qualify_nadeo_member_type_name(&db.ns, ns_name, type_name),
                                    values,
                                );
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

    pub fn function_count(&self) -> usize {
        self.functions.values().map(|v| v.len()).sum()
    }

    pub fn enum_count(&self) -> usize {
        self.enums.len()
    }
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
