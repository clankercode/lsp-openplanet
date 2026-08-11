// Entry / UI — undefined id, unknown type, external arity.

const string MenuTitle = "Showcase Diags";

[Setting hidden]
bool g_WindowOpen = true;

void Main() {
    // undefined identifier
    int boot = g_MissingBootFlag;

    // unknown type
    NoSuchEngineType@ ghost;

    // workspace call wrong arg type (ShowStatus expects string)
    ShowStatus(42);
}

void RenderMenu() {
    if (UI::MenuItem(MenuTitle, "", g_WindowOpen)) {
        g_WindowOpen = !g_WindowOpen;
    }
}

void Render() {
    if (!g_WindowOpen) return;
    UI::Begin(MenuTitle);
    DrawBadge("ok");
    UI::End();
}
