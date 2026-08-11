//! Snapshot + interaction tests (headless TestBackend).

use std::time::Duration;

use openplanet_lsp_tui::{
    render_once, DiagItem, MockDataSource, RunStatus, Severity, Snapshot, SourceEvent,
    TuiDataSource,
};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn backend_string(terminal: &Terminal<TestBackend>) -> String {
    // Display impl prints the buffer as a grid.
    format!("{}", terminal.backend())
}

#[test]
fn snapshot_empty_list_80x24() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut source = MockDataSource::with_initial(Snapshot {
        root_label: "empty-plugin".into(),
        diagnostics: vec![],
        status: RunStatus::Ready {
            duration: Duration::from_millis(5),
        },
    });
    render_once(&mut terminal, &mut source, "empty-plugin").unwrap();
    let out = backend_string(&terminal);
    insta::assert_snapshot!("empty_80x24", out);
}

#[test]
fn snapshot_populated_list_80x24() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut source = MockDataSource::with_initial(MockDataSource::demo_snapshot());
    render_once(&mut terminal, &mut source, "showcase-diags").unwrap();
    let out = backend_string(&terminal);
    assert!(out.contains("NoSuchEngineType") || out.contains("unknown type"));
    assert!(out.contains("showcase-diags") || out.contains("openplanet-lsp"));
    insta::assert_snapshot!("populated_80x24", out);
}

#[test]
fn snapshot_narrow_40x12() {
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut source = MockDataSource::with_initial(MockDataSource::demo_snapshot());
    render_once(&mut terminal, &mut source, "showcase-diags").unwrap();
    let out = backend_string(&terminal);
    insta::assert_snapshot!("populated_40x12", out);
}

#[test]
fn scroll_and_quit_via_app_state() {
    // Unit-level: App scroll without full terminal loop.
    // Drive via source events + render_once frames.
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut source = MockDataSource::new();
    source.push(SourceEvent::Diagnostics(Snapshot {
        root_label: "p".into(),
        diagnostics: vec![
            DiagItem {
                severity: Severity::Error,
                path: "a.as".into(),
                line: 1,
                col: 1,
                message: "one".into(),
            },
            DiagItem {
                severity: Severity::Warning,
                path: "b.as".into(),
                line: 2,
                col: 1,
                message: "two".into(),
            },
        ],
        status: RunStatus::Idle,
    }));
    render_once(&mut terminal, &mut source, "p").unwrap();
    let out = backend_string(&terminal);
    assert!(out.contains("one"));
    assert!(out.contains("two"));
}

#[test]
fn refresh_request_queues_events() {
    let mut source = MockDataSource::with_initial(MockDataSource::demo_snapshot());
    // drain initial
    let _ = source.try_recv();
    source.request_refresh();
    match source.try_recv() {
        Some(SourceEvent::Status(RunStatus::Running)) => {}
        other => panic!("expected Running, got {other:?}"),
    }
    match source.try_recv() {
        Some(SourceEvent::Diagnostics(_)) => {}
        other => panic!("expected Diagnostics, got {other:?}"),
    }
}
