//! B053: a bare enum value two globally visible enums carry is ambiguous —
//! the game answers `Found multiple matching enum values` and the checker now
//! says so. The *editor* surfaces must not contradict that by silently
//! picking one of the candidates and presenting it as the answer.
//!
//! Driven through the same public entry points the `Backend` handlers call
//! (`server::hover::hover`, `server::navigation::goto_definition`) against
//! the real shipped type DB — a hand-built stub would not prove anything
//! about the data the editor actually sees. Same method as
//! `tests/b044_narrowed_surfaces.rs`, which pinned the anonymous-enum half.

use std::path::PathBuf;

use openplanet_lsp::analysis::DocumentAnalysis;
use openplanet_lsp::analysis_snapshot::AnalysisSnapshot;
use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::hover::hover;
use openplanet_lsp::typecheck::GlobalScope;
use openplanet_lsp::typedb::TypeIndex;
use tower_lsp::lsp_types::{HoverContents, Position};

/// Names the live game rejects with `Found multiple matching enum values`
/// (RemoteBuild, Trackmania + Openplanet 1.29.5, 2026-08-30).
const AMBIGUOUS: [&str; 4] = ["Turbo", "WallJump", "FreeWheeling", "None"];

/// Carried by exactly one globally visible enum, so the game compiles them
/// and every surface may answer freely.
const UNAMBIGUOUS: [&str; 3] = ["GlassSmash", "JumpChain1", "Horn"];

fn fixture_index() -> TypeIndex {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    TypeIndex::load(
        &fixtures.join("OpenplanetCore.json"),
        &fixtures.join("OpenplanetNext.json"),
    )
    .expect("typedb fixtures must load")
}

fn snapshot(source: &str) -> AnalysisSnapshot {
    AnalysisSnapshot::from_files(
        &[(PathBuf::from("/test/b053.as"), source.to_string())],
        &LspConfig::default(),
    )
}

/// Cursor position just inside the first occurrence of `needle`.
fn pos_in(source: &str, needle: &str) -> Position {
    let byte = source.find(needle).expect("needle must occur");
    let prefix = &source[..byte];
    let line = prefix.matches('\n').count() as u32;
    let col = prefix.rfind('\n').map_or(byte, |nl| byte - nl - 1) as u32;
    Position::new(line, col + 1)
}

fn hover_text(name: &str, index: &TypeIndex) -> Option<String> {
    let source = format!("void f() {{ print(\"\" + {name}); }}\n");
    let snap = snapshot(&source);
    let scope = GlobalScope::new(snap.symbols(), Some(index));
    let analysis = DocumentAnalysis::analyze_plain(&source);
    let h = hover(&analysis, None, pos_in(&source, name), &scope)?;
    match h.contents {
        HoverContents::Markup(m) => Some(m.value),
        _ => None,
    }
}

/// The load-bearing one. Hover must not answer a bare ambiguous value with
/// ONE of its candidate enums as though that were the answer — that is the
/// same first-hit assumption the checker just stopped making, and an editor
/// that shows `ESceneCharVisNotice` for `Turbo` is telling the user their
/// code means something the game will refuse to compile.
#[test]
fn hover_does_not_present_one_arbitrary_candidate_for_an_ambiguous_value() {
    let index = fixture_index();
    // Today hover answers None for all of these: it looks a name up as a
    // TYPE/function/member, and a bare enum VALUE of an engine enum is none
    // of those. Recorded so the assertion below is understood as a guard on
    // a future change rather than as a description of current output.
    assert!(
        AMBIGUOUS.iter().all(|n| hover_text(n, &index).is_none()),
        "hover currently has nothing to say about a bare engine enum value; \
         if that changes, the assertion below is what it must satisfy"
    );
    for name in AMBIGUOUS {
        if let Some(text) = hover_text(name, &index) {
            // If a future change gives these a hover, it must be one that
            // shows the ambiguity, not one candidate presented as the answer.
            assert!(
                text.contains("ambiguous") || text.contains("multiple"),
                "hover on the ambiguous `{name}` must not present a single \
                 candidate as the answer; got {text:?}"
            );
        }
    }
}

/// The other half: an unambiguous bare value is a perfectly good symbol and
/// nothing here should have made it worse.
#[test]
fn hover_still_works_for_an_unambiguous_value() {
    let index = fixture_index();
    for name in UNAMBIGUOUS {
        // No assertion that hover is Some — the point is only that asking is
        // safe and never panics, and that if it answers it is not an
        // ambiguity complaint about a name that is not ambiguous.
        if let Some(text) = hover_text(name, &index) {
            assert!(
                !text.contains("ambiguous"),
                "`{name}` is carried by one globally visible enum; hover must \
                 not call it ambiguous; got {text:?}"
            );
        }
    }
}

/// Go-to-definition resolves workspace symbols and class members only — it
/// never reaches into the engine type DB — so a bare engine enum value has
/// no definition to jump to and it returns nothing rather than jumping to an
/// arbitrary candidate. Pinned so that a later change which DOES teach it the
/// type DB has to decide what an ambiguous name means before it lands.
#[test]
fn goto_definition_does_not_jump_to_an_arbitrary_candidate() {
    use openplanet_lsp::server::navigation::{goto_definition, WorkspaceFiles};
    use std::collections::HashMap;

    let index = fixture_index();
    for name in AMBIGUOUS {
        let source = format!("void f() {{ print(\"\" + {name}); }}\n");
        let snap = snapshot(&source);
        let scope = GlobalScope::new(snap.symbols(), Some(&index));
        let analysis = DocumentAnalysis::analyze_plain(&source);
        let empty = HashMap::new();
        let files = WorkspaceFiles { files: &empty };
        assert!(
            goto_definition(&analysis, None, pos_in(&source, name), &scope, &files).is_empty(),
            "`{name}` is ambiguous in-game; go-to-definition must not pick one \
             of its candidate enums"
        );
    }
}
