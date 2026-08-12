//! Immediate-mode app state + draw.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::types::{DiagItem, ListDensity, RunStatus, Severity, Snapshot, WatchHealth};

/// Watch-list application state.
#[derive(Debug)]
pub struct App {
    pub snapshot: Snapshot,
    pub list_state: ListState,
    pub should_quit: bool,
    pub density: ListDensity,
    /// Last selected diagnostic identity (for stable reselect on refresh).
    selected_key: Option<String>,
    /// Inner list height from last draw (for viewport-aware paging).
    list_inner_h: u16,
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
            selected_key: None,
            list_inner_h: 10,
        }
    }

    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        let mut app = Self::new(snapshot.root_label.clone());
        app.apply_snapshot(snapshot);
        app
    }

    pub fn apply_snapshot(&mut self, snap: Snapshot) {
        // Prefer previous identity; fall back to current selection index.
        let prev_key = self
            .selected_key
            .clone()
            .or_else(|| self.selected_diag().map(|d| d.identity_key()));
        let prev_idx = self.list_state.selected();

        let len = snap.diagnostics.len();
        self.snapshot = snap;
        if len == 0 {
            self.list_state.select(None);
            self.selected_key = None;
            return;
        }

        let idx = if let Some(key) = prev_key.as_ref() {
            snap_find_key(&self.snapshot, key)
                .or_else(|| snap_nearest(&self.snapshot, key, prev_idx))
                .unwrap_or_else(|| prev_idx.unwrap_or(0).min(len - 1))
        } else {
            prev_idx.unwrap_or(0).min(len - 1)
        };
        self.list_state.select(Some(idx));
        self.selected_key = self.snapshot.diagnostics.get(idx).map(|d| d.identity_key());
    }

    pub fn apply_status(&mut self, status: RunStatus) {
        // Running/failed over last-good list → mark stale; ready clears stale.
        match &status {
            RunStatus::Running | RunStatus::Failed { .. } => {
                if !self.snapshot.diagnostics.is_empty() {
                    self.snapshot.stale = true;
                }
            }
            RunStatus::Ready { .. } => {
                self.snapshot.stale = false;
            }
            RunStatus::Idle => {}
        }
        self.snapshot.status = status;
    }

    pub fn apply_watch_health(&mut self, health: WatchHealth) {
        self.snapshot.watch_health = health;
    }

    /// Page step from last-drawn list height and density.
    pub fn page_step(&self) -> usize {
        let inner = self.list_inner_h.saturating_sub(2) as usize; // borders
        let per = self.density.rows_per_item().max(1);
        let visible = (inner / per).max(1);
        visible.saturating_sub(1).max(1)
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
        self.select_index((i + 1).min(len - 1));
    }

    pub fn scroll_up(&mut self) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.select_index(i.saturating_sub(1));
    }

    pub fn page_down(&mut self, page: usize) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.select_index((i + page.max(1)).min(len - 1));
    }

    pub fn page_up(&mut self, page: usize) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.select_index(i.saturating_sub(page.max(1)));
    }

    pub fn select_first(&mut self) {
        if self.snapshot.diagnostics.is_empty() {
            self.list_state.select(None);
            self.selected_key = None;
        } else {
            self.select_index(0);
        }
    }

    pub fn select_last(&mut self) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            self.list_state.select(None);
            self.selected_key = None;
        } else {
            self.select_index(len - 1);
        }
    }

    fn select_index(&mut self, idx: usize) {
        self.list_state.select(Some(idx));
        self.selected_key = self.snapshot.diagnostics.get(idx).map(|d| d.identity_key());
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
        self.list_inner_h = mid[0].height;
        self.draw_list(f, mid[0]);
        self.draw_detail(f, mid[1]);

        self.draw_footer(f, chunks[2]);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let n = self.snapshot.diagnostics.len();
        let e = self.snapshot.error_count();
        let w = self.snapshot.warning_count();
        let i = self.snapshot.info_count();
        let h = self.snapshot.hint_count();
        let title = " openplanet-lsp · watch ";
        let mut parts = vec![
            self.snapshot.root_label.clone(),
            diag_counts_phrase(n, e, w, i, h),
            self.snapshot.status.label(),
        ];
        if self.snapshot.stale {
            parts.push("stale".into());
        }
        let wh = self.snapshot.watch_health.label();
        if !wh.is_empty() {
            parts.push(wh);
        }
        let body = format!(" {} ", parts.join("  ·  "));
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
            // Inner list width (borders already drawn by Block; ▸ marker uses 1).
            let content_w = area.width.saturating_sub(2).saturating_sub(1) as usize;
            let loc_cap = (content_w * 40 / 100).clamp(12, 40);
            let loc_w = self
                .snapshot
                .diagnostics
                .iter()
                .map(|d| bare_location(d).chars().count())
                .max()
                .unwrap_or(16)
                .min(loc_cap);
            let lhs_max = self
                .snapshot
                .diagnostics
                .iter()
                .map(lhs_width)
                .max()
                .unwrap_or(12);
            let selected = self.list_state.selected();
            self.snapshot
                .diagnostics
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    list_item_for(
                        d,
                        self.density,
                        loc_w,
                        content_w,
                        lhs_max,
                        selected == Some(i),
                    )
                })
                .collect()
        };

        let sel = self
            .list_state
            .selected()
            .map(|i| i + 1)
            .unwrap_or(0);
        let n = self.snapshot.diagnostics.len();
        let stale = if self.snapshot.stale { " · stale" } else { "" };
        let title = if n == 0 {
            format!(" diagnostics · {}{stale} ", self.density.label())
        } else {
            format!(" diagnostics · {} · {sel}/{n}{stale} ", self.density.label())
        };
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(ratatui::style::Color::Rgb(55, 58, 78))
                    .add_modifier(Modifier::BOLD),
            )
            // One-cell non-color selection affordance (plus RGB bg when available).
            .highlight_symbol("▸");

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
            " j/k move · PgUp/Dn page · g/G top/end · r refresh · {density_hint} · q/^C quit "
        ))
        .style(Style::default().add_modifier(Modifier::DIM));
        f.render_widget(hints, area);
    }
}

fn diag_counts_phrase(n: usize, e: usize, w: usize, i: usize, h: usize) -> String {
    let d = plural(n, "diagnostic", "diagnostics");
    let mut parts = Vec::new();
    if e > 0 {
        parts.push(plural(e, "error", "errors"));
    }
    if w > 0 {
        parts.push(plural(w, "warning", "warnings"));
    }
    if i > 0 {
        parts.push(plural(i, "info", "infos"));
    }
    if h > 0 {
        parts.push(plural(h, "hint", "hints"));
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


fn snap_find_key(snap: &Snapshot, key: &str) -> Option<usize> {
    snap.diagnostics.iter().position(|d| d.identity_key() == key)
}

/// Nearest diagnostic by (path, line, col) when exact key vanishes.
fn snap_nearest(snap: &Snapshot, key: &str, prev_idx: Option<usize>) -> Option<usize> {
    // key format: path:line:col:end:glyph:message
    let parts: Vec<&str> = key.splitn(6, ':').collect();
    if parts.len() < 3 {
        return prev_idx.map(|i| i.min(snap.diagnostics.len().saturating_sub(1)));
    }
    // path may contain ':', so parse from the right for numbers — use identity fields loosely
    let line = parts.iter().rev().nth(4).and_then(|s| s.parse::<u32>().ok());
    let path_hint = parts.first().copied().unwrap_or("");
    let mut best: Option<(usize, u32)> = None;
    for (i, d) in snap.diagnostics.iter().enumerate() {
        let path_s = d.path.display().to_string();
        if !path_s.contains(path_hint) && !path_hint.is_empty() && !path_s.ends_with(path_hint) {
            // still consider by line distance if path matches loosely
        }
        let dist = if let Some(l) = line {
            d.line.abs_diff(l)
        } else {
            0
        };
        let path_bonus = if path_s == path_hint || path_s.ends_with(path_hint) {
            0u32
        } else {
            1000
        };
        let score = dist.saturating_add(path_bonus);
        if best.map(|(_, s)| score < s).unwrap_or(true) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i).or_else(|| {
        prev_idx.map(|i| i.min(snap.diagnostics.len().saturating_sub(1)))
    })
}

fn ellipsize(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".into();
    }
    let take: String = s.chars().take(max_chars - 1).collect();
    format!("{take}…")
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

fn lhs_width(d: &DiagItem) -> usize {
    // " E " + bare location
    3 + bare_location(d).chars().count()
}

fn list_item_for(
    d: &DiagItem,
    density: ListDensity,
    loc_w: usize,
    content_w: usize,
    lhs_max: usize,
    selected: bool,
) -> ListItem<'static> {
    let glyph = d.severity.glyph();
    let style = severity_style(d.severity);
    let msg_style = if selected {
        style
    } else {
        style.add_modifier(Modifier::DIM)
    };
    let loc_style = if selected {
        Style::default()
            .fg(ratatui::style::Color::Rgb(120, 220, 255))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ratatui::style::Color::Rgb(100, 190, 230))
    };
    match density {
        ListDensity::Compact => {
            let loc = format_location(d, loc_w);
            let used = 3 + loc.chars().count() + 2; // " E " + loc + "  "
            let msg_budget = content_w.saturating_sub(used).max(4);
            let msg = ellipsize(&d.message, msg_budget);
            let row = Line::from(vec![
                Span::styled(format!(" {glyph} "), style.add_modifier(Modifier::BOLD)),
                Span::styled(loc, loc_style),
                Span::raw("  "),
                Span::styled(msg, msg_style),
            ]);
            ListItem::new(row)
        }
        ListDensity::Relaxed => {
            // LHS: severity + bare path:line:col (no trailing pad).
            let bare = bare_location(d);
            let lhs = format!(" {glyph} {bare}");
            let mut head_spans = vec![
                Span::styled(format!(" {glyph} "), style.add_modifier(Modifier::BOLD)),
                Span::styled(bare, loc_style),
            ];
            // RHS: `› frag ‹` right-aligned to content_w.
            // Only elide when the fragment would start left of lhs_max + 3.
            if let Some(frag) = code_fragment(d) {
                let min_gap = 3usize;
                let rhs_budget = content_w.saturating_sub(lhs_max.saturating_add(min_gap));
                if let Some(rhs) = format_fragment_rhs(&frag, rhs_budget) {
                    let lhs_len = lhs.chars().count();
                    let rhs_len = rhs.chars().count();
                    let gap = content_w.saturating_sub(lhs_len).saturating_sub(rhs_len);
                    // Prefer at least min_gap when budget allows; right-align uses full remainder.
                    let gap = gap.max(min_gap.min(content_w.saturating_sub(lhs_len)));
                    // Recompute if gap+lhs+rhs > content_w (gap forced): shrink gap.
                    let gap = if lhs_len + gap + rhs_len > content_w {
                        content_w.saturating_sub(lhs_len + rhs_len)
                    } else {
                        gap
                    };
                    if gap > 0 {
                        head_spans.push(Span::raw(" ".repeat(gap)));
                    }
                    head_spans.extend(paint_fragment_field(&rhs, style));
                }
            }
            let head = Line::from(head_spans);
            let msg_budget = content_w.saturating_sub(3).max(4);
            let msg_txt = ellipsize(&d.message, msg_budget);
            let msg = Line::from(Span::styled(format!("   {msg_txt}"), msg_style));
            ListItem::new(vec![head, msg])
        }
    }
}

/// Build `› frag ‹`, truncating `frag` only when it exceeds `budget` chars total.
fn format_fragment_rhs(frag: &str, budget: usize) -> Option<String> {
    // overhead: '›' + ' ' + ' ' + '‹' = 4
    const OVERHEAD: usize = 4;
    if budget <= OVERHEAD {
        return None;
    }
    let max_inner = budget - OVERHEAD;
    let inner = if frag.chars().count() > max_inner {
        if max_inner == 0 {
            return None;
        }
        if max_inner == 1 {
            "…".to_string()
        } else {
            let take: String = frag.chars().take(max_inner - 1).collect();
            format!("{take}…")
        }
    } else {
        frag.to_string()
    };
    Some(format!("› {inner} ‹"))
}

fn code_fragment(d: &DiagItem) -> Option<String> {
    let src = d.source_line.as_ref()?;
    let chars: Vec<char> = src.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let start = d.start_col0().min(chars.len().saturating_sub(1));
    let end = d.end_col0().max(start + 1).min(chars.len());
    let mut frag: String = chars[start..end].iter().collect();
    frag = frag.trim().to_string();
    if frag.is_empty() {
        return None;
    }
    // Prefer a slightly richer call slice when the bare span is a tiny literal
    // inside `Name(...)` — e.g. `true` → `MakeTint(true)` when short enough.
    if frag.chars().count() <= 6 {
        if let Some(richer) = enrich_call_fragment(&chars, start, end) {
            frag = richer;
        }
    }
    // Layout-time truncation only (right-align budget) — keep full frag here.
    Some(frag)
}

/// If span sits inside `Ident(...)`, return that call slice when compact.

/// Style a padded `› frag ‹…` field: dim brackets, severity body, dim trailing pad.
fn paint_fragment_field(field: &str, style: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let chars: Vec<char> = field.chars().collect();
    if chars.is_empty() {
        return out;
    }
    // Expect starts with ›
    let mut i = 0usize;
    if chars[0] == '›' {
        out.push(Span::styled("›".to_string(), Style::default().add_modifier(Modifier::DIM)));
        i = 1;
    }
    if i < chars.len() && chars[i] == ' ' {
        out.push(Span::raw(" "));
        i += 1;
    }
    // body until space+‹ or ‹
    let body_start = i;
    while i < chars.len() && chars[i] != '‹' {
        i += 1;
    }
    // trim trailing space before ‹ from body
    let mut body_end = i;
    while body_end > body_start && chars[body_end - 1] == ' ' {
        body_end -= 1;
    }
    if body_end > body_start {
        let body: String = chars[body_start..body_end].iter().collect();
        out.push(Span::styled(body, style.add_modifier(Modifier::BOLD)));
    }
    if body_end < i {
        out.push(Span::raw(" ".repeat(i - body_end)));
    }
    if i < chars.len() && chars[i] == '‹' {
        out.push(Span::styled("‹".to_string(), Style::default().add_modifier(Modifier::DIM)));
        i += 1;
    }
    if i < chars.len() {
        let pad: String = chars[i..].iter().collect();
        out.push(Span::raw(pad));
    }
    out
}

fn enrich_call_fragment(chars: &[char], start: usize, end: usize) -> Option<String> {
    // Only on statement-like lines (calls), not bare declarations.
    let line: String = chars.iter().collect();
    let stmt_like = line.contains('=') || line.trim_end().ends_with(';');
    if !stmt_like {
        return None;
    }
    // Walk left for '(' then identifier.
    let mut i = start;
    while i > 0 && chars[i] != '(' {
        i -= 1;
    }
    if chars.get(i) != Some(&'(') {
        return None;
    }
    let open = i;
    // identifier immediately before '('
    let mut j = open;
    while j > 0 && (chars[j - 1].is_ascii_alphanumeric() || chars[j - 1] == '_' || chars[j - 1] == ':') {
        j -= 1;
    }
    if j == open {
        return None;
    }
    // matching ')' after end
    let mut k = end.saturating_sub(1);
    let mut depth = 0i32;
    let mut close = None;
    while k < chars.len() {
        match chars[k] {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    close = Some(k);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
        k += 1;
    }
    let close = close?;
    let slice: String = chars[j..=close].iter().collect();
    let slice = slice.trim().to_string();
    if slice.chars().count() > 28 || slice.chars().count() < 4 {
        return None;
    }
    Some(slice)
}

fn pretty_detail_lines(d: &DiagItem) -> Vec<Line<'static>> {
    let path = d.path.display().to_string();
    let sev = d.severity.label();
    let sev_style = severity_style(d.severity).add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                path,
                Style::default()
                    .fg(ratatui::style::Color::Rgb(120, 220, 255))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(":{}:{}: ", d.line, d.col)),
            Span::styled(sev.to_string(), sev_style),
        ]),
        Line::from(""), // blank between header and source (even spacing in the box)
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
        // Bright palette — default Red/Yellow are muddy on dark terminals.
        Severity::Error => Style::default().fg(ratatui::style::Color::Rgb(255, 85, 85)),
        Severity::Warning => Style::default().fg(ratatui::style::Color::Rgb(255, 215, 0)),
        Severity::Info => Style::default().fg(ratatui::style::Color::Rgb(120, 200, 255)),
        Severity::Hint => Style::default()
            .fg(ratatui::style::Color::Rgb(160, 160, 170))
            .add_modifier(Modifier::DIM),
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
    fn selection_survives_insert_above() {
        let mut app = App::from_snapshot(canned_snapshot());
        app.scroll_down(); // select warning (index 1)
        let key = app.selected_diag().unwrap().identity_key();
        assert!(key.contains("Helpers"));

        let mut snap = canned_snapshot();
        // insert a new error above the warning
        snap.diagnostics.insert(
            0,
            DiagItem {
                severity: Severity::Error,
                path: std::path::PathBuf::from("src/New.as"),
                line: 1,
                col: 1,
                end_col: 2,
                message: "brand new".into(),
                source_line: None,
            },
        );
        app.apply_snapshot(snap);
        let sel = app.selected_diag().unwrap();
        assert_eq!(sel.identity_key(), key, "should keep Helpers warning selected");
        assert_eq!(app.selected(), Some(2)); // shifted down by 1
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
        assert!(diag_counts_phrase(3, 2, 1, 0, 0).contains("1 warning"));
        assert!(!diag_counts_phrase(3, 2, 1, 0, 0).contains("1 warnings"));
        assert!(diag_counts_phrase(4, 1, 1, 1, 1).contains("1 info"));
    }

    #[test]
    fn fragment_rhs_no_truncate_when_room() {
        // At 80 cols, canned frags must stay full (user requirement).
        let content_w = 78usize; // 80 - borders
        let diags = &canned_snapshot().diagnostics;
        let lhs_max = diags.iter().map(lhs_width).max().unwrap();
        let budget = content_w.saturating_sub(lhs_max + 3);
        for d in diags {
            let frag = code_fragment(d).expect("frag");
            let rhs = format_fragment_rhs(&frag, budget).expect("rhs");
            assert!(!rhs.contains('…'), "should not truncate {rhs} budget={budget}");
            if frag.contains("MakeTint") {
                assert!(rhs.contains("MakeTint(true)"), "{rhs}");
            }
            if frag == "FakeVehicleState" || frag.contains("FakeVehicle") {
                assert!(rhs.contains("FakeVehicleState"), "{rhs}");
            }
        }
    }

    #[test]
    fn fragment_rhs_right_aligns() {
        let content_w = 78usize;
        let diags = &canned_snapshot().diagnostics;
        let lhs_max = diags.iter().map(lhs_width).max().unwrap();
        let budget = content_w - lhs_max - 3;
        for d in diags {
            let bare = bare_location(d);
            let lhs = format!(" {} {bare}", d.severity.glyph());
            let frag = code_fragment(d).unwrap();
            let rhs = format_fragment_rhs(&frag, budget).unwrap();
            let gap = content_w - lhs.chars().count() - rhs.chars().count();
            assert!(gap >= 3, "gap={gap} lhs={lhs:?} rhs={rhs:?}");
            assert_eq!(
                lhs.chars().count() + gap + rhs.chars().count(),
                content_w,
                "must fill to right edge"
            );
        }
    }

    #[test]
    fn detail_has_blank_after_header() {
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
        assert!(rows.len() >= 3, "{rows:?}");
        assert!(rows[0].contains("Overlay"), "{rows:?}");
        assert_eq!(rows[1], "", "blank line after header: {rows:?}");
        assert!(rows[2].contains('│'), "{rows:?}");
    }

    #[test]
    fn code_fragment_extracts_span() {
        let d = &canned_snapshot().diagnostics[0];
        let frag = code_fragment(d).expect("frag");
        assert!(
            frag.contains("true") || frag.contains("MakeTint"),
            "{frag}"
        );
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
