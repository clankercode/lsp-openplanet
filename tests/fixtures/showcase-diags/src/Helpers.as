// Workspace helpers — warning + member + missing return.

// bare `string` param → StringByValueParam warning
void ShowStatus(string msg) {
    UI::Text(msg);
}

void DrawBadge(const string &in text) {
    UI::Text(text);
}

// missing return on non-void function
int BadgePriority() {
    // fall off end
}

void ProbeStringApi() {
    string name = "board";
    // undefined member on external string
    int bad = name.NotARealMethod();
}
