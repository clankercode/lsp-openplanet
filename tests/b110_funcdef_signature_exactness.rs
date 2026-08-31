//! B110: a funcdef SIGNATURE match is not a conversion site.
//!
//! `funcdef_types_compatible`'s primitive arm read
//! `a == b || is_convertible(got, expected)`, which treats a function
//! pointer's parameter and return types as if they were VALUES being
//! converted. They are not. A delegate is called with the caller's types
//! directly and there is no seam for the engine to insert a numeric
//! conversion into, so a `double()` cannot stand in for an `int()`.
//!
//! Found while writing B100's over-firing gates: a `double()` callback
//! expecting rejection at an `int()` slot was accepted.
//!
//! # One predicate, six reachers
//!
//! B023, B076, B089, B091, B100 and B102 all funnel through
//! `func_matches_funcdef` -> `funcdef_types_compatible`. Fixing it once fixes
//! every funcdef position, which is why the fix is one line at one site and
//! this file tests the POSITIONS rather than re-testing the rule per ticket.
//!
//! # The risk polarity is the opposite of B091/B100/B102
//!
//! Those ADDED diagnostics where there were none and a zero fleet delta was
//! reassuring. This TIGHTENS, so every changed answer is a candidate false
//! ERROR on code that compiles today, and the sweep was the decision
//! procedure rather than a formality.
//!
//! ## What the sweep measured, before any of this was written
//!
//! A throwaway build that keeps the old answer but prints a marker wherever
//! the new rule disagrees, over all 151 plugins in
//! `~/src/openplanet/my-plugins` plus the five in-repo fixture plugins:
//!
//!   * the leniency is EXERCISED — 408 comparisons across 15 plugins flip
//!     from accept to reject;
//!   * every one of them is `int64 <-> uint64` or `int64`/`uint64` ->
//!     `double`, and every one is at a `startnew` / `Meta::StartWithRunContext`
//!     userdata slot, where the overload set ALSO contains an exactly matching
//!     `CoroutineFuncUserdata*` candidate. Two or three siblings flip; the
//!     right one still accepts; the call stays silent. Confirmed on source:
//!     `tm-simple-room-admin` writes `startnew(SetTimeLimit_Async, 120)`
//!     against `void SetTimeLimit_Async(int64 time)`;
//!   * so the fleet delta is 361 errors before and 361 after, line-identical.
//!
//! The leniency is therefore not load-bearing on any shipped plugin: it is
//! exercised constantly and relied upon nowhere.
//!
//! ## B165 measured the formerly reasoned row
//!
//! A handler declared with a BARE `int` at a coroutine userdata slot
//! (`void H(int)` against `CoroutineFuncUserdataInt64`, i.e. `void(int64)`)
//! is rejected by the live engine itself. B165 ran the real `startnew` seam,
//! both directions of int/int64 and uint/uint64, and both directions of the
//! equal-width signed/unsigned crossings. All nine question rows reject; the
//! exact-match and bogus-symbol controls accept and reject respectively. The
//! durable evidence is `docs/probes/b165-primitive-funcdef.tsv`.
//!
//! The fleet audit was repeated after measurement. The old-vs-exact marker
//! still reached 408 comparisons across the 151 shipped plugin dirs plus five
//! in-repo fixtures, all in the four known overload-sibling shapes
//! (`int64 <-> uint64`, either to `double`). No `int <-> int64` exposure or
//! primitive reference-data defect appeared. Thus the bare-int row is engine
//! evidence, while the fleet remains evidence that existing plugins do not
//! rely on it.
//!
//! # What is deliberately NOT touched
//!
//!   * VALUE conversions elsewhere. `int x = someDouble;` is a real
//!     conversion and `is_convertible` is right about it. The claim is
//!     narrowly that a funcdef signature match is not a conversion site, so
//!     only the predicate reached through `func_matches_funcdef` changes.
//!   * The parameter MODE comparison (B027), which is the sibling axis and
//!     already lives at exactly one site. Nothing here adds a second.
//!   * `const`, WHICH B152 HAS SINCE TIGHTENED. When B110 landed, B102 peeled
//!     `const` deliberately because the engine's stance was unmeasured, and
//!     this file recorded that the two arms did not disagree. B144 measured
//!     the engine and B152 acted: `const` is now compared exactly, before the
//!     peel, and the peel still runs for the handle axis. Nothing in B110's
//!     own subject moved — every test in this file passed unchanged across
//!     that change — see `const_alone_decides_a_funcdef_match` at the bottom
//!     for what replaced the pin, and why the pin could never have failed.

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

/// ERROR severity only: these fixtures declare helpers they never call, and
/// unused-declaration findings would swamp an unfiltered assertion.
fn errors(source: &str) -> Vec<String> {
    let uri = Url::parse("file:///test/b110.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), None)
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| d.message)
        .collect()
}

/// Every numeric primitive `is_convertible` used to equate. `bool` and
/// `string` are NOT here: they were already rejected, so including them would
/// pad the pass count with cases this change cannot affect. They get their own
/// unchanged-behaviour test instead.
const NUMERICS: &[&str] = &[
    "int", "uint", "int64", "uint64", "float", "double", "int8", "int16", "uint8", "uint16",
];

/// Anti-vacuity for the whole file: the harness DOES report on this shape.
/// Every silence assertion below is worthless without it.
#[test]
fn anti_vacuity_the_harness_reports_on_this_shape() {
    let msgs = errors("void Use() { ZZZNoSuchType q; }");
    assert!(
        msgs.iter().any(|m| m.contains("unknown type")),
        "the harness must be able to report on a plugin-shaped fixture: {msgs:?}"
    );
}

/// The four workspace positions the predicate is reached from, each rendered
/// as a source file. `slot` is the funcdef's type, `got` the function's.
///
/// They are generated from one table rather than written out per position
/// because the whole point of the ticket is that ONE predicate serves all of
/// them: a position that drifted would be the defect, not a variation.
fn positions(
    fd_sig: &str,
    fn_sig: &str,
    fd_ret: &str,
    fn_ret: &str,
    body: &str,
) -> Vec<(&'static str, String)> {
    vec![
        (
            "argument of a workspace function",
            format!(
                "funcdef {fd_ret} Maker({fd_sig});\n\
                 {fn_ret} Make({fn_sig}) {{ {body} }}\n\
                 void Take(Maker@ cb) {{ if (cb is null) return; }}\n\
                 void Use() {{ Take(Make); }}\n"
            ),
        ),
        (
            "funcdef constructor",
            format!(
                "funcdef {fd_ret} Maker({fd_sig});\n\
                 {fn_ret} Make({fn_sig}) {{ {body} }}\n\
                 void Use() {{ Maker@ h = Maker(Make); if (h is null) return; }}\n"
            ),
        ),
        (
            "declaration initializer",
            format!(
                "funcdef {fd_ret} Maker({fd_sig});\n\
                 {fn_ret} Make({fn_sig}) {{ {body} }}\n\
                 void Use() {{ Maker@ h = Make; if (h is null) return; }}\n"
            ),
        ),
        (
            "assignment",
            format!(
                "funcdef {fd_ret} Maker({fd_sig});\n\
                 {fn_ret} Make({fn_sig}) {{ {body} }}\n\
                 void Use() {{ Maker@ h; @h = Make; if (h is null) return; }}\n"
            ),
        ),
    ]
}

/// RETURN TYPE, the full lattice both directions, at all four positions.
///
/// 10 x 10 x 4 = 400 cases: 40 exact-match controls that must stay silent and
/// 360 mismatches that must be rejected. Without the controls a predicate
/// that rejects everything scores as a pass on the whole population.
#[test]
fn every_return_type_pair_is_exact_at_every_position() {
    for &slot in NUMERICS {
        for &got in NUMERICS {
            for (position, src) in positions("", "", slot, got, "return 0;") {
                let msgs = errors(&src);
                if slot == got {
                    assert!(
                        msgs.is_empty(),
                        "an EXACT return-type match must bind at the {position}: \
                         funcdef returns {slot}, function returns {got}; got {msgs:?}\n{src}"
                    );
                } else {
                    assert!(
                        !msgs.is_empty(),
                        "a {got}() function must not bind a {slot}() slot at the \
                         {position} — a delegate has no conversion seam:\n{src}"
                    );
                }
            }
        }
    }
}

/// PARAMETER TYPE, the same lattice, the same four positions. A funcdef's
/// parameters are compared by the same predicate as its return type, and a
/// fix that reached only one of the two would be half a fix.
#[test]
fn every_parameter_type_pair_is_exact_at_every_position() {
    for &slot in NUMERICS {
        for &got in NUMERICS {
            let fd_sig = format!("{slot} v");
            let fn_sig = format!("{got} v");
            for (position, src) in
                positions(&fd_sig, &fn_sig, "void", "void", "if (v == 0) return;")
            {
                let msgs = errors(&src);
                if slot == got {
                    assert!(
                        msgs.is_empty(),
                        "an EXACT parameter match must bind at the {position}: \
                         slot {slot}, function {got}; got {msgs:?}\n{src}"
                    );
                } else {
                    assert!(
                        !msgs.is_empty(),
                        "a void({got}) function must not bind a void({slot}) slot \
                         at the {position}:\n{src}"
                    );
                }
            }
        }
    }
}

/// A SECOND parameter, so the fix is not passing because it only ever looks
/// at parameter zero. The first parameter matches exactly in every case here;
/// only the second varies.
#[test]
fn a_later_parameter_is_compared_too() {
    for &got in NUMERICS {
        let src = format!(
            "funcdef void Maker(int a, int b);\n\
             void Make(int a, {got} b) {{ if (a == 0) return; if (b == 0) return; }}\n\
             void Take(Maker@ cb) {{ if (cb is null) return; }}\n\
             void Use() {{ Take(Make); }}\n"
        );
        let msgs = errors(&src);
        if got == "int" {
            assert!(
                msgs.is_empty(),
                "exact match on both params: {msgs:?}\n{src}"
            );
        } else {
            assert!(
                !msgs.is_empty(),
                "the SECOND parameter differs ({got} vs int) and must be caught:\n{src}"
            );
        }
    }
}

/// UNCHANGED BEHAVIOUR, pinned so a later reader does not mistake it for
/// something this ticket did. `bool` and `string` were never numerically
/// convertible, so they were already rejected and still are — and the exact
/// forms still bind.
#[test]
fn non_numeric_primitives_are_unaffected() {
    for &slot in &["bool", "string"] {
        for (position, src) in positions("", "", slot, slot, "return {};") {
            let src = src.replace(
                "return {};",
                if slot == "bool" {
                    "return true;"
                } else {
                    "return \"\";"
                },
            );
            let msgs = errors(&src);
            assert!(
                msgs.is_empty(),
                "an exact {slot}() match must still bind at the {position}: {msgs:?}\n{src}"
            );
        }
        let src = format!(
            "funcdef {slot} Maker();\n\
             int Make() {{ return 0; }}\n\
             void Take(Maker@ cb) {{ if (cb is null) return; }}\n\
             void Use() {{ Take(Make); }}\n"
        );
        assert!(
            !errors(&src).is_empty(),
            "an int() function must still not bind a {slot}() slot:\n{src}"
        );
    }
}

/// EXACTNESS MUST NOT BECOME A SPELLING CHECK, which is the main way a change
/// like this false-positives.
///
/// AngelScript writes `int`/`uint` in source and the typedb writes
/// `int32`/`uint32` for the same types. `PrimitiveType::from_name` already
/// folds those onto one variant, so comparing the parsed primitives is
/// spelling-safe where comparing the text would not have been. Asserted here
/// rather than trusted, because if that folding is ever removed this change
/// turns into a false error on every `int` parameter the typedb describes as
/// `int32`.
#[test]
fn the_int32_spelling_is_the_same_type_as_int() {
    use openplanet_lsp::typecheck::repr::TypeRepr;
    assert_eq!(
        TypeRepr::parse_type_string("int32"),
        TypeRepr::parse_type_string("int"),
        "int32 and int must parse to one primitive, or exactness becomes a \
         spelling check and every typedb-described int param false-positives"
    );
    assert_eq!(
        TypeRepr::parse_type_string("uint32"),
        TypeRepr::parse_type_string("uint"),
    );
}

/// B152 REPLACED THIS TEST, and how it failed is worth more than what it
/// asserted.
///
/// It shipped as `const_is_still_peeled_before_the_exactness_test`, pinning
/// B102's claim that "`const int` and `int` still bind each other, in both
/// directions". It never checked that. Its three rows were
///
/// ```text
/// ("const int &in v", "int v")        <- also a MODE difference
/// ("int v", "const int &in v")        <- also a MODE difference
/// ("const int &in v", "const int &in v")   <- identical on both axes
/// ```
///
/// and its assertion was guarded by `fd.contains("&in") == f.contains("&in")`,
/// which is false for the first two. So the only row that ever reached an
/// assertion was the one where both sides are the same string. The test
/// carried const-neutrality in its name for its whole life and could not
/// observe const at all.
///
/// That is the second time in this family: B144 found
/// `parse_type_string_handle_trailing_const` named for the trailing-`const`
/// case and asserting `Foo@` instead. Both were written while the engine's
/// answer was unmeasured, and in both the un-asserted axis is exactly the one
/// nobody could then justify a claim about. **A test that cannot fail on its
/// subject is where an unmeasured assumption goes to look settled.**
///
/// The engine's answer is now measured
/// (`docs/probes/b144-const-handle-funcdef.tsv`, OP 1.29.14) and it is the
/// opposite of what the old name said: `const` alone DOES decide. So these
/// rows vary const ONLY — same mode, same type, same arity — which is what
/// the original was trying to do, and they assert the measured verdict.
#[test]
fn const_alone_decides_a_funcdef_match() {
    for (fd, f, must_bind) in [
        ("const int &in v", "const int &in v", true),
        ("int &in v", "int &in v", true),
        ("const int &in v", "int &in v", false),
        ("int &in v", "const int &in v", false),
    ] {
        let src = format!(
            "funcdef void Maker({fd});\n\
             void Make({f}) {{ if (v == 0) return; }}\n\
             void Take(Maker@ cb) {{ if (cb is null) return; }}\n\
             void Use() {{ Take(Make); }}\n"
        );
        let msgs = errors(&src);
        let rejected = msgs
            .iter()
            .any(|m| m.contains("No matching signatures") || m.contains("implicitly convert"));
        assert_eq!(
            rejected, !must_bind,
            "funcdef `{fd}` vs function `{f}`: expected bind={must_bind}, got \
             {msgs:#?}\n{src}"
        );
    }
}
