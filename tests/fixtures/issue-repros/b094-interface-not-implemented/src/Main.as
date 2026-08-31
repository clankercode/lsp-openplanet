// B094. Engine-measured (OP 1.29.5, 2026-08-31): every case below that we
// report, the game reports too, under the same single message form
// `Missing implementation of '<signature>'`. A same-name-different-signature
// method is NOT a distinct error — the engine calls it missing and names the
// signature it wanted.

interface IThing {
    void Go();
    int Count(float scale);
}

// 1. Nothing implemented at all — two errors, one per missing method.
class NoneImplemented : IThing {}

// 2. Same names, wrong signatures — still "missing", per the engine.
class WrongSignatures : IThing {
    int Go() { return 0; }
    int Count(int scale) { return scale; }
}

// 3. `abstract` does NOT exempt. Measured; the opposite of the usual rule.
abstract class AbstractStillErrors : IThing {} // lsp: ignore unused

// ── REQUIRED SILENCES, in the same file so "report everything" cannot pass ──

// 4. Correct implementation.
class Correct : IThing {
    void Go() {}
    int Count(float scale) { return int(scale); }
}

// 5. `private` satisfies the interface — measured clean.
class PrivateSatisfies : IThing {
    private void Go() {}
    private int Count(float scale) { return int(scale); }
}

// 6. Inherited from a base class satisfies.
class Base {
    void Go() {}
    int Count(float scale) { return int(scale); }
}
class InheritsImplementation : Base, IThing {}

// 7. A class that never declares the interface is not checked.
class Unrelated {
    string ToString() { return "u"; }
}

// Everything is referenced below so the only diagnostics this fixture
// produces are B094's — an unused-declaration warning here would be noise
// that hides a change in what this ticket actually asserts.
void Main() {
    Correct c;
    PrivateSatisfies p;
    InheritsImplementation i;
    Unrelated u;
    NoneImplemented n;
    WrongSignatures w;
    c.Go();
    i.Go();
    print("" + p.Count(1.0f) + i.Count(2.0f) + w.Go());
    print("" + n.Count(1.0f) + u.ToString());
}
