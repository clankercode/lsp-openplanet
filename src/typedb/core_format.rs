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
    /// B131: NEVER present in any dump measured — 0 of 75 classes in both the
    /// 1.29.5 fixture and an installed 1.29.14. Kept anyway, and that is a
    /// judgement rather than an oversight.
    ///
    /// It is an `Option`, so its default says "absent", not "no parent" — the
    /// distinction that made `is_const`'s `bool` default a lie and this one
    /// merely uninformative. It is also read: `TypeIndex` copies it into
    /// `TypeInfo::parent`, which `class_facts` renders as `inherits {parent}`
    /// in the `typedb diff` drift report.
    ///
    /// And the absence is most likely CORRECT rather than a gap. Core holds
    /// AngelScript-registered types (`string`, `array`, `Json::Value`,
    /// `MwFastBuffer`), which do not inherit; the engine class hierarchy lives
    /// in the Nadeo half, which populates `parent` by its own path. So there is
    /// nothing here to recover.
    ///
    /// Do not "fix" this by deriving a parent from somewhere else. If a dump
    /// ever does carry `inherits`, the audit test in this module fails and
    /// tells you — and the existing synthetic test at
    /// `nadeo_parent_shows_in_diff` already proves the plumbing works, which is
    /// exactly why nothing else noticed the key was absent from real data.
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
}

// B131: there is deliberately no `is_protected` here, and no access
// specifier of any kind.
//
// A `#[serde(rename = "protected", default)] is_protected: bool` used to sit
// on `CoreMethod`. It was DOUBLY dead: nothing read it, and no dump has ever
// carried the key. Measured across two dump versions — the shipped 1.29.5
// fixture and an installed 1.29.14 — the string `"protected"` does not occur
// anywhere in either file, and 0 of 1002 / 0 of 975 method `decl`s mention
// `protected` or `private`.
//
// So this is NOT the shape `is_const` turned out to be. Constness was
// recoverable from the `decl` suffix and had a real consumer whose output was
// wrong without it (B125), which is why that field was populated rather than
// removed. Access specifiers are recoverable from nothing, and nothing wants
// them. Populating would have produced the other half of the same defect: a
// field with no reader.
//
// If a future dump starts carrying access specifiers, the audit test in this
// module fails and says so — that is the intended way to find out, rather than
// a `bool` that has been quietly answering `false` in the meantime.

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
    #[serde(default)]
    pub d: Option<String>,
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

// ── B131: is every declared field ever actually populated? ──────────────────
//
// Two dead fields were found on this struct family by accident within one
// night — `CoreMethod::is_const` (B125) and `CoreMethod::is_protected` (B131)
// — which says the question had never been asked systematically. This module
// asks it, of every field of every struct here, on every run.
//
// It reads its OWN SOURCE for the struct definitions rather than a hand-copied
// list, so adding a field to any struct above automatically brings it under
// audit. A hardcoded list would have exactly the blind spot this exists to
// remove.
//
// It checks BOTH directions, because "is this field populated?" has a mirror
// image that is just as costly:
//
//   * DECLARED BUT NEVER PRESENT — a field that quietly takes its default.
//     Harmless for an `Option` (`None` honestly means "absent"); a lie for a
//     `bool`, which asserts `false` to every caller. That asymmetry is the
//     whole lesson of B125 and B131.
//   * PRESENT BUT NEVER DECLARED — data the dump ships and the loader drops
//     on the floor. `returntypeflags` is the live example: 1002 of 1002
//     methods carry it, it holds the by-value/by-reference signal that
//     `returntypedecl` throws away, and nothing reads it. It is NOT added
//     here on purpose — nothing consumes it yet, and adding it would create
//     the other half of the defect: a populated field with no reader. B130 is
//     the ticket that would give it one.
#[cfg(test)]
mod b131_dump_field_audit {
    /// Field names as serde will look for them (honouring `rename`), parsed
    /// from this file's own text.
    fn declared_fields(struct_name: &str) -> Vec<String> {
        let src = include_str!("core_format.rs");
        let head = format!("pub struct {struct_name} {{");
        let start = src
            .find(&head)
            .unwrap_or_else(|| panic!("struct {struct_name} not found in core_format.rs"))
            + head.len();
        let body = &src[start..];
        let end = body
            .find("\n}")
            .unwrap_or_else(|| panic!("unterminated struct {struct_name}"));
        let mut fields = Vec::new();
        let mut rename: Option<String> = None;
        for line in body[..end].lines() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if let Some(rest) = t.strip_prefix("#[serde(rename = \"") {
                if let Some(name) = rest.split('"').next() {
                    rename = Some(name.to_string());
                }
                continue;
            }
            if let Some(rest) = t.strip_prefix("pub ") {
                if let Some((name, _)) = rest.split_once(':') {
                    fields.push(rename.take().unwrap_or_else(|| name.to_string()));
                }
            }
        }
        assert!(
            !fields.is_empty(),
            "parsed no fields for {struct_name} — the parser has drifted from the source"
        );
        fields
    }

    fn core() -> serde_json::Value {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        serde_json::from_str(&std::fs::read_to_string(p).expect("fixture must exist"))
            .expect("fixture must parse")
    }

    fn arr<'a>(v: &'a serde_json::Value, key: &str) -> Vec<&'a serde_json::Value> {
        v[key].as_array().expect("array").iter().collect()
    }

    /// Every `pub struct` declared in this file, parsed from source. The
    /// audit table must cover all of them — see
    /// `the_audit_covers_every_struct_in_this_file`, which exists because
    /// the first draft of this module silently covered 7 of 11.
    fn all_struct_names() -> Vec<String> {
        include_str!("core_format.rs")
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub struct "))
            .filter_map(|r| r.split_whitespace().next())
            .map(|s| s.to_string())
            .collect()
    }

    /// One row of the audit: the struct's name, the dump objects it is
    /// deserialized from, the fields expected to be absent from those
    /// objects, and the keys those objects carry that it does not declare.
    type AuditRow = (
        &'static str,
        Vec<serde_json::Value>,
        &'static [&'static str],
        &'static [&'static str],
    );

    fn audit_table(db: &serde_json::Value) -> Vec<AuditRow> {
        let classes = arr(db, "classes");
        let nested = |key: &str| -> Vec<serde_json::Value> {
            classes
                .iter()
                .flat_map(|c| c[key].as_array().cloned().unwrap_or_default())
                .collect()
        };
        let methods = nested("methods");
        let behaviors = nested("behaviors");
        let behavior_funcs: Vec<serde_json::Value> = behaviors
            .iter()
            .filter_map(|b| b.get("func").filter(|f| f.is_object()).cloned())
            .collect();
        let args: Vec<serde_json::Value> = methods
            .iter()
            .flat_map(|m| m["args"].as_array().cloned().unwrap_or_default())
            .collect();
        let enum_values: Vec<serde_json::Value> = arr(db, "enums")
            .iter()
            .flat_map(|e| {
                e["values"]
                    .as_object()
                    .map(|o| o.values().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .filter(|v| v.is_object())
            .collect();
        let own = |v: &[&serde_json::Value]| -> Vec<serde_json::Value> {
            v.iter().map(|x| (*x).clone()).collect()
        };
        vec![
            // The root object. Top-level `props` is knowingly dropped —
            // `diff_indexes` documents it as count-only via `TypeDbCounts`.
            ("CoreDatabase", vec![db.clone()], &[], &["props"]),
            (
                "CoreFunction",
                own(&arr(db, "functions")),
                &[],
                &["flags", "returntypeflags", "returntypeid", "returntypename"],
            ),
            ("CoreArg", args, &[], &["desc", "typeid", "typename"]),
            // `inherits` is the one accepted dead field, and it is an
            // `Option` whose `None` honestly means "absent". See its doc
            // comment — Core holds registered types, which do not inherit.
            (
                "CoreClass",
                own(&classes),
                &["inherits"],
                &["flags", "group"],
            ),
            ("CoreBehavior", behaviors, &[], &[]),
            // Only the CONSTRUCT behaviour's shape is consumed (GH #54), so
            // most of the func object is deliberately unread.
            (
                "CoreBehaviorFunc",
                behavior_funcs,
                &[],
                &[
                    "desc",
                    "flags",
                    "group",
                    "name",
                    "returntypedecl",
                    "returntypeflags",
                    "returntypeid",
                    "returntypename",
                ],
            ),
            (
                "CoreMethod",
                methods,
                &[],
                &[
                    "flags",
                    "group",
                    "returntypeflags",
                    "returntypeid",
                    "returntypename",
                ],
            ),
            (
                "CoreProp",
                nested("props"),
                &[],
                &["flags", "group", "typeflags", "typeid", "typename"],
            ),
            (
                "CoreFuncdef",
                own(&arr(db, "funcdefs")),
                &[],
                &["flags", "group", "id", "returntypeflags"],
            ),
            (
                "CoreEnum",
                own(&arr(db, "enums")),
                &[],
                &["desc", "flags", "group"],
            ),
            // B133: `d` is the enum VALUE's doc string. It is loaded together
            // with its hover/completion consumers; neither half lands alone.
            ("EnumValue", enum_values, &[], &[]),
        ]
    }

    /// The hole this module was written to remove, applied to itself. The
    /// first draft audited 7 of the 11 structs in this file and the omission
    /// was invisible — which is precisely the failure mode being audited.
    #[test]
    fn the_audit_covers_every_struct_in_this_file() {
        let db = core();
        let covered: std::collections::BTreeSet<String> = audit_table(&db)
            .into_iter()
            .map(|(name, ..)| name.to_string())
            .collect();
        let declared: std::collections::BTreeSet<String> = all_struct_names().into_iter().collect();
        assert!(
            declared.len() >= 11,
            "parsed only {} structs",
            declared.len()
        );
        assert_eq!(
            declared.difference(&covered).cloned().collect::<Vec<_>>(),
            Vec::<String>::new(),
            "a struct in this file is not in the audit table — add it, or the \
             next dead field in it goes unnoticed the way `is_const` and \
             `is_protected` both did"
        );
    }

    /// THE AUDIT. Each row is `(struct, expected-dead, expected-undeclared)`.
    ///
    /// A row changing is not automatically a bug — it means the dump's shape
    /// moved, and someone has to decide what that means. That decision is the
    /// point; the failure is how it gets made deliberately instead of by
    /// accident three tickets later.
    #[test]
    fn every_declared_field_is_populated_and_every_populated_key_is_declared() {
        let db = core();
        for (name, objs, want_dead, want_undeclared) in audit_table(&db) {
            assert!(!objs.is_empty(), "{name}: no objects to audit");
            let declared: std::collections::BTreeSet<String> =
                declared_fields(name).into_iter().collect();
            let mut present = std::collections::BTreeSet::new();
            for o in &objs {
                for k in o.as_object().expect("object").keys() {
                    present.insert(k.clone());
                }
            }
            let dead: Vec<String> = declared.difference(&present).cloned().collect();
            let undeclared: Vec<String> = present.difference(&declared).cloned().collect();
            assert_eq!(
                dead, want_dead,
                "{name}: fields declared but NEVER present in the dump. A `bool` \
                 here silently answers `false` to every caller (B125); an \
                 `Option` merely says `None`. Populate it, delete it, or \
                 document why it stays."
            );
            assert_eq!(
                undeclared, want_undeclared,
                "{name}: keys the dump ships that the loader drops. Adding one \
                 is only an improvement if something READS it — trace the \
                 consumer before deciding (B125)."
            );
        }
    }

    /// `is_protected` is gone and must not come back on a hunch. Measured
    /// across the 1.29.5 fixture and an installed 1.29.14: the string
    /// `"protected"` does not appear anywhere in either dump, and no `decl`
    /// mentions `protected` or `private`. Unlike constness, there is nothing
    /// to recover it from.
    #[test]
    fn no_access_specifier_exists_anywhere_in_the_dump() {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let raw = std::fs::read_to_string(p).expect("fixture must exist");
        for needle in ["\"protected\"", "\"private\""] {
            assert!(
                !raw.contains(needle),
                "{needle} now appears in the dump — engine access specifiers \
                 became available; decide whether anything should read them \
                 before adding a field (B131)"
            );
        }
        let db = core();
        let with_specifier = arr(&db, "classes")
            .iter()
            .flat_map(|c| c["methods"].as_array().cloned().unwrap_or_default())
            .filter(|m| {
                m["decl"]
                    .as_str()
                    .is_some_and(|d| d.contains("protected") || d.contains("private"))
            })
            .count();
        assert_eq!(
            with_specifier, 0,
            "a `decl` now carries an access specifier — it would be recoverable \
             the way ` const` was (B125)"
        );
    }
}
