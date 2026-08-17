// Minimal repro for GH #37 (variable shadowing warning parity).
// Game: inner-scope local redeclaring an outer local's name ->
//       WARN Variable 'x' hides another variable of same name in outer scope.

void ShadowProbe() {
    int x = 1;
    if (true) {
        int x = 2;    // must warn (hides outer x)
        x++;
    }
    int y = 3;        // silent (distinct name)
    x++;
    y++;
}

void Main() {}
