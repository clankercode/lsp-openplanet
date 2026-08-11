//! `check --watch` adapter: file events → re-check → TUI events.
//!
//! Watching lives here (main crate), not in the TUI module.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};
use crate::tui::{DiagItem, RunStatus, Severity, Snapshot, SourceEvent, TuiDataSource};
use tower_lsp::lsp_types::DiagnosticSeverity;

use super::{run_check, CheckOptions, CheckReport, CliError};

const DEBOUNCE: Duration = Duration::from_millis(250);

/// Host-side data source for the watch TUI.
pub struct WatchDataSource {
    options: CheckOptions,
    root_label: String,
    pending: VecDeque<SourceEvent>,
    dirty: Arc<AtomicBool>,
    /// Keep the debouncer alive for the TUI lifetime.
    _debouncer: Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>,
    first_poll: bool,
    checking: bool,
}

impl WatchDataSource {
    pub fn new(options: CheckOptions) -> Result<Self, CliError> {
        let root = resolve_root_label(&options.path)?;
        let dirty = Arc::new(AtomicBool::new(true));
        let dirty_flag = Arc::clone(&dirty);

        let debouncer = match start_watcher(&root, Arc::clone(&dirty_flag)) {
            Ok(d) => Some(d),
            Err(err) => {
                // Still usable: manual `r` refresh + initial check.
                tracing::warn!("file watch unavailable ({err}); use `r` to recheck");
                None
            }
        };

        let label = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());

        Ok(Self {
            options,
            root_label: label,
            pending: VecDeque::new(),
            dirty,
            _debouncer: debouncer,
            first_poll: true,
            checking: false,
        })
    }

    fn run_and_queue(&mut self) {
        if self.checking {
            return;
        }
        self.checking = true;
        self.pending
            .push_back(SourceEvent::Status(RunStatus::Running));

        let started = Instant::now();
        let result = run_check(&self.options);
        let elapsed = started.elapsed();
        self.checking = false;

        match result {
            Ok(report) => {
                let snap = report_to_snapshot(&report, &self.root_label, elapsed);
                self.pending.push_back(SourceEvent::Diagnostics(snap));
            }
            Err(err) => {
                self.pending.push_back(SourceEvent::Status(RunStatus::Failed {
                    message: err.to_string(),
                }));
            }
        }
    }
}

impl TuiDataSource for WatchDataSource {
    fn try_recv(&mut self) -> Option<SourceEvent> {
        if self.first_poll || self.dirty.swap(false, Ordering::SeqCst) {
            self.first_poll = false;
            self.run_and_queue();
        }
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
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| {
            match res {
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
            let source_line = source_cache
                .entry(item.path.clone())
                .or_insert_with(|| std::fs::read_to_string(&item.path).ok())
                .as_ref()
                .and_then(|src| {
                    src.lines()
                        .nth(line_idx)
                        .map(|l| expand_tabs(l.trim_end_matches('\r'), 4))
                });

            let start_col = range.start.character + 1;
            let end_col = if range.end.line == range.start.line
                && range.end.character > range.start.character
            {
                range.end.character + 1
            } else {
                start_col
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
    }
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

    #[test]
    fn interesting_paths() {
        assert!(is_interesting_path(Path::new("src/Main.as")));
        assert!(is_interesting_path(Path::new("/p/info.toml")));
        assert!(!is_interesting_path(Path::new("src/foo.png")));
        assert!(!is_interesting_path(Path::new(".git/config")));
        assert!(!is_interesting_path(Path::new("target/debug/x")));
        assert!(!is_interesting_path(Path::new("src/Main.as~")));
    }
}
