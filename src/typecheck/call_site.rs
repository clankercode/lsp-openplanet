//! Call-site binding and overload selection helpers.
//!
//! Deepens the scattered call-arg paths in the checker into one small module:
//! named/positional binding, unique-arity overload pick. The checker still owns
//! typing (`expr_type`) and diagnostic emission; this module owns **binding
//! truth** shared with signature help via [`crate::typecheck::global_scope::GlobalScope::callables_free`].

use super::global_scope::OverloadSig;

/// Result of binding one call argument to a parameter slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgBind {
    /// Bound to parameter index `n`.
    Index(usize),
    /// Named arg did not match any parameter (caller should still type the value).
    UnknownName,
}

/// Bind one argument to a parameter index.
///
/// - Named args (`name: value`) match `param_names[i] == Some(name)`.
/// - Positional args take the next free index via `next_positional` (advanced
///   only on positional success).
pub fn bind_arg(
    named: Option<&str>,
    param_names: &[Option<String>],
    next_positional: &mut usize,
) -> ArgBind {
    if let Some(name) = named {
        match param_names
            .iter()
            .position(|param_name| param_name.as_deref() == Some(name))
        {
            Some(index) => ArgBind::Index(index),
            None => ArgBind::UnknownName,
        }
    } else {
        let index = *next_positional;
        *next_positional += 1;
        ArgBind::Index(index)
    }
}

/// Bind using workspace `(name, type_text)` param pairs (name always present).
pub fn bind_arg_workspace(
    named: Option<&str>,
    params: &[(String, String)],
    next_positional: &mut usize,
) -> ArgBind {
    if let Some(name) = named {
        match params.iter().position(|(n, _)| n == name) {
            Some(index) => ArgBind::Index(index),
            None => ArgBind::UnknownName,
        }
    } else {
        let index = *next_positional;
        *next_positional += 1;
        ArgBind::Index(index)
    }
}

/// Among overloads, return the unique signature whose arity accepts `argc`.
///
/// Multi-match or zero-match → `None` (callers stay silent on types; arity
/// diagnostics are separate).
pub fn unique_overload_for_argc(overloads: &[OverloadSig], argc: usize) -> Option<&OverloadSig> {
    let mut matching = overloads
        .iter()
        .filter(|sig| argc >= sig.min_args && argc <= sig.param_types.len());
    let first = matching.next()?;
    if matching.next().is_some() {
        None
    } else {
        Some(first)
    }
}

/// Arity ranges `(min, max)` derived from overload signatures.
pub fn arity_ranges(overloads: &[OverloadSig]) -> Vec<(usize, usize)> {
    overloads
        .iter()
        .map(|s| (s.min_args, s.param_types.len()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(names: &[&str], min: usize) -> OverloadSig {
        OverloadSig {
            param_names: names.iter().map(|n| Some((*n).to_string())).collect(),
            param_types: names.iter().map(|_| "int".to_string()).collect(),
            min_args: min,
            return_type: "void".into(),
        }
    }

    #[test]
    fn positional_bind_advances() {
        let names = [Some("a".into()), Some("b".into())];
        let mut next = 0;
        assert_eq!(bind_arg(None, &names, &mut next), ArgBind::Index(0));
        assert_eq!(bind_arg(None, &names, &mut next), ArgBind::Index(1));
        assert_eq!(next, 2);
    }

    #[test]
    fn named_bind_skips_positional() {
        let names = [Some("a".into()), Some("b".into()), Some("c".into())];
        let mut next = 0;
        assert_eq!(bind_arg(Some("b"), &names, &mut next), ArgBind::Index(1));
        assert_eq!(next, 0); // named does not advance positional
        assert_eq!(bind_arg(None, &names, &mut next), ArgBind::Index(0));
    }

    #[test]
    fn unknown_name() {
        let names = [Some("a".into())];
        let mut next = 0;
        assert_eq!(
            bind_arg(Some("nope"), &names, &mut next),
            ArgBind::UnknownName
        );
    }

    #[test]
    fn unique_overload_picks_one() {
        let ovs = vec![sig(&["a"], 1), sig(&["a", "b"], 2)];
        // argc=1 only matches first (min1 max1)
        let u = unique_overload_for_argc(&ovs, 1).unwrap();
        assert_eq!(u.param_types.len(), 1);
        // argc=2 only matches second
        let u = unique_overload_for_argc(&ovs, 2).unwrap();
        assert_eq!(u.param_types.len(), 2);
        // argc=0 matches none
        assert!(unique_overload_for_argc(&ovs, 0).is_none());
    }

    #[test]
    fn unique_overload_ambiguous_is_none() {
        // Two overloads both accept argc=1
        let ovs = vec![sig(&["a"], 0), sig(&["x"], 1)];
        // first: min0 max1, second min1 max1 — both accept 1
        assert!(unique_overload_for_argc(&ovs, 1).is_none());
    }
}
