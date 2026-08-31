// B087. Every declaration kind the `shared` modifier can carry, in an
// ORDINARY source file that no manifest key names. A dependent plugin
// consumes each of these through the AngelScript shared-entity mechanism,
// with no textual reference anywhere in this workspace — so an unreferenced
// `shared` declaration is reachable by definition.
shared class SharedClass {
    int SharedField;
    void SharedMethod() {}
}

shared interface ISharedFace {
    void Go();
}

shared enum SharedEnum {
    SharedA,
    SharedB,
}

shared funcdef void SharedCallback(int a);

shared void SharedFunction() {}

shared abstract class SharedAbstract {
    void AbstractMethod() {}
}

// ── THE NEGATIVE CONTROLS ────────────────────────────────────────────────
// Identical declarations WITHOUT `shared`, in the SAME FILE as the rooted
// ones so that "exempt the file" cannot pass this fixture. Every one of
// these must still be reported.
class PlainClass {
    int PlainField;
    void PlainMethod() {}
}

interface IPlainFace {
    void Go();
}

enum PlainEnum {
    PlainA,
    PlainB,
}

funcdef void PlainCallback(int a);

void PlainFunction() {}
