//! Interactive editor for a workspace or user-global config file.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

use crate::user_config::{
    format_nested_list, format_string_list, parse_nested_list, parse_string_list, SettingKind,
    SettingSpec, UserConfigFile, SETTINGS,
};

/// Interactive config-file editor.
pub struct ConfigApp {
    file: UserConfigFile,
    list_state: ListState,
    mode: Mode,
    status: String,
    pub should_quit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    List,
    Editing { buffer: String },
}

impl ConfigApp {
    pub fn new(file: UserConfigFile) -> Self {
        let mut list_state = ListState::default();
        if !SETTINGS.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            file,
            list_state,
            mode: Mode::List,
            status: String::new(),
            should_quit: false,
        }
    }

    pub fn path(&self) -> &std::path::Path {
        self.file.path()
    }

    pub fn cursor(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn selected_spec(&self) -> &'static SettingSpec {
        &SETTINGS[self.cursor().min(SETTINGS.len().saturating_sub(1))]
    }

    pub fn display_value(&self, spec: &SettingSpec) -> (String, bool) {
        match spec.kind {
            SettingKind::Bool { default } => match self.file.get_bool(spec.key) {
                Some(v) => (v.to_string(), true),
                None => (format!("{default} (default)"), false),
            },
            SettingKind::Enum { default, .. } => match self.file.get_string(spec.key) {
                Some(v) => (v, true),
                None => (format!("{default} (default)"), false),
            },
            SettingKind::String | SettingKind::Path => match self.file.get_string(spec.key) {
                Some(v) => (v, true),
                None => ("(unset)".into(), false),
            },
            SettingKind::StringList { .. } => match self.file.get_string_list(spec.key) {
                Some(v) if v.is_empty() => ("[]".into(), true),
                Some(v) => (format_string_list(&v), true),
                None => ("(unset)".into(), false),
            },
            SettingKind::NestedStringList => match self.file.get_nested_string_list(spec.key) {
                Some(v) if v.is_empty() => ("(none)".into(), true),
                Some(v) => (format_nested_list(&v), true),
                None => ("(unset)".into(), false),
            },
            SettingKind::Usize { default } => match self.file.get_usize(spec.key) {
                Some(v) => (v.to_string(), true),
                None => (format!("{default} (default)"), false),
            },
        }
    }

    pub fn draw(&mut self, f: &mut Frame<'_>) {
        // Footer: help · value hint · keys. The hint line is always present
        // (B024) and wraps when the accepted values are too wide to fit, so
        // nothing about the value format is silently cut off.
        let hint_lines = wrap_words(&self.value_hint(), footer_text_width(f.area().width));
        // Safety valve for very narrow terminals: the footer must never grow
        // until it crowds the settings list off the screen.
        let hint_lines = &hint_lines[..hint_lines.len().min(MAX_HINT_LINES)];
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(4 + hint_lines.len() as u16),
            ])
            .split(f.area());

        self.draw_header(f, chunks[0]);
        self.draw_list(f, chunks[1]);
        self.draw_footer(f, chunks[2], hint_lines);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(Span::styled(
                header_title(self.file.path()),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        f.render_widget(
            Paragraph::new(format!(" {} ", self.file.path().display())).block(block),
            area,
        );
    }

    fn draw_list(&mut self, f: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem> = SETTINGS
            .iter()
            .map(|spec| {
                let (value, set) = self.display_value(spec);
                let value_style = if set {
                    Style::default()
                } else {
                    Style::default().add_modifier(Modifier::DIM)
                };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<38}", spec.key)),
                    Span::styled(value, value_style),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" settings "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    /// How the selected setting's value is typed in. Rendered on its own
    /// footer line for every row: the accepted values are otherwise
    /// invisible — an enum never shows its other choices, and `defines`
    /// gave no clue that the editor splits what you type on commas (B024).
    pub fn value_hint(&self) -> String {
        match self.selected_spec().kind {
            SettingKind::Bool { default } => format!("true or false · default {default}"),
            SettingKind::Enum { choices, default } => {
                format!("one of: {} · default {default}", choices.join(", "))
            }
            SettingKind::String => "free text".to_string(),
            SettingKind::Path => "filesystem path · e.g. ~/OpenplanetNext".to_string(),
            SettingKind::StringList { example } => {
                format!("comma-separated · e.g. {example}")
            }
            SettingKind::NestedStringList => {
                "comma-separated groups · e.g. [A, B] | [C, D]".to_string()
            }
            SettingKind::Usize { default } => {
                format!("whole number · default {default}")
            }
        }
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect, hint_lines: &[String]) {
        let spec = self.selected_spec();
        let help = match &self.mode {
            Mode::Editing { buffer } => format!("edit {}: {buffer}_", spec.key),
            Mode::List => spec.help.to_string(),
        };
        let keys = match self.mode {
            Mode::Editing { .. } => "enter save   esc cancel",
            Mode::List => "↑/k ↓/j  enter/space edit  ←/→ cycle  d unset  q quit",
        };
        let status = if self.status.is_empty() {
            String::new()
        } else {
            format!(" · {}", self.status)
        };
        let hint = hint_lines
            .iter()
            .map(|line| format!(" {line}\n"))
            .collect::<String>();
        let text = format!(" {help}\n{hint} {keys}{status} ");
        f.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn persist(&mut self) {
        match self.file.save() {
            Ok(()) => self.status = "saved".into(),
            Err(err) => self.status = format!("save failed: {err}"),
        }
    }

    fn select(&mut self, idx: usize) {
        if SETTINGS.is_empty() {
            return;
        }
        self.list_state.select(Some(idx.min(SETTINGS.len() - 1)));
    }

    fn apply_bool(&mut self, value: bool) {
        let key = self.selected_spec().key;
        self.file.set_bool(key, value);
        self.persist();
    }

    fn apply_string(&mut self, value: &str) {
        let key = self.selected_spec().key;
        self.file.set_string(key, value);
        self.persist();
    }

    fn apply_usize(&mut self, value: usize) {
        let key = self.selected_spec().key;
        self.file.set_usize(key, value);
        self.persist();
    }

    fn apply_string_list(&mut self, values: &[String]) {
        let key = self.selected_spec().key;
        self.file.set_string_list(key, values);
        self.persist();
    }

    fn apply_nested_list(&mut self, values: &[Vec<String>]) {
        let key = self.selected_spec().key;
        self.file.set_nested_string_list(key, values);
        self.persist();
    }

    fn cycle_enum(&mut self, delta: isize) {
        let spec = self.selected_spec();
        let SettingKind::Enum { choices, default } = spec.kind else {
            return;
        };
        let current = self
            .file
            .get_string(spec.key)
            .unwrap_or_else(|| default.to_string());
        let idx = choices
            .iter()
            .position(|c| *c == current.as_str())
            .unwrap_or(0);
        let len = choices.len() as isize;
        let next = (idx as isize + delta).rem_euclid(len) as usize;
        self.apply_string(choices[next]);
    }

    fn toggle_or_edit(&mut self) {
        let spec = self.selected_spec();
        match spec.kind {
            SettingKind::Bool { default } => {
                let next = !self.file.get_bool(spec.key).unwrap_or(default);
                self.apply_bool(next);
            }
            SettingKind::Enum { .. } => self.cycle_enum(1),
            SettingKind::Usize { default } => {
                let shown = self
                    .file
                    .get_usize(spec.key)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| default.to_string());
                self.mode = Mode::Editing { buffer: shown };
            }
            SettingKind::String | SettingKind::Path => {
                let buffer = self.file.get_string(spec.key).unwrap_or_default();
                self.mode = Mode::Editing { buffer };
            }
            SettingKind::StringList { .. } => {
                let buffer = self
                    .file
                    .get_string_list(spec.key)
                    .map(|v| format_string_list(&v))
                    .unwrap_or_default();
                self.mode = Mode::Editing { buffer };
            }
            SettingKind::NestedStringList => {
                let buffer = self
                    .file
                    .get_nested_string_list(spec.key)
                    .map(|v| format_nested_list(&v))
                    .unwrap_or_default();
                self.mode = Mode::Editing { buffer };
            }
        }
    }

    fn commit_edit(&mut self) {
        let Mode::Editing { buffer } = self.mode.clone() else {
            return;
        };
        let spec = self.selected_spec();
        match spec.kind {
            SettingKind::String | SettingKind::Path | SettingKind::Enum { .. } => {
                if buffer.trim().is_empty() {
                    self.file.unset(spec.key);
                    self.persist();
                } else {
                    self.apply_string(buffer.trim());
                }
            }
            SettingKind::Usize { .. } => match buffer.trim().parse::<usize>() {
                Ok(n) => self.apply_usize(n),
                Err(_) => {
                    self.status = "save failed: expected a non-negative integer".into();
                    return;
                }
            },
            SettingKind::StringList { .. } => {
                let values = parse_string_list(&buffer);
                if values.is_empty() && !persist_empty_array(spec.key) {
                    self.file.unset(spec.key);
                    self.persist();
                } else {
                    self.apply_string_list(&values);
                }
            }
            SettingKind::NestedStringList => match parse_nested_list(&buffer) {
                Ok(groups) if groups.is_empty() && !persist_empty_array(spec.key) => {
                    self.file.unset(spec.key);
                    self.persist();
                }
                Ok(groups) => self.apply_nested_list(&groups),
                Err(err) => {
                    self.status = format!("save failed: {err}");
                    return;
                }
            },
            SettingKind::Bool { .. } => {}
        }
        self.mode = Mode::List;
    }
}

/// Most footer lines the value hint may occupy, however narrow the terminal.
const MAX_HINT_LINES: usize = 3;

/// Usable text width inside the footer block (two border columns plus the
/// one-space left margin every footer line carries).
fn footer_text_width(area_width: u16) -> usize {
    (area_width as usize).saturating_sub(3).max(8)
}

/// Word-wrap `text` to `width`, never splitting a word that fits on a line of
/// its own. Always returns at least one line.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    lines.push(current);
    lines
}

fn header_title(path: &std::path::Path) -> &'static str {
    if crate::config_watch::is_workspace_config_path(path) {
        " openplanet-lsp config · workspace "
    } else if crate::config_watch::is_user_config_path(path) {
        " openplanet-lsp config · global "
    } else {
        " openplanet-lsp config "
    }
}

/// `[]` is distinct from unset for these keys (`LspConfig` treats `Some([])`
/// differently from a missing key).
fn persist_empty_array(key: &str) -> bool {
    matches!(key, "define_combinations" | "shortcut_namespaces")
}

/// Map a key to a config-TUI action. Mutates + persists when a setting changes.
pub fn handle_key(app: &mut ConfigApp, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        app.should_quit = true;
        return;
    }

    if matches!(app.mode, Mode::Editing { .. }) {
        match key.code {
            KeyCode::Esc => {
                app.mode = Mode::List;
                app.status.clear();
            }
            KeyCode::Enter => app.commit_edit(),
            KeyCode::Backspace => {
                if let Mode::Editing { buffer } = &mut app.mode {
                    buffer.pop();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Editing { buffer } = &mut app.mode {
                    buffer.push(c);
                }
            }
            _ => {}
        }
        return;
    }

    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select(app.cursor().saturating_add(1)),
        KeyCode::Char('k') | KeyCode::Up => app.select(app.cursor().saturating_sub(1)),
        KeyCode::Home | KeyCode::Char('g') => app.select(0),
        KeyCode::End | KeyCode::Char('G') => app.select(SETTINGS.len().saturating_sub(1)),
        KeyCode::Left => app.cycle_enum(-1),
        KeyCode::Right => app.cycle_enum(1),
        KeyCode::Enter | KeyCode::Char(' ') => app.toggle_or_edit(),
        KeyCode::Char('d') => {
            let key = app.selected_spec().key.to_string();
            app.file.unset(&key);
            app.persist();
        }
        _ => {}
    }
}

/// Run the config TUI on the real terminal until quit.
pub fn run(path: PathBuf) -> io::Result<()> {
    let file = UserConfigFile::load(&path).map_err(|err| io::Error::other(err.to_string()))?;
    let mut terminal = ratatui::init();
    let result = run_with_backend(&mut terminal, file);
    ratatui::restore();
    result
}

/// Run against any backend (real or [`ratatui::backend::TestBackend`]).
pub fn run_with_backend<B: Backend>(
    terminal: &mut Terminal<B>,
    file: UserConfigFile,
) -> io::Result<()> {
    let mut app = ConfigApp::new(file);
    loop {
        terminal
            .draw(|f| app.draw(f))
            .map_err(|err| io::Error::other(err.to_string()))?;
        if app.should_quit {
            break;
        }
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                handle_key(&mut app, key);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_config::UserConfigFile;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// The guard comes back with the app: `ConfigApp` writes to the config
    /// file, so the directory has to outlive it.
    fn temp_app(name: &str) -> (crate::testing::TempDir, ConfigApp) {
        let dir = crate::testing::TempDir::new(&format!("opl-config-tui-{name}"));
        let path = dir.join("config.toml");
        let app = ConfigApp::new(UserConfigFile::load(&path).unwrap());
        (dir, app)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn frame_size(app: &mut ConfigApp, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        format!("{:?}", terminal.backend())
    }

    #[test]
    fn header_labels_workspace_and_global_paths() {
        let dir = crate::testing::TempDir::new("opl-config-tui-scope");

        let ws = dir.join(".openplanet-lsp.toml");
        let mut app = ConfigApp::new(UserConfigFile::load(&ws).unwrap());
        let out = frame_size(&mut app, 80, 24);
        assert!(
            out.contains("workspace"),
            "workspace file should be labeled in the header:\n{out}"
        );
        assert!(
            !out.contains("global"),
            "workspace file must not also say global:\n{out}"
        );

        let global = dir.join(".config/openplanet-lsp/config.toml");
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        let mut app = ConfigApp::new(UserConfigFile::load(&global).unwrap());
        let out = frame_size(&mut app, 80, 24);
        assert!(
            out.contains("global"),
            "user-global file should be labeled in the header:\n{out}"
        );
        assert!(
            !out.contains("workspace"),
            "global file must not also say workspace:\n{out}"
        );
    }

    #[test]
    fn header_custom_path_has_no_scope_label() {
        let dir = crate::testing::TempDir::new("opl-config-tui-custom");
        let path = dir.join("alt.toml");
        let mut app = ConfigApp::new(UserConfigFile::load(&path).unwrap());
        let out = frame_size(&mut app, 80, 24);
        assert!(
            out.contains("openplanet-lsp config"),
            "title missing:\n{out}"
        );
        assert!(
            !out.contains("workspace") && !out.contains("global"),
            "--path custom file should not claim workspace/global:\n{out}"
        );
    }

    #[test]
    fn render_lists_every_persisted_setting() {
        let (_dir, mut app) = temp_app("render");
        // Tall enough for the WHOLE catalog plus chrome, derived from
        // SETTINGS so adding a key can never silently clip this test into
        // asserting on a scrolled-off view (a hardcoded 32 went stale the
        // moment B143 added `plugin_files_search_paths`).
        let height = SETTINGS.len() as u16 + 12;
        let out = frame_size(&mut app, 80, height);
        let _ = std::fs::write("/tmp/opl-config-tui-frame.txt", &out);
        for spec in SETTINGS {
            assert!(
                out.contains(spec.key),
                "missing {} in frame:\n{out}",
                spec.key
            );
        }
        assert!(
            out.contains("openplanet-lsp config"),
            "title missing:\n{out}"
        );
    }

    /// B143: the new key is reachable by the spec-driven navigation and its
    /// list editor persists a comma-separated value like every other
    /// `StringList` key — no special-casing anywhere in this TUI.
    #[test]
    fn plugin_files_search_paths_is_navigable_and_editable() {
        let (dir, mut app) = temp_app("b143-key");
        let mut guard = 0;
        while app.selected_spec().key != "plugin_files_search_paths" {
            handle_key(&mut app, press(KeyCode::Char('j')));
            guard += 1;
            assert!(
                guard < SETTINGS.len() * 2,
                "key never reached by navigation"
            );
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for ch in "src, Source".chars() {
            handle_key(&mut app, press(KeyCode::Char(ch)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        let written = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(
            written.contains("plugin_files_search_paths")
                && written.contains("src")
                && written.contains("Source"),
            "expected the list to persist:\n{written}"
        );
    }

    #[test]
    fn scrolling_to_end_reveals_clipped_settings() {
        let (_dir, mut app) = temp_app("scroll-end");
        handle_key(&mut app, press(KeyCode::Char('G')));
        let out = frame_size(&mut app, 80, 24);
        assert!(
            out.contains("define_combinations"),
            "end of list missing after G:\n{out}"
        );
        assert!(
            out.contains("define_matrix_max_combinations"),
            "last setting missing after G:\n{out}"
        );
    }

    #[test]
    fn space_toggles_bool_and_writes_file() {
        let (_dir, mut app) = temp_app("toggle");
        // Navigate by key: the catalog's order changes as settings land.
        while app.selected_spec().key != "warnings_as_errors" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(app.status(), "saved");
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("warnings_as_errors = true"),
            "file not updated:\n{written}"
        );
        handle_key(&mut app, press(KeyCode::Char(' ')));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("warnings_as_errors = false"),
            "toggle back failed:\n{written}"
        );
    }

    #[test]
    fn space_toggles_bool_default_true_variant() {
        let (_dir, mut app) = temp_app("toggle-true");
        while app.selected_spec().key != "honor_gitignore" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Char(' ')));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("honor_gitignore = false"),
            "default-true toggle should persist false:\n{written}"
        );
    }

    #[test]
    fn cycle_enum_writes_each_choice() {
        let (_dir, mut app) = temp_app("cycle");
        assert_eq!(app.selected_spec().key, "default_mode");
        handle_key(&mut app, press(KeyCode::Right));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("default_mode = \"lsp\""),
            "cycle right:\n{written}"
        );
        handle_key(&mut app, press(KeyCode::Left));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("default_mode = \"tui\""),
            "cycle left:\n{written}"
        );
    }

    #[test]
    fn edit_path_and_string_variants_commit() {
        let (_dir, mut app) = temp_app("path-string");
        while app.selected_spec().key != "openplanet_dir" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "/tmp/OpenplanetNext".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));

        while app.selected_spec().key != "plugins_dir" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "/tmp/Plugins".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));

        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("openplanet_dir") && written.contains("/tmp/OpenplanetNext"),
            "path not written:\n{written}"
        );
        assert!(
            written.contains("plugins_dir") && written.contains("/tmp/Plugins"),
            "second path not written:\n{written}"
        );
    }

    #[test]
    fn edit_string_list_commits_on_enter() {
        let (_dir, mut app) = temp_app("list-edit");
        while app.selected_spec().key != "defines" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "DEV, DEPENDENCY_MLHOOK".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.status(), "saved");
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("DEV") && written.contains("DEPENDENCY_MLHOOK"),
            "list not written:\n{written}"
        );
    }

    #[test]
    fn edit_nested_list_and_usize_variants() {
        let (_dir, mut app) = temp_app("nested-usize");
        while app.selected_spec().key != "define_groups" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "[A, B] | [C]".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.status(), "saved");

        while app.selected_spec().key != "define_matrix_max_combinations" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        handle_key(&mut app, press(KeyCode::Backspace));
        handle_key(&mut app, press(KeyCode::Backspace));
        handle_key(&mut app, press(KeyCode::Char('3')));
        handle_key(&mut app, press(KeyCode::Char('2')));
        handle_key(&mut app, press(KeyCode::Enter));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("define_groups") && written.contains("define_matrix_max_combinations"),
            "nested/usize missing:\n{written}"
        );
        assert!(written.contains("32"), "usize 32 not written:\n{written}");
    }

    #[test]
    fn empty_source_paths_edit_unsets_instead_of_writing_empty_array() {
        let (_dir, mut app) = temp_app("empty-source-paths");
        while app.selected_spec().key != "source_paths" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "src".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        handle_key(&mut app, press(KeyCode::Enter));
        for _ in 0..3 {
            handle_key(&mut app, press(KeyCode::Backspace));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            !written.contains("source_paths"),
            "empty source_paths must unset, not write []:\n{written}"
        );
    }

    #[test]
    fn empty_define_combinations_persists_empty_array() {
        let (_dir, mut app) = temp_app("empty-combos");
        while app.selected_spec().key != "define_combinations" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        handle_key(&mut app, press(KeyCode::Enter));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("define_combinations") && written.contains("[]"),
            "empty pin must persist []:\n{written}"
        );
        let reloaded = UserConfigFile::load(app.path()).unwrap();
        assert_eq!(
            reloaded.get_nested_string_list("define_combinations"),
            Some(Vec::new())
        );
        let (shown, set) = app.display_value(app.selected_spec());
        assert!(set, "empty pin is set, not default");
        assert_eq!(shown, "(none)");
    }

    #[test]
    fn empty_shortcut_namespaces_persists_empty_array() {
        let (_dir, mut app) = temp_app("empty-shortcuts");
        while app.selected_spec().key != "shortcut_namespaces" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        handle_key(&mut app, press(KeyCode::Enter));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("shortcut_namespaces") && written.contains("[]"),
            "empty shortcut_namespaces must persist []:\n{written}"
        );
        assert_eq!(
            UserConfigFile::load(app.path())
                .unwrap()
                .get_string_list("shortcut_namespaces"),
            Some(Vec::new())
        );
    }

    #[test]
    fn invalid_usize_does_not_write() {
        let (_dir, mut app) = temp_app("bad-usize");
        while app.selected_spec().key != "define_matrix_max_combinations" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        handle_key(&mut app, press(KeyCode::Backspace));
        handle_key(&mut app, press(KeyCode::Backspace));
        for c in "nope".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(
            app.status().starts_with("save failed"),
            "expected parse error, got {}",
            app.status()
        );
        assert!(
            !app.path().exists()
                || !std::fs::read_to_string(app.path())
                    .unwrap()
                    .contains("define_matrix_max_combinations"),
            "invalid usize must not persist"
        );
    }

    #[test]
    fn empty_list_edit_unsets_instead_of_writing_empty_array() {
        let (_dir, mut app) = temp_app("empty-list");
        while app.selected_spec().key != "defines" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "DEV".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(std::fs::read_to_string(app.path()).unwrap().contains("DEV"));

        handle_key(&mut app, press(KeyCode::Enter));
        handle_key(&mut app, press(KeyCode::Backspace));
        handle_key(&mut app, press(KeyCode::Backspace));
        handle_key(&mut app, press(KeyCode::Backspace));
        handle_key(&mut app, press(KeyCode::Enter));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            !written.contains("defines"),
            "empty list edit must unset, not write []:\n{written}"
        );
    }

    #[test]
    fn invalid_nested_list_does_not_write() {
        let (_dir, mut app) = temp_app("bad-nested");
        while app.selected_spec().key != "define_groups" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "not-brackets".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        assert!(
            app.status().starts_with("save failed"),
            "expected parse error, got {}",
            app.status()
        );
        assert!(
            !app.path().exists()
                || !std::fs::read_to_string(app.path())
                    .unwrap()
                    .contains("define_groups"),
            "invalid edit must not persist"
        );
    }

    #[test]
    fn d_unsets_key() {
        let (_dir, mut app) = temp_app("unset");
        handle_key(&mut app, press(KeyCode::Right));
        assert!(std::fs::read_to_string(app.path())
            .unwrap()
            .contains("default_mode"));
        handle_key(&mut app, press(KeyCode::Char('d')));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            !written.contains("default_mode"),
            "unset left the key:\n{written}"
        );
    }

    /// B024: the `defines` row must say how the editor parses what you type
    /// (comma-separated) and show a concrete example — "DEV" alone gave no
    /// clue whether the field wanted commas, spaces or TOML array syntax.
    #[test]
    fn string_list_row_shows_separator_hint_and_example() {
        let (_dir, mut app) = temp_app("list-hint");
        while app.selected_spec().key != "defines" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        let out = frame_size(&mut app, 80, 30);
        assert!(
            out.contains("comma-separated"),
            "no separator hint for defines:\n{out}"
        );
        assert!(out.contains("e.g. DEV"), "no example for defines:\n{out}");
    }

    /// The hint stays visible while the value is being typed.
    #[test]
    fn string_list_hint_visible_while_editing() {
        let (_dir, mut app) = temp_app("list-hint-edit");
        while app.selected_spec().key != "defines" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        handle_key(&mut app, press(KeyCode::Enter));
        for c in "DEV".chars() {
            handle_key(&mut app, press(KeyCode::Char(c)));
        }
        let out = frame_size(&mut app, 80, 30);
        assert!(
            out.contains("comma-separated") && out.contains("e.g. DEV"),
            "hint lost while editing:\n{out}"
        );
        assert!(out.contains("edit defines"), "edit prompt missing:\n{out}");
    }

    /// Nested lists get their own grouping hint (they are NOT plain commas).
    #[test]
    fn nested_list_row_shows_group_hint() {
        let (_dir, mut app) = temp_app("nested-hint");
        while app.selected_spec().key != "define_groups" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        let out = frame_size(&mut app, 80, 30);
        assert!(
            out.contains("[A, B] | [C, D]"),
            "no group-syntax hint:\n{out}"
        );
    }

    /// Every row explains how its value is typed in — the choices for an
    /// enum are otherwise invisible until you cycle through them.
    #[test]
    fn every_setting_row_shows_a_value_hint() {
        let (_dir, mut app) = temp_app("hint-every-row");
        for spec in SETTINGS {
            while app.selected_spec().key != spec.key {
                handle_key(&mut app, press(KeyCode::Char('j')));
            }
            let hint = app.value_hint();
            assert!(!hint.trim().is_empty(), "{} has no value hint", spec.key);
            let out = frame_size(&mut app, 80, 30);
            // Long hints (the nine game targets) wrap onto a second line —
            // every piece must still be on screen, none silently cut off.
            for line in wrap_words(&hint, footer_text_width(80)) {
                assert!(
                    out.contains(&line),
                    "hint line {line:?} not rendered for {}:\n{out}",
                    spec.key
                );
            }
        }
    }

    #[test]
    fn enum_and_number_rows_name_their_accepted_values() {
        let (_dir, mut app) = temp_app("hint-enum-number");
        assert_eq!(app.selected_spec().key, "default_mode");
        let out = frame_size(&mut app, 80, 30);
        assert!(
            out.contains("one of: tui, lsp"),
            "enum choices missing:\n{out}"
        );

        while app.selected_spec().key != "define_matrix_max_combinations" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        let out = frame_size(&mut app, 80, 30);
        assert!(out.contains("whole number"), "number hint missing:\n{out}");

        while app.selected_spec().key != "warnings_as_errors" {
            handle_key(&mut app, press(KeyCode::Char('k')));
        }
        let out = frame_size(&mut app, 80, 30);
        assert!(out.contains("true or false"), "bool hint missing:\n{out}");
    }

    /// Every list-valued setting carries an example, so no row can ship
    /// without one.
    #[test]
    fn every_string_list_setting_has_an_example() {
        for spec in SETTINGS {
            if let SettingKind::StringList { example } = spec.kind {
                assert!(
                    !example.trim().is_empty(),
                    "{} has no example value",
                    spec.key
                );
            }
        }
    }

    /// The `defines` help must describe the post-B024 additive semantics.
    #[test]
    fn defines_help_describes_additive_semantics() {
        let spec = SETTINGS
            .iter()
            .find(|s| s.key == "defines")
            .expect("defines setting");
        assert!(
            !spec.help.to_lowercase().contains("replaces"),
            "stale replace-semantics help: {}",
            spec.help
        );
        assert!(
            spec.help.to_lowercase().contains("add"),
            "help should say defines are added: {}",
            spec.help
        );
    }

    #[test]
    fn wrap_words_splits_on_spaces_and_never_returns_nothing() {
        assert_eq!(wrap_words("", 20), vec![String::new()]);
        assert_eq!(wrap_words("one of: tui, lsp", 40), vec!["one of: tui, lsp"]);
        assert_eq!(wrap_words("aaa bbb ccc ddd", 7), vec!["aaa bbb", "ccc ddd"]);
        // A word wider than the line is kept whole rather than chopped.
        assert_eq!(
            wrap_words("NATIONS_FOREVER x", 4),
            vec!["NATIONS_FOREVER", "x"]
        );
    }

    #[test]
    fn narrow_terminal_keeps_the_settings_list_on_screen() {
        let (_dir, mut app) = temp_app("narrow");
        while app.selected_spec().key != "game_target" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        let out = frame_size(&mut app, 28, 20);
        assert!(
            out.contains("game_target"),
            "selected setting fell off a narrow screen:\n{out}"
        );
    }

    /// GH #64: the signature level is editable in the TUI, and the value-hint
    /// line lists the four levels.
    #[test]
    fn signature_mode_row_cycles_and_persists() {
        let (_dir, mut app) = temp_app("signature-mode");
        while app.selected_spec().key != "signature_mode" {
            handle_key(&mut app, press(KeyCode::Char('j')));
        }
        let out = frame_size(&mut app, 80, 30);
        assert!(
            out.contains("developer, school, regular, official"),
            "signature levels not offered:\n{out}"
        );
        assert!(
            out.contains("developer (default)"),
            "default level not shown:\n{out}"
        );

        handle_key(&mut app, press(KeyCode::Right));
        let written = std::fs::read_to_string(app.path()).unwrap();
        assert!(
            written.contains("signature_mode = \"school\""),
            "cycle right must persist the next level:\n{written}"
        );
    }

    /// The TUI choice list is the server's own target list — no second copy
    /// to drift.
    #[test]
    fn game_target_choices_come_from_the_server_list() {
        let spec = SETTINGS
            .iter()
            .find(|s| s.key == "game_target")
            .expect("game_target setting");
        let SettingKind::Enum { choices, .. } = spec.kind else {
            panic!("game_target must be an enum setting");
        };
        assert_eq!(choices, crate::config::LspConfig::GAME_TARGETS);
    }

    #[test]
    fn q_quits() {
        let (_dir, mut app) = temp_app("quit");
        handle_key(&mut app, press(KeyCode::Char('q')));
        assert!(app.should_quit);
    }
}
