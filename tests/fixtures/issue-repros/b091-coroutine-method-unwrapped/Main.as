// B091. A class METHOD does not implicitly decay to a funcdef handle: the
// funcdef CONSTRUCTOR is the method-to-funcdef conversion. Every bare method
// reference below was SILENT before this ticket.
//
// Generalised on three axes, because the bind test is shared and a rule that
// only reached `startnew` would be the same defect one position over:
//   * RECEIVER spelling -- `this.M`, a local, a workspace global, an indexed
//     element, a member chain. Each takes a different arm of
//     `method_ref_receiver_type`, and only `this.M` had ever been exercised.
//   * WRAPPER family -- the no-userdata slot and every `CoroutineFuncUserdata*`
//     slot, since "or similar (depending on args)" is which one the argument
//     list demands.
//   * POSITION -- an engine call argument, a `Meta::StartWithRunContext`
//     argument, a workspace funcdef parameter, a declaration initializer and
//     an assignment. These are B023's, B089's and B076's positions
//     respectively, and they share one predicate precisely so they cannot give
//     three answers to one question.

funcdef void Job();

class Noodle {
    void Disconnect() {}
}

class Inner {
    void Tick() {}
}

class Holder {
    Inner@ inner;
    Holder() { @inner = Inner(); }
}

class Worker {
    void Tick() {}
    void Revert(ref@ data) { if (data is null) return; }
    void OnBool(bool b) { if (b) return; }
    void OnInt(int64 i) { if (i == 0) return; }
    void OnUint(uint64 u) { if (u == 0) return; }
    void OnDouble(double d) { if (d == 0) return; }
    void OnString(const string &in s) { if (s == "") return; }

    void StartAllUnwrapped() {
        startnew(this.Tick);
        startnew(this.Revert, this);
        startnew(this.OnBool, true);
        startnew(this.OnInt, 1);
        startnew(this.OnUint, uint(1));
        startnew(this.OnDouble, 1.0);
        startnew(this.OnString, "x");
        Meta::StartWithRunContext(Meta::RunContext::MainLoop, this.Tick);
        Meta::StartWithRunContext(Meta::RunContext::MainLoop, this.Revert, this);
    }
}

Worker@ g_worker = Worker();
Noodle@[] g_edges = { Noodle() };

void Take(Job@ j) { if (j is null) return; }

void Main() {
    Worker w;
    w.StartAllUnwrapped();
    // Every other receiver spelling, at the plain `CoroutineFunc` slot.
    startnew(w.Tick);
    startnew(g_worker.Tick);
    startnew(g_edges[0].Disconnect);
    Holder h;
    startnew(h.inner.Tick);
    Meta::StartWithRunContext(Meta::RunContext::MainLoop, w.Tick);
    // A WORKSPACE funcdef parameter is the same binding one position over.
    Take(w.Tick);
    // Declaration and assignment positions, engine funcdef and workspace
    // funcdef. These read `Can't implicitly convert`, which is the wording
    // that position already uses for a mismatched free function.
    CoroutineFunc@ engineSlot = w.Tick;
    Job@ workspaceSlot = w.Tick;
    @engineSlot = w.Tick;
    // The "or similar (depending on args)" half: a correctly CONSTRUCTED
    // delegate of the wrong family. Already reported before this ticket --
    // measured, not assumed -- and pinned here so the two halves of the
    // ticket cannot drift apart.
    startnew(CoroutineFunc(w.Tick), w);
    print(workspaceSlot is null ? "a" : "b");
}
