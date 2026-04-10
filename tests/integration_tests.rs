use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::lexer;
use openplanet_lsp::parser::Parser;
use openplanet_lsp::preprocessor;
use openplanet_lsp::workspace::project;

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
