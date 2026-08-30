// B060 review follow-up: `arg_value_funcdef` resolves a `Named(n)` value
// through `lookup_funcdef`, and `lookup_funcdef` falls through to the typedb
// whenever the workspace symbol of that name is not itself a funcdef. So a
// plugin class named after a Core funcdef would resolve to the ENGINE
// funcdef and its instances would be rejected as function-shaped at an
// ordinary value slot. GH #38 shadowing decides it: the plugin's own
// declaration wins.
//
// `SCoroutineFunc` below is NOT a collision — it is the control. The
// collision is `CoroutineFunc`, which Core declares as `void CoroutineFunc()`.

class CoroutineFunc {
    int v;
}

class SCoroutineFunc {
    int v;
}

CoroutineFunc@ shadowGlobal;
SCoroutineFunc@ controlGlobal;

CoroutineFunc@ get_ShadowThing() { return shadowGlobal; }
SCoroutineFunc@ get_ControlThing() { return controlGlobal; }

void SUse() {
    // The plugin's class is a value, not the engine's funcdef, whether it is
    // read as a property or as a plain global.
    Text::Format("%d", ShadowThing);
    Text::Format("%d", shadowGlobal);
    // The non-colliding control must behave identically.
    Text::Format("%d", ControlThing);
    Text::Format("%d", controlGlobal);
}
