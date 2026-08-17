// Minimal repro for GH #47 (false negative):
// `UI::InputText(label, string, const bool)` has no matching overload — the
// game rejects it (OP 1.27.9, RemoteBuild):
//   (34, 29) ERR: No matching signatures to 'UI::InputText(const string, string, const bool)'
// Candidates are (label, str[, flags[, callback]]) and
// (label, str, bool&out changed[, flags[, callback]]) — a `bool` VALUE arg
// binds to neither. The issue's author fixed their code with the 2-arg form.
// Legal counterparts: the 2-arg form; and note the legal 3rd-arg `bool&out`
// form needs an l-value — `false` literal is not one, which is exactly why
// the game rejects the flagged line. The same BOOL-literal rule applies to
// unique overloads (`UI::Begin("w", true)` is rejected in-game), while
// NUMERIC literals bind `&out` via temporaries — the game accepts
// `Text::TryParseFloat("1.5", 0.0)` (RemoteBuild probe outmap, 2026-08-18).

void Main() {
    string s = "x";
    string a = UI::InputText("##name", s);         // legal: 2-arg overload
    bool changed = false;
    string b = UI::InputText("##n2", s, changed);  // legal: bool&out overload (l-value)
    string c = UI::InputText("##name", s, false);  // GH #47: game ERR, LSP must flag
    float pf = 0.0;
    bool okF = Text::TryParseFloat("1.5", pf);     // legal: float&out (l-value)
    bool okT = Text::TryParseFloat("1.5", 0.0);    // legal: numeric literal binds &out temp
    bool open = true;
    UI::Begin("w", open);                          // legal: bool&out l-value
    UI::End();
    UI::Begin("w2", true);                         // GH #47: bool literal into bool&out — flag
    UI::End();
    print(a + b + c + tostring(okF) + tostring(okT) + tostring(open));
}
