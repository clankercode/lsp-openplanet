// B060: a virtual-property READ (`Off`, `c.Amt`, `Ns::Slot`) is a VALUE, not
// a function reference. B023's `arg_is_function_shaped` asked
// `lookup_function_overloads`, whose `get_`/`set_` alternates made every
// property read look function-shaped, so it could not bind a `uint`/`float`
// parameter and the whole overload set died with a bogus
// `No matching signatures to 'Text::Format(const string, uint)'`.
// `get_X` is a function when WRITTEN `get_X`; `X` is a value.
// Live in tm-dips-plus-plus src/DebugWindow.as:265,269,270 and
// src/MainUI/Donations.as:24 — the game compiles all four.
//
// The fixture pins BOTH sides of the line, because a fix that silences more
// than intended looks identical to a correct one: every property read used
// as a value must go silent, and every neighbouring case that is genuinely
// wrong must keep erroring.

uint get_Off() { return 1; }
float get_Ratio() { return 0.5f; }
int get_Delta() { return -2; }

const uint16 SZ = 4;

funcdef void Cb();
funcdef void BadCb(int x);
funcdef uint UintFn();
funcdef int Doubler(int x);

Cb@ stored;
BadCb@ storedBad;
Cb@ handlerVar;
Cb@ get_Handler() { return stored; }
BadCb@ get_BadHandler() { return storedBad; }

class C {
    float last = 1.5f;
    Cb@ h;
    BadCb@ bh;
    float get_Amt() { return last; }
    uint get_Count() { return 3; }
    Cb@ get_Handler() { return h; }
    BadCb@ get_BadHandler() { return bh; }
    int Twice(int x) { return x * 2; }
}

namespace Ns {
    uint get_Slot() { return 7; }
    Cb@ nested;
    Cb@ get_Handler() { return nested; }
    class D {
        float get_Weight() { return 2.0f; }
    }
}

void Tick() {}
void WrongSig(int x) {}

void Use() {
    uint u = 3;
    float f = 1.5f;
    int i = -1;
    C@ c = C();
    Ns::D@ d = Ns::D();

    // silent (pre-existing): locals and a const of the same primitive types
    Text::Format("0x%04x", u);
    Text::Format("%.2f", f);
    Text::Format("%d", i);
    Text::Format("0x%04x", SZ);

    // silent (B060): free-function getters read as property values
    Text::Format("0x%04x", Off);
    Text::Format("%.2f", Ratio);
    Text::Format("%d", Delta);

    // silent (B060): class getters read through a handle
    Text::Format(" ($%.2f)", c.Amt);
    Text::Format("0x%04x", c.Count);

    // silent (B060): namespace-qualified getter, and a getter on a
    // namespaced class
    Text::Format("0x%04x", Ns::Slot);
    Text::Format("%.2f", d.Weight);

    // silent (B060): same FP on a different non-funcdef overload set
    Math::Max(Delta, 1);
    Math::Max(Ratio, 0.5f);

    // silent (B060): a property whose VALUE IS a funcdef handle binds a
    // funcdef slot by ITS OWN signature — the widening previously compared
    // the ACCESSOR's signature (`Cb@()` against `void()`) and rejected it
    startnew(Handler);
    startnew(c.Handler);
    startnew(Ns::Handler);

    // silent (B023 unchanged): the accessor referenced by its FUNCTION name
    // is still a function and still binds a matching funcdef slot
    UintFn@ uf = UintFn(get_Off);
    Doubler@ db = Doubler(c.Twice);
    // silent (B023 unchanged): plain function and funcdef-typed local
    startnew(Tick);
    Cb@ local = stored;
    startnew(local);

    // ---- the boundary: everything below must STAY an error ----

    // REJECT (B023): a real free function into a non-funcdef parameter slot
    Text::Format("%d", WrongSig);
    // REJECT (B023): wrong-signature fn into a funcdef slot
    startnew(WrongSig);
    // REJECT: the neighbour of the three silent `startnew` lines above — a
    // funcdef-handle property of the WRONG signature. Silencing this instead
    // is exactly the over-broad fix this fixture exists to catch.
    startnew(BadHandler);
    startnew(c.BadHandler);
    // REJECT: a funcdef handle cannot bind a `uint` value slot, whichever
    // path types it (property read, member read, plain global)
    Text::Format("%d", Handler);
    Text::Format("%d", c.Handler);
    Text::Format("%d", handlerVar);
    // REJECT (B060 mirror): a property VALUE cannot construct a delegate.
    // Not game-probed directly, but the LSP already asserted exactly this
    // via `is_clearly_not_a_function` on the `startnew(Off)` line below,
    // which errors on master too.
    UintFn@ bad = UintFn(Off);
    startnew(Off);
}
