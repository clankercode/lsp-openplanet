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

/// One engine class. Every field below is documented from the SHIPPED dump
/// (2585 classes, 16321 members) rather than from a description of it — this
/// file carried no field documentation at all before B153, which is how two
/// keys stayed undeclared and seven stayed unread.
#[derive(Debug, Deserialize)]
pub struct NadeoType {
    /// Gbx class id as a decimal string (`CMwNod` = `"1001000"`). Present on
    /// all 2585 classes. Identifies the class in the Gbx file format; it is
    /// not part of the script API surface.
    #[serde(default)]
    pub i: Option<String>,
    /// A presence flag on 869 classes. ALWAYS the literal `1` — measured
    /// across every class in the dump — so it carries one bit and the dump
    /// gives no clue which bit. Meaning undetermined; see B153.
    #[serde(default)]
    pub c: Option<u32>,
    /// Base class, as a bare short name (1997 classes).
    #[serde(default, rename = "p")]
    pub parent: Option<String>,
    /// Gbx file this class is the root of (`"Title.Gbx"`, `"Menu.Gbx"`),
    /// on 314 classes. A file-format association, not script API surface.
    #[serde(default)]
    pub f: Option<String>,
    /// The class's members (1465 classes have any).
    #[serde(default)]
    pub m: Vec<NadeoMember>,
    /// Named nested enums (134 classes).
    #[serde(default)]
    pub e: Option<Vec<NadeoEnumEntry>>,
    /// Class documentation, on 190 classes: `{"d": <class description>,
    /// "o": [[member_i, message, kind, _], ...]}`. 143 carry a description
    /// and 189 carry an `o` list, 829 entries in total.
    ///
    /// `o[0]` is a member's `i` — all 829 resolve, none dangle — and `o[2]`
    /// is a kind: 570 are plain notes ("Is title present in WorkTitles
    /// folder?") and **259 are deprecation/migration advice** ("Use
    /// TextSizeReal instead", "Use RelativeOffset property instead.").
    ///
    /// So this dump DOES carry per-member deprecation metadata, contrary to
    /// what GH #18's notes have said. Nothing reads it yet — the field and
    /// its consumer must land together (B125), so that is its own ticket.
    #[serde(default)]
    pub d: Option<serde_json::Value>,
    /// Instance size in bytes; `0` on 23 classes that do not record one.
    ///
    /// Undeclared until B153, so serde discarded it on all 2585 classes.
    /// The reading is structurally verified rather than assumed: across
    /// 1978 parent links with a non-zero child, no class is ever smaller
    /// than its base, and 2443 of 2585 values are multiples of 8. It is C++
    /// memory layout, so nothing in a language server should consume it —
    /// but it is declared, because a key nobody declared is a key nobody
    /// can see.
    #[serde(default)]
    pub s: Option<u64>,
}

/// One member of an engine class: a property, a method, or an enum-typed
/// property with an inline enum block.
#[derive(Debug, Deserialize)]
pub struct NadeoMember {
    /// Member name.
    pub n: String,
    /// The member's index within its class, and the JOIN KEY for the
    /// owning class's `d.o` notes. It is NOT reliably the positional index
    /// — it matches position for 16145 of 16321 members and diverges for
    /// 176 — so the notes must be resolved through this field rather than
    /// by counting.
    #[serde(default)]
    pub i: Option<u32>,
    /// Kind discriminator. A STRING is a property's type name; a NUMBER
    /// means a method, where `1` documents a signature (`a` and `r` both
    /// present, 1618 members) and `0` does not (neither present, 1466).
    /// The correspondence is exact in the shipped dump.
    pub t: serde_json::Value,
    /// A method's parameter list as one flat string, `"Type name, Type
    /// name"` (1618 members). There is no per-parameter key anywhere in
    /// this format, which is why parameter direction is unknown (B146).
    #[serde(default)]
    pub a: Option<String>,
    /// An inline `{"n": name, "v": [...]}` enum block, marking this member
    /// an enum-typed property (683 members).
    #[serde(default)]
    pub e: Option<serde_json::Value>,
    /// For a METHOD, the return type as a plain type-name string — `"void"`,
    /// `"bool"`, `"CWebServicesTaskResult@"`, 120 distinct values across
    /// 1618 methods. For a PROPERTY, a 2-element numeric range such as
    /// `[0, 1]` (593 members). Never anything else.
    ///
    /// The loader has never read it: it writes `String::new()` for every
    /// Nadeo return type behind a comment claiming the format "uses type
    /// IDs", which is measurably false. Consuming it changes what call
    /// expressions resolve to, so it lands with its consumer (B125) in its
    /// own ticket rather than here.
    #[serde(default)]
    pub r: Option<serde_json::Value>,
    /// A presence flag on 3831 members. ALWAYS the literal `1`. It is not
    /// method constness — it sits on 1943 properties and 264 enum members
    /// but only 4 of 3084 methods (B137). Meaning undetermined; see B153.
    #[serde(default)]
    pub c: Option<u32>,
    /// A second presence flag, on 5036 members. ALWAYS the literal `1`, and
    /// INDEPENDENT of `c` — all four combinations of the two occur, across
    /// all three member kinds — so it is not a spelling of `c`.
    ///
    /// Undeclared until B153, so serde discarded it 5036 times. Meaning
    /// undetermined: it is declared so that it is visible, and deliberately
    /// not consumed, because guessing what a bit means is how B146 nearly
    /// shipped 57 false positives.
    #[serde(default)]
    pub m: Option<u32>,
}

/// One note the dump attaches to a member: its text, and whether the dump
/// classifies it as migration advice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NadeoMemberNote {
    pub message: String,
    /// `kind == 1` in the dump. Verified against all 829 notes rather than a
    /// sample: of the 259 marked, the ones that do not match a crude
    /// "use X instead" regex are still migration advice phrased without it
    /// ("Use Track_Context_MenuStart"), and of the 570 unmarked, every regex
    /// hit is a false one ("desires to spectate instead of playing"). So the
    /// flag discriminates and the regex does not — which is why this reads
    /// the flag.
    pub is_migration: bool,
}

impl NadeoType {
    /// The class description from `d.d`, or `None` when absent or empty.
    ///
    /// 143 classes carry the key and 133 a non-empty string; the other 10 are
    /// `""`, which is an absent description written a different way and must
    /// not reach hover as an empty doc block.
    pub fn class_description(&self) -> Option<&str> {
        let text = self.d.as_ref()?.get("d")?.as_str()?.trim();
        (!text.is_empty()).then_some(text)
    }

    /// Member notes from `d.o`, keyed by the member's `i`.
    ///
    /// THE KEY IS `i`, NEVER THE POSITION. 176 members have an `i` that
    /// differs from their index and 59 notes land on one of them, so a
    /// positional join does not merely miss those notes — it attaches
    /// someone else's, which is worse than dropping them.
    ///
    /// Parsed defensively on purpose: a malformed or reshaped entry is
    /// skipped rather than failing the load. Every note in both shipped
    /// dumps is a 4-element `[i, message, kind, _]`, and that shape is
    /// pinned by a test, so a change shows up there rather than by taking
    /// the language server offline.
    pub fn member_notes(&self) -> HashMap<u32, NadeoMemberNote> {
        let mut out = HashMap::new();
        let Some(entries) = self
            .d
            .as_ref()
            .and_then(|d| d.get("o"))
            .and_then(|o| o.as_array())
        else {
            return out;
        };
        for entry in entries {
            let Some(fields) = entry.as_array() else {
                continue;
            };
            let (Some(i), Some(message)) = (
                fields.first().and_then(|v| v.as_u64()),
                fields.get(1).and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let message = message.trim();
            if message.is_empty() {
                continue;
            }
            let is_migration = fields.get(2).and_then(|v| v.as_u64()) == Some(1);
            out.insert(
                i as u32,
                NadeoMemberNote {
                    message: message.to_string(),
                    is_migration,
                },
            );
        }
        out
    }
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

    /// The method's return type from `r`, as a plain type-name string.
    ///
    /// `r` is TWO different things depending on the member: on a method it is
    /// the return type (`"void"`, `"bool"`, `"CWebServicesTaskResult@"` —
    /// 120 distinct values across 1618 methods); on a PROPERTY it is a
    /// 2-element numeric range such as `[0, 1]` (593 members). Only the
    /// string form is a type, so the `as_str` is the discriminator and not a
    /// convenience — reading `r` unconditionally would turn a slider range
    /// into a return type.
    ///
    /// `None` for the 1466 methods that document no signature (`t == 0`,
    /// neither `a` nor `r`). That is a real absence and must stay one: the
    /// empty return type is what makes a call to them unchecked rather than
    /// wrongly checked.
    pub fn return_type_name(&self) -> Option<&str> {
        let text = self.r.as_ref()?.as_str()?.trim();
        (!text.is_empty()).then_some(text)
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

/// B153: a two-directional field audit for this loader, the mirror of B131's
/// audit on `core_format.rs` — the file B131 never covered.
///
/// B131 asked one question: is every declared field actually populated by the
/// dump? That catches a field we invented. It cannot catch the opposite
/// defect, which is what this file had two of: a key the dump carries that
/// the loader never declares (so serde silently drops it), and a field the
/// loader declares, parses, and then never reads.
///
/// Five instances of "the loader quietly answers a question the dump never
/// asked" were found by hand in this batch — B125, B131, B137, B142, B146.
/// This module exists so the sixth is found by a machine.
///
/// It scores the VALUE WRITTEN rather than the type declared, per B138's
/// near-miss: `Some(false)` asserts exactly as much as a bare `false`, so a
/// check that only looks at the declaration learns nothing.
#[cfg(test)]
mod b153_nadeo_dump_field_audit {
    use std::collections::BTreeSet;

    const SRC: &str = include_str!("nadeo_format.rs");

    /// Field names as serde will look for them (honouring `rename`), parsed
    /// from this file's own text rather than a hand-copied list — a list
    /// maintained by hand drifts silently, which is the whole failure mode.
    /// Returns `(serde_name, rust_name)`. The two differ wherever a field is
    /// `#[serde(rename)]`d, and conflating them makes the audit wrong in both
    /// directions at once: the dump-key checks need the WIRE name, while the
    /// is-it-read check needs the name the Rust code actually types. The
    /// first draft of this module used the wire name for both and reported
    /// `NadeoType.p` as never read when `.parent` is read constantly.
    fn declared_fields_pairs(struct_name: &str) -> Vec<(String, String)> {
        let head = format!("pub struct {struct_name} {{");
        let start = SRC
            .find(&head)
            .unwrap_or_else(|| panic!("struct {struct_name} not found in nadeo_format.rs"))
            + head.len();
        let body = &SRC[start..];
        let end = body
            .find("\n}")
            .unwrap_or_else(|| panic!("unterminated struct {struct_name}"));
        let mut fields = Vec::new();
        let mut rename: Option<String> = None;
        for line in body[..end].lines() {
            let t = line.trim();
            if t.starts_with("//") || t.starts_with("///") {
                continue;
            }
            if let Some(rest) = t.strip_prefix("#[serde(default, rename = \"") {
                if let Some(name) = rest.split('"').next() {
                    rename = Some(name.to_string());
                }
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
                    let rust = name.trim().to_string();
                    let wire = rename.take().unwrap_or_else(|| rust.clone());
                    fields.push((wire, rust));
                }
            }
        }
        assert!(
            !fields.is_empty(),
            "parsed no fields for {struct_name} — the parser has drifted from the source"
        );
        fields
    }

    /// Wire names only — what serde looks for in the dump.
    fn declared_fields(struct_name: &str) -> Vec<String> {
        declared_fields_pairs(struct_name)
            .into_iter()
            .map(|(wire, _)| wire)
            .collect()
    }

    /// Every `pub struct` in this file, parsed from source, so a struct added
    /// later joins the audit without anyone remembering to add it.
    /// Only structs serde DESERIALIZES from the dump, identified by the
    /// `Deserialize` derive immediately above them.
    ///
    /// B158 added `NadeoMemberNote`, which is derived from the dump rather
    /// than read out of it, and the coverage test failed — correctly, on the
    /// rule as first written, but for the wrong reason. A type nobody
    /// deserializes has no dump keys to audit, so the question the audit asks
    /// does not apply to it. The derive is the discriminator, read from the
    /// source, so this stays honest as types are added either way.
    fn all_struct_names() -> Vec<String> {
        let mut out = Vec::new();
        let mut derives_deserialize = false;
        for line in SRC.lines() {
            let t = line.trim();
            if t.starts_with("#[derive(") {
                derives_deserialize = t.contains("Deserialize");
                continue;
            }
            if let Some(rest) = t.strip_prefix("pub struct ") {
                if derives_deserialize {
                    if let Some(name) = rest.split_whitespace().next() {
                        out.push(name.trim_end_matches('{').to_string());
                    }
                }
                derives_deserialize = false;
            }
        }
        assert!(
            out.len() >= 4,
            "parsed only {} deserialized structs — the parser has drifted",
            out.len()
        );
        out
    }

    fn nadeo() -> serde_json::Value {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        serde_json::from_str(&std::fs::read_to_string(p).expect("fixture must exist"))
            .expect("fixture must parse")
    }

    /// The keys each struct's dump objects actually carry, gathered from the
    /// shipped dump rather than from a description of it.
    fn dump_keys(db: &serde_json::Value) -> Vec<(&'static str, BTreeSet<String>)> {
        let mut database = BTreeSet::new();
        let mut types = BTreeSet::new();
        let mut members = BTreeSet::new();
        let mut entries = BTreeSet::new();
        for k in db.as_object().expect("root object").keys() {
            database.insert(k.clone());
        }
        for ns in db["ns"].as_object().expect("ns object").values() {
            for class in ns.as_object().expect("class map").values() {
                for k in class.as_object().expect("class object").keys() {
                    types.insert(k.clone());
                }
                if let Some(ms) = class.get("m").and_then(|m| m.as_array()) {
                    for m in ms {
                        for k in m.as_object().expect("member object").keys() {
                            members.insert(k.clone());
                        }
                    }
                }
                if let Some(es) = class.get("e").and_then(|e| e.as_array()) {
                    for e in es {
                        for k in e.as_object().expect("enum entry").keys() {
                            entries.insert(k.clone());
                        }
                    }
                }
            }
        }
        vec![
            ("NadeoDatabase", database),
            ("NadeoType", types),
            ("NadeoMember", members),
            ("NadeoEnumEntry", entries),
        ]
    }

    /// Production text of every loader in `src/typedb/`, each file cut at its
    /// first test module so a field "read" only by a test does not count as
    /// consumed. Reading the directory rather than a list means a new loader
    /// file joins the audit on its own.
    fn production_loader_text() -> String {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/typedb");
        let mut out = String::new();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("src/typedb must exist")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .collect();
        files.sort();
        assert!(files.len() >= 3, "only {} loader files found", files.len());
        for f in files {
            let text = std::fs::read_to_string(&f).expect("loader must read");
            let cut = text.find("#[cfg(test)]").unwrap_or(text.len());
            out.push_str(&text[..cut]);
            out.push('\n');
        }
        out
    }

    /// True when production code anywhere in `src/typedb/` reads `.field`.
    /// The word boundary matters: these are one-letter names, so a naive
    /// `.contains(".m")` would match `.map(` and score an unread field as
    /// read — an audit that lies in the reassuring direction.
    fn is_read(prod: &str, field: &str) -> bool {
        let needle = format!(".{field}");
        let mut from = 0usize;
        while let Some(i) = prod[from..].find(&needle) {
            let at = from + i;
            let after = prod[at + needle.len()..].chars().next();
            let before = prod[..at].chars().next_back();
            let ident_after = after.is_some_and(|c| c.is_alphanumeric() || c == '_');
            // `self.m` counts; `x.map(` and `2.0` do not.
            let numeric_before = before.is_some_and(|c| c.is_ascii_digit());
            if !ident_after && !numeric_before {
                return true;
            }
            from = at + needle.len();
        }
        false
    }

    /// Machine-checked evidence for a claim about a dump field (B168).
    ///
    /// B157 found that B138's audit carried a row exempting the exact literal
    /// that turned out to be the bug, and the stated reason WAS the false
    /// claim. The audit did not accept the defect despite a justification; it
    /// accepted it BECAUSE of one. Free text records that somebody was
    /// convinced, never that they were right, and once written the scanner
    /// stops asking — so a wrong belief becomes permanent and load-bearing.
    ///
    /// `READ_CONFIRMED` never had that weakness: its snippet must literally
    /// still occur. This is the same idea applied to the JUSTIFIED half —
    /// every row now carries at least one claim the shipped dump can refute.
    #[derive(Debug, Clone, Copy)]
    enum Evidence {
        /// The key appears on exactly this many objects.
        OccursExactly(usize),
        /// Every occurrence has exactly this integer value.
        AlwaysExactly(u64),
        /// Every value is a string of hex digits.
        EveryValueIsHexDigits,
        /// No two objects share a value.
        EveryValueIsUnique,
        /// Every value is a non-empty string.
        EveryValueIsNonEmptyString,
        /// Exactly this many values end with the suffix, ignoring case. A
        /// COUNTED distribution claim: "mostly Gbx files" is prose, "294 of
        /// 314 end in gbx" is refutable.
        CaseInsensitiveSuffixCount(&'static str, usize),
        /// Following `p`, no class carries a smaller value than its base.
        /// Zero-valued children are skipped — they record no size.
        NeverSmallerThanParentIgnoringZero,
        /// This key and the named one occur in all four presence
        /// combinations, so neither is a spelling of the other.
        VariesIndependentlyOf(&'static str),
        /// NOTHING IN THE DUMP SUBSTANTIATES THE MEANING. The row's counts
        /// may still be checked; this marks the INTERPRETATION as unexamined,
        /// and a row carrying it must say UNEXAMINED in its prose so the next
        /// reader is invited to look rather than told not to.
        Unexamined,
    }

    /// (struct, WIRE field name, why nothing reads it, EVIDENCE).
    ///
    /// The fourth element is the point of B168: every row must carry at least
    /// one claim the shipped dump can refute. A row with an empty evidence
    /// list is rejected, and a row whose meaning is genuinely undetermined
    /// must carry `Evidence::Unexamined` AND say UNEXAMINED in its prose.
    ///
    /// Converting the prose to predicates immediately refuted two of these
    /// rows, both written from a couple of samples and then blessed — which
    /// is precisely the failure B157 found in B138.
    const JUSTIFIED: &[(&str, &str, &str, &[Evidence])] = &[
        // --- nothing for a language server to consume ---
        (
            "NadeoType",
            "i",
            "a per-class id: a 7-or-8 character HEX string, unique across all \
             2585 classes (CMwNod = 1001000, CMwCmdBuffer = 101C000). It names \
             the class in the Gbx file format, not in the script API. \
             CORRECTED BY B168: this row used to say `Gbx class id (1001000)`, \
             which reads as decimal — 1522 of the 2585 values contain hex \
             letters, which nobody who checked would have written.",
            &[
                Evidence::OccursExactly(2585),
                Evidence::EveryValueIsHexDigits,
                Evidence::EveryValueIsUnique,
            ],
        ),
        (
            "NadeoType",
            "f",
            "a file name associated with the class: 296 of the 314 end in `gbx` \
             case-insensitively (229 `.Gbx`, 65 `.gbx`, and 2 bare `gbx` with \
             no dot at all), and 18 do not — 11 `.json`, 3 `.txt`/`.Txt`, 1 \
             `.svg`, and 3 with no extension (`Vehicle`, `txt`, `zip`). \
             CORRECTED THREE TIMES BY B168, each time by the evaluator: the \
             row began as `the Gbx file this class roots`; the first fix still \
             claimed every value was filename-shaped, refuted by the 5 with no \
             dot; the second said 294, refuted because the 2 bare `gbx` values \
             end in gbx too. Prose survives arithmetic that a predicate does \
             not.",
            &[
                Evidence::OccursExactly(314),
                Evidence::EveryValueIsNonEmptyString,
                Evidence::CaseInsensitiveSuffixCount("gbx", 296),
            ],
        ),
        (
            "NadeoType",
            "s",
            "instance size in bytes, `0` on the 23 classes that record none. \
             C++ memory layout, so nothing in a language server consumes it. \
             The reading is structural rather than inferred from the name: \
             across 1978 parent links with a non-zero child, no class is ever \
             smaller than its base.",
            &[
                Evidence::OccursExactly(2585),
                Evidence::NeverSmallerThanParentIgnoringZero,
            ],
        ),
        // --- counted, but the MEANING is UNEXAMINED ---
        (
            "NadeoType",
            "c",
            "a presence flag on 869 classes, always the literal 1. Its MEANING \
             IS UNEXAMINED: it carries one bit and nothing in the dump says \
             which. Guessing what a bit means is how B146 nearly shipped 57 \
             false positives, so this is recorded as unknown rather than \
             explained.",
            &[
                Evidence::OccursExactly(869),
                Evidence::AlwaysExactly(1),
                Evidence::Unexamined,
            ],
        ),
        (
            "NadeoMember",
            "c",
            "a presence flag on 3831 members, always the literal 1. Measured \
             NOT to be method constness — it sits on 1943 properties and 264 \
             enum members but only 4 of 3084 methods (B137). Beyond that its \
             MEANING IS UNEXAMINED.",
            &[
                Evidence::OccursExactly(3831),
                Evidence::AlwaysExactly(1),
                Evidence::Unexamined,
            ],
        ),
        (
            "NadeoMember",
            "m",
            "a second presence flag on 5036 members, always the literal 1 and \
             independent of `c` — all four presence combinations occur, so it \
             is not a spelling of `c`. Its MEANING IS UNEXAMINED, so it is \
             declared to be visible and deliberately not consumed.",
            &[
                Evidence::OccursExactly(5036),
                Evidence::AlwaysExactly(1),
                Evidence::VariesIndependentlyOf("c"),
                Evidence::Unexamined,
            ],
        ),
    ];

    /// The structs the audit knows about, in the order their objects nest.
    const ROLES: [&str; 4] = [
        "NadeoDatabase",
        "NadeoType",
        "NadeoMember",
        "NadeoEnumEntry",
    ];

    /// Field names declared on more than one struct in this file.
    ///
    /// `is_read` is a text scan, so it cannot tell `NadeoType.m` (the member
    /// list, read constantly) from `NadeoMember.m` (a flag, read nowhere) —
    /// it scored the second as read because of the first. That is a false
    /// negative in the REASSURING direction, which is the one that matters,
    /// so an ambiguous name is never decided by the scan: it must be either
    /// justified or proven read by `READ_CONFIRMED`.
    fn ambiguous_names() -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut twice: BTreeSet<String> = BTreeSet::new();
        for role in ROLES {
            for (wire, _) in declared_fields_pairs(role) {
                if !seen.insert(wire.clone()) {
                    twice.insert(wire);
                }
            }
        }
        twice
    }

    /// (struct, wire field, a literal snippet of production code that reads
    /// it). The snippet must actually occur in `src/typedb/`, so the row
    /// cannot outlive the read it claims — this is proof, not assertion.
    const READ_CONFIRMED: &[(&str, &str, &str)] = &[
        ("NadeoType", "m", "nadeo_type.m.iter()"),
        ("NadeoType", "e", "nadeo_type.e"),
        ("NadeoMember", "n", "member.n.clone()"),
        ("NadeoMember", "e", "self.e.is_some()"),
        ("NadeoEnumEntry", "n", "type_name, en.n"),
        // B158 consumed both of these; they were OPEN FINDING rows until then.
        ("NadeoType", "d", "self.d.as_ref()?.get(\"d\")"),
        ("NadeoMember", "i", "notes.get(&member.i?)"),
    ];

    /// Every object a struct is deserialized from, in dump order.
    fn objects_for<'a>(db: &'a serde_json::Value, role: &str) -> Vec<&'a serde_json::Value> {
        let mut out = Vec::new();
        for ns in db["ns"].as_object().expect("ns").values() {
            for class in ns.as_object().expect("classes").values() {
                match role {
                    "NadeoType" => out.push(class),
                    "NadeoMember" => {
                        if let Some(ms) = class.get("m").and_then(|m| m.as_array()) {
                            out.extend(ms.iter());
                        }
                    }
                    _ => panic!("no object walker for {role}"),
                }
            }
        }
        out
    }

    /// Check one `Evidence` against the shipped dump. `Err` carries what the
    /// dump actually says, so a refuted row reports the truth rather than
    /// only that it was wrong.
    fn check_evidence(
        db: &serde_json::Value,
        role: &str,
        field: &str,
        ev: &Evidence,
    ) -> Result<(), String> {
        let objs = objects_for(db, role);
        let present: Vec<&serde_json::Value> = objs.iter().filter_map(|o| o.get(field)).collect();
        match ev {
            Evidence::OccursExactly(n) => (present.len() == *n)
                .then_some(())
                .ok_or_else(|| format!("occurs {} times, not {n}", present.len())),
            Evidence::AlwaysExactly(v) => {
                let bad: Vec<String> = present
                    .iter()
                    .filter(|x| x.as_u64() != Some(*v))
                    .map(|x| x.to_string())
                    .collect();
                bad.is_empty().then_some(()).ok_or_else(|| {
                    format!(
                        "{} values are not {v}, e.g. {:?}",
                        bad.len(),
                        &bad[..bad.len().min(3)]
                    )
                })
            }
            Evidence::EveryValueIsHexDigits => {
                let bad: Vec<String> = present
                    .iter()
                    .filter(|x| {
                        !x.as_str().is_some_and(|t| {
                            !t.is_empty() && t.chars().all(|c| c.is_ascii_hexdigit())
                        })
                    })
                    .map(|x| x.to_string())
                    .collect();
                bad.is_empty().then_some(()).ok_or_else(|| {
                    format!(
                        "{} values are not hex, e.g. {:?}",
                        bad.len(),
                        &bad[..bad.len().min(3)]
                    )
                })
            }
            Evidence::EveryValueIsUnique => {
                let mut seen = BTreeSet::new();
                let mut dupes = Vec::new();
                for v in &present {
                    if !seen.insert(v.to_string()) {
                        dupes.push(v.to_string());
                    }
                }
                dupes.is_empty().then_some(()).ok_or_else(|| {
                    format!(
                        "{} duplicate values, e.g. {:?}",
                        dupes.len(),
                        &dupes[..dupes.len().min(3)]
                    )
                })
            }
            Evidence::EveryValueIsNonEmptyString => {
                let bad: Vec<String> = present
                    .iter()
                    .filter(|x| !x.as_str().is_some_and(|t| !t.trim().is_empty()))
                    .map(|x| x.to_string())
                    .collect();
                bad.is_empty().then_some(()).ok_or_else(|| {
                    format!(
                        "{} are not non-empty strings, e.g. {:?}",
                        bad.len(),
                        &bad[..bad.len().min(3)]
                    )
                })
            }
            Evidence::CaseInsensitiveSuffixCount(suffix, n) => {
                let want = suffix.to_ascii_lowercase();
                let got = present
                    .iter()
                    .filter(|x| {
                        x.as_str()
                            .is_some_and(|t| t.to_ascii_lowercase().ends_with(&want))
                    })
                    .count();
                (got == *n)
                    .then_some(())
                    .ok_or_else(|| format!("{got} values end with `{suffix}`, not {n}"))
            }
            Evidence::NeverSmallerThanParentIgnoringZero => {
                let mut by_name = std::collections::HashMap::new();
                for ns in db["ns"].as_object().expect("ns").values() {
                    for (name, class) in ns.as_object().expect("classes") {
                        by_name.insert(name.clone(), class);
                    }
                }
                let (mut checked, mut viol) = (0usize, Vec::new());
                for (name, class) in &by_name {
                    let (Some(size), Some(parent)) = (
                        class.get(field).and_then(|v| v.as_u64()),
                        class.get("p").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    if size == 0 {
                        continue;
                    }
                    let Some(psize) = by_name
                        .get(parent)
                        .and_then(|p| p.get(field))
                        .and_then(|v| v.as_u64())
                    else {
                        continue;
                    };
                    checked += 1;
                    if size < psize {
                        viol.push(format!("{name} {size} < {parent} {psize}"));
                    }
                }
                if !viol.is_empty() {
                    return Err(format!("{} smaller than parent: {viol:?}", viol.len()));
                }
                (checked >= 1000)
                    .then_some(())
                    .ok_or_else(|| format!("only {checked} parent links checked — vacuous"))
            }
            Evidence::VariesIndependentlyOf(other) => {
                let mut combos = BTreeSet::new();
                for o in &objs {
                    combos.insert((o.get(field).is_some(), o.get(*other).is_some()));
                }
                (combos.len() == 4).then_some(()).ok_or_else(|| {
                    format!(
                        "only {} of 4 presence combinations with `{other}`",
                        combos.len()
                    )
                })
            }
            // Nothing to check — the honesty is enforced by the prose rule.
            Evidence::Unexamined => Ok(()),
        }
    }

    /// B168: EVERY JUSTIFIED ROW MUST STATE SOMETHING THE DUMP CAN REFUTE.
    ///
    /// Free-text justification is how a wrong belief becomes a permanent
    /// exemption: it records that somebody was convinced, never that they
    /// were right, and the scanner then stops asking. This test is the
    /// scanner continuing to ask.
    #[test]
    fn every_justified_row_states_evidence_the_dump_confirms() {
        let db = nadeo();
        let mut failures: Vec<String> = Vec::new();
        for (role, field, why, evidence) in JUSTIFIED {
            assert!(
                !evidence.is_empty(),
                "{role}.{field} carries no evidence — prose alone is what B157 \
                 found protecting the defect in B138"
            );
            let unexamined = evidence.iter().any(|e| matches!(e, Evidence::Unexamined));
            if unexamined && !why.contains("UNEXAMINED") {
                failures.push(format!(
                    "{role}.{field} is marked Unexamined but its prose does not \
                     SAY so — the next reader must be invited to look"
                ));
            }
            if !unexamined
                && evidence
                    .iter()
                    .all(|e| matches!(e, Evidence::OccursExactly(_)))
            {
                failures.push(format!(
                    "{role}.{field} only counts occurrences — a count is not a \
                     claim about what the field MEANS"
                ));
            }
            for e in *evidence {
                if let Err(actual) = check_evidence(&db, role, field, e) {
                    failures.push(format!("{role}.{field}: {e:?} REFUTED — {actual}"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "JUSTIFIED rows the dump refutes: {failures:#?}"
        );
    }

    /// Direction 1 — THE DEFECT THIS FILE HAD. Every key the dump carries
    /// must be declared, or serde drops it silently and no one finds out.
    #[test]
    fn every_dump_key_is_declared() {
        let db = nadeo();
        let mut undeclared: Vec<String> = Vec::new();
        for (name, keys) in dump_keys(&db) {
            let declared: BTreeSet<String> = declared_fields(name).into_iter().collect();
            for k in keys.difference(&declared) {
                undeclared.push(format!("{name}.{k}"));
            }
        }
        assert!(
            undeclared.is_empty(),
            "the dump carries keys this loader does not declare, so serde \
             discards them without a word: {undeclared:?}"
        );
    }

    /// Direction 2 — the B131 question, asked of this file for the first
    /// time. A declared field the dump never carries is a field we invented.
    #[test]
    fn every_declared_field_exists_in_the_dump() {
        let db = nadeo();
        let mut phantom: Vec<String> = Vec::new();
        for (name, keys) in dump_keys(&db) {
            for f in declared_fields(name) {
                if !keys.contains(&f) {
                    phantom.push(format!("{name}.{f}"));
                }
            }
        }
        assert!(
            phantom.is_empty(),
            "these fields are declared but no object in the shipped dump \
             carries them: {phantom:?}"
        );
    }

    /// Direction 3 — the one that finds a field parsed and thrown away.
    /// A declared field nothing reads is data the dump gave us and the
    /// loader dropped on the floor, which is how 1618 Nadeo return types
    /// went missing behind a comment claiming the dump "uses type IDs".
    #[test]
    fn every_declared_field_is_read_or_justified() {
        let justified = JUSTIFIED;
        let prod = production_loader_text();
        let mut unread: Vec<String> = Vec::new();
        for name in ROLES {
            let ambiguous = ambiguous_names();
            for (wire, rust) in declared_fields_pairs(name) {
                let is_justified = justified
                    .iter()
                    .any(|(s, field, why, _)| *s == name && *field == wire && !why.is_empty());
                if is_justified {
                    continue;
                }
                if ambiguous.contains(&wire) {
                    // The scan is not trusted here; demand proof instead.
                    let proven = READ_CONFIRMED.iter().any(|(s, field, snippet)| {
                        *s == name && *field == wire && prod.contains(snippet)
                    });
                    if !proven {
                        unread.push(format!(
                            "{name}.{wire} (name shared across structs: justify it \
                             or add a READ_CONFIRMED snippet)"
                        ));
                    }
                    continue;
                }
                if !is_read(&prod, &rust) {
                    unread.push(format!("{name}.{wire}"));
                }
            }
        }
        assert!(
            unread.is_empty(),
            "these fields are parsed and then never read by any loader — \
             either consume them or add a JUSTIFIED row saying why not: \
             {unread:?}"
        );
    }

    /// B131's own lesson: its first draft silently audited 7 of 11 structs.
    /// This asserts the audit's roles cover every struct in the file, so a
    /// struct added later cannot slip past unexamined.
    #[test]
    fn the_audit_covers_every_struct_in_this_file() {
        let db = nadeo();
        let covered: BTreeSet<&str> = dump_keys(&db).into_iter().map(|(n, _)| n).collect();
        let declared: BTreeSet<String> = all_struct_names().into_iter().collect();
        let missing: Vec<&String> = declared
            .iter()
            .filter(|s| !covered.contains(s.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "structs in nadeo_format.rs that no audit row covers: {missing:?}"
        );
    }
    /// A justification only means something while it is still true. If
    /// someone consumes a field, its row must go — a stale row would sit
    /// there excusing a field that no longer needs excusing, and would mask
    /// the next regression on it. Also rejects an empty reason, so a row
    /// cannot be added as a silent suppression.
    #[test]
    fn every_justified_row_is_live_and_reasoned() {
        let prod = production_loader_text();
        let mut stale: Vec<String> = Vec::new();
        for (name, wire, why, _evidence) in JUSTIFIED {
            assert!(
                !why.trim().is_empty(),
                "{name}.{wire} has an empty justification"
            );
            let pairs = declared_fields_pairs(name);
            let Some((_, rust)) = pairs.iter().find(|(w, _)| w == wire) else {
                stale.push(format!("{name}.{wire} (no such field)"));
                continue;
            };
            // An ambiguous name is never decided by the scan (see
            // `ambiguous_names`), so it cannot be called stale by it either.
            if !ambiguous_names().contains(*wire) && is_read(&prod, rust) {
                stale.push(format!("{name}.{wire} (now read — delete the row)"));
            }
        }
        assert!(stale.is_empty(), "stale JUSTIFIED rows: {stale:?}");
    }
    /// A `READ_CONFIRMED` row is only proof while its snippet still exists.
    #[test]
    fn every_read_confirmed_snippet_still_occurs_in_production_code() {
        let prod = production_loader_text();
        let missing: Vec<&str> = READ_CONFIRMED
            .iter()
            .filter(|(_, _, snippet)| !prod.contains(snippet))
            .map(|(_, _, snippet)| *snippet)
            .collect();
        assert!(
            missing.is_empty(),
            "these READ_CONFIRMED snippets no longer appear in src/typedb/, \
             so they prove nothing: {missing:?}"
        );
    }
}
