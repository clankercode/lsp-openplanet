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
