//! Immediate-mode app state + draw.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::types::{DiagItem, ListDensity, RunStatus, Severity, Snapshot};

/// Watch-list application state.
#[derive(Debug)]
pub struct App {
    pub snapshot: Snapshot,
    pub list_state: ListState,
    pub should_quit: bool,
    pub density: ListDensity,
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
            density: ListDensity::Compact,
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

    pub fn toggle_density(&mut self) {
        self.density = self.density.toggle();
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

        // List + detail: give detail more room when a pretty excerpt is shown.
        let detail_pct = if self.selected_diag().and_then(|d| d.source_line.as_ref()).is_some() {
            48
        } else {
            38
        };
        let list_pct = 100 - detail_pct;
        let mid = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(list_pct),
                Constraint::Percentage(detail_pct),
            ])
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
            " {}  ·  {n} diagnostics ({e} errors · {w} warnings)  ·  {}  ·  list: {} ",
            self.snapshot.root_label,
            self.snapshot.status.label(),
            self.density.label(),
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
                .map(|d| list_item_for(d, self.density))
                .collect()
        };

        let title = format!(" diagnostics · {} ", self.density.label());
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
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
                let title = format!(" detail · {} ", d.severity.label());
                (title, pretty_detail_lines(d))
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
        let density_hint = match self.density {
            ListDensity::Compact => "c relaxed",
            ListDensity::Relaxed => "c compact",
        };
        let hints = Paragraph::new(format!(
            " q quit  ·  j/k ↑↓  ·  PgUp/PgDn  ·  g/G  ·  r refresh  ·  {density_hint} "
        ))
        .block(Block::default().borders(Borders::ALL).title(" keys "));
        f.render_widget(hints, area);
    }
}

fn list_item_for(d: &DiagItem, density: ListDensity) -> ListItem<'static> {
    let glyph = d.severity.glyph();
    let path = short_path(&d.path);
    let style = severity_style(d.severity);
    match density {
        ListDensity::Compact => {
            let row = format!(" {glyph}  {path}:{}:{}  {}", d.line, d.col, d.message);
            ListItem::new(Line::from(Span::styled(row, style)))
        }
        ListDensity::Relaxed => {
            let head = Line::from(vec![
                Span::styled(
                    format!(" {glyph}  "),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{path}:{}:{}", d.line, d.col),
                    Style::default().fg(ratatui::style::Color::Cyan),
                ),
            ]);
            let msg = Line::from(Span::styled(
                format!("     {}", d.message),
                style,
            ));
            // Trailing blank line separates relaxed rows visually.
            ListItem::new(vec![head, msg, Line::from("")])
        }
    }
}

/// Pretty detail block matching CLI pretty check layout (ratatui spans).
fn pretty_detail_lines(d: &DiagItem) -> Vec<Line<'static>> {
    let path = d.path.display().to_string();
    let sev = d.severity.label();
    let sev_style = severity_style(d.severity).add_modifier(Modifier::BOLD);

    let mut lines = vec![Line::from(vec![
        Span::styled(path, Style::default().fg(ratatui::style::Color::Cyan)),
        Span::raw(format!(":{}:{}: ", d.line, d.col)),
        Span::styled(sev.to_string(), sev_style),
    ])];

    if let Some(src) = d.source_line.as_ref() {
        let gutter_w = d.line.to_string().len().max(2);
        let gutter = format!("{:>gutter_w$}", d.line);
        let blank = " ".repeat(gutter_w);
        let pipe = Style::default().add_modifier(Modifier::DIM);

        lines.push(Line::from(vec![
            Span::raw(format!("  {gutter} ")),
            Span::styled("│".to_string(), pipe),
            Span::raw(format!(" {src}")),
        ]));

        let start = d.start_col0().min(src.chars().count());
        let end = d.end_col0().max(start + 1);
        let end = end.min(src.chars().count().max(start + 1));
        let pad = " ".repeat(start);
        let carets = "^".repeat((end - start).max(1));

        lines.push(Line::from(vec![
            Span::raw(format!("  {blank} ")),
            Span::styled("│".to_string(), pipe),
            Span::raw(format!(" {pad}")),
            Span::styled(carets, severity_style(d.severity).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(d.message.clone(), severity_style(d.severity)),
        ]));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            d.message.clone(),
            severity_style(d.severity),
        )));
    }

    lines
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
    use super::super::mock::canned_snapshot;

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
    fn density_toggles() {
        let mut app = App::new("x");
        assert_eq!(app.density, ListDensity::Compact);
        app.toggle_density();
        assert_eq!(app.density, ListDensity::Relaxed);
        app.toggle_density();
        assert_eq!(app.density, ListDensity::Compact);
    }

    #[test]
    fn pretty_detail_includes_caret_when_source_present() {
        let d = &canned_snapshot().diagnostics[0];
        assert!(d.source_line.is_some());
        let lines = pretty_detail_lines(d);
        let joined: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains('│'), "{joined}");
        assert!(joined.contains('^'), "{joined}");
        assert!(joined.contains("MakeTint") || joined.contains("Foo") || joined.contains("true"), "{joined}");
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
