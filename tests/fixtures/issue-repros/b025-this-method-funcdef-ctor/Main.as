// B025 minimal repro: `this.Method` / `obj.Method` references at funcdef
// ctor sites are method *references* — the ctor is the REQUIRED
// method-to-funcdef conversion, because class methods do not implicitly
// decay the way free functions do. Before the fix the LSP typed these
// expressions as the method's return type and emitted
//   error: argument 1 of `CoroutineFunc`: expected `void ()`, got `void`
// (53 such false errors on tm-editor-plus-plus alone).
//
// Game ground truth (RemoteBuild probe 2026-08-29, OP 1.29.5):
//   - every call in the LEGAL half compiles clean;
//   - each DIAGNOSE line errors in-game (`Can't create delegate` / the
//     member is a property, not a function) — the LSP must flag exactly
//     those and nothing else.

class Noodle {
    void Disconnect() {}
}

class Worker {
    string label = "x";
    void Tick() {}
    void Revert(ref@ data) {}
    bool BoolTick() { return true; }
    void OV() {}
    bool OV(int x) { return true; }

    void LegalThis() {
        // legal: ctor conversion of an own method
        startnew(CoroutineFunc(this.Tick));
        // legal: void(ref@) method matches CoroutineFuncUserdata (void (ref))
        startnew(CoroutineFuncUserdata(this.Revert), this);
        // legal: the void() overload binds (bool OV(int) does not block it)
        startnew(CoroutineFunc(this.OV));
        // legal: handle init from a method reference
        auto @h = CoroutineFunc(this.Tick);
        print(h !is null ? "a" : "b");
    }
}

Noodle@[] g_edges;

void Main() {
    Worker w;
    // legal: method reference through a local instance
    startnew(CoroutineFunc(w.Tick));
    // legal: receiver is a workspace global / an indexed array
    startnew(CoroutineFunc(g_edges[0].Disconnect));
    // DIAGNOSE: bool() method into void() CoroutineFunc — game ERR
    startnew(CoroutineFunc(w.BoolTick));
    // DIAGNOSE: void() method into CoroutineFuncUserdata (void (ref)) — game ERR
    startnew(CoroutineFuncUserdata(w.Tick), w);
    // DIAGNOSE: a property, not a function — game ERR
    startnew(CoroutineFunc(w.label));
    w.LegalThis();
}
