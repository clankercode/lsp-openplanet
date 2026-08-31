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

// Tail-visible shadowing: `Dep::CoroutineFuncUserdataString` lives in
// ShadowingDep.as and is written here unqualified.
CoroutineFuncUserdataString@ tailShadow;
CoroutineFuncUserdataString@ get_TailShadowP() { return tailShadow; }

void TailUse() {
    Text::Format("%d", TailShadowP);
    Text::Format("%d", tailShadow);
}

// The other direction, and the reason the ladder must resolve ONE candidate
// rather than test one name and look up another: a global class shadows the
// bare name, but inside `Nq` the name means the namespace's own funcdef.
// Both lines below are REAL errors — a `void(int)` handle binds neither a
// `uint` slot nor `startnew`'s `void()` — and master misses both.
class ProcIntercept {
    int v;
}
namespace Nq {
    funcdef void ProcIntercept(int);
    ProcIntercept@ nqCb;
    void NqUse() {
        Text::Format("%d", nqCb);
        startnew(nqCb);
    }
}

// The over-blocking direction, which the first three rounds of this guard
// all got wrong in some form: only a TYPE declaration can shadow a TYPE
// name. A variable that happens to share the name is a VALUE, and treating
// it as shadowing silently swallows the diagnostics that depend on knowing
// the type. Both blocks below MUST keep erroring — master gets them right
// and an over-broad guard loses them.
funcdef void MaskedCb(int);
MaskedCb@ maskedHandle;
MaskedCb@ get_MaskedP() { return maskedHandle; }

namespace Inner {
    int MaskedCb = 3;
    void InnerUse() {
        Text::Format("%d", MaskedP);
        startnew(MaskedP);
    }
}

// Same shape reached through the TAIL table rather than the ladder: an
// unrelated namespace-level VALUE must not shadow the Core funcdef name.
CoroutineFuncUserdataUint64@ coreCb;
CoroutineFuncUserdataUint64@ get_CoreCbP() { return coreCb; }
namespace Unrelated {
    int CoroutineFuncUserdataUint64 = 7;
}

void OverblockUse() {
    Text::Format("%d", CoreCbP);
}

// The other edge of the qualified-spelling rule, and the mirror of the
// `Nq::ProcIntercept` block above. There the written name had to lose to the
// namespace ladder; here it must WIN. `Qz::UI::InputTextCallback` is an
// unrelated nested funcdef with a colliding tail; the written
// `UI::InputTextCallback` is the CORE one and binds the core slot, so this
// must stay SILENT. Testing only WORKSPACE declarations for the written name
// invents an error here — the written spelling wins whenever it denotes a
// type the checker knows at all, typedb included.
void QzGood(UI::InputTextCallbackData@ d) { d.SelectAll(); }

namespace Qz {
    namespace UI {
        funcdef void InputTextCallback(int);
    }
    UI::InputTextCallback@ qzCallback = QzGood;
    void QzUse() {
        string s = "x";
        UI::InputText("##qz", s, 0, qzCallback);
    }
}

// Round 7: the value's type is a CORE ENUM reached by a relative qualified
// spelling — `Plot::Marker` inside `namespace UI` is `UI::Plot::Marker`.
// Restricting the non-callable test to WORKSPACE classes left this
// unresolved and therefore neutral, losing a diagnostic master reports.
// An enum value binds no funcdef slot, so this MUST error.
namespace UI {
    Plot::Marker get_EnumCallback() { return Plot::Marker::Auto; }
    void EnumUse() {
        string t = "x";
        InputText("##enum", t, 0, EnumCallback);
    }
}

// Round 8: the OTHER slot for the very same shadowed values.
//
// `SUse` above pins them in a VALUE slot, where shadowing means they must
// stay SILENT: the plugin's own type wins, so the value is not a function
// reference. This block pins the complement — the same reads into a FUNCDEF
// slot, where the same conclusion makes each one an ERROR, because a value
// of a plugin class/interface/enum/typedef is not callable.
//
// `names_a_known_non_callable_type` asked raw `lookup_funcdef`, which falls
// through the workspace declaration to the Core funcdef of that name, so
// these looked "possibly callable" and their rejections were lost. Master
// reports all eight; the branch reported only the four controls until the
// workspace declaration was made to win here too.
//
// Each collision is paired with its non-colliding CONTROL, and the controls
// are the load-bearing half: they error on BOTH binaries, which proves this
// block reaches the rejection path at all. Without them, four silences read
// as "startnew accepts these" rather than "four diagnostics were lost" — a
// probe written with names that collide with nothing agrees on both sides
// and reports a false all-clear.
void ShadowIntoFuncdefSlot() {
    startnew(ShadowClassP);    // shadows Core `CoroutineFunc`
    startnew(ControlClassP);   // control: same shape, no collision
    startnew(ShadowIfaceP);    // shadows Core `CoroutineFuncUserdata`
    startnew(ControlIfaceP);   // control
    startnew(ShadowEnumP);     // shadows Core `CoroutineFuncUserdataBool`
    startnew(ControlEnumP);    // control
    startnew(ShadowAliasP);    // typedef shadows Core `CoroutineFuncUserdataInt64`
    startnew(ControlAliasP);   // control
}
