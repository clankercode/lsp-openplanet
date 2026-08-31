// B091, THE NEGATIVE CONTROL, and it earns its keep: a rule that rejected
// every function-shaped argument -- or every member access -- would pass the
// sibling fixture on its own and only fail here.
//
// Three things have to stay legal and they are easy to conflate:
//   * a class method WRAPPED in the right funcdef constructor;
//   * a FREE function passed bare, which does implicitly decay;
//   * a member access that is NOT a method -- a funcdef-typed FIELD read.
// The last is the discriminator: the rule keys on "this member resolves to a
// method", not on "this argument is a member access".

funcdef void Job();

class Noodle {
    void Disconnect() {}
}

class Inner {
    void Tick() {}
}

class Holder {
    Inner@ inner;
    CoroutineFunc@ cb;
    Job@ ownCb;
    Holder() {
        @inner = Inner();
        @cb = CoroutineFunc(FreeTick);
        @ownCb = Job(FreeTick);
    }
}

class Worker {
    void Tick() {}
    void Revert(ref@ data) { if (data is null) return; }
    void OnBool(bool b) { if (b) return; }
    void OnInt(int64 i) { if (i == 0) return; }
    void OnUint(uint64 u) { if (u == 0) return; }
    void OnDouble(double d) { if (d == 0) return; }
    void OnString(const string &in s) { if (s == "") return; }

    void StartAllWrapped() {
        startnew(CoroutineFunc(this.Tick));
        startnew(CoroutineFuncUserdata(this.Revert), this);
        startnew(CoroutineFuncUserdataBool(this.OnBool), true);
        startnew(CoroutineFuncUserdataInt64(this.OnInt), 1);
        startnew(CoroutineFuncUserdataUint64(this.OnUint), uint(1));
        startnew(CoroutineFuncUserdataDouble(this.OnDouble), 1.0);
        startnew(CoroutineFuncUserdataString(this.OnString), "x");
        Meta::StartWithRunContext(Meta::RunContext::MainLoop, CoroutineFunc(this.Tick));
        Meta::StartWithRunContext(Meta::RunContext::MainLoop, CoroutineFuncUserdata(this.Revert), this);
        // PINNED NEUTRALITY, not a claim of legality. A bare identifier
        // naming a method of the enclosing class is the same language rule as
        // `this.Tick`, but the identifier arm of `lookup_arg_function_sigs`
        // merges class methods with same-named free and namespace functions
        // into one candidate list, so rejecting on "a method is among the
        // candidates" would reject a legal free-function decay whenever a
        // method shares its name. B097 blocks the probe that would settle
        // whether the engine accepts this spelling at all, so the neutral
        // behaviour is chosen BY CONSTRUCTION -- the call B102 made on
        // const-ness. Tightening it later has to be a deliberate change with
        // a probe behind it, and this line is what makes that visible.
        startnew(Tick);
    }
}

Worker@ g_worker = Worker();
Noodle@[] g_edges = { Noodle() };

void FreeTick() {}
void FreeRevert(ref@ data) { if (data is null) return; }
void FreeOnBool(bool b) { if (b) return; }
void FreeOnInt(int64 i) { if (i == 0) return; }
void FreeOnString(const string &in s) { if (s == "") return; }

void Take(Job@ j) { if (j is null) return; }

void Main() {
    Worker w;
    w.StartAllWrapped();
    // Every receiver spelling the sibling fixture rejects, wrapped.
    startnew(CoroutineFunc(w.Tick));
    startnew(CoroutineFunc(g_worker.Tick));
    startnew(CoroutineFunc(g_edges[0].Disconnect));
    Holder h;
    startnew(CoroutineFunc(h.inner.Tick));
    Meta::StartWithRunContext(Meta::RunContext::MainLoop, CoroutineFunc(w.Tick));
    // Free functions DO decay -- no constructor required, at any position.
    startnew(FreeTick);
    startnew(FreeRevert, w);
    startnew(FreeOnBool, true);
    startnew(FreeOnInt, 1);
    startnew(FreeOnString, "x");
    Meta::StartWithRunContext(Meta::RunContext::MainLoop, FreeTick);
    Take(FreeTick);
    CoroutineFunc@ engineSlot = FreeTick;
    Job@ workspaceSlot = FreeTick;
    // Wrapped method references at the declaration and workspace-parameter
    // positions the sibling fixture rejects unwrapped.
    Take(Job(w.Tick));
    @engineSlot = CoroutineFunc(w.Tick);
    // A member access that is a FIELD, not a method. This row is what makes
    // the rule falsifiable: retyping `Holder::cb` to a wrong-shaped funcdef
    // was MEASURED to turn `startnew(h.cb)` into
    // `No matching signatures to 'startnew(StringTick@)'`, so the silence
    // here is the check passing, not the check being absent.
    startnew(h.cb);
    startnew(h.ownCb);
    // A funcdef-typed local, reached as a plain identifier.
    CoroutineFunc@ localCb = CoroutineFunc(FreeTick);
    startnew(localCb);
    print(workspaceSlot is null ? "a" : "b");
}
