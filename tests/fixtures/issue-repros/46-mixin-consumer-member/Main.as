// Minimal repro for GH #46 (fixed in 8c13ef5):
// A mixin class body references a member its *consuming* class declares.
// The game compiler checks mixin bodies in the consumer's context, so
// `tabs` resolves in-game. Before the fix the LSP flagged
// `undefined identifier `tabs`` at the `tabs.Length` access below.

class Tab {
    void DrawWindow() {}
}

mixin class HasGroupMeta {
    bool WritingJson_WriteObjKeyEl(string[]& parts, bool commaPrefix = false) {
        if (tabs.Length == 0) return false; // GH #46: was a false `undefined identifier`
        return true;
    }
}

class TabGroup : HasGroupMeta {
    Tab@[] tabs;
}

void Main() {}
