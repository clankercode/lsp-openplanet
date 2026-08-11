//! Canned data source for demos and tests.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{DiagItem, RunStatus, Severity, Snapshot, SourceEvent, TuiDataSource};

/// Queue-backed mock. Can seed a snapshot delivered on the first poll.
#[derive(Debug, Default)]
pub struct MockSource {
    queue: VecDeque<SourceEvent>,
    /// Kept for refresh re-push (not consumed by try_recv).
    canned: Option<Snapshot>,
    seeded: bool,
    refresh_count: u32,
}

impl MockSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue events drained by successive [`TuiDataSource::try_recv`] calls.
    pub fn push(&mut self, event: SourceEvent) {
        self.queue.push_back(event);
    }

    /// Seed a snapshot delivered on the first poll (and kept for refresh).
    pub fn with_snapshot(snapshot: Snapshot) -> Self {
        Self {
            queue: VecDeque::new(),
            canned: Some(snapshot),
            seeded: false,
            refresh_count: 0,
        }
    }

    /// Preload the standard canned diagnostics used by tests and demos.
    pub fn with_canned_diags() -> Self {
        Self::with_snapshot(canned_snapshot())
    }

    pub fn refresh_count(&self) -> u32 {
        self.refresh_count
    }
}

impl TuiDataSource for MockSource {
    fn try_recv(&mut self) -> Option<SourceEvent> {
        if !self.seeded {
            self.seeded = true;
            if let Some(snap) = self.canned.clone() {
                return Some(SourceEvent::Diagnostics(snap));
            }
        }
        self.queue.pop_front()
    }

    fn request_refresh(&mut self) {
        self.refresh_count = self.refresh_count.saturating_add(1);
        self.queue
            .push_back(SourceEvent::Status(RunStatus::Running));
        let snap = self.canned.clone().unwrap_or_else(canned_snapshot);
        self.queue.push_back(SourceEvent::Diagnostics(snap));
    }
}

/// Sample snapshot matching the research write-up UI sketch.
pub fn canned_snapshot() -> Snapshot {
    Snapshot {
        root_label: "./MyPlugin".into(),
        diagnostics: vec![
            DiagItem {
                severity: Severity::Error,
                path: PathBuf::from("src/Main.as"),
                line: 42,
                col: 5,
                message: "Undefined member 'Foo'".into(),
            },
            DiagItem {
                severity: Severity::Warning,
                path: PathBuf::from("src/Main.as"),
                line: 10,
                col: 1,
                message: "Use 'const string &in x' to pass a string by reference".into(),
            },
            DiagItem {
                severity: Severity::Error,
                path: PathBuf::from("src/Helper.as"),
                line: 7,
                col: 12,
                message: "No matching signatures to 'Bar(int)'".into(),
            },
        ],
        status: RunStatus::Ready {
            duration: Duration::from_millis(120),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_poll_yields_canned_then_none() {
        let mut source = MockSource::with_canned_diags();
        let first = source.try_recv();
        assert!(matches!(first, Some(SourceEvent::Diagnostics(_))));
        assert!(source.try_recv().is_none());
    }

    #[test]
    fn refresh_queues_status_and_snapshot() {
        let mut source = MockSource::with_canned_diags();
        let _ = source.try_recv();
        source.request_refresh();
        assert_eq!(source.refresh_count(), 1);
        assert!(matches!(
            source.try_recv(),
            Some(SourceEvent::Status(RunStatus::Running))
        ));
        assert!(matches!(
            source.try_recv(),
            Some(SourceEvent::Diagnostics(_))
        ));
    }
}
