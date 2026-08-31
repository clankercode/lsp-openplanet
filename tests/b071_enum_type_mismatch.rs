//! B071: enum type mismatches are not diagnosed.
//!
//! # The ground truth this file is written against
//!
//! The ticket's prose expectation was UNVERIFIED and is superseded by a live
//! RemoteBuild probe (ten cases, each its own compilation unit — never
//! batched, B068). That table is the specification here:
//!
//! | case | engine | required of the LSP |
//! |---|---|---|
//! | two engine enums assigned across | REJECT | error |
//! | two workspace enums assigned across | REJECT | error |
//! | undeclared member, workspace enum | REJECT | error |
//! | undeclared member, engine enum | REJECT | error |
//! | `int` -> enum | REJECT | error |
//! | enum -> `int` | ACCEPT | **silence** |
//! | cross-enum ARITHMETIC | ACCEPT | **silence** |
//!
//! Engine wording. The messages below match it CLOSELY BUT NOT EXACTLY, and
//! the difference is a recorded decision — see B155 and
//! `tests/b155_engine_wording_divergence.rs`, which enforces that the gap is
//! only ever the two marks named here.
//!
//! Measured from the game at 1.29.14 (B148 regeneration), the engine names an
//! assignment target as a REFERENCE and ends with a full stop:
//!
//! ```text
//! Can't implicitly convert from 'LocalOne' to 'LocalTwo&'.
//! Can't implicitly convert from 'int' to 'LocalOne&'.
//! No matching symbol 'LocalOne::NoSuchValueZZZ'
//! ```
//!
//! Ours deliberately drops the `&` and the full stop, so the assertions in
//! this file spell the target unsuffixed. (This block previously quoted a
//! third spelling — `to 'LocalTwo'.` — that matched neither side, which is
//! what B155 was opened on.)
//!
//! # Why the two silences live in the acceptance fixture
//!
//! `enum -> int` is LEGAL and cross-enum arithmetic is LEGAL, because enum
//! values decay to `int`. The asymmetry is the trap: `enum -> int` is
//! accepted, `int -> enum` is rejected, so a fix written as "enums and ints
//! are interchangeable" gets one of them backwards whichever way it guesses,
//! and a fix written as "two enum types in one expression is an error" ships
//! false errors on code the game compiles. Both silences therefore sit in the
//! SAME source as the six errors — a fix that over-fires cannot pass by
//! passing the error half.
//!
//! # Anti-vacuity
//!
//! Every engine-enum case needs a loaded typedb. With `type_index: None`
//! there is no `VirtualKey` to resolve, so a silence assertion would be
//! satisfied by a code path that never ran. `fixture_index()` loads the
//! repo's fixture DBs, and `anti_vacuity_*` below prove the engine enum is
//! actually live in exactly the configuration the other tests use.

use std::path::PathBuf;

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use openplanet_lsp::typedb::TypeIndex;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Url};

fn fixture_index() -> TypeIndex {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    TypeIndex::load(
        &fixtures.join("OpenplanetCore.json"),
        &fixtures.join("OpenplanetNext.json"),
    )
    .expect("typedb fixtures must load")
}

/// ERROR severity only — `compute_diagnostics` also emits unused-declaration
/// warnings, and every fixture here declares enum values it never reads.
fn errors_with_typedb(source: &str) -> Vec<Diagnostic> {
    let index = fixture_index();
    errors_with(source, Some(&index))
}

fn errors_with(source: &str, index: Option<&TypeIndex>) -> Vec<Diagnostic> {
    let uri = Url::parse("file:///test/b071.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), index)
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .collect()
}

fn messages(source: &str) -> Vec<String> {
    errors_with_typedb(source)
        .into_iter()
        .map(|d| d.message)
        .collect()
}

fn assert_rejected(source: &str, needle: &str, what: &str) {
    let msgs = messages(source);
    assert!(
        msgs.iter().any(|m| m.contains(needle)),
        "expected an error containing {needle:?} for {what}, got {msgs:?}:\n{source}"
    );
}

fn assert_accepted(source: &str, what: &str) {
    let msgs = messages(source);
    assert!(
        msgs.is_empty(),
        "expected silence for {what}, got {msgs:?}:\n{source}"
    );
}

// ---------------------------------------------------------------------------
// Anti-vacuity. Without these, every engine-enum assertion below could be
// satisfied by a checker that never resolved `VirtualKey` at all.
// ---------------------------------------------------------------------------

/// The engine enum IS live in this configuration: a real one is silent, and a
/// misspelt one is an unknown type. Both directions, because a harness that
/// reports nothing and a harness that reports everything each satisfy one of
/// them alone.
#[test]
fn anti_vacuity_engine_enum_is_live_with_the_fixture_typedb() {
    assert_accepted(
        r#"
        void Use() { VirtualKey k = VirtualKey::A; print("" + int(k)); }
        "#,
        "a real engine enum and a real member of it",
    );
    assert_rejected(
        r#"
        void Use() { VirtualKeyZZZ k; print("" + 1); }
        "#,
        "unknown type",
        "a misspelt engine enum type",
    );
}

/// The same fixture with NO typedb must not report the engine rows — proving
/// that the engine assertions elsewhere are carried by the loaded DB and not
/// by some name-shaped heuristic that would fire on any `Foo::Bar`.
#[test]
fn anti_vacuity_engine_rows_need_the_typedb() {
    let source = r#"
        void Use() { VirtualKey k = VirtualKey::NoSuchValueZZZ; print("" + int(k)); }
        "#;
    let without: Vec<String> = errors_with(source, None)
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert!(
        !without.iter().any(|m| m.contains("No matching symbol")),
        "with no typedb the engine enum does not exist, so nothing may claim \
         its members are wrong: {without:?}"
    );
}

// ---------------------------------------------------------------------------
// THE ACCEPTANCE FIXTURE: six required diagnostics and two required silences,
// in one source. Splitting them would let an over-firing fix pass.
// ---------------------------------------------------------------------------

const ACCEPTANCE: &str = r#"
enum LocalOne { X }
enum LocalTwo { Y }

void Acceptance() {
    // anti-vacuity, in the fixture itself: this must stay silent, so a later
    // silence means "accepted" rather than "never looked".
    VirtualKey live = VirtualKey::A;
    LocalOne ok = LocalOne::X;

    // --- 1. two engine enums ---------------------------------------------
    UI::MouseButton crossEngine = live;

    // --- 2. two workspace enums ------------------------------------------
    LocalTwo crossWorkspace = ok;

    // --- 3. undeclared member, workspace enum ----------------------------
    LocalOne badWorkspace = LocalOne::NoSuchValueZZZ;

    // --- 4. undeclared member, engine enum -------------------------------
    VirtualKey badEngine = VirtualKey::NoSuchValueZZZ;

    // --- 5. int -> enum, workspace ---------------------------------------
    LocalOne intToWorkspace = 5;

    // --- 6. int -> enum, engine ------------------------------------------
    VirtualKey intToEngine = 5;

    // --- SILENCE A: enum -> int is LEGAL ---------------------------------
    int toInt = LocalOne::X;

    // --- SILENCE B: cross-enum ARITHMETIC is LEGAL -----------------------
    int arith = LocalOne::X + LocalTwo::Y;

    print("" + int(crossEngine) + int(crossWorkspace) + int(badWorkspace)
            + int(badEngine) + int(intToWorkspace) + int(intToEngine)
            + toInt + arith);
}
"#;

/// The six. Asserted by message text because the engine's wording is part of
/// the requirement, not an implementation detail.
#[test]
fn acceptance_six_required_diagnostics() {
    let msgs = messages(ACCEPTANCE);
    let required = [
        "Can't implicitly convert from 'VirtualKey' to 'UI::MouseButton'",
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "No matching symbol 'LocalOne::NoSuchValueZZZ'",
        "No matching symbol 'VirtualKey::NoSuchValueZZZ'",
        "Can't implicitly convert from 'int' to 'LocalOne'",
        "Can't implicitly convert from 'int' to 'VirtualKey'",
    ];
    for needle in required {
        assert!(
            msgs.iter().any(|m| m.contains(needle)),
            "missing required diagnostic {needle:?}; got {msgs:?}"
        );
    }
}

/// The two silences, checked as an EXACT total against the six above: any
/// seventh error in this fixture is an over-fire, and the two legal lines are
/// the only places one can come from.
#[test]
fn acceptance_two_required_silences_and_nothing_more() {
    let msgs = messages(ACCEPTANCE);
    assert_eq!(
        msgs.len(),
        6,
        "the fixture has exactly six illegal lines; anything else is an \
         over-fire on `int i = LocalOne::X` or on cross-enum arithmetic, \
         both of which the engine ACCEPTS. Got: {msgs:?}"
    );
}

/// The same two silences in isolation, so a failure names which one broke
/// rather than only moving the count above.
#[test]
fn silence_enum_to_int_is_legal() {
    assert_accepted(
        r#"
        enum LocalOne { X }
        void Use() { int i = LocalOne::X; print("" + i); }
        "#,
        "enum -> int, which the engine ACCEPTS",
    );
}

#[test]
fn silence_cross_enum_arithmetic_is_legal() {
    assert_accepted(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        void Use() { int r = LocalOne::X + LocalTwo::Y; print("" + r); }
        "#,
        "cross-enum arithmetic, which the engine ACCEPTS (both decay to int)",
    );
}

/// Cross-enum arithmetic in the ENGINE flavour too — the decay rule is not a
/// workspace-only property, and a fix keyed on provenance would pass the test
/// above while failing this.
#[test]
fn silence_cross_enum_arithmetic_engine_flavour() {
    assert_accepted(
        r#"
        void Use() {
            int r = VirtualKey::A + UI::MouseButton::Left;
            print("" + r);
        }
        "#,
        "engine cross-enum arithmetic",
    );
}

/// Bitwise combination of an enum's own flags into an `int` — the single most
/// common shape in real Openplanet source. It is `enum -> int` wearing an
/// operator, and it must stay silent.
#[test]
fn silence_flag_combination_into_int() {
    assert_accepted(
        r#"
        void Use() {
            int flags = UI::WindowFlags::NoTitleBar | UI::WindowFlags::NoResize;
            print("" + flags);
        }
        "#,
        "OR-ing two members of one engine flag enum into an int",
    );
}

// ---------------------------------------------------------------------------
// POSITION AXIS. `check_init_conversion` is reached from exactly one of the
// four initializer positions (local var decl). B076 was that asymmetry, one
// ticket earlier. These tests are what stop it recurring here.
// ---------------------------------------------------------------------------

#[test]
fn position_local_var_decl() {
    assert_rejected(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        void Use() { LocalOne a = LocalOne::X; LocalTwo b = a; print("" + int(b)); }
        "#,
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "a cross-enum LOCAL declaration",
    );
}

#[test]
fn position_global_var_decl() {
    assert_rejected(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        LocalOne g_a = LocalOne::X;
        LocalTwo g_b = g_a;
        void Use() { print("" + int(g_b)); }
        "#,
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "a cross-enum GLOBAL declaration",
    );
}

#[test]
fn position_class_field_initializer() {
    assert_rejected(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        class Holder { LocalTwo f = LocalOne::X; }
        void Use() { Holder h; print("" + int(h.f)); }
        "#,
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "a cross-enum CLASS FIELD initializer",
    );
}

#[test]
fn position_assignment() {
    assert_rejected(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        void Use() {
            LocalOne a = LocalOne::X;
            LocalTwo b = LocalTwo::Y;
            b = a;
            print("" + int(b));
        }
        "#,
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "a cross-enum ASSIGNMENT",
    );
}

/// The `int -> enum` half at every position too, because the two rules share
/// a call site and a fix could wire only one of them through.
#[test]
fn position_int_to_enum_at_all_four_positions() {
    let cases = [
        (
            "local",
            r#"
            enum LocalOne { X }
            void Use() { LocalOne a = 5; print("" + int(a)); }
            "#,
        ),
        (
            "global",
            r#"
            enum LocalOne { X }
            LocalOne g_a = 5;
            void Use() { print("" + int(g_a)); }
            "#,
        ),
        (
            "class field",
            r#"
            enum LocalOne { X }
            class Holder { LocalOne f = 5; }
            void Use() { Holder h; print("" + int(h.f)); }
            "#,
        ),
        (
            "assignment",
            r#"
            enum LocalOne { X }
            void Use() { LocalOne a = LocalOne::X; a = 5; print("" + int(a)); }
            "#,
        ),
    ];
    for (position, source) in cases {
        assert_rejected(
            source,
            "Can't implicitly convert from 'int' to 'LocalOne'",
            &format!("int -> enum in {position} position"),
        );
    }
}

// ---------------------------------------------------------------------------
// GENERALISATION (repo CLAUDE.md): the same mismatch across the type
// modifiers a declaration can carry.
// ---------------------------------------------------------------------------

/// `const` on the target must not hide the mismatch — the check has to peel
/// it exactly as the initializer check does.
#[test]
fn variant_const_target_still_rejects() {
    assert_rejected(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        void Use() { LocalOne a = LocalOne::X; const LocalTwo b = a; print("" + int(b)); }
        "#,
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "a `const` cross-enum declaration",
    );
}

/// … and `const` on the SOURCE, which arrives wrapped the other way round.
#[test]
fn variant_const_source_still_rejects() {
    assert_rejected(
        r#"
        enum LocalOne { X }
        enum LocalTwo { Y }
        void Use() { const LocalOne a = LocalOne::X; LocalTwo b = a; print("" + int(b)); }
        "#,
        "Can't implicitly convert from 'LocalOne' to 'LocalTwo'",
        "a `const`-sourced cross-enum declaration",
    );
}

/// A parameter carries the enum type in by reference; assigning across
/// inside the body is the same mismatch reached through a different binding.
/// One case per reference mode, because the modes are separate code paths in
/// the type representation.
#[test]
fn variant_parameter_reference_modes() {
    for mode in [
        "const LocalOne &in",
        "LocalOne &in",
        "LocalOne &out",
        "LocalOne &inout",
    ] {
        let source = format!(
            r#"
            enum LocalOne {{ X }}
            enum LocalTwo {{ Y }}
            void Take({mode} a) {{ LocalTwo b = a; print("" + int(b)); }}
            void Use() {{ LocalOne v = LocalOne::X; Take(v); }}
            "#
        );
        assert_rejected(
            &source,
            "to 'LocalTwo'",
            &format!("a cross-enum assignment from a `{mode}` parameter"),
        );
    }
}

/// A namespaced workspace enum: the mismatch must survive qualification, and
/// the two spellings of the SAME enum must still compare equal (the second
/// half is the over-firing gate for canonicalization).
#[test]
fn variant_namespaced_workspace_enum() {
    assert_rejected(
        r#"
        namespace N { enum Inner { X } }
        enum Outer { Y }
        void Use() { N::Inner a = N::Inner::X; Outer b = a; print("" + int(b)); }
        "#,
        "to 'Outer'",
        "a namespaced workspace enum assigned to a global one",
    );
    assert_accepted(
        r#"
        namespace N {
            enum Inner { X }
            void Inside() { Inner a = Inner::X; N::Inner b = a; print("" + int(b)); }
        }
        "#,
        "two spellings of ONE namespaced enum",
    );
}

/// A namespaced ENGINE enum on both sides of the assignment: same enum, two
/// spellings, must stay silent. `UI::Cond` is reachable unqualified from
/// inside `namespace UI`-style code paths through the ancestor walk.
#[test]
fn variant_engine_enum_same_type_two_spellings_is_silent() {
    assert_accepted(
        r#"
        void Use() {
            UI::Cond a = UI::Cond::Always;
            UI::Cond b = a;
            print("" + int(b));
        }
        "#,
        "one engine enum assigned to itself",
    );
}

/// A global engine enum reached through a real scope's qualifier (B052's
/// ancestor walk): `UI::VirtualKey` and `VirtualKey` name ONE enum, so this
/// must stay silent. A fix comparing raw strings fires here.
#[test]
fn variant_engine_global_enum_through_ancestor_walk_is_silent() {
    assert_accepted(
        r#"
        void Use() {
            VirtualKey a = VirtualKey::A;
            UI::VirtualKey b = a;
            print("" + int(b));
        }
        "#,
        "one global engine enum written two ways",
    );
}

/// FLEET-MEASURED. Running base and branch binaries over 151 real plugins
/// produced exactly ONE new diagnostic, and it was a false positive:
/// `tm-editor-plus-plus` declares `enum Axis` and writes `Axis(c)`, and that
/// call resolved to the CORE enum `UI::Plot::Axis` through the short-name
/// rescue — so the plugin's own enum was reported as incompatible with
/// itself. The plugin's declaration wins, which is the rule
/// `workspace_shadows_funcdef` already applies to funcdefs.
///
/// This test is the reason the fix is not confined to the new check: the
/// mismatch report was correct given the types it was handed, and the wrong
/// half was the resolution feeding it.
#[test]
fn variant_workspace_enum_shadows_engine_enum_of_the_same_short_name() {
    assert_accepted(
        r#"
        enum Axis { X, Y, Z }
        void Use() {
            Axis a = Axis::X;
            Axis b = Axis(1);
            a = Axis(2);
            print("" + int(a) + int(b));
        }
        "#,
        "a workspace enum whose short name collides with the Core enum \
         `UI::Plot::Axis`",
    );
}

/// The same collision through the other declaration kinds a plugin can use,
/// so the fix is not keyed to `enum` specifically. `Cond` collides with
/// `UI::Cond`, `Marker` with `UI::Plot::Marker`.
#[test]
fn variant_workspace_declarations_shadow_engine_enum_short_names() {
    assert_accepted(
        r#"
        enum Cond { A, B }
        class Marker { int v; }
        void Use() {
            Cond c = Cond(1);
            Marker@ m = Marker();
            print("" + int(c) + m.v);
        }
        "#,
        "a workspace enum and a workspace class colliding with engine enum \
         short names",
    );
}

/// The over-firing gate for the gate: shadowing must not make the check go
/// blind. With `Axis` declared locally, a genuine cross-enum assignment
/// involving it is still an error.
#[test]
fn variant_shadowing_does_not_disable_the_check() {
    assert_rejected(
        r#"
        enum Axis { X }
        enum Other { Y }
        void Use() { Axis a = Axis::X; Other b = a; print("" + int(b)); }
        "#,
        "Can't implicitly convert from 'Axis' to 'Other'",
        "a cross-enum assignment where one enum shadows an engine short name",
    );
}

// ---------------------------------------------------------------------------
// UNDECLARED MEMBER: the other half of the ticket, generalised.
// ---------------------------------------------------------------------------

#[test]
fn bad_member_workspace_enum_in_every_position() {
    let cases = [
        (
            "initializer",
            "enum LocalOne { X }\nvoid Use() { LocalOne a = LocalOne::ZZZ; print(\"\" + int(a)); }",
        ),
        (
            "bare expression",
            "enum LocalOne { X }\nvoid Use() { print(\"\" + int(LocalOne::ZZZ)); }",
        ),
        (
            "argument",
            "enum LocalOne { X }\nvoid Take(int v) {}\nvoid Use() { Take(LocalOne::ZZZ); }",
        ),
        (
            "return",
            "enum LocalOne { X }\nLocalOne Give() { return LocalOne::ZZZ; }\nvoid Use() { print(\"\" + int(Give())); }",
        ),
    ];
    for (position, source) in cases {
        assert_rejected(
            source,
            "No matching symbol 'LocalOne::ZZZ'",
            &format!("an undeclared workspace enum member in {position} position"),
        );
    }
}

#[test]
fn bad_member_engine_enum_in_every_position() {
    let cases = [
        (
            "initializer",
            "void Use() { VirtualKey a = VirtualKey::ZZZ; print(\"\" + int(a)); }",
        ),
        (
            "bare expression",
            "void Use() { print(\"\" + int(VirtualKey::ZZZ)); }",
        ),
        (
            "namespaced engine enum",
            "void Use() { print(\"\" + int(UI::Cond::ZZZ)); }",
        ),
    ];
    for (position, source) in cases {
        assert_rejected(source, "No matching symbol", position);
    }
}

/// The over-firing gate for the member rule: every real member of a workspace
/// enum, an engine global enum, an engine namespaced enum, and an engine
/// DEEPLY namespaced enum must stay silent. This is the half that breaks if
/// the value list is read from the wrong place.
#[test]
fn real_members_stay_silent() {
    assert_accepted(
        r#"
        enum LocalOne { X, Y = 3, Z }
        void Use() {
            print("" + int(LocalOne::X) + int(LocalOne::Y) + int(LocalOne::Z));
            print("" + int(VirtualKey::A) + int(VirtualKey::Escape));
            print("" + int(UI::Cond::Always) + int(UI::MouseButton::Left));
            print("" + int(UI::Plot::Marker::Circle));
        }
        "#,
        "real members of workspace, global-engine, namespaced-engine and \
         deeply-namespaced-engine enums",
    );
}

/// A qualified name whose head is NOT an enum must be untouched by the member
/// rule — a namespace function, a class, and a plain namespace value each
/// keep whatever answer they had.
#[test]
fn non_enum_qualified_names_are_untouched() {
    assert_accepted(
        r#"
        namespace Ns { void Fn() {} int g_v = 1; }
        class Cls { int Field; }
        void Use() { Ns::Fn(); print("" + Ns::g_v); Cls c; c.Field = 1; print("" + c.Field); }
        "#,
        "a namespace function, a namespace value and a class field",
    );
}
