//! Event loop: poll source + keyboard, draw.

use std::fmt;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::Backend;
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
    // Drain any initial events before first draw.
    drain_source(&mut app, &mut source);

    loop {
        terminal.draw(|f| app.draw(f)).map_err(map_backend_err)?;

        if app.should_quit {
            break;
        }

        if event::poll(opts.tick_rate)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                    KeyCode::Char('j') | KeyCode::Down => app.scroll_down(),
                    KeyCode::Char('k') | KeyCode::Up => app.scroll_up(),
                    KeyCode::Char('r') => source.request_refresh(),
                    _ => {}
                },
                _ => {}
            }
        }

        if drain_source(&mut app, &mut source) {
            break;
        }
    }

    Ok(())
}

/// Apply all pending source events. Returns true if exit was requested.
fn drain_source(app: &mut App, source: &mut impl TuiDataSource) -> bool {
    let mut done = false;
    while let Some(ev) = source.try_recv() {
        match ev {
            SourceEvent::Diagnostics(snap) => app.apply_snapshot(snap),
            SourceEvent::Status(st) => app.apply_status(st),
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
