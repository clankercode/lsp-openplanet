// Minimal repro for B059 (false positive):
// Openplanet injects methods onto engine classes that the Nadeo type dump
// never lists, so `openplanet-lsp check` reported `UndefinedMember` on calls
// the game compiler accepts. Same family as GH #21 (`MwAddRef`/`MwRelease`).
//
//   uint GetCurrentEntityID()                      on CGamePlayer, CSmPlayer
//   array<CGameManialinkPage@>@ GetManialinkPages() on CGameNetwork,
//       CGameCtnNetwork, CGameManiaPlanetNetwork, CTrackManiaNetwork
//
// Those six registrations are the complete explicit surface, recovered from
// Openplanet.dll 1.29.5 — docs/probes/openplanet-injected-methods.md.
//
// Live callers: tm-dips-plus-plus src/PlayerState.as:121,
// tm-change-car-color src/Main.as:72, tm-magic-spectator src/PlayerState.as:105
// (GetCurrentEntityID); tm-shadowless-chrono (GetManialinkPages).
//
// The legal half must stay silent AND keep its real return type — a silence
// sentinel would hide the diagnostic while typing the local wrong. The fence
// half must stay loud: B006 trusts these types' non-empty member lists and
// must keep trusting them for genuinely missing members.

void UseInjected(CSmPlayer@ player, CGamePlayer@ basePlayer, // lsp: ignore unused
                 CGameNetwork@ gameNet, CGameCtnNetwork@ net) {
    // Legal: registered on CSmPlayer in its own right.
    uint entId = player.GetCurrentEntityID();
    // Legal: and on CGamePlayer, the parent.
    uint baseEntId = basePlayer.GetCurrentEntityID();
    // Legal: CGameNetwork is a registered receiver too. The stale
    // convert_nadeo.ts listed only CGameCtnNetwork, so this line is the one
    // that separates the DLL-derived list from the ticket's first guess.
    auto basePages = gameNet.GetManialinkPages();
    auto pages = net.GetManialinkPages();
    // `.Length` only resolves if the return really is an array — a silence
    // sentinel would leave this silent too.
    uint pageCount = pages.Length + basePages.Length;
    print("" + entId + " " + baseEntId + " " + pageCount);
}

void Fences(CMwNod@ nod, CSmPlayer@ player) { // lsp: ignore unused
    // Wrong receiver: the injection is not on every engine type.
    auto _a = nod.GetCurrentEntityID();
    // Wrong receiver: GetManialinkPages is on the network types.
    auto _b = player.GetManialinkPages();
    // Wrong arity: the injected signature takes no arguments.
    auto _c = player.GetCurrentEntityID(1);
    // Real typo on a trusted Nadeo member list (B006 must not be widened).
    auto _d = player.GetCurrentEntityIdd();
}

void Main() {}
