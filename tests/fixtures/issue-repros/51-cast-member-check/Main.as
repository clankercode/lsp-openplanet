// Minimal repro for GH #51 (false negative):
// Member access on the result of `cast<T>(expr)` must check against T's
// member set. `CControlFrame` has only `ChildsRelativeLocations` — the game
// rejects `cf.Visible` with
//   (289, 34) ERR: 'Visible' is not a member of 'CControlFrame'
// Legal counterparts (own member + base-chain members) must stay silent.

void Walk(CControlContainer@ root) {
    for (uint i = 0; i < root.Childs.Length; i++) {
        auto c = root.Childs[i];                  // CControlBase@
        auto cf = cast<CControlFrame>(c);
        if (cf.ChildsRelativeLocations.Length == 0) continue; // legal: own member
        if (cf.IsReadOnly) continue;              // legal: base CControlBase
        if (cf.Childs.Length == 0) continue;      // legal: base CControlContainer
        if (cf.Visible) continue;                 // GH #51: game ERR, LSP must flag
    }
}

void Main() {}
