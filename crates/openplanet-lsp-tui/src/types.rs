//! View-model types shared between the TUI and host adapters.

use std::path::PathBuf;
use std::time::Duration;

/// Diagnostic severity for list styling / glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn glyph(self) -> char {
        match self {
            Severity::Error => 'E',
            Severity::Warning => 'W',
            Severity::Info => 'I',
            Severity::Hint => 'H',
        }
    }
}

/// One row in the watch list (CLI view model — not LSP wire types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagItem {
    pub severity: Severity,
    /// Display-relative path preferred (avoid absolute host paths in snapshots).
    pub path: PathBuf,
    /// 1-based for display.
    pub line: u32,
    /// 1-based for display.
    pub col: u32,
    pub message: String,
}

/// Status of the last / current check run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Running,
    Ready { duration: Duration },
    Failed { message: String },
}

impl RunStatus {
    pub fn label(&self) -> String {
        match self {
            RunStatus::Idle => "idle".into(),
            RunStatus::Running => "checking…".into(),
            RunStatus::Ready { duration } => format!("last: {}ms", duration.as_millis()),
            RunStatus::Failed { message } => format!("failed: {message}"),
        }
    }
}

/// Full diagnostics snapshot for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// e.g. workspace path basename or `./MyPlugin`.
    pub root_label: String,
    pub diagnostics: Vec<DiagItem>,
    pub status: RunStatus,
}

impl Snapshot {
    pub fn empty(root_label: impl Into<String>) -> Self {
        Self {
            root_label: root_label.into(),
            diagnostics: Vec::new(),
            status: RunStatus::Idle,
        }
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }
}

/// Events the UI understands. Host maps notify + check results into these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    /// Replace the visible diagnostic set (full refresh is fine for v1).
    Diagnostics(Snapshot),
    /// Non-fatal status line update without clearing the list.
    Status(RunStatus),
    /// Source requests UI exit (e.g. parent cancelled).
    Shutdown,
}

/// Mock-first seam. Production adapter lives in openplanet-lsp.
pub trait TuiDataSource {
    /// Non-blocking poll. Return `None` when nothing new.
    fn try_recv(&mut self) -> Option<SourceEvent>;

    /// Hint that the user wants a manual recheck (key `r`) — v1 may no-op.
    fn request_refresh(&mut self) {}
}
