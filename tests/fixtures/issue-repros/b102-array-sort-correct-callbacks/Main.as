// B102 NEGATIVE CONTROL. Every callback here is CORRECT for its element
// type, so this fixture must stay completely silent on errors. Without it a
// rule that rejects every `.Sort(cb)` scores as a pass on the sibling
// fixture, which is the whole population of this ticket.
//
// The expected signature is TEMPLATE-MEMBER: `T[]::less` takes the ARRAY'S
// ELEMENT TYPE, so each element type below is a different expected signature
// computed at the call site, not one fixed funcdef.

class Item { int v; }
enum Colour { Red, Green }

bool LessInt(const int &in a, const int &in b) { return a < b; }
bool LessFloat(const float &in a, const float &in b) { return a < b; }
bool LessString(const string &in a, const string &in b) { return a < b; }
bool LessItemVal(const Item &in a, const Item &in b) { return a.v < b.v; }
bool LessItemHandle(const Item@ &in a, const Item@ &in b) { return a.v < b.v; }
bool LessNestedArr(const array<int> &in a, const array<int> &in b) { return a.Length < b.Length; }
bool LessColour(const Colour &in a, const Colour &in b) { return int(a) < int(b); }
bool LessDict(const dictionary@ &in a, const dictionary@ &in b) { return a.GetSize() < b.GetSize(); }

// NON-CONST parameters. The engine's const `less` funcdef is DANGLING in the
// typedb (typeid 67108888, referenced by Sort, never declared). B102 recorded
// the engine's stance here as UNMEASURED because B097 blocked game probing.
//
// B144 MEASURED IT (2026-09-01, OP 1.29.14,
// `docs/probes/b144-const-handle-funcdef.tsv`): the engine prints its own
// slot signature when a bind fails, and it is EXACT in both directions —
//
//     Where 'less' is 'bool int[]::less(const int&in, const int&in)'
//     Where 'less_nonconst' is 'bool int[]::less_nonconst(int&in, int&in)'
//
// so `int &in` at `Sort` is REJECTED by the engine, and `const int &in` at
// `SortNonConst` is REJECTED too. This row is therefore a KNOWN FALSE
// NEGATIVE, not an open question: the checker stays const-neutral by
// construction (`funcdef_types_compatible` peels `const`), which is the
// safe direction, and tightening it is now a change with a probe behind it
// rather than a guess. It is deliberately NOT made here — tightening fires
// on all four const positions at once, including the leading one B110
// guards, and its risk polarity is the opposite of B144's.
bool LessIntNonConst(int &in a, int &in b) { return a < b; }

// B144: a CONST HANDLE — the trailing `const` — at the same slot.
//
// This is the spelling the ENGINE actually wants. For a HANDLE element type
// the synthesised comparator is leading AND trailing const:
//
//     Where 'less' is 'bool Item@[]::less(const Item@const&in, const Item@const&in)'
//
// measured for `Json::Value@`, a workspace class handle, an interface
// handle, `array<int>@` and `dictionary@` alike; for a NON-handle element
// type it is plain `const T&in`. So `LessItemHandle` above — `const Item@
// &in` — is the row the engine rejects, and these two are the rows it
// accepts.
//
// Before B144 the checker rejected BOTH, because `Item@ const` was parsed
// out of a stored signature string as a type literally NAMED "Item@ const",
// equal to nothing. That is the false positive Max reported against
// `tm-ghosts-plus-plus`, whose `SortGhosts` is spelled exactly like this.
bool LessItemHandleConst(const Item@ const &in a, const Item@ const &in b) { return a.v < b.v; }
bool LessDictConst(const dictionary@ const &in a, const dictionary@ const &in b) { return a.GetSize() < b.GetSize(); }

funcdef bool MyLess(const int &in a, const int &in b);

void Main() {
    int[] xs = {3, 1, 2};
    xs.Sort(LessInt);
    xs.Sort(LessIntNonConst);
    xs.Sort(LessInt, 0, 2);          // the extra uint args must not disturb the slot
    xs.SortNonConst(LessIntNonConst);
    xs.SortAsc();
    xs.SortDesc();
    xs.SortAsc(0, 2);

    // A funcdef HANDLE variable of the right shape binds the slot.
    // NOT a vacuous row: retyping `MyLess` to a string comparer and passing
    // it here was MEASURED to produce
    // `No matching signatures to 'array::Sort(MyLess@)'`, so the silence
    // below is the check passing rather than the check being absent. The
    // mismatched fixture pins that direction permanently.
    MyLess@ viaHandle = LessInt;
    xs.Sort(viaHandle);

    float[] fs = {1.0f};
    fs.Sort(LessFloat);

    string[] ss = {"a"};
    ss.Sort(LessString);

    array<Item> vals;
    vals.Sort(LessItemVal);

    array<Item@> handles;
    handles.Sort(LessItemHandle);
    handles.Sort(LessItemHandleConst);      // B144: the const-handle spelling
    handles.Sort(LessItemHandleConst, 0, 1); // B144: and the 3-arg form

    array<array<int>> nested;
    nested.Sort(LessNestedArr);

    array<Colour> cols;
    cols.InsertLast(Colour::Red);
    cols.InsertLast(Colour::Green);
    cols.Sort(LessColour);

    array<dictionary@> dicts;
    dicts.Sort(LessDict);
    dicts.Sort(LessDictConst);              // B144: engine handle, other family

    print("" + xs.Length + fs.Length + ss.Length + vals.Length + handles.Length
            + nested.Length + cols.Length + dicts.Length);
}
