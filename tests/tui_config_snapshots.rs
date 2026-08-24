//! Headless snapshots of the `openplanet-lsp config` TUI.

use openplanet_lsp::tui::config::ConfigApp;
use openplanet_lsp::user_config::UserConfigFile;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

fn draw(app: &mut ConfigApp) -> Terminal<TestBackend> {
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    terminal
}

fn temp_app() -> ConfigApp {
    // Fixed path so the header line is byte-stable across runs.
    let path = std::path::PathBuf::from("/tmp/openplanet-lsp-config-snapshot.toml");
    let _ = std::fs::remove_file(&path);
    ConfigApp::new(UserConfigFile::load(&path).unwrap())
}

#[test]
fn snapshot_defaults_80x24() {
    let mut app = temp_app();
    let terminal = draw(&mut app);
    insta::assert_snapshot!("config_defaults_80x24", terminal.backend());
}
