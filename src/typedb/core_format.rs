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
    #[serde(default)]
    pub funcdefs: Vec<CoreFuncdef>,
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
    pub typeflags: u32,
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
    pub behaviors: Vec<CoreBehavior>,
    #[serde(default)]
    pub desc: Option<String>,
}

/// A registered object behavior. `kind` is AngelScript's `asEBehaviours`
/// discriminant — `0` is `asBEHAVE_CONSTRUCT` (value-type constructor),
/// `2` is `asBEHAVE_DESTRUCT`, `3` is `asBEHAVE_FACTORY` (reference-type
/// factory). Only CONSTRUCT is consumed today (GH #54).
#[derive(Debug, Deserialize)]
pub struct CoreBehavior {
    #[serde(rename = "type", default)]
    pub kind: u32,
    #[serde(default)]
    pub func: Option<CoreBehaviorFunc>,
}

#[derive(Debug, Deserialize)]
pub struct CoreBehaviorFunc {
    #[serde(default)]
    pub args: Vec<CoreArg>,
    #[serde(default)]
    pub decl: Option<String>,
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
    #[serde(rename = "protected", default)]
    pub is_protected: bool,
}

impl CoreMethod {
    /// B125: does this method carry a trailing `const` qualifier?
    ///
    /// Read from `decl`, which is the only place the dumps put it. There
    /// used to be a `#[serde(rename = "const")] is_const: bool` field here
    /// and it was DEAD: measured against the shipped 1.29.5 Core dump, 0 of
    /// 1002 class methods carry a `const` key, so `serde(default)` made it
    /// answer `false` for every engine method on earth. Nothing
    /// distinguished "no engine method is const" from "we never read this",
    /// and its one consumer — the `typedb diff` drift report — silently
    /// rendered every engine signature as non-const.
    ///
    /// The suffix is the whole rule, and it has to be the SUFFIX rather
    /// than a substring search. Measured on the same dump:
    ///
    /// * 301 of 1002 decls end in ` const` — the real answer.
    /// * 352 contain `const` only inside a PARAMETER
    ///   (`bool opEquals(const MwId&in)`); a `contains("const")` test marks
    ///   every one of them const, which is the trap this avoids.
    /// * 26 begin with `const ` because the RETURN type is const
    ///   (`const Json::Value@ opIndex(int) const`). A prefix test reads
    ///   return-constness as method-constness. All 26 happen to also be
    ///   method-const here, so a prefix test would look right on this dump
    ///   and be wrong on the next one.
    /// * 0 use a spaceless `)const`, and 0 lack a `decl` entirely.
    /// * 0 of 854 free functions end in ` const`, which is the sanity
    ///   check that the suffix means what it should — a free function
    ///   cannot carry the qualifier.
    ///
    /// This says what the dump said and nothing more. It is NOT wired into
    /// the const-receiver diagnostics — see B124 and the follow-up ticket;
    /// doing that changes what those rules can see and needs its own
    /// measured sweep.
    pub fn declared_const(&self) -> bool {
        self.decl
            .as_deref()
            .is_some_and(|d| d.trim_end().ends_with(" const"))
    }
}

#[derive(Debug, Deserialize)]
pub struct CoreProp {
    pub name: String,
    #[serde(default)]
    pub typedecl: String,
    #[serde(default)]
    pub desc: Option<String>,
}

/// A Core `funcdefs[]` entry. Same `ns` / `name` / `returntypedecl` /
/// `args` / `decl` shape as [`CoreFunction`]; AngelScript uses the name
/// as a constructor that wraps a compatible function (`UI::InputTextCallback(fn)`).
#[derive(Debug, Deserialize)]
pub struct CoreFuncdef {
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

    #[test]
    fn test_core_funcdefs_deserialized() {
        let path = core_json_path();
        if !path.exists() {
            panic!("OpenplanetCore.json not found at {:?}", path);
        }
        let db = CoreDatabase::load_from_file(&path).unwrap();
        assert_eq!(db.funcdefs.len(), 12, "expected 12 Core funcdefs");
        assert!(
            db.funcdefs
                .iter()
                .any(|f| f.ns.as_deref() == Some("UI") && f.name == "InputTextCallback"),
            "expected UI::InputTextCallback"
        );
        assert!(
            db.funcdefs.iter().any(|f| f.name == "CoroutineFunc"),
            "expected un-namespaced CoroutineFunc"
        );
        assert!(
            db.funcdefs
                .iter()
                .any(|f| f.ns.as_deref() == Some("Dev") && f.name == "ProcIntercept"),
            "expected Dev::ProcIntercept"
        );
    }
}
