// B023: wrong-signature / non-function args passed DIRECTLY into typedb
// funcdef-typed parameter slots are silently accepted by the LSP while the
// game compiler rejects them. The ctor form was already correct (GH #63 /
// B022); this is the GH #47 overload-set + unique-overload call path.
// Game ground truth (RemoteBuild probe 2026-08-29, OP 1.29.5): every
// REJECT line errors `No matching signatures to '...'`; every legal line
// compiles clean.

void Good(UI::InputTextCallbackData@ data) { data.SelectAll(); }
void WrongSig(int x) {}
void Tick() {}
void OnMember(CMwNod@ nod, Reflection::MwMemberInfo@ info) {}
bool Hook(CMwStack &in st) { return false; }

funcdef void WSig(int x);

void Render() {
    string s = "test";
    CMwNod@ nod;
    WSig@ wrongVar = null;
    UI::InputTextCallback@ rightVar = Good;
    // REJECT: wrong-signature fn into the callback slot
    UI::InputText("##a", s, UI::InputTextFlags::None, WrongSig);
    // REJECT: string literal into the slot
    UI::InputText("##b", s, UI::InputTextFlags::None, "nope");
    // REJECT: wrong-signature funcdef-typed variable
    UI::InputText("##c", s, UI::InputTextFlags::None, wrongVar);
    // REJECT: same on InputTextMultiline
    UI::InputTextMultiline("##d", s, vec2(), UI::InputTextFlags::None, WrongSig);
    // REJECT: wrong-sig fn into startnew (CoroutineFunc family)
    startnew(WrongSig);
    // REJECT: wrong-sig fn into the NodTree member-callback slot
    UI::NodTree(nod, WrongSig);
    // REJECT: wrong-sig fn into Dev::InterceptProc (ProcIntercept family)
    Dev::InterceptProc("CMwNod", "MwAddRef", WrongSig);
    // legal: matching free fn / ctor-wrapped fn / right-typed var
    UI::InputText("##e", s, UI::InputTextFlags::None, Good);
    UI::InputText("##f", s, UI::InputTextFlags::None, UI::InputTextCallback(Good));
    UI::InputText("##g", s, UI::InputTextFlags::None, rightVar);
    // legal: void() fn into startnew; matching member callbacks
    startnew(Tick);
    UI::NodTree(nod, OnMember);
    Dev::InterceptProc("CMwNod", "MwAddRef", Hook);
}
