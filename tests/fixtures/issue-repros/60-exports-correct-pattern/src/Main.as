// Ordinary source file: the real bodies for the exported names, plus internal
// use of the shared-export helper. Must stay free of ERROR-severity
// diagnostics; warnings are out of scope for this fixture.
//
// Every signature here is repeated character for character by a stub in
// Exports_Decls.as. That is not tidiness — it is the binding rule (B086).
string SendEvent(const string &in a, const string &in b) {
    return a + b;
}

void FillLast(string &out slot) {
    slot = "last";
}

string Tag(string &in raw) {
    return "[" + raw + "]";
}

Payload@ Wrap(Payload@ p) {
    return p;
}

bool Accepts(Payload@ &in p) {
    return p !is null;
}

int Tally(int a, int b = 2) {
    return a + b;
}

uint Bump(uint n) {
    return n + 1;
}

void Main() {
    SendEvent("x", "y");
    SharedHelper@ h = SharedHelper();
    h.Helper();
}
