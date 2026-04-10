use std::collections::HashMap;
use std::path::Path;

use super::core_format::{CoreDatabase};
use super::nadeo_format::{NadeoDatabase, NadeoMemberKind};

/// Merged type index combining Core API and Nadeo game engine types.
pub struct TypeIndex {
    /// All types keyed by fully qualified name (e.g., "Net::HttpRequest")
    types: HashMap<String, TypeInfo>,
    /// Global functions keyed by qualified name (e.g., "UI::Begin")
    functions: HashMap<String, Vec<FunctionInfo>>,
    /// Enums keyed by qualified name
    enums: HashMap<String, EnumInfo>,
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

impl TypeIndex {
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            functions: HashMap::new(),
            enums: HashMap::new(),
        }
    }

    pub fn load(core_path: &Path, nadeo_path: &Path) -> Result<Self, String> {
        let mut index = Self::new();
        let core = CoreDatabase::load_from_file(core_path)?;
        index.add_core(&core);
        let nadeo = NadeoDatabase::load_from_file(nadeo_path)?;
        index.add_nadeo(&nadeo);
        Ok(index)
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
            let mut values: Vec<_> = en.values.iter().map(|(k, v)| (k.clone(), *v)).collect();
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
                            // Nested enum — add as enum type
                        }
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
                self.types.insert(qname, info);
            }
        }
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

    /// Get all member names for namespace completion (e.g., after "UI::")
    pub fn namespace_members(&self, namespace: &str) -> Vec<String> {
        let mut members = Vec::new();
        for (qname, _) in &self.types {
            if let Some(name) = qname.strip_prefix(namespace).and_then(|s| s.strip_prefix("::")) {
                if !name.contains("::") {
                    members.push(name.to_string());
                }
            }
        }
        for (qname, _) in &self.functions {
            if let Some(name) = qname.strip_prefix(namespace).and_then(|s| s.strip_prefix("::")) {
                if !name.contains("::") && !members.contains(&name.to_string()) {
                    members.push(name.to_string());
                }
            }
        }
        for (qname, _) in &self.enums {
            if let Some(name) = qname.strip_prefix(namespace).and_then(|s| s.strip_prefix("::")) {
                if !name.contains("::") && !members.contains(&name.to_string()) {
                    members.push(name.to_string());
                }
            }
        }
        members.sort();
        members
    }

    /// Get all known namespaces
    pub fn namespaces(&self) -> Vec<String> {
        let mut nss: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_, info) in &self.types {
            if let Some(ns) = &info.namespace {
                nss.insert(ns.clone());
            }
        }
        for (_, fns) in &self.functions {
            for f in fns {
                if let Some(ns) = &f.namespace {
                    nss.insert(ns.clone());
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn core_path() -> PathBuf {
        PathBuf::from(env!("HOME")).join("src/openplanet/tm-scripts/OpenplanetCore.json")
    }

    fn next_path() -> PathBuf {
        PathBuf::from(env!("HOME")).join("src/openplanet/tm-scripts/OpenplanetNext.json")
    }

    #[test]
    fn test_merged_index() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() { return; }
        let index = TypeIndex::load(&cp, &np).unwrap();
        assert!(index.type_count() > 100, "expected many types, got {}", index.type_count());
        assert!(index.function_count() > 50, "expected many functions, got {}", index.function_count());
    }

    #[test]
    fn test_namespace_members_ui() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() { return; }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let ui_members = index.namespace_members("UI");
        assert!(!ui_members.is_empty(), "expected UI namespace members");
    }

    #[test]
    fn test_lookup_cmwnod() {
        let cp = core_path();
        let np = next_path();
        if !cp.exists() || !np.exists() { return; }
        let index = TypeIndex::load(&cp, &np).unwrap();
        let nod = index.lookup_type("MwFoundations::CMwNod");
        assert!(nod.is_some(), "expected CMwNod type");
    }
}
