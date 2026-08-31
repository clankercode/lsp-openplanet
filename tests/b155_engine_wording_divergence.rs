//! B155: the engine names an assignment target as a REFERENCE and ends the
//! message with a full stop; our diagnostic does neither. That divergence is
//! deliberate and documented in `checker.rs` — this file makes it CHECKED.
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
//! Ours renders `Can't implicitly convert from 'int' to 'VirtualKey'` — no
//! `&`, no full stop. B148 recorded three rows as WORDING-CHANGED and ZERO as
//! behaviour-changed, so nothing about the RULES follows from this.
//!
//! # Why this file exists rather than a fix
//!
//! The divergence is a product decision, and the repo currently states BOTH
//! sides of it in different places:
//!
//!   * `checker.rs` (B148): "Our own diagnostic deliberately keeps the
//!     unsuffixed spelling. The `&` is an artifact of how the engine models
//!     assignment, not part of the type the user wrote."
//!   * `b071_enum_type_mismatch.rs`: "Asserted by message text because the
//!     engine's wording is part of the requirement, not an implementation
//!     detail", and "the messages below match it so a user searching an error
//!     string finds both tools" — quoting a string that today matches neither
//!     the engine (no `&`) nor our output (it has the full stop).
//!
//! Changing the wording is a user-visible behaviour change touching 15
//! exact-string assertions, so it is Max's call, not an automatic fix. What
//! IS automatic is refusing to let the divergence drift: these tests derive
//! the engine's spelling FROM the corpora and assert that the difference is
//! EXACTLY "drop the target's `&`, drop the trailing `.`" and nothing else.
//! If the engine's wording moves again, or ours does, this fails and names
//! the new difference instead of leaving a stale comment behind.
//!
//! Flipping the decision is then a one-line change to the renderer plus this
//! file's `DIVERGENCE` transform.

use std::path::PathBuf;

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use openplanet_lsp::typedb::TypeIndex;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

/// The documented divergence, as a transform from the ENGINE's text to ours:
/// drop the trailing full stop, then drop a `&` immediately before the
/// closing quote of the target. Nothing else.
fn engine_text_to_ours(engine: &str) -> String {
    engine
        .strip_suffix('.')
        .unwrap_or(engine)
        .replace("&'", "'")
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
// Our side, and the exact shape of the difference
// ---------------------------------------------------------------------------

/// Our renderer omits both marks, at every site that produces this message.
#[test]
fn our_diagnostic_omits_the_reference_marker_and_the_full_stop() {
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
            assert!(!m.contains("&'"), "ours must not suffix the target: {m:?}");
            assert!(
                !m.ends_with('.'),
                "ours must not end with a full stop: {m:?}"
            );
        }
    }
    assert!(
        seen >= 2,
        "expected both shapes to produce the message, saw {seen} — the test \
         cannot conclude anything from a checker that stayed silent"
    );
}

/// The load-bearing one: the difference is EXACTLY the documented transform.
/// Each engine row is converted by `engine_text_to_ours` and must equal our
/// text character for character. If the engine's wording moves in any other
/// way — or ours does — this fails and names the mismatch, instead of a code
/// comment quietly going stale the way b071's header did.
#[test]
fn the_divergence_is_exactly_the_documented_transform() {
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
        let expected = engine_text_to_ours(engine);
        let ours = our_errors(src);
        assert!(
            ours.iter().any(|m| m == &expected),
            "engine {engine:?}\n  transformed to {expected:?}\n  but ours were {ours:?}"
        );
    }
}

/// Control: the transform is capable of changing a string, and leaves an
/// already-unsuffixed message alone. Without this, `engine_text_to_ours`
/// could be the identity function and every assertion above would still pass.
#[test]
fn the_documented_transform_actually_transforms() {
    assert_eq!(
        engine_text_to_ours("Can't implicitly convert from 'int' to 'VirtualKey&'."),
        "Can't implicitly convert from 'int' to 'VirtualKey'"
    );
    assert_eq!(
        engine_text_to_ours("Can't implicitly convert from 'A' to 'B'"),
        "Can't implicitly convert from 'A' to 'B'",
        "an already-unsuffixed message must pass through unchanged"
    );
}
