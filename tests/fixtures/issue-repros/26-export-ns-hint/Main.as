// Minimal repro for GH #26 (diagnostic quality):
// `unknown type` for a qualified name whose leading segment is NOT a known
// Core/Nadeo engine namespace almost always means a cross-plugin export
// dependency that failed to load (missing install / not in --plugins-dir /
// exports didn't parse). The error must stay an error, but it should carry
// a note pointing at the common cause.
//
// MLFeed is a real plugin module (dogfood origin of this issue); Math is a
// Core API namespace — the note must fire for the former, not the latter.

void Main() {
    // Unknown type in a cast — qualified, non-engine prefix.
    MLFeed::PlayerCpInfo@ info = null; // GH #26: expect note about plugin exports
    // Engine-namespace typo must stay a plain error, no note.
    Math::NotARealThing@ nope = null;
}
