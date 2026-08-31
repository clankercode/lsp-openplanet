//! B089: a funcdef-typed parameter accepts a mismatched function at a
//! WORKSPACE CALL SITE.
//!
//! The two red cases live in `tests/b076_funcdef_return_type.rs`
//! (`position_argument_already_rejects_the_same_mismatch` and
//! `position_workspace_call_with_differing_params`), where B076 left them
//! `#[ignore]`d as the specification of this ticket. This file is the
//! generalisation the repo's CLAUDE.md asks for: the same binding across
//! handle/const/reference forms, namespaces, methods, and — the part that
//! matters most — overload sets.
//!
//! # Why an overload set is the dangerous case
//!
//! The four positions B076 closed are declaration-shaped: one declared type,
//! one initializer. A call site is overload-shaped — N candidates — and
//! **rejecting a candidate is not rejecting the call**. B060 is the standing
//! demonstration of getting that backwards: widening B023's function-shaped
//! rule made one candidate's verdict kill the whole set, and real plugins
//! started reporting errors on lines the game compiles. So every
//! `overload_*` silence below sits in the same fixture as the errors, and
//! `acceptance_*` asserts an exact total rather than a lower bound.
//!
//! # Game probe: NOT AVAILABLE, and why
//!
//! The ticket asks for a live verdict before trusting LSP silence. The game
//! is up (RemoteBuild answers `get_status`), but its plugin-control routes
//! cannot be driven right now: RemoteBuild reads at most 64 bytes per socket
//! read and treats each chunk as a whole payload, so any request longer than
//! that is split into two unparseable halves. `load_plugin`'s payload is ~79
//! bytes before the id, so it can never be delivered, and `unload_plugin`
//! needs an id of 18 characters or fewer — shorter than the harness's own
//! `lsp-issue-repro-` prefix leaves room for. Every probe today therefore
//! stages a fixture, fails to load it, and fails to tear it down. Reported
//! to the coordinator; the one fixture this lane staged was verified never
//! loaded (no `Loaded plugin` line) and removed by hand.
//!
//! What the expectation rests on instead, stated so a later reader can weigh
//! it: the engine's verdict on this exact binding is already known from the
//! OTHER positions. `Maker@ b = MakeDistinct;` errors (B076, four positions)
//! and `Maker(MakeDistinct)` errors (B023's constructor path), and the
//! ticket's own framing is that the LSP currently gives two answers for one
//! binding depending on where it is written. This change makes the answer
//! uniform; it does not invent a new rule. The empirical guard in place of
//! the probe is a base-vs-branch sweep over 151 real plugins.

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

/// `type_index: None`, matching the B076 file: every fixture here is
/// workspace-only, so a typedb would add nothing and the B045 hermetic gate
/// scrubs `OPENPLANET*` anyway.
///
/// ERROR severity only — these fixtures declare things they never call, and
/// unused-declaration warnings would swamp an unfiltered assertion.
fn errors(source: &str) -> Vec<String> {
    let uri = Url::parse("file:///test/b089.as").expect("test uri");
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

fn assert_accepted(source: &str, what: &str) {
    let msgs = errors(source);
    assert!(
        msgs.is_empty(),
        "expected silence for {what}, got {msgs:?}:\n{source}"
    );
}

/// Anti-vacuity for the whole file: the harness DOES report on these
/// fixtures. Every silence assertion below is worthless without it.
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

// ---------------------------------------------------------------------------
// THE ACCEPTANCE FIXTURE: the errors and the over-firing gate in one source.
// ---------------------------------------------------------------------------

const ACCEPTANCE: &str = r#"
funcdef void Maker();

void MakeOk() {}
int MakeDistinct() { return 1; }
void MakeIt(int x) { int y = x; }

void TakeOne(Maker@ cb) { }

// An overload SET. It is written on a class deliberately — see
// `a_workspace_free_function_overload_set_is_a_different_gap` below: a
// free-function set never reaches any argument check, so a set written that
// way would assert its silence vacuously.
class Both {
    void Go(Maker@ cb) { }
    void Go(int v) { int y = v; }
}

void Acceptance() {
    Both b;

    // --- required silences -----------------------------------------------
    TakeOne(MakeOk);        // exact match
    TakeOne(null);          // null binds any funcdef handle
    b.Go(MakeOk);           // overload set: the funcdef candidate matches
    b.Go(7);                // overload set: the int candidate matches

    // --- required errors --------------------------------------------------
    TakeOne(MakeDistinct);  // return type differs
    TakeOne(MakeIt);        // parameter list differs
}
"#;

#[test]
fn acceptance_exactly_two_errors_and_four_silences() {
    let msgs = errors(ACCEPTANCE);
    assert_eq!(
        msgs.len(),
        2,
        "two mismatched binds must error and the four legal calls must not. \
         A third error means the overload set was killed by one candidate's \
         verdict, which is exactly what B060 was. Got: {msgs:?}"
    );
    assert!(
        msgs.iter().all(|m| m.contains("No matching signatures")),
        "call-site wording, matching the engine and B023's external path: {msgs:?}"
    );
}

// ---------------------------------------------------------------------------
// The over-firing gate on its own, so a failure names the cause.
//
// All of these use METHODS. Measured, not assumed: a workspace FREE-function
// overload set never reaches `resolve_workspace_function_call` at all, because
// `lookup_function_return` returns `None` the moment two workspace symbols
// share a name. A silence test written on a free-function set would therefore
// pass without the set logic ever running — Mechanism 3, an oracle that cannot
// discriminate what it asserts. `a_workspace_free_function_overload_set_is_a_
// different_gap` pins that separately so the reason is on the record.
// ---------------------------------------------------------------------------

/// One candidate's funcdef matching keeps the call alive. This is THE test
/// this ticket exists to not break.
#[test]
fn overload_set_stays_silent_when_one_candidate_matches() {
    assert_accepted(
        r#"
        funcdef void Maker();
        void MakeOk() {}
        class Both { void Go(Maker@ cb) { } void Go(int v) { int y = v; } }
        void Use() { Both b; b.Go(MakeOk); }
        "#,
        "an overload set where the funcdef candidate binds",
    );
}

/// The same set called with something only the NON-funcdef candidate accepts.
/// The funcdef candidate rejects an `int`, and that rejection must not carry
/// the call.
#[test]
fn overload_set_stays_silent_when_the_other_candidate_matches() {
    assert_accepted(
        r#"
        funcdef void Maker();
        class Both { void Go(Maker@ cb) { } void Go(int v) { int y = v; } }
        void Use() { Both b; b.Go(7); }
        "#,
        "an overload set where the non-funcdef candidate binds",
    );
}

/// Two funcdef candidates, one of which matches. A set can only fire when
/// EVERY arity-compatible candidate is rejected, and this is the shape that
/// distinguishes "every" from "any".
#[test]
fn overload_set_of_two_funcdefs_stays_silent_when_either_matches() {
    assert_accepted(
        r#"
        funcdef void VoidMaker();
        funcdef int IntMaker();
        void MakeVoid() {}
        int MakeInt() { return 1; }
        class Both {
            void Go(VoidMaker@ cb) { }
            void Go(IntMaker@ cb) { }
        }
        void Use() { Both b; b.Go(MakeVoid); b.Go(MakeInt); }
        "#,
        "two funcdef candidates, each matched by one call",
    );
}

/// … and the same set with a function matching NEITHER candidate does fire.
/// Paired with the test above so "every candidate rejected" is measured in
/// both directions rather than only the safe one — a rule that never fires
/// passes every silence test there is.
#[test]
fn overload_set_of_two_funcdefs_rejects_when_neither_matches() {
    assert_rejected(
        r#"
        funcdef void VoidMaker();
        funcdef int IntMaker();
        string MakeString() { return ""; }
        class Both {
            void Go(VoidMaker@ cb) { }
            void Go(IntMaker@ cb) { }
        }
        void Use() { Both b; b.Go(MakeString); }
        "#,
        "an overload set of two funcdefs, neither of which the arg matches",
    );
}

/// A mixed set — one funcdef candidate, one not — with an argument the
/// funcdef candidate rejects. The non-funcdef candidate is left NEUTRAL by
/// this rule rather than rejected, so the set survives and nothing is
/// reported. That is deliberately weaker than the engine, and it is the
/// direction B060 says to err in; widening it is B023's scope to reopen.
#[test]
fn a_mixed_set_stays_silent_because_the_non_funcdef_candidate_is_neutral() {
    assert_accepted(
        r#"
        funcdef void Maker();
        string MakeString() { return ""; }
        class Both { void Go(Maker@ cb) { } void Go(int v) { int y = v; } }
        void Use() { Both b; b.Go(MakeString); }
        "#,
        "a mixed overload set where only the funcdef candidate rejects the arg",
    );
}

/// PRE-EXISTING, SEPARATE GAP, pinned so the silence above is not read as
/// this ticket's doing. A workspace FREE-function overload set reaches no
/// argument check of any kind: `lookup_function_return` refuses as soon as
/// two workspace symbols share the name, so `call_type` falls through to the
/// `has_global_ident` arm and only walks the arguments. Arity, primitive
/// types and funcdef slots are all unchecked there.
///
/// Closing that is a different change from this one — it means giving the
/// Ident-callee path an overload-set entry point — and it is reported
/// separately. If this test ever starts failing because the call DOES error,
/// that gap was closed and this assertion should become the positive.
#[test]
fn a_workspace_free_function_overload_set_is_a_different_gap() {
    assert_accepted(
        r#"
        funcdef void VoidMaker();
        funcdef int IntMaker();
        string MakeString() { return ""; }
        void Both(VoidMaker@ cb) { }
        void Both(IntMaker@ cb) { }
        void Use() { Both(MakeString); }
        "#,
        "a workspace FREE-function overload set, which no argument check reaches",
    );
}

/// Arity puts the funcdef candidate out of the running entirely. A candidate
/// that cannot take this many arguments is not a candidate, and must not be
/// consulted for evidence.
#[test]
fn a_candidate_outside_the_arity_range_is_not_consulted() {
    assert_accepted(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        class Both {
            void Go(Maker@ cb) { }
            void Go(int a, int b) { int y = a + b; }
        }
        void Use() { Both b; b.Go(1, 2); }
        "#,
        "a two-argument call where only the non-funcdef candidate has that arity",
    );
}

// ---------------------------------------------------------------------------
// PARAMETER SHAPE AXIS (repo CLAUDE.md): the same call-site binding across
// every form a funcdef-typed parameter can be written in.
// ---------------------------------------------------------------------------

/// A mismatched function must be rejected however the slot is decorated, and
/// a matching one must bind through the same decoration. Both directions per
/// shape, because a shape the check simply cannot SEE passes the silence half
/// on its own.
#[test]
fn parameter_shapes_reject_a_mismatch_and_accept_a_match() {
    for slot in [
        "Maker@ cb",
        "Maker@ &in cb",
        "const Maker@ cb",
        "const Maker@ &in cb",
    ] {
        let reject = format!(
            r#"
            funcdef void Maker();
            int MakeDistinct() {{ return 1; }}
            void Take({slot}) {{}}
            void Use() {{ Take(MakeDistinct); }}
            "#
        );
        assert_rejected(&reject, &format!("a mismatched function into `{slot}`"));

        let accept = format!(
            r#"
            funcdef void Maker();
            void MakeOk() {{}}
            void Take({slot}) {{}}
            void Use() {{ Take(MakeOk); }}
            "#
        );
        assert_accepted(&accept, &format!("a matching function into `{slot}`"));
    }
}

/// `&out` and `&inout` take an l-value, so the mismatch arrives as a
/// funcdef-typed VARIABLE rather than a function reference — a different arm
/// of `arg_fails_funcdef_slot` (`arg_value_funcdef`) than the shapes above.
#[test]
fn out_and_inout_slots_compare_a_funcdef_typed_variable() {
    for mode in ["&out", "&inout"] {
        let reject = format!(
            r#"
            funcdef void Maker();
            funcdef int Other();
            void Take(Maker@ {mode} cb) {{}}
            void Use() {{ Other@ wrong; Take(wrong); }}
            "#
        );
        assert_rejected(
            &reject,
            &format!("a differently-typed funcdef handle into a `{mode}` slot"),
        );

        let accept = format!(
            r#"
            funcdef void Maker();
            void Take(Maker@ {mode} cb) {{}}
            void Use() {{ Maker@ right; Take(right); }}
            "#
        );
        assert_accepted(
            &accept,
            &format!("a matching funcdef handle into a `{mode}` slot"),
        );
    }
}

/// A funcdef-typed variable rather than a bare function name, at the plain
/// slot too — the same two arms, without the reference decoration.
#[test]
fn a_funcdef_typed_variable_is_compared_by_its_own_signature() {
    assert_rejected(
        r#"
        funcdef void Maker();
        funcdef int Other();
        void Take(Maker@ cb) {}
        void Use() { Other@ wrong; Take(wrong); }
        "#,
        "a `Other@` variable bound to a `Maker@` slot",
    );
    assert_accepted(
        r#"
        funcdef void Maker();
        void Take(Maker@ cb) {}
        void Use() { Maker@ right; Take(right); }
        "#,
        "a `Maker@` variable bound to a `Maker@` slot",
    );
}

// ---------------------------------------------------------------------------
// CALLEE SHAPE AXIS: the same slot reached through every kind of workspace
// call, because the gap was positional and each of these is its own position.
// ---------------------------------------------------------------------------

#[test]
fn namespaced_free_function() {
    assert_rejected(
        r#"
        namespace NS {
            funcdef void Maker();
            void Take(NS::Maker@ cb) {}
        }
        int MakeDistinct() { return 1; }
        void Use() { NS::Take(MakeDistinct); }
        "#,
        "a namespaced funcdef slot on a namespaced free function",
    );
    assert_accepted(
        r#"
        namespace NS {
            funcdef void Maker();
            void Take(NS::Maker@ cb) {}
        }
        void MakeOk() {}
        void Use() { NS::Take(MakeOk); }
        "#,
        "a matching function into a namespaced funcdef slot",
    );
}

/// The unqualified spelling of the same call from inside the namespace — a
/// different branch of the Ident-callee walk from the qualified one above.
#[test]
fn unqualified_call_from_inside_the_namespace() {
    assert_rejected(
        r#"
        int MakeDistinct() { return 1; }
        namespace NS {
            funcdef void Maker();
            void Take(Maker@ cb) {}
            void Use() { Take(MakeDistinct); }
        }
        "#,
        "an unqualified namespaced call with a mismatched function",
    );
}

#[test]
fn workspace_class_method() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        class Holder { void TakeM(Maker@ cb) {} }
        void Use() { Holder h; h.TakeM(MakeDistinct); }
        "#,
        "a mismatched function into a funcdef param on a workspace METHOD",
    );
    assert_accepted(
        r#"
        funcdef void Maker();
        void MakeOk() {}
        class Holder { void TakeM(Maker@ cb) {} }
        void Use() { Holder h; h.TakeM(MakeOk); }
        "#,
        "a matching function into a funcdef param on a workspace METHOD",
    );
}

/// An INHERITED method. The overload lookup walks the class hierarchy, so a
/// fix that only read the receiver's own members would pass the test above
/// and fail this.
#[test]
fn inherited_workspace_class_method() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        class Base { void TakeM(Maker@ cb) {} }
        class Derived : Base {}
        void Use() { Derived d; d.TakeM(MakeDistinct); }
        "#,
        "a mismatched function into a funcdef param on an INHERITED method",
    );
}

/// A named argument binds by parameter name and may skip defaulted ones, so
/// the slot it lands on is not its positional index. A check that assumed
/// positional binding would compare against the wrong parameter.
#[test]
fn named_argument_binds_to_the_right_slot() {
    assert_rejected(
        r#"
        funcdef void Maker();
        int MakeDistinct() { return 1; }
        void Take(int first = 0, Maker@ cb = null) {}
        void Use() { Take(cb: MakeDistinct); }
        "#,
        "a mismatched function passed by NAME into a skipped-over funcdef slot",
    );
    assert_accepted(
        r#"
        funcdef void Maker();
        void MakeOk() {}
        void Take(int first = 0, Maker@ cb = null) {}
        void Use() { Take(cb: MakeOk); }
        "#,
        "a matching function passed by NAME into a skipped-over funcdef slot",
    );
}

// ---------------------------------------------------------------------------
// FALSE-POSITIVE GATES. Each of these is a shape B023's guards deliberately
// leave neutral, and the call-site position must inherit every one of them.
// ---------------------------------------------------------------------------

/// `null` binds any funcdef handle.
#[test]
fn null_binds_any_funcdef_slot() {
    assert_accepted(
        r#"
        funcdef void Maker();
        void Take(Maker@ cb) {}
        void Use() { Take(null); }
        "#,
        "null into a funcdef slot",
    );
}

/// A parameter that is NOT a funcdef is untouched by this rule — including
/// when a function reference is passed to it. Widening to that case is
/// B023's scope and B060 is what it cost; this test pins that the call-site
/// position did not quietly take it on.
#[test]
fn a_non_funcdef_parameter_is_left_alone() {
    assert_accepted(
        r#"
        void MakeOk() {}
        void TakeInt(int v) { int y = v; }
        void Use() { TakeInt(MakeOk); }
        "#,
        "a function reference into a plain `int` parameter, which this rule \
         deliberately does not judge",
    );
}

/// A class handle the checker cannot resolve to a callable stays neutral —
/// the GH #47 guard. Here the argument is a workspace class instance, which
/// `arg_is_known_non_callable` DOES resolve, so the pair below is the
/// discriminating one: a resolvable non-callable rejects, an opaque shape
/// does not.
#[test]
fn a_workspace_class_instance_is_not_a_callable() {
    assert_rejected(
        r#"
        funcdef void Maker();
        class Thing {}
        void Take(Maker@ cb) {}
        void Use() { Thing@ t = Thing(); Take(t); }
        "#,
        "a workspace class handle bound to a funcdef slot",
    );
}

/// An argument whose own type failed to resolve must not be judged: an
/// `Error` type is the absence of information, and reporting on it turns one
/// diagnostic into two.
#[test]
fn an_unresolvable_argument_adds_no_second_diagnostic() {
    let msgs = errors(
        r#"
        funcdef void Maker();
        void Take(Maker@ cb) {}
        void Use() { Take(zzzNoSuchThing); }
        "#,
    );
    assert_eq!(
        msgs.len(),
        1,
        "the undefined identifier is the only error; the funcdef slot must \
         stay neutral on an Error-typed arg: {msgs:?}"
    );
}

/// A recursive / self-referential call and a call inside the funcdef's own
/// declaration file must not confuse the lookup. Cheap, and it is the shape
/// that broke when the check was first wired before `type_args_once` existed.
#[test]
fn arguments_are_typed_exactly_once() {
    let msgs = errors(
        r#"
        funcdef void Maker();
        void Take(Maker@ cb) {}
        void Use() { Take(zzzNoSuchThing); Take(zzzNoSuchThing); }
        "#,
    );
    assert_eq!(
        msgs.len(),
        2,
        "two undefined-identifier errors, one per call — a double walk of the \
         argument subtree would report four: {msgs:?}"
    );
}

/// B060 adjacency, pinned because the ticket names it as the precedent to
/// check against. A virtual-property READ is a value, not a function
/// reference, and B060 exists because the function-shaped rule classified it
/// as one and killed whole overload sets. Passing such a read to a funcdef
/// slot must be rejected — the engine agrees, verbatim from B060's own probe:
/// `UintFn@ bad = UintFn(Off);` → `No matching signatures to 'UintFn(uint)'`
/// — while passing it to a NON-funcdef parameter alongside must stay silent,
/// which is the half B060 got wrong.
///
/// Both halves in one fixture, because a fix that rejects everything passes
/// the first on its own.
#[test]
fn a_property_read_is_a_value_at_a_funcdef_slot_and_at_a_plain_one() {
    let msgs = errors(
        r#"
        funcdef void Maker();
        uint get_Off() { return 1; }
        void TakeCb(Maker@ cb) { }
        void TakeUint(uint v) { uint y = v; }
        void Use() { TakeUint(Off); TakeCb(Off); }
        "#,
    );
    assert_eq!(
        msgs.len(),
        1,
        "the `uint` slot must accept the property read (B060) and the funcdef \
         slot must reject it: {msgs:?}"
    );
    assert!(
        msgs[0].contains("TakeCb"),
        "the one error must be the funcdef slot, not the uint one: {msgs:?}"
    );
}
