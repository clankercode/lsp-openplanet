//! B115: a class FIELD's visibility keyword survives the parser.
//!
//! # The baseline, measured before a line of the fix was written
//!
//! The ticket asked which of three things the parser did with `private` on a
//! field — reject it, error on it, or silently accept it — because the red
//! test is a different test in each case. A throwaway probe parsed ten
//! spellings and printed the resulting AST. The answer was the third and the
//! most awkward one: **silent acceptance**.
//!
//! ```text
//! --- SOURCE: class C { private int a; }
//!     errors (0): []
//!     FIELD declarators=["a"] attrs=0
//! --- SOURCE: class C { protected int b; }
//!     errors (0): []
//!     FIELD declarators=["b"] attrs=0
//! --- SOURCE: class C { int c; }
//!     errors (0): []
//!     FIELD declarators=["c"] attrs=0
//! --- SOURCE: class C { private int a; private void M() {} }
//!     errors (0): []
//!     FIELD declarators=["a"] attrs=0
//!     METHOD M is_private=true is_protected=false
//! ```
//!
//! Three different sources, three identical AST nodes. `parse_class_member`
//! read the keyword into a local, handed it to `parse_function_rest` for a
//! method, and then let it fall out of scope on the field path — so the three
//! spellings were not merely unfiltered downstream, they were *unrepresentable*
//! downstream. The fourth line is the control that makes this a field bug and
//! not a parser bug: the method in the same class, from the same local, kept
//! its flag.
//!
//! That baseline is why this file asserts a clean parse everywhere. The fix
//! must not turn a silently-accepted keyword into a diagnostic; `private` on a
//! field is valid AngelScript and always was. The only thing that changes is
//! that the AST now says so.
//!
//! # Why this is a parser test and not an access-check test
//!
//! The ticket's consequence is that completion cannot exclude private fields.
//! It is worth being exact about how far from that this fix is: at the time of
//! writing, a grep for `is_private` / `is_protected` across `src/symbols`,
//! `src/typecheck` and `src/server/completion.rs` returns **nothing at all**.
//! No layer above the parser filters on visibility today, for fields *or* for
//! methods. So there is no access-site behaviour to assert yet, and a test
//! that claimed to check one would be asserting on a filter that does not
//! exist.
//!
//! What this file does instead is pin the fact that filter will need, at every
//! site it will need it — including the derived-class site, which is the only
//! place `protected` and `private` differ. See
//! [`protected_and_private_stay_distinct_across_a_class_hierarchy`].
//!
//! # One layer below B114
//!
//! B114 is about a modifier the parser recorded and the symbol table failed to
//! pool. This is the layer beneath: nothing was recorded, so pooling had
//! nothing to lose and landing B114 could not have fixed it. Nothing in this
//! file touches the symbol table.

use openplanet_lsp::parser::ast::{ClassMember, Item, PropertyDecl, VarDeclStmt};
use openplanet_lsp::parser::parser::parse;

/// The visibility pair as the parser recorded it, in the shape the assertions
/// read most clearly: `(is_private, is_protected)`.
type Vis = (bool, bool);

const PRIVATE: Vis = (true, false);
const PROTECTED: Vis = (false, true);
/// No keyword. Pinned as its own named constant rather than written inline as
/// `(false, false)`, because "the default is public" is exactly the assumption
/// the ticket asked to have pinned instead of assumed.
const PUBLIC: Vis = (false, false);

fn parse_clean(src: &str) -> Vec<Item> {
    let (file, errors) = parse(src);
    assert!(
        errors.is_empty(),
        "a visibility keyword on a member must parse cleanly (it did before \
         B115 and must still); source `{src}` produced {errors:?}"
    );
    file.items
}

fn class_members<'a>(items: &'a [Item], class_name: &str, src: &str) -> &'a [ClassMember] {
    for item in items {
        if let Item::Class(c) = item {
            if c.name.text(src) == class_name {
                return &c.members;
            }
        }
    }
    panic!("no class `{class_name}` in `{src}`");
}

fn field<'a>(items: &'a [Item], class_name: &str, field_name: &str, src: &str) -> &'a VarDeclStmt {
    for m in class_members(items, class_name, src) {
        if let ClassMember::Field(v) = m {
            if v.declarators.iter().any(|d| d.name.text(src) == field_name) {
                return v;
            }
        }
    }
    panic!("no field `{field_name}` on class `{class_name}` in `{src}`");
}

fn field_vis(src: &str, class_name: &str, field_name: &str) -> Vis {
    let items = parse_clean(src);
    let f = field(&items, class_name, field_name, src);
    (f.is_private, f.is_protected)
}

fn method_vis(src: &str, class_name: &str, method_name: &str) -> Vis {
    let items = parse_clean(src);
    for m in class_members(&items, class_name, src) {
        if let ClassMember::Method(f) = m {
            if f.name.text(src) == method_name {
                return (f.is_private, f.is_protected);
            }
        }
    }
    panic!("no method `{method_name}` on class `{class_name}` in `{src}`");
}

fn property<'a>(items: &'a [Item], class_name: &str, name: &str, src: &str) -> &'a PropertyDecl {
    for m in class_members(items, class_name, src) {
        if let ClassMember::Property(p) = m {
            if p.name.text(src) == name {
                return p;
            }
        }
    }
    panic!("no property `{name}` on class `{class_name}` in `{src}`");
}

fn property_vis(src: &str, class_name: &str, name: &str) -> Vis {
    let items = parse_clean(src);
    let p = property(&items, class_name, name, src);
    (p.is_private, p.is_protected)
}

// ── The three keywords, on a plain field ─────────────────────────────────────

/// The row the ticket is named for.
#[test]
fn a_private_field_records_private() {
    assert_eq!(field_vis("class C { private int a; }", "C", "a"), PRIVATE);
}

/// The row that makes a conflating fix fail. A fix that sets one "not public"
/// flag for both keywords passes every other test in this file and fails this
/// one, because it would report `protected` as `(true, false)`.
#[test]
fn a_protected_field_records_protected_and_not_private() {
    assert_eq!(
        field_vis("class C { protected int b; }", "C", "b"),
        PROTECTED
    );
}

/// The default, pinned rather than assumed. A bare field is public: BOTH flags
/// are false, not merely `is_private == false`.
#[test]
fn a_field_with_no_keyword_is_public() {
    assert_eq!(field_vis("class C { int c; }", "C", "c"), PUBLIC);
}

/// The three spellings were byte-identical in the AST before B115. Asserting
/// they are now pairwise distinct is the single strongest statement of what
/// changed, and it is immune to the flags being right in isolation but wired
/// to the wrong keyword.
#[test]
fn the_three_spellings_are_pairwise_distinct() {
    let private = field_vis("class C { private int x; }", "C", "x");
    let protected = field_vis("class C { protected int x; }", "C", "x");
    let public = field_vis("class C { int x; }", "C", "x");
    assert_ne!(private, protected, "private must not read as protected");
    assert_ne!(private, public, "private must not read as public");
    assert_ne!(protected, public, "protected must not read as public");
}

/// Neither keyword may set both flags — they are mutually exclusive, and a
/// consumer is entitled to treat `is_private` and `is_protected` as a tagged
/// choice rather than an arbitrary pair.
#[test]
fn no_field_is_both_private_and_protected() {
    for src in [
        "class C { private int x; }",
        "class C { protected int x; }",
        "class C { int x; }",
        "class C { private int x = 1; }",
        "class C { protected array<int>@ x; }",
    ] {
        let (is_private, is_protected) = field_vis(src, "C", "x");
        assert!(
            !(is_private && is_protected),
            "`{src}` set both visibility flags"
        );
    }
}

// ── The method control, in the same class ────────────────────────────────────

/// The ticket asked for a field and a method carrying the SAME keyword in one
/// class, because the method path already worked and is therefore an in-file
/// control: if this fails on the method half, the bug is not the one B115
/// describes.
#[test]
fn a_field_and_a_method_with_the_same_keyword_agree() {
    for (src, expected) in [
        ("class C { private int x; private void M() {} }", PRIVATE),
        (
            "class C { protected int x; protected void M() {} }",
            PROTECTED,
        ),
        ("class C { int x; void M() {} }", PUBLIC),
    ] {
        assert_eq!(method_vis(src, "C", "M"), expected, "method half: `{src}`");
        assert_eq!(field_vis(src, "C", "x"), expected, "field half: `{src}`");
    }
}

/// The keywords are per-member, not per-class: a private field next to a public
/// method must not leak its visibility sideways. Before B115 this could not
/// even be stated, since one side of it did not exist.
#[test]
fn visibility_does_not_leak_between_members() {
    let src = "class C { private int a; int b; protected int c; void M() {} private void N() {} }";
    assert_eq!(field_vis(src, "C", "a"), PRIVATE);
    assert_eq!(field_vis(src, "C", "b"), PUBLIC);
    assert_eq!(field_vis(src, "C", "c"), PROTECTED);
    assert_eq!(method_vis(src, "C", "M"), PUBLIC);
    assert_eq!(method_vis(src, "C", "N"), PRIVATE);
}

// ── The derived-class row ────────────────────────────────────────────────────

/// The row the ticket singles out. `private` and `protected` are
/// indistinguishable everywhere EXCEPT at a derived-class access site: a
/// `protected` base field is reachable from a subclass and a `private` one is
/// not. No access check exists yet (see the module docs), so what this pins is
/// the input that check will consume: three classes in one file — the declaring
/// class, a class derived from it, and an unrelated class — each accessing the
/// members, with every field's visibility recorded independently and correctly.
///
/// The assertion that matters is `base_protected != base_private`: if those two
/// entries were equal, the future access check could not tell the derived-class
/// site from the unrelated one no matter how it was written.
#[test]
fn protected_and_private_stay_distinct_across_a_class_hierarchy() {
    let src = "\
class Base {
    private int hidden;
    protected int shared_with_kids;
    int open;
    void Touch() { hidden = 1; shared_with_kids = 2; open = 3; }
}
class Derived : Base {
    private int own_hidden;
    protected int own_shared;
    int own_open;
    void Touch() { shared_with_kids = 4; open = 5; own_hidden = 6; }
}
class Unrelated {
    void Touch(Base@ b) { b.open = 7; }
}
";
    let items = parse_clean(src);

    let base_private = {
        let f = field(&items, "Base", "hidden", src);
        (f.is_private, f.is_protected)
    };
    let base_protected = {
        let f = field(&items, "Base", "shared_with_kids", src);
        (f.is_private, f.is_protected)
    };
    let base_public = {
        let f = field(&items, "Base", "open", src);
        (f.is_private, f.is_protected)
    };

    assert_eq!(base_private, PRIVATE);
    assert_eq!(base_protected, PROTECTED);
    assert_eq!(base_public, PUBLIC);
    assert_ne!(
        base_private, base_protected,
        "a derived-class access check can only distinguish these two if the \
         parser kept them distinct"
    );

    // Deriving does not rewrite the base's flags, and the subclass's own
    // fields are recorded on their own terms.
    assert_eq!(field_vis(src, "Derived", "own_hidden"), PRIVATE);
    assert_eq!(field_vis(src, "Derived", "own_shared"), PROTECTED);
    assert_eq!(field_vis(src, "Derived", "own_open"), PUBLIC);

    // And an unrelated class in the same file is untouched by either.
    assert_eq!(class_members(&items, "Unrelated", src).len(), 1);
}

// ── The same three keywords, across field spellings ──────────────────────────

/// A field is not only `int x;`. Every spelling `parse_class_member` routes to
/// the field path must carry the keyword: a handle, an array handle, an
/// initialised field, a `const` field, and the constructor-initialiser form,
/// which reaches `parse_var_decl_rest` down a SEPARATE branch of
/// `parse_class_member` (the `looks_like_constructor_init` one) and would be
/// missed by a fix that only patched the obvious call site.
#[test]
fn every_field_spelling_carries_the_keyword() {
    let spellings = [
        "int x;",
        "int x = 1;",
        "int x, y;",
        "const int x = 1;",
        "Foo@ x;",
        "array<int>@ x;",
        "dictionary@ x = dictionary();",
        "Foo x(1, 2);",
    ];
    for tail in spellings {
        assert_eq!(
            field_vis(&format!("class C {{ private {tail} }}"), "C", "x"),
            PRIVATE,
            "private `{tail}`"
        );
        assert_eq!(
            field_vis(&format!("class C {{ protected {tail} }}"), "C", "x"),
            PROTECTED,
            "protected `{tail}`"
        );
        assert_eq!(
            field_vis(&format!("class C {{ {tail} }}"), "C", "x"),
            PUBLIC,
            "bare `{tail}`"
        );
    }
}

/// One keyword governs every declarator it introduces. `private int a, b;` is
/// a single `VarDeclStmt` with two declarators, so this is really a statement
/// about where the flag lives — on the declaration, not on the name — but it
/// is the spelling a plugin actually writes and it deserves a row.
#[test]
fn one_keyword_covers_every_declarator_it_introduces() {
    let src = "class C { private int a, b, c; }";
    let items = parse_clean(src);
    let decl = field(&items, "C", "a", src);
    assert_eq!(
        decl.declarators.len(),
        3,
        "expected one decl of three names"
    );
    assert_eq!((decl.is_private, decl.is_protected), PRIVATE);
    // Reached by any of its names, it is the same node with the same flags.
    for name in ["a", "b", "c"] {
        assert_eq!(field_vis(src, "C", name), PRIVATE, "via `{name}`");
    }
}

// ── The other member-bearing declaration the parser accepts ──────────────────

/// `mixin class` is the other member-bearing declaration `parse_class_decl`
/// serves, and its fields are copied into every class that mixes it in — so a
/// visibility keyword dropped here would be dropped in every consumer of the
/// mixin at once.
#[test]
fn a_mixin_class_field_carries_the_keyword() {
    assert_eq!(
        field_vis("mixin class M { private int x; }", "M", "x"),
        PRIVATE
    );
    assert_eq!(
        field_vis("mixin class M { protected int x; }", "M", "x"),
        PROTECTED
    );
    assert_eq!(field_vis("mixin class M { int x; }", "M", "x"), PUBLIC);
}

/// A property is the other state-shaped class member, and the baseline probe
/// found it dropped its keyword exactly as a plain field did — `private int p
/// { get { ... } }` parsed clean into a `PropertyDecl` with nowhere to put the
/// fact. Fixing the field alone would have left member-completion still unable
/// to filter half of a class's state.
#[test]
fn a_class_property_carries_the_keyword() {
    let tail = "int p { get { return 1; } }";
    assert_eq!(
        property_vis(&format!("class C {{ private {tail} }}"), "C", "p"),
        PRIVATE
    );
    assert_eq!(
        property_vis(&format!("class C {{ protected {tail} }}"), "C", "p"),
        PROTECTED
    );
    assert_eq!(
        property_vis(&format!("class C {{ {tail} }}"), "C", "p"),
        PUBLIC
    );
}

/// A property with a setter as well as a getter travels the same path; pinned
/// so a future edit to the accessor loop cannot quietly drop the flags on the
/// two-accessor branch only.
#[test]
fn a_property_with_both_accessors_carries_the_keyword() {
    let src = "class C { private int p { get { return 1; } set { } } }";
    let items = parse_clean(src);
    let p = property(&items, "C", "p", src);
    assert!(p.getter.is_some() && p.setter.is_some(), "expected get+set");
    assert_eq!((p.is_private, p.is_protected), PRIVATE);
}

// ── Negative controls: the nodes that must stay false ────────────────────────

/// `VarDeclStmt` also models a local and a file-scope variable, neither of
/// which can carry member visibility. They must read as `PUBLIC` — not because
/// a local is public, but because the flags are meaningless there and a
/// consumer must never see a stray `true`.
#[test]
fn a_local_variable_records_no_visibility() {
    let src = "void f() { int local = 1; }";
    let (file, errors) = parse(src);
    assert!(errors.is_empty(), "{errors:?}");
    let Item::Function(func) = &file.items[0] else {
        panic!("expected a function, got {:?}", file.items[0]);
    };
    let body = func.body.as_ref().expect("expected a body");
    let mut seen = 0;
    for stmt in &body.stmts {
        if let openplanet_lsp::parser::ast::StmtKind::VarDecl(v) = &stmt.kind {
            assert_eq!((v.is_private, v.is_protected), PUBLIC, "local `{src}`");
            seen += 1;
        }
    }
    assert_eq!(seen, 1, "expected exactly one local var decl");
}

/// A file-scope `private` is eaten by `parse_item` on purpose and deliberately
/// not propagated (there is a standing comment there saying so). B115 does not
/// change that, and this row exists so that a later change which DOES propagate
/// it has to come here and say so rather than doing it by accident.
#[test]
fn a_file_scope_variable_records_no_visibility() {
    for src in ["int g;", "private int g;", "protected int g;"] {
        let (file, errors) = parse(src);
        assert!(errors.is_empty(), "`{src}` -> {errors:?}");
        let Item::VarDecl(v) = &file.items[0] else {
            panic!("expected a var decl for `{src}`, got {:?}", file.items[0]);
        };
        assert_eq!((v.is_private, v.is_protected), PUBLIC, "file scope `{src}`");
    }
}

/// A top-level property is the same story as a top-level variable.
#[test]
fn a_file_scope_property_records_no_visibility() {
    let src = "int p { get { return 1; } }";
    let (file, errors) = parse(src);
    assert!(errors.is_empty(), "{errors:?}");
    let Item::Property(p) = &file.items[0] else {
        panic!("expected a property, got {:?}", file.items[0]);
    };
    assert_eq!((p.is_private, p.is_protected), PUBLIC);
}
