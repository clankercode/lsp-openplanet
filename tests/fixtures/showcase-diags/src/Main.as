// Entry points with deliberate typecheck issues for screenshots / CI.
// Keep braces balanced — we want type diagnostics, not parse cascades.

const string MenuTitle = "Showcase Diags";

[Setting hidden]
bool g_WindowOpen = true;

void Main() {
    // undefined identifier
    int boot = g_MissingBootFlag;

    // unknown type
    NoSuchEngineType@ ghost;

    // external free-function arity (UI::Selectable expects 2..=3)
    bool picked = UI::Selectable("lane", true, 0, 99);

    // external method arity — string::IndexOf is 1-arg only
    string label = "Trackmania";
    int idx = label.IndexOf("man", 0);

    // workspace call with wrong arg type (ShowStatus expects string)
    ShowStatus(42);

    // return-type mismatch: ComputeTitle returns int
    string title = ComputeTitle();
    UI::Text(title);
}

void RenderMenu() {
    if (UI::MenuItem(MenuTitle, "", g_WindowOpen)) {
        g_WindowOpen = !g_WindowOpen;
    }
}

void Render() {
    if (!g_WindowOpen) return;

    UI::Begin(MenuTitle);
    // wrong arg type into workspace helper (DrawBadge wants string)
    DrawBadge(vec2(4.0f, 8.0f));
    UI::End();
}

// openplanet-lsp defines DEPENDENCY_* for every optional_dependencies entry
// listed in info.toml (found or not). These branches stay active on CI.
#if DEPENDENCY_SHOWCASEFAKEHOOK
void OnFakeHookDefineActive() {
    // unknown type under DEPENDENCY_SHOWCASEFAKEHOOK
    FakeHookClient@ client;
    // undefined identifier
    client = g_HookClient;
}
#else
void OnFakeHookDefineInactive() {
    // clean fallback if define policy ever changes
    trace("ShowcaseFakeHook define inactive");
}
#endif
