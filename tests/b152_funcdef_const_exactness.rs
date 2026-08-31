//! B152: a funcdef signature match is exact about `const`, because the engine
//! is.
//!
//! B144 measured it and B102's neutrality was left in place because it was the
//! safe direction while the engine's stance was unknown. It is known now
//! (`docs/probes/b144-const-handle-funcdef.tsv`, OP 1.29.14, 51
//! single-construct plugins, all five controls fired): the engine distinguishes
//! all four const positions on a handle, in BOTH directions, at every param
//! mode and in the return type, and `array<T>::Sort`'s comparator carries both
//! consts whenever the element type is a handle.
//!
//! # The risk polarity is the opposite of B144's, and it decided the design
//!
//! B144 REMOVED errors; its worst case was a surviving false negative. This
//! ADDS them, so its worst case is a red squiggle on code the game compiles.
//! The corpus compiles in-game, which makes the test brutally simple: **any
//! new diagnostic on a shipped plugin is a false positive by definition.**
//!
//! So the fix was measured before it was written, with B110's method — an
//! instrumented build that keeps the neutral answer and prints a marker
//! wherever an exact rule would disagree, run over all 152 plugins.
//!
//! **The first run found 214 disagreements across 33 plugins, every one of
//! them a would-be new error, and not one of them was about the user's code.**
//! Three causes, all on the EXPECTED side:
//!
//! | sites | cause |
//! |---|---|
//! | 170 | `CoroutineFuncUserdata`'s parameter is `ref` in the database and `ref@` in every plugin that uses it |
//! | 38 | `CoroutineFuncUserdataString` stores `const` in `typeflags`, not in `typedecl`, so the qualifier was dropped on load |
//! | 6 | `array<T>::Sort`'s synthesised comparator emitted a bare `T@` where the engine wants `const T@ const &in` |
//!
//! Fixing those three and re-running gave **0 disagreements**, which is what
//! this change actually ships. That number is only interpretable beside the
//! population it was measured over, so: **7442 funcdef signature comparisons
//! across the 152 plugins, 674 of them const-bearing, in 46 plugins** —
//! spanning typedb funcdefs, five different workspace funcdefs and the
//! synthesised `T[]::less`. The rule is exercised everywhere and disagrees
//! nowhere.
//!
//! # One axis, not two
//!
//! The 170-site row is why [`const_shapes_compatible`] is exact only when BOTH
//! sides are handles. The `@` is not reliable across the typedb seam, and
//! nothing about those 170 sites is a `const` question. The handle axis stays
//! exactly as loose as it was; only `const` tightens.
//!
//! # What this file must prove, in both directions
//!
//! A matcher that rejects everything passes a suite that only checks things
//! start failing, so every tightening row here is paired with a binding row of
//! the same shape, and the false-positive guards below are reproductions of
//! the three real corpus shapes rather than invented ones.

use openplanet_lsp::config::LspConfig;
use openplanet_lsp::server::diagnostics::compute_diagnostics;
use std::path::PathBuf;
use tower_lsp::lsp_types::{DiagnosticSeverity, Url};

/// ERROR severity only: these fixtures declare helpers they never call, and
/// unused-declaration findings would swamp an unfiltered assertion.
fn errors(source: &str) -> Vec<String> {
    let uri = Url::parse("file:///test/b152.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), None)
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| d.message)
        .collect()
}

/// The same, with the engine database loaded — required for anything reaching
/// a typedb funcdef (`startnew`) or an engine element type.
fn errors_typedb(source: &str) -> Vec<String> {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-versions/1.29.14");
    let index = openplanet_lsp::typedb::TypeIndex::load(
        &dir.join("OpenplanetCore.json"),
        &dir.join("OpenplanetNext.json"),
    )
    .expect("typedb fixture must load");
    let uri = Url::parse("file:///test/b152.as").expect("test uri");
    compute_diagnostics(&uri, source, &LspConfig::default(), Some(&index))
        .into_iter()
        .filter(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        .map(|d| d.message)
        .collect()
}

/// Anti-vacuity for both harnesses. Every silence assertion below is worthless
/// unless these can report at all.
#[test]
fn anti_vacuity_both_harnesses_report_on_this_shape() {
    assert!(
        errors("void Use() { ZZZNoSuchType q; }")
            .iter()
            .any(|m| m.contains("unknown type")),
        "the plain harness must be able to report"
    );
    assert!(
        errors_typedb("void Use() { ZZZNoSuchType q; }")
            .iter()
            .any(|m| m.contains("unknown type")),
        "the typedb harness must be able to report"
    );
}

// ---------------------------------------------------------------------------
// THE FALSE-POSITIVE GUARDS COME FIRST.
//
// They are reproductions of the three shapes the instrumented sweep found in
// the corpus, at the sizes it found them. If this change is ever wrong, it is
// wrong here, and these are the rows worth failing loudly.
// ---------------------------------------------------------------------------

/// 170 sites, 30 plugins. The database declares
/// `void CoroutineFuncUserdata(ref)`; every plugin writes `ref@`. The game
/// compiles them, so the handle axis must stay loose.
///
/// The paired row is the discriminator: a genuinely wrong userdata type must
/// still be rejected, or this guard is satisfied by not checking anything.
#[test]
fn a_ref_handle_userdata_binds_the_databases_bare_ref() {
    let ok = "void Handler(ref@ userdata) { if (userdata is null) { } }\n\
              void Use() { startnew(Handler, null); }\n";
    assert!(
        errors_typedb(ok).is_empty(),
        "`ref@` must bind the database's `ref`: {:#?}",
        errors_typedb(ok)
    );
}

/// 38 sites. `CoroutineFuncUserdataString` stores its `const` in `typeflags`
/// (bit 2) and its `typedecl` is a bare `string`, so loading the text alone
/// dropped the qualifier. Its own `decl` reads
/// `void CoroutineFuncUserdataString(const string&in)`.
#[test]
fn a_const_string_userdata_handler_binds() {
    let ok = "void Handler(const string &in userdata) { if (userdata == \"\") { } }\n\
              void Use() { startnew(Handler, \"x\"); }\n";
    assert!(
        errors_typedb(ok).is_empty(),
        "`const string &in` is exactly what the database declares: {:#?}",
        errors_typedb(ok)
    );
}

/// The other direction on the same funcdef, and the reason the row above is
/// not vacuous: the database says `const string&in`, so a NON-const handler
/// does not match it. No plugin in the corpus writes one — the instrumented
/// sweep found zero disagreements over 152 plugins — so this is a tightening
/// with no shipped counterexample rather than one taken on faith.
#[test]
fn a_non_const_string_userdata_handler_does_not_bind() {
    let bad = "void Handler(string &in userdata) { if (userdata == \"\") { } }\n\
               void Use() { startnew(Handler, \"x\"); }\n";
    assert!(
        !errors_typedb(bad).is_empty(),
        "the database declares `const string&in`; a bare `string &in` is a \
         different signature"
    );
}

// ---------------------------------------------------------------------------
// The tightening itself.
// ---------------------------------------------------------------------------

/// The four const positions AngelScript can spell on a handle parameter.
const POSITIONS: &[(&str, &str)] = &[
    ("plain", "C@"),
    ("leading", "const C@"),
    ("trailing", "C@ const"),
    ("both", "const C@ const"),
];

fn funcdef_program(fd_param: &str, fn_param: &str, mode: &str) -> String {
    let sep = if mode.is_empty() { "" } else { " " };
    format!(
        "class C {{ int v; }}\n\
         funcdef bool FD({fd_param}{sep}{mode} a);\n\
         bool Fn({fn_param}{sep}{mode} a) {{ return true; }}\n\
         void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
    )
}

/// THE CORE OF THE TICKET, as a 4x4 matrix in each legal mode.
///
/// The diagonal must bind and every off-diagonal cell must be rejected —
/// measured in both directions, so this is not "stricter is better": the
/// engine rejects dropping a const and adding one alike.
///
/// `&inout` is absent because `T@ const &inout` is not declarable at all
/// ("Only object types that support object handles can use &inout").
#[test]
fn all_four_const_positions_are_distinguished_in_both_directions() {
    let mut bound = 0usize;
    let mut rejected = 0usize;
    for mode in ["&in", "&out", ""] {
        for (fd_label, fd_pat) in POSITIONS {
            for (fn_label, fn_pat) in POSITIONS {
                let src = funcdef_program(fd_pat, fn_pat, mode);
                let msgs = errors(&src);
                if fd_label == fn_label {
                    assert!(
                        msgs.is_empty(),
                        "the SAME const position must bind ({fd_label}, mode {mode:?}): \
                         {msgs:#?}\n{src}"
                    );
                    bound += 1;
                } else {
                    assert!(
                        !msgs.is_empty(),
                        "`{fn_label}` must not bind a `{fd_label}` funcdef \
                         (mode {mode:?}) — the engine rejects it:\n{src}"
                    );
                    rejected += 1;
                }
            }
        }
    }
    // Over-condemn guard: a matcher that rejects everything scores 36 here and
    // 0 there. Both halves must be non-empty and complete.
    assert_eq!(bound, 12, "the binding half did not run in full");
    assert_eq!(rejected, 36, "the rejecting half did not run in full");
}

/// A VALUE parameter has only one const position, and it is exact too:
/// measured at the `Sort` slot, where `int &in` is rejected against
/// `const int&in` and `const int &in` is rejected against
/// `less_nonconst(int&in, int&in)`.
#[test]
fn a_value_parameters_single_const_position_is_exact() {
    for (fd, f, must_bind) in [
        ("const int &in a", "const int &in a", true),
        ("int &in a", "int &in a", true),
        ("const int &in a", "int &in a", false),
        ("int &in a", "const int &in a", false),
    ] {
        let src = format!(
            "funcdef bool FD({fd});\n\
             bool Fn({f}) {{ return true; }}\n\
             void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
        );
        let msgs = errors(&src);
        assert_eq!(
            msgs.is_empty(),
            must_bind,
            "funcdef `{fd}` vs function `{f}`: expected bind={must_bind}, got \
             {msgs:#?}\n{src}"
        );
    }
}

/// The RETURN type, which reaches the same predicate.
#[test]
fn the_return_types_const_position_is_exact() {
    for (fd_ret, fn_ret, must_bind) in [
        ("C@", "C@", true),
        ("const C@", "const C@", true),
        ("C@ const", "C@ const", true),
        ("C@", "const C@", false),
        ("C@", "C@ const", false),
        ("const C@", "C@ const", false),
    ] {
        let src = format!(
            "class C {{ int v; }}\n\
             funcdef {fd_ret} FD();\n\
             {fn_ret} Fn() {{ return null; }}\n\
             void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
        );
        let msgs = errors(&src);
        assert_eq!(
            msgs.is_empty(),
            must_bind,
            "return `{fn_ret}` at a `{fd_ret}` funcdef: expected bind={must_bind}, \
             got {msgs:#?}\n{src}"
        );
    }
}

// ---------------------------------------------------------------------------
// `array<T>::Sort`, where the expected signature is synthesised per call site.
// ---------------------------------------------------------------------------

fn sort_program(elem_decl: &str, cmp_param: &str, method: &str) -> String {
    format!(
        "class Item {{ int v; }}\n\
         bool Cmp({cmp_param} a, {cmp_param} b) {{ return true; }}\n\
         void Use() {{\n\
         \x20   {elem_decl} xs;\n\
         \x20   xs.{method}(Cmp);\n\
         }}\n"
    )
}

/// The engine prints its own comparator when a bind fails, and it depends on
/// whether the element is a handle:
///
/// ```text
/// bool int[]::less(const int&in, const int&in)
/// bool Item@[]::less(const Item@const&in, const Item@const&in)
/// bool int[]::less_nonconst(int&in, int&in)
/// ```
///
/// Both halves are asserted together so a synthesis that emitted one spelling
/// everywhere cannot pass.
#[test]
fn the_sort_slot_wants_the_engines_own_comparator_spelling() {
    for (elem, cmp, method, must_bind) in [
        // VALUE element: `const T&in`.
        ("int[]", "const int &in", "Sort", true),
        ("int[]", "int &in", "Sort", false),
        ("Item[]", "const Item &in", "Sort", true),
        ("Item[]", "Item &in", "Sort", false),
        // HANDLE element: BOTH consts.
        ("Item@[]", "const Item@ const &in", "Sort", true),
        ("Item@[]", "const Item@ &in", "Sort", false),
        ("Item@[]", "Item@ const &in", "Sort", false),
        ("Item@[]", "Item@ &in", "Sort", false),
        // The NON-const slot is the mirror image, and it is what stops the
        // synthesis from simply adding `const` everywhere.
        ("int[]", "int &in", "SortNonConst", true),
        ("int[]", "const int &in", "SortNonConst", false),
    ] {
        let src = sort_program(elem, cmp, method);
        let msgs = errors_typedb(&src);
        assert_eq!(
            msgs.is_empty(),
            must_bind,
            "{elem}.{method} with `{cmp}`: expected bind={must_bind}, got \
             {msgs:#?}\n{src}"
        );
    }
}

/// The three plugins B144 fixed write the handle spelling, so they must keep
/// binding — this is the row that would catch a synthesis that got the handle
/// case backwards and re-broke Max's plugin.
#[test]
fn the_shipped_const_handle_comparator_still_binds() {
    let src = "bool SortGhosts(const Json::Value@ const &in a, const Json::Value@ const &in b) \
               { return true; }\n\
               void Use() {\n\
               \x20   Json::Value@[] arr;\n\
               \x20   arr.Sort(SortGhosts);\n\
               \x20   arr.Sort(SortGhosts, 0, 1);\n\
               }\n";
    assert!(
        errors_typedb(src).is_empty(),
        "tm-ghosts-plus-plus's comparator must keep binding: {:#?}",
        errors_typedb(src)
    );
}

/// A const handle must not become a wildcard: the ELEMENT type inside it is
/// still compared, and so are arity, return type and mode. Without this the
/// tightening could be satisfied by a predicate that rejects on const and
/// stopped checking anything else.
#[test]
fn tightening_const_did_not_replace_the_other_checks() {
    let decls = "class C { int v; }\nclass D { int w; }\n";
    for (what, funcdef, function) in [
        (
            "wrong element type, both sides const-identical",
            "funcdef bool FD(const C@ const &in a);",
            "bool Fn(const D@ const &in a) { return true; }",
        ),
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
            "funcdef bool FD(const C@ const &in a);",
            "bool Fn(const C@ const &out a) { return true; }",
        ),
    ] {
        let src = format!(
            "{decls}{funcdef}\n{function}\n\
             void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
        );
        assert!(
            !errors(&src).is_empty(),
            "{what} must still be reported:\n{src}"
        );
    }
    // The positive control for the block above: identical signatures bind.
    let ok = format!(
        "{decls}funcdef bool FD(const C@ const &in a);\n\
         bool Fn(const C@ const &in a) {{ return true; }}\n\
         void Use() {{ FD@ h = Fn; if (h is null) {{ }} }}\n"
    );
    assert!(
        errors(&ok).is_empty(),
        "and an exactly-matching pair must bind: {:#?}",
        errors(&ok)
    );
}

// ---------------------------------------------------------------------------
// The database's own words are the oracle for the reconstruction.
// ---------------------------------------------------------------------------

/// The checker rebuilds a Core funcdef parameter's `const` from `typeflags`
/// bit 2, because `typedecl` does not carry it. This checks that
/// reconstruction against the database's OWN `decl` string, for EVERY funcdef
/// it declares rather than the handful anyone thought to name.
///
/// It also fails if a future dump introduces a const HANDLE parameter, where
/// a single flag bit cannot say whether it means `const T@` or `T@ const`.
/// That ambiguity is unreachable today and this is what keeps it from
/// becoming a silent wrong guess later.
#[test]
fn every_core_funcdef_param_const_agrees_with_its_own_decl() {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-versions/1.29.14");
    let index = openplanet_lsp::typedb::TypeIndex::load(
        &dir.join("OpenplanetCore.json"),
        &dir.join("OpenplanetNext.json"),
    )
    .expect("typedb fixture must load");

    let funcdefs = index.funcdefs();
    assert!(
        funcdefs.len() >= 12,
        "the database must declare funcdefs at all, or this test is vacuous: {}",
        funcdefs.len()
    );

    let mut checked = 0usize;
    let mut const_params = 0usize;
    for (name, info) in &funcdefs {
        let Some(decl) = info.decl.as_deref() else {
            continue;
        };
        let args = decl
            .rsplit_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .unwrap_or("");
        let spelled: Vec<&str> = if args.trim().is_empty() {
            Vec::new()
        } else {
            args.split(',').collect()
        };
        assert_eq!(
            spelled.len(),
            info.params.len(),
            "{name}: decl {decl:?} lists {} args but the loader built {}",
            spelled.len(),
            info.params.len()
        );
        for (arg, p) in spelled.iter().zip(&info.params) {
            checked += 1;
            let decl_says_const = arg.trim_start().starts_with("const ");
            assert_eq!(
                p.is_const,
                Some(decl_says_const),
                "{name}: decl arg {arg:?} says const={decl_says_const}, but the \
                 loader read is_const={:?} from typeflags",
                p.is_const
            );
            if decl_says_const {
                const_params += 1;
                assert!(
                    !arg.contains('@'),
                    "{name}: decl arg {arg:?} is a CONST HANDLE. One typeflags bit \
                     cannot say whether that is `const T@` or `T@ const`, and the \
                     loader places it as a leading `const`. Decide deliberately."
                );
            }
        }
    }
    assert!(
        checked > 0,
        "no funcdef parameter was checked — the decl parsing found nothing"
    );
    // Anti-vacuity in the direction that matters: if NO parameter were const,
    // the const assertions above would all be trivially satisfied.
    assert!(
        const_params > 0,
        "no const parameter in the whole database, so the const half of this \
         test proved nothing"
    );
}
