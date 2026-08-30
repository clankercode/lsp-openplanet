// B060 review follow-up: `arg_value_funcdef` resolves a `Named(n)` value
// through `lookup_funcdef`, which walks workspace symbols but only RETURNS
// on a real funcdef and otherwise falls through to the typedb. So a plugin
// declaration named after a Core funcdef resolves to the ENGINE funcdef and
// its values get rejected as function-shaped at an ordinary value slot.
// GH #38 shadowing decides it: the plugin's own declaration wins.
//
// Four shadowing shapes, because the first guard only covered the first two
// and the reviewer was right that a list is the wrong instrument:
//   class · interface · enum · typedef
// plus a NAMESPACED class, which matters separately: a value's type text is
// stored as WRITTEN, so `Nz::CoroutineFunc` reaches the checker as a bare
// `CoroutineFunc` and an exact-name guard misses it.
//
// Every collision is paired with a non-colliding CONTROL of the same shape,
// so the test distinguishes "the shadowing rule works" from "values of this
// shape are just never function-shaped".

class CoroutineFunc {
    int v;
}
class SafeClass {
    int v;
}

interface CoroutineFuncUserdata {
    void Go();
}
interface SafeIface {
    void Go();
}

enum CoroutineFuncUserdataBool { EA, EB }
enum SafeEnum { SA, SB }

typedef uint CoroutineFuncUserdataInt64;
typedef uint SafeAlias;

CoroutineFunc@ shadowClass;
SafeClass@ controlClass;
CoroutineFuncUserdata@ shadowIface;
SafeIface@ controlIface;
CoroutineFuncUserdataBool shadowEnum = CoroutineFuncUserdataBool::EA;
SafeEnum controlEnum = SafeEnum::SA;
CoroutineFuncUserdataInt64 shadowAlias = 1;
SafeAlias controlAlias = 1;

CoroutineFunc@ get_ShadowClassP() { return shadowClass; }
SafeClass@ get_ControlClassP() { return controlClass; }
CoroutineFuncUserdata@ get_ShadowIfaceP() { return shadowIface; }
SafeIface@ get_ControlIfaceP() { return controlIface; }
CoroutineFuncUserdataBool get_ShadowEnumP() { return shadowEnum; }
SafeEnum get_ControlEnumP() { return controlEnum; }
CoroutineFuncUserdataInt64 get_ShadowAliasP() { return shadowAlias; }
SafeAlias get_ControlAliasP() { return controlAlias; }

namespace Nz {
    // Written bare inside the namespace; declared as `Nz::NsCoroutineFunc`.
    class CoroutineFuncUserdataDouble {
        int v;
    }
    CoroutineFuncUserdataDouble@ nsShadow;
    CoroutineFuncUserdataDouble@ get_NsShadowP() { return nsShadow; }

    void NsUse() {
        Text::Format("%d", NsShadowP);
        Text::Format("%d", nsShadow);
    }
}

void SUse() {
    // property read (left) and plain global (right), for each shape
    Text::Format("%d", ShadowClassP);
    Text::Format("%d", shadowClass);
    Text::Format("%d", ControlClassP);
    Text::Format("%d", controlClass);

    Text::Format("%d", ShadowIfaceP);
    Text::Format("%d", shadowIface);
    Text::Format("%d", ControlIfaceP);
    Text::Format("%d", controlIface);

    Text::Format("%d", ShadowEnumP);
    Text::Format("%d", shadowEnum);
    Text::Format("%d", ControlEnumP);
    Text::Format("%d", controlEnum);

    Text::Format("%d", ShadowAliasP);
    Text::Format("%d", shadowAlias);
    Text::Format("%d", ControlAliasP);
    Text::Format("%d", controlAlias);
}
