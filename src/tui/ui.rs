//! Immediate-mode app state + draw.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
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
                Constraint::Min(6),
                Constraint::Length(1), // dim hint line
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);

        // Detail height: content-driven, min 6 inner lines, +1 slack, +2 borders.
        // Caps so the list keeps the bulk of the screen (no huge empty detail).
        let detail_h = detail_box_height(self.selected_diag());
        let mid = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(detail_h),
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
            " {}  ·  {}  ·  {} ",
            self.snapshot.root_label,
            diag_counts_phrase(n, e, w),
            self.snapshot.status.label(),
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
            vec![
                ListItem::new(Line::from(Span::styled(
                    "  ✓  No diagnostics",
                    Style::default().fg(ratatui::style::Color::Green),
                ))),
                ListItem::new(Line::from(Span::styled(
                    "     Edit a .as file or press r to recheck.",
                    Style::default().add_modifier(Modifier::DIM),
                ))),
            ]
        } else {
            let loc_w = self
                .snapshot
                .diagnostics
                .iter()
                .map(|d| bare_location(d).chars().count())
                .max()
                .unwrap_or(16)
                .min(40);
            self.snapshot
                .diagnostics
                .iter()
                .map(|d| list_item_for(d, self.density, loc_w))
                .collect()
        };

        let title = format!(" diagnostics · {} ", self.density.label());
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(ratatui::style::Color::Rgb(55, 58, 78))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_detail(&self, f: &mut Frame<'_>, area: Rect) {
        let (title, lines): (String, Vec<Line<'_>>) = match self.selected_diag() {
            None => (
                " detail ".to_string(),
                vec![
                    Line::from(Span::styled(
                        "  Nothing selected.",
                        Style::default().add_modifier(Modifier::DIM),
                    )),
                    Line::from(Span::styled(
                        "  j/k moves the list · Enter is not required.",
                        Style::default().add_modifier(Modifier::DIM),
                    )),
                ],
            ),
            Some(d) => (
                format!(" detail · {} ", d.severity.label()),
                pretty_detail_lines(d),
            ),
        };

        let block = Block::default().borders(Borders::ALL).title(title);
        // No wrap: source + caret lines must stay column-aligned.
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let density_hint = match self.density {
            ListDensity::Compact => "c density",
            ListDensity::Relaxed => "c density",
        };
        // Single muted line — no heavy box title competing with content.
        let hints = Paragraph::new(format!(
            " j/k move · PgUp/Dn page · g/G top/end · r refresh · {density_hint} · q quit "
        ))
        .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(hints, area);
    }
}

fn diag_counts_phrase(n: usize, e: usize, w: usize) -> String {
    let d = plural(n, "diagnostic", "diagnostics");
    let mut parts = Vec::new();
    if e > 0 {
        parts.push(plural(e, "error", "errors"));
    }
    if w > 0 {
        parts.push(plural(w, "warning", "warnings"));
    }
    if parts.is_empty() {
        format!("{d}")
    } else {
        format!("{d} ({})", parts.join(" · "))
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

fn bare_location(d: &DiagItem) -> String {
    format!("{}:{}:{}", short_path(&d.path), d.line, d.col)
}

fn format_location(d: &DiagItem, loc_w: usize) -> String {
    let bare = bare_location(d);
    let truncated = if bare.chars().count() > loc_w {
        // Keep the tail (file:line:col) when truncating long paths.
        let tail: String = bare.chars().rev().take(loc_w.saturating_sub(1)).collect::<String>().chars().rev().collect();
        format!("…{tail}")
    } else {
        bare
    };
    format!("{:<loc_w$}", truncated)
}

/// Total terminal rows for the detail box (borders included).
/// Inner content height = max(6, content_lines + 1), then +2 for borders.
fn detail_box_height(selected: Option<&DiagItem>) -> u16 {
    let content = match selected {
        None => 2usize, // empty-state lines
        Some(d) => pretty_detail_lines(d).len(),
    };
    let inner = content.saturating_add(1).max(6);
    // Hard cap: never more than 8 content rows (+2 borders = 10).
    let inner = inner.min(8);
    (inner + 2) as u16
}

fn list_item_for(d: &DiagItem, density: ListDensity, loc_w: usize) -> ListItem<'static> {
    let glyph = d.severity.glyph();
    let loc = format_location(d, loc_w);
    let style = severity_style(d.severity);
    match density {
        ListDensity::Compact => {
            let row = Line::from(vec![
                Span::styled(format!(" {glyph} "), style.add_modifier(Modifier::BOLD)),
                Span::styled(loc, Style::default().fg(ratatui::style::Color::Cyan)),
                Span::raw("  "),
                Span::styled(d.message.clone(), style),
            ]);
            ListItem::new(row)
        }
        ListDensity::Relaxed => {
            // Row 1: severity + location … optional `> fragment <` on the RHS.
            let mut head_spans = vec![
                Span::styled(format!(" {glyph} "), style.add_modifier(Modifier::BOLD)),
                Span::styled(
                    loc.trim_end().to_string(),
                    Style::default().fg(ratatui::style::Color::Cyan),
                ),
            ];
            if let Some(frag) = code_fragment(d) {
                head_spans.push(Span::raw("   "));
                head_spans.push(Span::styled(
                    "› ".to_string(),
                    Style::default().add_modifier(Modifier::DIM),
                ));
                head_spans.push(Span::styled(
                    frag,
                    style.add_modifier(Modifier::BOLD),
                ));
                head_spans.push(Span::styled(
                    " ‹".to_string(),
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            let head = Line::from(head_spans);
            let msg = Line::from(Span::styled(format!("   {}", d.message), style));
            ListItem::new(vec![head, msg])
        }
    }
}

/// Problematic span from the source line, for relaxed-mode list chrome.
/// Prefers the exact caret span; widens slightly for single-char / empty spans.
fn code_fragment(d: &DiagItem) -> Option<String> {
    let src = d.source_line.as_ref()?;
    let chars: Vec<char> = src.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut start = d.start_col0().min(chars.len().saturating_sub(1));
    let mut end = d.end_col0().max(start + 1).min(chars.len());
    // If the span is tiny, try a modest widen so the fragment is readable
    // (e.g. a single `(` is less useful than a short token around it).
    if end - start < 3 && chars.len() > 3 {
        let pad = 2usize;
        start = start.saturating_sub(pad);
        end = (end + pad).min(chars.len());
    }
    let frag: String = chars[start..end].iter().collect();
    let frag = frag.trim();
    if frag.is_empty() {
        return None;
    }
    // Cap display width so long lines don't dominate the row.
    const MAX: usize = 28;
    let frag = if frag.chars().count() > MAX {
        let take: String = frag.chars().take(MAX.saturating_sub(1)).collect();
        format!("{take}…")
    } else {
        frag.to_string()
    };
    Some(frag)
}

fn pretty_detail_lines(d: &DiagItem) -> Vec<Line<'static>> {
    let path = d.path.display().to_string();
    let sev = d.severity.label();
    let sev_style = severity_style(d.severity).add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(path, Style::default().fg(ratatui::style::Color::Cyan)),
            Span::raw(format!(":{}:{}: ", d.line, d.col)),
            Span::styled(sev.to_string(), sev_style),
        ]),
        Line::from(""),
    ];

    if let Some(src) = d.source_line.as_ref() {
        let gutter_w = d.line.to_string().len().max(2);
        let gutter = format!("{:>gutter_w$}", d.line);
        let blank = " ".repeat(gutter_w);
        let pipe = Style::default().add_modifier(Modifier::DIM);

        let start = d.start_col0().min(src.chars().count());
        let end = d.end_col0().max(start + 1);
        let end = end.min(src.chars().count().max(start + 1));
        let pad = " ".repeat(start);
        let carets = "^".repeat((end - start).max(1));

        // Source line with the underlined span emphasized.
        let chars: Vec<char> = src.chars().collect();
        let before: String = chars[..start.min(chars.len())].iter().collect();
        let mid: String = if start < chars.len() {
            chars[start..end.min(chars.len())].iter().collect()
        } else {
            String::new()
        };
        let after: String = if end < chars.len() {
            chars[end..].iter().collect()
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::raw(format!("  {gutter} ")),
            Span::styled("│".to_string(), pipe),
            Span::raw(" "),
            Span::raw(before),
            Span::styled(mid, severity_style(d.severity).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Span::raw(after),
        ]));

        // Caret-only line (message on next line — CLI pretty puts message inline,
        // but TUI detail has room and vision review preferred separation).
        lines.push(Line::from(vec![
            Span::raw(format!("  {blank} ")),
            Span::styled("│".to_string(), pipe),
            Span::raw(format!(" {pad}")),
            Span::styled(
                carets,
                severity_style(d.severity).add_modifier(Modifier::BOLD),
            ),
        ]));

        lines.push(Line::from(vec![
            Span::raw(format!("  {blank} ")),
            Span::styled("│".to_string(), pipe),
            Span::raw(" "),
            Span::styled(d.message.clone(), severity_style(d.severity)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  {}", d.message),
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
    use super::super::mock::canned_snapshot;
    use super::*;

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
    fn pretty_detail_separates_carets_and_message() {
        let d = &canned_snapshot().diagnostics[0];
        let lines = pretty_detail_lines(d);
        let rows: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let joined = rows.join("\n");
        assert!(joined.contains('│'), "{joined}");
        assert!(joined.contains('^'), "{joined}");
        // Caret line should not also contain the full message.
        let caret_row = rows.iter().find(|r| r.contains('^')).expect("caret row");
        assert!(
            !caret_row.contains("expected"),
            "caret row should not include message: {caret_row}"
        );
        assert!(joined.contains("expected"), "{joined}");
    }

    #[test]
    fn plural_grammar() {
        assert_eq!(plural(1, "warning", "warnings"), "1 warning");
        assert_eq!(plural(2, "warning", "warnings"), "2 warnings");
        assert!(diag_counts_phrase(3, 2, 1).contains("1 warning"));
        assert!(!diag_counts_phrase(3, 2, 1).contains("1 warnings"));
    }

    #[test]
    fn code_fragment_extracts_span() {
        let d = &canned_snapshot().diagnostics[0];
        let frag = code_fragment(d).expect("frag");
        assert!(frag.contains("true") || frag.contains("MakeTint"), "{frag}");
    }

    #[test]
    fn detail_box_height_min_six_inner() {
        // borders + max(6, content+1) — at least 8 total rows
        let h = detail_box_height(Some(&canned_snapshot().diagnostics[0]));
        assert!(h >= 8, "h={h}");
        assert!(h <= 10, "h={h}");
        let empty = detail_box_height(None);
        assert!(empty >= 8, "empty={empty}");
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
