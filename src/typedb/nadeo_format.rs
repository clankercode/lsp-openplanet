use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct NadeoDatabase {
    pub op: String,
    #[serde(default)]
    pub mp: Option<String>,
    pub ns: HashMap<String, HashMap<String, NadeoType>>,
}

#[derive(Debug, Deserialize)]
pub struct NadeoType {
    #[serde(default)]
    pub i: Option<String>,
    #[serde(default)]
    pub c: Option<u32>,
    #[serde(default, rename = "p")]
    pub parent: Option<String>,
    #[serde(default)]
    pub f: Option<String>,
    #[serde(default)]
    pub m: Vec<NadeoMember>,
    #[serde(default)]
    pub e: Option<Vec<NadeoEnumEntry>>,
    #[serde(default)]
    pub d: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct NadeoMember {
    pub n: String,
    #[serde(default)]
    pub i: Option<u32>,
    pub t: serde_json::Value,
    #[serde(default)]
    pub a: Option<String>,
    #[serde(default)]
    pub e: Option<serde_json::Value>,
    #[serde(default)]
    pub r: Option<serde_json::Value>,
    #[serde(default)]
    pub c: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct NadeoEnumEntry {
    pub n: String,
    #[serde(default)]
    pub v: Vec<String>,
}

impl NadeoMember {
    /// Discriminate member kind based on the `t` field type.
    pub fn kind(&self) -> NadeoMemberKind {
        if self.e.is_some() {
            NadeoMemberKind::Enum
        } else if self.t.is_number() {
            NadeoMemberKind::Method
        } else {
            NadeoMemberKind::Property
        }
    }

    /// Get type name for properties (t is a string)
    pub fn type_name(&self) -> Option<&str> {
        self.t.as_str()
    }

    /// Parse arguments string "Type1 name1, Type2 name2" into pairs
    pub fn parse_args(&self) -> Vec<(String, String)> {
        let Some(args_str) = &self.a else {
            return Vec::new();
        };
        if args_str.is_empty() {
            return Vec::new();
        }
        args_str
            .split(',')
            .filter_map(|part| {
                let trimmed = part.trim();
                let mut parts = trimmed.rsplitn(2, ' ');
                let name = parts.next()?.to_string();
                let ty = parts.next().unwrap_or("").to_string();
                Some((ty, name))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NadeoMemberKind {
    Property,
    Method,
    Enum,
}

impl NadeoDatabase {
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

    fn next_json_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/OpenplanetNext.json")
    }

    #[test]
    fn test_load_nadeo_json() {
        let path = next_json_path();
        if !path.exists() {
            panic!("OpenplanetNext.json not found at {:?}", path);
        }
        let db = NadeoDatabase::load_from_file(&path).unwrap();
        assert!(!db.op.is_empty());
        assert!(!db.ns.is_empty());
    }

    #[test]
    fn test_nadeo_has_known_types() {
        let path = next_json_path();
        if !path.exists() {
            panic!("OpenplanetNext.json not found at {:?}", path);
        }
        let db = NadeoDatabase::load_from_file(&path).unwrap();
        assert!(
            db.ns.contains_key("MwFoundations"),
            "expected MwFoundations namespace"
        );
        let mw = &db.ns["MwFoundations"];
        assert!(mw.contains_key("CMwNod"), "expected CMwNod class");
    }

    #[test]
    fn test_nadeo_member_discrimination() {
        let path = next_json_path();
        if !path.exists() {
            panic!("OpenplanetNext.json not found at {:?}", path);
        }
        let db = NadeoDatabase::load_from_file(&path).unwrap();
        let mw = &db.ns["MwFoundations"];
        let nod = &mw["CMwNod"];
        // CMwNod should have members
        assert!(!nod.m.is_empty());
        // Check that at least one property exists
        let has_prop = nod.m.iter().any(|m| m.kind() == NadeoMemberKind::Property);
        assert!(has_prop, "expected at least one property on CMwNod");
    }
}
