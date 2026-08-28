//! First-run modal (#19): one-time bare-TTY prompt — watch TUI vs LSP.
//!
//! Shown only when a bare launch (no subcommand) on a TTY is about to
//! auto-start the watch TUI and no explicit preference exists yet. The
//! user's answer is persisted to the user-global config
//! (`~/.config/openplanet-lsp/config.toml`): `default_mode` plus
//! `first_run_prompt_done = true`, so the modal is truly one-time.

use std::io;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

use crate::user_config::UserConfigFile;

/// How often the modal loop wakes when idle.
const TICK_RATE: Duration = Duration::from_millis(100);

/// Config key holding the bare-TTY default (`"tui"` / `"lsp"`).
const MODE_KEY: &str = "default_mode";

/// Config key set after the modal has been answered (any outcome).
const DONE_KEY: &str = "first_run_prompt_done";

/// What the user picked in the first-run modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Keep the watch TUI as the bare-TTY default (`default_mode = "tui"`).
    KeepTui,
    /// Switch the bare-TTY default to the language server
    /// (`default_mode = "lsp"`); takes effect for this launch too.
    SwitchLsp,
    /// Keep the TUI default without changing `default_mode`, but mark the
    /// prompt done (Esc / q).
    Dismissed,
    /// Ctrl-C: quit now, persist nothing (asks again next launch).
    Aborted,
}

impl Choice {
    /// `default_mode` value to persist, if the choice pins one.
    fn mode_str(self) -> Option<&'static str> {
        match self {
            Choice::KeepTui => Some("tui"),
            Choice::SwitchLsp => Some("lsp"),
            Choice::Dismissed | Choice::Aborted => None,
        }
    }
}

/// Whether the first-run modal should show for this launch.
///
/// True only when no explicit preference exists anywhere: the user config
/// has neither `default_mode` nor `first_run_prompt_done = true`, and the
/// workspace config does not pin `default_mode` (an explicit workspace pin
/// is already a preference — nagging over it would be wrong).
pub fn needed(user_cfg: Option<&Path>, workspace_cfg: Option<&Path>) -> bool {
    let user = user_cfg.and_then(|path| UserConfigFile::load(path).ok());
    if user
        .as_ref()
        .and_then(|file| file.get_bool(DONE_KEY))
        .unwrap_or(false)
    {
        return false;
    }
    let user_explicit = user.is_some_and(|file| file.get_string(MODE_KEY).is_some());
    let ws_explicit = workspace_cfg
        .and_then(|path| UserConfigFile::load(path).ok())
        .is_some_and(|file| file.get_string(MODE_KEY).is_some());
    !(user_explicit || ws_explicit)
}

/// Persist the modal outcome to the user-global config.
///
/// Writes `default_mode` for [`Choice::KeepTui`] / [`Choice::SwitchLsp`],
/// and `first_run_prompt_done = true` for every non-aborted outcome.
/// [`Choice::Aborted`] persists nothing.
pub fn record(
    choice: Choice,
    user_cfg: Option<&Path>,
) -> Result<(), crate::user_config::UserConfigError> {
    if choice == Choice::Aborted {
        return Ok(());
    }
    let Some(path) = user_cfg else {
        return Err(crate::user_config::UserConfigError::Parse(
            "no user config path ($HOME unset); cannot persist first-run choice".to_string(),
        ));
    };
    let mut file = UserConfigFile::load(path)?;
    if let Some(mode) = choice.mode_str() {
        file.set_string(MODE_KEY, mode);
    }
    file.set_bool(DONE_KEY, true);
    file.save()
}

/// Map a modal key press to a choice. `None` = keep showing the modal
/// (unhandled key; Ctrl/Alt chords are ignored, Ctrl-C aborts in the loop).
pub fn modal_choice_for_key(key: KeyEvent) -> Option<Choice> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Enter => Some(Choice::KeepTui),
        KeyCode::Char('l') | KeyCode::Char('L') => Some(Choice::SwitchLsp),
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => Some(Choice::Dismissed),
        _ => None,
    }
}

fn map_backend_err<E: std::fmt::Display>(err: E) -> io::Error {
    io::Error::other(err.to_string())
}

/// Draw the modal centered on `frame`.
pub fn draw_modal(frame: &mut Frame) {
    let area = centered(84, 70, frame.area());
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let lines = vec![
        Line::from(""),
        Line::from("Running openplanet-lsp in a plugin directory on a terminal"),
        Line::from("starts the watch TUI — not the language server."),
        Line::from(""),
        Line::from("Editors that pipe stdio still get the language server."),
        Line::from(""),
        Line::from("What should a bare launch do?"),
        Line::from(Span::styled("  [t] TUI (default) — watch TUI", bold)),
        Line::from(Span::styled("  [l] LSP — language server on stdio", bold)),
        Line::from(""),
        Line::from("  Enter = TUI   Esc = keep TUI, don't ask again"),
        Line::from("  Ctrl-C = quit, ask again next launch"),
        Line::from(""),
        Line::from("Change later: openplanet-lsp config (default_mode)"),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" First run — bare-TTY default ")
                .border_style(Style::default().add_modifier(Modifier::BOLD)),
        ),
        area,
    );
}

/// Centered sub-rectangle (percentage of the available area).
fn centered(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1])[1]
}

/// Render one modal frame (headless snapshot tests).
pub fn render_once<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<()> {
    terminal.draw(draw_modal).map(drop).map_err(map_backend_err)
}

/// Run the modal against any backend until a choice is made.
pub fn run_with_backend<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<Choice> {
    modal_loop(
        || terminal.draw(draw_modal).map(drop).map_err(map_backend_err),
        || {
            if event::poll(TICK_RATE)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(key)),
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            }
        },
    )
}

/// Modal event loop over injected seams: `draw` renders a frame, `get_key`
/// returns the next key press or `None` (wake with no key). Returns as soon
/// as a key answers the modal; Ctrl-C aborts without an answer.
fn modal_loop<D, F>(mut draw: D, mut get_key: F) -> io::Result<Choice>
where
    D: FnMut() -> io::Result<()>,
    F: FnMut() -> io::Result<Option<KeyEvent>>,
{
    loop {
        draw()?;
        if let Some(key) = get_key()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                return Ok(Choice::Aborted);
            }
            if let Some(choice) = modal_choice_for_key(key) {
                return Ok(choice);
            }
        }
    }
}

/// Run the modal on the real terminal.
pub fn run_real() -> io::Result<Choice> {
    let mut terminal = ratatui::init();
    let result = run_with_backend(&mut terminal);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("op-lsp-first-run-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── gate: needed() ──────────────────────────────────────────────────

    #[test]
    fn no_config_anywhere_means_needed() {
        let dir = temp_dir("gate-empty");
        assert!(needed(Some(&dir.join("u.toml")), Some(&dir.join("w.toml"))));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_files_and_none_paths_mean_needed() {
        assert!(needed(None, None));
        assert!(needed(None, Some(Path::new("/nonexistent/w.toml"))));
    }

    #[test]
    fn user_default_mode_set_means_not_needed() {
        let dir = temp_dir("gate-user-mode");
        let user = dir.join("u.toml");
        fs::write(&user, "default_mode = \"lsp\"\n").unwrap();
        assert!(!needed(Some(&user), None));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn user_done_flag_means_not_needed() {
        let dir = temp_dir("gate-user-done");
        let user = dir.join("u.toml");
        fs::write(&user, "first_run_prompt_done = true\n").unwrap();
        assert!(!needed(Some(&user), None));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn done_flag_false_is_not_done() {
        let dir = temp_dir("gate-done-false");
        let user = dir.join("u.toml");
        fs::write(&user, "first_run_prompt_done = false\n").unwrap();
        assert!(
            needed(Some(&user), None),
            "only `true` marks the prompt done"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn workspace_default_mode_pin_means_not_needed() {
        let dir = temp_dir("gate-ws-pin");
        let ws = dir.join("w.toml");
        fs::write(&ws, "default_mode = \"tui\"\n").unwrap();
        assert!(!needed(None, Some(&ws)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_user_value_still_counts_as_explicit() {
        let dir = temp_dir("gate-invalid");
        let user = dir.join("u.toml");
        fs::write(&user, "default_mode = \"banana\"\n").unwrap();
        assert!(
            !needed(Some(&user), None),
            "an explicit key (even a non-mode value) means the user chose; no nag"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn user_and_workspace_both_explicit_means_not_needed() {
        let dir = temp_dir("gate-both");
        let user = dir.join("u.toml");
        let ws = dir.join("w.toml");
        fs::write(&user, "default_mode = \"lsp\"\n").unwrap();
        fs::write(&ws, "default_mode = \"tui\"\n").unwrap();
        assert!(!needed(Some(&user), Some(&ws)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_user_toml_still_needs_prompt_and_record_fails() {
        let dir = temp_dir("gate-corrupt");
        let user = dir.join("u.toml");
        fs::write(&user, "default_mode = [1,\n").unwrap();
        assert!(
            needed(Some(&user), None),
            "unparseable file = no explicit preference; the modal may ask"
        );
        assert!(
            record(Choice::KeepTui, Some(&user)).is_err(),
            "persisting over an unparseable file must error, not corrupt it further"
        );
        let _ = fs::remove_dir_all(dir);
    }

    // ── persistence: record() ───────────────────────────────────────────

    #[test]
    fn record_keep_tui_writes_mode_and_done() {
        let dir = temp_dir("rec-tui");
        let user = dir.join("u.toml");
        record(Choice::KeepTui, Some(&user)).unwrap();
        let file = UserConfigFile::load(&user).unwrap();
        assert_eq!(file.get_string("default_mode").as_deref(), Some("tui"));
        assert_eq!(file.get_bool(DONE_KEY), Some(true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_switch_lsp_writes_mode_and_done() {
        let dir = temp_dir("rec-lsp");
        let user = dir.join("u.toml");
        record(Choice::SwitchLsp, Some(&user)).unwrap();
        let file = UserConfigFile::load(&user).unwrap();
        assert_eq!(file.get_string("default_mode").as_deref(), Some("lsp"));
        assert_eq!(file.get_bool(DONE_KEY), Some(true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_dismissed_writes_done_but_no_mode() {
        let dir = temp_dir("rec-dismiss");
        let user = dir.join("u.toml");
        record(Choice::Dismissed, Some(&user)).unwrap();
        let file = UserConfigFile::load(&user).unwrap();
        assert_eq!(
            file.get_string("default_mode"),
            None,
            "dismiss must not pin a mode"
        );
        assert_eq!(file.get_bool(DONE_KEY), Some(true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_aborted_persists_nothing() {
        let dir = temp_dir("rec-abort");
        let user = dir.join("u.toml");
        record(Choice::Aborted, Some(&user)).unwrap();
        assert!(!user.exists(), "abort must not create the config file");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_preserves_comments_and_unknown_keys() {
        let dir = temp_dir("rec-keep");
        let user = dir.join("u.toml");
        fs::write(
            &user,
            "# my mode\ndefault_mode = \"tui\"\n\n# keep\nextra = 1\n",
        )
        .unwrap();
        record(Choice::SwitchLsp, Some(&user)).unwrap();
        let written = fs::read_to_string(&user).unwrap();
        assert!(written.contains("# my mode"), "comment lost:\n{written}");
        assert!(written.contains("# keep"), "comment lost:\n{written}");
        assert!(
            written.contains("extra = 1"),
            "unknown key lost:\n{written}"
        );
        assert!(
            written.contains("default_mode = \"lsp\""),
            "not updated:\n{written}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_creates_missing_parent_dirs() {
        let dir = temp_dir("rec-parents");
        let user = dir.join("nested/deeper/u.toml");
        record(Choice::KeepTui, Some(&user)).unwrap();
        assert!(user.is_file(), "parent dirs + file must exist");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_without_path_is_an_error() {
        assert!(record(Choice::KeepTui, None).is_err());
    }

    // ── keys: modal_choice_for_key() ────────────────────────────────────

    #[test]
    fn t_and_t_keep_tui() {
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Char('t'))),
            Some(Choice::KeepTui)
        );
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Char('T'))),
            Some(Choice::KeepTui)
        );
    }

    #[test]
    fn l_and_l_switch_lsp() {
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Char('l'))),
            Some(Choice::SwitchLsp)
        );
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Char('L'))),
            Some(Choice::SwitchLsp)
        );
    }

    #[test]
    fn enter_accepts_the_tui_default() {
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Enter)),
            Some(Choice::KeepTui)
        );
    }

    #[test]
    fn esc_and_q_dismiss_without_changing_mode() {
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Esc)),
            Some(Choice::Dismissed)
        );
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Char('q'))),
            Some(Choice::Dismissed)
        );
        assert_eq!(
            modal_choice_for_key(key(KeyCode::Char('Q'))),
            Some(Choice::Dismissed)
        );
    }

    #[test]
    fn other_keys_keep_the_modal_open() {
        for code in [
            KeyCode::Char('x'),
            KeyCode::Char('r'),
            KeyCode::Tab,
            KeyCode::Down,
        ] {
            assert_eq!(
                modal_choice_for_key(key(code)),
                None,
                "{code:?} must not answer the modal"
            );
        }
    }

    #[test]
    fn ctrl_c_is_not_a_modal_answer() {
        assert_eq!(
            modal_choice_for_key(key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            None,
            "the event loop owns Ctrl-C (abort)"
        );
    }

    #[test]
    fn shift_chords_answer_the_modal() {
        // Real terminals report Shift+T/L/Q as the uppercase char + SHIFT.
        assert_eq!(
            modal_choice_for_key(key_mod(KeyCode::Char('T'), KeyModifiers::SHIFT)),
            Some(Choice::KeepTui)
        );
        assert_eq!(
            modal_choice_for_key(key_mod(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            Some(Choice::SwitchLsp)
        );
        assert_eq!(
            modal_choice_for_key(key_mod(KeyCode::Char('Q'), KeyModifiers::SHIFT)),
            Some(Choice::Dismissed)
        );
    }

    // ── loop: run_with_backend() ────────────────────────────────────────

    /// Feed synthetic key events to the modal loop with a no-op draw seam.
    /// An exhausted queue errors out (fail closed) so a modal that never
    /// answers fails the test instead of spinning forever.
    fn run_with_keys(keys: &[KeyEvent]) -> Choice {
        let mut queue = keys.to_vec();
        modal_loop(
            || Ok(()),
            || {
                if queue.is_empty() {
                    Err(io::Error::other(
                        "key queue exhausted before the modal answered",
                    ))
                } else {
                    Ok(Some(queue.remove(0)))
                }
            },
        )
        .expect("modal loop")
    }

    #[test]
    fn loop_answers_on_first_choice_key() {
        assert_eq!(run_with_keys(&[key(KeyCode::Char('l'))]), Choice::SwitchLsp);
        assert_eq!(run_with_keys(&[key(KeyCode::Char('t'))]), Choice::KeepTui);
        assert_eq!(run_with_keys(&[key(KeyCode::Esc)]), Choice::Dismissed);
    }

    #[test]
    fn loop_ignores_then_answers() {
        assert_eq!(
            run_with_keys(&[key(KeyCode::Char('x')), key(KeyCode::Char('L'))]),
            Choice::SwitchLsp
        );
    }

    #[test]
    fn loop_ctrl_c_aborts_without_answer() {
        assert_eq!(
            run_with_keys(&[key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)]),
            Choice::Aborted
        );
    }

    #[test]
    fn loop_ignores_release_events() {
        let release_l = KeyEvent {
            code: KeyCode::Char('l'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        // If the Release leaked through, this would answer SwitchLsp, not
        // the KeepTui the subsequent press produces.
        assert_eq!(
            run_with_keys(&[release_l, key(KeyCode::Char('t'))]),
            Choice::KeepTui
        );
    }
}
