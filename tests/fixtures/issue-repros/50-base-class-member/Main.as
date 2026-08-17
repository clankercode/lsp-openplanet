// Minimal repro for GH #50 (false negative):
// Member access on a typedb engine type must be checked against the type's
// own members AND its base-class chain. `CControlBase` (Game data) has no
// `Visible` member — the game compiler rejects `c.Visible` with
//   (275, 19) ERR: 'Visible' is not a member of 'CControlBase'
// (member lives on the unrelated `CGameManialinkControl` branch).
// The legal counterpart (`IsReadOnly`, a real CControlBase member, plus the
// base-class member `Model` from CSceneMobil) must stay silent.

void WalkContainer(CControlContainer@ root) {
    for (uint i = 0; i < root.Childs.Length; i++) {
        auto c = root.Childs[i];        // CControlBase@
        if (!c.Visible) continue;       // GH #50: game ERR, LSP must flag
        if (c.IsReadOnly) continue;     // legal: own member of CControlBase
        if (c.Model is null) continue;  // legal: inherited from CSceneMobil
    }
}

void Main() {}
