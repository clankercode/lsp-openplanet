//! `Openplanet.h` — the engine header, read for the enums the JSON dump
//! cannot express (B046).
//!
//! The Nadeo dump's `ns` map is namespace -> type name -> type, so the only
//! enum it can carry directly is one nested in a class. It reaches most
//! MODULE-LEVEL enums anyway, by accident of shape: a class member typed by
//! one carries the enum inline (`"t": "ESpectatorCameraType"`), and
//! [`crate::typedb::index::TypeIndex::add_nadeo`] keys that `Module::EName`.
//! A module-level enum that no member anywhere is typed by has no such route,
//! and is invisible. Measured against OP 1.29.5: 59 module-level enums, 57
//! reachable through a member, 2 (`ESceneCharVisNotice`,
//! `ESceneVehicleVisEvent`) reachable only here.
//!
//! # What this parser takes, and what it deliberately ignores
//!
//! The header is a superset of the dump, so taking everything would
//! double-index. This parser extracts ONLY module-level enums — a
//! declaration at namespace level whose name is unqualified. Everything else
//! is skipped at the source:
//!
//! - `enum class CGameMenu::EMenuOrder {` inside a `struct` body — the dump
//!   carries these as nested enums, keyed `Module::Class::EName`.
//! - `enum class NPlugAnim::EPoseType {` at namespace level but qualified —
//!   nested in something the header does not emit as a `struct`. The dump
//!   reaches these through a member's `"t"` when anything is typed by them.
//!
//! Skipping at the source is not sufficient on its own: 57 of the 59 names
//! this parser does return are already in the index. Preferring the dump is
//! the loader's job — see `TypeIndex::add_header`.
//!
//! # Namespaces
//!
//! The header has 21 `namespace X { }` blocks: the 20 the dump also has, plus
//! a trailing `namespace Enums { }` that every module-level enum is filed
//! under. That bucket is a documentation artefact, not a namespace the game
//! knows — a live RemoteBuild probe (OP 1.29.5, 2026-08-30) answers
//! `Namespace 'Enums' doesn't exist.` for `Enums::ESceneCharVisNotice`, and
//! `Namespace 'Scene' doesn't exist.` for `Scene::ESceneCharVisNotice`. The
//! same probe accepts the bare name as a type and its values bare. So a
//! module-level enum has NO namespace, and this parser attributes none.
//!
//! # Not a C++ parser
//!
//! This reads one machine-generated file with a stable shape, not arbitrary
//! C++. It tracks brace depth to know whether a declaration is at namespace
//! level or inside a `struct`, and reads `Name = <int>,` lines out of an enum
//! body. Anything it does not recognise it skips; a header it cannot make
//! sense of yields no enums rather than an error, because this source is
//! optional (see `TypeIndex::load`).

use std::path::Path;

/// A module-level enum: a name the game accepts unqualified, and its values.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderEnum {
    pub name: String,
    /// `(value name, value)`, in declaration order. Unlike the dump, the
    /// header carries explicit integers, so these are the engine's own
    /// values rather than positions — six engine enums are non-contiguous.
    pub values: Vec<(String, i64)>,
}

/// Every module-level enum `Openplanet.h` declares.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeaderDatabase {
    pub module_enums: Vec<HeaderEnum>,
}

impl HeaderDatabase {
    /// Read and parse a header. The error names the path, matching the JSON
    /// loaders; callers that treat the header as optional check for the file
    /// first rather than swallowing this.
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        Ok(Self::parse(&text))
    }

    /// Parse header text. Never fails: an unrecognised header yields no
    /// enums, which degrades to today's behaviour rather than to an error.
    pub fn parse(text: &str) -> Self {
        let mut module_enums = Vec::new();
        // Depth of `struct` bodies we are inside. A declaration is at
        // namespace level exactly when this is zero; namespace blocks
        // themselves do not count, since their contents are what we want.
        let mut struct_depth = 0usize;
        // Brace depth of each open block, innermost last, so a closing brace
        // knows whether it ended a struct.
        let mut open_blocks: Vec<BlockKind> = Vec::new();

        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            let trimmed = line.trim();

            if let Some(name) = enum_decl_name(trimmed) {
                // The body is consumed either way; only its ownership of the
                // values differs.
                let values = read_enum_body(&mut lines);
                // Qualified (`Class::EName`) means the header is telling us
                // this enum belongs to something else, and the dump is the
                // authority on those. Inside a struct, likewise.
                if struct_depth == 0 && !name.contains("::") {
                    module_enums.push(HeaderEnum {
                        name: name.to_string(),
                        values,
                    });
                }
                continue;
            }

            if is_struct_open(trimmed) {
                open_blocks.push(BlockKind::Struct);
                struct_depth += 1;
                continue;
            }
            if is_namespace_open(trimmed) {
                open_blocks.push(BlockKind::Namespace);
                continue;
            }
            if trimmed.starts_with('}') {
                if let Some(BlockKind::Struct) = open_blocks.pop() {
                    struct_depth = struct_depth.saturating_sub(1);
                }
            }
        }

        HeaderDatabase { module_enums }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockKind {
    Namespace,
    Struct,
}

/// `enum class EName {` / `enum class Class::EName {` -> the declared name.
fn enum_decl_name(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("enum class ")?;
    let name = rest.strip_suffix('{')?.trim();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// `struct CMwNod {` / `struct CMwEngine : public CMwNod {`. A forward
/// declaration (`struct X;`) opens no block.
fn is_struct_open(trimmed: &str) -> bool {
    trimmed.starts_with("struct ") && trimmed.ends_with('{')
}

/// `namespace Game {`.
fn is_namespace_open(trimmed: &str) -> bool {
    trimmed.starts_with("namespace ") && trimmed.ends_with('{')
}

/// Consume an enum body up to and including its closing brace, collecting
/// `Name = <int>,` entries. A line the shape does not match is skipped, so a
/// comment or a value the header writes differently costs that value, not the
/// enum.
fn read_enum_body<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
) -> Vec<(String, i64)> {
    let mut values = Vec::new();
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with('}') {
            break;
        }
        if let Some(entry) = enum_value(trimmed) {
            values.push(entry);
        }
    }
    values
}

/// `Menu = 2,` -> `("Menu", 2)`. Trailing `// comment` is tolerated.
fn enum_value(trimmed: &str) -> Option<(String, i64)> {
    let (name, rest) = trimmed.split_once('=')?;
    let name = name.trim();
    if name.is_empty() || !is_identifier(name) {
        return None;
    }
    let digits: String = rest
        .trim()
        .trim_end_matches(',')
        .split("//")
        .next()?
        .trim()
        .trim_end_matches(',')
        .trim()
        .to_string();
    let value = digits.parse::<i64>().ok()?;
    Some((name.to_string(), value))
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_header() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb/Openplanet.h")
    }

    fn names(db: &HeaderDatabase) -> Vec<&str> {
        db.module_enums.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn parses_a_module_level_enum() {
        let db = HeaderDatabase::parse(
            "namespace Enums {\n\nenum class EPoseType {\n  Parent = 0,\n  Global = 1,\n};\n\n}\n",
        );
        assert_eq!(names(&db), vec!["EPoseType"]);
        assert_eq!(
            db.module_enums[0].values,
            vec![("Parent".to_string(), 0), ("Global".to_string(), 1)]
        );
    }

    #[test]
    fn skips_an_enum_nested_in_a_struct() {
        let db = HeaderDatabase::parse(
            "namespace Game {\n\nstruct CGameMenu : public CMwNod {\n  enum class CGameMenu::EMenuOrder {\n    Menu = 2,\n  };\n  int Frames;\n};\n\n}\n",
        );
        assert!(db.module_enums.is_empty(), "{:?}", db.module_enums);
    }

    #[test]
    fn skips_a_namespace_level_enum_whose_name_is_qualified() {
        let db = HeaderDatabase::parse(
            "namespace Enums {\n\nenum class NPlugAnim::EPoseType {\n  Parent = 0,\n};\n\n}\n",
        );
        assert!(db.module_enums.is_empty(), "{:?}", db.module_enums);
    }

    #[test]
    fn a_struct_body_does_not_swallow_the_enums_after_it() {
        // The struct must close cleanly, or every later module-level enum is
        // read as nested and lost.
        let db = HeaderDatabase::parse(
            "namespace Game {\n\nstruct CGameMenu {\n  enum class CGameMenu::EMenuOrder {\n    Menu = 2,\n  };\n};\n\n}\n\nnamespace Enums {\n\nenum class EAfter {\n  A = 0,\n};\n\n}\n",
        );
        assert_eq!(names(&db), vec!["EAfter"]);
    }

    #[test]
    fn keeps_the_headers_own_integer_values_not_positions() {
        // Six engine enums are non-contiguous; positions would misreport them.
        let db = HeaderDatabase::parse(
            "namespace Enums {\n\nenum class EMenuOrder {\n  Menu = 2,\n  InGameMenu = 3,\n  GameDialog = 5,\n  SystemDialog = 8,\n  BasicDialog = 11,\n};\n\n}\n",
        );
        assert_eq!(
            db.module_enums[0].values,
            vec![
                ("Menu".to_string(), 2),
                ("InGameMenu".to_string(), 3),
                ("GameDialog".to_string(), 5),
                ("SystemDialog".to_string(), 8),
                ("BasicDialog".to_string(), 11),
            ]
        );
    }

    #[test]
    fn tolerates_a_trailing_comment_on_a_value() {
        let db = HeaderDatabase::parse(
            "namespace Enums {\n\nenum class EThing {\n  XXX_Null = 24, // XXX Null\n};\n\n}\n",
        );
        assert_eq!(
            db.module_enums[0].values,
            vec![("XXX_Null".to_string(), 24)]
        );
    }

    #[test]
    fn a_forward_declaration_opens_no_block() {
        let db = HeaderDatabase::parse(
            "namespace Enums {\n\nstruct CForward;\n\nenum class EAfter {\n  A = 0,\n};\n\n}\n",
        );
        assert_eq!(names(&db), vec!["EAfter"]);
    }

    #[test]
    fn unrecognised_text_yields_no_enums_rather_than_an_error() {
        let db = HeaderDatabase::parse("this is not a header at all\n{{{\n");
        assert!(db.module_enums.is_empty());
    }

    #[test]
    fn parses_the_shipped_header_fixture() {
        let db = HeaderDatabase::load_from_file(&fixture_header()).expect("fixture header parses");
        // 59 module-level enums in OP 1.29.5; the other 499 declarations are
        // struct-nested or namespace-level-but-qualified and are skipped.
        assert_eq!(db.module_enums.len(), 59);
        let names = names(&db);
        assert!(names.contains(&"ESceneCharVisNotice"), "{names:?}");
        assert!(names.contains(&"ESceneVehicleVisEvent"), "{names:?}");
        // Struct-nested and qualified declarations must not leak in.
        assert!(!names.iter().any(|n| n.contains("::")), "{names:?}");
        assert!(!names.contains(&"EMenuOrder"), "{names:?}");
    }

    #[test]
    fn the_two_enums_the_dump_cannot_express_carry_their_full_value_lists() {
        let db = HeaderDatabase::load_from_file(&fixture_header()).expect("fixture header parses");
        let by_name = |n: &str| {
            db.module_enums
                .iter()
                .find(|e| e.name == n)
                .unwrap_or_else(|| panic!("{n} missing"))
        };
        let char_vis = by_name("ESceneCharVisNotice");
        assert_eq!(char_vis.values.len(), 33);
        assert_eq!(char_vis.values[0], ("Jump".to_string(), 0));
        assert!(char_vis.values.iter().any(|(n, _)| n == "DoubleJump"));
        assert!(char_vis.values.iter().any(|(n, _)| n == "TechJump"));
        let vehicle = by_name("ESceneVehicleVisEvent");
        assert_eq!(vehicle.values.len(), 26);
        assert!(vehicle.values.iter().any(|(n, _)| n == "WallBounce"));
    }

    #[test]
    fn every_parsed_enum_has_at_least_one_value() {
        let db = HeaderDatabase::load_from_file(&fixture_header()).expect("fixture header parses");
        for e in &db.module_enums {
            assert!(!e.values.is_empty(), "{} parsed with no values", e.name);
        }
    }

    #[test]
    fn parsing_the_same_header_twice_yields_identical_output() {
        let a = HeaderDatabase::load_from_file(&fixture_header()).expect("parses");
        let b = HeaderDatabase::load_from_file(&fixture_header()).expect("parses");
        assert_eq!(a, b);
    }

    #[test]
    fn a_missing_header_is_an_error_the_caller_can_see() {
        let err = HeaderDatabase::load_from_file(Path::new("/nonexistent/Openplanet.h"))
            .expect_err("missing file must not parse as empty");
        assert!(err.contains("Openplanet.h"), "{err}");
    }
}
