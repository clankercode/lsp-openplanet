//! Headless TUI render snapshots via `TestBackend` + `insta`.

use openplanet_lsp::tui::{
    canned_snapshot, render_once, App, DiagItem, MockSource, RunStatus, Severity, Snapshot,
    TuiDataSource, WatchHealth,
};
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

// ---- B029: narrow terminals -------------------------------------------------

const NARROW_W: u16 = 44;

/// The B029 worst case: a deep path plus a long "no matching signatures"
/// message, on a terminal too narrow for either.
fn narrow_snapshot() -> Snapshot {
    Snapshot {
        root_label: "./MyPlugin".into(),
        diagnostics: vec![
            DiagItem {
                severity: Severity::Error,
                path: std::path::PathBuf::from("plugins/MyPlugin/src/ui/Overlay.as"),
                line: 123,
                col: 26,
                end_col: 30,
                message: "no matching signatures to 'MakeTint(bool)' - candidates are \
                          'vec4 MakeTint(int, int, int)', 'vec4 MakeTint(const string &in)' \
                          and 'vec4 MakeTint(const vec3 &in, float alpha)'"
                    .into(),
                source_line: Some("    vec4 tint = MakeTint(true);".into()),
            },
            DiagItem {
                severity: Severity::Warning,
                path: std::path::PathBuf::from("plugins/MyPlugin/src/util/Helpers.as"),
                line: 4,
                col: 24,
                end_col: 27,
                message: "Use 'const string &in msg' to pass a string by reference".into(),
                source_line: Some("void ShowStatus(string msg) {".into()),
            },
        ],
        status: RunStatus::Ready {
            duration: std::time::Duration::from_millis(120),
        },
        stale: false,
        watch_health: WatchHealth::Active,
    }
}

fn draw_narrow(app: &mut App) -> Terminal<TestBackend> {
    let backend = TestBackend::new(NARROW_W, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    terminal
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn snapshot_narrow_compact_44x24() {
    let mut app = App::from_snapshot(narrow_snapshot());
    let terminal = draw_narrow(&mut app);
    insta::assert_snapshot!("narrow_compact_44x24", terminal.backend());
}

#[test]
fn snapshot_narrow_relaxed_44x24() {
    let mut app = App::from_snapshot(narrow_snapshot());
    app.toggle_density();
    let terminal = draw_narrow(&mut app);
    insta::assert_snapshot!("narrow_relaxed_44x24", terminal.backend());
}

#[test]
fn snapshot_narrow_detail_scrolled_44x24() {
    let mut app = App::from_snapshot(narrow_snapshot());
    app.scroll_detail_down();
    app.scroll_detail_down();
    let terminal = draw_narrow(&mut app);
    insta::assert_snapshot!("narrow_detail_scrolled_44x24", terminal.backend());
}

/// B029: at 44 columns the detail box must actually show text that used to be
/// clipped away, and must offer a way to reach the rest.
#[test]
fn narrow_detail_box_reveals_wrapped_message() {
    let mut app = App::from_snapshot(narrow_snapshot());
    let top = rendered_text(&draw_narrow(&mut app)).join("\n");
    assert!(
        top.contains("candidates") || top.contains("candidat"),
        "wrapped continuation should be visible:\n{top}"
    );
    assert!(top.contains("J/K"), "scroll affordance missing:\n{top}");

    assert!(
        !top.contains("float alpha"),
        "precondition: the tail must start out below the fold:\n{top}"
    );

    // Scrolling reaches the tail of the message.
    for _ in 0..6 {
        app.scroll_detail_down();
        let _ = draw_narrow(&mut app);
    }
    let bottom = rendered_text(&draw_narrow(&mut app)).join("\n");
    assert!(
        bottom.contains("float alpha"),
        "tail of the message unreachable:\n{bottom}"
    );
}

/// The list row keeps a readable message even when the location is long.
#[test]
fn narrow_list_row_keeps_a_readable_message() {
    let mut app = App::from_snapshot(narrow_snapshot());
    let text = rendered_text(&draw_narrow(&mut app));
    let row = text
        .iter()
        .find(|r| r.contains('E') && r.contains(":123:26"))
        .unwrap_or_else(|| panic!("no compact error row in:\n{}", text.join("\n")));
    // Everything after the two-space gap that follows the location.
    let msg = row
        .split_once(":123:26")
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();
    assert!(
        msg.chars().count() >= 12,
        "message squeezed out by the location: {row:?}"
    );
    assert!(msg.starts_with("no matching"), "row={row:?}");
}

/// Very short / very narrow terminals must not panic or render nonsense.
#[test]
fn tiny_terminals_do_not_panic() {
    for (w, h) in [(1u16, 1u16), (4, 3), (10, 6), (20, 8), (44, 9), (200, 60)] {
        let mut app = App::from_snapshot(narrow_snapshot());
        for _ in 0..3 {
            app.scroll_detail_down();
        }
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| app.draw(frame))
            .unwrap_or_else(|e| panic!("draw failed at {w}x{h}: {e}"));
        app.toggle_density();
        terminal
            .draw(|frame| app.draw(frame))
            .unwrap_or_else(|e| panic!("relaxed draw failed at {w}x{h}: {e}"));
    }
}

/// The scroll affordance must survive terminals too narrow for the full title —
/// it is the only thing telling the reader there is more message below.
#[test]
fn very_narrow_detail_title_keeps_the_scroll_hint() {
    for w in [22u16, 24, 30, 36, 44] {
        let mut app = App::from_snapshot(narrow_snapshot());
        let backend = TestBackend::new(w, HEIGHT);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        let text = rendered_text(&terminal).join("\n");
        assert!(
            text.contains("J/K"),
            "no scroll affordance at width {w}:\n{text}"
        );
    }
}

/// A diagnostic with no source excerpt wraps into more rows for a given width,
/// so it is the case that can overflow the detail box on a truly tiny terminal.
/// The hint must still be there, in whatever form fits.
#[test]
fn scroll_hint_survives_the_narrowest_scrollable_box() {
    let mut snap = narrow_snapshot();
    snap.diagnostics[0].source_line = None;
    snap.diagnostics.truncate(1);

    for w in 5u16..=20 {
        let mut app = App::from_snapshot(snap.clone());
        let backend = TestBackend::new(w, HEIGHT);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        let rows = rendered_text(&terminal);
        // Nothing may spill past the terminal edge.
        for row in &rows {
            assert!(
                row.chars().count() <= w as usize,
                "row overflows {w}: {row:?}"
            );
        }
        let text = rows.join("\n");
        assert!(
            text.contains("J/K") || text.contains('↕'),
            "no overflow hint at width {w}:\n{text}"
        );
        // ...and it must never be a clipped one: half a key name teaches the
        // reader nothing, so the hint has to degrade to a shorter whole form.
        assert!(
            !text.contains("J/") || text.contains("J/K"),
            "key hint clipped mid-token at width {w}:\n{text}"
        );
    }
}
