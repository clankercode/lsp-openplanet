//! B094: interface-implementation checking.
//!
//! When `class C : I` does not provide a method the interface declares, the
//! game refuses to compile the plugin. Measured against the live engine
//! (OP 1.29.5, 2026-08-31, via `tm-control-mcp`, each fixture its own
//! plugin so each was its own compilation unit):
//!
//! ```text
//! class C : I {}                          Missing implementation of 'void I::M()'
//! class C : I { int M() {...} }           Missing implementation of 'void I::M()'
//! class C : I { void M() {} }   (wants M(int))
//!                                         Missing implementation of 'void I::M(int)'
//! class C : I { void M(float) {} }        Missing implementation of 'void I::M(int)'
//! class C : I { void M(string) {} }       Missing implementation of 'void I::M(const string&in)'
//! ```
//!
//! **The engine has exactly ONE message form.** Max's ticket asked whether a
//! same-name-different-signature method is a separate error "unless
//! openplanet compile error is just unimplemented" — it is just
//! unimplemented. A mismatched signature does not satisfy the interface and
//! is reported as the method being MISSING, with the message naming the
//! signature the engine wanted. So this module emits one diagnostic kind,
//! not two, and the expected signature is the payload.
//!
//! Other measured behaviour, all of it load-bearing here:
//!
//! * **One error per missing method** — three missing methods gave three
//!   errors, not one summary.
//! * **`abstract` does NOT exempt.** `abstract class C : I {}` still fails.
//!   This is the opposite of most languages and would have been wrong if
//!   assumed.
//! * **`private` satisfies.** A private method implements the interface.
//! * **Inherited satisfies**, through base classes and transitively.
//! * **An interface extending an interface** contributes its base's methods.
//! * **Extra overloads are harmless** beside a correct implementation.
//!
//! # Fail-silent, deliberately
//!
//! This rule compares AngelScript signature TEXT, which B062 recorded as
//! precisely where a normalisation slip becomes a false report. The polarity
//! is worse here than it was there: B062 risked a missed warning, this risks
//! a false ERROR on a class that compiles, on a construct every plugin using
//! interfaces depends on.
//!
//! So every uncertainty resolves to SILENCE. The bail conditions are
//! enumerated in [`missing_interface_methods`] and each has its own test.

use std::collections::{HashSet, VecDeque};

use crate::symbols::scope::SymbolKind;
use crate::typecheck::GlobalScope;

/// One interface method a class fails to implement.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingImpl {
    /// The engine-shaped signature: `void I::M(const string&in)`. Mirrors
    /// the game's own message so a user can match ours against a compile
    /// log, and gives B095's code action the exact stub to generate.
    pub signature: String,
    /// Interface that declares the method (`I`, qualified as pooled).
    pub interface: String,
    /// Simple method name (`M`).
    pub method: String,
}

/// Interface methods `class_qname` declares it implements but does not
/// provide.
///
/// Empty — meaning SILENT, not "nothing missing" — whenever the answer
/// cannot be established with confidence:
///
/// 1. `class_qname` is not a workspace class.
/// 2. Any name in its base list, or in any ancestor's base list, does not
///    resolve in the workspace. An unresolved base may be an engine type or
///    a declaration we cannot see, and it may supply the very method that
///    would otherwise look missing.
/// 3. The class declares no interface base at all (nothing to check).
///
/// and per-method:
///
/// 4. A method of the right name exists but its signature cannot be
///    compared confidently — see [`signatures_match`], which returns `None`
///    for "unsure" rather than guessing.
pub fn missing_interface_methods(class_qname: &str, scope: &GlobalScope) -> Vec<MissingImpl> {
    let Some(bases) = class_bases(class_qname, scope) else {
        return Vec::new();
    };

    // Bail 2, applied to the whole transitive ancestry before anything else:
    // one unresolvable link anywhere and the provided-method set is unknown.
    let Some(ancestry) = resolved_ancestry(class_qname, scope) else {
        return Vec::new();
    };

    // Base names are pooled as WRITTEN (`class C : I` inside `namespace N`
    // stores `I`), so they must be resolved to their pooled spelling
    // (`N::I`) before anything can be looked up under them. `bases` is
    // known resolvable here because `resolved_ancestry` above succeeded.
    let resolved_bases: Vec<String> = bases
        .iter()
        .filter_map(|b| resolve_base(b, class_qname, scope))
        .collect();
    let interfaces = required_interfaces(&resolved_bases, scope);
    if interfaces.is_empty() {
        return Vec::new();
    }

    // Every method reachable on the class, own and inherited. Interface
    // members are excluded: an interface method is a contract, not an
    // implementation, so an interface base cannot satisfy another
    // interface's requirement.
    let provided: Vec<MethodSig> = ancestry
        .iter()
        .filter(|owner| !scope.is_workspace_interface(owner))
        .flat_map(|owner| methods_of(owner, scope))
        .collect();

    let mut out = Vec::new();
    for iface in &interfaces {
        for required in methods_of(iface, scope) {
            let candidates: Vec<&MethodSig> = provided
                .iter()
                .filter(|m| m.name == required.name)
                .collect();
            // Unsure about ANY candidate means unsure overall: silence.
            let mut satisfied = false;
            for cand in &candidates {
                match signatures_match(&required, cand, scope) {
                    Some(true) => {
                        satisfied = true;
                        break;
                    }
                    Some(false) => {}
                    None => {
                        satisfied = true; // bail 4 — treat as implemented
                        break;
                    }
                }
            }
            if !satisfied {
                out.push(MissingImpl {
                    signature: render_signature(iface, &required),
                    interface: iface.clone(),
                    method: required.name.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.signature.cmp(&b.signature));
    out.dedup();
    out
}

/// A pooled method, in the form this module compares.
#[derive(Debug, Clone)]
pub struct MethodSig {
    pub name: String,
    pub return_type: String,
    pub param_types: Vec<String>,
    pub min_args: usize,
}

/// Base list of a workspace CLASS. `None` when `class_qname` names no
/// workspace class (bail 1) — an interface or enum of the same name is not
/// a class and must not be checked as one.
fn class_bases(class_qname: &str, scope: &GlobalScope) -> Option<Vec<String>> {
    scope
        .lookup_workspace(class_qname)
        .iter()
        .find_map(|s| match &s.kind {
            SymbolKind::Class { parents } => Some(parents.clone()),
            _ => None,
        })
}

/// Resolve a base-list name the way the class body sees it, preferring an
/// exact pooled hit and then the enclosing-namespace spelling.
fn resolve_base(base: &str, from: &str, scope: &GlobalScope) -> Option<String> {
    if !scope.lookup_workspace(base).is_empty() {
        return Some(base.to_string());
    }
    // `namespace N { class C : Base }` pools the base as `N::Base`.
    let mut prefix = from;
    while let Some((head, _)) = prefix.rsplit_once("::") {
        let candidate = format!("{}::{}", head, base);
        if !scope.lookup_workspace(&candidate).is_empty() {
            return Some(candidate);
        }
        prefix = head;
    }
    None
}

/// The class itself plus every transitively reachable ancestor, or `None`
/// if any link fails to resolve (bail 2).
fn resolved_ancestry(class_qname: &str, scope: &GlobalScope) -> Option<Vec<String>> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    let mut queue = VecDeque::from([class_qname.to_string()]);
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > 128 {
            return None; // pathological or cyclic — do not guess
        }
        order.push(name.clone());
        let parents = scope
            .lookup_workspace(&name)
            .iter()
            .find_map(|s| match &s.kind {
                SymbolKind::Class { parents } | SymbolKind::Interface { parents } => {
                    Some(parents.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        for p in parents {
            let resolved = resolve_base(&p, &name, scope)?;
            queue.push_back(resolved);
        }
    }
    Some(order)
}

/// Interfaces the class must satisfy: its direct interface bases plus every
/// interface those extend, transitively.
fn required_interfaces(bases: &[String], scope: &GlobalScope) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut queue: VecDeque<String> = bases.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) || seen.len() > 128 {
            continue;
        }
        if !scope.is_workspace_interface(&name) {
            continue;
        }
        for p in scope.workspace_class_parents(&name) {
            if let Some(resolved) = resolve_base(&p, &name, scope) {
                queue.push_back(resolved);
            }
        }
        out.push(name);
    }
    out
}

/// Methods pooled directly under `owner` (`Owner::name`), excluding
/// constructors and destructors, which no interface can require.
fn methods_of(owner: &str, scope: &GlobalScope) -> Vec<MethodSig> {
    let prefix = format!("{}::", owner);
    let simple_owner = owner.rsplit("::").next().unwrap_or(owner);
    let mut out = Vec::new();
    for sym in scope.lookup_workspace_members(owner) {
        let SymbolKind::Function {
            return_type,
            params,
            min_args,
            imported: false,
        } = &sym.kind
        else {
            continue;
        };
        let Some(name) = sym.name.strip_prefix(&prefix) else {
            continue;
        };
        if name.contains("::") || name == simple_owner || name.starts_with('~') {
            continue;
        }
        out.push(MethodSig {
            name: name.to_string(),
            return_type: return_type.clone(),
            param_types: params.iter().map(|(_, t)| t.clone()).collect(),
            min_args: *min_args,
        });
    }
    out
}

/// Does `candidate` implement `required`?
///
/// `None` means UNSURE, and every caller treats unsure as implemented. The
/// whole point of this function is that it is allowed to say so.
///
/// Certain only when both sides are built from type spellings this module
/// can normalise to a canonical form — see [`canonical_type`]. A workspace
/// `typedef`, an alias, or a name that resolves nowhere makes the
/// comparison unsafe, because two different spellings can denote the same
/// type and reporting a difference would be a false "not implemented" on a
/// class that compiles.
fn signatures_match(
    required: &MethodSig,
    candidate: &MethodSig,
    scope: &GlobalScope,
) -> Option<bool> {
    if required.param_types.len() != candidate.param_types.len() {
        return Some(false);
    }
    let mut all_equal = types_equal(&required.return_type, &candidate.return_type, scope)?;
    for (r, c) in required
        .param_types
        .iter()
        .zip(candidate.param_types.iter())
    {
        all_equal &= types_equal(r, c, scope)?;
    }
    Some(all_equal)
}

/// `None` when either spelling cannot be compared with confidence.
fn types_equal(a: &str, b: &str, scope: &GlobalScope) -> Option<bool> {
    let (ca, cb) = (canonical_type(a), canonical_type(b));
    if ca == cb {
        return Some(true);
    }
    // Different text. Only call that a real difference when BOTH spellings
    // name something this workspace can vouch for; otherwise stay unsure.
    if type_is_comparable(&ca, scope) && type_is_comparable(&cb, scope) {
        Some(false)
    } else {
        None
    }
}

/// Collapse a source type spelling to a comparable form: whitespace
/// removed, so `const string &in` and `const string&in` are one thing.
/// Deliberately NOT a semantic normalisation — the semantic question is
/// handed to [`type_is_comparable`], which refuses rather than guesses.
fn canonical_type(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// True when a canonical spelling denotes a type this module is willing to
/// call different from another. Primitives are safe (no aliasing), and so
/// is a name pooled as a workspace class, interface or enum. Anything else
/// — a typedef, a funcdef alias, an engine type, an unresolved name — is
/// not, because a differing spelling may still be the same type.
fn type_is_comparable(canonical: &str, scope: &GlobalScope) -> bool {
    let bare = canonical
        .trim_start_matches("const")
        .trim_end_matches("&in")
        .trim_end_matches("&out")
        .trim_end_matches("&inout")
        .trim_end_matches('&')
        .trim_end_matches("const")
        .trim_end_matches('@')
        .trim();
    if bare.is_empty() {
        return false;
    }
    const PRIMITIVES: &[&str] = &[
        "void", "bool", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16",
        "uint32", "uint64", "float", "double", "string",
    ];
    if PRIMITIVES.contains(&bare) {
        return true;
    }
    scope.lookup_workspace(bare).iter().any(|s| {
        matches!(
            s.kind,
            SymbolKind::Class { .. } | SymbolKind::Interface { .. } | SymbolKind::Enum { .. }
        )
    })
}

/// Render the engine's message payload: `void I::M(const string&in)`.
fn render_signature(interface: &str, m: &MethodSig) -> String {
    let params: Vec<String> = m.param_types.iter().map(|t| canonical_type(t)).collect();
    format!(
        "{} {}::{}({})",
        m.return_type.trim(),
        interface,
        m.name,
        params.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;
    use crate::typecheck::{Checker, TypeDiagnosticKind};

    /// Every `Missing implementation of '...'` message a source produces,
    /// through the real checker — not the module function directly, so the
    /// wiring is covered too.
    fn missing(source: &str) -> Vec<String> {
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, None);
        let mut checker = Checker::new(source, &scope);
        checker.check_file(&file);
        let mut out: Vec<String> = checker
            .diagnostics
            .iter()
            .filter_map(|d| match &d.kind {
                TypeDiagnosticKind::MissingInterfaceImplementation { signature } => {
                    Some(signature.clone())
                }
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    // ── the engine's positive cases: each was measured, OP 1.29.5 ────────

    #[test]
    fn a_method_missing_entirely_is_reported() {
        assert_eq!(
            missing("interface I { void M(); }\nclass C : I {}\n"),
            vec!["void I::M()"]
        );
    }

    #[test]
    fn a_different_return_type_is_reported_as_missing() {
        assert_eq!(
            missing("interface I { void M(); }\nclass C : I { int M() { return 1; } }\n"),
            vec!["void I::M()"],
            "the engine reports a mismatched signature AS missing, not as a distinct error"
        );
    }

    #[test]
    fn a_different_parameter_count_is_reported_as_missing() {
        assert_eq!(
            missing("interface I { void M(int a); }\nclass C : I { void M() {} }\n"),
            vec!["void I::M(int)"]
        );
    }

    #[test]
    fn a_different_parameter_type_is_reported_as_missing() {
        assert_eq!(
            missing("interface I { void M(int a); }\nclass C : I { void M(float a) {} }\n"),
            vec!["void I::M(int)"]
        );
    }

    #[test]
    fn a_by_value_parameter_against_const_ref_in_is_reported_as_missing() {
        assert_eq!(
            missing(
                "interface I { void M(const string &in s); }\nclass C : I { void M(string s) {} }\n"
            ),
            vec!["void I::M(conststring&in)"]
        );
    }

    /// Per repo convention, sweep the passing modes rather than pinning one.
    #[test]
    fn each_reference_passing_mode_is_distinguished() {
        for (iface, klass, want) in [
            (
                "void M(int &out a);",
                "void M(int a) {}",
                "void I::M(int&out)",
            ),
            (
                "void M(int &in a);",
                "void M(int a) {}",
                "void I::M(int&in)",
            ),
            (
                "void M(int &inout a);",
                "void M(int a) {}",
                "void I::M(int&inout)",
            ),
            ("void M(int a);", "void M(int &out a) {}", "void I::M(int)"),
        ] {
            let src = format!("interface I {{ {iface} }}\nclass C : I {{ {klass} }}\n");
            assert_eq!(missing(&src), vec![want], "for `{iface}` vs `{klass}`");
        }
    }

    /// A handle parameter is a different type from a value parameter.
    #[test]
    fn a_handle_parameter_is_distinguished_from_a_value_one() {
        assert_eq!(
            missing("class T {}\ninterface I { void M(T@ t); }\nclass C : I { void M(T t) {} }\n"),
            vec!["void I::M(T@)"]
        );
    }

    /// ONE DIAGNOSTIC PER MISSING METHOD — measured: 3 missing gave 3 errors.
    #[test]
    fn every_missing_method_is_reported_separately() {
        assert_eq!(
            missing("interface I { void M(); void N(); int P(float f); }\nclass C : I {}\n"),
            vec!["int I::P(float)", "void I::M()", "void I::N()"]
        );
    }

    /// `abstract` does NOT exempt. Measured, and the opposite of the usual
    /// language rule — worth its own test because the intuition is wrong.
    #[test]
    fn an_abstract_class_is_not_exempt() {
        assert_eq!(
            missing("interface I { void M(); }\nabstract class C : I {}\n"),
            vec!["void I::M()"]
        );
    }

    /// An interface extending an interface contributes its base's methods.
    #[test]
    fn an_interface_extending_another_requires_the_bases_methods() {
        assert_eq!(
            missing(
                "interface I { void M(); }\ninterface J : I { void N(); }\nclass C : J { void N() {} }\n"
            ),
            vec!["void I::M()"]
        );
    }

    /// Multiple interfaces: only the unsatisfied one is named.
    #[test]
    fn only_the_unsatisfied_interface_of_several_is_named() {
        assert_eq!(
            missing(
                "interface I { void M(); }\ninterface J { void N(); }\nclass C : I, J { void M() {} }\n"
            ),
            vec!["void J::N()"]
        );
    }

    /// `shared interface` behaves identically — measured.
    #[test]
    fn a_shared_interface_is_checked_the_same_way() {
        assert_eq!(
            missing("shared interface I { void M(); }\nshared class C : I {}\n"),
            vec!["void I::M()"]
        );
    }

    /// Inside a namespace, where the base spells short and pools long.
    #[test]
    fn a_namespaced_class_and_interface_are_checked() {
        assert_eq!(
            missing("namespace N { interface I { void M(); } class C : I {} }\n"),
            vec!["void N::I::M()"]
        );
    }

    // ── REQUIRED SILENCES, each measured CLEAN against the engine ────────

    #[test]
    fn a_correctly_implemented_class_is_silent() {
        assert!(missing("interface I { void M(); }\nclass C : I { void M() {} }\n").is_empty());
    }

    /// `private` satisfies the interface — measured clean.
    #[test]
    fn a_private_implementation_satisfies_the_interface() {
        assert!(
            missing("interface I { void M(); }\nclass C : I { private void M() {} }\n").is_empty()
        );
    }

    /// An inherited method satisfies, both orders of the base list.
    #[test]
    fn an_inherited_method_satisfies_the_interface() {
        assert!(
            missing("interface I { void M(); }\nclass B { void M() {} }\nclass C : B, I {}\n")
                .is_empty()
        );
        assert!(
            missing("interface I { void M(); }\nclass B { void M() {} }\nclass C : I, B {}\n")
                .is_empty()
        );
    }

    /// …including through a grandparent, and when the BASE is what declares
    /// the interface.
    #[test]
    fn an_implementation_inherited_transitively_satisfies() {
        assert!(missing(
            "interface I { void M(); }\nclass A { void M() {} }\nclass B : A {}\nclass C : B, I {}\n"
        )
        .is_empty());
        assert!(missing(
            "interface I { void M(); }\nclass B : I { void M() {} }\nclass C : B {}\n"
        )
        .is_empty());
    }

    /// An extra overload beside a correct implementation is harmless.
    #[test]
    fn an_extra_overload_beside_a_correct_one_is_silent() {
        assert!(missing(
            "interface I { void M(int a); }\nclass C : I { void M(int a) {} void M(float a) {} }\n"
        )
        .is_empty());
    }

    /// A class that does not declare the interface is never checked.
    #[test]
    fn a_class_not_declaring_the_interface_is_never_checked() {
        assert!(missing("interface I { void M(); }\nclass C {}\n").is_empty());
    }

    /// A class with no bases at all, and an interface with no methods.
    #[test]
    fn trivial_shapes_are_silent() {
        assert!(missing("class C {}\n").is_empty());
        assert!(missing("interface I {}\nclass C : I {}\n").is_empty());
    }

    /// A class with a constructor and destructor implementing an interface
    /// correctly is silent, and a constructor does NOT satisfy a
    /// same-named requirement.
    ///
    /// HONEST LABEL: the second half is real behaviour worth pinning, but
    /// the ctor/dtor filter in `methods_of` is NOT what produces it — the
    /// return types differ (`void` vs the class), and that is what decides
    /// it. Mutation-tested: deleting the filter entirely leaves this test,
    /// and the whole suite, green.
    ///
    /// **The filter has no discriminating witness among reachable inputs.**
    /// On the PROVIDED side a constructor's synthesized return type is the
    /// class, which no ordinary requirement asks for; on the REQUIRED side
    /// an interface cannot declare a constructor. So there is no input that
    /// tells "filter present" from "filter absent", and an assertion over
    /// these inputs is testing the inputs, not the rule. Recorded rather
    /// than dressed up as coverage — see the "corpus cannot discriminate"
    /// entry in docs/verification-hazards.md.
    ///
    /// One consequence is worth knowing: because the filter also applies to
    /// the interface side, an interface method whose name equals its own
    /// interface's (`interface I { void I(); }`) would be dropped from the
    /// requirement set. Whether the engine even accepts that declaration is
    /// unmeasured — it was not worth re-taking the game port for — and the
    /// failure direction is a missed error, which is this module's chosen
    /// direction throughout.
    #[test]
    fn a_class_with_a_constructor_implementing_an_interface_is_silent() {
        assert!(
            missing("interface I { void M(); }\nclass C : I { C() {} ~C() {} void M() {} }\n")
                .is_empty()
        );
        assert_eq!(
            missing("interface I { void C(); }\nclass C : I { C() {} }\n"),
            vec!["void I::C()"],
            "a constructor must not satisfy a requirement that shares its name"
        );
    }

    // ── THE FAIL-SILENT BAILS. Each is a deliberate missed error. ────────

    /// BAIL 2: an unresolvable base. The base may be an engine type that
    /// supplies the method, so nothing can be concluded.
    #[test]
    fn an_unresolvable_base_silences_the_whole_check() {
        assert!(
            missing("interface I { void M(); }\nclass C : I, CSomeEngineThing {}\n").is_empty(),
            "an unknown base may supply the method — reporting here would be a \
             false error on code that compiles"
        );
    }

    /// BAIL 4: a same-named method whose types cannot be compared with
    /// confidence. A `typedef` means two spellings can denote one type, so
    /// a textual difference is not evidence of a mismatch.
    #[test]
    fn a_typedef_shaped_parameter_difference_stays_silent() {
        assert!(
            missing(
                "typedef int MyInt;\ninterface I { void M(MyInt a); }\nclass C : I { void M(int a) {} }\n"
            )
            .is_empty(),
            "`MyInt` and `int` may be the same type; a spelling difference is not a mismatch"
        );
    }

    /// …and the same in the other direction.
    #[test]
    fn an_unknown_parameter_type_stays_silent() {
        assert!(missing(
            "interface I { void M(CSomeEngineType@ t); }\nclass C : I { void M(int a) {} }\n"
        )
        .is_empty());
    }

    /// The bail is per-METHOD, not per-class: an uncomparable signature on
    /// one method must not silence a plainly missing sibling.
    #[test]
    fn a_bail_on_one_method_does_not_silence_another() {
        assert_eq!(
            missing(
                "typedef int MyInt;\ninterface I { void M(MyInt a); void N(); }\n\
                 class C : I { void M(int a) {} }\n"
            ),
            vec!["void I::N()"],
            "`N` is unambiguously absent even though `M` cannot be compared"
        );
    }
}
