//! Event loop: poll source + keyboard, draw.

use std::fmt;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::Terminal;

use super::types::{SourceEvent, TuiDataSource};
use super::ui::App;

/// Options for the run loop.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// How often to poll the data source / redraw when idle.
    pub tick_rate: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            tick_rate: Duration::from_millis(100),
        }
    }
}

fn map_backend_err<E: fmt::Display>(err: E) -> io::Error {
    io::Error::other(err.to_string())
}

/// Run the watch TUI on the real terminal until quit.
pub fn run(source: impl TuiDataSource, opts: RunOptions) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run_with_backend(&mut terminal, source, opts);
    ratatui::restore();
    result
}

/// Run against any backend (real or [`ratatui::backend::TestBackend`]).
///
/// Blocks until quit/exit. For headless snapshot tests prefer
/// [`render_once`] so the loop does not poll crossterm.
pub fn run_with_backend<B, S>(
    terminal: &mut Terminal<B>,
    mut source: S,
    opts: RunOptions,
) -> io::Result<()>
where
    B: Backend,
    S: TuiDataSource,
{
    let mut app = App::new("plugin");
    // Drain any initial events before first draw (may only be Running).
    drain_source(&mut app, &mut source);

    loop {
        terminal.draw(|f| app.draw(f)).map_err(map_backend_err)?;

        if app.should_quit {
            break;
        }

        if event::poll(opts.tick_rate)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, &mut source, key);
                }
                Event::Resize(_, _) => {
                    // next draw picks up new size
                }
                _ => {}
            }
        }

        if drain_source(&mut app, &mut source) {
            break;
        }
    }

    Ok(())
}

/// Map a key event to an app action. Pure enough to unit-test.
pub fn handle_key(app: &mut App, source: &mut impl TuiDataSource, key: KeyEvent) {
    // Ctrl-C always quits (raw mode often delivers Char('c') + CONTROL).
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
        app.should_quit = true;
        return;
    }

    // Ignore accidental Ctrl/Alt combos for ordinary bindings.
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return;
    }

    let page = app.page_step();
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.scroll_down(),
        KeyCode::Char('k') | KeyCode::Up => app.scroll_up(),
        KeyCode::PageDown | KeyCode::Char(' ') => app.page_down(page),
        KeyCode::PageUp => app.page_up(page),
        KeyCode::Home => app.select_first(),
        KeyCode::End => app.select_last(),
        // g = top; Shift+G = end (crossterm reports 'G' with SHIFT)
        KeyCode::Char('g') if !key.modifiers.contains(KeyModifiers::SHIFT) => app.select_first(),
        KeyCode::Char('G') | KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.select_last()
        }
        KeyCode::Char('G') => app.select_last(),
        KeyCode::Char('c') => app.toggle_density(),
        KeyCode::Char('r') => source.request_refresh(),
        _ => {}
    }
}

/// Apply all pending source events. Returns true if exit was requested.
fn drain_source(app: &mut App, source: &mut impl TuiDataSource) -> bool {
    let mut done = false;
    while let Some(ev) = source.try_recv() {
        match ev {
            SourceEvent::Diagnostics(snap) => app.apply_snapshot(snap),
            SourceEvent::Status(st) => app.apply_status(st),
            SourceEvent::WatchHealth(h) => app.apply_watch_health(h),
            SourceEvent::Shutdown => {
                done = true;
                app.should_quit = true;
            }
        }
    }
    done
}

/// Draw one frame from a mock/source without entering the interactive loop.
/// Used by snapshot tests.
pub fn render_once<B: Backend>(
    terminal: &mut Terminal<B>,
    source: &mut impl TuiDataSource,
    root_label: &str,
) -> io::Result<()> {
    let mut app = App::new(root_label);
    drain_source(&mut app, source);
    terminal.draw(|f| app.draw(f)).map_err(map_backend_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::mock::canned_snapshot;
    use crate::tui::types::ListDensity;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    struct NopSource;
    impl TuiDataSource for NopSource {
        fn try_recv(&mut self) -> Option<SourceEvent> {
            None
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn press_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App::from_snapshot(canned_snapshot());
        let mut src = NopSource;
        handle_key(&mut app, &mut src, press_mod(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_c_toggles_density_not_quit() {
        let mut app = App::from_snapshot(canned_snapshot());
        let mut src = NopSource;
        assert_eq!(app.density, ListDensity::Compact);
        handle_key(&mut app, &mut src, press(KeyCode::Char('c')));
        assert!(!app.should_quit);
        assert_eq!(app.density, ListDensity::Relaxed);
    }

    #[test]
    fn ctrl_r_does_not_refresh_or_quit() {
        let mut app = App::from_snapshot(canned_snapshot());
        let mut src = NopSource;
        handle_key(&mut app, &mut src, press_mod(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(!app.should_quit);
    }

    #[test]
    fn q_quits() {
        let mut app = App::new("x");
        let mut src = NopSource;
        handle_key(&mut app, &mut src, press(KeyCode::Char('q')));
        assert!(app.should_quit);
    }
}
