//! `check --watch` adapter: file events → re-check → TUI events.
//!
//! Watching lives here (main crate), not in the TUI module.
//! Checks run on a background thread so the UI can paint `checking…` and stay responsive.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};
use crate::tui::{
    DiagItem, RunStatus, Severity, Snapshot, SourceEvent, TuiDataSource, WatchHealth,
};
use tower_lsp::lsp_types::DiagnosticSeverity;

use super::{run_check, CheckOptions, CheckReport, CliError};

const DEBOUNCE: Duration = Duration::from_millis(250);

/// Result from a background check worker.
struct CheckOutcome {
    generation: u64,
    elapsed: Duration,
    result: Result<CheckReport, String>,
}

/// Host-side data source for the watch TUI.
pub struct WatchDataSource {
    options: CheckOptions,
    root_label: String,
    pending: VecDeque<SourceEvent>,
    dirty: Arc<AtomicBool>,
    /// Keep the debouncer alive for the TUI lifetime.
    _debouncer: Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>,
    /// True while a background check is in flight.
    checking: bool,
    /// Edits while checking → one follow-up run after completion.
    rerun_needed: bool,
    generation: u64,
    result_tx: Sender<CheckOutcome>,
    result_rx: Receiver<CheckOutcome>,
    /// Injectable for tests; default uses [`run_check`].
    checker: Arc<dyn Fn(&CheckOptions) -> Result<CheckReport, CliError> + Send + Sync>,
    /// How many checks have been started (tests).
    pub checks_started: u64,
    watch_health: WatchHealth,
}

impl WatchDataSource {
    pub fn new(options: CheckOptions) -> Result<Self, CliError> {
        Self::with_checker(options, Arc::new(|opts| run_check(opts)))
    }

    /// Construct with an injectable checker (unit tests).
    pub fn with_checker(
        options: CheckOptions,
        checker: Arc<dyn Fn(&CheckOptions) -> Result<CheckReport, CliError> + Send + Sync>,
    ) -> Result<Self, CliError> {
        let root = resolve_root_label(&options.path)?;
        // Single initial trigger: dirty starts true (no separate first_poll).
        let dirty = Arc::new(AtomicBool::new(true));
        let dirty_flag = Arc::clone(&dirty);

        let (watch_health, debouncer) = match start_watcher(&root, Arc::clone(&dirty_flag)) {
            Ok(d) => (WatchHealth::Active, Some(d)),
            Err(err) => {
                tracing::warn!("file watch unavailable ({err}); use `r` to recheck");
                (
                    WatchHealth::ManualOnly {
                        reason: err.clone(),
                    },
                    None,
                )
            }
        };

        let label = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());

        let (result_tx, result_rx) = mpsc::channel();

        let mut src = Self {
            options,
            root_label: label,
            pending: VecDeque::new(),
            dirty,
            _debouncer: debouncer,
            checking: false,
            rerun_needed: false,
            generation: 0,
            result_tx,
            result_rx,
            checker,
            checks_started: 0,
            watch_health: watch_health.clone(),
        };
        // Surface watch health once at start.
        if !matches!(watch_health, WatchHealth::Active) {
            src.pending
                .push_back(SourceEvent::WatchHealth(watch_health));
        }
        Ok(src)
    }

    fn start_check_if_needed(&mut self) {
        let dirty = self.dirty.swap(false, Ordering::SeqCst);
        if !dirty && !self.rerun_needed {
            return;
        }
        if self.checking {
            // Coalesce: remember we need one more run after current finishes.
            if dirty {
                self.rerun_needed = true;
            }
            return;
        }

        self.rerun_needed = false;
        self.checking = true;
        self.checks_started = self.checks_started.saturating_add(1);
        self.generation = self.generation.saturating_add(1);
        let gen = self.generation;

        // Paint checking immediately (before worker finishes).
        self.pending
            .push_back(SourceEvent::Status(RunStatus::Running));

        let options = self.options.clone();
        let checker = Arc::clone(&self.checker);
        let tx = self.result_tx.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let result = checker(&options).map_err(|e| e.to_string());
            let elapsed = started.elapsed();
            let _ = tx.send(CheckOutcome {
                generation: gen,
                elapsed,
                result,
            });
        });
    }

    fn poll_worker(&mut self) {
        // Non-blocking drain of completed checks.
        while let Ok(outcome) = self.result_rx.try_recv() {
            // Ignore stale generations (shouldn't happen with single worker, but safe).
            if outcome.generation != self.generation {
                continue;
            }
            self.checking = false;
            match outcome.result {
                Ok(report) => {
                    let mut snap =
                        report_to_snapshot(&report, &self.root_label, outcome.elapsed);
                    snap.watch_health = self.watch_health.clone();
                    snap.stale = false;
                    self.pending
                        .push_back(SourceEvent::Diagnostics(snap));
                }
                Err(message) => {
                    self.pending
                        .push_back(SourceEvent::Status(RunStatus::Failed { message }));
                }
            }
            // One coalesced follow-up if edits arrived during the run.
            if self.rerun_needed || self.dirty.load(Ordering::SeqCst) {
                self.dirty.store(true, Ordering::SeqCst);
                self.rerun_needed = false;
            }
        }
    }
}

impl TuiDataSource for WatchDataSource {
    fn try_recv(&mut self) -> Option<SourceEvent> {
        // Prefer delivering already-queued UI events first so Running paints.
        if let Some(ev) = self.pending.pop_front() {
            return Some(ev);
        }
        self.poll_worker();
        if let Some(ev) = self.pending.pop_front() {
            return Some(ev);
        }
        self.start_check_if_needed();
        self.pending.pop_front()
    }

    fn request_refresh(&mut self) {
        self.dirty.store(true, Ordering::SeqCst);
    }
}

fn resolve_root_label(path: &Path) -> Result<PathBuf, CliError> {
    let path = if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_path_buf()
    };
    let root = crate::workspace::project::find_workspace_root(&path).ok_or_else(|| {
        CliError::Check(format!(
            "could not find info.toml at or above {}",
            path.display()
        ))
    })?;
    root.canonicalize()
        .map_err(|e| CliError::Check(format!("failed to resolve {}: {e}", root.display())))
}

fn start_watcher(
    root: &Path,
    dirty: Arc<AtomicBool>,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>, String> {
    let root = root.to_path_buf();
    let mut debouncer = new_debouncer(
        DEBOUNCE,
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| match res {
            Ok(events) => {
                for ev in events {
                    if is_interesting_path(&ev.path) {
                        dirty.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
            Err(err) => {
                tracing::warn!("watch error: {err:?}");
            }
        },
    )
    .map_err(|e| e.to_string())?;

    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;
    Ok(debouncer)
}

/// Paths that should trigger a re-check.
pub fn is_interesting_path(path: &Path) -> bool {
    // Ignore noisy dirs
    for comp in path.components() {
        let s = comp.as_os_str();
        if s == ".git" || s == "target" || s == "node_modules" || s == ".hg" || s == ".svn" {
            return false;
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name == "info.toml" {
            return true;
        }
        if name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".tmp") {
            return false;
        }
        if name.starts_with(".#") {
            return false;
        }
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("as") | Some("inc") => true,
        Some("png" | "jpg" | "jpeg" | "gif" | "ogg" | "wav" | "mp3" | "zip" | "op") => false,
        _ => false,
    }
}

pub fn report_to_snapshot(report: &CheckReport, root_label: &str, elapsed: Duration) -> Snapshot {
    let mut source_cache: std::collections::HashMap<PathBuf, Option<String>> =
        std::collections::HashMap::new();

    let diagnostics = report
        .diagnostics
        .iter()
        .map(|item| {
            let rel = item
                .path
                .strip_prefix(&report.root)
                .unwrap_or(&item.path)
                .to_path_buf();
            let range = item.diagnostic.range;
            let line_idx = range.start.line as usize;
            let raw_line = source_cache
                .entry(item.path.clone())
                .or_insert_with(|| std::fs::read_to_string(&item.path).ok())
                .as_ref()
                .and_then(|src| {
                    src.lines()
                        .nth(line_idx)
                        .map(|l| l.trim_end_matches('\r').to_string())
                });

            // Expand tabs for display; map LSP cols through the same expand.
            let (source_line, start_col, end_col) = match raw_line.as_deref() {
                Some(raw) => {
                    let expanded = expand_tabs(raw, 4);
                    let start_d = lsp_col_to_display(raw, range.start.character as usize, 4);
                    let end_d = if range.end.line == range.start.line {
                        let e = lsp_col_to_display(raw, range.end.character as usize, 4);
                        if e <= start_d {
                            start_d + 1
                        } else {
                            e
                        }
                    } else {
                        // Multi-line: carets through end of start line (CLI pretty parity).
                        expanded.chars().count().max(start_d + 1)
                    };
                    // Store 1-based exclusive end for DiagItem.
                    (Some(expanded), (start_d as u32) + 1, (end_d as u32) + 1)
                }
                None => {
                    let start_col = range.start.character + 1;
                    let end_col = if range.end.line == range.start.line
                        && range.end.character > range.start.character
                    {
                        range.end.character + 1
                    } else {
                        start_col
                    };
                    (None, start_col, end_col)
                }
            };

            DiagItem {
                severity: map_severity(item.diagnostic.severity),
                path: rel,
                line: range.start.line + 1,
                col: start_col,
                end_col,
                message: item.diagnostic.message.clone(),
                source_line,
            }
        })
        .collect();

    Snapshot {
        root_label: root_label.to_string(),
        diagnostics,
        status: RunStatus::Ready { duration: elapsed },
        stale: false,
        watch_health: WatchHealth::Active,
    }
}

/// Map a 0-based character index on a raw line to a display column after tab expand.
fn lsp_col_to_display(raw_line: &str, lsp_col: usize, tab_width: usize) -> usize {
    let mut display = 0usize;
    let mut i = 0usize;
    for ch in raw_line.chars() {
        if i >= lsp_col {
            break;
        }
        if ch == '\t' {
            display += tab_width - (display % tab_width);
        } else {
            display += 1;
        }
        i += 1;
    }
    // If lsp_col is past the end, pad.
    if i < lsp_col {
        display += lsp_col - i;
    }
    display
}

fn expand_tabs(line: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = tab_width - (col % tab_width);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

fn map_severity(sev: Option<DiagnosticSeverity>) -> Severity {
    match sev {
        Some(DiagnosticSeverity::ERROR) | None => Severity::Error,
        Some(DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
        Some(DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    }
}

/// Run the interactive watch TUI until quit.
pub fn run_watch(options: CheckOptions) -> Result<(), CliError> {
    let source = WatchDataSource::new(options)?;
    crate::tui::run(source, crate::tui::RunOptions::default())
        .map_err(|e| CliError::Check(format!("watch TUI failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomOrd};
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn interesting_paths() {
        assert!(is_interesting_path(Path::new("src/Main.as")));
        assert!(is_interesting_path(Path::new("/p/info.toml")));
        assert!(!is_interesting_path(Path::new("src/foo.png")));
        assert!(!is_interesting_path(Path::new(".git/config")));
        assert!(!is_interesting_path(Path::new("target/debug/x")));
        assert!(!is_interesting_path(Path::new("src/Main.as~")));
    }

    #[test]
    fn lsp_col_to_display_tabs() {
        assert_eq!(lsp_col_to_display("\tx", 0, 4), 0);
        assert_eq!(lsp_col_to_display("\tx", 1, 4), 4);
        assert_eq!(lsp_col_to_display("a\tb", 2, 4), 4);
    }

    fn showcase_options() -> CheckOptions {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/showcase-diags");
        CheckOptions {
            path: root,
            typedb_dir: Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb"),
            ),
            no_typedb: false,
            plugins_dirs: Vec::new(),
            plugin_files_search_paths: vec![PathBuf::from("src")],
            format: crate::cli::CheckFormat::Plain,
            watch: true,
        }
    }

    #[test]
    fn startup_runs_exactly_one_check() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let checker: Arc<dyn Fn(&CheckOptions) -> Result<CheckReport, CliError> + Send + Sync> =
            Arc::new(move |opts| {
                calls2.fetch_add(1, AtomOrd::SeqCst);
                // tiny delay so async path is exercised
                thread::sleep(Duration::from_millis(20));
                run_check(opts)
            });

        let mut src = WatchDataSource::with_checker(showcase_options(), checker).unwrap();

        // Drain until we get Diagnostics (Running first, then result).
        let mut saw_running = false;
        let mut saw_diags = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !saw_diags {
            match src.try_recv() {
                Some(SourceEvent::Status(RunStatus::Running)) => saw_running = true,
                Some(SourceEvent::Diagnostics(_)) => saw_diags = true,
                Some(SourceEvent::WatchHealth(_)) => {}
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
        assert!(saw_running, "should emit checking… before completion");
        assert!(saw_diags, "should deliver diagnostics");
        // Allow a brief moment; must still be exactly one start.
        thread::sleep(Duration::from_millis(50));
        // Poll a few more times without refresh — no second start.
        for _ in 0..20 {
            let _ = src.try_recv();
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            calls.load(AtomOrd::SeqCst),
            1,
            "startup must run exactly one check"
        );
        assert_eq!(src.checks_started, 1);
    }

    #[test]
    fn edits_during_check_coalesce_to_one_followup() {
        let phase = Arc::new(Mutex::new(0u32));
        let phase2 = Arc::clone(&phase);
        let checker: Arc<dyn Fn(&CheckOptions) -> Result<CheckReport, CliError> + Send + Sync> =
            Arc::new(move |opts| {
                let mut p = phase2.lock().unwrap();
                *p += 1;
                let n = *p;
                drop(p);
                if n == 1 {
                    thread::sleep(Duration::from_millis(80));
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
                run_check(opts)
            });

        let mut src = WatchDataSource::with_checker(showcase_options(), checker).unwrap();

        // Start first check
        let mut got_first_running = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !got_first_running {
            if matches!(
                src.try_recv(),
                Some(SourceEvent::Status(RunStatus::Running))
            ) {
                got_first_running = true;
            } else {
                thread::sleep(Duration::from_millis(2));
            }
        }
        assert!(got_first_running);

        // Spam refreshes while first check runs.
        for _ in 0..20 {
            src.request_refresh();
            let _ = src.try_recv();
            thread::sleep(Duration::from_millis(2));
        }

        // Wait for both results.
        let mut diags = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && diags < 2 {
            match src.try_recv() {
                Some(SourceEvent::Diagnostics(_)) => diags += 1,
                Some(_) => {}
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
        // Idle a bit to ensure no third.
        for _ in 0..30 {
            let _ = src.try_recv();
            thread::sleep(Duration::from_millis(5));
        }
        let started = src.checks_started;
        assert!(
            started == 2,
            "expected one startup + one coalesced follow-up, got {started}"
        );
        assert_eq!(*phase.lock().unwrap(), 2);
    }
}
