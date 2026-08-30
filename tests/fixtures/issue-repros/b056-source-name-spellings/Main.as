// B056: a type or enum answers to its SOURCE NAME, and to nothing else.
//
// The index key is not the game's name for a type, and openplanet-lsp accepted
// the key. So `Game::CGameCtnApp` resolved -- the game answers `Namespace
// 'Game' doesn't exist.` -- and so did a bare nested enum name, and so did any
// trailing path. 1012 spellings the game rejects, silently accepted: false
// NEGATIVES on code that will not compile.
//
// The rule, and it is ONE rule rather than the two the ticket describes:
//
//   * every source-writable entry has a SOURCE NAME, which is the spelling
//     the game accepts; a spelling resolves when it equals that name,
//   * or when it is `P::T` with P a REAL scope and T a globally visible enum,
//     which is AngelScript's ancestor walk (B052, already landed).
//
// The source name is derived per ORIGIN, and the two origins are asymmetric.
// This is the part that is easy to get wrong in both directions at once:
//
//   * NADEO. Keyed `Module::Rest`, and the module -- `Game`, `Scene`, `Audio`,
//     `Meta` -- is openplanet-lsp's own invention. The game has no such
//     namespace, so the module is DROPPED.
//   * CORE. The namespace is real and must be written in FULL.
//
// A rule phrased "drop leading segments" gets `UI::Plot::Marker` wrong.
// A rule phrased "require the whole key" gets `CGameMenu::EMenuOrder` wrong.
// Both spellings are recorded game verdicts, in opposite directions.
//
// Ground truth: 1901 spellings submitted to the live game as `<spelling> v;`
// (RemoteBuild, Trackmania + Openplanet 1.29.5, 2026-08-30), committed at
// `docs/probes/b052-qualified-enum-spellings.tsv`. Every name below is one of
// those rows. `every_probed_type_spelling_matches_the_games_verdict` checks
// all 1901 through the real resolver; this fixture is the end-to-end half.

// --- Rejected: a NADEO key written with its synthetic module ---------------
//
// `Namespace 'Game' doesn't exist.` and friends. The module segment exists
// nowhere in the game.
void ModulePrefixIsNotANamespace()
{
    Game::CGameCtnApp _a;
    Game::CGameMenu _b;
    Game::CGameMenu::EMenuOrder _c;
    Audio::CAudioScriptManager::ELibSound _d;
    Meta::NPlugAnim::EExtractMotionMode _e;
    Scene::ESceneCharVisNotice _f;
    Enums::ESceneCharVisNotice _g;
}

// --- Rejected: a CORE key written with a segment dropped -------------------
//
// The mirror image. `UI::Plot::Marker` is the name; `Plot::Marker` answers
// `Namespace 'Plot' doesn't exist.` A trailing-path match accepted it.
void CoreNamespaceMustBeWrittenInFull()
{
    Plot::Marker _a;
    Plot::Axis _b;
    Plot::Cond _c;
}

// --- Rejected: a non-global enum written bare ------------------------------
//
// `Identifier 'X' is not a data type in global namespace`. Being NAMED lets
// source write the enum's own name; it does not put that name in the global
// namespace. 436 names in all.
void NonGlobalEnumsNeedTheirQualifier()
{
    EMenuOrder _a;
    EExtractMotionMode _b;
    ECardinalDirections _c;
    ButtonFlags _d;
    Cond _e;
    Align _f;
}

// --- Accepted: the NADEO source name, module dropped -----------------------
void NadeoSourceNames()
{
    CGameCtnApp _a;
    CGameMenu _b;
    CGameMenu::EMenuOrder _c;
    CAudioScriptManager::ELibSound _d;
    NPlugAnim::EExtractMotionMode _e;
    CGameCtnBlock::ECardinalDirections _f;
}

// --- Accepted: the CORE source name, namespace in full ---------------------
void CoreSourceNames()
{
    UI::ButtonFlags _a;
    UI::Cond _b;
    nvg::Align _c;
    UI::Plot::Marker _d;
    UI::Plot::Axis _e;
}

// --- Accepted: a HEADER enum, bare ----------------------------------------
//
// The third origin, and it gets its own arm in the source-name computation
// rather than falling through the Nadeo one: a header key is a single
// segment, and "drop the leading module" applied to it would yield the empty
// string. B046 is what makes these visible at all.
void HeaderEnumsAreBare()
{
    ESceneCharVisNotice _a;
    ESceneVehicleVisEvent _b;
}

// --- Accepted: a GLOBAL enum behind any real scope (B052's ancestor walk) --
//
// Unchanged by this ticket, and pinned because the narrowing runs right next
// to it. `Meta` is the sharp case: it is BOTH a module name the loader
// invents AND a real Core namespace. The paired probe settles it --
// `Meta::ZZZNoSuchEnumName` answers `Identifier 'ZZZNoSuchEnumName' is not a
// data type in namespace 'Meta' or parent`, so `Meta` is a scope, while
// `Game::ZZZNoSuchEnumName` answers `Namespace 'Game' doesn't exist.`
void GlobalEnumsBehindARealScope()
{
    VirtualKey _a;
    UI::VirtualKey _b;
    CGameMenu::VirtualKey _c;
    EPlugAnimGraphStateSelectCond _d;
    Meta::EPlugAnimGraphStateSelectCond _e;
    Meta::ESceneCharVisNotice _f;
}

void Main()
{
    ModulePrefixIsNotANamespace();
    CoreNamespaceMustBeWrittenInFull();
    NonGlobalEnumsNeedTheirQualifier();
    NadeoSourceNames();
    CoreSourceNames();
    HeaderEnumsAreBare();
    GlobalEnumsBehindARealScope();
}
