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

use std::fs;
use std::io;
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
        Some("refresh") => run_refresh(&args[1..]),
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
            _ if arg == "--from" || arg.starts_with("--from=") => {
                let (dir, used) = take_dir_arg(args, i, "--from")?;
                parsed.from = Some(dir);
                i += used;
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
                // Same shape and same pretty-printing as the success path:
                // a machine surface that changes formatting between paths is
                // worse than one that is merely terse.
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "core": null,
                        "game": null,
                    }))
                    .unwrap_or_default()
                );
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

// === refresh ===

#[derive(Debug, Default)]
struct RefreshArgs {
    from: Option<PathBuf>,
    into: Option<PathBuf>,
    yes: bool,
}

/// Read a `--flag DIR` / `--flag=DIR` pair at `args[i]`, returning the value
/// and how many arguments it consumed.
fn take_dir_arg(args: &[String], i: usize, flag: &str) -> Result<(PathBuf, usize), String> {
    let missing = || format!("{flag} requires a directory argument");
    if let Some(value) = args[i].strip_prefix(&format!("{flag}=")) {
        if value.is_empty() {
            return Err(missing());
        }
        return Ok((PathBuf::from(value), 1));
    }
    let value = args.get(i + 1).ok_or_else(missing)?;
    Ok((PathBuf::from(value), 2))
}

fn parse_refresh_args(args: &[String]) -> Result<Option<RefreshArgs>, String> {
    let mut parsed = RefreshArgs::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => return Ok(None),
            "--yes" => {
                parsed.yes = true;
                i += 1;
            }
            _ if arg == "--from" || arg.starts_with("--from=") => {
                let (dir, used) = take_dir_arg(args, i, "--from")?;
                parsed.from = Some(dir);
                i += used;
            }
            _ if arg == "--into" || arg.starts_with("--into=") => {
                let (dir, used) = take_dir_arg(args, i, "--into")?;
                parsed.into = Some(dir);
                i += used;
            }
            _ => {
                return Err(format!(
                    "unknown typedb refresh argument: {arg}\n\
                     Run `openplanet-lsp typedb --help` for usage."
                ))
            }
        }
    }
    if parsed.into.is_none() {
        // GH #59 decision: never an implicit write path.
        return Err(
            "typedb refresh requires --into DIR (there is no default write path)".to_string(),
        );
    }
    Ok(Some(parsed))
}

fn run_refresh(args: &[String]) -> i32 {
    let parsed = match parse_refresh_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            print!("{TYPEDB_HELP}");
            return 0;
        }
        Err(err) => return usage_error(&err),
    };
    let into = parsed.into.expect("parse guarantees --into");

    let source = match DbPair::resolve(parsed.from.as_deref()) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return fail("no Openplanet install auto-detected; pass --from DIR");
        }
        Err(err) => return fail(&err),
    };
    let (source_core, source_game) = match probe_pair(&source) {
        Ok(meta) => meta,
        Err(err) => return fail(&err),
    };

    // The destination mirrors the source's filenames, so a Turbo/MP4 harvest
    // is not silently renamed to the Next dump.
    let target_paths = DbPair {
        core: into.join(file_name_of(&source.core)),
        game: into.join(file_name_of(&source.game)),
    };
    if let Err(err) = validate_refresh_destination(&source, &into, &target_paths) {
        return fail(&err);
    }

    let target = target_paths.clone();

    let mut out = String::new();
    out.push_str(&format!("source:  {}\n", pair_dir(&source)));
    out.push_str(&indent(&describe_pair(&source, &source_core, &source_game)));

    let target_exists = target.core.is_file() && target.game.is_file();
    if target_exists {
        let (target_core, target_game) = match probe_pair(&target) {
            Ok(meta) => meta,
            Err(err) => return fail(&err),
        };
        out.push_str(&format!("target:  {}\n", into.display()));
        out.push_str(&indent(&describe_pair(
            &target_paths,
            &target_core,
            &target_game,
        )));
        out.push('\n');
        match (load_index(&target), load_index(&source)) {
            (Ok(old), Ok(new)) => {
                out.push_str(&render_diff(
                    &target_paths,
                    &source,
                    &diff_indexes(&old, &new),
                ));
            }
            (Err(err), _) | (_, Err(err)) => return fail(&err),
        }
    } else {
        // First harvest: there is nothing to diff against, but the source
        // must still survive a real load before we copy it. `probe` only
        // proves the file is JSON — a schema-invalid DB would otherwise land
        // in the destination and be caught later by the shape guard instead
        // of here, where it is still harmless. The diff branch above already
        // loads both sides, so this closes the only unvalidated write path.
        if let Err(err) = load_index(&source) {
            return fail(&err);
        }
        out.push_str(&format!(
            "target:  {} (no existing type database — first harvest)\n",
            into.display()
        ));
    }

    // Byte comparison decides what actually needs writing: an `op`-identical
    // re-dump with no symbol drift can still differ byte-wise, and a
    // byte-identical file never needs a write.
    let files: Vec<(&Path, &Path)> = vec![
        (source.core.as_path(), target.core.as_path()),
        (source.game.as_path(), target.game.as_path()),
    ];
    let differing: Vec<&(&Path, &Path)> = files
        .iter()
        .filter(|(src, dst)| differs(src, dst))
        .collect();

    out.push('\n');
    out.push_str(&format!(
        "{} of {} files differ",
        differing.len(),
        files.len()
    ));

    if differing.is_empty() {
        out.push_str("; nothing to write\n");
        print!("{out}");
        return 0;
    }
    out.push('\n');

    if !parsed.yes {
        out.push_str(&format!(
            "dry run: nothing written (pass --yes to copy {} into {})\n",
            plural_files(differing.len()),
            into.display()
        ));
        print!("{out}");
        return 0;
    }

    // Created only now, so a dry run never makes a directory (the
    // --yes-only write contract).
    if let Err(err) = fs::create_dir_all(&into) {
        print!("{out}");
        return fail(&format!("failed to create {}: {err}", into.display()));
    }
    for (src, dst) in &differing {
        if let Err(err) = atomic_copy(src, dst) {
            print!("{out}");
            return fail(&err);
        }
    }
    out.push_str(&format!(
        "wrote {} into {}\n",
        plural_files(differing.len()),
        into.display()
    ));
    print!("{out}");
    0
}

fn plural_files(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Refuse destination shapes that could redirect a probe, diff, or write out
/// of the explicitly named `--into` directory. The final write still uses an
/// atomic rename so a symlink swapped in after this check is replaced rather
/// than followed.
fn validate_refresh_destination(
    source: &DbPair,
    into: &Path,
    target: &DbPair,
) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(into) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing to write through symbolic-link directory {}; remove it and use a real directory before retrying",
                into.display()
            ));
        }
        if !meta.is_dir() {
            return Err(format!(
                "refusing destination {}: it is not a directory; remove it and use a directory before retrying",
                into.display()
            ));
        }
    }

    let source_dir = source.core.parent().unwrap_or(source.core.as_path());
    if same_directory(source_dir, into) {
        return Err(format!(
            "refusing to refresh into source directory {}; choose a separate --into directory",
            into.display()
        ));
    }
    for home in [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
    {
        let install = PathBuf::from(home).join("OpenplanetNext");
        if same_directory(&install, into) {
            return Err(format!(
                "refusing to refresh into Openplanet install {}; choose a separate --into directory",
                into.display()
            ));
        }
    }

    for path in [&target.core, &target.game] {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "refusing symbolic link destination {}; remove it and replace it with a regular file before retrying",
                    path.display()
                ));
            }
            Ok(meta) if !meta.is_file() => {
                return Err(format!(
                    "refusing unsafe destination {}; remove it and use a regular file before retrying",
                    path.display()
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!(
                    "failed to inspect destination {}: {err}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Copy `src` over `dst` without leaving a half-written database behind.
///
/// Writes a temporary sibling in the destination directory, flushes it, then
/// renames it over `dst`. A same-directory rename replaces atomically on POSIX
/// and on Windows, so a crash mid-copy leaves either the old file or the new
/// one -- never a truncated DB, which would still parse as valid-but-empty
/// JSON and silently disable every type-aware diagnostic.
///
/// Threat model, deliberately narrow: `typedb refresh --yes` is a maintainer
/// command copying two files into a directory the caller named, normally this
/// repo's own `tests/fixtures/typedb`. That destination is *trusted* -- anyone
/// able to race writes into it can already edit those fixtures, or the build
/// scripts beside them, directly, so no privilege boundary is crossed here.
/// The guarantees worth paying for are the two honest-mistake failure modes:
/// writing *through* a symlink out of the named directory (refused up front by
/// [`validate_refresh_destination`]) and a torn file (handled here). Both are
/// portable, which matters because this project ships linux/darwin/win32 on
/// x64 and arm64 -- a Linux-only write path would break `--yes` on four of
/// those six targets.
fn atomic_copy(src: &Path, dst: &Path) -> Result<(), String> {
    let dir = dst
        .parent()
        .ok_or_else(|| format!("destination {} has no parent directory", dst.display()))?;
    let file_name = dst
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| format!("destination {} has no file name", dst.display()))?;
    let temp = dir.join(format!(
        ".{file_name}.typedb-refresh-{}",
        std::process::id()
    ));

    // A temp left behind by an earlier killed run must never be appended to.
    let _ = fs::remove_file(&temp);

    let write = (|| -> io::Result<()> {
        let mut input = fs::File::open(src)?;
        let mut output = fs::File::create(&temp)?;
        io::copy(&mut input, &mut output)?;
        // Durable before the rename: otherwise a crash can publish a short file.
        output.sync_all()
    })();
    if let Err(err) = write {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "failed to copy {} to {}: {err}",
            src.display(),
            temp.display()
        ));
    }

    fs::rename(&temp, dst).map_err(|err| {
        let _ = fs::remove_file(&temp);
        format!(
            "failed to replace {} with {}: {err}",
            dst.display(),
            temp.display()
        )
    })
}

/// True when `dst` is absent or its bytes differ from `src`.
fn differs(src: &Path, dst: &Path) -> bool {
    match (fs::read(src), fs::read(dst)) {
        (Ok(a), Ok(b)) => a != b,
        _ => true,
    }
}

fn indent(block: &str) -> String {
    block
        .lines()
        .map(|line| format!("  {line}\n"))
        .collect::<String>()
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
    fn parse_refresh_args_variants() {
        let parse = |args: &[&str]| {
            parse_refresh_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
        };
        let a = parse(&["--into", "/out"]).unwrap().unwrap();
        assert_eq!(a.into, Some(PathBuf::from("/out")));
        assert_eq!(a.from, None, "source defaults to auto-detect");
        assert!(!a.yes, "dry run is the default");

        let a = parse(&["--from=/install", "--into=/out", "--yes"])
            .unwrap()
            .unwrap();
        assert_eq!(a.from, Some(PathBuf::from("/install")));
        assert_eq!(a.into, Some(PathBuf::from("/out")));
        assert!(a.yes);

        // Order does not matter.
        let a = parse(&["--yes", "--into", "/out", "--from", "/install"])
            .unwrap()
            .unwrap();
        assert!(a.yes && a.from == Some(PathBuf::from("/install")));

        assert!(parse(&["--help"]).unwrap().is_none());
        // --into is mandatory: no implicit write path.
        assert!(parse(&[]).is_err());
        assert!(parse(&["--from", "/install"]).is_err());
        assert!(parse(&["--yes"]).is_err());
        // Malformed / unknown.
        assert!(parse(&["--into"]).is_err());
        assert!(parse(&["--into="]).is_err());
        assert!(parse(&["--into", "/out", "--from="]).is_err());
        assert!(parse(&["--into", "/out", "--fetch"]).is_err());
        assert!(parse(&["--into", "/out", "/positional"]).is_err());
    }

    #[test]
    fn take_dir_arg_space_and_equals_forms() {
        let args: Vec<String> = ["--from", "/a", "--into=/b", "--into"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            take_dir_arg(&args, 0, "--from").unwrap(),
            (PathBuf::from("/a"), 2)
        );
        assert_eq!(
            take_dir_arg(&args, 2, "--into").unwrap(),
            (PathBuf::from("/b"), 1)
        );
        // Trailing flag with no value.
        assert!(take_dir_arg(&args, 3, "--into").is_err());
    }

    #[test]
    fn differs_treats_missing_destination_as_different() {
        let dir = tmp_dir("differs");
        let src = dir.join("a.json");
        let dst = dir.join("b.json");
        std::fs::write(&src, "same").unwrap();
        assert!(differs(&src, &dst), "absent destination differs");
        std::fs::write(&dst, "same").unwrap();
        assert!(!differs(&src, &dst));
        std::fs::write(&dst, "other").unwrap();
        assert!(differs(&src, &dst));
    }

    /// Atomic replacement: the destination ends up as either the old bytes or
    /// the new ones, and no temporary sibling is left for the shape guard or a
    /// later `differs()` byte-compare to trip over.
    #[test]
    fn atomic_copy_replaces_in_place_and_leaves_no_temp_file() {
        let root = tmp_dir("atomic-copy");
        let src = root.join("src.json");
        let dst = root.join(CORE_DB_FILENAME);
        std::fs::write(&src, "new bytes").unwrap();
        std::fs::write(&dst, "old bytes").unwrap();

        atomic_copy(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new bytes");
        let leftovers = temp_files(&root);
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    /// First harvest: the same call creates the destination when absent.
    #[test]
    fn atomic_copy_creates_absent_destination() {
        let root = tmp_dir("atomic-copy-new");
        let src = root.join("src.json");
        std::fs::write(&src, "fresh").unwrap();
        let dst = root.join(CORE_DB_FILENAME);
        atomic_copy(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "fresh");
    }

    /// A leftover temp from an earlier killed run must not be appended to --
    /// its bytes must not survive into the destination.
    #[test]
    fn atomic_copy_overwrites_stale_temp_from_a_killed_run() {
        let root = tmp_dir("atomic-copy-stale");
        let src = root.join("src.json");
        std::fs::write(&src, "ab").unwrap();
        let dst = root.join(CORE_DB_FILENAME);
        let stale = root.join(format!(
            ".{CORE_DB_FILENAME}.typedb-refresh-{}",
            std::process::id()
        ));
        std::fs::write(&stale, "much longer stale content").unwrap();

        atomic_copy(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "ab");
        assert!(!stale.exists(), "stale temp should be gone");
    }

    /// A missing source errors, leaves no temp, and preserves the destination.
    #[test]
    fn atomic_copy_missing_source_errs_and_preserves_destination() {
        let root = tmp_dir("atomic-copy-nosrc");
        let dst = root.join(CORE_DB_FILENAME);
        std::fs::write(&dst, "keep me").unwrap();
        let err = atomic_copy(&root.join("absent.json"), &dst).unwrap_err();
        assert!(err.contains("absent.json"), "{err}");
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "keep me");
        assert!(temp_files(&root).is_empty(), "failed copy left a temp file");
    }

    /// A symlinked destination *directory* is refused before any write -- the
    /// footgun the write path does defend: writing outside the named directory.
    #[cfg(unix)]
    #[test]
    fn validate_refresh_destination_refuses_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let root = tmp_dir("symlink-dir");
        let source_dir = root.join("source");
        let real = root.join("real");
        let link = root.join("link");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        symlink(&real, &link).unwrap();

        let source = DbPair {
            core: source_dir.join(CORE_DB_FILENAME),
            game: source_dir.join("OpenplanetNext.json"),
        };
        let target = DbPair {
            core: link.join(CORE_DB_FILENAME),
            game: link.join("OpenplanetNext.json"),
        };
        let err = validate_refresh_destination(&source, &link, &target).unwrap_err();
        assert!(err.contains("symbolic-link directory"), "{err}");
    }

    /// A symlinked destination *file* is refused too: following it would
    /// rewrite something outside the named directory.
    #[cfg(unix)]
    #[test]
    fn validate_refresh_destination_refuses_symlinked_target_file() {
        use std::os::unix::fs::symlink;

        let root = tmp_dir("symlink-file");
        let source_dir = root.join("source");
        let into = root.join("into");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&into).unwrap();
        let outside = root.join("outside.json");
        std::fs::write(&outside, "outside").unwrap();
        symlink(&outside, into.join(CORE_DB_FILENAME)).unwrap();

        let source = DbPair {
            core: source_dir.join(CORE_DB_FILENAME),
            game: source_dir.join("OpenplanetNext.json"),
        };
        let target = DbPair {
            core: into.join(CORE_DB_FILENAME),
            game: into.join("OpenplanetNext.json"),
        };
        let err = validate_refresh_destination(&source, &into, &target).unwrap_err();
        assert!(err.contains("symbolic link destination"), "{err}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");
    }

    /// Names of leftover refresh temp files in `dir`.
    fn temp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("typedb-refresh"))
            .collect()
    }

    #[test]
    fn plural_files_agrees_with_count() {
        assert_eq!(plural_files(0), "0 files");
        assert_eq!(plural_files(1), "1 file");
        assert_eq!(plural_files(2), "2 files");
    }

    #[test]
    fn indent_prefixes_every_line() {
        assert_eq!(indent("a\nb\n"), "  a\n  b\n");
        assert_eq!(indent(""), "");
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
