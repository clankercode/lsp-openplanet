// Minimal repro for GH #52 repro 2 (false negative):
// A ternary whose branches have no unifiable common type. The game
// rejects `c ? "" : map.MapInfo.FileName` (string literal vs wstring
// member) with
//   ERR : Can't find unambiguous implicit conversion to make both
//         expressions have the same type
// — the plugin fix is a plain if or an explicit cast on one branch.
// Same game error fires for bool-vs-int and nat3-vs-int3 branches.
// Legal counterparts (same type, numeric mix, null+handle) must stay
// silent.

void F(CGameCtnChallenge@ map, bool c, CGameCtnEditorFree@ editor, int i, uint u) {
    string bad1 = c ? "" : map.MapInfo.FileName;        // GH #52: string vs wstring — game ERR, LSP must flag
    int bad2 = c ? 1 : true;                            // bool vs int — same game ERR
    auto bad3 = c ? map.Size : int3(1, 2, 3);           // nat3 vs int3 — same game ERR
    string ok1 = c ? "" : string(map.MapInfo.FileName); // legal: both string
    auto ok2 = c ? map.MapInfo.Name : map.MapInfo.FileName; // legal: both wstring
    auto ok3 = c ? 1 : 2.5;                             // legal: numeric mix
    auto ok4 = c ? i : u;                               // legal: int/uint vars
    auto ok5 = c ? null : editor;                       // legal: null + handle
    print(ok1 + ok3 + ok4);
    if (ok5 is null) print("x");
}

void Main() {}
