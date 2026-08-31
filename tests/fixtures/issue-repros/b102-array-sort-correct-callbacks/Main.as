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
bool LessNestedArr(const array<int> &in a, const array<int> &in b) { return a.Length < b.Length; }
bool LessColour(const Colour &in a, const Colour &in b) { return int(a) < int(b); }

// NON-CONST parameters, at the NON-const slot. B102 recorded the engine's
// stance here as UNMEASURED; B144 measured it and B152 acted on it.
//
// The engine prints its own slot signature when a bind fails, so it answers
// in its own words (2026-09-01, OP 1.29.14,
// `docs/probes/b144-const-handle-funcdef.tsv`):
//
//     Where 'less' is 'bool int[]::less(const int&in, const int&in)'
//     Where 'less_nonconst' is 'bool int[]::less_nonconst(int&in, int&in)'
//
// The two slots are mirror images and each is exact, so this comparator is
// correct at `SortNonConst` and WRONG at `Sort`. It used to be used at both
// here, as a pin on const-neutrality; B152 moved the `Sort` use to the
// mismatched fixture, where the engine's rejection of it is the point.
bool LessIntNonConst(int &in a, int &in b) { return a < b; }

// B144/B152: a CONST HANDLE — the trailing `const` — at the same slot.
//
// This is the spelling the ENGINE wants. For a HANDLE element type the
// synthesised comparator carries leading AND trailing const:
//
//     Where 'less' is 'bool Item@[]::less(const Item@const&in, const Item@const&in)'
//
// measured for `Json::Value@`, a workspace class handle, an interface handle,
// `array<int>@` and `dictionary@` alike; for a NON-handle element type it is
// plain `const T&in`. The `const Item@ &in` spelling that used to sit above
// these rows is the one the engine REJECTS, and it now lives in the
// mismatched fixture.
//
// Before B144 the checker rejected these too, because `Item@ const` was
// parsed out of a stored signature string as a type literally NAMED
// "Item@ const", equal to nothing. That is the false positive Max reported
// against `tm-ghosts-plus-plus`, whose `SortGhosts` is spelled exactly like
// this.
bool LessItemHandleConst(const Item@ const &in a, const Item@ const &in b) { return a.v < b.v; }
bool LessDictConst(const dictionary@ const &in a, const dictionary@ const &in b) { return a.GetSize() < b.GetSize(); }

funcdef bool MyLess(const int &in a, const int &in b);

void Main() {
    int[] xs = {3, 1, 2};
    xs.Sort(LessInt);
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
    handles.Sort(LessItemHandleConst);  // B144: the const-handle spelling
    handles.Sort(LessItemHandleConst, 0, 1); // B144: and the 3-arg form

    array<array<int>> nested;
    nested.Sort(LessNestedArr);

    array<Colour> cols;
    cols.InsertLast(Colour::Red);
    cols.InsertLast(Colour::Green);
    cols.Sort(LessColour);

    array<dictionary@> dicts;
    dicts.Sort(LessDictConst);              // B144: engine handle, other family

    print("" + xs.Length + fs.Length + ss.Length + vals.Length + handles.Length
            + nested.Length + cols.Length + dicts.Length);
}
