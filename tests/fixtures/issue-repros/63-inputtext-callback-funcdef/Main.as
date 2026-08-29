// GH #63 regression net: `UI::InputTextCallback` is a namespaced Core
// funcdef (`void InputTextCallback(InputTextCallbackData@ data)`), and every
// pattern below is legal AngelScript that the LSP must keep accepting with
// 0 diagnostics:
//   - a matching free function / class method decays to `InputTextCallback@`
//     where an overload parameter expects it (UI::InputText[Multiline])
//   - the ctor form `UI::InputTextCallback(fn)`
//   - bare-handle assignment `UI::InputTextCallback@ cb = OnInput;`
//   - passing a funcdef-typed variable back into the callback slot
//   - calling through a funcdef handle
//   - InputTextCallbackData member access in the callback body
// Unit-level coverage lives in src/typecheck/checker.rs (typedb_funcdef_ctor_*
// tests); this fixture proves the end-to-end `openplanet-lsp check` path.
// Game ground truth (RemoteBuild probe 2026-08-29, OP 1.29.5): compiles clean.

void OnInput(UI::InputTextCallbackData@ data) {
    if (data.HasSelection()) {
        data.DeleteChars(data.SelectionStart, data.SelectionEnd - data.SelectionStart);
    }
    data.InsertChars(data.CursorPos, "x");
    data.SelectAll();
}

class Filter {
    void OnFilter(UI::InputTextCallbackData@ data) { data.ClearSelection(); }
    void Wire() {
        string str = "w";
        UI::InputText("##f", str, 0, UI::InputTextCallback(OnFilter));
    }
}

void Render() {
    string s = "test";
    // 1. matching free function decays into the InputTextCallback@ slot
    string a = UI::InputText("##1", s, UI::InputTextFlags::None, OnInput);
    // 2. bool&out overload keeps the trailing callback slot
    bool changed = false;
    string b = UI::InputText("##2", s, changed, UI::InputTextFlags::None, OnInput);
    // 3. multiline overload
    string c = UI::InputTextMultiline("##3", s, vec2(), UI::InputTextFlags::None, OnInput);
    // 4. ctor form
    UI::InputTextCallback@ cb = UI::InputTextCallback(OnInput);
    // 5. bare function decays on direct handle assignment
    UI::InputTextCallback@ cb2 = OnInput;
    // 6. funcdef-typed variable back into the callback slot
    string d = UI::InputText("##4", s, UI::InputTextFlags::None, cb2);
    // 7. class method callback
    Filter f;
    f.Wire();
    print(a + b + c + d + tostring(changed));
    cb(null);
}
