// Minimal repro for GH #37 (implicit sign change warning parity).
// Game: `uint u = -1;` -> WARN Implicit conversion changed sign of value.

void SignChangeProbe() {
    uint u = -1;      // must warn
    uint ok = 1;      // silent
}

void Main() {}
