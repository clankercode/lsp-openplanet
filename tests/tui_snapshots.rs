//! Headless TUI render snapshots via `TestBackend` + `insta`.

use openplanet_lsp::tui::{canned_snapshot, render_once, App, MockSource, Snapshot, TuiDataSource};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

fn draw_app(app: &mut App) -> Terminal<TestBackend> {
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    terminal
}

#[test]
fn snapshot_empty_80x24() {
    let mut app = App::from_snapshot(Snapshot::empty("./MyPlugin"));
    let terminal = draw_app(&mut app);
    insta::assert_snapshot!("empty_80x24", terminal.backend());
}

#[test]
fn snapshot_populated_80x24() {
    let mut app = App::from_snapshot(canned_snapshot());
    let terminal = draw_app(&mut app);
    insta::assert_snapshot!("populated_80x24", terminal.backend());
}

#[test]
fn mock_source_pushes_canned_on_first_poll() {
    let mut source = MockSource::with_canned_diags();
    let first = source.try_recv();
    assert!(first.is_some(), "first poll should yield canned diags");
    assert!(source.try_recv().is_none(), "second poll should be empty");
}

#[test]
fn render_once_from_mock_matches_populated() {
    let mut source = MockSource::with_canned_diags();
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    render_once(&mut terminal, &mut source, "./MyPlugin").expect("render");
    insta::assert_snapshot!("render_once_mock_80x24", terminal.backend());
}

#[test]
fn snapshot_relaxed_80x24() {
    let mut app = App::from_snapshot(canned_snapshot());
    app.toggle_density();
    let terminal = draw_app(&mut app);
    insta::assert_snapshot!("relaxed_80x24", terminal.backend());
}

/// #19: the one-time first-run modal shown before the watch TUI on a
/// bare TTY with no explicit `default_mode` preference.
#[test]
fn snapshot_first_run_modal_80x24() {
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    openplanet_lsp::tui::first_run::render_once(&mut terminal).expect("render");
    insta::assert_snapshot!("first_run_modal_80x24", terminal.backend());
}
