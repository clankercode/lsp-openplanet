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

    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

/// List density for the diagnostics pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListDensity {
    /// One row per diagnostic (default).
    #[default]
    Compact,
    /// Multi-line rows with path and message separated.
    Relaxed,
}

impl ListDensity {
    pub fn toggle(self) -> Self {
        match self {
            ListDensity::Compact => ListDensity::Relaxed,
            ListDensity::Relaxed => ListDensity::Compact,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ListDensity::Compact => "compact",
            ListDensity::Relaxed => "relaxed",
        }
    }

    /// Rows consumed per list item (including message line in relaxed).
    pub fn rows_per_item(self) -> usize {
        match self {
            ListDensity::Compact => 1,
            ListDensity::Relaxed => 2,
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
    /// 1-based for display (start column).
    pub col: u32,
    /// 1-based exclusive end column on the start line (for carets).
    /// When equal to `col`, a single `^` is shown.
    pub end_col: u32,
    pub message: String,
    /// Expanded source line (tabs → spaces) for pretty detail; `None` if unread.
    pub source_line: Option<String>,
}

impl DiagItem {
    /// Stable identity for selection across refreshes (path + range + message).
    pub fn identity_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.path.display(),
            self.line,
            self.col,
            self.end_col,
            self.severity.glyph(),
            self.message
        )
    }

    /// 0-based start column on the expanded source line.
    pub fn start_col0(&self) -> usize {
        self.col.saturating_sub(1) as usize
    }

    /// 0-based exclusive end column for carets (at least one cell).
    pub fn end_col0(&self) -> usize {
        let start = self.start_col0();
        let end = self.end_col.saturating_sub(1) as usize;
        if end <= start {
            start + 1
        } else {
            end
        }
    }
}

/// File-watch subsystem health (independent of check status).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WatchHealth {
    /// notify is active.
    #[default]
    Active,
    /// Watcher failed; manual `r` still works.
    ManualOnly { reason: String },
}

impl WatchHealth {
    pub fn label(&self) -> String {
        match self {
            WatchHealth::Active => String::new(),
            WatchHealth::ManualOnly { .. } => "watch off · r to refresh".into(),
        }
    }
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
            RunStatus::Ready { duration } => format!("checked in {} ms", duration.as_millis()),
            RunStatus::Failed { message } => {
                // Keep header short; full message lives in detail/banner via status.
                let short: String = message.chars().take(40).collect();
                if message.chars().count() > 40 {
                    format!("failed: {short}…")
                } else {
                    format!("failed: {short}")
                }
            }
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, RunStatus::Running)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, RunStatus::Failed { .. })
    }
}

/// Full diagnostics snapshot for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// e.g. workspace path basename or `./MyPlugin`.
    pub root_label: String,
    pub diagnostics: Vec<DiagItem>,
    pub status: RunStatus,
    /// True when list is last-good while a new check is running or after failure.
    pub stale: bool,
    pub watch_health: WatchHealth,
}

impl Snapshot {
    pub fn empty(root_label: impl Into<String>) -> Self {
        Self {
            root_label: root_label.into(),
            diagnostics: Vec::new(),
            status: RunStatus::Idle,
            stale: false,
            watch_health: WatchHealth::Active,
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

    pub fn info_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Info)
            .count()
    }

    pub fn hint_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Hint)
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
    /// Update watch-health badge without clearing diagnostics.
    WatchHealth(WatchHealth),
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
