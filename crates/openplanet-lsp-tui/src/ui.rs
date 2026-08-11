//! Immediate-mode app state + draw.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::types::{RunStatus, Severity, Snapshot};

/// Watch-list application state.
#[derive(Debug)]
pub struct App {
    pub snapshot: Snapshot,
    pub list_state: ListState,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new("(no workspace)")
    }
}

impl App {
    pub fn new(root_label: impl Into<String>) -> Self {
        Self {
            snapshot: Snapshot::empty(root_label),
            list_state: ListState::default(),
            should_quit: false,
        }
    }

    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let mut app = Self::new(snapshot.root_label.clone());
        app.apply_snapshot(snapshot);
        app
    }

    pub fn apply_snapshot(&mut self, snap: Snapshot) {
        let len = snap.diagnostics.len();
        self.snapshot = snap;
        if len == 0 {
            self.list_state.select(None);
        } else {
            let idx = self.list_state.selected().unwrap_or(0).min(len - 1);
            self.list_state.select(Some(idx));
        }
    }

    pub fn apply_status(&mut self, status: RunStatus) {
        self.snapshot.status = status;
    }

    pub fn scroll_down(&mut self) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((i + 1).min(len - 1)));
    }

    pub fn scroll_up(&mut self) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(i.saturating_sub(1)));
    }

    pub fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    /// Pure render of current state into a frame.
    pub fn draw(&mut self, f: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(3),
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);
        self.draw_list(f, chunks[1]);
        self.draw_footer(f, chunks[2]);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let n = self.snapshot.diagnostics.len();
        let e = self.snapshot.error_count();
        let w = self.snapshot.warning_count();
        let title = " openplanet-lsp check --watch ";
        let body = format!(
            " watching: {}  ·  {n} diags ({e}E/{w}W)  ·  {} ",
            self.snapshot.root_label,
            self.snapshot.status.label()
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )));
        f.render_widget(Paragraph::new(body).block(block), area);
    }

    fn draw_list(&mut self, f: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem<'_>> = if self.snapshot.diagnostics.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "  (no diagnostics)",
                Style::default().add_modifier(Modifier::DIM),
            )))]
        } else {
            self.snapshot
                .diagnostics
                .iter()
                .map(|d| {
                    let glyph = d.severity.glyph();
                    let path = d.path.display();
                    let row = format!(" {glyph}  {path}:{}:{}  {}", d.line, d.col, d.message);
                    let style = match d.severity {
                        Severity::Error => Style::default().fg(ratatui::style::Color::Red),
                        Severity::Warning => Style::default().fg(ratatui::style::Color::Yellow),
                        Severity::Info => Style::default().fg(ratatui::style::Color::Cyan),
                        Severity::Hint => Style::default().add_modifier(Modifier::DIM),
                    };
                    ListItem::new(Line::from(Span::styled(row, style)))
                })
                .collect()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" diagnostics "),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ");

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hints = Paragraph::new(" q quit  ·  j/k scroll  ·  r refresh ")
            .block(Block::default().borders(Borders::ALL).title(" keys "));
        f.render_widget(hints, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::canned_snapshot;

    #[test]
    fn scroll_clamps() {
        let mut app = App::new("x");
        app.scroll_down();
        assert_eq!(app.selected(), None);

        app.apply_snapshot(canned_snapshot());
        assert_eq!(app.selected(), Some(0));
        app.scroll_down();
        assert_eq!(app.selected(), Some(1));
        app.scroll_down();
        app.scroll_down();
        assert_eq!(app.selected(), Some(2));
        app.scroll_up();
        assert_eq!(app.selected(), Some(1));
    }
}
