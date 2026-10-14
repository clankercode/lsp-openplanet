// Minimal repro for GH #37 (signed/unsigned comparison warning parity).
// Game (live probe 2026-08-17): `i < u` -> WARN Signed/Unsigned mismatch.

void SignedUnsignedProbe() {
    int i = 1;
    uint u = 2;
    bool b = i < u;    // must warn
    bool eq = i == u;  // must warn
    bool ne = i != u;  // must warn
    bool ok1 = i < 3;  // both signed — silent
    uint v = 1;
    bool ok2 = u < v;  // both unsigned — silent
    bool ok3 = i == 0; // literal comparison — silent
    bool ok4 = 0 != u; // literal comparison — silent
    bool ok5 = int(i) == u; // explicit conversion — silent
}

void Main() {}
