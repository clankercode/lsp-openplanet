// B053, second half: how a plugin's OWN declarations meet the engine's enums.
//
// Kept apart from `b053-ambiguous-enum-values` because each declaration below
// changes the verdict on a bare name, which would perturb that fixture's
// counts.
//
// The intuition "my own declaration wins, so the ambiguity goes away" is only
// half right, and the half that is wrong is the one that matters. Probed
// individually against the live game (RemoteBuild, Trackmania + Openplanet
// 1.29.5, 2026-08-30), one plugin per shape:
//
//   * a global `enum` of the plugin's own JOINS the pool. `GlassSmash` is
//     carried by exactly one engine enum and compiles bare -- until this
//     plugin declares it too, and then the game answers `Found multiple
//     matching enum values`. A workspace enum is a candidate, not a shield.
//   * a global VARIABLE outranks the enums. `Down` is carried by two engine
//     enums and is ambiguous bare, yet `int Down = 1;` makes the game accept
//     it: the variable is simply the answer.
//   * a NAMESPACED enum does not take part, exactly as a namespaced ENGINE
//     enum does not. `Horn` stays unambiguous.
//   * a class member does not take part either.
//
// Game verdict on this file: ONE error, on the bare `GlassSmash` in
// `WorkspaceEnumJoinsThePool`.

// Carried by two engine enums (ESceneVehicleVisReactorBoostType, VirtualKey),
// so bare `Down` is ambiguous -- but this declaration outranks both.
int Down = 1;

// Carried by exactly one engine enum (ESceneVehicleVisEvent). This makes it
// two, which is what the game rejects.
enum LocalPick {
    GlassSmash,
}

namespace NsBox {
    // Namespaced, so not in scope for a bare `Horn`.
    enum NsEnum {
        Horn,
    }
}

class Holder {
    // A class member is not a global either.
    int JumpChain1;
}

// The variable wins: accepted in-game.
void VariableOutranksTheEnums() {
    print("" + Down);
}

// The plugin's own global enum is a second candidate: REJECTED in-game with
// `Found multiple matching enum values`, and the diagnostic must name both
// `ESceneVehicleVisEvent` and this file's `LocalPick`.
void WorkspaceEnumJoinsThePool() {
    print("" + GlassSmash);
}

// Neither of these takes part; both accepted in-game.
void NamespacedEnumAndClassMemberDoNot() {
    print("" + Horn);
    print("" + JumpChain1);
}

// Qualifying is the fix, on either side of the collision.
void Qualified() {
    print("" + LocalPick::GlassSmash);
    print("" + ESceneVehicleVisEvent::GlassSmash);
}

void Main() {
    VariableOutranksTheEnums();
    WorkspaceEnumJoinsThePool();
    NamespacedEnumAndClassMemberDoNot();
    Qualified();
    Holder h;
    print("" + h.JumpChain1);
}
