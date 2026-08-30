// B057, second half: what happens when the PLUGIN declares one of the names.
//
// `b057-nonglobal-enum-value-names` pins the rejects, and it cannot also pin
// this: declaring `North` there would make the very statement it asserts an
// error on resolve, so the two halves need two plugins. Same split as B053's
// `b053-ambiguous-enum-values` / `-workspace` pair.
//
// The narrowing runs LAST in `has_global_ident`, after the type, function,
// virtual-property, workspace-variable and workspace-enum-value lookups. So a
// plugin that declares one of the 2373 rejected names for itself keeps it, and
// must not inherit an error from an engine enum it never referred to. That is
// the false-POSITIVE direction, which is the damaging one for this tool.
//
// Every name below is a real collision, taken from
// `docs/probes/b053-bare-enum-values.tsv`, and the game rejects all four
// BARE — `No matching symbol` — when nothing declares them:
//
//     North         Game::CGameCtnBlock::ECardinalDirections  (nested)
//     VCenter2      Game::CGameManialinkControl::EAlignVertical (nested)
//     Once          UI::Cond and several nested Nadeo enums
//     Appearing     UI::Cond                                  (namespaced Core)
//
// A fence built on names that collide with nothing would pass on the
// merge-base and prove nothing.
//
// Game ground truth: RemoteBuild, Trackmania + Openplanet 1.29.5, 2026-08-30.
// This file compiled with zero diagnostics.

// A global VARIABLE of the name.
int North = 0;

// A global ENUM of the plugin's own carrying the name as a value. The unused
// suppression is not hiding anything from this fixture: a bare reference to a
// value does not currently count as a use of the enum that declares it, which
// is an unused-analysis gap and not a B057 one.
enum LocalAlignment // lsp: ignore unused
{
    VCenter2,
}

// A global FUNCTION of the name.
void Once()
{
}

// A global VIRTUAL PROPERTY of the name, via its `get_` accessor.
int get_Appearing()
{
    return 1;
}

void Main()
{
    print("" + North);
    print("" + VCenter2);
    Once();
    print("" + Appearing);
}
