//! Hardcoded AngelScript / Openplanet built-in names.
//!
//! These names are ubiquitous across plugin code but are not registered in
//! any loaded type database (neither `OpenplanetCore.json` nor
//! `OpenplanetNext.json`). Rather than teach the resolver where they live
//! — which would require per-runtime wiring — we treat them as always
//! resolvable. This is a pragmatic suppression, not a correctness fix:
//! the goal is to stop the checker from false-positiving on names every
//! plugin legitimately uses.
//!
//! Maintain this list lean — only add names once they show up in the
//! undefined-ident corpus histogram AND are verifiably builtin (not just
//! a plugin global that the walker failed to pick up).

/// True if `name` refers to an AngelScript / Openplanet builtin type name
/// that plugins can reference directly (as a constructor-like callable or
/// a bare type reference). Currently covers the `CoroutineFunc` funcdef
/// family that AngelScript's `Meta::startnew` accepts.
pub fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // AngelScript funcdefs exposed by Openplanet for startnew().
        "CoroutineFunc"
            | "CoroutineFuncUserdata"
            | "CoroutineFuncUserdataInt64"
            | "CoroutineFuncUserdataUint64"
            | "CoroutineFuncUserdataString"
    )
}

/// True if `name` is a well-known AngelScript / Openplanet builtin global
/// identifier (free function or global variable). Not currently populated
/// — the undefined-ident bucket doesn't show the usual suspects
/// (`print`/`trace`/etc.) because those live in the Core type DB. This
/// hook exists so later iterations can grow it if the histogram changes.
#[allow(dead_code)]
pub fn is_builtin_global(_name: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coroutine_func_family_is_builtin() {
        assert!(is_builtin_type("CoroutineFunc"));
        assert!(is_builtin_type("CoroutineFuncUserdata"));
        assert!(is_builtin_type("CoroutineFuncUserdataInt64"));
        assert!(is_builtin_type("CoroutineFuncUserdataUint64"));
        assert!(is_builtin_type("CoroutineFuncUserdataString"));
    }

    #[test]
    fn unknown_name_is_not_builtin() {
        assert!(!is_builtin_type("Foo"));
        assert!(!is_builtin_type(""));
        assert!(!is_builtin_type("CoroutineFuncSomethingElse"));
    }
}
