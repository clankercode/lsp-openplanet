//! B155/B160: the engine names an assignment target as a REFERENCE and ends
//! the message with a full stop. B155 measured and pinned the old divergence;
//! B160 adopted the engine spelling exactly.
//!
//! # What was measured (B148 regeneration, OP 1.29.14)
//!
//! Across two independent probe corpora — `b071-enum-conversion.tsv` (enum
//! targets) and `b144-const-handle-funcdef.tsv` (funcdef-handle targets) —
//! **every** measured `Can't implicitly convert` message names its target with
//! a trailing `&` and ends with `.`:
//!
//! ```text
//! Can't implicitly convert from 'int' to 'VirtualKey&'.
//! Can't implicitly convert from '<function>@const' to 'FDB144CHFq4val@&'.
//! ```
//!
//! Before B160 ours rendered `Can't implicitly convert from 'int' to
//! 'VirtualKey'` — no `&`, no full stop. B148 recorded three rows as
//! WORDING-CHANGED and ZERO as behaviour-changed, so nothing about the RULES
//! follows from this.
//!
//! # Why the relationship test remains after the fix
//!
//! The measured strings still come FROM the corpora. B160 inverted B155's
//! documented transform: instead of dropping the engine's two marks to model
//! the old local spelling, `legacy_local_text_to_engine` adds those marks and
//! must reproduce each measured engine row character-for-character. The same
//! test then requires the live renderer to equal that row. If either the
//! engine corpus or our renderer moves, the failure names the new difference.

use std::path::PathBuf;

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use openplanet_lsp::typedb::TypeIndex;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

/// B160's inverse of B155's transform: take the former local spelling and add
/// a reference marker to the target plus the final full stop. Already-adopted
/// text passes through unchanged, which keeps this helper safe for controls.
fn legacy_local_text_to_engine(local: &str) -> String {
    let mut out = local.to_string();
    if !out.ends_with('.') {
        out.push('.');
    }
    let target_quote = out
        .strip_suffix('.')
        .and_then(|s| s.rfind('\''))
        .expect("conversion diagnostic must end with a quoted target");
    if !out[..target_quote].ends_with('&') {
        out.insert(target_quote, '&');
    }
    out
}

fn probe_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/probes")
}

/// The corpus files this test reads: the CURRENT-version corpora at the top
/// level of `docs/probes/`, deliberately NOT recursing into
/// `docs/probes/versions/<v>/`.
///
/// That is load-bearing, not incidental. B151's retention design keeps
/// per-version snapshots under `versions/`, and an older snapshot records the
/// engine's OLDER wording — the very unsuffixed spelling this file asserts is
/// gone. Recursing would mix two engine versions into one population and turn
/// `the_engine_suffixes_every_measured_conversion_target` into a test that
/// fails for a correct reason nobody could read. `the_extractor_reads_only_
/// the_current_version_corpora` holds this shut.
fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(probe_dir())
        .expect("probe dir must exist")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("tsv"))
        .collect();
    files.sort();
    files
}

/// Every `Can't implicitly convert …` message MEASURED from the game, across
/// every current-version corpus. Comment lines are skipped: they are prose
/// about our own expected output, not measurements, and folding them in is
/// exactly how a hand-written expectation gets mistaken for evidence.
fn measured_engine_messages() -> Vec<String> {
    let mut out = Vec::new();
    for path in corpus_files() {
        let text = std::fs::read_to_string(&path).expect("corpus must read");
        for line in text.lines() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            for field in line.split('\t') {
                let mut rest = field;
                while let Some(i) = rest.find("Can't implicitly convert from '") {
                    let tail = &rest[i..];
                    // Message ends at the closing quote of the target, plus an
                    // optional full stop.
                    let after_from = &tail["Can't implicitly convert from '".len()..];
                    let Some(q1) = after_from.find('\'') else {
                        break;
                    };
                    let after_to = &after_from[q1 + 1..];
                    let Some(open) = after_to.find('\'') else {
                        break;
                    };
                    let Some(q2) = after_to[open + 1..].find('\'') else {
                        break;
                    };
                    let end = "Can't implicitly convert from '".len() + q1 + 1 + open + 1 + q2 + 1;
                    let mut msg = tail[..end].to_string();
                    if tail[end..].starts_with('.') {
                        msg.push('.');
                    }
                    out.push(msg);
                    rest = &tail[end..];
                }
            }
        }
    }
    out
}

fn our_errors(source: &str) -> Vec<String> {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let index = TypeIndex::load(
        &fixtures.join("OpenplanetCore.json"),
        &fixtures.join("OpenplanetNext.json"),
    )
    .expect("typedb fixtures must load");
    let uri = Url::parse("file:///test/b155.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), Some(&index))
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| d.message)
        .collect()
}

// ---------------------------------------------------------------------------
// The engine's side, read off the corpora
// ---------------------------------------------------------------------------

/// The engine suffixes EVERY measured conversion target with `&` and ends the
/// message with `.`. Uniform, across two corpora — so there is no judgement
/// about when it applies, which is what makes the divergence a single
/// decision rather than a per-case one.
#[test]
fn the_engine_suffixes_every_measured_conversion_target() {
    let msgs = measured_engine_messages();
    assert!(
        msgs.len() >= 22,
        "expected the corpora to carry the measured population (>=22), found {} — \
         the extractor is broken, not the engine: {msgs:?}",
        msgs.len()
    );
    let no_amp: Vec<_> = msgs.iter().filter(|m| !m.contains("&'")).collect();
    assert!(
        no_amp.is_empty(),
        "some measured engine messages do NOT suffix the target: {no_amp:?}"
    );
    let no_stop: Vec<_> = msgs.iter().filter(|m| !m.ends_with('.')).collect();
    assert!(
        no_stop.is_empty(),
        "some measured engine messages do NOT end with a full stop: {no_stop:?}"
    );
}

/// The extractor must read the CURRENT corpora only. If someone "improves" it
/// to recurse, a retained older-version snapshot would inject the engine's
/// previous wording and the uniformity assertion above would fail for a reason
/// that reads as a contradiction rather than as a version mix.
#[test]
fn the_extractor_reads_only_the_current_version_corpora() {
    let files = corpus_files();
    assert!(
        files.len() >= 10,
        "expected the current-version corpora, found {} — the reader is broken: {files:?}",
        files.len()
    );
    for f in &files {
        assert!(
            !f.components().any(|c| c.as_os_str() == "versions"),
            "the extractor must not read retained per-version snapshots: {f:?}"
        );
    }
    // The premise: there IS a versions/ tree to have wrongly included, so
    // this test is guarding something that exists rather than nothing.
    assert!(
        probe_dir().join("versions").is_dir(),
        "premise: docs/probes/versions/ exists (B151 retention), so excluding          it is a real exclusion"
    );
}

/// Anti-vacuity for the extractor: it must be reading real corpus rows, and
/// it must NOT be picking up the prose in comment headers (which quotes the
/// unsuffixed spelling and would silently contradict the test above).
#[test]
fn the_extractor_reads_measurements_and_not_prose() {
    let msgs = measured_engine_messages();
    assert!(
        msgs.iter().any(|m| m.contains("VirtualKey&")),
        "b071's measured int->enum row must be present: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("<function>@const")),
        "b144's measured funcdef rows must be present: {msgs:?}"
    );
    // The b071 header quotes `to 'LocalTwo'.` as prose. If the extractor ever
    // starts reading comments, this fires.
    assert!(
        !msgs.iter().any(|m| m.contains("'LocalTwo'")),
        "the extractor picked up prose from a comment header: {msgs:?}"
    );
}

// ---------------------------------------------------------------------------
// Our side, and its exact relationship to the measured engine spelling
// ---------------------------------------------------------------------------

/// Our renderer adopts both engine marks at every site producing this message.
#[test]
fn our_diagnostic_uses_the_reference_marker_and_the_full_stop() {
    let sources = [
        // enum -> different enum (workspace)
        "enum LocalOne { X }\nenum LocalTwo { Y }\n\
         void f() { LocalOne c = LocalOne::X; LocalTwo d = c; print('' + int(d)); }",
        // int -> enum (engine enum, needs the typedb)
        "void f() { int a = 0; VirtualKey b = a; print('' + int(b)); }",
    ];
    let mut seen = 0;
    for src in sources {
        for m in our_errors(src) {
            if !m.starts_with("Can't implicitly convert") {
                continue;
            }
            seen += 1;
            assert!(m.contains("&'"), "ours must suffix the target: {m:?}");
            assert!(m.ends_with('.'), "ours must end with a full stop: {m:?}");
        }
    }
    assert!(
        seen >= 2,
        "expected both shapes to produce the message, saw {seen} — the test \
         cannot conclude anything from a checker that stayed silent"
    );
}

/// The load-bearing one: the inverse transform must reconstruct the measured
/// engine row, and our live renderer must equal that row character for
/// character. The transform stays checked rather than being deleted now that
/// the two sides agree.
#[test]
fn the_renderer_is_exactly_the_corpus_derived_engine_spelling() {
    let cases = [
        (
            "Can't implicitly convert from 'LocalOne' to 'LocalTwo&'.",
            "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
            "enum LocalOne { X }\nenum LocalTwo { Y }\n\
             void f() { LocalOne c = LocalOne::X; LocalTwo d = c; print('' + int(d)); }",
        ),
        (
            "Can't implicitly convert from 'int' to 'VirtualKey&'.",
            "Can't implicitly convert from 'int' to 'VirtualKey'",
            "void f() { int a = 0; VirtualKey b = a; print('' + int(b)); }",
        ),
    ];
    for (engine, legacy_local, src) in cases {
        // The engine string is not hardcoded folklore: it must be one the
        // corpora actually measured.
        assert!(
            measured_engine_messages().iter().any(|m| m == engine),
            "{engine:?} is not in any probe corpus — update the case, do not \
             invent engine text"
        );
        let expected = legacy_local_text_to_engine(legacy_local);
        assert_eq!(
            expected, engine,
            "the inverted transform must reproduce the measured engine row"
        );
        let ours = our_errors(src);
        assert!(
            ours.iter().any(|m| m == &expected),
            "engine {engine:?}\n  transformed to {expected:?}\n  but ours were {ours:?}"
        );
    }
}

/// Control: the inverted transform changes legacy text and leaves text already
/// using the engine spelling alone. Without this, it could silently become an
/// identity function and the relationship assertion would lose its subject.
#[test]
fn the_inverted_transform_actually_transforms() {
    assert_eq!(
        legacy_local_text_to_engine("Can't implicitly convert from 'int' to 'VirtualKey'"),
        "Can't implicitly convert from 'int' to 'VirtualKey&'."
    );
    assert_eq!(
        legacy_local_text_to_engine("Can't implicitly convert from 'A' to 'B&'."),
        "Can't implicitly convert from 'A' to 'B&'.",
        "an already-adopted message must pass through unchanged"
    );
}
