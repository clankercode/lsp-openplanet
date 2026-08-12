//! Export TUI frames as JSON for visual review (not a published bin).
//! Run: cargo test --test tui_export_frames -- --nocapture
//! Writes docs/images/tui-review/*.json

use openplanet_lsp::tui::{canned_snapshot, App, ListDensity, Snapshot};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use std::fs;
use std::path::PathBuf;

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

fn export_sized(name: &str, mut app: App, w: u16, h: u16) {
    let backend = TestBackend::new(w, h);
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
#[ignore = "writes docs/images; run via just tui-frames / tui-showcase-shots"]
fn export_frames_for_visual_review() {
    // Main multi-diagnostic screens (compact + relaxed) at product sizes.
    let compact = App::from_snapshot(canned_snapshot());
    export_sized("01-compact", compact, 100, 32);
    export_sized(
        "01-compact-80x24",
        App::from_snapshot(canned_snapshot()),
        80,
        24,
    );

    let mut warn = App::from_snapshot(canned_snapshot());
    warn.scroll_down();
    export_sized("02-warning-detail", warn, 100, 32);

    let mut third = App::from_snapshot(canned_snapshot());
    third.scroll_down();
    third.scroll_down();
    export_sized("03-fakevehicle", third, 100, 32);

    // Primary relaxed multi-diag snapshot (user-requested).
    let mut relaxed = App::from_snapshot(canned_snapshot());
    relaxed.toggle_density();
    assert_eq!(relaxed.density, ListDensity::Relaxed);
    export_sized("04-relaxed", relaxed, 100, 32);

    let mut relaxed_std = App::from_snapshot(canned_snapshot());
    relaxed_std.toggle_density();
    export_sized("04-relaxed-80x24", relaxed_std, 80, 24);

    // Second-selected relaxed (warning) for fragment variety.
    let mut relaxed_w = App::from_snapshot(canned_snapshot());
    relaxed_w.toggle_density();
    relaxed_w.scroll_down();
    export_sized("04b-relaxed-warning", relaxed_w, 100, 32);

    let empty = App::from_snapshot(Snapshot::empty("./EmptyPlugin"));
    export_sized("05-empty", empty, 100, 32);
}
