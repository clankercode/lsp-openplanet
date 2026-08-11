//! Export TUI frames as JSON for visual review (not a published bin).
//! Run: cargo test --test tui_export_frames -- --nocapture
//! Writes docs/images/tui-review/*.json

use openplanet_lsp::tui::{canned_snapshot, App, ListDensity};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use std::fs;
use std::path::PathBuf;

const W: u16 = 100;
const H: u16 = 32;

fn color_hex(c: Color) -> Option<String> {
    match c {
        Color::Reset => None,
        Color::Black => Some("#000000".into()),
        Color::Red => Some("#ff5555".into()),
        Color::Green => Some("#50fa7b".into()),
        Color::Yellow => Some("#f1fa8c".into()),
        Color::Blue => Some("#bd93f9".into()),
        Color::Magenta => Some("#ff79c6".into()),
        Color::Cyan => Some("#8be9fd".into()),
        Color::Gray | Color::DarkGray => Some("#6272a4".into()),
        Color::LightRed => Some("#ff6e6e".into()),
        Color::LightGreen => Some("#69ff94".into()),
        Color::LightYellow => Some("#ffffa5".into()),
        Color::LightBlue => Some("#d6acff".into()),
        Color::LightMagenta => Some("#ff92df".into()),
        Color::LightCyan => Some("#a4ffff".into()),
        Color::White => Some("#f8f8f2".into()),
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Indexed(i) => Some(format!("#idx{i}")),
    }
}

fn export(name: &str, mut app: App) {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    let buf = terminal.backend().buffer();
    let area = buf.area();

    let mut cells = Vec::with_capacity((area.width * area.height) as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            let fg = color_hex(cell.fg);
            let bg = color_hex(cell.bg);
            let bold = cell
                .modifier
                .contains(ratatui::style::Modifier::BOLD);
            cells.push(serde_json::json!({
                "ch": ch.to_string(),
                "fg": fg,
                "bg": bg,
                "bold": bold,
            }));
        }
    }

    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/images/tui-review");
    fs::create_dir_all(&out_dir).unwrap();
    let path = out_dir.join(format!("{name}.json"));
    let doc = serde_json::json!({
        "width": area.width,
        "height": area.height,
        "cells": cells,
    });
    fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
    println!("wrote {}", path.display());
}

#[test]
fn export_frames_for_visual_review() {
    // compact + selected first (pretty detail with carets)
    let mut compact = App::from_snapshot(canned_snapshot());
    export("01-compact", compact);

    // select second (warning)
    let mut warn = App::from_snapshot(canned_snapshot());
    warn.scroll_down();
    export("02-warning-detail", warn);

    // select third
    let mut third = App::from_snapshot(canned_snapshot());
    third.scroll_down();
    third.scroll_down();
    export("03-fakevehicle", third);

    // relaxed density
    let mut relaxed = App::from_snapshot(canned_snapshot());
    relaxed.toggle_density();
    assert_eq!(relaxed.density, ListDensity::Relaxed);
    export("04-relaxed", relaxed);

    // empty
    let empty = App::from_snapshot(openplanet_lsp::tui::Snapshot::empty("./EmptyPlugin"));
    export("05-empty", empty);
}
