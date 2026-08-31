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
/// OUR spelling, derived FROM a measured engine row rather than typed out.
///
/// B171 inverted this. It used to add both marks, because B160 believed the
/// engine's `&` was universal and we had adopted it. The `&` is conditional on
/// facts invisible at our diagnostic site, so ours now keeps the engine's full
/// stop and drops the target's `&`. This function is the single place that
/// relationship is written down, and the tests below derive from it rather than
/// hardcoding our text — so the divergence cannot drift without this changing.
fn engine_text_to_our_spelling(engine: &str) -> String {
    let mut out = engine.to_string();
    if !out.ends_with('.') {
        out.push('.');
    }
    let target_quote = out
        .strip_suffix('.')
        .and_then(|s| s.rfind('\''))
        .expect("conversion diagnostic must end with a quoted target");
    if out[..target_quote].ends_with('&') {
        out.remove(target_quote - 1);
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

/// The engine ends EVERY measured conversion message with `.`, and suffixes the
/// target with `&` only CONDITIONALLY.
///
/// B160 measured 22 of 22 carrying both marks and read that as a universal. It
/// was not: both corpora it had available carried non-const object targets in
/// variable slots, so the sample was uniform because the SAMPLING FRAME was.
/// B145's corpus refuted it within the hour and B171 measured the rule — the
/// target carries `&` iff the source is non-const AND the target type is not a
/// primitive AND the slot is an assignable variable rather than a return.
///
/// We reproduce the full stop and deliberately not the `&`: two of those three
/// conditions are invisible at our diagnostic site. See `NoImplicitConversion`
/// in `src/typecheck/checker.rs` and `docs/probes/b171-conversion-target-spelling.tsv`.
#[test]
fn the_engine_suffixes_the_target_only_sometimes_and_always_ends_with_a_stop() {
    let msgs = measured_engine_messages();
    assert!(
        msgs.len() >= 22,
        "expected the corpora to carry the measured population (>=22), found {} — \
         the extractor is broken, not the engine: {msgs:?}",
        msgs.len()
    );

    // The trailing full stop IS unconditional. 22 measured by B160, 10 more by
    // B171's probe, no exceptions in either.
    let no_stop: Vec<_> = msgs.iter().filter(|m| !m.ends_with('.')).collect();
    assert!(
        no_stop.is_empty(),
        "some measured engine messages do NOT end with a full stop: {no_stop:?}"
    );

    // The `&` is NOT. B160 read 22 of 22 as a universal because both corpora it
    // had happened to carry non-const object targets; B145 refuted it and
    // B171's probe measured the actual rule. BOTH populations must be present
    // here, or this test has stopped being able to see the thing it exists for.
    let suffixed = msgs.iter().filter(|m| m.contains("&'")).count();
    let bare = msgs.len() - suffixed;
    assert!(
        suffixed > 0,
        "no measured message suffixes its target — the extractor is broken: {msgs:?}"
    );
    assert!(
        bare > 0,
        "no measured message leaves its target bare. Either a corpus was \
         narrowed or B171's was dropped — and with only suffixed rows present \
         this file would once again read a confounded sample as a universal. \
         That is exactly how B160 happened: {msgs:?}"
    );
}

/// The specific pair that refutes every single-variable rule, pinned by value
/// rather than by count.
///
/// `r2` and `r13` are the SAME conversion into the SAME target type in the SAME
/// syntactic slot. They differ only in whether the source is `const`, and they
/// disagree about the target's `&`. Any future "simplification" of the renderer
/// that reintroduces a single unconditional spelling has to delete one of these
/// two lines to go green.
#[test]
fn the_decisive_pair_disagrees_on_the_ampersand() {
    let msgs = measured_engine_messages();
    assert!(
        msgs.iter()
            .any(|m| m == "Can't implicitly convert from 'nat3&' to 'int3&'."),
        "B171 r2 (non-const source) must be present: {msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m == "Can't implicitly convert from 'const nat3' to 'int3'."),
        "B171 r13 / B145's refuting row (const source) must be present: {msgs:?}"
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

/// Our renderer adopts the full stop at every site producing this message, and
/// deliberately leaves the target BARE.
///
/// B171: the engine's `&` is conditional on facts this diagnostic site does not
/// have (the source's constness, and whether the slot is a return). Emitting it
/// unconditionally was wrong for every conversion from a const source — which is
/// what an engine property read like `map.Size` is, i.e. the shape a plugin
/// author actually writes.
#[test]
fn our_diagnostic_is_bare_targeted_and_ends_with_the_full_stop() {
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
            assert!(
                !m.contains("&'"),
                "ours must NOT suffix the target — the engine's `&` is \
                 conditional and we cannot see the conditions: {m:?}"
            );
            assert!(m.ends_with('.'), "ours must end with a full stop: {m:?}");
        }
    }
    assert!(
        seen >= 2,
        "expected both shapes to produce the message, saw {seen} — the test \
         cannot conclude anything from a checker that stayed silent"
    );
}

/// The load-bearing one: our live renderer must equal, character for character,
/// the measured engine row with the target's `&` removed — and nothing else
/// changed. Derived from the corpus, never from hardcoded expectations, so the
/// day the engine's wording moves this fails instead of quietly agreeing with
/// a spelling nobody measured.
#[test]
fn the_renderer_is_exactly_the_corpus_derived_engine_spelling() {
    let cases = [
        (
            "Can't implicitly convert from 'LocalOne' to 'LocalTwo&'.",
            "enum LocalOne { X }\nenum LocalTwo { Y }\n\
             void f() { LocalOne c = LocalOne::X; LocalTwo d = c; print('' + int(d)); }",
        ),
        (
            "Can't implicitly convert from 'int' to 'VirtualKey&'.",
            "void f() { int a = 0; VirtualKey b = a; print('' + int(b)); }",
        ),
    ];
    for (engine, src) in cases {
        // The engine string is not hardcoded folklore: it must be one the
        // corpora actually measured.
        assert!(
            measured_engine_messages().iter().any(|m| m == engine),
            "{engine:?} is not in any probe corpus — update the case, do not \
             invent engine text"
        );
        let expected = engine_text_to_our_spelling(engine);
        assert_ne!(
            expected, engine,
            "these cases carry a suffixed target, so the transform MUST change \
             them — an identity here would make the assertion below vacuous"
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
        engine_text_to_our_spelling("Can't implicitly convert from 'int' to 'VirtualKey&'."),
        "Can't implicitly convert from 'int' to 'VirtualKey'.",
        "a suffixed engine target must lose its `&` and keep its full stop"
    );
    // B171: the engine leaves the target BARE whenever the source is const, so
    // this is a real engine row and not a hypothetical -- it is B145's refuting
    // message. It must pass through untouched, or the transform would be
    // corrupting the very rows that refuted the universal rule.
    assert_eq!(
        engine_text_to_our_spelling("Can't implicitly convert from 'const nat3' to 'int3'."),
        "Can't implicitly convert from 'const nat3' to 'int3'.",
        "an already-bare engine target must pass through unchanged"
    );
    // The `&` inside a SOURCE must survive: only the target's is dropped.
    assert_eq!(
        engine_text_to_our_spelling("Can't implicitly convert from 'nat3&' to 'int3&'."),
        "Can't implicitly convert from 'nat3&' to 'int3'.",
        "only the TARGET's `&` is dropped; the source keeps its own"
    );
}
