// B057: a bare enum-value name is a symbol only if a GLOBALLY VISIBLE enum
// carries it. Being named is not enough.
//
// The game resolves a bare value name against the enums it puts in the global
// namespace. openplanet-lsp scanned EVERY named enum instead -- nested ones,
// namespaced ones, the lot -- and so stayed silent on 2373 names the compiler
// answers `No matching symbol` for. A false NEGATIVE: code that will not
// compile, with nothing said about it.
//
// B044 fixed the same shape for ANONYMOUS enums and generalised from the only
// evidence it had, concluding that a named enum's values always resolve. The
// exhaustive probe below showed the split that actually governs is the enum's
// VISIBILITY. An enum is globally visible iff its index key is depth <= 1 with
// the loader's synthetic module prefix stripped: the `Openplanet.h`
// module-level enums, the Nadeo dump's module-level enums (whose `Game::` /
// `Scene::` prefix is this crate's own key, not a namespace), and Core enums
// with no namespace. 68 keys, 60 distinct NAMES -- distinct names, because the
// loader keys one engine enum under several modules.
//
// Ground truth: all 2989 bare value names of the shipped type DB's named enums
// were submitted to the live game (RemoteBuild, Trackmania + Openplanet
// 1.29.5, 2026-08-30), one statement per line, and the verdicts are committed
// at `docs/probes/b053-bare-enum-values.tsv`:
//
//     2373  No matching symbol                  <- this ticket
//      592  accepted                            <- the fence below
//       22  Found multiple matching enum values <- B053, already landed
//        2  resolved to a non-enum symbol (`Main`, `RGBAColor`)
//
// Every name in this fixture is one of those rows. The reject side is grouped
// by WHY the enum is not globally visible, so a regression tells you which arm
// of the rule broke.
//
// Out of scope on purpose: how to spell a non-global enum's value CORRECTLY.
// The probe put these names in value position only, and the corpus carries no
// evidence for a qualified value spelling -- type lookup walks ancestor scopes
// and value lookup does not (see `docs/probes/README.md`), so a qualified form
// that works for the TYPE proves nothing about the VALUE. B052/B056 own the
// qualified surface.

// --- Reject: a value of a NESTED engine enum -------------------------------
//
// `Game::CGameCtnBlock::ECardinalDirections` is named and source-writable, and
// its values are still not in scope bare: `No matching symbol 'North'`.
void NestedEnumValues()
{
    print("" + North);
    print("" + East);
    print("" + South);
    print("" + West);
    // Carried by two nested enums, which is still zero globally visible ones.
    print("" + Party);
    print("" + Aborted);
    print("" + Action1);
    // Carried by exactly one nested enum.
    print("" + Achievements);
}

// --- Reject: a value of a NAMESPACED Core enum -----------------------------
//
// `UI::Cond` is named, source-writable and namespaced. The game rejects its
// bare values -- and also `UI::Appearing`, because value lookup does not walk
// ancestor scopes the way type lookup does.
void NamespacedCoreEnumValues()
{
    print("" + FirstUseEver);
    print("" + Appearing);
}

// --- Reject: a value of a NAMESPACED Nadeo enum ----------------------------
//
// `Meta::NPlugAnim::EExtractMotionMode` sits below the module prefix, so it is
// depth 2 once the prefix is stripped -- a real namespace, not a module.
void NamespacedNadeoEnumValues()
{
    print("" + Absolute);
}

// --- Reject: carried by BOTH an anonymous enum and a nested named one ------
//
// This is the case B044 got wrong. `VCenter2` is a value of the anonymous
// `Control::CControlBase::AlignVertical` AND of the named
// `Game::CGameManialinkControl::EAlignVertical`, and B044 pinned it as
// resolving on the strength of the second. The game says
// `No matching symbol 'VCenter2'`: the named enum it is shared with is
// nested, so it never put the name in the global scope either.
void SharedWithANestedEnum()
{
    print("" + VCenter2);
}

// --- Reject: carried by a namespaced Core enum AND several nested ones ------
//
// `Always` and `Once` each appear in `UI::Cond` and in a handful of nested
// Nadeo enums. Many non-global carriers still sum to no global one.
void ManyNonGlobalCarriers()
{
    print("" + Always);
    print("" + Once);
}

// --- Fence: a value of a GLOBALLY VISIBLE enum stays silent ----------------
//
// The 592 the game accepts. All three kinds of globally visible enum are
// represented, because they reach the index by three different routes and a
// narrowing that broke one of them would still pass on the other two.
void GloballyVisible()
{
    // `Openplanet.h` module-level enums, read by the loader for B046.
    print("" + Jump);
    print("" + DoubleJump);
    print("" + TechJump);
    print("" + GlassSmash);
    // A Core enum with no namespace.
    print("" + Space);
    print("" + Accept);
    print("" + A);
    // A Nadeo module-level enum: `Meta::EPlugAnimGraphStateSelectCond`, whose
    // `Meta::` is the loader's key, not a namespace the game has.
    print("" + Action1IsEvent);
    // Carried by a global enum AND by nested ones -- the global carrier is
    // what decides, so these must not be dragged down with their siblings.
    // `Left` is in `VirtualKey` and in five nested enums; `Border` is in the
    // global `EGxTexAddress` and in an anonymous member enum, which is the
    // fence B044 needed and put on the wrong name.
    print("" + Left);
    print("" + Border);
}

void Main()
{
    NestedEnumValues();
    NamespacedCoreEnumValues();
    NamespacedNadeoEnumValues();
    SharedWithANestedEnum();
    ManyNonGlobalCarriers();
    GloballyVisible();
}
