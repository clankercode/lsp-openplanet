//! B038: the three fix families whose answer is a rename or a `const`
//! adjustment.
//!
//! Grouped under one module — and so one `pub mod` line in
//! [`super::FAMILIES`] — because they share [`scope`], and because the
//! three shipped together as one ticket. Each family is still a separate
//! file exporting its own `pub const FAMILY`, exactly as the seam's author
//! contract describes.
//!
//! What they have in common is the reason none of them is `is_preferred`
//! or `auto_applicable`: every one either changes an API surface
//! (a function name, a method's const-ness) or picks between two intents
//! the tool cannot tell apart (rename this declaration or the other one;
//! make the method const or the receiver mutable). Offering them on
//! right-click is right; applying them on save is not.

pub mod const_receiver;
pub mod duplicate_function;
// B049: the scope model this ticket built was promoted to
// `crate::server::lexical_scope` so `textDocument/rename` and
// `textDocument/references` could stop being scope-blind. Re-exported
// under the old name so the three families read unchanged.
pub use crate::server::lexical_scope as scope;
pub mod shadowed_variable;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::test_support::{context, Workspace};
    use super::{const_receiver, duplicate_function, shadowed_variable};
    use crate::server::code_actions::fix::{collect_fix_descriptors, materialize_auto_descriptors};
    use crate::server::code_actions::fix_all::fix_all_action;
    use crate::server::code_actions::fixes::FAMILIES;

    /// One document carrying all three B038 diagnostics.
    const ALL_THREE: &str = "class C { bool M() { return true; } }\n\
                             void Foo() {}\n\
                             void Foo() {}\n\
                             void f(const C &in h) {\n\
                                 int x = 1;\n\
                                 { int x = 2; x++; }\n\
                                 h.M();\n\
                             }\n";

    #[test]
    fn every_b038_family_offers_something_here() {
        // Guards the negative test below against silently passing because
        // nothing was diagnosed in the first place.
        let workspace = Workspace::single(ALL_THREE);
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let mut families: Vec<&str> = collect_fix_descriptors(&ctx)
            .iter()
            .map(|candidate| candidate.family)
            .collect();
        families.sort_unstable();
        families.dedup();
        assert_eq!(
            families,
            vec![
                const_receiver::FAMILY.name,
                duplicate_function::FAMILY.name,
                shadowed_variable::FAMILY.name,
            ]
        );
    }

    #[test]
    fn no_b038_fix_participates_in_source_fix_all() {
        // B038 decision: each of these either changes an API surface or
        // picks between two intents the tool cannot distinguish. Offering
        // them on right-click is right; applying them on save is not.
        let workspace = Workspace::single(ALL_THREE);
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        let descriptors = collect_fix_descriptors(&ctx);
        let auto = materialize_auto_descriptors(&ctx, &descriptors, FAMILIES);
        assert!(auto.is_empty(), "{:?}", auto);
        assert!(fix_all_action(workspace.uri(0), &auto).is_none());
    }

    #[test]
    fn no_b038_fix_is_marked_preferred() {
        let workspace = Workspace::single(ALL_THREE);
        let pairs = workspace.pairs();
        let ctx = context(&workspace, &pairs, 0);
        for candidate in collect_fix_descriptors(&ctx) {
            assert!(
                !candidate.descriptor.is_preferred,
                "{} marked preferred",
                candidate.descriptor.title
            );
        }
    }
}
