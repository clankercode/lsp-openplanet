//! Immediate-mode app state + draw.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
    /// First visible detail row — the detail box scrolls when the message
    /// wraps past the box cap (B029).
    detail_scroll: u16,
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
            detail_scroll: 0,
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
            self.detail_scroll = 0;
            return;
        }

        let idx = if let Some(key) = prev_key.as_ref() {
            snap_find_key(&self.snapshot, key)
                .or_else(|| snap_nearest(&self.snapshot, key, prev_idx))
                .unwrap_or_else(|| prev_idx.unwrap_or(0).min(len - 1))
        } else {
            prev_idx.unwrap_or(0).min(len - 1)
        };
        self.select_index(idx);
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
    ///
    /// In Relaxed the *selected* item can spill its message onto a third row
    /// (B029), so one item in the viewport may be taller than `rows_per_item`.
    /// Budget for that row rather than measuring every item: under-paging by at
    /// most one item is harmless, paging past the viewport is not.
    pub fn page_step(&self) -> usize {
        let spill = usize::from(self.density == ListDensity::Relaxed);
        let inner = (self.list_inner_h.saturating_sub(2) as usize).saturating_sub(spill);
        let per = self.density.rows_per_item().max(1);
        let visible = (inner / per).max(1);
        visible.saturating_sub(1).max(1)
    }

    pub fn toggle_density(&mut self) {
        // Density is a list-only concern — the detail box shows the same text
        // either way, so the reader keeps their place in it.
        self.density = self.density.toggle();
    }

    #[cfg(test)]
    fn set_list_inner_h_for_test(&mut self, h: u16) {
        self.list_inner_h = h;
    }

    /// First visible detail-box row (0 = top).
    pub fn detail_scroll(&self) -> u16 {
        self.detail_scroll
    }

    /// Scroll the detail box down one row. Clamped to the content at draw time.
    pub fn scroll_detail_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    /// Scroll the detail box up one row.
    pub fn scroll_detail_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
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
            self.detail_scroll = 0;
        } else {
            self.select_index(0);
        }
    }

    pub fn select_last(&mut self) {
        let len = self.snapshot.diagnostics.len();
        if len == 0 {
            self.list_state.select(None);
            self.selected_key = None;
            self.detail_scroll = 0;
        } else {
            self.select_index(len - 1);
        }
    }

    fn select_index(&mut self, idx: usize) {
        let key = self.snapshot.diagnostics.get(idx).map(|d| d.identity_key());
        // The identity key embeds the message, so an unchanged key means the
        // detail box still shows the same text — keep the reader's place in it.
        // (k at the top of the list, or a watch re-check, must not scroll them
        // back to the start of a long message.)
        if key != self.selected_key {
            self.detail_scroll = 0;
        }
        self.list_state.select(Some(idx));
        self.selected_key = key;
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
        // Width-aware: a message that wraps needs more rows (B029). Built once
        // per frame so the height and the rendered rows cannot disagree.
        let detail_inner_w = chunks[1].width.saturating_sub(2) as usize;
        let (detail_title, detail_lines) = match self.selected_diag() {
            None => (" detail ".to_string(), empty_detail_lines()),
            Some(d) => (
                format!(" detail · {} ", d.severity.label()),
                pretty_detail_lines(d, detail_inner_w),
            ),
        };
        let detail_h = detail_box_height(detail_lines.len());
        let mid = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(detail_h)])
            .split(chunks[1]);
        self.list_inner_h = mid[0].height;
        self.draw_list(f, mid[0]);
        self.draw_detail(f, mid[1], detail_title, detail_lines);

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
        // `root_label` and a failed run's message are untrusted display text.
        let body = sanitize_row(&format!(" {} ", parts.join("  ·  ")));
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
                .map(|d| str_w(&bare_location(d)))
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

        let sel = self.list_state.selected().map(|i| i + 1).unwrap_or(0);
        let n = self.snapshot.diagnostics.len();
        let stale = if self.snapshot.stale { " · stale" } else { "" };
        let title = if n == 0 {
            format!(" diagnostics · {}{stale} ", self.density.label())
        } else {
            format!(
                " diagnostics · {} · {sel}/{n}{stale} ",
                self.density.label()
            )
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

    fn draw_detail(
        &mut self,
        f: &mut Frame<'_>,
        area: Rect,
        mut title: String,
        lines: Vec<Line<'static>>,
    ) {
        let inner_w = area.width.saturating_sub(2) as usize;
        let inner_h = area.height.saturating_sub(2);

        // The box caps at 8 inner rows; a longer wrapped message scrolls (B029).
        // Saturate rather than wrap: `Paragraph::scroll` is u16, so a message
        // with more rows than that is unreachable either way, but a wrapped
        // count would report (and scroll to) nonsense.
        let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let max_scroll = total.saturating_sub(inner_h);
        self.detail_scroll = self.detail_scroll.min(max_scroll);
        // `inner_h == 0` means the pane got squeezed to nothing on a very short
        // terminal — there is no range to report.
        if max_scroll > 0 && inner_h > 0 {
            let top = self.detail_scroll + 1;
            let bot = (self.detail_scroll + inner_h).min(total);
            let suffix = format!("· {top}-{bot}/{total} · J/K ");
            if str_w(&title) + str_w(&suffix) <= inner_w {
                title.push_str(&suffix);
            } else {
                // Too narrow for the range — the hint still has to survive, it
                // is the only thing telling the user there is more to read.
                title = scroll_hint_title(inner_w);
            }
        }

        let block = Block::default().borders(Borders::ALL).title(title);
        // No wrap: source + caret lines must stay column-aligned. The message
        // is soft-wrapped to `inner_w` by `pretty_detail_lines` instead, so the
        // Paragraph never reflows the aligned rows.
        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((self.detail_scroll, 0)),
            area,
        );
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
    snap.diagnostics
        .iter()
        .position(|d| d.identity_key() == key)
}

/// Nearest diagnostic by (path, line, col) when exact key vanishes.
fn snap_nearest(snap: &Snapshot, key: &str, prev_idx: Option<usize>) -> Option<usize> {
    // key format: path:line:col:end:glyph:message
    let parts: Vec<&str> = key.splitn(6, ':').collect();
    if parts.len() < 3 {
        return prev_idx.map(|i| i.min(snap.diagnostics.len().saturating_sub(1)));
    }
    // path may contain ':', so parse from the right for numbers — use identity fields loosely
    let line = parts
        .iter()
        .rev()
        .nth(4)
        .and_then(|s| s.parse::<u32>().ok());
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
    best.map(|(i, _)| i)
        .or_else(|| prev_idx.map(|i| i.min(snap.diagnostics.len().saturating_sub(1))))
}

/// Display columns of `s` (wide/CJK chars count as 2).
fn str_w(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// User-perceived characters. Truncation and wrapping must never split one:
/// `"\u{a9}\u{fe0f}"` and ZWJ emoji are several `char`s but one cell group, and
/// summing per-`char` widths across them does not equal the cluster's width.
fn graphemes(s: &str) -> impl DoubleEndedIterator<Item = &str> {
    UnicodeSegmentation::graphemes(s, true)
}

/// Truncate `s` to `max_w` display columns, marking the cut with `…`.
/// Returns `s` unchanged when it already fits (no stray marker).
fn ellipsize(s: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }
    if str_w(s) <= max_w {
        return s.to_string();
    }
    truncate_with_marker(s, max_w)
}

/// Keep the *tail* of `s` within `max_w` columns, marking the cut with a
/// leading `…` (paths read better from the right).
fn trim_left_to(s: &str, max_w: usize) -> String {
    if str_w(s) <= max_w {
        return s.to_string();
    }
    if max_w == 0 {
        return String::new();
    }
    let mut tail: Vec<&str> = Vec::new();
    let mut w = 1usize; // the leading `…`
    for g in graphemes(s).rev() {
        let gw = str_w(g);
        if w + gw > max_w {
            break;
        }
        tail.push(g);
        w += gw;
    }
    tail.reverse();
    format!("…{}", tail.concat())
}

/// Append a `…` to `s` (already within `max_w`) to signal more content follows.
fn with_more_marker(s: &str, max_w: usize) -> String {
    if max_w == 0 {
        return String::new();
    }
    truncate_with_marker(s, max_w)
}

/// Fill `max_w` columns from the front of `s`, always leaving room for a `…`.
fn truncate_with_marker(s: &str, max_w: usize) -> String {
    let mut out = String::new();
    let mut w = 0usize;
    for g in graphemes(s) {
        let gw = str_w(g);
        // Reserve one column for the `…`.
        if w + gw + 1 > max_w {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    out.push('…');
    out
}

/// Soft-wrap `s` to `width` display columns.
///
/// Line terminators are hard breaks (blank lines are preserved), tabs become a
/// single space and any other control character is dropped — a `Line` renders as
/// one row, so an unsplit `"a\nb"` would draw as `ab` and mis-size the box.
/// Within a logical line: greedy word wrap on single spaces, with tokens wider
/// than the line hard-broken on grapheme-cluster boundaries so nothing overflows.
///
/// Content is never dropped. For a single logical line of single-space-separated
/// words no wider than `width`, `wrap_display(msg, width).join(" ") == msg`.
///
/// A produced row exceeds `width` only when one grapheme cluster is itself wider
/// than `width` (a 2-column char in a 1-column box); dropping it would be worse.
fn wrap_display(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for logical in normalize_for_wrap(s).split('\n') {
        out.extend(wrap_logical_line(logical, width));
    }
    out
}

/// Fold every line terminator to `\n`, tabs to a space, and drop any other
/// control character.
///
/// A `Line` is one rendered row, so an embedded terminator has to become a real
/// row split. The remaining controls are dropped here rather than left for the
/// renderer to drop, so the wrapped width matches what is actually painted.
fn normalize_for_wrap(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF is one break, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            // LF, NEL, LINE SEPARATOR, PARAGRAPH SEPARATOR.
            '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Word-wrap one newline-free line. See [`wrap_display`].
fn wrap_logical_line(s: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    // Whether a word has been placed on `cur`. Not the same as `!cur.is_empty()`:
    // an empty word (from a double space) starts the line without adding text,
    // and the following word still needs its separating space.
    let mut started = false;

    for word in s.split(' ') {
        let word_w = str_w(word);
        let sep = usize::from(started);
        if cur_w + sep + word_w <= width {
            if started {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += word_w;
            started = true;
            continue;
        }
        // Does not fit here — start a fresh line.
        if started {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        started = true;
        if word_w <= width {
            cur.push_str(word);
            cur_w = word_w;
            continue;
        }
        // Oversized token: hard-break it on cluster boundaries by display width.
        for g in graphemes(word) {
            let gw = str_w(g);
            if cur_w + gw > width && cur_w > 0 {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push_str(g);
            cur_w += gw;
        }
    }
    out.push(cur);
    out
}

fn bare_location(d: &DiagItem) -> String {
    sanitize_row(&format!("{}:{}:{}", short_path(&d.path), d.line, d.col))
}

fn format_location(d: &DiagItem, loc_w: usize) -> String {
    if loc_w == 0 {
        return String::new();
    }
    // Keep the tail (file:line:col) when truncating long paths, then pad by
    // display columns (not chars) so the message column stays put.
    let truncated = trim_left_to(&bare_location(d), loc_w);
    let pad = loc_w.saturating_sub(str_w(&truncated));
    format!("{truncated}{}", " ".repeat(pad))
}

/// One-row display text with every control character removed.
///
/// Ratatui drops control characters as it writes into the buffer, which is what
/// keeps them off the terminal — but dropping *shifts* everything after them a
/// column left, so a row built from unsanitised text does not land where its
/// width was computed. Removing them here keeps the layout honest, and keeps the
/// TUI correct regardless of what the renderer chooses to do. A list row is one
/// row, so line terminators and tabs collapse to a space.
fn sanitize_row(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' | '\u{85}' | '\u{2028}' | '\u{2029}' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Same, but one char in, one char out: a diagnostic's column is a char offset
/// into the source line, so sanitising it must not shift anything. This is the
/// one that matters visibly — the caret row is built from those offsets, so a
/// control character the renderer drops from the source row would slide the code
/// out from under the carets.
fn sanitize_keep_columns(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Widest "there is more below" hint that fits `inner_w` title columns.
///
/// Tiered rather than truncated: a clipped `J/` teaches the reader nothing, and
/// on a pane this narrow the hint is the only overflow signal there is.
fn scroll_hint_title(inner_w: usize) -> String {
    for hint in [" ↕ J/K ", "↕ J/K", "J/K", "↕"] {
        if str_w(hint) <= inner_w {
            return hint.to_string();
        }
    }
    String::new()
}

/// Placeholder body shown when nothing is selected.
fn empty_detail_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "  Nothing selected.",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "  j/k moves the list · Enter is not required.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]
}

/// Total terminal rows for a detail box holding `content` lines (borders
/// included). Inner content height = max(6, content + 1), then +2 for borders.
fn detail_box_height(content: usize) -> u16 {
    let inner = content.saturating_add(1).max(6);
    // Hard cap: never more than 8 content rows (+2 borders = 10). Past this the
    // box scrolls rather than growing.
    let inner = inner.min(8);
    (inner + 2) as u16
}

fn lhs_width(d: &DiagItem) -> usize {
    // " E " + bare location
    3 + str_w(&bare_location(d))
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
            let (eff_loc_w, msg_budget) = compact_field_widths(content_w, loc_w);
            let loc = format_location(d, eff_loc_w);
            let msg = ellipsize(&sanitize_row(&d.message), msg_budget);
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
                    let lhs_len = str_w(&lhs);
                    let rhs_len = str_w(&rhs);
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
            let mut rows = vec![head];
            for txt in relaxed_msg_rows(&sanitize_row(&d.message), content_w, selected) {
                rows.push(Line::from(Span::styled(
                    format!("{}{txt}", " ".repeat(RELAXED_MSG_INDENT)),
                    msg_style,
                )));
            }
            ListItem::new(rows)
        }
    }
}

/// Fixed chrome around a Compact row's two fields: `" E "` plus the `"  "` gap.
const COMPACT_CHROME: usize = 5;
/// Columns the message would like to keep before the location may grow into them.
const COMPACT_MSG_MIN: usize = 20;
/// Indent of a Relaxed message row under its header row.
const RELAXED_MSG_INDENT: usize = 3;

/// Split a Compact row's content width into `(location, message)` budgets.
///
/// The location shrinks first (B029), so a long `path:line:col` can never
/// squeeze the message down to `abc…`: the message is guaranteed
/// `min(COMPACT_MSG_MIN, ceil(avail / 2))` columns. The location never grows
/// beyond what it asked for.
fn compact_field_widths(content_w: usize, loc_w: usize) -> (usize, usize) {
    let avail = content_w.saturating_sub(COMPACT_CHROME);
    let loc_cap = avail.saturating_sub(COMPACT_MSG_MIN).max(avail / 2);
    let loc = loc_w.min(loc_cap);
    (loc, avail - loc)
}

/// Message row text for a Relaxed list item.
///
/// One row normally; the *selected* item spills to a second row when the
/// message does not fit, so the row the user is reading shows more (B029).
/// A `…` on the last row means content is still cut — the detail box has it all.
fn relaxed_msg_rows(msg: &str, content_w: usize, selected: bool) -> Vec<String> {
    let budget = content_w.saturating_sub(RELAXED_MSG_INDENT);
    if !selected {
        return vec![ellipsize(msg, budget)];
    }
    let wrapped = wrap_display(msg, budget);
    if wrapped.len() < 2 {
        return vec![ellipsize(msg, budget)];
    }
    let second = if wrapped.len() > 2 {
        with_more_marker(&wrapped[1], budget)
    } else {
        wrapped[1].clone()
    };
    vec![wrapped[0].clone(), second]
}

/// Build `› frag ‹`, truncating `frag` only when it exceeds `budget` columns total.
fn format_fragment_rhs(frag: &str, budget: usize) -> Option<String> {
    // overhead: '›' + ' ' + ' ' + '‹' = 4
    const OVERHEAD: usize = 4;
    if budget <= OVERHEAD {
        return None;
    }
    Some(format!("› {} ‹", ellipsize(frag, budget - OVERHEAD)))
}

fn code_fragment(d: &DiagItem) -> Option<String> {
    // Column-preserving: `start`/`end` below are char offsets into the line.
    let src = sanitize_keep_columns(d.source_line.as_ref()?);
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
        out.push(Span::styled(
            "›".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
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
        out.push(Span::styled(
            "‹".to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
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
    while j > 0
        && (chars[j - 1].is_ascii_alphanumeric() || chars[j - 1] == '_' || chars[j - 1] == ':')
    {
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

/// Detail-box body for `d`, laid out for an inner width of `inner_w` columns.
fn pretty_detail_lines(d: &DiagItem, inner_w: usize) -> Vec<Line<'static>> {
    let sev = d.severity.label();
    let sev_style = severity_style(d.severity).add_modifier(Modifier::BOLD);
    let path_style = Style::default()
        .fg(ratatui::style::Color::Rgb(120, 220, 255))
        .add_modifier(Modifier::BOLD);

    // Header: keep `:line:col: severity` and trim the path from the left, so a
    // long absolute path is visibly shortened rather than silently clipped.
    let loc_tail = format!(":{}:{}: ", d.line, d.col);
    let tail_w = str_w(&loc_tail) + str_w(sev);
    let header = if inner_w >= tail_w {
        vec![
            Span::styled(
                trim_left_to(
                    &sanitize_row(&d.path.display().to_string()),
                    inner_w - tail_w,
                ),
                path_style,
            ),
            Span::raw(loc_tail),
            Span::styled(sev.to_string(), sev_style),
        ]
    } else {
        vec![Span::styled(
            ellipsize(&format!("{loc_tail}{sev}"), inner_w),
            sev_style,
        )]
    };

    let mut lines = vec![
        Line::from(header),
        Line::from(""), // blank between header and source (even spacing in the box)
    ];

    if let Some(src) = d.source_line.as_deref().map(sanitize_keep_columns) {
        let src = src.as_str();
        let gutter_w = d.line.to_string().len().max(2);
        let gutter = format!("{:>gutter_w$}", d.line);
        let blank = " ".repeat(gutter_w);
        let pipe = Style::default().add_modifier(Modifier::DIM);

        let start = d.start_col0().min(src.chars().count());
        let end = d.end_col0().max(start + 1);
        let end = end.min(src.chars().count().max(start + 1));
        // A range that lands inside a cluster (a lone combining mark, or half a
        // ZWJ sequence) would otherwise caret the gap next to the glyph rather
        // than the glyph itself.
        let (start, end) = snap_to_clusters(src, start, end);

        // Source line with the underlined span emphasized. Columns are char
        // offsets (that is what the diagnostic carries), but the caret row is
        // padded in DISPLAY columns so a wide char before the span does not
        // push the carets out of alignment with it.
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
        let pad = " ".repeat(str_w(&before));
        let carets = "^".repeat(str_w(&mid).max(1));
        lines.push(Line::from(vec![
            Span::raw(format!("  {gutter} ")),
            Span::styled("│".to_string(), pipe),
            Span::raw(" "),
            Span::raw(before),
            Span::styled(
                mid,
                severity_style(d.severity).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
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

        // Message: soft-wrapped here (B029) so the Paragraph can stay unwrapped
        // and the source/caret rows above keep their column alignment. Every
        // continuation row repeats the gutter prefix, so it lands in the same
        // column as the first.
        let prefix = format!("  {blank} ");
        let msg_w = inner_w.saturating_sub(str_w(&prefix) + 2); // + "│" + " "
        for chunk in wrap_display(&d.message, msg_w) {
            lines.push(Line::from(vec![
                Span::raw(prefix.clone()),
                Span::styled("│".to_string(), pipe),
                Span::raw(" "),
                Span::styled(chunk, severity_style(d.severity)),
            ]));
        }
    } else {
        for chunk in wrap_display(&d.message, inner_w.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(chunk, severity_style(d.severity)),
            ]));
        }
    }

    lines
}

/// Widen a char-offset range to whole grapheme clusters.
///
/// Diagnostics carry character columns, but a caret has to sit under a rendered
/// glyph — and one glyph can be several chars (`e` + U+0301, a ZWJ emoji).
fn snap_to_clusters(src: &str, start: usize, end: usize) -> (usize, usize) {
    let (mut lo, mut hi) = (start, end);
    let mut off = 0usize; // char offset of the cluster being examined
    for g in graphemes(src) {
        let next = off + g.chars().count();
        if start > off && start < next {
            lo = off;
        }
        if end > off && end < next {
            hi = next;
        }
        off = next;
    }
    (lo, hi.max(lo + 1))
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
    use unicode_width::UnicodeWidthStr;

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
        assert_eq!(
            sel.identity_key(),
            key,
            "should keep Helpers warning selected"
        );
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
        let lines = pretty_detail_lines(d, 78);
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
            assert!(
                !rhs.contains('…'),
                "should not truncate {rhs} budget={budget}"
            );
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
        let lines = pretty_detail_lines(d, 78);
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
        assert!(frag.contains("true") || frag.contains("MakeTint"), "{frag}");
    }

    #[test]
    fn detail_box_height_min_six_inner() {
        // borders + max(6, content+1) — at least 8 total rows
        let h = box_height(Some(&canned_snapshot().diagnostics[0]), 78);
        assert!(h >= 8, "h={h}");
        assert!(h <= 10, "h={h}");
        let empty = box_height(None, 78);
        assert!(empty >= 8, "empty={empty}");
    }

    // ---- B029: readable messages at narrow widths ------------------------

    /// The ticket's worst case: a long "no matching signatures" message.
    const LONG_MSG: &str = "no matching signatures to 'MakeTint(bool)' - candidates are \
                            'vec4 MakeTint(int, int, int)' and 'vec4 MakeTint(const string &in)'";
    /// Long enough to need two rows at ~44 columns, short enough to fit them.
    const MEDIUM_MSG: &str = "expected 'int', got 'bool' in argument 1 of MakeTint";

    const SEVERITIES: [Severity; 4] = [
        Severity::Error,
        Severity::Warning,
        Severity::Info,
        Severity::Hint,
    ];

    fn diag(sev: Severity, message: &str) -> DiagItem {
        DiagItem {
            severity: sev,
            path: std::path::PathBuf::from("src/Overlay.as"),
            line: 23,
            col: 26,
            end_col: 30,
            message: message.into(),
            source_line: Some("    vec4 tint = MakeTint(true);".into()),
        }
    }

    fn diag_no_source(sev: Severity, message: &str) -> DiagItem {
        DiagItem {
            source_line: None,
            ..diag(sev, message)
        }
    }

    fn line_text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn detail_rows(d: &DiagItem, inner_w: usize) -> Vec<String> {
        pretty_detail_lines(d, inner_w)
            .iter()
            .map(line_text)
            .collect()
    }

    /// Index of the first message row. The layout is fixed:
    /// header, blank, [source, caret,] message rows.
    fn detail_msg_start(d: &DiagItem) -> usize {
        if d.source_line.is_some() {
            4
        } else {
            2
        }
    }

    /// Message text of each detail message row, with the gutter prefix stripped.
    fn detail_msg_parts(d: &DiagItem, inner_w: usize) -> Vec<String> {
        let lines = pretty_detail_lines(d, inner_w);
        lines[detail_msg_start(d)..]
            .iter()
            .map(|l| {
                l.spans
                    .last()
                    .map(|s| s.content.to_string())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Rows the detail box would occupy for `d` at `inner_w` columns — the same
    /// two steps `App::draw` takes.
    fn box_height(d: Option<&DiagItem>, inner_w: usize) -> u16 {
        detail_box_height(match d {
            None => empty_detail_lines().len(),
            Some(d) => pretty_detail_lines(d, inner_w).len(),
        })
    }

    fn squash(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn detail_message_wraps_at_narrow_width() {
        let d = diag(Severity::Error, LONG_MSG);
        let inner_w = 40usize;
        let parts = detail_msg_parts(&d, inner_w);
        assert!(parts.len() > 1, "message should wrap: {parts:?}");

        for row in detail_rows(&d, inner_w)[detail_msg_start(&d)..].iter() {
            assert!(
                row.width() <= inner_w,
                "row {row:?} is {} wide, inner_w={inner_w}",
                row.width()
            );
        }
        assert_eq!(parts.join(" "), LONG_MSG, "wrapping must not drop content");
    }

    #[test]
    fn detail_box_height_grows_with_wrapped_message() {
        let d = diag(Severity::Error, LONG_MSG);
        let wide = box_height(Some(&d), 200);
        let narrow = box_height(Some(&d), 34);
        assert!(wide >= 8, "wide={wide}");
        assert!(
            narrow > wide,
            "box must grow when the message wraps: {narrow} vs {wide}"
        );
        assert!(narrow <= 10, "capped at 8 inner rows + 2 borders: {narrow}");
    }

    #[test]
    fn caret_alignment_survives_wrapping() {
        let d = diag(Severity::Error, LONG_MSG);
        assert!(
            detail_msg_parts(&d, 34).len() > 1,
            "precondition: the message wraps at 34 columns"
        );
        let col_of = |inner_w: usize, needle: char| -> usize {
            detail_rows(&d, inner_w)
                .into_iter()
                .find(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("no row with {needle:?}"))
                .find(needle)
                .expect("needle")
        };
        assert_eq!(
            col_of(34, '^'),
            col_of(200, '^'),
            "caret column moved when the message wrapped"
        );
        assert_eq!(
            col_of(34, '\u{2502}'),
            col_of(200, '\u{2502}'),
            "gutter column moved when the message wrapped"
        );
    }

    #[test]
    fn detail_message_wraps_identically_for_every_severity() {
        let base = detail_msg_parts(&diag(Severity::Error, LONG_MSG), 40);
        assert!(base.len() > 1, "precondition: wraps");
        for sev in SEVERITIES {
            assert_eq!(
                detail_msg_parts(&diag(sev, LONG_MSG), 40),
                base,
                "severity {sev:?} wrapped differently"
            );
            assert_eq!(
                detail_msg_parts(&diag_no_source(sev, LONG_MSG), 40).len(),
                detail_msg_parts(&diag_no_source(Severity::Error, LONG_MSG), 40).len(),
                "severity {sev:?} wrapped differently without a source line"
            );
        }
    }

    /// Width sweep. Source + caret rows are excluded on purpose: they must stay
    /// column-aligned and are allowed to clip (see `draw_detail`).
    #[test]
    fn detail_wrap_width_sweep_never_overflows_or_drops() {
        for d in [
            diag(Severity::Error, LONG_MSG),
            diag_no_source(Severity::Warning, LONG_MSG),
        ] {
            for inner_w in [20usize, 40, 60, 100, 200] {
                let start = detail_msg_start(&d);
                let rows = detail_rows(&d, inner_w);
                assert!(
                    rows[0].width() <= inner_w,
                    "header overflows at {inner_w}: {:?}",
                    rows[0]
                );
                for row in &rows[start..] {
                    assert!(row.width() <= inner_w, "w={inner_w} row={row:?}");
                }
                let parts = detail_msg_parts(&d, inner_w);
                assert_eq!(
                    squash(&parts.concat()),
                    squash(LONG_MSG),
                    "content dropped at w={inner_w}"
                );
                assert!(
                    !parts.concat().contains('\u{2026}'),
                    "the detail box must wrap, not ellipsize (w={inner_w})"
                );
            }
        }
    }

    #[test]
    fn detail_hard_wraps_unbroken_token() {
        let token = "A".repeat(200);
        let d = diag(Severity::Error, &token);
        for inner_w in [20usize, 40, 60] {
            for row in detail_rows(&d, inner_w)[detail_msg_start(&d)..].iter() {
                assert!(row.width() <= inner_w, "w={inner_w} row={row:?}");
            }
            assert_eq!(
                detail_msg_parts(&d, inner_w).concat(),
                token,
                "hard wrap dropped content at w={inner_w}"
            );
        }
    }

    #[test]
    fn detail_wraps_by_display_width_not_char_count() {
        // Every char here is two columns wide.
        let msg = "\u{8a3a}\u{65ad}\u{30e1}\u{30c3}\u{30bb}\u{30fc}\u{30b8}\u{304c}\u{9577}\u{3059}\u{304e}\u{3066}\u{6298}\u{308a}\u{8fd4}\u{3057}\u{304c}\u{5fc5}\u{8981}";
        assert_eq!(msg.width(), msg.chars().count() * 2, "fixture must be wide");
        let d = diag(Severity::Error, msg);
        for inner_w in [20usize, 30, 41] {
            for row in detail_rows(&d, inner_w)[detail_msg_start(&d)..].iter() {
                assert!(
                    row.width() <= inner_w,
                    "w={inner_w} row={row:?} width={}",
                    row.width()
                );
            }
            assert_eq!(detail_msg_parts(&d, inner_w).concat(), msg);
        }
    }

    #[test]
    fn detail_empty_and_short_messages_are_safe() {
        for msg in ["", "x"] {
            for inner_w in [8usize, 40, 200] {
                for d in [
                    diag(Severity::Hint, msg),
                    diag_no_source(Severity::Hint, msg),
                ] {
                    let parts = detail_msg_parts(&d, inner_w);
                    assert_eq!(parts.len(), 1, "msg={msg:?} w={inner_w}: {parts:?}");
                    assert_eq!(parts[0].trim(), msg, "msg={msg:?} w={inner_w}");
                    assert!(
                        !parts[0].contains('\u{2026}'),
                        "stray ellipsis: {parts:?} (msg={msg:?} w={inner_w})"
                    );
                }
            }
        }
    }

    #[test]
    fn detail_tiny_widths_do_not_panic() {
        for inner_w in 0..=12usize {
            for msg in ["", "x", LONG_MSG] {
                let _ = pretty_detail_lines(&diag(Severity::Error, msg), inner_w);
                let _ = pretty_detail_lines(&diag_no_source(Severity::Error, msg), inner_w);
                let _ = box_height(Some(&diag(Severity::Error, msg)), inner_w);
            }
        }
    }

    #[test]
    fn detail_header_fits_inner_width() {
        let mut d = diag(Severity::Error, "short");
        d.path = std::path::PathBuf::from("/very/long/absolute/path/to/the/plugin/src/Overlay.as");
        let full = format!("{}:23:26: error", d.path.display());
        for inner_w in [20usize, 30, 40, 200] {
            let header = detail_rows(&d, inner_w).remove(0);
            assert!(
                header.width() <= inner_w,
                "header overflows at {inner_w}: {header:?}"
            );
            assert!(
                header.ends_with(":23:26: error"),
                "header must keep line:col + severity at {inner_w}: {header:?}"
            );
            assert_eq!(
                header.contains('\u{2026}'),
                full.width() > inner_w,
                "header \u{2026} iff truncated at {inner_w}: {header:?}"
            );
        }
    }

    #[test]
    fn compact_field_widths_shrink_location_first() {
        for content_w in [12usize, 20, 26, 40, 60, 78, 120] {
            for loc_w in [4usize, 12, 20, 40] {
                let (loc, msg) = compact_field_widths(content_w, loc_w);
                let avail = content_w.saturating_sub(COMPACT_CHROME);
                assert_eq!(
                    loc + msg,
                    avail,
                    "budgets must fill the row (cw={content_w} lw={loc_w})"
                );
                assert!(
                    loc <= loc_w,
                    "location must never grow (cw={content_w} lw={loc_w} -> {loc})"
                );
                let floor = COMPACT_MSG_MIN.min(avail - avail / 2);
                assert!(
                    msg >= floor,
                    "message floor {floor} not met: msg={msg} cw={content_w} lw={loc_w}"
                );
            }
        }
    }

    #[test]
    fn compact_long_location_still_leaves_a_readable_message() {
        let d = diag(Severity::Error, LONG_MSG);
        // A pathological `path:line:col` that wants the whole row.
        let loc_w = 40usize;
        for content_w in [30usize, 40, 60, 78] {
            let (_, budget) = compact_field_widths(content_w, loc_w);
            assert!(
                budget >= 12,
                "content_w={content_w} left only {budget} columns for the message"
            );
            let msg = ellipsize(&d.message, budget);
            assert!(msg.width() <= budget, "{msg:?} exceeds {budget}");
            assert!(
                msg.width() >= 12,
                "message unreadable at content_w={content_w}: {msg:?}"
            );
        }
    }

    #[test]
    fn list_rows_ellipsize_only_when_truncated() {
        for msg in ["", "x", "short message", LONG_MSG] {
            for content_w in [20usize, 40, 60, 78, 120] {
                let (_, budget) = compact_field_widths(content_w, 20);
                let out = ellipsize(msg, budget);
                assert!(
                    out.width() <= budget,
                    "compact overflow: {out:?} budget={budget}"
                );
                assert_eq!(
                    out.contains('\u{2026}'),
                    msg.width() > budget,
                    "compact \u{2026} iff truncated (msg={msg:?} budget={budget})"
                );

                let rows = relaxed_msg_rows(msg, content_w, false);
                assert_eq!(rows.len(), 1, "unselected relaxed row stays single-line");
                let budget = content_w.saturating_sub(RELAXED_MSG_INDENT);
                assert!(
                    rows[0].width() <= budget,
                    "relaxed overflow: {:?} budget={budget}",
                    rows[0]
                );
                assert_eq!(
                    rows[0].contains('\u{2026}'),
                    msg.width() > budget,
                    "relaxed \u{2026} iff truncated (msg={msg:?} w={content_w})"
                );
            }
        }
    }

    #[test]
    fn relaxed_selected_row_spills_to_second_line() {
        for content_w in [30usize, 44, 60] {
            let budget = content_w - RELAXED_MSG_INDENT;
            let sel = relaxed_msg_rows(LONG_MSG, content_w, true);
            let unsel = relaxed_msg_rows(LONG_MSG, content_w, false);
            assert_eq!(unsel.len(), 1, "unselected stays one row");
            assert_eq!(
                sel.len(),
                2,
                "selected spills to a second row (w={content_w})"
            );
            for r in &sel {
                assert!(r.width() <= budget, "{r:?} exceeds {budget}");
            }
            assert!(
                sel[1].ends_with('\u{2026}'),
                "LONG_MSG needs more than two rows at {content_w}: {sel:?}"
            );
            assert!(
                squash(&sel.concat()).len() > squash(&unsel.concat()).len(),
                "spilling must reveal more text: {sel:?} vs {unsel:?}"
            );
        }
    }

    #[test]
    fn relaxed_selected_row_stays_single_when_it_fits() {
        assert_eq!(relaxed_msg_rows("short", 60, true).len(), 1);
        assert_eq!(relaxed_msg_rows("", 60, true).len(), 1);
        // Fits in exactly two rows -> no ellipsis on the spill.
        let rows = relaxed_msg_rows(MEDIUM_MSG, 33, true);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            !rows.concat().contains('\u{2026}'),
            "no ellipsis when the spill fits: {rows:?}"
        );
        assert_eq!(rows.join(" "), MEDIUM_MSG, "{rows:?}");
    }

    #[test]
    fn ellipsize_respects_display_width() {
        let wide = "\u{8a3a}\u{65ad}\u{30e1}\u{30c3}\u{30bb}\u{30fc}\u{30b8}";
        assert_eq!(wide.width(), 14);
        for max in [1usize, 2, 5, 8, 13, 14, 20] {
            let out = ellipsize(wide, max);
            assert!(
                out.width() <= max,
                "max={max} out={out:?} width={}",
                out.width()
            );
            assert_eq!(
                out.contains('\u{2026}'),
                wide.width() > max,
                "max={max} out={out:?}"
            );
        }
        assert_eq!(ellipsize("abc", 10), "abc");
        assert_eq!(ellipsize("", 10), "");
        assert_eq!(ellipsize("abcdef", 4), "abc\u{2026}");
        assert_eq!(ellipsize("abc", 0), "");
    }

    #[test]
    fn wrap_display_round_trips_and_never_overflows() {
        for (label, msg) in [
            ("plain", LONG_MSG.to_string()),
            ("leading space", format!(" {LONG_MSG}")),
            ("trailing space", format!("{LONG_MSG} ")),
            ("double spaces", LONG_MSG.replace(' ', "  ")),
            ("one word", "x".to_string()),
            ("empty", String::new()),
        ] {
            let longest_word = msg.split(' ').map(str_w).max().unwrap_or(0);
            for width in 1..=60usize {
                let out = wrap_display(&msg, width);
                assert!(!out.is_empty(), "{label} w={width}: no rows");
                for row in &out {
                    assert!(row.width() <= width, "{label} w={width} row={row:?}");
                }
                if width >= longest_word {
                    // No hard breaks: the separators are recoverable exactly.
                    assert_eq!(out.join(" "), msg, "{label} w={width} lost content");
                } else {
                    // Hard-broken tokens lose only the row-boundary spaces.
                    assert_eq!(
                        squash(&out.concat()),
                        squash(&msg),
                        "{label} w={width} dropped characters"
                    );
                }
            }
        }
    }

    #[test]
    fn wrap_display_zero_width_is_a_single_empty_row() {
        assert_eq!(wrap_display(LONG_MSG, 0), vec![String::new()]);
        assert_eq!(wrap_display("", 10), vec![String::new()]);
    }

    #[test]
    fn wrap_display_only_overflows_for_a_char_wider_than_the_line() {
        // A 2-column char cannot fit a 1-column line; every row still holds
        // exactly one char rather than looping or dropping it.
        let msg = "\u{8a3a}\u{65ad}\u{30e1}";
        let out = wrap_display(msg, 1);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out.concat(), msg);
        // From width 2 up, nothing overflows.
        for width in 2..=12usize {
            for row in wrap_display(msg, width) {
                assert!(row.width() <= width, "w={width} row={row:?}");
            }
        }
    }

    #[test]
    fn detail_scroll_survives_an_identical_refresh() {
        let mut app = App::from_snapshot(canned_snapshot());
        app.scroll_detail_down();
        app.scroll_detail_down();
        assert_eq!(app.detail_scroll(), 2);
        // A watch re-check that produces the same list must not yank the
        // reader back to the top of the message.
        app.apply_snapshot(canned_snapshot());
        assert_eq!(app.detail_scroll(), 2);

        // ...but a refresh that changes the selected diagnostic does reset it.
        let mut snap = canned_snapshot();
        snap.diagnostics[0].message = "a completely different message".into();
        app.apply_snapshot(snap);
        assert_eq!(app.detail_scroll(), 0);
    }

    /// Whole clusters only: `part` must be a prefix of `s` on cluster boundaries.
    fn is_cluster_prefix(s: &str, part: &str) -> bool {
        if part.is_empty() {
            return true;
        }
        let mut acc = String::new();
        for g in graphemes(s) {
            acc.push_str(g);
            if acc == part {
                return true;
            }
        }
        false
    }

    fn is_cluster_suffix(s: &str, part: &str) -> bool {
        if part.is_empty() {
            return true;
        }
        let mut acc = String::new();
        for g in graphemes(s).rev() {
            acc.insert_str(0, g);
            if acc == part {
                return true;
            }
        }
        false
    }

    #[test]
    fn truncation_never_splits_a_grapheme_cluster() {
        // Base + variation selector, a ZWJ sequence, and a combining mark.
        for s in [
            "\u{a9}\u{fe0f}x",
            "\u{1f469}\u{200d}\u{1f4bb}!",
            "e\u{301}xample",
            "\u{8a3a}\u{65ad}x",
        ] {
            for max_w in 0..=str_w(s) + 2 {
                let out = ellipsize(s, max_w);
                assert!(
                    out.width() <= max_w,
                    "ellipsize({s:?}, {max_w}) -> {out:?} is {} wide",
                    out.width()
                );
                assert!(
                    is_cluster_prefix(s, out.trim_end_matches('\u{2026}')),
                    "ellipsize({s:?}, {max_w}) split a cluster: {out:?}"
                );

                let out = with_more_marker(s, max_w);
                assert!(out.width() <= max_w, "with_more_marker -> {out:?}");
                assert!(
                    is_cluster_prefix(s, out.trim_end_matches('\u{2026}')),
                    "with_more_marker({s:?}, {max_w}) split a cluster: {out:?}"
                );

                let out = trim_left_to(s, max_w);
                assert!(out.width() <= max_w, "trim_left_to -> {out:?}");
                assert!(
                    is_cluster_suffix(s, out.trim_start_matches('\u{2026}')),
                    "trim_left_to({s:?}, {max_w}) split a cluster: {out:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_display_never_splits_a_grapheme_cluster() {
        let s = "\u{1f469}\u{200d}\u{1f4bb}\u{1f469}\u{200d}\u{1f4bb}\u{1f469}\u{200d}\u{1f4bb}";
        for width in 2..=12usize {
            let rows = wrap_display(s, width);
            assert_eq!(rows.concat(), s, "w={width} lost content");
            for row in &rows {
                assert!(row.width() <= width, "w={width} row={row:?}");
                assert!(
                    graphemes(row).all(|g| graphemes(s).any(|o| o == g)),
                    "w={width} produced a partial cluster: {rows:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_display_treats_newlines_as_hard_breaks() {
        assert_eq!(wrap_display("first\nsecond", 40), vec!["first", "second"]);
        assert_eq!(wrap_display("first\r\nsecond", 40), vec!["first", "second"]);
        assert_eq!(wrap_display("first\rsecond", 40), vec!["first", "second"]);
        assert_eq!(wrap_display("a\n\nb", 40), vec!["a", "", "b"]);
        assert_eq!(wrap_display("a\tb", 40), vec!["a b"]);
        // A Line renders as one row and ratatui drops control characters, so
        // none may survive into a produced row.
        for row in wrap_display("one\ttwo\r\nthree\nfour and more words", 6) {
            assert!(!row.chars().any(char::is_control), "{row:?}");
            assert!(row.width() <= 6, "{row:?}");
        }
    }

    #[test]
    fn wrap_display_strips_other_control_characters() {
        // NEL / LS / PS are line terminators too.
        assert_eq!(wrap_display("a\u{85}b", 40), vec!["a", "b"]);
        assert_eq!(wrap_display("a\u{2028}b", 40), vec!["a", "b"]);
        assert_eq!(wrap_display("a\u{2029}b", 40), vec!["a", "b"]);
        // Everything else is dropped here rather than by the renderer, so the
        // wrapped width matches the row that actually gets painted.
        assert_eq!(wrap_display("a\u{1b}[31mb", 40), vec!["a[31mb"]);
        assert_eq!(wrap_display("a\u{0}\u{7}\u{b}\u{c}b", 40), vec!["ab"]);
        for row in wrap_display("\u{1b}]0;title\u{7}text", 40) {
            assert!(!row.chars().any(char::is_control), "{row:?}");
        }
    }

    #[test]
    fn detail_message_with_newlines_becomes_separate_rows() {
        let d = diag(Severity::Error, "line one\nline two\nline three");
        assert_eq!(
            detail_msg_parts(&d, 200),
            vec!["line one", "line two", "line three"]
        );
        // ...and the extra rows are counted, so the box is sized for them.
        assert!(
            box_height(Some(&d), 200) >= box_height(Some(&diag(Severity::Error, "line one")), 200)
        );
    }

    #[test]
    fn carets_align_and_size_by_display_columns() {
        // A wide char BEFORE the span must not push the carets left.
        let mut d = diag(Severity::Error, "wide char before the span");
        d.source_line = Some("  \u{8a3a}x = 1;".into());
        d.col = 4; // 1-based char column of `x`
        d.end_col = 5;
        let rows = detail_rows(&d, 200);
        let (src_row, caret_row) = (&rows[2], &rows[3]);
        let span_col = str_w(&src_row[..src_row.find('x').expect("span char")]);
        let caret_col = str_w(&caret_row[..caret_row.find('^').expect("caret")]);
        assert_eq!(
            caret_col, span_col,
            "caret misaligned\n  src={src_row:?}\ncaret={caret_row:?}"
        );

        // A wide char UNDER the span needs a caret per column it occupies.
        let mut w = diag(Severity::Error, "wide span");
        w.source_line = Some("  \u{8a3a} = 1;".into());
        w.col = 3;
        w.end_col = 4;
        let rows = detail_rows(&w, 200);
        assert_eq!(
            rows[3].matches('^').count(),
            2,
            "a 2-column char needs 2 carets: {:?}",
            rows[3]
        );
    }

    #[test]
    fn no_control_character_ever_reaches_a_span() {
        // An ESC/OSC sequence smuggled in through a source string literal, a
        // filename, or a checker message must not reach a Span: the renderer
        // would drop it and slide the rest of the row out of position.
        let nasty = "a\u{1b}]0;pwned\u{7}b\u{9b}c\u{0}\td\ne";
        let mut d = diag(Severity::Error, nasty);
        d.path = std::path::PathBuf::from(format!("src/{nasty}.as"));
        d.source_line = Some(format!("    vec4 t = F(\"{nasty}\");"));

        let mut snap = canned_snapshot();
        snap.root_label = nasty.into();
        snap.diagnostics = vec![d.clone()];
        snap.status = RunStatus::Failed {
            message: nasty.into(),
        };

        let spans_of = |lines: Vec<Line<'static>>| -> Vec<String> {
            lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string()))
                .collect()
        };
        let mut all: Vec<String> = spans_of(pretty_detail_lines(&d, 60));
        all.extend(spans_of(pretty_detail_lines(&d, 20)));
        for density in [ListDensity::Compact, ListDensity::Relaxed] {
            for selected in [true, false] {
                // `list_item_for` returns a ListItem, so exercise its inputs
                // through the same helpers it uses.
                all.push(ellipsize(&sanitize_row(&d.message), 30));
                all.extend(relaxed_msg_rows(&sanitize_row(&d.message), 40, selected));
                all.push(format_location(&d, 24));
                let _ = density;
            }
        }
        if let Some(frag) = code_fragment(&d) {
            all.push(frag);
        }
        all.push(sanitize_row(&snap.root_label));
        all.push(sanitize_row(&snap.status.label()));

        for text in &all {
            assert!(
                !text.chars().any(char::is_control),
                "control character reached a span: {text:?}"
            );
        }
        // The visible text still survives sanitisation.
        assert!(
            all.iter().any(|t| t.contains('a') && t.contains('b')),
            "sanitising ate the message: {all:?}"
        );
    }

    #[test]
    fn sanitize_keep_columns_preserves_char_offsets() {
        for s in [
            "",
            "plain",
            "a\u{1b}b\u{0}c",
            "\u{8a3a}\u{7}x",
            "e\u{301}\u{1b}",
        ] {
            let out = sanitize_keep_columns(s);
            assert_eq!(
                out.chars().count(),
                s.chars().count(),
                "offset shift on {s:?} -> {out:?}"
            );
            assert!(!out.chars().any(char::is_control), "{out:?}");
        }
    }

    #[test]
    fn relaxed_page_step_allows_for_the_spilled_row() {
        let mut app = App::from_snapshot(canned_snapshot());
        app.set_list_inner_h_for_test(14); // 12 inner rows
        let compact = app.page_step();
        app.toggle_density();
        let relaxed = app.page_step();
        // Compact: 12 rows / 1 = 12 visible -> step 11.
        assert_eq!(compact, 11);
        // Relaxed: one item may be 3 rows, so budget for it: (12-1)/2 = 5 -> 4.
        assert_eq!(relaxed, 4);
        assert!(
            relaxed * 2 < 12,
            "a page must never outrun the viewport: {relaxed}"
        );
    }

    #[test]
    fn carets_sit_under_whole_grapheme_clusters() {
        // A range selecting only the combining mark must caret the glyph it
        // belongs to, not the gap after the base char.
        let mut d = diag(Severity::Error, "combining-mark-only span");
        d.source_line = Some("e\u{301}x = 1;".into());
        d.col = 2; // 1-based char column of U+0301
        d.end_col = 3;
        let rows = detail_rows(&d, 200);
        let caret_col = str_w(&rows[3][..rows[3].find('^').expect("caret")]);
        let glyph_col = str_w(&rows[2][..rows[2].find('e').expect("glyph")]);
        assert_eq!(
            caret_col, glyph_col,
            "caret must land on the rendered glyph\n  src={:?}\ncaret={:?}",
            rows[2], rows[3]
        );

        // Half of a ZWJ sequence: the whole emoji gets caretted.
        let mut z = diag(Severity::Error, "zwj-internal span");
        z.source_line = Some("\u{1f469}\u{200d}\u{1f4bb}x".into());
        z.col = 2; // the ZWJ itself
        z.end_col = 3;
        let rows = detail_rows(&z, 200);
        assert_eq!(
            str_w(&rows[3][..rows[3].find('^').expect("caret")]),
            str_w(&rows[2][..rows[2].find('\u{1f469}').expect("glyph")]),
            "caret must start at the emoji, not inside it: {:?}",
            rows[3]
        );
    }

    #[test]
    fn snap_to_clusters_widens_only_partial_ranges() {
        // ASCII is one char per cluster: nothing moves.
        assert_eq!(snap_to_clusters("abcdef", 2, 4), (2, 4));
        // "e\u{301}" is one cluster spanning chars 0..2.
        assert_eq!(snap_to_clusters("e\u{301}x", 1, 2), (0, 2));
        assert_eq!(snap_to_clusters("e\u{301}x", 0, 2), (0, 2));
        assert_eq!(snap_to_clusters("e\u{301}x", 2, 3), (2, 3));
        // Past the end of the line: left alone, but never empty.
        assert_eq!(snap_to_clusters("ab", 2, 3), (2, 3));
        // The result always contains the input range.
        for (lo, hi) in [(0, 1), (1, 2), (0, 3), (2, 3), (1, 3)] {
            let (a, b) = snap_to_clusters("e\u{301}x", lo, hi);
            assert!(a <= lo && b >= hi, "({lo},{hi}) -> ({a},{b})");
            assert!(a < b, "({lo},{hi}) -> empty range ({a},{b})");
        }
    }

    #[test]
    fn scroll_hint_title_degrades_but_never_overflows() {
        for inner_w in 0..=40usize {
            let hint = scroll_hint_title(inner_w);
            assert!(
                str_w(&hint) <= inner_w,
                "w={inner_w} hint={hint:?} is {} wide",
                str_w(&hint)
            );
            // Anything that can show a hint at all shows one.
            if inner_w >= 1 {
                assert!(!hint.is_empty(), "w={inner_w}: gave up on the hint");
            }
            if inner_w >= 3 {
                assert!(hint.contains("J/K"), "w={inner_w} hint={hint:?}");
            }
        }
        assert_eq!(scroll_hint_title(0), "");
    }

    #[test]
    fn detail_scroll_clears_when_the_list_empties() {
        let mut app = App::from_snapshot(canned_snapshot());
        app.scroll_detail_down();
        app.scroll_detail_down();
        assert_eq!(app.detail_scroll(), 2);
        app.apply_snapshot(Snapshot::empty("./MyPlugin"));
        assert_eq!(app.selected(), None);
        assert_eq!(
            app.detail_scroll(),
            0,
            "an empty refresh must not leave stale scroll state"
        );
        // Same for the explicit selection clears.
        app.apply_snapshot(canned_snapshot());
        app.scroll_detail_down();
        app.apply_snapshot(Snapshot::empty("./MyPlugin"));
        app.select_first();
        assert_eq!(app.detail_scroll(), 0);
        app.select_last();
        assert_eq!(app.detail_scroll(), 0);
    }

    #[test]
    fn detail_scroll_survives_boundary_navigation() {
        let mut app = App::from_snapshot(canned_snapshot());
        app.scroll_detail_down();
        app.scroll_detail_down();
        assert_eq!(app.detail_scroll(), 2);

        // Already at the top: the selection does not change, so neither does
        // the detail box — the reader must keep their place.
        app.scroll_up();
        assert_eq!(app.detail_scroll(), 2, "k at the top reset the scroll");
        app.page_up(10);
        assert_eq!(app.detail_scroll(), 2, "PgUp at the top reset the scroll");
        app.select_first();
        assert_eq!(app.detail_scroll(), 2, "g at the top reset the scroll");

        // A real selection change does reset it.
        app.select_last();
        assert_eq!(app.detail_scroll(), 0);

        // Symmetric at the bottom.
        app.scroll_detail_down();
        app.scroll_down();
        assert_eq!(app.detail_scroll(), 1, "j at the end reset the scroll");
        app.page_down(10);
        assert_eq!(app.detail_scroll(), 1, "PgDn at the end reset the scroll");
        app.select_last();
        assert_eq!(app.detail_scroll(), 1, "G at the end reset the scroll");
    }

    #[test]
    fn detail_scroll_resets_with_the_selection() {
        let mut app = App::from_snapshot(canned_snapshot());
        assert_eq!(app.detail_scroll(), 0);
        app.scroll_detail_down();
        app.scroll_detail_down();
        assert_eq!(app.detail_scroll(), 2);
        app.scroll_down();
        assert_eq!(
            app.detail_scroll(),
            0,
            "moving the list resets detail scroll"
        );
        app.scroll_detail_up();
        assert_eq!(app.detail_scroll(), 0, "saturates at the top");
        app.scroll_detail_down();
        app.toggle_density();
        assert_eq!(
            app.detail_scroll(),
            1,
            "density is a list concern; the detail box is unchanged"
        );
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
