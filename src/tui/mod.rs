//! Watch-mode diagnostics TUI for openplanet-lsp.
//!
//! Mock-first: production adapters live in the main binary and implement
//! [`TuiDataSource`]. This module owns presentation + the event loop only.
//!
//! File watching and typechecking are out of scope here.

mod app;
mod mock;
mod types;
mod ui;

pub use app::{handle_key, render_once, run, run_with_backend, RunOptions};
pub use mock::{canned_snapshot, MockSource};
pub use types::{
    DiagItem, ListDensity, RunStatus, Severity, Snapshot, SourceEvent, TuiDataSource, WatchHealth,
};
pub use ui::App;
