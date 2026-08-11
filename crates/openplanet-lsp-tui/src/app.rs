//! Event loop: poll source + keyboard, draw.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;

use crate::types::{SourceEvent, TuiDataSource};
use crate::ui::App;

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

/// Run the watch TUI on the real terminal until quit.
pub fn run(source: impl TuiDataSource, opts: RunOptions) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_with_backend(&mut terminal, source, opts, true);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// Run against any backend (real or [`ratatui::backend::TestBackend`]).
///
/// When `handle_input` is false, only drains the data source once per tick
/// (for snapshot tests that inject state via the mock source).
pub fn run_with_backend<B, S>(
    terminal: &mut Terminal<B>,
    mut source: S,
    opts: RunOptions,
    handle_input: bool,
) -> io::Result<()>
where
    B: Backend,
    S: TuiDataSource,
{
    let mut app = App::new("plugin");
    // Drain any initial events before first draw.
    drain_source(&mut app, &mut source);

    loop {
        terminal.draw(|f| app.draw(f))?;

        if app.should_quit {
            break;
        }

        if handle_input {
            if event::poll(opts.tick_rate)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                            KeyCode::Char('j') | KeyCode::Down => app.scroll_down(),
                            KeyCode::Char('k') | KeyCode::Up => app.scroll_up(),
                            KeyCode::Char('r') => source.request_refresh(),
                            _ => {}
                        }
                    }
                }
            }
        } else {
            // Test / headless: single drain then exit after one frame cycle
            // unless more events appear. Callers typically draw once via
            // `render_once` helpers instead — keep a short sleep for e2e loops.
            std::thread::sleep(opts.tick_rate);
        }

        if drain_source(&mut app, &mut source) {
            // Shutdown requested
            break;
        }
    }

    Ok(())
}

/// Apply all pending source events. Returns true if shutdown was requested.
fn drain_source(app: &mut App, source: &mut impl TuiDataSource) -> bool {
    let mut shutdown = false;
    while let Some(ev) = source.try_recv() {
        match ev {
            SourceEvent::Diagnostics(snap) => app.apply_snapshot(snap),
            SourceEvent::Status(st) => app.apply_status(st),
            SourceEvent::Shutdown => {
                shutdown = true;
                app.should_quit = true;
            }
        }
    }
    shutdown
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
    terminal.draw(|f| app.draw(f))?;
    Ok(())
}
