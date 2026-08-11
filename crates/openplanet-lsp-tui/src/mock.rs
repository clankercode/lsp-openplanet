//! Canned data source for demos and tests.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{DiagItem, RunStatus, Severity, Snapshot, SourceEvent, TuiDataSource};

/// Simple queue-backed source. Optionally seeds a first snapshot on first poll.
pub struct MockDataSource {
    queue: VecDeque<SourceEvent>,
    seeded: bool,
    initial: Option<Snapshot>,
    last: Option<Snapshot>,
}

impl MockDataSource {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            seeded: false,
            initial: None,
            last: None,
        }
    }

    pub fn with_initial(snapshot: Snapshot) -> Self {
        Self {
            queue: VecDeque::new(),
            seeded: false,
            initial: Some(snapshot),
            last: None,
        }
    }

    pub fn push(&mut self, event: SourceEvent) {
        self.queue.push_back(event);
    }

    /// Demo snapshot used in snapshots / dogfood.
    pub fn demo_snapshot() -> Snapshot {
        Snapshot {
            root_label: "showcase-diags".into(),
            diagnostics: vec![
                DiagItem {
                    severity: Severity::Error,
                    path: PathBuf::from("src/Main.as"),
                    line: 14,
                    col: 5,
                    message: "unknown type `NoSuchEngineType`".into(),
                },
                DiagItem {
                    severity: Severity::Error,
                    path: PathBuf::from("src/Helpers.as"),
                    line: 31,
                    col: 19,
                    message: "type `string` has no member `NotARealMethod`".into(),
                },
                DiagItem {
                    severity: Severity::Warning,
                    path: PathBuf::from("src/Helpers.as"),
                    line: 4,
                    col: 24,
                    message: "Use 'const string &in msg' to pass a string by reference".into(),
                },
            ],
            status: RunStatus::Ready {
                duration: Duration::from_millis(42),
            },
        }
    }
}

impl Default for MockDataSource {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiDataSource for MockDataSource {
    fn try_recv(&mut self) -> Option<SourceEvent> {
        if !self.seeded {
            self.seeded = true;
            if let Some(snap) = self.initial.take() {
                self.last = Some(snap.clone());
                return Some(SourceEvent::Diagnostics(snap));
            }
        }
        self.queue.pop_front()
    }

    fn request_refresh(&mut self) {
        // Demo: re-push ready status so UI can show activity.
        self.queue
            .push_back(SourceEvent::Status(RunStatus::Running));
        let snap = self.last.clone().unwrap_or_else(Self::demo_snapshot);
        self.last = Some(snap.clone());
        self.queue.push_back(SourceEvent::Diagnostics(snap));
    }
}
