//! B100: a workspace FREE-FUNCTION overload set reached no argument check of
//! any kind — not arity, not types, not funcdef slots.
//!
//! # The mechanism
//!
//! `lookup_function_return` refuses as soon as two workspace symbols share a
//! name (the deliberate "ambiguous, say nothing" arm). Every entry point that
//! reaches [`resolve_workspace_function_call`] for a free function was gated
//! behind that lookup, so a set fell through to the `has_global_ident` arm,
//! which suppresses the undefined-identifier diagnostic and only walks the
//! arguments for their own errors. Nothing compared them to any signature.
//!
//! Workspace METHODS never had this hole: they produce real sets via
//! `lookup_method_overloads_with_inheritance`.
//!
//! B084 fixed the CREDITING half of this same arm — an overloaded global
//! function was called and never recorded as used. This is the CHECKING half.
//! One `lookup_function_return` restriction, two consequences.
//!
//! # How it was found, and why that is on the record
//!
//! By a test PASSING when it should not have. lane-enum's first draft of
//! B089's over-firing gate asserted that a set with one matching candidate
//! stays silent, and it passed VACUOUSLY because the set logic never ran on a
//! free-function set at all. It was rewritten onto methods, and
//! `a_workspace_free_function_overload_set_is_checked_since_b100` in
//! `tests/b089_funcdef_call_site.rs` pinned the reason. That test is now the
//! POSITIVE it said it should become.
//!
//! So every silence in this file carries a positive control in the SAME
//! fixture: a call that must error off the same overload set. Without one,
//! "the set matched" and "the set logic never ran" produce identical output —
//! which is the exact failure this ticket came from.
//!
//! # Baseline, measured before the change (a release build of master over a
//! fixture plugin)
//!
//! Nine rows silent: arity, primitive type and funcdef slot, each in the bare
//! spelling, the `NS::` spelling, and the bare-inside-the-namespace spelling.
//! The single-overload reference point in the same file — `One(1, 2, 3)` —
//! errored `function 'One' expects 1 args, got 3`, so the harness was not the
//! thing staying quiet.
//!
//! # What this does NOT change
//!
//! The set is handed to the SAME `resolve_workspace_function_call` the
//! single-overload and method paths already use. No new rule is decided here,
//! which is why a TYPE-driven no-match on a set stays silent: that resolver
//! is deliberately conservative there and has been since GH #34. Pinned below
//! rather than left to be rediscovered.

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

/// ERROR severity only: these fixtures declare helpers they never call, and
/// unused-declaration findings would swamp an unfiltered assertion.
fn errors(source: &str) -> Vec<String> {
    let uri = Url::parse("file:///test/b100.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), None)
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| d.message)
        .collect()
}

fn assert_rejected(source: &str, what: &str) {
    let msgs = errors(source);
    assert!(
        !msgs.is_empty(),
        "expected an error for {what}, got silence:\n{source}"
    );
}

/// Anti-vacuity for the whole file: the harness DOES report on this shape.
/// Every silence assertion below is worthless without it.
#[test]
fn anti_vacuity_the_harness_reports_on_this_shape() {
    let msgs = errors(
        r#"
        void Use() { ZZZNoSuchType q; }
        "#,
    );
    assert!(
        msgs.iter().any(|m| m.contains("unknown type")),
        "the harness must be able to report on a plugin-shaped fixture: {msgs:?}"
    );
}

/// The reference point the whole ticket is measured against: the SAME wrong
/// call against a set of ONE errors, and always has. Nothing about three
/// arguments to a one-argument function became wrong when a second overload
/// was declared; the second overload only made the checker stop looking.
#[test]
fn a_single_overload_free_function_already_rejects_bad_arity() {
    assert_rejected(
        r#"
        void One(int a) { int y = a; }
        void Use() { One(1, 2, 3); }
        "#,
        "a single-overload free function called with too many arguments",
    );
}

// ---------------------------------------------------------------------------
// ARITY. Generalised across the three spellings that reach three DIFFERENT
// arms of `call_type`: a bare identifier at top level, a `NS::`-qualified
// callee (`ExprKind::NamespaceAccess`), and a bare identifier resolved by the
// namespace ladder from inside the namespace. All three were gated behind the
// same `lookup_function_return`, and testing only one would have left two
// open in exactly the way this ticket describes.
// ---------------------------------------------------------------------------

const ARITY_BARE: &str = r#"
void Both(int a) { int y = a; }
void Both(int a, int b) { int y = a + b; }
void Use() { Both(1, 2, 3); }
"#;

const ARITY_QUALIFIED: &str = r#"
namespace NS {
    void Both(int a) { int y = a; }
    void Both(int a, int b) { int y = a + b; }
}
void Use() { NS::Both(1, 2, 3); }
"#;

const ARITY_LADDER: &str = r#"
namespace NS {
    void Both(int a) { int y = a; }
    void Both(int a, int b) { int y = a + b; }
    void Use() { Both(1, 2, 3); }
}
"#;

#[test]
fn arity_outside_every_candidate_is_rejected_in_all_three_spellings() {
    for (source, what) in [
        (ARITY_BARE, "a bare call to an overloaded free function"),
        (ARITY_QUALIFIED, "the `NS::` spelling of the same call"),
        (ARITY_LADDER, "the same call from inside the namespace"),
    ] {
        assert_rejected(source, what);
    }
}

/// The wording, asserted rather than assumed: this is the existing
/// arity-range message the single-overload and method paths already emit, not
/// a new one invented for sets.
#[test]
fn the_arity_message_is_the_shared_one() {
    let msgs = errors(ARITY_BARE);
    assert!(
        msgs.iter().any(|m| m.contains("expects") && m.contains("Both")),
        "the shared arity wording must name the callee: {msgs:?}"
    );
}

// ---------------------------------------------------------------------------
// FUNCDEF SLOTS — B089's subject, one position over.
// ---------------------------------------------------------------------------

const SLOT_BARE: &str = r#"
funcdef void VoidMaker();
funcdef int IntMaker();
string MakeString() { return ""; }
void Slot(VoidMaker@ cb) { if (cb is null) return; }
void Slot(IntMaker@ cb) { if (cb is null) return; }
void Use() { Slot(MakeString); }
"#;

const SLOT_QUALIFIED: &str = r#"
funcdef void VoidMaker();
funcdef int IntMaker();
string MakeString() { return ""; }
namespace NS {
    void Slot(VoidMaker@ cb) { if (cb is null) return; }
    void Slot(IntMaker@ cb) { if (cb is null) return; }
}
void Use() { NS::Slot(MakeString); }
"#;

const SLOT_LADDER: &str = r#"
funcdef void VoidMaker();
funcdef int IntMaker();
string MakeString() { return ""; }
namespace NS {
    void Slot(VoidMaker@ cb) { if (cb is null) return; }
    void Slot(IntMaker@ cb) { if (cb is null) return; }
    void Use() { Slot(MakeString); }
}
"#;

#[test]
fn a_funcdef_slot_no_candidate_accepts_is_rejected_in_all_three_spellings() {
    for (source, what) in [
        (SLOT_BARE, "a bare call into an overloaded funcdef slot"),
        (SLOT_QUALIFIED, "the `NS::` spelling of the same call"),
        (SLOT_LADDER, "the same call from inside the namespace"),
    ] {
        assert_rejected(source, what);
    }
}

/// B023's call-site wording, which the engine uses and which the method path
/// already emits for the identical set written on a class.
#[test]
fn the_funcdef_slot_message_is_the_shared_one() {
    let msgs = errors(SLOT_BARE);
    assert!(
        msgs.iter().any(|m| m.contains("No matching signatures")),
        "call-site wording, shared with B023 and B089: {msgs:?}"
    );
}

// ---------------------------------------------------------------------------
// THE OVER-FIRING GATE. Each silence sits in a fixture that ALSO errors off
// the same overload set, so "the candidate matched" is distinguishable from
// "the set logic never ran" — the discrimination this ticket was born from.
// ---------------------------------------------------------------------------

/// One candidate accepts the argument, so the call stands — and the second
/// call in the same fixture proves the set was actually consulted.
#[test]
fn a_call_matching_exactly_one_candidate_survives_while_a_bad_one_errors() {
    let msgs = errors(
        r#"
        funcdef void VoidMaker();
        funcdef int IntMaker();
        void MakeVoid() {}
        string MakeString() { return ""; }
        void Slot(VoidMaker@ cb) { if (cb is null) return; }
        void Slot(IntMaker@ cb) { if (cb is null) return; }
        void Use() {
            Slot(MakeVoid);     // the VoidMaker candidate accepts it
            Slot(MakeString);   // no candidate accepts it
        }
        "#,
    );
    assert_eq!(
        msgs.len(),
        1,
        "exactly the mismatched call must error. Two errors means one \
         candidate's verdict killed the set, which is what B060 was; zero \
         means the set logic never ran and the silence proves nothing. {msgs:?}"
    );
    assert!(
        msgs[0].contains("No matching signatures"),
        "and it must be the call-site wording: {msgs:?}"
    );
}

/// The same gate with THREE candidates, and the matching one is neither the
/// first nor the last. A set of one is a declaration wearing a disguise; a
/// set of three is where "rejecting a candidate is not rejecting the call"
/// actually has to hold.
#[test]
fn a_call_matching_one_of_three_candidates_survives_while_a_bad_one_errors() {
    let msgs = errors(
        r#"
        funcdef void VoidMaker();
        funcdef int IntMaker();
        funcdef string StringMaker();
        class Thing {}
        void MakeVoid() {}
        int MakeInt() { return 0; }
        string MakeString() { return ""; }
        Thing@ MakeThing() { return null; }
        void Slot(VoidMaker@ cb) { if (cb is null) return; }
        void Slot(IntMaker@ cb) { if (cb is null) return; }
        void Slot(StringMaker@ cb) { if (cb is null) return; }
        void Use() {
            Slot(MakeVoid);     // first candidate
            Slot(MakeInt);      // middle candidate
            Slot(MakeString);   // last candidate
            Slot(MakeThing);    // no candidate accepts it
        }
        "#,
    );
    assert_eq!(
        msgs.len(),
        1,
        "only the mismatched call may error out of a three-candidate set. \
         More means one candidate's verdict killed the set, which is what \
         B060 was: {msgs:?}"
    );
}

/// The non-vacuity gate stated as its own test, because it is the one this
/// ticket exists to enforce — and on the ARITY axis, so it is not the funcdef
/// gate above wearing a different name.
///
/// A silence and an absent check produce identical output. The pair is the
/// only thing that separates them: same set, same entry point, one argument
/// list changed.
#[test]
fn the_silences_above_are_not_vacuous() {
    let matching_only = errors(
        r#"
        void Both(int a) { int y = a; }
        void Both(int a, int b) { int y = a + b; }
        void Use() { Both(1); Both(1, 2); }
        "#,
    );
    assert!(
        matching_only.is_empty(),
        "both in-range calls must be silent: {matching_only:?}"
    );
    assert_rejected(
        r#"
        void Both(int a) { int y = a; }
        void Both(int a, int b) { int y = a + b; }
        void Use() { Both(1, 2, 3); }
        "#,
        "the same set with an argument count outside every candidate",
    );
}

/// MY OWN ERROR, kept as a test because the rule it tripped over is real and
/// was undocumented outside one paragraph of `report_workspace_funcdef_slot_
/// failure`'s doc comment.
///
/// Two of this file's gates were originally written with a NON-FUNCDEF
/// candidate in the set (`void Slot(int v)`) and asserted an error. They
/// stayed red after the fix, and the fix was not the thing that was wrong:
/// B089 deliberately rejects a candidate ONLY by a funcdef slot, so a
/// function-shaped argument meeting a non-funcdef parameter leaves that
/// candidate NEUTRAL and one neutral candidate keeps the whole call alive.
/// Widening that is B023's scope decision to reopen, and B060 is what
/// widening it cost.
///
/// Measured, not assumed: the identical set on a CLASS — the path that never
/// had B100's hole — is silent on the same call. So this is the resolver's
/// existing rule reached by a new door, not a gap B100 left open.
#[test]
fn a_non_funcdef_candidate_keeps_the_call_alive_as_it_does_for_methods() {
    let free = errors(
        r#"
        funcdef void VoidMaker();
        string MakeString() { return ""; }
        void Slot(VoidMaker@ cb) { if (cb is null) return; }
        void Slot(int v) { int y = v; }
        void Use() { Slot(MakeString); }
        "#,
    );
    let method = errors(
        r#"
        funcdef void VoidMaker();
        string MakeString() { return ""; }
        class C {
            void Slot(VoidMaker@ cb) { if (cb is null) return; }
            void Slot(int v) { int y = v; }
        }
        void Use() { C c; c.Slot(MakeString); }
        "#,
    );
    assert_eq!(
        free, method,
        "a free-function set and a method set must answer a neutral candidate \
         the same way; free={free:?} method={method:?}"
    );
    assert!(
        free.is_empty(),
        "and that shared answer is B089's deliberate narrowness: {free:?}"
    );
}

// ---------------------------------------------------------------------------
// WHAT IS DELIBERATELY LEFT SILENT, pinned so it is a decision rather than an
// oversight.
// ---------------------------------------------------------------------------

/// A TYPE-driven no-match on an overload set stays silent, and that is
/// `resolve_workspace_function_call`'s existing rule, not a gap this ticket
/// left open: its `OverloadMatch::NoMatch` arm diagnoses ARITY and nothing
/// else, because a conversion or a workspace-local argument may resolve
/// later. B100 routes free-function sets INTO that resolver; it does not get
/// to change what the resolver decides.
///
/// Measured, not assumed: the identical set written on a CLASS — the path
/// that never had this hole — is silent on the same call. Parity with the
/// method path is the target, so a future tightening belongs on the resolver
/// where it fixes both at once.
#[test]
fn a_primitive_type_no_match_on_a_set_stays_silent_as_it_does_for_methods() {
    let free = errors(
        r#"
        void Both(int a) { int y = a; }
        void Both(int a, int b) { int y = a + b; }
        void Use() { Both("x"); }
        "#,
    );
    let method = errors(
        r#"
        class C {
            void Both(int a) { int y = a; }
            void Both(int a, int b) { int y = a + b; }
        }
        void Use() { C c; c.Both("x"); }
        "#,
    );
    assert_eq!(
        free, method,
        "a free-function set and a method set must answer a type no-match the \
         same way; free={free:?} method={method:?}"
    );
    assert!(
        free.is_empty(),
        "and that shared answer is today's conservative silence: {free:?}"
    );
}
