// todo-cover-cases-next.txt, 2026-08-25, tm-dips-plus-plus:
// game: `'cp0' is already declared` when a second local of the same name
// is introduced in the SAME function scope. The failed redeclaration
// leaves the FIRST binding's type in scope, so `cp0.index` must then
// report that `index` is not a member of `CpSpec` (cascade).
//
// Inner-block shadowing is a different rule (GH #37d warning) and must
// stay a warning, not `already declared`.

class CpSpec {
    int dummy;
}

class Zone {
    int index;
}

void Motivating() {
    CpSpec@ cp0 = CpSpec();                 // first declaration
    auto @cp0 = Zone();                     // 21: ERROR already declared
    if (cp0 is null || cp0.index != 0) {}   // 22: ERROR not a member of CpSpec
}

void SameType() {
    int a = 1;
    int a = 2;                              // 27: ERROR
}

void DifferentType() {
    int b = 1;
    string b = "x";                         // 32: ERROR
}

void TypedThenAuto() {
    int c = 1;
    auto c = 2;                             // 37: ERROR
}

void AutoThenTyped() {
    auto d = 1;
    int d = 2;                              // 42: ERROR
}

void ConstThenMut() {
    const int e = 1;
    int e = 2;                              // 47: ERROR
}

void HandleThenValue() {
    CpSpec@ f = CpSpec();
    CpSpec f;                               // 52: ERROR
}

void ParamThenLocal(int g) {
    int g = 2;                              // 56: ERROR
}

void RefInThenLocal(int &in h) {
    int h = 2;                              // 60: ERROR
}

void RefOutThenLocal(int &out i) {
    int i = 2;                              // 64: ERROR
}

void HandleInThenLocal(CpSpec@ &in j) {
    CpSpec@ j;                              // 68: ERROR
}

void MultiDecl() {
    int k = 1, k = 2;                       // 72: ERROR
}

void HandleOutThenLocal(CpSpec@ &out p) {
    CpSpec@ p;                              // 75: ERROR
}

void ConstHandleThenHandle() {
    const CpSpec@ q = CpSpec();
    CpSpec@ q;                              // 80: ERROR
}

void ShadowIsNotRedecl() {
    int x = 1;
    if (true) {
        int x = 2;                          // 86: WARNING shadow, not already-declared
        x++;
    }
    x++;
}

void Main() {}
