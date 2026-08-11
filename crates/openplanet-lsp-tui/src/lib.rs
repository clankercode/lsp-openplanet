//! Watch-mode diagnostics TUI for openplanet-lsp.
//!
//! Mock-first: production adapters live in the main binary and implement
//! [`TuiDataSource`]. This crate owns presentation + the event loop only.
//!
//! File watching and typechecking are out of scope here.

mod app;
mod mock;
mod types;
mod ui;

pub use app::{render_once, run, run_with_backend, RunOptions};
pub use mock::{canned_snapshot, MockSource};
pub use types::{DiagItem, RunStatus, Severity, Snapshot, SourceEvent, TuiDataSource};
pub use ui::App;
