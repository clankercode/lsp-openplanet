//! B044: the bare value name of an anonymous (inline member) engine enum is
//! not a symbol. The checker stopped accepting it; the *editor* surfaces must
//! agree, or the LSP would suggest a name it then flags as undefined.
//!
//! Driven through the same public entry points the `Backend` handlers call
//! (`server::hover::hover`, `server::completion::complete`) against the real
//! shipped type DB — a hand-built stub would not prove anything about the
//! data the editor actually sees.

use std::path::PathBuf;

use openplanet_lsp::analysis::DocumentAnalysis;
use openplanet_lsp::analysis_snapshot::AnalysisSnapshot;
use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::completion::complete;
use openplanet_lsp::server::hover::hover;
use openplanet_lsp::typecheck::GlobalScope;
use openplanet_lsp::typedb::TypeIndex;
use tower_lsp::lsp_types::{HoverContents, Position};

/// Values of anonymous member enums, reachable in the game only through the
/// declaring member's type. Two from set B (accepted before B016) and two
/// from set C (recovered by B016).
const NARROWED: [&str; 4] = [
    "HyperTnL",
    "RenderPlaneReflect",
    "MinimalReadOnly",
    "Text_Icon",
];

/// Also a value of the GLOBALLY VISIBLE `EGxTexAddress`, so a bare reference
/// to it is legitimate and must keep resolving. B044 used `VCenter2` here,
/// which is shared with a merely *named* (nested) enum; the exhaustive probe
/// answered `No matching symbol 'VCenter2'`, so B057 moved this fence onto a
/// name the game actually accepts.
const SHARED_WITH_GLOBAL_ENUM: &str = "Border";

/// Shared with a NESTED named enum only. The game rejects it, so it belongs
/// on the narrowed side now — see `docs/probes/b053-bare-enum-values.tsv`.
const SHARED_WITH_NESTED_ENUM: &str = "VCenter2";

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
        &[(PathBuf::from("/test/b044.as"), source.to_string())],
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

#[test]
fn hover_does_not_describe_a_bare_anonymous_enum_value() {
    let index = fixture_index();
    for name in NARROWED {
        let src = format!("void f() {{\n    print(\"\" + {name});\n}}\n");
        let snap = snapshot(&src);
        let uri = snap.uri_map()[&0].0.clone();
        let scope = GlobalScope::new(snap.symbols(), Some(&index));
        // The checked view the `Backend` hover handler always supplies — the
        // fallback path alone would not exercise the recorded-type branch.
        let checked = snap.checked_file(&uri, &scope);
        assert!(checked.is_some(), "the snapshot must yield a checked view");
        let analysis = DocumentAnalysis::analyze_plain(&src);
        let text = hover(&analysis, checked.as_ref(), pos_in(&src, name), &scope)
            .map(|h| match h.contents {
                HoverContents::Markup(m) => m.value,
                other => panic!("unexpected hover contents: {other:?}"),
            })
            .unwrap_or_default();
        assert!(
            !text.contains("enum"),
            "hover offered `{name}` as an enum: {text:?}"
        );
        assert!(
            !text.contains("UnnamedEnum"),
            "hover leaked a synthesized anonymous key for `{name}`: {text:?}"
        );
        // Whatever hover says, it must not claim the name is known: the
        // checker flags it undefined on the very same bytes. Observed
        // production text is "(local) <error:HyperTnL> HyperTnL" — the
        // unresolved sentinel carries the name. (A name that DOES resolve
        // but has no stored value type reads "(local) <error> Border";
        // improving that display is hover's own ticket, not this one.)
        assert!(
            text.is_empty() || text.contains("error"),
            "hover described the undefined `{name}` as something real: {text:?}"
        );
    }
}

#[test]
fn identifier_completion_does_not_offer_a_bare_anonymous_enum_value() {
    let index = fixture_index();
    // Every namespace that declares an anonymous member enum, with enum
    // shortcut matching ON — the most permissive configuration there is.
    let namespaces: Vec<String> = [
        "Control", "Graphic", "Plug", "Scene", "Game", "Net", "Input",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    for name in NARROWED {
        let prefix = &name[..name.len().min(4)];
        let src = format!("void f() {{ {prefix} }}\n");
        let analysis = DocumentAnalysis::analyze_plain(&src);
        let items = complete(
            &analysis,
            None,
            pos_in(&src, prefix),
            Some(&index),
            None,
            &namespaces,
            true,
        );
        let tail = format!("::{name}");
        let offered: Vec<&String> = items
            .iter()
            .map(|i| &i.label)
            .filter(|l| *l == name || l.ends_with(&tail))
            .collect();
        assert!(
            offered.is_empty(),
            "completion offered the narrowed `{name}`: {offered:?}"
        );
    }
}

/// Fence: the narrowing is about the enum being nameless, not about enum
/// values in general. The namespace-shortcut surface — the only place bare
/// engine enum VALUES are ever offered — still offers a NAMED enum's values.
#[test]
fn shortcut_completion_still_offers_a_named_enums_values() {
    let index = fixture_index();
    let namespaces = vec!["Json".to_string()];
    let src = "void f() { Unkno }\n";
    let analysis = DocumentAnalysis::analyze_plain(src);
    let items = complete(
        &analysis,
        None,
        pos_in(src, "Unkno"),
        Some(&index),
        None,
        &namespaces,
        true,
    );
    assert!(
        items.iter().any(|i| i.label == "Json::Type::Unknown"),
        "completion dropped `Json::Type::Unknown`, a NAMED enum's value: {:?}",
        items
            .iter()
            .map(|i| &i.label)
            .filter(|l| l.starts_with("Json::"))
            .collect::<Vec<_>>()
    );
}

/// Fence: a value that ALSO belongs to a GLOBALLY VISIBLE enum is not
/// narrowed anywhere. `Border` is a value of an anonymous member enum AND of
/// the global `EGxTexAddress`, so the checker keeps resolving it — the
/// property this whole change turns on.
///
/// The negative half is B057's correction: `VCenter2` is shared with the
/// NESTED `Game::CGameManialinkControl::EAlignVertical`, which puts nothing
/// in the global value scope, and the game answers `No matching symbol`.
#[test]
fn a_value_shared_with_a_global_enum_still_resolves() {
    let index = fixture_index();
    let snap = snapshot("void f() {}\n");
    let scope = GlobalScope::new(snap.symbols(), Some(&index));
    assert!(
        scope.has_global_ident(SHARED_WITH_GLOBAL_ENUM),
        "`{SHARED_WITH_GLOBAL_ENUM}` is a globally visible enum's value and must keep resolving"
    );
    assert!(
        !scope.has_global_ident(SHARED_WITH_NESTED_ENUM),
        "`{SHARED_WITH_NESTED_ENUM}` is only a nested enum's value; the game rejects it"
    );
    for name in NARROWED {
        assert!(
            !scope.has_global_ident(name),
            "`{name}` belongs to no globally visible enum and must not resolve"
        );
    }
}
