// Bodies for the export surface. NOTHING in this plugin calls them —
// consumers bind by signature — so before B062 every one was reported unused.
string SendEvent(const string &in kind, const string &in payload) {
    return Helper(kind) + payload;
}

bool IsConnected() { return true; }

int Tally(int a, int b = 2) { return a + b; }

// Private helper reachable ONLY from an exported body. Already silent before
// B062: the analysis asks "referenced anywhere", not "reachable from a root",
// so transitive reachability costs nothing. Pinned so that stays true.
string Helper(const string &in s) { return s; }

// THE OVER-ROOTING GATE, deliberately in the SAME FILE as the rooted bodies
// so that "exempt the file" cannot pass this fixture: no stub names it, so it
// must still be reported.
void TrulyDead() {}

// Same name as an exported stub but a CLASS MEMBER, not a global. An `import`
// stub names a global function, never a method, so this must still warn.
class Holder {
    void SendEvent() {}
}
