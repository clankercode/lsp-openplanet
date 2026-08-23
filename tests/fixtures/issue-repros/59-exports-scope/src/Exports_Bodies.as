// WRONG pattern (game-verified 2026-08-23): bodies written in a strict
// `[script] exports` file. The file compiles into CONSUMERS only — the
// defining plugin's own module does not get these symbols, so the game
// fails with "No matching symbol 'SendEvent'".
string SendEvent(string a, string b) {
    return a + b;
}
