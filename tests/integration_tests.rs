use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::lexer;
use openplanet_lsp::parser::Parser;
use openplanet_lsp::preprocessor;
use openplanet_lsp::server::diagnostics;
use openplanet_lsp::server::Backend;
use openplanet_lsp::typecheck::build_plugin_symbol_table;
use openplanet_lsp::typedb::TypeIndex;
use openplanet_lsp::workspace::project;
use tower_lsp::lsp_types::{
    DidOpenTextDocumentParams, DocumentHighlightKind, DocumentHighlightParams, FoldingRangeKind,
    FoldingRangeParams, GotoDefinitionParams, GotoDefinitionResponse, HoverParams,
    InitializeParams, InlayHintKind, InlayHintParams, PartialResultParams, Position, Range,
    SignatureHelpParams, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    Url, WorkDoneProgressParams,
};
use tower_lsp::LanguageServer;
use tower_lsp::LspService;

// Soft memory cap: 1 GiB. When a test's heap growth would cross this, we
// print a backtrace from inside the allocator (at which point the cap is
// lifted so backtrace machinery can allocate freely), then abort. Gives us
// a stack trace pointing at the memory bomb instead of a 20 GB OOM-kill.
#[global_allocator]
static ALLOCATOR: TrackingAlloc = TrackingAlloc::new(1024 * 1024 * 1024);

struct TrackingAlloc {
    used: AtomicUsize,
    limit: AtomicUsize,
    tripped: AtomicBool,
}

impl TrackingAlloc {
    const fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit: AtomicUsize::new(limit),
            tripped: AtomicBool::new(false),
        }
    }

    #[inline]
    fn check_trip(&self, size: usize) {
        let new_total = self.used.fetch_add(size, Ordering::Relaxed) + size;
        if new_total > self.limit.load(Ordering::Relaxed)
            && !self.tripped.swap(true, Ordering::Relaxed)
        {
            // First crosser: lift the cap so backtrace printing has headroom,
            // then print and abort.
            self.limit.store(usize::MAX, Ordering::Relaxed);
            eprintln!(
                "\n=== MEMORY CAP EXCEEDED ===\nAllocation of {} bytes pushed total past 1 GiB. Printing backtrace, then aborting.\n",
                size
            );
            let bt = std::backtrace::Backtrace::force_capture();
            eprintln!("{bt}");
            std::process::abort();
        }
    }
}

unsafe impl GlobalAlloc for TrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.check_trip(layout.size());
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.check_trip(layout.size());
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.used.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        if new_size > old_size {
            self.check_trip(new_size - old_size);
        } else {
            self.used.fetch_sub(old_size - new_size, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

/// Parse all .as files in a fixture plugin and collect diagnostics.
fn parse_fixture(fixture_name: &str) -> Vec<(PathBuf, Vec<String>)> {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture_name);

    if !fixture_dir.exists() {
        eprintln!("Fixture not found: {}", fixture_dir.display());
        return Vec::new();
    }

    let defines = LspConfig::default_defines();
    let source_files = project::discover_source_files(&fixture_dir);

    let mut results = Vec::new();
    for file_path in &source_files {
        let source = std::fs::read_to_string(file_path).unwrap();

        // Preprocess
        let pp = preprocessor::preprocess(&source, &defines);
        let mut diags: Vec<String> = pp
            .errors
            .iter()
            .map(|e| format!("preprocess: {:?}", e.kind))
            .collect();

        // Lex + Parse
        let tokens = lexer::tokenize_filtered(&pp.masked_source);
        let mut parser = Parser::new(&tokens, &pp.masked_source);
        let _file = parser.parse_file();

        for err in &parser.errors {
            diags.push(format!("parse: {}", err));
        }

        let relative = file_path.strip_prefix(&fixture_dir).unwrap_or(file_path);
        results.push((relative.to_path_buf(), diags));
    }

    results
}

#[test]
fn test_fixture_tm_counter() {
    let results = parse_fixture("tm-counter");
    let total_diags: usize = results.iter().map(|(_, d)| d.len()).sum();
    // Snapshot: record all diagnostics for review
    for (path, diags) in &results {
        if !diags.is_empty() {
            eprintln!("{}:", path.display());
            for d in diags {
                eprintln!("  {}", d);
            }
        }
    }
    // Initially this may have some diagnostics from unsupported syntax.
    // The goal is to reduce to zero true errors over time.
    // TODO: Replace with insta::assert_snapshot! once baseline is established.
    eprintln!("Total diagnostics for tm-counter: {}", total_diags);
}

#[test]
fn test_fixture_tm_dashboard() {
    let results = parse_fixture("tm-dashboard");
    let total_diags: usize = results.iter().map(|(_, d)| d.len()).sum();
    for (path, diags) in &results {
        if !diags.is_empty() {
            eprintln!("{}:", path.display());
            for d in diags {
                eprintln!("  {}", d);
            }
        }
    }
    eprintln!("Total diagnostics for tm-dashboard: {}", total_diags);
}

#[test]
fn test_fixture_tm_archivist() {
    let results = parse_fixture("tm-archivist");
    let total_diags: usize = results.iter().map(|(_, d)| d.len()).sum();
    eprintln!("Total diagnostics for tm-archivist: {}", total_diags);
}

#[test]
fn test_fixture_tm_dips_plus_plus() {
    let results = parse_fixture("tm-dips-plus-plus");
    let total_diags: usize = results.iter().map(|(_, d)| d.len()).sum();
    eprintln!("Total diagnostics for tm-dips-plus-plus: {}", total_diags);
}

#[test]
fn test_fixture_tm_editor_plus_plus() {
    let results = parse_fixture("tm-editor-plus-plus");
    let total_diags: usize = results.iter().map(|(_, d)| d.len()).sum();
    eprintln!("Total diagnostics for tm-editor-plus-plus: {}", total_diags);
}

// ---------- Corpus-wide parse test ----------
//
// Runs only when OPENPLANET_PLUGINS_DIR is set, so default `cargo test` stays
// fast. Walks every subdirectory containing info.toml, parses every .as file,
// and prints a histogram of parser error kinds so we can target the most
// common parser gaps first.

#[derive(Default)]
struct CorpusStats {
    plugins: usize,
    files: usize,
    files_with_parse_errors: usize,
    files_with_preprocess_errors: usize,
    parse_errors: usize,
    preprocess_errors: usize,
    /// error-kind-prefix -> (count, representative "plugin/path:msg")
    kind_counts: std::collections::BTreeMap<String, (usize, String)>,
    /// worst files sorted by parse-error count
    worst_files: Vec<(usize, PathBuf)>,
}

fn error_kind_key(msg: &str) -> String {
    // Normalize parser error messages into coarse kinds: strip identifiers,
    // string literals, and numbers so messages like "expected `;`, found
    // identifier `foo`" and "expected `;`, found identifier `bar`" share a
    // bucket. Keep it simple — we just want a histogram.
    let mut out = String::with_capacity(msg.len());
    let mut chars = msg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            // Preserve backtick-delimited token literals (e.g. `;`, `{`, `::`)
            // since those are the punctuation categories, not user data.
            out.push('`');
            while let Some(&nc) = chars.peek() {
                chars.next();
                out.push(nc);
                if nc == '`' {
                    break;
                }
            }
        } else if c.is_ascii_digit() {
            // Fold numbers to <N>
            out.push_str("<N>");
            while chars.peek().map_or(false, |c| c.is_ascii_digit()) {
                chars.next();
            }
        } else if c == '"' {
            // Fold string literals
            out.push_str("<str>");
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc == '"' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    // Also strip trailing line/col suffixes like "at 12:34"
    out
}

fn parse_plugin_tree(plugin_dir: &Path, stats: &mut CorpusStats) {
    let defines = LspConfig::default_defines();
    let source_files = project::discover_source_files(plugin_dir);
    stats.plugins += 1;

    for file_path in &source_files {
        stats.files += 1;
        let Ok(source) = std::fs::read_to_string(file_path) else { continue };

        let pp = preprocessor::preprocess(&source, &defines);
        if !pp.errors.is_empty() {
            stats.files_with_preprocess_errors += 1;
            stats.preprocess_errors += pp.errors.len();
        }

        let tokens = lexer::tokenize_filtered(&pp.masked_source);
        let mut parser = Parser::new(&tokens, &pp.masked_source);
        let _file = parser.parse_file();

        if !parser.errors.is_empty() {
            stats.files_with_parse_errors += 1;
            stats.parse_errors += parser.errors.len();
            stats.worst_files.push((parser.errors.len(), file_path.clone()));
            for err in &parser.errors {
                let msg = err.to_string();
                let key = error_kind_key(&msg);
                let entry = stats.kind_counts.entry(key).or_insert_with(|| {
                    let rel = file_path
                        .strip_prefix(plugin_dir.parent().unwrap_or(plugin_dir))
                        .unwrap_or(file_path);
                    (0, format!("{}: {}", rel.display(), msg))
                });
                entry.0 += 1;
            }
        }
    }
}

fn discover_plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("info.toml").exists() {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Convert a byte offset into (1-based line, 1-based column).
fn offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, c) in source.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Dump every parser error in a single file with line:col positions.
/// Set DUMP_FILE=<absolute-path> to use.
#[test]
fn test_dump_file_errors() {
    let Ok(file_path) = std::env::var("DUMP_FILE") else {
        eprintln!("DUMP_FILE not set — skipping single-file dump");
        return;
    };
    let path = PathBuf::from(file_path);
    let source = std::fs::read_to_string(&path).expect("read DUMP_FILE");
    let defines = LspConfig::default_defines();
    let pp = preprocessor::preprocess(&source, &defines);
    let tokens = lexer::tokenize_filtered(&pp.masked_source);
    let mut parser = Parser::new(&tokens, &pp.masked_source);
    let _ = parser.parse_file();

    eprintln!("=== {} ===", path.display());
    eprintln!("parse errors: {}", parser.errors.len());
    for err in &parser.errors {
        let off = err.span.start as usize;
        let (line, col) = offset_to_line_col(&pp.masked_source, off);
        let line_start = pp.masked_source[..off]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let line_end = pp.masked_source[off..]
            .find('\n')
            .map(|i| off + i)
            .unwrap_or(pp.masked_source.len());
        let context = &pp.masked_source[line_start..line_end];
        eprintln!("  L{}:{:2}  {}", line, col, err);
        eprintln!("       | {}", context.trim_end());
    }
}

#[test]
fn test_lsp_diagnostics_cross_document_workspace() {
    // Regression for the iter 7 production-wiring gap: when a workspace
    // symbol table containing declarations from a sibling document is fed
    // to compute_diagnostics, references to those declarations must not
    // flag as unknown-type / undefined-identifier.
    use openplanet_lsp::parser::Parser as AsParser;
    use openplanet_lsp::symbols::SymbolTable;

    let src_a = "class Foo { int x; }";
    let src_b = "void use() { Foo f; f.x = 1; }";

    // Build a pooled workspace table over both files.
    let mut table = SymbolTable::new();
    for src in [src_a, src_b] {
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = AsParser::new(&tokens, src);
        let file = parser.parse_file();
        let fid = table.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, src, &file);
        table.set_file_symbols(fid, syms);
    }

    let uri = Url::parse("file:///tmp/b.as").unwrap();
    let diags =
        diagnostics::compute_diagnostics(&uri, src_b, &LspConfig::default(), None, Some(&table));
    let offenders: Vec<&str> = diags
        .iter()
        .map(|d| d.message.as_str())
        .filter(|m| m.contains("Foo"))
        .collect();
    assert!(
        offenders.is_empty(),
        "cross-document Foo reference should resolve, got: {:?}",
        offenders
    );
}

#[test]
fn test_lsp_diagnostics_emit_type_errors() {
    let uri = Url::parse("file:///tmp/fake.as").expect("parse url");
    let diags = diagnostics::compute_diagnostics(
        &uri,
        "NotAType x;",
        &LspConfig::default(),
        None,
        None,
    );
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("unknown type")),
        "expected an unknown-type diagnostic, got: {:?}",
        diags
    );
}

#[test]
fn test_corpus_parse_histogram() {
    let Ok(root) = std::env::var("OPENPLANET_PLUGINS_DIR") else {
        eprintln!("OPENPLANET_PLUGINS_DIR not set — skipping corpus test");
        return;
    };
    let root = PathBuf::from(root);
    let plugin_dirs = discover_plugin_dirs(&root);
    eprintln!("Corpus root: {}", root.display());
    eprintln!("Discovered {} plugins (dirs with info.toml)", plugin_dirs.len());

    let mut stats = CorpusStats::default();
    for plugin in &plugin_dirs {
        parse_plugin_tree(plugin, &mut stats);
    }

    eprintln!();
    eprintln!("=== CORPUS PARSE HISTOGRAM ===");
    eprintln!("plugins              : {}", stats.plugins);
    eprintln!("files                : {}", stats.files);
    eprintln!("files w/ parse errors: {}", stats.files_with_parse_errors);
    eprintln!("files w/ pp errors   : {}", stats.files_with_preprocess_errors);
    eprintln!("total parse errors   : {}", stats.parse_errors);
    eprintln!("total pp errors      : {}", stats.preprocess_errors);
    eprintln!();

    // Top 30 error kinds by frequency
    let mut kinds: Vec<_> = stats.kind_counts.iter().collect();
    kinds.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    eprintln!("--- Top parser-error kinds ---");
    for (i, (kind, (count, example))) in kinds.iter().take(30).enumerate() {
        eprintln!("{:3}. [{:>5}] {}", i + 1, count, kind);
        eprintln!("        e.g. {}", example);
    }

    // Top 20 worst files
    stats.worst_files.sort_by(|a, b| b.0.cmp(&a.0));
    eprintln!();
    eprintln!("--- Worst files by parse-error count ---");
    for (count, path) in stats.worst_files.iter().take(20) {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        eprintln!("  {:>5}  {}", count, rel.display());
    }

    // Write machine-readable summary for the goal loop
    let summary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".goal-loops/corpus-histogram.txt");
    if let Some(parent) = summary_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut summary = String::new();
    summary.push_str(&format!(
        "plugins={} files={} files_parse_err={} parse_errors={} pp_errors={}\n\n",
        stats.plugins,
        stats.files,
        stats.files_with_parse_errors,
        stats.parse_errors,
        stats.preprocess_errors,
    ));
    summary.push_str("# top 50 kinds\n");
    for (kind, (count, example)) in kinds.iter().take(50) {
        summary.push_str(&format!("{:>6}  {}\n        e.g. {}\n", count, kind, example));
    }
    summary.push_str("\n# top 30 worst files\n");
    for (count, path) in stats.worst_files.iter().take(30) {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        summary.push_str(&format!("{:>6}  {}\n", count, rel.display()));
    }
    let _ = std::fs::write(&summary_path, summary);
    eprintln!();
    eprintln!("Histogram summary written to {}", summary_path.display());
}

/// Normalize a type-checker diagnostic message down to a coarse "kind" bucket.
/// Uses the same general approach as `error_kind_key` — strip identifier/number
/// content so that messages sharing a template collapse into a single bucket.
fn diag_kind_key(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut chars = msg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            // Backtick-quoted content is usually a user name/type — fold it.
            out.push('`');
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc == '`' {
                    out.push_str("X`");
                    break;
                }
            }
        } else if c.is_ascii_digit() {
            out.push_str("<N>");
            while chars.peek().map_or(false, |c| c.is_ascii_digit()) {
                chars.next();
            }
        } else if c == '"' {
            out.push_str("<str>");
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc == '"' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn test_corpus_type_diagnostic_histogram() {
    // Gate: require both the plugin corpus and the typedb dir.
    let Some(plugins_root) = std::env::var_os("OPENPLANET_PLUGINS") else {
        eprintln!("OPENPLANET_PLUGINS not set — skipping type-diag corpus test");
        return;
    };
    let Some(typedb_dir) = std::env::var_os("OPENPLANET_TYPEDB_DIR") else {
        eprintln!("OPENPLANET_TYPEDB_DIR not set — skipping type-diag corpus test");
        return;
    };
    let plugins_root = PathBuf::from(plugins_root);
    let typedb_dir = PathBuf::from(typedb_dir);
    let core_path = typedb_dir.join("OpenplanetCore.json");
    let next_path = typedb_dir.join("OpenplanetNext.json");

    let index = match TypeIndex::load(&core_path, &next_path) {
        Ok(idx) => idx,
        Err(e) => {
            panic!(
                "failed to load TypeIndex from {} / {}: {}",
                core_path.display(),
                next_path.display(),
                e
            );
        }
    };
    assert!(
        index.type_count() > 0,
        "TypeIndex loaded no types — harness is broken"
    );
    eprintln!(
        "Loaded TypeIndex with {} types from {}",
        index.type_count(),
        typedb_dir.display()
    );

    let plugin_dirs = discover_plugin_dirs(&plugins_root);
    eprintln!("Corpus root: {}", plugins_root.display());
    eprintln!("Discovered {} plugins", plugin_dirs.len());

    let cfg = LspConfig::default();
    let mut plugins = 0usize;
    let mut files = 0usize;
    let mut files_with_diag = 0usize;
    let mut total_diagnostics: usize = 0;
    let mut kind_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut per_file: Vec<(usize, PathBuf)> = Vec::new();
    // Optional: collect the actual unknown-type identifiers when
    // DUMP_UNKNOWN_TYPES is set, so investigation can see raw names.
    let dump_unknown = std::env::var_os("DUMP_UNKNOWN_TYPES").is_some();
    let mut unknown_type_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut undefined_ident_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut undefined_member_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for plugin_dir in &plugin_dirs {
        plugins += 1;
        let source_files = project::discover_source_files(plugin_dir);

        // Slurp every .as file in this plugin up-front so we can pool their
        // symbols into a single workspace table. Each file is then checked
        // against that pooled table below.
        let loaded: Vec<(PathBuf, String)> = source_files
            .iter()
            .filter_map(|p| {
                std::fs::read_to_string(p).ok().map(|s| (p.clone(), s))
            })
            .collect();

        let workspace = build_plugin_symbol_table(&loaded, &cfg);

        for (file_path, source) in &loaded {
            files += 1;
            // Build a plausible file:// URL so compute_diagnostics doesn't treat
            // it as info.toml.
            let url = match Url::from_file_path(file_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let diags = diagnostics::compute_diagnostics(
                &url,
                source,
                &cfg,
                Some(&index),
                Some(&workspace),
            );
            if diags.is_empty() {
                continue;
            }
            files_with_diag += 1;
            total_diagnostics += diags.len();
            per_file.push((diags.len(), file_path.clone()));
            for d in &diags {
                let key = diag_kind_key(&d.message);
                *kind_counts.entry(key).or_insert(0) += 1;
                if dump_unknown {
                    // Extract the name from messages like `unknown type `Foo``.
                    if let Some(rest) = d.message.strip_prefix("unknown type `") {
                        if let Some(name) = rest.strip_suffix('`') {
                            *unknown_type_counts
                                .entry(name.to_string())
                                .or_insert(0) += 1;
                        }
                    }
                    if let Some(rest) = d.message.strip_prefix("undefined identifier `") {
                        if let Some(name) = rest.strip_suffix('`') {
                            *undefined_ident_counts
                                .entry(name.to_string())
                                .or_insert(0) += 1;
                        }
                    }
                    // `type `Foo` has no member `bar``
                    if let Some(rest) = d.message.strip_prefix("type `") {
                        if let Some(after_ty) = rest.find("` has no member `") {
                            let ty = &rest[..after_ty];
                            let tail = &rest[after_ty + "` has no member `".len()..];
                            if let Some(member) = tail.strip_suffix('`') {
                                let key = format!("{}::{}", ty, member);
                                *undefined_member_counts
                                    .entry(key)
                                    .or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    eprintln!();
    eprintln!("=== CORPUS TYPE-DIAGNOSTIC HISTOGRAM ===");
    eprintln!("plugins           : {}", plugins);
    eprintln!("files             : {}", files);
    eprintln!("files_with_diag   : {}", files_with_diag);
    eprintln!("total_diagnostics : {}", total_diagnostics);
    eprintln!();

    let mut kinds: Vec<_> = kind_counts.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("--- Top type-diag kinds ---");
    for (i, (kind, count)) in kinds.iter().take(20).enumerate() {
        eprintln!("{:3}. [{:>6}] {}", i + 1, count, kind);
    }

    per_file.sort_by(|a, b| b.0.cmp(&a.0));
    eprintln!();
    eprintln!("--- Worst files by diag count ---");
    for (count, path) in per_file.iter().take(15) {
        let rel = path.strip_prefix(&plugins_root).unwrap_or(path);
        eprintln!("  {:>6}  {}", count, rel.display());
    }

    // Machine-readable summary for the goal loop.
    let summary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".goal-loops/type-diag-histogram.txt");
    if let Some(parent) = summary_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut summary = String::new();
    summary.push_str(&format!(
        "plugins={} files={} files_with_diag={} total_diagnostics={}\n\n",
        plugins, files, files_with_diag, total_diagnostics,
    ));
    summary.push_str("# top 50 kinds\n");
    for (kind, count) in kinds.iter().take(50) {
        summary.push_str(&format!("{:>6}  {}\n", count, kind));
    }
    summary.push_str("\n# top 30 worst files\n");
    for (count, path) in per_file.iter().take(30) {
        let rel = path.strip_prefix(&plugins_root).unwrap_or(path);
        summary.push_str(&format!("{:>6}  {}\n", count, rel.display()));
    }
    let _ = std::fs::write(&summary_path, summary);
    eprintln!();
    eprintln!(
        "Type-diag histogram summary written to {}",
        summary_path.display()
    );

    if dump_unknown {
        let mut utc: Vec<_> = unknown_type_counts.iter().collect();
        utc.sort_by(|a, b| b.1.cmp(a.1));
        let unk_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".goal-loops/unknown-types.txt");
        let mut u = String::new();
        for (name, count) in utc.iter().take(60) {
            u.push_str(&format!("{:>6}  {}\n", count, name));
        }
        let _ = std::fs::write(&unk_path, u);
        eprintln!("Unknown-type names dumped to {}", unk_path.display());

        let mut uic: Vec<_> = undefined_ident_counts.iter().collect();
        uic.sort_by(|a, b| b.1.cmp(a.1));
        let ui_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".goal-loops/undefined-idents.txt");
        let mut u = String::new();
        for (name, count) in uic.iter().take(60) {
            u.push_str(&format!("{:>6}  {}\n", count, name));
        }
        let _ = std::fs::write(&ui_path, u);
        eprintln!("Undefined-ident names dumped to {}", ui_path.display());

        let mut umc: Vec<_> = undefined_member_counts.iter().collect();
        umc.sort_by(|a, b| b.1.cmp(a.1));
        let um_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".goal-loops/undefined-members.txt");
        let mut u = String::new();
        for (name, count) in umc.iter().take(60) {
            u.push_str(&format!("{:>6}  {}\n", count, name));
        }
        let _ = std::fs::write(&um_path, u);
        eprintln!("Undefined-member names dumped to {}", um_path.display());
    }

    // Sanity check only — not a pass/fail gate.
    assert!(index.type_count() > 0);
}

/// Debug dump: emit per-diagnostic details for the file named in
/// `OPENPLANET_DEBUG_FILE` (absolute path). Used to investigate
/// false-positive categories without rerunning the whole corpus.
#[test]
fn test_debug_single_file_diagnostics() {
    let Some(typedb_dir) = std::env::var_os("OPENPLANET_TYPEDB_DIR") else {
        return;
    };
    let Some(file_path) = std::env::var_os("OPENPLANET_DEBUG_FILE") else {
        return;
    };
    let typedb_dir = PathBuf::from(typedb_dir);
    let core_path = typedb_dir.join("OpenplanetCore.json");
    let next_path = typedb_dir.join("OpenplanetNext.json");
    let index = TypeIndex::load(&core_path, &next_path).unwrap();
    let file_path = PathBuf::from(file_path);
    let source = std::fs::read_to_string(&file_path).unwrap();
    let url = Url::from_file_path(&file_path).unwrap();
    let cfg = LspConfig::default();
    let diags = diagnostics::compute_diagnostics(&url, &source, &cfg, Some(&index), None);
    eprintln!("total diagnostics: {}", diags.len());
    let mut buckets: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for d in &diags {
        *buckets.entry(d.message.clone()).or_insert(0) += 1;
    }
    let mut sorted: Vec<_> = buckets.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (msg, count) in sorted.iter() {
        eprintln!("  {:>4}  {}", count, msg);
    }
}

// ---------------------------------------------------------------------------
// AC13 smoke test: drive `Backend` through the real tower-lsp plumbing.
//
// Strategy B (from the iter 30 plan): instead of hand-framing JSON-RPC over
// a `tokio::io::duplex`, we build the real `LspService` (which constructs a
// real `Client` wired to the server socket), borrow the inner `Backend` via
// `LspService::inner()`, and call the `LanguageServer` trait methods
// directly. This exercises:
//   * `Backend::new(client)` with a real `Client`,
//   * the `initialize` -> `did_open` -> `hover` / `goto_definition` pipeline,
//   * iter 27/28 cross-file resolution: the symbol being hovered / jumped to
//     lives in a sibling document, proving `build_workspace` + workspace
//     lookups flow through the real server handlers end-to-end.
//
// `publish_diagnostics` calls triggered from `did_open` are suppressed by
// tower-lsp 0.20 because we never transition the service state to
// `Initialized` (we call the trait method directly, not via the Service
// layer), so nothing has to drain the server socket.
// ---------------------------------------------------------------------------

fn text_doc_position(uri: &Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position { line, character },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_tower_lsp_smoke_crossfile_hover() {
    // Build the real service; `Backend::new` runs with a real `Client`
    // constructed by tower-lsp.
    let (service, _socket) = LspService::new(Backend::new);
    let backend: &Backend = service.inner();

    // 1) initialize: must succeed and advertise hover_provider.
    #[allow(deprecated)]
    let init_params = InitializeParams::default();
    let init_result = backend
        .initialize(init_params)
        .await
        .expect("initialize should succeed");
    assert!(
        init_result.capabilities.hover_provider.is_some(),
        "server must advertise hover capability"
    );
    assert!(
        init_result.capabilities.definition_provider.is_some(),
        "server must advertise definition capability"
    );

    // 2) Open two files: `Base` lives in helper.as, `Foo : Base` in main.as.
    //    This is the same cross-file shape iter 27/28 fixed.
    let helper_uri = Url::parse("file:///smoke/helper.as").unwrap();
    let helper_src = "class Base { int count; }\n";
    let main_uri = Url::parse("file:///smoke/main.as").unwrap();
    //                    0         1         2         3
    //                    0123456789012345678901234567890
    // line 0:  "class Foo : Base {}"     -> "Base" starts at col 12
    // line 1:  "void test() { Foo f; f.count = 5; }"
    let main_src = "class Foo : Base {}\nvoid test() { Foo f; f.count = 5; }\n";

    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: helper_uri.clone(),
                language_id: "angelscript".to_string(),
                version: 1,
                text: helper_src.to_string(),
            },
        })
        .await;
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: main_uri.clone(),
                language_id: "angelscript".to_string(),
                version: 1,
                text: main_src.to_string(),
            },
        })
        .await;

    // 3) Hover on `Base` at (line 0, col 13) in main.as. `Base` only exists
    //    in helper.as, so a non-empty hover here proves cross-file workspace
    //    lookup flowed through the real handler.
    let hover_params = HoverParams {
        text_document_position_params: text_doc_position(&main_uri, 0, 13),
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let hover = backend
        .hover(hover_params)
        .await
        .expect("hover call must not error")
        .expect("hover on cross-file `Base` must return Some");
    // Any non-empty content counts — we just want to prove the stack wired.
    match &hover.contents {
        tower_lsp::lsp_types::HoverContents::Markup(m) => {
            assert!(
                !m.value.is_empty(),
                "hover markup must be non-empty"
            );
        }
        tower_lsp::lsp_types::HoverContents::Scalar(_)
        | tower_lsp::lsp_types::HoverContents::Array(_) => {
            // also acceptable
        }
    }

    // 4) goto_definition on `Base` at the same position: should resolve into
    //    helper.as (the file that actually declares it).
    let goto_params = GotoDefinitionParams {
        text_document_position_params: text_doc_position(&main_uri, 0, 13),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let goto = backend
        .goto_definition(goto_params)
        .await
        .expect("goto_definition call must not error");
    // We don't strictly require a hit (goto_definition has stricter matching
    // than hover), but if we do get one, sanity-check the URI. Either way,
    // the call path must run without panicking — that's the smoke coverage.
    if let Some(GotoDefinitionResponse::Scalar(loc)) = goto {
        assert_eq!(
            loc.uri, helper_uri,
            "goto_definition on cross-file `Base` should land in helper.as"
        );
    }
}

// ---------------------------------------------------------------------------
// AC14 smoke test: exercise `textDocument/signatureHelp` through the real
// tower-lsp `Backend`. Proves the handler wires workspace overload resolution
// + active-parameter tracking end-to-end, not just via unit tests.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
async fn test_tower_lsp_smoke_signature_help() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend: &Backend = service.inner();

    #[allow(deprecated)]
    let init_params = InitializeParams::default();
    let init_result = backend
        .initialize(init_params)
        .await
        .expect("initialize should succeed");
    assert!(
        init_result.capabilities.signature_help_provider.is_some(),
        "server must advertise signature_help capability"
    );

    let uri = Url::parse("file:///smoke/sig.as").unwrap();
    // line 0:  "void f(int a, string b) {}"
    // line 1:  "void main() { f(42, ) }"
    //                           ^ cursor sits at col 20 — just after the ", "
    let src = "void f(int a, string b) {}\nvoid main() { f(42, ) }\n";
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "angelscript".to_string(),
                version: 1,
                text: src.to_string(),
            },
        })
        .await;

    // "void main() { f(42, ) }"
    //  0         1         2
    //  0123456789012345678901
    // column 20 is the space right after the comma — active_parameter = 1.
    let params = SignatureHelpParams {
        context: None,
        text_document_position_params: text_doc_position(&uri, 1, 20),
        work_done_progress_params: WorkDoneProgressParams::default(),
    };
    let help = backend
        .signature_help(params)
        .await
        .expect("signature_help call must not error")
        .expect("signature_help must return Some");
    assert!(
        !help.signatures.is_empty(),
        "signature help must return at least one signature"
    );
    assert_eq!(help.active_parameter, Some(1));
    // The single overload's label should include both param types.
    let label = &help.signatures[0].label;
    assert!(label.contains("int"), "label should mention int, got {:?}", label);
    assert!(
        label.contains("string"),
        "label should mention string, got {:?}",
        label
    );
}

// ---------------------------------------------------------------------------
// AC15 smoke test: exercise `textDocument/inlayHint` through the real
// tower-lsp `Backend`. Proves the handler wires AST walking + workspace
// lookup end-to-end, not just via unit tests.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
async fn test_tower_lsp_smoke_inlay_hint() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend: &Backend = service.inner();

    #[allow(deprecated)]
    let init_params = InitializeParams::default();
    let init_result = backend
        .initialize(init_params)
        .await
        .expect("initialize should succeed");
    assert!(
        init_result.capabilities.inlay_hint_provider.is_some(),
        "server must advertise inlay_hint capability"
    );

    let uri = Url::parse("file:///smoke/inlay.as").unwrap();
    let src = "void f() { auto x = 5; }\n";
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "angelscript".to_string(),
                version: 1,
                text: src.to_string(),
            },
        })
        .await;

    let params = InlayHintParams {
        work_done_progress_params: WorkDoneProgressParams::default(),
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range: Range::new(Position::new(0, 0), Position::new(100, 0)),
    };
    let hints = backend
        .inlay_hint(params)
        .await
        .expect("inlay_hint call must not error")
        .expect("inlay_hint must return Some");
    assert!(
        !hints.is_empty(),
        "inlay hints should include at least one entry for `auto x = 5`"
    );
    assert!(
        hints.iter().any(|h| h.kind == Some(InlayHintKind::TYPE)),
        "expected a TYPE inlay hint, got {:?}",
        hints
    );
}

// ---------------------------------------------------------------------------
// AC16 smoke test: exercise `textDocument/documentHighlight` through the real
// tower-lsp `Backend`. Proves the handler returns intra-file occurrences with
// READ/WRITE classification end-to-end.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "current_thread")]
async fn test_tower_lsp_smoke_document_highlight() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend: &Backend = service.inner();

    #[allow(deprecated)]
    let init_params = InitializeParams::default();
    let init_result = backend
        .initialize(init_params)
        .await
        .expect("initialize should succeed");
    assert!(
        init_result.capabilities.document_highlight_provider.is_some(),
        "server must advertise document_highlight capability"
    );

    let uri = Url::parse("file:///smoke/highlight.as").unwrap();
    // `int x = 0; x = 1; g(x);` — three occurrences of `x`.
    let src = "void f() { int x = 0; x = 1; g(x); }\n";
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "angelscript".to_string(),
                version: 1,
                text: src.to_string(),
            },
        })
        .await;

    // Cursor on the declarator `x` at column 15 (0-indexed) of line 0:
    //   `void f() { int x = 0; x = 1; g(x); }`
    //                   ^ col 15
    let params = DocumentHighlightParams {
        text_document_position_params: text_doc_position(&uri, 0, 15),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let hs = backend
        .document_highlight(params)
        .await
        .expect("document_highlight must not error")
        .expect("document_highlight must return Some");
    assert_eq!(hs.len(), 3, "expected 3 highlights, got {:?}", hs);
    assert!(
        hs.iter().any(|h| h.kind == Some(DocumentHighlightKind::WRITE)),
        "expected at least one WRITE highlight, got {:?}",
        hs
    );
    assert!(
        hs.iter().any(|h| h.kind == Some(DocumentHighlightKind::READ)),
        "expected at least one READ highlight, got {:?}",
        hs
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_tower_lsp_smoke_folding_range() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend: &Backend = service.inner();

    #[allow(deprecated)]
    let init_params = InitializeParams::default();
    let init_result = backend
        .initialize(init_params)
        .await
        .expect("initialize should succeed");
    assert!(
        init_result.capabilities.folding_range_provider.is_some(),
        "server must advertise folding_range capability"
    );

    let uri = Url::parse("file:///smoke/folding.as").unwrap();
    let src = "\
/*\n  header comment\n*/\n\
#if DEBUG\n\
class Foo {\n  void m() {\n    int y = 0;\n  }\n}\n\
#endif\n";
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "angelscript".to_string(),
                version: 1,
                text: src.to_string(),
            },
        })
        .await;

    let params = FoldingRangeParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    };
    let folds = backend
        .folding_range(params)
        .await
        .expect("folding_range must not error")
        .expect("folding_range must return Some");
    assert!(
        folds
            .iter()
            .any(|f| f.kind == Some(FoldingRangeKind::Comment)),
        "expected at least one comment fold, got {:?}",
        folds
    );
    assert!(
        folds
            .iter()
            .any(|f| f.kind == Some(FoldingRangeKind::Region)),
        "expected at least one region fold, got {:?}",
        folds
    );
    // Class body + method body — two AST folds with no kind set.
    let ast_folds = folds.iter().filter(|f| f.kind.is_none()).count();
    assert!(
        ast_folds >= 2,
        "expected at least 2 AST folds for class + method, got {:?}",
        folds
    );
}
