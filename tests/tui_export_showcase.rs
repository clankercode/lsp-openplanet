//! Export TUI frames from the real showcase-diags fixture (not canned mock).
//! `cargo test --test tui_export_showcase -- --nocapture`

use openplanet_lsp::cli::watch::report_to_snapshot;
use openplanet_lsp::cli::{run_check, CheckOptions};
use openplanet_lsp::tui::{App, ListDensity, Snapshot};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

fn showcase_snapshot() -> Snapshot {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/showcase-diags");
    let typedb = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    assert!(root.join("info.toml").is_file(), "showcase fixture missing");

    let options = CheckOptions {
        path: root.clone(),
        typedb_dir: Some(typedb),
        no_typedb: false,
        plugins_dirs: Vec::new(),
        plugin_files_search_paths: vec![PathBuf::from("src")],
        format: openplanet_lsp::cli::CheckFormat::Plain,
        watch: false,
    };
    let report = run_check(&options).expect("run_check showcase");
    assert!(
        report.diagnostics.len() >= 8,
        "expected multi-file showcase diags, got {}",
        report.diagnostics.len()
    );
    report_to_snapshot(&report, "showcase-diags", Duration::from_millis(42))
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
            cells.push(serde_json::json!({
                "ch": ch.to_string(),
                "fg": color_hex(cell.fg),
                "bg": color_hex(cell.bg),
                "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
            }));
        }
    }

    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/images/tui-review");
    std::fs::create_dir_all(&out_dir).unwrap();
    let path = out_dir.join(format!("{name}.json"));
    let doc = serde_json::json!({
        "width": area.width,
        "height": area.height,
        "cells": cells,
        "source": "tests/fixtures/showcase-diags",
    });
    std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
    println!("wrote {} ({} diags in app)", path.display(), app.snapshot.diagnostics.len());
}

#[test]
#[ignore = "writes docs/images; run via just tui-frames / tui-showcase-shots"]
fn export_showcase_tui_frames() {
    let snap = showcase_snapshot();
    let n = snap.diagnostics.len();
    println!("showcase diagnostics: {n}");

    // Product-looking sizes
    let sizes = [(100u16, 32u16), (96u16, 28u16), (80u16, 24u16)];

    for &(w, h) in &sizes {
        let tag = format!("{w}x{h}");

        // compact, first error selected (pretty detail)
        let compact = App::from_snapshot(snap.clone());
        export_sized(&format!("showcase-compact-{tag}"), compact, w, h);

        // relaxed multi-diag — primary README candidate
        let mut relaxed = App::from_snapshot(snap.clone());
        relaxed.toggle_density();
        assert_eq!(relaxed.density, ListDensity::Relaxed);
        export_sized(&format!("showcase-relaxed-{tag}"), relaxed, w, h);

        // relaxed + scroll to a middle item with a juicy caret (MakeTint / Overlay)
        let mut mid = App::from_snapshot(snap.clone());
        mid.toggle_density();
        // Prefer Overlay MakeTint if present
        if let Some(idx) = snap
            .diagnostics
            .iter()
            .position(|d| d.message.contains("MakeTint") || d.path.ends_with("Overlay.as"))
        {
            mid.select_first();
            for _ in 0..idx {
                mid.scroll_down();
            }
        } else {
            for _ in 0..(n / 2) {
                mid.scroll_down();
            }
        }
        export_sized(&format!("showcase-relaxed-mid-{tag}"), mid, w, h);
    }

    // Canonical names for docs (best default size 100x32)
    let mut hero = App::from_snapshot(snap.clone());
    hero.toggle_density();
    // Select MakeTint if available — best detail caret demo
    if let Some(idx) = snap
        .diagnostics
        .iter()
        .position(|d| d.message.contains("MakeTint"))
    {
        hero.select_first();
        for _ in 0..idx {
            hero.scroll_down();
        }
    }
    export_sized("showcase-relaxed-hero", hero, 100, 32);

    let _ = Path::new("."); // silence unused in some rustc configs
}
