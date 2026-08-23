// Ordinary source file: real body for the exported name + internal use
// of both the body and the shared-export helper. Must stay free of
// ERROR-severity diagnostics (pre-existing warnings about string-by-value
// params and the import/decl usage quirk are out of scope here).
string SendEvent(const string &in a, const string &in b) {
    return a + b;
}

void Main() {
    SendEvent("x", "y");
    SharedHelper@ h = SharedHelper();
    h.Helper();
}
