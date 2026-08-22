// Minimal repro for GH #28 (+ #27): the game compiler rejects free-function
// calls into namespaces it doesn't know (`No matching symbol 'Draw::GetWidth'`)
// and calls to members missing from a KNOWN namespace
// (`No matching symbol 'UI::GetIO'`). The LSP used to stay silent on both.
//
// Rules (typedb-driven, no version overlays needed for the core signal):
//  1. If `Ns` appears as a functions[].ns in the typedb (a KNOWN API
//     namespace — UI has 237 fns on 1.29.5) and `Ns::Fn` does not,
//     diagnose `No matching symbol 'Ns::Fn'` with high confidence.
//  2. If `Ns` is absent from the typedb entirely (no functions, no types,
//     no enums — `Draw` on 1.29.5), diagnose the same. Cheap trigger that
//     catches removed namespaces like Draw without a version table.
//  3. Unknown user namespaces (not in the typedb at all AND not a known
//     game API namespace) — this fixture doesn't cover that case; the
//     checker must stay quiet there (see the negative cases in
//     tests/cli_check_tests.rs).
// Relocations hint (GH #27): Draw::GetWidth → Display::GetWidth etc.

void DrawProbe() {
    float w = Draw::GetWidth();   // line 18: game ERR `No matching symbol 'Draw::GetWidth'`
    float h = Draw::GetHeight();  // line 19: same for GetHeight
}

void GetIOProbe() {
    // UI is a known namespace (typedb has it); UI::GetIO is not a member.
    // Game ERR: `No matching symbol 'UI::GetIO'` (tm-draw-tests GraphTab.as:361).
    vec2 delta = UI::GetIO().MouseDelta;  // line 24
}

void Main() {}
