//! B144: a CONST HANDLE parameter (`T@ const &in`) was treated as a type the
//! checker had never heard of, so every funcdef signature comparison against
//! it failed.
//!
//! Reported by Max, 2026-09-01, against a shipped plugin that compiles in the
//! game (`tm-ghosts-plus-plus`, the ONLY error it reported):
//!
//! ```text
//! src/Interface.as:641:31: error: No matching signatures to 'array::Sort(SortGhosts)'
//! bool SortGhosts(const Json::Value@ const &in a, const Json::Value@ const &in b)
//! ```
//!
//! # Mechanism
//!
//! `TypeRepr::parse_type_string` had no rule for a TRAILING `const`. The
//! suffix chain it understands is `&in`/`&out`/`&inout`, `@`, `[]` and
//! `<...>`; `Json::Value@ const` matched none of them and fell through to the
//! `Named` arm as `Named("Json::Value@ const")` — a type whose NAME contains
//! its own qualifiers, and which is therefore equal to nothing at all. Every
//! comparison against a real `Handle(Const(Json::Value))` failed, which is
//! why the defect showed up identically at the funcdef constructor, at
//! implicit handle assignment, at argument passing and at `array<T>::Sort`.
//!
//! The parser was never at fault — the rendered diagnostic showed
//! `const Json::Value@ const &in` round-tripping correctly, because that text
//! is the stored SOURCE SLICE. `parse_type_string` is the separate string
//! path that turns a stored signature back into a type, and only it was
//! blind. `src/parser/parser.rs` has produced `Const(Handle(T))` for a
//! trailing `const` since iter 38 (AC20), and `receiver_is_const` /
//! `base_type_name` / `array_element_type` were all written against that
//! shape already. The string parser simply never built it.
//!
//! # The engine was measured first, and it moved the fix
//!
//! `scripts/probe_const_handle_funcdef.py`, 33 single-construct plugins
//! compiled by the live game (2026-09-01, OP 1.29.14). Full table with
//! verbatim engine messages: `docs/probes/b144-const-handle-funcdef.tsv`.
//!
//! The headline is not what the ticket expected. The engine is not lenient
//! about const handles — it is EXACT about them, and the reason Max's code
//! compiles is that his spelling is the one the engine actually wants:
//!
//! ```text
//! Where 'less' is 'bool Json::Value@[]::less(const Json::Value@const&in,
//!                                            const Json::Value@const&in)'
//! ```
//!
//! `array<T>::Sort`'s comparator, for a HANDLE element type `X@`, is
//! `const X@ const &in` — leading AND trailing const. Measured for
//! `Json::Value@`, a workspace class handle, an interface handle,
//! `array<int>@` and `dictionary@`; for a non-handle element type (`int`,
//! `uint`, `string`, `vec3`, an enum, a value class) it is plain
//! `const T&in`. So `const Json::Value@ &in` — the spelling this checker
//! demanded — is the one the engine REJECTS.
//!
//! # Why the fix is still not "match the engine exactly"
//!
//! Because the checker's stance on `const` at a funcdef slot is deliberate
//! NEUTRALITY, set by B102 and left in place by B110:
//! `funcdef_types_compatible` peels `Const`/`Handle` off both sides before
//! comparing. Every const position is therefore accepted at every other const
//! position — a false-NEGATIVE-only stance, which is the safe direction for a
//! rule whose failure mode is red squiggles on code that compiles.
//!
//! B144 was not the checker being neutral. It was the checker being neither:
//! `T@ const` was not peeled, because it was never parsed as a const handle
//! in the first place. The fix restores uniform neutrality; it does not
//! loosen the comparison, and it must not tighten it either. Every case
//! below that the engine rejects is listed as such, so the gap between this
//! checker and the engine is written down rather than implied.
//!
//! Tightening to the engine's exactness is a separate, measured change: it
//! would fire on all four const positions at once, including the leading one
//! B110 guards, and its risk polarity is the opposite of this ticket's.

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

fn errors(source: &str) -> Vec<String> {
    let uri = Url::parse("file:///test/b144.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), None)
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| d.message)
        .collect()
}

/// Anti-vacuity for the whole file. Every silence assertion below is
/// worthless unless this harness can report on a fixture of this shape.
#[test]
fn anti_vacuity_the_harness_reports_on_this_shape() {
    let msgs = errors("void Use() { ZZZNoSuchType q; }");
    assert!(
        msgs.iter().any(|m| m.contains("unknown type")),
        "the harness must be able to report on a plugin-shaped fixture: {msgs:?}"
    );
}

/// The four const positions AngelScript can spell on a handle parameter.
/// `%T%` is replaced by the element type name.
const CONST_POSITIONS: &[(&str, &str)] = &[
    ("plain", "%T%@"),
    ("leading", "const %T%@"),
    ("trailing", "%T%@ const"),
    ("both", "const %T%@ const"),
];

/// Handle-shaped parameter types, with the declarations each needs.
/// `&inout` is deliberately absent from the mode list below: the engine
/// refuses `T@ const &inout` at DECLARATION — "Only object types that
/// support object handles can use &inout" (2026-09-01, OP 1.29.14) — so
/// there is no legal program to test.
const HANDLE_TYPES: &[(&str, &str)] = &[
    ("workspace class", "class C { int v; }\n"),
    ("interface", "interface C { void M(); }\n"),
];

const MODES: &[&str] = &["&in", "&out", ""];

fn spell(pattern: &str, ty: &str) -> String {
    pattern.replace("%T%", ty)
}

/// A funcdef declared one way, a function declared another, bound through
/// one of the four paths that reach `func_matches_funcdef`.
fn program(decls: &str, fd_param: &str, fn_param: &str, mode: &str, path: &str) -> String {
    let sep = if mode.is_empty() { "" } else { " " };
    // No body. An `&out` parameter that IS a const handle cannot be written
    // to at all — the engine says "Expression is not an l-value" for
    // `@a = C();` through a `C@ const &out` (2026-09-01, OP 1.29.14), and so
    // does this checker, which is a true positive and has its own test
    // below. Writing to it here would make half the cross product fail for a
    // reason that has nothing to do with signature matching.
    let body = "";
    format!(
        "{decls}\
         funcdef bool FD({fd_param}{sep}{mode} a);\n\
         bool Fn({fn_param}{sep}{mode} a) {{ {body} return true; }}\n\
         void Slot(FD@ cb) {{ if (cb is null) {{ }} }}\n\
         void Use() {{\n\
         {path}\
         }}\n"
    )
}

const PATH_ASSIGN: &str = "    FD@ h = Fn;\n    if (h is null) { }\n";
const PATH_CTOR: &str = "    FD@ h = FD(Fn);\n    if (h is null) { }\n";
const PATH_ARG: &str = "    Slot(Fn);\n";

const PATHS: &[(&str, &str)] = &[
    ("implicit assignment", PATH_ASSIGN),
    ("explicit funcdef construction", PATH_CTOR),
    ("argument passing", PATH_ARG),
];

/// THE REGRESSION, narrowed by B152 to the claim that is B144's alone.
///
/// B144 shipped this as "every const position binds every OTHER position",
/// because the comparison was const-NEUTRAL at the time (B102's stance).
/// B152 measured the engine and made it const-EXACT, so the off-diagonal
/// cells are now errors and they are tested — in both directions, with their
/// engine verdicts — by `tests/b152_funcdef_const_exactness.rs`.
///
/// What survives here is the part B152 does not test and B144 exists for:
/// **each const position binds ITSELF**. That is not trivial and it is not
/// duplicated. Before B144, `C@ const` at a `C@ const` funcdef FAILED — the
/// trailing `const` was parsed into the type NAME, so the type was equal to
/// nothing, itself included. Two of the four positions could not match their
/// own spelling. A const-exactness rule layered on top of that parse would
/// have been exact about a type it could not read.
///
/// So: the diagonal, across every funcdef path, every legal mode and both
/// handle-shaped types. It leans on
/// `anti_vacuity_the_harness_reports_on_this_shape` above and on
/// `a_const_handle_is_not_a_wildcard` below.
#[test]
fn every_const_position_binds_itself_at_a_funcdef_slot() {
    let mut checked = 0usize;
    for (ty_label, decls) in HANDLE_TYPES {
        for (label, pat) in CONST_POSITIONS {
            for mode in MODES {
                for (path_label, path) in PATHS {
                    let src = program(decls, &spell(pat, "C"), &spell(pat, "C"), mode, path);
                    let msgs = errors(&src);
                    assert!(
                        msgs.is_empty(),
                        "{ty_label}: the `{label}` const position must bind its own                          spelling (mode `{mode}`, via {path_label}):\n{msgs:#?}\n{src}"
                    );
                    checked += 1;
                }
            }
        }
    }
    // A loop that ran zero times is a passing test. 2 types x 4 x 3 x 3.
    assert_eq!(checked, 72, "the cross product did not run in full");
}

/// THE REPORTED CASE, at the call path it was actually reported from.
///
/// B144 originally asserted this as `const C@ const &in` binding a
/// `const C@ &in` funcdef, which was true only while the comparison was
/// const-neutral — and the engine rejects that pair (measured, B144's own
/// corpus). The real report was `arr.Sort(SortGhosts)` on a `Json::Value@[]`,
/// where the slot's synthesised signature IS `const T@ const &in`, so this
/// now pins the shape Max shipped against the slot he shipped it into.
#[test]
fn the_reported_spelling_binds() {
    let src = "class C { int v; }\n\
               bool SortGhosts(const C@ const &in a, const C@ const &in b) { return true; }\n\
               void Use() {\n\
               \x20   C@[] arr;\n\
               \x20   arr.Sort(SortGhosts);\n\
               }\n";
    assert!(
        errors(src).is_empty(),
        "`const C@ const &in` is what Max's SortGhosts declares, and the Sort \
         slot is what it was passed to: {:#?}",
        errors(src)
    );
}

/// THE NON-VACUITY GUARD, and the one that stops this fix from being "make
/// everything match". A const handle must not become a wildcard: the
/// ELEMENT TYPE inside it is still compared.
///
/// Mutation-tested by construction — before the fix these already failed
/// (both sides were unequal `Named` strings), so passing here does not prove
/// the fix; what it proves is that the fix did not turn a const handle into
/// a hole. `checked` guards against the loop silently not running.
#[test]
fn a_const_handle_is_not_a_wildcard() {
    let decls = "class C { int v; }\nclass D { int w; }\n";
    let mut fired = 0usize;
    for (_, fd_pat) in CONST_POSITIONS {
        for (_, fn_pat) in CONST_POSITIONS {
            let src = program(
                decls,
                &spell(fd_pat, "C"),
                &spell(fn_pat, "D"),
                "&in",
                PATH_ASSIGN,
            );
            let msgs = errors(&src);
            assert!(
                !msgs.is_empty(),
                "a `D@` function must not bind a `C@` funcdef whatever the \
                 const spelling:\n{src}"
            );
            fired += 1;
        }
    }
    assert_eq!(fired, 16, "the cross product did not run in full");
}

/// The other three negative controls the ticket names, each carrying a
/// const handle so the fix cannot have widened them: wrong arity, wrong
/// return type, wrong parameter mode.
#[test]
fn const_handles_do_not_excuse_arity_return_or_mode_mismatches() {
    let decls = "class C { int v; }\n";
    for (what, funcdef, function) in [
        (
            "wrong arity",
            "funcdef bool FD(const C@ const &in a, const C@ const &in b);",
            "bool Fn(const C@ const &in a) { return true; }",
        ),
        (
            "wrong return type",
            "funcdef bool FD(const C@ const &in a);",
            "int Fn(const C@ const &in a) { return 1; }",
        ),
        (
            "wrong parameter mode",
            "funcdef bool FD(C@ const &in a);",
            "bool Fn(C@ const &out a) { @a = null; return true; }",
        ),
    ] {
        let src = format!(
            "{decls}{funcdef}\n{function}\n\
             void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
        );
        assert!(
            !errors(&src).is_empty(),
            "{what} must still be reported even with a const handle:\n{src}"
        );
    }
}

/// A const handle in the RETURN position parses the same way.
///
/// B144 asserted all four return spellings bound a `C@` funcdef, which was
/// const-neutrality talking. B152 made it exact — matching the engine, which
/// rejects both `C@ const` and `const C@` at a `C@` slot — so what is pinned
/// here is B144's own claim: each spelling binds ITSELF, which it could not
/// do before the trailing `const` parsed. The element type is still checked,
/// which is the half a wildcard would break.
#[test]
fn const_handle_return_types_bind_themselves_and_the_type_is_still_checked() {
    let decls = "class C { int v; }\nclass D { int w; }\n";
    for ret in ["C@", "const C@", "C@ const", "const C@ const"] {
        let src = format!(
            "{decls}funcdef {ret} FD();\n\
             {ret} Fn() {{ return null; }}\n\
             void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
        );
        assert!(
            errors(&src).is_empty(),
            "a `{ret}` return must bind a `{ret}` funcdef:\n{src}"
        );
    }
    for ret in ["D@", "const D@", "D@ const", "const D@ const"] {
        let src = format!(
            "{decls}funcdef C@ FD();\n\
             {ret} Fn() {{ return null; }}\n\
             void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
        );
        assert!(
            !errors(&src).is_empty(),
            "a `{ret}` return must NOT bind a `C@` funcdef:\n{src}"
        );
    }
}

/// An array OF const handles is an array of the element type, so
/// `array_element_type` must see through the new `Const(Handle(T))` shape.
/// Without this, the fix could have moved the defect from the parameter
/// position into the container position.
#[test]
fn an_array_of_const_handles_still_indexes_to_its_element() {
    let src = "class C { int v; }\n\
               void Use() {\n\
               \x20   C@ const[] xs;\n\
               \x20   int q = xs[0].v;\n\
               \x20   q += 1;\n\
               }\n";
    assert!(
        errors(src).is_empty(),
        "indexing an array of const handles must reach `C::v`: {:#?}",
        errors(src)
    );
    let bad = "class C { int v; }\n\
               void Use() {\n\
               \x20   C@ const[] xs;\n\
               \x20   int q = xs[0].ZZZNoSuchField;\n\
               \x20   q += 1;\n\
               }\n";
    assert!(
        !errors(bad).is_empty(),
        "and the member lookup must still be able to fail"
    );
}

/// A const HANDLE does not make the pointed-to object const, so calling a
/// mutating method through one is legal. `receiver_is_const` already drew
/// that line (`Const(Handle(_))` -> not const); before B144 no string-parsed
/// type could ever reach it in that shape, so the arm was unreachable from
/// this path. The paired row is the discriminator: `const C@` — the OTHER
/// const position — is the one that must complain.
#[test]
fn a_const_handle_does_not_make_the_pointee_const() {
    let mutating = "class C { int v; void Mutate() { v = 1; } }\n\
                    void Use(C@ const c) { c.Mutate(); }\n\
                    void Main() { Use(C()); }\n";
    assert!(
        errors(mutating).is_empty(),
        "a const HANDLE leaves the pointee mutable: {:#?}",
        errors(mutating)
    );
}

/// The engine's answer to "can you write through a `T@ const &out`": no.
///
/// ```text
/// bool F(C@ const &out a) { @a = C(); }
/// (6,51) ERR: Expression is not an l-value        (2026-09-01, OP 1.29.14)
/// ```
///
/// This checker already agreed, and the agreement is worth pinning because
/// B144's fix is what makes the const-handle shape reachable from a
/// string-parsed type at all. The paired row is the discriminator: the same
/// function without the trailing `const` must be silent, so the error is
/// attributable to the const handle and not to `&out` generally.
#[test]
fn writing_through_a_const_handle_out_parameter_is_rejected() {
    let bad = "class C { int v; }\n\
               bool Fn(C@ const &out a) { @a = C(); return true; }\n\
               void Use() { C@ c; Fn(c); }\n";
    assert!(
        !errors(bad).is_empty(),
        "a const HANDLE may not be rebound, and the engine agrees"
    );
    let ok = "class C { int v; }\n\
              bool Fn(C@ &out a) { @a = C(); return true; }\n\
              void Use() { C@ c; Fn(c); }\n";
    assert!(
        errors(ok).is_empty(),
        "without the trailing `const` the same write is legal: {:#?}",
        errors(ok)
    );
}
