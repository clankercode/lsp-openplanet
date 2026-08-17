// Minimal repro for GH #37 (unreachable code warning parity).
// Game: statement after `return` in the same block -> WARN Unreachable code.

int UnreachProbe() {
    return 1;
    int y = 2;        // must warn (first unreachable statement)
    return y;
}

void Main() {}
