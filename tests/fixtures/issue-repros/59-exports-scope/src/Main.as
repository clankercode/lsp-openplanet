// Sibling file calling the export-file body directly — must ERROR now
// (game: No matching symbol 'SendEvent').
void Main() {
    SendEvent("x", "y");
}
