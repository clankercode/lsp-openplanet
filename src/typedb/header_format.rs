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
        // Namespace blocks themselves do not count: their contents are what
        // we want. Every other block stays on the stack so its closing brace
        // cannot accidentally close an enclosing aggregate.
        let mut aggregate_depth = 0usize;
        let mut open_blocks: Vec<BlockKind> = Vec::new();
        let mut pending_block = None;
        let mut pending_enum: Option<String> = None;

        let mut in_block_comment = false;
        let cleaned_lines: Vec<String> = text
            .lines()
            .map(|line| strip_comments(line, &mut in_block_comment))
            .collect();
        let mut lines = cleaned_lines.iter().map(String::as_str).peekable();
        while let Some(line) = lines.next() {
            let trimmed = line.trim();

            if let Some(name) = pending_enum.take() {
                if trimmed.starts_with('{') {
                    let values = read_enum_after_open(trimmed, &mut lines);
                    if aggregate_depth == 0 && !name.contains("::") {
                        module_enums.push(HeaderEnum { name, values });
                    }
                    continue;
                }
            }

            if let Some((name, has_open_brace)) = enum_decl(trimmed) {
                if !has_open_brace {
                    pending_enum = Some(name.to_string());
                    continue;
                }
                // The body is consumed either way; only its ownership of the
                // values differs.
                let after_open = trimmed.split_once('{').map(|(_, after)| after).unwrap();
                let values = read_enum_after_open(after_open, &mut lines);
                // Qualified (`Class::EName`) means the header is telling us
                // this enum belongs to something else, and the dump is the
                // authority on those. Inside a struct, likewise.
                if aggregate_depth == 0 && !name.contains("::") {
                    module_enums.push(HeaderEnum {
                        name: name.to_string(),
                        values,
                    });
                }
                continue;
            }

            track_blocks(
                trimmed,
                &mut open_blocks,
                &mut aggregate_depth,
                &mut pending_block,
            );
        }

        HeaderDatabase { module_enums }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockKind {
    Namespace,
    Aggregate,
    Other,
}

/// `enum class EName {` / `enum class Class::EName {` -> `(name, opens)`.
///
/// A declaration whose brace is on the following line is also useful in the
/// generated header. Forward declarations (`enum class EName;`) are not.
fn enum_decl(trimmed: &str) -> Option<(&str, bool)> {
    let rest = trimmed.strip_prefix("enum class ")?;
    let (name, opens) = match rest.split_once('{') {
        Some((name, _)) => (name.trim(), true),
        None if !rest.contains(';') => (rest.trim(), false),
        None => return None,
    };
    if name.is_empty() {
        return None;
    }
    Some((name, opens))
}

/// Record braces while remembering the block kind of the first opening brace
/// on a declaration line. This intentionally understands only the regular
/// machine-generated aggregate/namespace forms, not C++ in general.
fn track_blocks(
    line: &str,
    open_blocks: &mut Vec<BlockKind>,
    aggregate_depth: &mut usize,
    pending_block: &mut Option<BlockKind>,
) {
    let mut first_open = true;
    let prefix = line.split_once('{').map_or(line, |(before, _)| before);
    let declared_kind = block_decl_kind(prefix);
    if !line.contains('{') {
        if declared_kind.is_some() {
            *pending_block = declared_kind;
        } else if !line.is_empty() {
            *pending_block = None;
        }
    }

    for ch in line.chars() {
        match ch {
            '{' => {
                let kind = if first_open {
                    first_open = false;
                    declared_kind.or_else(|| pending_block.take())
                } else {
                    None
                }
                .unwrap_or(BlockKind::Other);
                if kind == BlockKind::Aggregate {
                    *aggregate_depth += 1;
                }
                open_blocks.push(kind);
            }
            '}' => {
                if let Some(BlockKind::Aggregate) = open_blocks.pop() {
                    *aggregate_depth = aggregate_depth.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

/// A generated aggregate or namespace declaration, with or without the
/// opening brace on this line. A semicolon is a forward declaration.
fn block_decl_kind(prefix: &str) -> Option<BlockKind> {
    let prefix = prefix.trim();
    if prefix.contains(';') {
        return None;
    }
    for keyword in ["struct", "class", "union"] {
        if prefix == keyword
            || prefix
                .strip_prefix(keyword)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        {
            return Some(BlockKind::Aggregate);
        }
    }
    if prefix == "namespace"
        || prefix
            .strip_prefix("namespace")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    {
        return Some(BlockKind::Namespace);
    }
    None
}

/// Remove line and block comments before brace tracking. The header grammar
/// does not need string-literal handling, but comments commonly contain
/// illustrative braces and must never alter scope.
fn strip_comments(line: &str, in_block_comment: &mut bool) -> String {
    let mut out = String::new();
    let mut rest = line;
    loop {
        if *in_block_comment {
            match rest.find("*/") {
                Some(end) => {
                    *in_block_comment = false;
                    rest = &rest[end + 2..];
                }
                None => return out,
            }
        } else {
            let block = rest.find("/*");
            let line_comment = rest.find("//");
            match (block, line_comment) {
                (_, Some(line_end)) if block.is_none_or(|block_start| line_end < block_start) => {
                    out.push_str(&rest[..line_end]);
                    return out;
                }
                (Some(block_start), _) => {
                    out.push_str(&rest[..block_start]);
                    *in_block_comment = true;
                    rest = &rest[block_start + 2..];
                }
                (None, None) => {
                    out.push_str(rest);
                    return out;
                }
                (None, Some(line_end)) => {
                    out.push_str(&rest[..line_end]);
                    return out;
                }
            }
        }
    }
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

/// Parse values after an enum's opening brace. The generated header normally
/// puts members on later lines, but recognising a one-line body prevents its
/// closing brace from consuming the surrounding aggregate's scope.
fn read_enum_after_open<'a>(
    after_open: &str,
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
) -> Vec<(String, i64)> {
    match after_open.split_once('}') {
        Some((body, _)) => body
            .split(',')
            .filter_map(|entry| enum_value(entry.trim()))
            .collect(),
        None => read_enum_body(lines),
    }
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
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

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
    fn skips_unqualified_enums_inside_aggregate_shapes_and_keeps_outer_enums() {
        let db = HeaderDatabase::parse(
            "namespace Enums\n{\n// { comments must not open a block }\nstruct Named\n{\n  enum class EInStruct { A = 0, };\n};\nclass NamedClass {\n  union {\n    enum class EInAnonymousUnion { A = 0, };\n  };\n};\nunion NamedUnion\n{\n  enum class EInUnion { A = 0, };\n};\nstruct Empty {};\nenum class EOuter {\n  Good = 7,\n}; // trailing suffix\n}\n",
        );
        assert_eq!(names(&db), vec!["EOuter"]);
        assert_eq!(db.module_enums[0].values, vec![("Good".to_string(), 7)]);
    }

    #[test]
    fn parses_an_enum_with_a_next_line_opening_brace() {
        let db = HeaderDatabase::parse("enum class ENextLine\n{\n  A = 1,\n};\n");
        assert_eq!(names(&db), vec!["ENextLine"]);
        assert_eq!(db.module_enums[0].values, vec![("A".to_string(), 1)]);
    }

    #[test]
    fn accepts_only_decimal_integer_enum_values() {
        let db = HeaderDatabase::parse(
            "enum class EValues {\n  Decimal = 1,\n  Negative = -2,\n  Plus = +3,\n  Hex = 0x4,\n  Suffix = 5u,\n  Fraction = 6.5,\n};\n",
        );
        assert_eq!(
            db.module_enums[0].values,
            vec![
                ("Decimal".to_string(), 1),
                ("Negative".to_string(), -2),
                ("Plus".to_string(), 3),
            ]
        );
    }

    #[test]
    fn python_probe_parser_matches_the_rust_header_grammar() {
        let samples = [
            "namespace Enums {\nenum class EValid {\n  Zero = 0,\n  Negative = -2,\n  Plus = +3,\n  Hex = 0x4,\n  Suffix = 5u,\n}; // enum close suffix\n}\n",
            "/* { a block-comment brace } */\nstruct CThing\n{\n  enum class EInside {\n    A = 0,\n  };\n  class Nested { union { enum class EDeeper { B = 1, }; }; };\n};\nenum class EAfter\n{\n  A = 7, // } a line-comment brace\n}; trailing\n",
            "namespace Enums\n{\nunion { enum class EAnonymous { A = 0, }; };\nstruct Empty {};\nenum class EOnly {\n  Fine = 9,\n  Fraction = 1.5,\n};\n}\n",
        ];
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for sample in samples {
            let rust = HeaderDatabase::parse(sample);
            let mut child = Command::new("python3")
                .arg(repo.join("scripts/gen_b044_probe.py"))
                .arg("--parse-header")
                .current_dir(&repo)
                .env_clear()
                .env(
                    "PATH",
                    std::env::var("PATH").expect("PATH is required for python3"),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("python3 must be available for header-parser parity");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(sample.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "python parser failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let python: Vec<(String, Vec<(String, i64)>)> =
                serde_json::from_slice(&output.stdout).unwrap();
            let rust: Vec<(String, Vec<(String, i64)>)> = rust
                .module_enums
                .into_iter()
                .map(|header_enum| (header_enum.name, header_enum.values))
                .collect();
            assert_eq!(python, rust, "header parser disagreement for {sample:?}");
        }
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
