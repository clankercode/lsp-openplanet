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
use crate::typedb::TypeDbMeta;

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
        Some("diff") => not_implemented("diff"),
        Some("refresh") => not_implemented("refresh"),
        Some(other) => usage_error(&format!("unknown typedb subcommand: {other}")),
        None => usage_error("typedb requires a subcommand"),
    }
}

fn usage_error(msg: &str) -> i32 {
    eprintln!("{msg}");
    eprint!("{TYPEDB_USAGE}");
    2
}

fn not_implemented(sub: &str) -> i32 {
    eprintln!("typedb {sub}: not implemented yet (GH #59)");
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
