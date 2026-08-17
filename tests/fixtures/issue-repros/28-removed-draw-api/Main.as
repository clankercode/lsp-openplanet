// Minimal repro for GH #28 (OPEN — false negative, needs typedb):
// `Draw::GetWidth` / `Draw::GetHeight` were removed from the Openplanet API;
// the game compiler rejects them (`No matching symbol 'Draw::GetWidth'`) but
// the LSP silently accepts unknown `Ns::Fn` calls. Proper fix is the
// version-ranged API rules of GH #18; this fixture pins the repro.
// Requires --typedb-dir (see the gate test in tests/cli_check_tests.rs).

void DrawProbe() {
    float w = Draw::GetWidth();  // GH #28: game rejects; LSP must flag (currently silent)
    float h = Draw::GetHeight(); // GH #28: same
}

void Main() {}
