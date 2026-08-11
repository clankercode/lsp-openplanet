//! Immediate-mode app state + draw.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::types::{DiagItem, RunStatus, Severity, Snapshot};

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

    pub fn page_down(&mut self, page: usize) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((i + page.max(1)).min(len - 1)));
    }

    pub fn page_up(&mut self, page: usize) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(i.saturating_sub(page.max(1))));
    }

    pub fn select_first(&mut self) {
        if self.snapshot.diagnostics.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    pub fn select_last(&mut self) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(len - 1));
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    pub fn selected_diag(&self) -> Option<&DiagItem> {
        let i = self.list_state.selected()?;
        self.snapshot.diagnostics.get(i)
    }

    /// Pure render of current state into a frame.
    pub fn draw(&mut self, f: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);

        // List (top ~60%) + detail (bottom ~40%) of the middle pane.
        let mid = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(chunks[1]);
        self.draw_list(f, mid[0]);
        self.draw_detail(f, mid[1]);

        self.draw_footer(f, chunks[2]);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let n = self.snapshot.diagnostics.len();
        let e = self.snapshot.error_count();
        let w = self.snapshot.warning_count();
        let title = " openplanet-lsp · watch ";
        let body = format!(
            " {}  ·  {n} diagnostics ({e} errors · {w} warnings)  ·  {} ",
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
                "  (no diagnostics — edit a .as file or press r)",
                Style::default().add_modifier(Modifier::DIM),
            )))]
        } else {
            self.snapshot
                .diagnostics
                .iter()
                .map(|d| {
                    let glyph = d.severity.glyph();
                    let path = short_path(&d.path);
                    let row = format!(" {glyph}  {path}:{}:{}  {}", d.line, d.col, d.message);
                    let style = severity_style(d.severity);
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

    fn draw_detail(&self, f: &mut Frame<'_>, area: Rect) {
        let (title, lines): (String, Vec<Line<'_>>) = match self.selected_diag() {
            None => (
                " detail ".to_string(),
                vec![Line::from(Span::styled(
                    "  Select a diagnostic to inspect.",
                    Style::default().add_modifier(Modifier::DIM),
                ))],
            ),
            Some(d) => {
                let sev = match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                    Severity::Hint => "hint",
                };
                let title = format!(" detail · {sev} ");
                let path = d.path.display();
                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", d.severity.glyph()),
                            severity_style(d.severity).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format!("{path}:{}:{}", d.line, d.col)),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(d.message.clone(), severity_style(d.severity))),
                ];
                (title, lines)
            }
        };

        let block = Block::default().borders(Borders::ALL).title(title);
        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hints =
            Paragraph::new(" q quit  ·  j/k ↑↓  ·  PgUp/PgDn  ·  g/G top/end  ·  r refresh ")
                .block(Block::default().borders(Borders::ALL).title(" keys "));
        f.render_widget(hints, area);
    }
}

fn severity_style(sev: Severity) -> Style {
    match sev {
        Severity::Error => Style::default().fg(ratatui::style::Color::Red),
        Severity::Warning => Style::default().fg(ratatui::style::Color::Yellow),
        Severity::Info => Style::default().fg(ratatui::style::Color::Cyan),
        Severity::Hint => Style::default().add_modifier(Modifier::DIM),
    }
}

/// Prefer a short relative-looking path for list rows.
fn short_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    // Keep last two components when long.
    let parts: Vec<&str> = s.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        s
    } else {
        format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
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

    #[test]
    fn page_and_ends() {
        let mut app = App::from_snapshot(canned_snapshot());
        app.page_down(10);
        assert_eq!(app.selected(), Some(2));
        app.select_first();
        assert_eq!(app.selected(), Some(0));
        app.select_last();
        assert_eq!(app.selected(), Some(2));
        app.page_up(10);
        assert_eq!(app.selected(), Some(0));
    }

    #[test]
    fn short_path_keeps_tail() {
        assert_eq!(
            short_path(std::path::Path::new("src/Main.as")),
            "src/Main.as"
        );
        assert_eq!(
            short_path(std::path::Path::new("/abs/plugin/src/Overlay.as")),
            "src/Overlay.as"
        );
    }
}
