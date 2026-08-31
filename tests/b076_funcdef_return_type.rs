//! B076: a funcdef handle accepts a function whose return type differs.
//!
//! ```angelscript
//! funcdef void Maker();
//! int MakeDistinct() { return 1; }
//! Maker@ b = MakeDistinct;   // silent — return types differ
//! ```
//!
//! RED FIRST, IMPLEMENTATION HELD. This branch must not merge before the fix
//! lands: the `binding_*` tests below are expected to fail, and they are the
//! specification of the fix. The controls around them pass today and must keep
//! passing.
//!
//! # Where the defect is NOT
//!
//! The ticket points at `funcdef_types_compatible` / `funcdef_handle_compatible`,
//! and the coordinator's collision analysis with B081 rests on that. Reading
//! the code first says otherwise, and these tests are arranged to MEASURE it
//! rather than argue it:
//!
//! * `func_matches_funcdef` already compares return types — it runs
//!   `funcdef_types_compatible` over `sig.return_type` vs `fd.return_type`
//!   before it ever looks at parameters.
//! * Its only three callers (`arg_fails_funcdef_slot` twice,
//!   `arg_binds_to_funcdef`) are ARGUMENT-position paths from B023.
//! * The initializer path does not go near it. `check_var_decl_local` runs
//!   only `check_init_conversion`, which is GH #37's implicit-conversion
//!   warning check, and `check_var_decl_global` type-checks the initializer
//!   expression and then discards the result entirely.
//!
//! So the prediction is: the same mismatch is ALREADY rejected in argument
//! position and silent in initializer position. The `position_*` tests below
//! are the discriminating pair. If they come out that way, the fix belongs in
//! the var-decl path and not in the region B081 contends, and the two tickets
//! do not overlap.
//!
//! # Why workspace types rather than engine types
//!
//! The ticket asks for `CMwNod@` vs `CGameCtnBlock@`. These use workspace
//! classes instead, which exercise the same `named_types_equivalent` axis
//! without depending on a type DB — the hermetic gate scrubs every
//! `OPENPLANET*` variable, so a typedb-dependent test would take its skip path
//! there and prove nothing. The engine-typed variant is worth adding alongside
//! the fix, where a fixture index can be wired in deliberately.
//!
//! # Game probe
//!
//! NOT probed: the game was down as of 2026-08-30 (B068). The expected
//! behaviour here is inferred from the engine's documented delegate rules and
//! from the checker already rejecting the identical mismatch in argument
//! position. A live REJECT should confirm it before the fix is called done; do
//! not infer from an ACCEPT.

use std::path::PathBuf;

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use openplanet_lsp::typedb::TypeIndex;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

/// `type_index: None` on purpose — no engine type DB, so these stay hermetic
/// (the B045 gate scrubs every `OPENPLANET*` variable, and a typedb-dependent
/// test would take its skip path there and prove nothing).
///
/// ERROR severity only. `compute_diagnostics` also emits unused-declaration
/// warnings, and every fixture below declares things it never uses, so an
/// unfiltered "no diagnostics" assertion would fail for a reason unrelated to
/// this ticket.
fn errors(source: &str) -> Vec<Diagnostic> {
    let uri = Url::parse("file:///test/b076.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), None)
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .collect()
}

/// Deliberately asserts only that SOMETHING is reported, not which kind.
/// The fix has not chosen a diagnostic kind yet, and a test that pinned one
/// would be specifying the implementation rather than the behaviour.
fn assert_rejected(source: &str, what: &str) {
    let diags = errors(source);
    assert!(
        !diags.is_empty(),
        "expected an error for {what}, got silence:\n{source}"
    );
}

fn assert_accepted(source: &str, what: &str) {
    let diags = errors(source);
    assert!(
        diags.is_empty(),
        "expected silence for {what}, got {:?}:\n{source}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Controls: these pass today. If any of them breaks, the fix has overreached.
// ---------------------------------------------------------------------------

/// The whole point of the feature: a matching signature still binds.
#[test]
fn control_matching_return_and_params_still_binds() {
    assert_accepted(
        r#"
        funcdef void Maker();
        void MakeIt() {}
        void Use() { Maker@ b = MakeIt; }
        "#,
        "an exactly matching function bound to a funcdef handle",
    );
}

/// The parameter check already works. It is the half this fix must not
/// disturb, and pinning it here means a regression shows up as this test
/// rather than as a mystery elsewhere.
#[test]
fn control_matching_return_with_differing_params_is_already_rejected() {
    assert_rejected(
        r#"
        funcdef void Maker();
        void MakeIt(int x) {}
        void Use() { Maker@ b = MakeIt; }
        "#,
        "matching return type but a differing parameter list",
    );
}

/// Nothing about this ticket should make a plain correct program noisy.
#[test]
fn control_a_funcdef_handle_accepts_null() {
    assert_accepted(
        r#"
        funcdef void Maker();
        void Use() { Maker@ b = null; }
        "#,
        "null bound to a funcdef handle",
    );
}

// ---------------------------------------------------------------------------
// The defect: return-type mismatch in initializer position. Expected RED.
// ---------------------------------------------------------------------------

/// The ticket's own repro: a value return against a `void` funcdef.
#[test]
fn binding_rejects_value_return_against_void_funcdef() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        void Use() { Maker@ b = MakeDistinct; }
        "#,
        "an `int`-returning function bound to a `void` funcdef",
    );
}

/// The other polarity: a `void` function against a value-returning funcdef.
#[test]
fn binding_rejects_void_return_against_value_funcdef() {
    assert_rejected(
        r#"
        funcdef int Maker();
        void MakeNothing() {}
        void Use() { Maker@ b = MakeNothing; }
        "#,
        "a `void` function bound to an `int`-returning funcdef",
    );
}

/// KNOWN LIMITATION, pinned as a consistency property rather than as a
/// verdict. `funcdef_types_compatible` accepts two primitives when
/// `is_convertible` holds, so a `float` return binds a `uint` funcdef —
/// and it does so at EVERY position, including the constructor, which
/// already worked before this fix. Measured: both silent.
///
/// So this is not the initializer gap B076 closed; it is a pre-existing
/// looseness in the shared predicate. Whether the engine agrees is
/// unresolved and needs a live REJECT to settle (the game is down, B068),
/// and tightening it is the error-ADDING direction, which is the one not to
/// guess at. Filed separately rather than changed here.
///
/// What this test does pin: the two positions must not diverge. If a later
/// change makes one of them stricter, that is a real inconsistency and this
/// goes red.
#[test]
fn primitive_convertible_return_is_accepted_at_both_positions() {
    let via_init = errors(
        r#"
        funcdef uint Maker();
        float MakeFloat() { return 1.0f; }
        void Use() { Maker@ b = MakeFloat; }
        "#,
    );
    let via_ctor = errors(
        r#"
        funcdef uint Maker();
        float MakeFloat() { return 1.0f; }
        void Use() { Maker@ b = Maker(MakeFloat); }
        "#,
    );
    assert_eq!(
        via_init.is_empty(),
        via_ctor.is_empty(),
        "initializer and constructor must agree on a convertible primitive \
         return; init={via_init:?} ctor={via_ctor:?}"
    );
    assert!(
        via_ctor.is_empty(),
        "documented current behaviour is that both accept it; if this now \
         rejects, the predicate was tightened and the comment is stale: {via_ctor:?}"
    );
}

/// Handle vs handle, two unrelated workspace classes.
#[test]
fn binding_rejects_differing_handle_returns() {
    assert_rejected(
        r#"
        class A {}
        class B {}
        funcdef A@ Maker();
        B@ MakeB() { return B(); }
        void Use() { Maker@ b = MakeB; }
        "#,
        "a `B@`-returning function bound to an `A@` funcdef",
    );
}

/// KNOWN LIMITATION, same shape as the primitive case above.
/// `funcdef_types_compatible` runs `peel_const_handle` over both sides, so
/// `A` and `A@` compare equal and an `A`-returning function binds an
/// `A@` funcdef. Measured at both positions: both silent, including the
/// constructor that already worked.
///
/// Pre-existing, not introduced by B076's fix, and tightening it is the
/// error-adding direction with no live engine to confirm against.
#[test]
fn handle_versus_value_return_is_accepted_at_both_positions() {
    let via_init = errors(
        r#"
        class A {}
        funcdef A@ Maker();
        A MakeValue() { return A(); }
        void Use() { Maker@ b = MakeValue; }
        "#,
    );
    let via_ctor = errors(
        r#"
        class A {}
        funcdef A@ Maker();
        A MakeValue() { return A(); }
        void Use() { Maker@ b = Maker(MakeValue); }
        "#,
    );
    assert_eq!(
        via_init.is_empty(),
        via_ctor.is_empty(),
        "initializer and constructor must agree on handle-vs-value return; \
         init={via_init:?} ctor={via_ctor:?}"
    );
    assert!(
        via_ctor.is_empty(),
        "documented current behaviour is that both accept it: {via_ctor:?}"
    );
}

/// A derived class is still not the declared return type for delegate
/// construction. Kept separate from the unrelated-classes case because a fix
/// that leans on `is_external_derived_from` would pass that one and fail this.
#[test]
fn binding_rejects_derived_handle_return_against_base_funcdef() {
    assert_rejected(
        r#"
        class Base {}
        class Derived : Base {}
        funcdef Derived@ Maker();
        Base@ MakeBase() { return Base(); }
        void Use() { Maker@ b = MakeBase; }
        "#,
        "a `Base@`-returning function bound to a `Derived@` funcdef",
    );
}

/// Both halves wrong at once must still be rejected — a fix that returns
/// early on the parameter mismatch would pass this for the wrong reason, so
/// it is paired with the params-only and return-only cases above.
#[test]
fn binding_rejects_both_return_and_params_differing() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct(int x) { return x; }
        void Use() { Maker@ b = MakeDistinct; }
        "#,
        "both the return type and the parameter list differing",
    );
}

// ---------------------------------------------------------------------------
// Position axis. These are the discriminating tests for where the fix goes.
// ---------------------------------------------------------------------------

/// PREDICTED GREEN: argument position already routes through
/// `func_matches_funcdef`, which compares return types.
#[test]
fn position_argument_already_rejects_the_same_mismatch() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        void Take(Maker@ cb) {}
        void Use() { Take(MakeDistinct); }
        "#,
        "the same return mismatch passed to a funcdef-typed parameter",
    );
}

/// Separates the two explanations for the test above being silent: is the
/// RETURN type unchecked at a workspace call, or is the funcdef slot not
/// checked there AT ALL? A parameter mismatch is already rejected in the
/// constructor form, so if this is silent too, the answer is "at all" and the
/// argument-position gap is wider than B076's title.
#[test]
fn position_workspace_call_with_differing_params() {
    assert_rejected(
        r#"
        funcdef void Maker();
        void MakeIt(int x) {}
        void Take(Maker@ cb) {}
        void Use() { Take(MakeIt); }
        "#,
        "a parameter mismatch passed to a workspace funcdef-typed parameter",
    );
}

/// The constructor form with a parameter mismatch, as the positive control
/// for the pair above: this one is known to work, so a silence in the two
/// workspace-call tests is about the call path and not about the fixtures.
#[test]
fn position_constructor_with_differing_params_is_already_rejected() {
    assert_rejected(
        r#"
        funcdef void Maker();
        void MakeIt(int x) {}
        void Use() { Maker@ b = Maker(MakeIt); }
        "#,
        "a parameter mismatch through the funcdef constructor",
    );
}

/// PREDICTED GREEN for the same reason: the funcdef constructor form is an
/// argument position too.
#[test]
fn position_funcdef_constructor_already_rejects_the_same_mismatch() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        void Use() { Maker@ b = Maker(MakeDistinct); }
        "#,
        "the same return mismatch through the funcdef constructor",
    );
}

/// PREDICTED RED, and separately from the local case: `check_var_decl_global`
/// evaluates the initializer and discards the result, so a global runs through
/// even less checking than a local does.
#[test]
fn position_global_initializer_rejects_the_mismatch() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        Maker@ g = MakeDistinct;
        "#,
        "the mismatch in a GLOBAL variable initializer",
    );
}

/// PREDICTED RED: plain assignment after declaration, not just initialization.
#[test]
fn position_plain_assignment_rejects_the_mismatch() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        void Use() {
            Maker@ b;
            @b = MakeDistinct;
        }
        "#,
        "the mismatch in a plain handle assignment",
    );
}

/// PREDICTED RED: a class field initializer is a third declaration site.
#[test]
fn position_class_field_initializer_rejects_the_mismatch() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        class Holder { Maker@ cb = MakeDistinct; }
        "#,
        "the mismatch in a class field initializer",
    );
}

// ---------------------------------------------------------------------------
// Engine types, against the shipped typedb FIXTURE.
//
// The red tests above use workspace classes so they hold with no type DB at
// all. These pin the engine-typed axis the ticket asked for, using the
// in-repo fixture rather than this machine's install — so they survive the
// B045 hermetic gate, which scrubs every `OPENPLANET*` variable.
//
// Every expectation here is MEASURED, not assumed. The inheritance direction
// in particular went the way round I would not have guessed, and is pinned
// rather than argued.
// ---------------------------------------------------------------------------

fn fixture_index() -> TypeIndex {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    TypeIndex::load(
        &fixtures.join("OpenplanetCore.json"),
        &fixtures.join("OpenplanetNext.json"),
    )
    .expect("typedb fixtures must load")
}

fn engine_errors(index: &TypeIndex, source: &str) -> Vec<Diagnostic> {
    let uri = Url::parse("file:///test/b076_engine.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), Some(index))
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .collect()
}

#[test]
fn engine_matching_handle_return_still_binds() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        funcdef CMwNod@ Maker();
        CMwNod@ Make() { return null; }
        void Use() { Maker@ b = Make; }
        "#,
    );
    assert!(
        diags.is_empty(),
        "an exact engine-type match must bind: {diags:?}"
    );
}

/// Two engine types with no inheritance relation.
#[test]
fn engine_unrelated_handle_returns_are_rejected() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        funcdef CGameCtnBlock@ Maker();
        CSmPlayer@ Make() { return null; }
        void Use() { Maker@ b = Make; }
        "#,
    );
    assert!(
        !diags.is_empty(),
        "`CSmPlayer@` must not bind a `CGameCtnBlock@` funcdef"
    );
}

/// Returning a BASE where the funcdef promises a DERIVED is unsound, and is
/// rejected. Pinned separately from the covariant direction below, because
/// the two go opposite ways and a fix that collapsed them would pass one.
#[test]
fn engine_base_return_against_derived_funcdef_is_rejected() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        funcdef CGameCtnBlock@ Maker();
        CMwNod@ Make() { return null; }
        void Use() { Maker@ b = Make; }
        "#,
    );
    assert!(
        !diags.is_empty(),
        "`CMwNod@` (base) must not bind a `CGameCtnBlock@` (derived) funcdef"
    );
}

/// The covariant direction: returning a DERIVED where the funcdef promises a
/// BASE is accepted, via `is_external_derived_from` inside
/// `funcdef_types_compatible`.
///
/// MEASURED, not asserted from first principles — and it is the reason the
/// workspace-class version of this axis
/// (`binding_rejects_derived_handle_return_against_base_funcdef`) rejects
/// while this one accepts: that predicate only knows the EXTERNAL type
/// hierarchy, so workspace `Derived : Base` is invisible to it. Whether the
/// engine agrees with either answer needs a live probe (B068).
#[test]
fn engine_derived_return_against_base_funcdef_is_accepted() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        funcdef CMwNod@ Maker();
        CGameCtnBlock@ Make() { return null; }
        void Use() { Maker@ b = Make; }
        "#,
    );
    assert!(
        diags.is_empty(),
        "documented current behaviour: a derived return binds a base funcdef: {diags:?}"
    );
}

#[test]
fn engine_primitive_return_against_handle_funcdef_is_rejected() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        funcdef int Maker();
        CMwNod@ Make() { return null; }
        void Use() { Maker@ b = Make; }
        "#,
    );
    assert!(
        !diags.is_empty(),
        "`CMwNod@` must not bind an `int` funcdef"
    );
}

/// The message names a SIGNATURE, not the function's identifier.
///
/// `from 'G' to 'F@'` — the first rendering — put an identifier where a type
/// belongs and hid the very thing that differs. This pins the fix so it
/// cannot regress into naming the identifier again.
#[test]
fn the_message_names_the_signature_not_the_identifier() {
    let diags = errors(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        void Use() { Maker@ b = MakeDistinct; }
        "#,
    );
    let message = &diags.first().expect("expected a diagnostic").message;
    assert!(
        message.contains("'int()'"),
        "message must name the signature, got {message:?}"
    );
    assert!(
        !message.contains("'MakeDistinct'"),
        "message must not name the identifier as if it were a type, got {message:?}"
    );
}

/// Regression: a plugin `typedef` (or class/enum/interface) named after a
/// Core funcdef must not turn its declaration site into a funcdef slot.
///
/// Found by the B060 fixture going red during this fix: reaching for
/// `lookup_funcdef` on the declared type name resolved
/// `CoroutineFuncUserdataInt64` to the ENGINE funcdef, so
/// `CoroutineFuncUserdataInt64 x = 1;` — a plain `uint` — was reported as a
/// bad delegate bind. GH #38 says the plugin's own declaration wins, and
/// `arg_value_funcdef` already encodes that; the fix is to call it rather
/// than to ask `lookup_funcdef` directly.
#[test]
fn a_workspace_typedef_shadowing_a_core_funcdef_is_not_a_slot() {
    // MUST run with the typedb loaded. Without it there is no Core funcdef
    // of this name to shadow, `lookup_funcdef` finds nothing, and the test
    // passes trivially without ever reaching the guard — which is exactly
    // how the first version of this test passed while the B060 fixture,
    // which does load a typedb, stayed red.
    let diags = engine_errors(
        &fixture_index(),
        r#"
        typedef uint CoroutineFuncUserdataInt64;
        CoroutineFuncUserdataInt64 shadowAlias = 1;
        void Use() { CoroutineFuncUserdataInt64 local = 2; }
        "#,
    );
    assert!(
        diags.is_empty(),
        "a workspace typedef shadowing a Core funcdef must not become a \
         funcdef slot: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

/// Anti-vacuity control for the test above: with the SAME typedb and no
/// workspace declaration, the Core funcdef is live and a plain integer is
/// rejected. If this ever goes silent, the shadowing test above is passing
/// because nothing is being checked at all.
#[test]
fn the_core_funcdef_is_live_when_the_workspace_does_not_shadow_it() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        CoroutineFuncUserdataInt64 notShadowed = 1;
        "#,
    );
    assert!(
        !diags.is_empty(),
        "without a workspace declaration the Core funcdef must be live, or \
         the shadowing test proves nothing"
    );
}

/// The same shadowing rule for a workspace ENUM, so the guard is not pinned
/// only by the `typedef` shape that happened to surface it.
///
/// A workspace CLASS shadow was tried first and makes a poor test: the only
/// natural initializer is `CoroutineFunc()`, and that constructor call
/// already reports `expects 1 args, got 0` on master, independently of
/// anything here. Asserting silence there would have failed for a reason
/// this ticket did not cause and does not fix — a separate shadowing gap in
/// constructor resolution, B073's neighbourhood.
#[test]
fn a_workspace_enum_shadowing_a_core_funcdef_is_not_a_slot() {
    let diags = engine_errors(
        &fixture_index(),
        r#"
        enum CoroutineFuncUserdataBool { EA, EB }
        CoroutineFuncUserdataBool shadowEnum = CoroutineFuncUserdataBool::EA;
        "#,
    );
    assert!(
        diags.is_empty(),
        "a workspace enum shadowing a Core funcdef must not become a \
         funcdef slot: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}
