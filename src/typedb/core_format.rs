use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct CoreDatabase {
    pub op: String,
    #[serde(default)]
    pub functions: Vec<CoreFunction>,
    #[serde(default)]
    pub classes: Vec<CoreClass>,
    #[serde(default)]
    pub enums: Vec<CoreEnum>,
}

#[derive(Debug, Deserialize)]
pub struct CoreFunction {
    #[serde(default)]
    pub ns: Option<String>,
    pub name: String,
    #[serde(default)]
    pub returntypedecl: String,
    #[serde(default)]
    pub args: Vec<CoreArg>,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub decl: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CoreArg {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub typedecl: String,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CoreClass {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub ns: Option<String>,
    pub name: String,
    #[serde(default)]
    pub inherits: Option<String>,
    #[serde(default)]
    pub methods: Vec<CoreMethod>,
    #[serde(default)]
    pub props: Vec<CoreProp>,
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CoreMethod {
    pub name: String,
    #[serde(default)]
    pub returntypedecl: String,
    #[serde(default)]
    pub args: Vec<CoreArg>,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub decl: Option<String>,
    #[serde(rename = "const", default)]
    pub is_const: bool,
    #[serde(rename = "protected", default)]
    pub is_protected: bool,
}

#[derive(Debug, Deserialize)]
pub struct CoreProp {
    pub name: String,
    #[serde(default)]
    pub typedecl: String,
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CoreEnum {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub ns: Option<String>,
    pub name: String,
    #[serde(default)]
    pub values: HashMap<String, EnumValue>,
}

#[derive(Debug, Deserialize)]
pub struct EnumValue {
    pub v: i64,
}

impl CoreDatabase {
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn core_json_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/OpenplanetCore.json")
    }

    #[test]
    fn test_load_core_json() {
        let path = core_json_path();
        if !path.exists() {
            panic!("OpenplanetCore.json not found at {:?}", path);
        }
        let db = CoreDatabase::load_from_file(&path).unwrap();
        assert!(!db.op.is_empty());
        assert!(!db.functions.is_empty(), "expected functions");
        assert!(!db.classes.is_empty(), "expected classes");
        assert!(!db.enums.is_empty(), "expected enums");
    }

    #[test]
    fn test_core_has_known_namespaces() {
        let path = core_json_path();
        if !path.exists() {
            panic!("OpenplanetCore.json not found at {:?}", path);
        }
        let db = CoreDatabase::load_from_file(&path).unwrap();
        let func_nss: std::collections::HashSet<_> = db
            .functions
            .iter()
            .filter_map(|f| f.ns.as_deref())
            .collect();
        assert!(func_nss.contains("UI"), "expected UI namespace");
        assert!(func_nss.contains("Net"), "expected Net namespace");
    }

    #[test]
    fn test_core_ui_begin() {
        let path = core_json_path();
        if !path.exists() {
            panic!("OpenplanetCore.json not found at {:?}", path);
        }
        let db = CoreDatabase::load_from_file(&path).unwrap();
        let ui_begin = db
            .functions
            .iter()
            .find(|f| f.ns.as_deref() == Some("UI") && f.name == "Begin");
        assert!(ui_begin.is_some(), "expected UI::Begin function");
    }
}
