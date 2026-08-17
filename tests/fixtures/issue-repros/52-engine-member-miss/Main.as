// Minimal repro for GH #52 repro 1 (false negative):
// Member lookup on an engine type must consult the type's own member set
// plus its base chain — and nothing else. The game rejects
//   string fn = string(map.FileName);
// with `ERR : 'FileName' is not a member of 'CGameCtnChallenge'` — the
// correct property path is `map.MapInfo.FileName` (MapInfo is
// CGameCtnChallengeInfo@, whose BASE CGameFid carries FileName).
// Legal counterparts must stay silent.

void F(CGameCtnChallenge@ map) {
    string bad = string(map.FileName);   // GH #52: game ERR, LSP must flag
    auto ok = map.MapInfo.FileName;      // legal: base-chain member (CGameFid)
    auto mapName = map.MapName;          // legal: own member (wstring)
    print(bad + ok + mapName);
}

void Main() {}
