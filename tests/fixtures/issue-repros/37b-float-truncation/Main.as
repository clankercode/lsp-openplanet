// Minimal repro for GH #37 (float->int implicit conversion warning parity).
// Game: literal -> WARN `Implicit conversion of value is not exact`;
//       float var -> WARN `Float value truncated in implicit conversion to integer`.

void FloatTruncProbe() {
    int ms = 3.7;     // must warn (not-exact literal)
    float f = 1.5;
    int g = f;        // must warn (truncated)
    int ok = 3;       // silent
}

void Main() {}
