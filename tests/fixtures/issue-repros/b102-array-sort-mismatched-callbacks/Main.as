// B102. Every `.Sort(...)` below binds a callback the engine cannot accept,
// and every one of them was SILENT before this ticket.
//
// The rows here are exactly the ones decidable WITHOUT a game probe — they
// are wrong under every reading of the undeclared `T[]::less` signature.
// The const-ness axis, which the typedb does not pin and B097 blocks
// probing, is deliberately NOT exercised here; it lives in the correct-
// callbacks fixture as a required silence.

class Item { int v; }

bool LessInt(const int &in a, const int &in b) { return a < b; }
bool LessItem(const Item@ &in a, const Item@ &in b) { return a.v < b.v; }
bool LessString(const string &in a, const string &in b) { return a < b; }

bool WrongArityOne(const int &in a) { return a < 0; }
bool WrongArityThree(const int &in a, const int &in b, const int &in c) { return a < b && b < c; }
void WrongReturnVoid(const int &in a, const int &in b) { }
int  WrongReturnInt(const int &in a, const int &in b) { return a < b ? 1 : 0; }
bool WrongModeOut(int &out a, int &out b) { a = 0; b = 0; return true; }
bool WrongModeInout(int &inout a, int &inout b) { return a < b; }
bool WrongModeValue(int a, int b) { return a < b; }

// A funcdef HANDLE of the wrong shape. This row exists because the sibling
// fixture's correct-handle row would otherwise be unfalsifiable: if a
// funcdef-typed variable were simply neutral at the slot, its silence there
// would prove nothing.
funcdef bool StringLess(const string &in a, const string &in b);

void Main() {
    int[] xs = {3, 1, 2};
    xs.Sort(WrongArityOne);      // arity
    xs.Sort(WrongArityThree);    // arity
    xs.Sort(WrongReturnVoid);    // return type
    xs.Sort(WrongReturnInt);     // return type
    xs.Sort(LessString);         // element type
    xs.Sort(LessItem);           // element type
    xs.Sort(WrongModeOut);       // parameter mode (&out)
    xs.Sort(WrongModeInout);     // parameter mode (&inout)
    xs.Sort(WrongModeValue);     // parameter mode (by value)

    array<Item@> items;
    items.Sort(LessInt);         // element type, the other direction

    xs.SortNonConst(LessString); // the non-const slot checks the same way

    StringLess@ badHandle = LessString;
    xs.Sort(badHandle);          // funcdef handle of the wrong shape

    print("" + xs.Length + items.Length);
}
