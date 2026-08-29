// B027: delegate construction requires EXACT parameter passing modes.
// Game ground truth (RemoteBuild probe 2026-08-29, OP 1.29.5): every
// mode-mismatched ctor below errors `Can't create delegate` (free-fn
// mismatches surface as `Can't implicitly convert from '<function>@const'`),
// including `&in` mismatches; `int &inout` is itself illegal for primitives
// ("Only object types that support object handles can use &inout. Use &in or
// &out instead"), so the `&inout` rows use an object type. Exact matches
// bind. The LSP must flag exactly the MISMATCH lines and nothing else.

funcdef void FPlain(int);
funcdef void FOut(int &out);
funcdef void FIn(int &in);

class C {
    void MPlain(int x) {}
    void MOut(int &out x) {}
    void MIn(int &in x) {}
}

void FnPlain(int x) {}
void FnOut(int &out x) {}
void FnIn(int &in x) {}

class H {}

funcdef void GInout(H &inout);

class D {
    void HInout(H &inout x) {}
    void HOut(H &out x) {}
}

void Main() {
    C c;
    D d;
    // MISMATCH: &out method into plain funcdef — game ERR
    FPlain@ a1 = FPlain(c.MOut);
    // MISMATCH: plain method into &out funcdef — game ERR
    FOut@ a2 = FOut(c.MPlain);
    // MISMATCH: free functions, both polarities — game ERR
    FPlain@ a3 = FPlain(FnOut);
    FOut@ a4 = FOut(FnPlain);
    // MISMATCH: &in is enforced exactly like &out — game ERR
    FIn@ a5 = FIn(c.MPlain);
    FPlain@ a6 = FPlain(c.MIn);
    // exact &in match binds — clean
    FIn@ a7 = FIn(c.MIn);
    FIn@ a8 = FIn(FnIn);
    // exact plain / &out matches bind — clean
    FPlain@ a9 = FPlain(c.MPlain);
    FOut@ a10 = FOut(c.MOut);
    // MISMATCH: &inout funcdef vs &out method — game ERR
    GInout@ a11 = GInout(d.HOut);
    // exact &inout match binds — clean
    GInout@ a12 = GInout(d.HInout);
}
