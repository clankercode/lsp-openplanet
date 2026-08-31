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
// typedb (typeid 67108888, referenced by Sort, never declared), so whether
// the engine accepts this at the const slot is UNMEASURED — see B097, game
// probing is unavailable. The checker is therefore neutral on const-ness by
// construction (`funcdef_types_compatible` peels `const`), and this row
// pins that neutrality so a later tightening is a deliberate change with a
// probe behind it rather than an accident.
bool LessIntNonConst(int &in a, int &in b) { return a < b; }

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

    array<array<int>> nested;
    nested.Sort(LessNestedArr);

    array<Colour> cols;
    cols.InsertLast(Colour::Red);
    cols.InsertLast(Colour::Green);
    cols.Sort(LessColour);

    array<dictionary@> dicts;
    dicts.Sort(LessDict);

    print("" + xs.Length + fs.Length + ss.Length + vals.Length + handles.Length
            + nested.Length + cols.Length + dicts.Length);
}
