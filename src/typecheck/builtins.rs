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

use crate::typecheck::global_scope::GlobalScope;

/// True if `name` refers to an AngelScript / Openplanet builtin type name
/// that plugins can reference directly (as a constructor-like callable or
/// a bare type reference). Currently covers the `CoroutineFunc` funcdef
/// family that AngelScript's `Meta::startnew` accepts, plus a few Core
/// types the JSON dump omits (still accepted by the game compiler).
pub fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // AngelScript funcdefs exposed by Openplanet for startnew().
        "CoroutineFunc"
            | "CoroutineFuncUserdata"
            | "CoroutineFuncUserdataInt64"
            | "CoroutineFuncUserdataUint64"
            | "CoroutineFuncUserdataString"
            // NanoVG font handle. Typedb exposes `nvg::LoadFont` → `int` and
            // `nvg::FontFace(int)` but does not declare a `nvg::Font` class;
            // the game still accepts `nvg::Font` as a typedef-like type
            // (tm-dashboard dogfood, Openplanet 1.29.5).
            | "nvg::Font"
    )
}

/// A method the Openplanet runtime registers on an engine class that the
/// Nadeo type dump never lists (B059, GH #21).
///
/// The previous `vscode-openplanet-angelscript` extension patched these into
/// the dump at convert time (`language-server/src/convert_nadeo.ts`);
/// openplanet-lsp loads the dump as-is, so the checker consults this table
/// instead. The receiver types have non-empty member lists, so B006 trusts
/// them and fires `UndefinedMember` — correct for a real typo, wrong for
/// these.
pub struct InjectedMethod {
    /// Method name as written at the call site.
    pub name: &'static str,
    /// Which receivers carry it — see [`InjectedReceiver`].
    pub receivers: InjectedReceiver,
    /// Declared return type in typedb type-string form, verbatim from the
    /// registration declaration in the DLL. Load-bearing: it is what
    /// `auto x = obj.M()` infers, so a wrong value here is a worse bug than
    /// the false positive being fixed.
    pub return_type: &'static str,
    /// Accepted argument count, `(min, max)`. Nothing else knows these
    /// methods exist, so this table is the only arity authority for them.
    pub arity: (usize, usize),
}

/// How an [`InjectedMethod`] picks its receivers. The two arms mirror the
/// two registration mechanisms found in the DLL — see
/// `docs/probes/openplanet-injected-methods.md`.
pub enum InjectedReceiver {
    /// Mechanism A: emitted by the generic class-registration loop, so it
    /// lands on EVERY reflected engine class. Matched as "derives from
    /// `CMwNod`", which is what "every engine class" means in the dump.
    EveryEngineClass,
    /// Mechanism B: Openplanet hard-codes the target type at each
    /// registration site, so the LSP matches the same explicit list.
    ///
    /// Deliberately NOT "base type and descendants". On the 1.29.5 dump the
    /// two rules are indistinguishable — each registered set happens to be
    /// exactly the full descendant subtree — but that is a coincidence of
    /// the current hierarchy. A future Nadeo type deriving from
    /// `CGamePlayer` would be accepted by a descendants rule and rejected
    /// by the game until Openplanet itself registered it, re-creating the
    /// false-positive class this table exists to remove.
    Exact(&'static [&'static str]),
}

/// Every Openplanet-injected method the checker knows about.
///
/// Recovered from `Openplanet.dll` 1.29.5 (next, Public, 1234ad9e), md5
/// `c179218b3b24582130ef42b92a3585f6`, by pairing each `RegisterObjectMethod`
/// declaration string with the type name loaded at the same call site. The
/// surface is bounded, not best-effort: `docs/probes/openplanet-injected-methods.md`
/// accounts for all 20 code references to a dump-known type name, and an
/// injection must name its target as a C string at its registration site.
/// Re-derive with `scripts/re/injected_methods.py` after an Openplanet update.
///
/// Kept as data rather than `matches!` arms because the decision is made at
/// TWO checker stages — bare member access and method call — so a hard-coded
/// name pair costs two edits per method, and had already been duplicated
/// once (GH #21).
///
/// Mechanism A also injects `void opCast(?&out)` and `X@ opImplCast()` on
/// every class; those are deliberately out of scope here (they back
/// `cast<T>(x)` rather than being written as calls).
pub static INJECTED_METHODS: &[InjectedMethod] = &[
    // GH #21: manual refcounting. Their absence from the dump flooded ~107
    // UndefinedMember false positives on tm-editor-plus-plus.
    InjectedMethod {
        name: "MwAddRef",
        receivers: InjectedReceiver::EveryEngineClass,
        return_type: "void",
        arity: (0, 0),
    },
    InjectedMethod {
        name: "MwRelease",
        receivers: InjectedReceiver::EveryEngineClass,
        return_type: "void",
        arity: (0, 0),
    },
    // B059. Registered at 0x180137e9d / 0x180137f0a. `CSmPlayer` is where
    // every reported call site sits (tm-dips-plus-plus, tm-change-car-color,
    // tm-magic-spectator) — and note it is registered in its own right, not
    // inherited from `CGamePlayer`.
    InjectedMethod {
        name: "GetCurrentEntityID",
        receivers: InjectedReceiver::Exact(&["CGamePlayer", "CSmPlayer"]),
        return_type: "uint",
        arity: (0, 0),
    },
    // B059 sibling. Registered at 0x180137cd3/…d43/…db3/…e23 — FOUR types.
    // The old `convert_nadeo.ts` (which the ticket quoted) listed only
    // `CGameCtnNetwork`; the shipped DLL includes `CGameNetwork`, its
    // PARENT, so a "CGameCtnNetwork and descendants" rule would leave a
    // false positive on a `CGameNetwork@` receiver. tm-shadowless-chrono
    // is the live caller.
    InjectedMethod {
        name: "GetManialinkPages",
        receivers: InjectedReceiver::Exact(&[
            "CGameNetwork",
            "CGameCtnNetwork",
            "CGameManiaPlanetNetwork",
            "CTrackManiaNetwork",
        ]),
        // Handle to an array, NOT `CGameManialinkPage@[]&` — `T[]` is sugar
        // for `array<T>`, but `@` and `&` are not interchangeable, and the
        // ticket's `&` spelling came from the stale convert script.
        return_type: "array<CGameManialinkPage@>@",
        arity: (0, 0),
    },
];

/// Injected rows whose method name is `name`. The caller applies the
/// receiver test; see [`injected_methods_for_receiver`] for the shared one.
pub fn injected_methods_named(name: &str) -> impl Iterator<Item = &'static InjectedMethod> + '_ {
    INJECTED_METHODS.iter().filter(move |m| m.name == name)
}

/// B063: every injected method that applies to the receiver `type_name`.
///
/// **This rule currently exists in TWO places, deliberately and temporarily.**
/// Here, and again as `Checker::injected_method` in `src/typecheck/checker.rs`.
/// That is not an oversight: removing the second copy means editing
/// `checker.rs`, which another lane owns, and racing it to deduplicate a rule
/// we have already filed a ticket for (B079) would trade a documented copy for
/// a merge conflict in the most-reviewed file in the batch.
///
/// The duplication ends with B079. Until then the tripwire is
/// `every_offered_injected_method_is_one_the_checker_accepts`, which drives
/// both features end-to-end and fails if the two copies disagree.
///
/// It is the answer to "does this method exist on this type", and B063 exists
/// because two features answered it differently: the checker consulted
/// `INJECTED_METHODS`, completion walked only the `TypeIndex` — where an
/// injected method is by definition absent, that being the whole premise of
/// B059 — so the checker accepted `player.GetCurrentEntityID()` while
/// completion would not suggest it.
///
/// Taking a [`GlobalScope`] rather than a `TypeIndex` is deliberate: the
/// derivation walk it needs is already public there, and re-deriving a parent
/// chain locally would be a THIRD implementation of an existing predicate —
/// exactly the hazard B079 describes.
///
/// The `is_external_type` gate is load-bearing and was missed on the first
/// attempt. A workspace type SHADOWS an external one of the same name, so a
/// plugin declaring `class CSmPlayer {}` would otherwise be offered the engine
/// class's injected methods — `is_external_derived_from` resolves the name
/// against the index whether or not the workspace shadows it, while
/// `is_external_type` checks `workspace_type_shadows` first. Picking the
/// predicate that ignored shadowing is itself an instance of B079.
///
/// `Exact` matches by identity rather than by descent, mirroring the binary:
/// Openplanet hard-codes a registration list, so a future Nadeo type deriving
/// from `CGamePlayer` would be offered the method by a descendants rule and
/// rejected by the game (B059).
pub fn injected_methods_for_receiver<'a>(
    scope: &'a GlobalScope<'a>,
    type_name: &'a str,
) -> impl Iterator<Item = &'static InjectedMethod> + 'a {
    let external_receiver = scope.is_external_type(type_name);
    INJECTED_METHODS.iter().filter(move |m| {
        external_receiver
            && match &m.receivers {
                InjectedReceiver::EveryEngineClass => {
                    scope.is_external_derived_from(type_name, "CMwNod")
                }
                InjectedReceiver::Exact(names) => names.iter().any(|n| {
                    // Same external type, tolerating short vs qualified spellings:
                    // the typedb parent chain is a tree, so mutual derivation can
                    // only mean the two names resolve to one type.
                    scope.is_external_derived_from(type_name, n)
                        && scope.is_external_derived_from(n, type_name)
                }),
            }
    })
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
    fn nvg_font_is_builtin() {
        assert!(is_builtin_type("nvg::Font"));
        // Bare Font is UI::Font in typedb — not this alias.
        assert!(!is_builtin_type("Font"));
        assert!(!is_builtin_type("nvg::Paint"));
    }

    #[test]
    fn injected_methods_lookup_is_name_exact() {
        assert_eq!(injected_methods_named("MwAddRef").count(), 1);
        assert_eq!(injected_methods_named("GetCurrentEntityID").count(), 1);
        assert_eq!(injected_methods_named("GetManialinkPages").count(), 1);
        // A near miss is not a hit — B006 must stay loud on real typos.
        assert_eq!(injected_methods_named("GetCurrentEntityId").count(), 0);
        assert_eq!(injected_methods_named("").count(), 0);
    }

    /// The return type is what `auto x = obj.M()` infers, so a table row
    /// silently regressing to `void` would re-introduce the bug B059
    /// explicitly rules out (a silence sentinel that mis-types the local).
    #[test]
    fn injected_method_rows_carry_their_return_type() {
        let by_name = |n: &str| injected_methods_named(n).next().expect("row present");
        assert_eq!(by_name("GetCurrentEntityID").return_type, "uint");
        // Handle to an array — not the `@[]&` reference spelling B059
        // inherited from the stale convert script.
        assert_eq!(
            by_name("GetManialinkPages").return_type,
            "array<CGameManialinkPage@>@"
        );
        assert_eq!(by_name("MwAddRef").return_type, "void");
        for m in INJECTED_METHODS {
            assert_eq!(m.arity, (0, 0), "{} arity", m.name);
        }
    }

    /// The Mechanism B receiver lists mirror `Openplanet.dll`'s six explicit
    /// registrations one-for-one. Anyone tempted to "simplify" these to a
    /// base type plus descendants should read
    /// `docs/probes/openplanet-injected-methods.md` first: the two rules
    /// agree on today's dump by coincidence, and this test is the tripwire.
    #[test]
    fn injected_receivers_mirror_the_binary_registrations() {
        let receivers = |n: &str| match &injected_methods_named(n).next().expect("row").receivers {
            InjectedReceiver::Exact(list) => list.to_vec(),
            InjectedReceiver::EveryEngineClass => panic!("{n} must be an explicit registration"),
        };
        assert_eq!(
            receivers("GetCurrentEntityID"),
            ["CGamePlayer", "CSmPlayer"]
        );
        assert_eq!(
            receivers("GetManialinkPages"),
            [
                "CGameNetwork",
                "CGameCtnNetwork",
                "CGameManiaPlanetNetwork",
                "CTrackManiaNetwork",
            ]
        );
        // GH #21's pair comes from the generic registration loop instead.
        for n in ["MwAddRef", "MwRelease"] {
            assert!(matches!(
                injected_methods_named(n).next().expect("row").receivers,
                InjectedReceiver::EveryEngineClass
            ));
        }
    }

    #[test]
    fn unknown_name_is_not_builtin() {
        assert!(!is_builtin_type("Foo"));
        assert!(!is_builtin_type(""));
        assert!(!is_builtin_type("CoroutineFuncSomethingElse"));
    }
}
