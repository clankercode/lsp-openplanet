// Minimal repro (todo-cover-cases-next.txt, spike-live-add-kinematic-ao):
// `Reflection::GetType` returns `const Reflection::MwClassInfo@` — a
// const handle. `auto ty = …` inherits that const-ness, and rebinding
// with `ty = …` (value-assign) is rejected by the game:
//   Scene.as (258, 28) : ERR : Reference is read-only
// Workaround: `@ty = …` (handle rebind) or a second local.
// The LSP used to stay silent.
// Legal: `@ty = …` handle rebind (the handle slot itself is mutable),
// and rebinding a non-const handle.

void Main() {
    auto ty = Reflection::GetType("NGameItem_SMgr");
    if (ty is null) ty = Reflection::GetType("NSceneItem_SMgr");  // line 10: game ERR (read-only)
    if (ty is null) @ty = Reflection::GetType("NSceneItem_SMgr"); // line 11: legal handle rebind
}
