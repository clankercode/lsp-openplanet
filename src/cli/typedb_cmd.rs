//! CLI `typedb` command family (GH #59): make type-DB freshness a managed,
//! visible pipeline instead of an accident.
//!
//! * `status` — which DB files a run resolves, and what build they came from.
//! * `diff` — symbol-level added/removed/changed between two DB directories.
//! * `refresh` — harvest both DBs out of a local Openplanet install.
//!
//! Deliberately offline: DBs come from a local install (`--from`), the
//! auto-detect ladder ([`LspConfig::load`], GH #45), or a directory the user
//! names. There is no upstream download endpoint and nothing here fetches.

use std::path::{Path, PathBuf};

use crate::config::LspConfig;
use crate::typedb::index::{diff_indexes, DbDiff, DbSymbol};
use crate::typedb::{TypeDbMeta, TypeIndex};

const TYPEDB_HELP: &str = "\
openplanet-lsp typedb - Inspect and refresh the Openplanet type databases

USAGE:
    openplanet-lsp typedb status [--from DIR] [--json]
    openplanet-lsp typedb diff OLD_DIR [NEW_DIR] [--json]
    openplanet-lsp typedb refresh --into DIR [--from DIR] [--yes]

SUBCOMMANDS:
    status      Show the resolved DB paths, their `op`/`mp` version stamps,
                and their top-level entry counts. Without --from, the same
                auto-detect ladder `check` and the language server use.
    diff        Symbol-level parity report between two DB directories:
                added / removed / changed classes, enums, and functions.
                NEW_DIR defaults to the auto-detected install.
    refresh     Copy both DB files out of a local install into --into.
                DRY RUN by default: prints the diff and writes nothing;
                pass --yes to actually copy.

OPTIONS:
    -h, --help      Show this help message
    --from DIR      Openplanet install directory holding the DB files
                    (e.g. ~/OpenplanetNext). Default: auto-detect.
    --into DIR      Destination directory for `refresh` (required — there is
                    no implicit write path).
    --yes           `refresh` only: actually write the files.
    --json          Machine-readable output (status, diff).

NOTES:
    The game DB filename follows the install variant: OpenplanetNext.json,
    OpenplanetMP4.json, OpenplanetTurbo.json, or OpenplanetUnited.json —
    whichever the directory holds.

    Nothing in this command family accesses the network.

EXAMPLES:
    openplanet-lsp typedb status
    openplanet-lsp typedb diff tests/fixtures/typedb
    openplanet-lsp typedb refresh --into tests/fixtures/typedb
    openplanet-lsp typedb refresh --into tests/fixtures/typedb --yes

";

const TYPEDB_USAGE: &str = "\
USAGE:
    openplanet-lsp typedb status [--from DIR] [--json]
    openplanet-lsp typedb diff OLD_DIR [NEW_DIR] [--json]
    openplanet-lsp typedb refresh --into DIR [--from DIR] [--yes]

Run `openplanet-lsp typedb --help` for details.
";

/// The Openplanet Core DB filename — identical across every install variant.
const CORE_DB_FILENAME: &str = "OpenplanetCore.json";

/// Game-engine dump filenames a harvest may find in an install directory,
/// most likely first. Mirrors [`LspConfig::game_json_filename_for_target`]
/// but probes by presence instead of by target, so `--from` works for any
/// variant without a `game_target` flag (per-variant DB selection is
/// deliberately deferred — GH #59 idea I6).
const GAME_DB_FILENAMES: &[&str] = &[
    "OpenplanetNext.json",
    "OpenplanetMP4.json",
    "OpenplanetTurbo.json",
    "OpenplanetUnited.json",
];

/// A resolved Core + game DB file pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbPair {
    pub core: PathBuf,
    pub game: PathBuf,
}

impl DbPair {
    /// Resolve both DB files inside an install (or fixture) directory.
    ///
    /// The error names the missing file: a directory the user pointed at is
    /// never downgraded to a soft warning.
    pub fn in_dir(dir: &Path) -> Result<Self, String> {
        let core = dir.join(CORE_DB_FILENAME);
        if !core.is_file() {
            return Err(format!("{CORE_DB_FILENAME} not found in {}", dir.display()));
        }
        let game = GAME_DB_FILENAMES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file())
            .ok_or_else(|| {
                format!(
                    "no game type database in {} (expected one of: {})",
                    dir.display(),
                    GAME_DB_FILENAMES.join(", ")
                )
            })?;
        Ok(Self { core, game })
    }

    /// The pair `check` and the language server would use: `--from DIR` when
    /// given, else the auto-detect ladder (GH #45).
    ///
    /// `Ok(None)` means "no install found anywhere" — only reachable without
    /// `--from`, and a warning rather than a failure.
    pub fn resolve(from: Option<&Path>) -> Result<Option<Self>, String> {
        if let Some(dir) = from {
            return Self::in_dir(dir).map(Some);
        }
        let config = LspConfig::load(None, None);
        match (config.core_json, config.game_json) {
            (Some(core), Some(game)) => Ok(Some(Self { core, game })),
            _ => Ok(None),
        }
    }
}

/// Warning printed when neither `--from` nor auto-detect finds an install.
const NOT_FOUND_WARNING: &str =
    "warning: type database not found (no Openplanet install auto-detected; pass --from DIR)";

pub fn run_typedb_command(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            print!("{TYPEDB_HELP}");
            0
        }
        Some("status") => run_status(&args[1..]),
        Some("diff") => run_diff(&args[1..]),
        // Implemented in the next step of GH #59 (task 5).
        Some("refresh") => {
            eprintln!("typedb refresh: not implemented yet (GH #59)");
            2
        }
        Some(other) => usage_error(&format!("unknown typedb subcommand: {other}")),
        None => usage_error("typedb requires a subcommand"),
    }
}

fn usage_error(msg: &str) -> i32 {
    eprintln!("{msg}");
    eprint!("{TYPEDB_USAGE}");
    2
}

// === status ===

#[derive(Debug, Default)]
struct StatusArgs {
    from: Option<PathBuf>,
    json: bool,
}

fn parse_status_args(args: &[String]) -> Result<Option<StatusArgs>, String> {
    let mut parsed = StatusArgs::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => return Ok(None),
            "--json" => {
                parsed.json = true;
                i += 1;
            }
            "--from" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("--from requires a directory argument".to_string());
                };
                parsed.from = Some(PathBuf::from(value));
                i += 2;
            }
            _ if arg.starts_with("--from=") => {
                let value = arg.trim_start_matches("--from=");
                if value.is_empty() {
                    return Err("--from requires a directory argument".to_string());
                }
                parsed.from = Some(PathBuf::from(value));
                i += 1;
            }
            _ => {
                return Err(format!(
                    "unknown typedb status argument: {arg}\n\
                     Run `openplanet-lsp typedb --help` for usage."
                ))
            }
        }
    }
    Ok(Some(parsed))
}

fn run_status(args: &[String]) -> i32 {
    let parsed = match parse_status_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{TYPEDB_HELP}");
            return 0;
        }
        Err(err) => return usage_error(&err),
    };

    let pair = match DbPair::resolve(parsed.from.as_deref()) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            if parsed.json {
                println!("{}", serde_json::json!({"core": null, "game": null}));
            }
            eprintln!("{NOT_FOUND_WARNING}");
            return 0;
        }
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    let (core, game) = match probe_pair(&pair) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    if parsed.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "core": meta_json(&pair.core, &core),
                "game": meta_json(&pair.game, &game),
            }))
            .unwrap_or_default()
        );
    } else {
        print!("{}", describe_pair(&pair, &core, &game));
    }
    0
}

/// Probe both files, reporting the first parse failure by path.
fn probe_pair(pair: &DbPair) -> Result<(TypeDbMeta, TypeDbMeta), String> {
    let core = TypeDbMeta::probe(&pair.core)?;
    let game = TypeDbMeta::probe(&pair.game)?;
    Ok((core, game))
}

/// Two aligned `core:` / `game:` lines — the G1 visibility primitive.
fn describe_pair(pair: &DbPair, core: &TypeDbMeta, game: &TypeDbMeta) -> String {
    format!(
        "{}{}",
        describe_db("core", &pair.core, core),
        describe_db("game", &pair.game, game)
    )
}

fn describe_db(label: &str, path: &Path, meta: &TypeDbMeta) -> String {
    let mut line = format!(
        "{label}:  {}  op={}",
        path.display(),
        meta.op.as_deref().unwrap_or("<none>")
    );
    if let Some(mp) = &meta.mp {
        line.push_str(&format!("  mp={mp}"));
    }
    line.push_str(&format!("  [{}]\n", describe_counts(meta)));
    line
}

/// Entry counts in the shape the file actually has: the Nadeo dump nests its
/// classes under `ns` modules and carries nothing else.
fn describe_counts(meta: &TypeDbMeta) -> String {
    let c = &meta.counts;
    if c.ns_modules > 0 {
        format!("{} classes in {} modules", c.classes, c.ns_modules)
    } else {
        format!(
            "{} functions, {} classes, {} enums, {} funcdefs, {} props",
            c.functions, c.classes, c.enums, c.funcdefs, c.props
        )
    }
}

fn meta_json(path: &Path, meta: &TypeDbMeta) -> serde_json::Value {
    let c = &meta.counts;
    serde_json::json!({
        "path": path.display().to_string(),
        "op": meta.op,
        "mp": meta.mp,
        "counts": {
            "functions": c.functions,
            "classes": c.classes,
            "enums": c.enums,
            "funcdefs": c.funcdefs,
            "props": c.props,
            "ns_modules": c.ns_modules,
        },
    })
}

// === diff ===

/// Plain-output cap per bucket. A maintainer scanning a parity report wants
/// the shape, not a 2000-line dump; `--json` carries everything.
const MAX_PLAIN_ROWS: usize = 25;
/// Cap on the per-entry fact lines of a changed row.
const MAX_PLAIN_DETAILS: usize = 8;

#[derive(Debug, Default)]
struct DiffArgs {
    old: Option<PathBuf>,
    new: Option<PathBuf>,
    json: bool,
}

fn parse_diff_args(args: &[String]) -> Result<Option<DiffArgs>, String> {
    let mut parsed = DiffArgs::default();
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--json" => parsed.json = true,
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown typedb diff option: {other}\n\
                     Run `openplanet-lsp typedb --help` for usage."
                ))
            }
            other if parsed.old.is_none() => parsed.old = Some(PathBuf::from(other)),
            other if parsed.new.is_none() => parsed.new = Some(PathBuf::from(other)),
            other => {
                return Err(format!(
                    "typedb diff accepts at most two directories (got extra: {other})"
                ))
            }
        }
    }
    if parsed.old.is_none() {
        return Err(
            "typedb diff requires OLD_DIR (NEW_DIR defaults to the auto-detected install)"
                .to_string(),
        );
    }
    Ok(Some(parsed))
}

fn run_diff(args: &[String]) -> i32 {
    let parsed = match parse_diff_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{TYPEDB_HELP}");
            return 0;
        }
        Err(err) => return usage_error(&err),
    };
    let old_dir = parsed.old.expect("parse guarantees OLD_DIR");

    let old_pair = match DbPair::in_dir(&old_dir) {
        Ok(pair) => pair,
        Err(err) => return fail(&err),
    };
    // NEW defaults to the auto-detected install: the common maintainer
    // question is "how far has the repo fixture drifted from my game?".
    let new_pair = match parsed.new.as_deref() {
        Some(dir) => match DbPair::in_dir(dir) {
            Ok(pair) => pair,
            Err(err) => return fail(&err),
        },
        None => match DbPair::resolve(None) {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return fail("no Openplanet install auto-detected; pass NEW_DIR explicitly")
            }
            Err(err) => return fail(&err),
        },
    };

    let (old_index, new_index) = match (load_index(&old_pair), load_index(&new_pair)) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(err), _) | (_, Err(err)) => return fail(&err),
    };
    let diff = diff_indexes(&old_index, &new_index);

    if parsed.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&diff_json(&old_pair, &new_pair, &diff))
                .unwrap_or_default()
        );
    } else {
        print!("{}", render_diff(&old_pair, &new_pair, &diff));
    }
    0
}

fn fail(msg: &str) -> i32 {
    eprintln!("error: {msg}");
    1
}

fn load_index(pair: &DbPair) -> Result<TypeIndex, String> {
    TypeIndex::load(&pair.core, &pair.game)
}

/// Directory a pair came from, for report headers.
fn pair_dir(pair: &DbPair) -> String {
    pair.core
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| pair.core.display().to_string())
}

fn render_diff(old: &DbPair, new: &DbPair, diff: &DbDiff) -> String {
    let mut out = format!("diff: {} -> {}\n", pair_dir(old), pair_dir(new));
    let (added, removed, changed) = diff.counts();

    render_bucket(&mut out, "added", '+', &diff.added);
    render_bucket(&mut out, "removed", '-', &diff.removed);

    if !diff.changed.is_empty() {
        out.push_str(&format!("changed ({}):\n", changed));
        for entry in diff.changed.iter().take(MAX_PLAIN_ROWS) {
            out.push_str(&format!("  ~ {} {}\n", entry.kind.label(), entry.name));
            for detail in entry.details.iter().take(MAX_PLAIN_DETAILS) {
                out.push_str(&format!("      {detail}\n"));
            }
            if entry.details.len() > MAX_PLAIN_DETAILS {
                out.push_str(&format!(
                    "      ... and {} more\n",
                    entry.details.len() - MAX_PLAIN_DETAILS
                ));
            }
        }
        push_elision(&mut out, diff.changed.len());
    }

    out.push_str(&format!(
        "{added} added, {removed} removed, {changed} changed\n"
    ));
    out
}

fn render_bucket(out: &mut String, label: &str, marker: char, symbols: &[DbSymbol]) {
    if symbols.is_empty() {
        return;
    }
    out.push_str(&format!("{label} ({}):\n", symbols.len()));
    for symbol in symbols.iter().take(MAX_PLAIN_ROWS) {
        out.push_str(&format!(
            "  {marker} {} {}\n",
            symbol.kind.label(),
            symbol.name
        ));
    }
    push_elision(out, symbols.len());
}

fn push_elision(out: &mut String, total: usize) {
    if total > MAX_PLAIN_ROWS {
        out.push_str(&format!(
            "  ... and {} more (use --json for the full list)\n",
            total - MAX_PLAIN_ROWS
        ));
    }
}

fn diff_json(old: &DbPair, new: &DbPair, diff: &DbDiff) -> serde_json::Value {
    let symbols = |list: &[DbSymbol]| {
        list.iter()
            .map(|s| serde_json::json!({"kind": s.kind.label(), "name": s.name}))
            .collect::<Vec<_>>()
    };
    let (added, removed, changed) = diff.counts();
    serde_json::json!({
        "old": pair_dir(old),
        "new": pair_dir(new),
        "added": symbols(&diff.added),
        "removed": symbols(&diff.removed),
        "changed": diff.changed.iter().map(|c| serde_json::json!({
            "kind": c.kind.label(),
            "name": c.name,
            "details": c.details,
        })).collect::<Vec<_>>(),
        "summary": {"added": added, "removed": removed, "changed": changed},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typedb::index::{DbChange, DbSymbolKind};

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-typedb-cmd-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn db_pair_in_dir_finds_core_and_next() {
        let dir = tmp_dir("pair-next");
        std::fs::write(dir.join(CORE_DB_FILENAME), "{}").unwrap();
        std::fs::write(dir.join("OpenplanetNext.json"), "{}").unwrap();
        let pair = DbPair::in_dir(&dir).unwrap();
        assert_eq!(pair.core, dir.join(CORE_DB_FILENAME));
        assert_eq!(pair.game, dir.join("OpenplanetNext.json"));
    }

    /// Every install variant's dump filename resolves, so `--from` needs no
    /// `game_target` flag.
    #[test]
    fn db_pair_in_dir_finds_each_variant_dump() {
        for variant in GAME_DB_FILENAMES {
            let dir = tmp_dir(&format!("pair-{variant}"));
            std::fs::write(dir.join(CORE_DB_FILENAME), "{}").unwrap();
            std::fs::write(dir.join(variant), "{}").unwrap();
            let pair = DbPair::in_dir(&dir).unwrap();
            assert_eq!(pair.game, dir.join(variant), "variant {variant}");
        }
    }

    #[test]
    fn db_pair_in_dir_names_missing_core() {
        let dir = tmp_dir("pair-no-core");
        std::fs::write(dir.join("OpenplanetNext.json"), "{}").unwrap();
        let err = DbPair::in_dir(&dir).unwrap_err();
        assert!(err.contains(CORE_DB_FILENAME), "{err}");
    }

    #[test]
    fn db_pair_in_dir_names_missing_game_variants() {
        let dir = tmp_dir("pair-no-game");
        std::fs::write(dir.join(CORE_DB_FILENAME), "{}").unwrap();
        let err = DbPair::in_dir(&dir).unwrap_err();
        assert!(err.contains("OpenplanetNext.json"), "{err}");
        assert!(err.contains("OpenplanetTurbo.json"), "{err}");
    }

    #[test]
    fn parse_status_args_variants() {
        let parse = |args: &[&str]| {
            parse_status_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
        };
        let a = parse(&["--from", "/tmp/x"]).unwrap().unwrap();
        assert_eq!(a.from, Some(PathBuf::from("/tmp/x")));
        assert!(!a.json);

        let a = parse(&["--from=/tmp/y", "--json"]).unwrap().unwrap();
        assert_eq!(a.from, Some(PathBuf::from("/tmp/y")));
        assert!(a.json);

        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["--from"]).is_err());
        assert!(parse(&["--from="]).is_err());
        assert!(parse(&["--fetch"]).is_err());
        assert!(parse(&["/positional"]).is_err());
    }

    #[test]
    fn describe_db_renders_op_mp_and_counts() {
        let meta = TypeDbMeta {
            op: Some("1.29.5 (next, Public, 1234ad9e)".into()),
            mp: None,
            counts: crate::typedb::TypeDbCounts {
                functions: 854,
                classes: 75,
                enums: 71,
                funcdefs: 12,
                props: 8,
                ns_modules: 0,
            },
        };
        let line = describe_db("core", Path::new("/db/OpenplanetCore.json"), &meta);
        assert!(line.contains("core:  /db/OpenplanetCore.json"), "{line}");
        assert!(
            line.contains("op=1.29.5 (next, Public, 1234ad9e)"),
            "{line}"
        );
        assert!(!line.contains("mp="), "Core carries no mp: {line}");
        assert!(line.contains("854 functions, 75 classes"), "{line}");

        let dump = TypeDbMeta {
            op: Some("1.29.5".into()),
            mp: Some("2026-02-03 03:51:19".into()),
            counts: crate::typedb::TypeDbCounts {
                classes: 2585,
                ns_modules: 20,
                ..Default::default()
            },
        };
        let line = describe_db("game", Path::new("/db/OpenplanetNext.json"), &dump);
        assert!(line.contains("mp=2026-02-03 03:51:19"), "{line}");
        assert!(line.contains("2585 classes in 20 modules"), "{line}");
    }

    /// A DB with no `op` still renders — `status` must show what it found.
    #[test]
    fn describe_db_renders_missing_op_as_none() {
        let line = describe_db("core", Path::new("/db/x.json"), &TypeDbMeta::default());
        assert!(line.contains("op=<none>"), "{line}");
    }

    fn pair(dir: &str) -> DbPair {
        DbPair {
            core: PathBuf::from(dir).join(CORE_DB_FILENAME),
            game: PathBuf::from(dir).join("OpenplanetNext.json"),
        }
    }

    fn symbol(kind: DbSymbolKind, name: &str) -> DbSymbol {
        DbSymbol {
            kind,
            name: name.to_string(),
        }
    }

    /// A clean diff is one header plus one all-zero summary line — the shape
    /// `just typedb-refresh` is read for.
    #[test]
    fn render_diff_empty_is_header_plus_zero_summary() {
        let out = render_diff(&pair("/old"), &pair("/new"), &DbDiff::default());
        assert_eq!(out, "diff: /old -> /new\n0 added, 0 removed, 0 changed\n");
    }

    #[test]
    fn render_diff_labels_each_bucket_with_kind_and_name() {
        let diff = DbDiff {
            added: vec![symbol(DbSymbolKind::Class, "Net::Added")],
            removed: vec![symbol(DbSymbolKind::Function, "Display::GetWidth")],
            changed: vec![DbChange {
                kind: DbSymbolKind::Enum,
                name: "UI::Cond".to_string(),
                details: vec!["- value Always = 0".into(), "+ value Always = 1".into()],
            }],
        };
        let out = render_diff(&pair("/old"), &pair("/new"), &diff);
        assert!(out.contains("added (1):\n  + class Net::Added\n"), "{out}");
        assert!(
            out.contains("removed (1):\n  - function Display::GetWidth\n"),
            "{out}"
        );
        assert!(out.contains("changed (1):\n  ~ enum UI::Cond\n"), "{out}");
        assert!(out.contains("      - value Always = 0\n"), "{out}");
        assert!(out.contains("      + value Always = 1\n"), "{out}");
        assert!(out.ends_with("1 added, 1 removed, 1 changed\n"), "{out}");
    }

    /// Long buckets are capped and say so, pointing at `--json` for the rest —
    /// a 2000-row engine-API shift must stay readable.
    #[test]
    fn render_diff_caps_long_buckets_and_details() {
        let added = (0..MAX_PLAIN_ROWS + 3)
            .map(|i| symbol(DbSymbolKind::Class, &format!("Ns::C{i:03}")))
            .collect();
        let details = (0..MAX_PLAIN_DETAILS + 2)
            .map(|i| format!("+ method M{i}() -> void"))
            .collect();
        let diff = DbDiff {
            added,
            removed: Vec::new(),
            changed: vec![DbChange {
                kind: DbSymbolKind::Class,
                name: "Ns::Big".to_string(),
                details,
            }],
        };
        let out = render_diff(&pair("/old"), &pair("/new"), &diff);
        assert_eq!(
            out.matches("  + class ").count(),
            MAX_PLAIN_ROWS,
            "bucket rows capped: {out}"
        );
        assert!(
            out.contains("... and 3 more (use --json for the full list)"),
            "{out}"
        );
        assert!(
            out.contains("      ... and 2 more\n"),
            "details capped: {out}"
        );
        // The summary always reports the true totals, not the shown rows.
        assert!(
            out.contains(&format!(
                "{} added, 0 removed, 1 changed",
                MAX_PLAIN_ROWS + 3
            )),
            "{out}"
        );
    }

    #[test]
    fn diff_json_carries_buckets_and_summary() {
        let diff = DbDiff {
            added: Vec::new(),
            removed: vec![symbol(DbSymbolKind::Function, "Display::GetWidth")],
            changed: Vec::new(),
        };
        let value = diff_json(&pair("/old"), &pair("/new"), &diff);
        assert_eq!(value["old"], "/old");
        assert_eq!(value["new"], "/new");
        assert_eq!(value["removed"][0]["kind"], "function");
        assert_eq!(value["removed"][0]["name"], "Display::GetWidth");
        assert_eq!(value["summary"]["removed"], 1);
        assert!(value["added"].as_array().unwrap().is_empty());
    }

    #[test]
    fn parse_diff_args_variants() {
        let parse = |args: &[&str]| {
            parse_diff_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
        };
        let a = parse(&["/old"]).unwrap().unwrap();
        assert_eq!(a.old, Some(PathBuf::from("/old")));
        assert_eq!(a.new, None);
        assert!(!a.json);

        let a = parse(&["/old", "/new", "--json"]).unwrap().unwrap();
        assert_eq!(a.new, Some(PathBuf::from("/new")));
        assert!(a.json);

        // Flags may precede or follow the positionals.
        let a = parse(&["--json", "/old", "/new"]).unwrap().unwrap();
        assert!(a.json && a.new == Some(PathBuf::from("/new")));

        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&[]).is_err(), "OLD_DIR is required");
        assert!(parse(&["/a", "/b", "/c"]).is_err(), "at most two dirs");
        assert!(
            parse(&["--from", "/a"]).is_err(),
            "--from is not a diff option"
        );
    }
}
