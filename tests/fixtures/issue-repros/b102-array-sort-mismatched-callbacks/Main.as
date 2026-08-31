// B102. Every `.Sort(...)` below binds a callback the engine cannot accept,
// and every one of them was SILENT before this ticket.
//
// The rows here are exactly the ones decidable WITHOUT a game probe — they
// are wrong under every reading of the undeclared `T[]::less` signature.
// The const-ness axis, which the typedb does not pin, is deliberately NOT
// exercised here; it lives in the correct-callbacks fixture as a required
// silence. (B097 used to block probing it. B144 probed it — see
// `docs/probes/b144-const-handle-funcdef.tsv` — and the checker's neutrality
// is now a KNOWN, recorded false negative rather than an open question. The
// rows below stay independent of it on purpose: each is wrong on an axis the
// checker enforces, so none of them depends on how const-ness is resolved.)
//
// B144 added the `*ConstHandle` rows. A CONST HANDLE (`T@ const`) must not
// become a wildcard: before B144 it was parsed out of a stored signature as a
// type literally NAMED "T@ const", so these lines failed for the WRONG reason
// — the name matched nothing, rather than the arity/return/element type/mode
// being wrong. After the fix they must still fail, now for the right reason.
//
// B152 added the `*ConstMismatch` rows and moved three rows here out of the
// correct-callbacks fixture, where they had been pinned as REQUIRED SILENCE
// on the const axis. That silence was a known false negative once B144
// measured the engine, and B152 made the comparison const-EXACT, so they are
// mismatches like everything else in this file. Their engine verdicts are not
// inferred — the game rejected all three, in its own words:
//
//     (68,8)   No matching signatures to 'array::Sort(::LessIntNonConst)'
//              Where 'less' is 'bool int[]::less(const int&in, const int&in)'
//     (94,13)  No matching signatures to 'array::Sort(::LessItemHandle)'
//              Where 'less' is 'bool Item@[]::less(const Item@const&in, ...)'
//     (107,11) No matching signatures to 'array::Sort(::LessDict)'
//              Where 'less' is 'bool dictionary@[]::less(const dictionary@const&in, ...)'
//
// (2026-09-01 probe of the correct-callbacks fixture, OP 1.29.14, at the line
// numbers it had then.)
//
// B152 also retyped `WrongModeInout` from `int &inout` to `Item &inout`, and
// moved it onto an `array<Item>` so it is still wrong on the MODE axis alone.
// `int &inout` is not declarable at all — "Only object types that support
// object handles can use &inout" — and that is a DECLARATION error, which
// aborts the compilation unit before `Main` is analysed, so the game could
// not adjudicate a single `Sort` row in this file. `Item@ &inout` fails the
// same way and for the same reason, measured not assumed: `&inout` wants the
// OBJECT type, not a handle to it.
class Item { int v; }

bool LessInt(const int &in a, const int &in b) { return a < b; }
bool LessItem(const Item@ &in a, const Item@ &in b) { return a.v < b.v; }
bool LessString(const string &in a, const string &in b) { return a < b; }

bool WrongArityOne(const int &in a) { return a < 0; }
bool WrongArityThree(const int &in a, const int &in b, const int &in c) { return a < b && b < c; }
void WrongReturnVoid(const int &in a, const int &in b) { }
int  WrongReturnInt(const int &in a, const int &in b) { return a < b ? 1 : 0; }
bool WrongModeOut(int &out a, int &out b) { a = 0; b = 0; return true; }
bool WrongModeInout(Item &inout a, Item &inout b) { return a.v < b.v; }
bool WrongModeValue(int a, int b) { return a < b; }

// B144: the same four wrongness axes, each carried on a CONST HANDLE.
bool WrongArityConstHandle(const Item@ const &in a) { return a.v < 0; }
void WrongReturnConstHandle(const Item@ const &in a, const Item@ const &in b) { }
bool WrongElemConstHandle(const Item@ const &in a, const Item@ const &in b) { return a.v < b.v; }
bool WrongModeConstHandle(Item@ const &out a, Item@ const &out b) { return true; }

// B152: the CONST AXIS itself, now that the comparison is exact about it.
// Each of these is correct on arity, return type, element type and mode, and
// wrong ONLY on where the `const` sits — so they cannot pass by accident on
// some other axis, and they are the rows that would go silent if the const
// tightening were reverted.
bool LessIntNonConst(int &in a, int &in b) { return a < b; }
bool LessItemHandle(const Item@ &in a, const Item@ &in b) { return a.v < b.v; }
bool LessDict(const dictionary@ &in a, const dictionary@ &in b) { return a.GetSize() < b.GetSize(); }
bool LessItemTrailingOnly(Item@ const &in a, Item@ const &in b) { return a.v < b.v; }
bool LessItemNoConst(Item@ &in a, Item@ &in b) { return a.v < b.v; }
bool LessIntConstAtNonConstSlot(const int &in a, const int &in b) { return a < b; }

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
    xs.Sort(WrongModeValue);     // parameter mode (by value)

    array<Item@> items;
    array<dictionary@> dicts;
    items.Sort(LessInt);         // element type, the other direction
    items.Sort(WrongArityConstHandle);   // B144: arity, on a const handle
    items.Sort(WrongReturnConstHandle);  // B144: return type, on a const handle
    items.Sort(WrongModeConstHandle);    // B144: parameter mode, on a const handle
    xs.Sort(WrongElemConstHandle);       // B144: element type, on a const handle

    array<Item> vals;
    vals.Sort(WrongModeInout);   // parameter mode (&inout)

    xs.SortNonConst(LessString); // the non-const slot checks the same way

    // B152: wrong only on the const axis.
    xs.Sort(LessIntNonConst);          // value element: the slot is `const int&in`
    xs.SortNonConst(LessIntConstAtNonConstSlot); // the mirror: that slot is `int&in`
    items.Sort(LessItemHandle);        // handle element: leading const only
    items.Sort(LessItemTrailingOnly);  // handle element: trailing const only
    items.Sort(LessItemNoConst);       // handle element: neither const
    dicts.Sort(LessDict);              // engine handle, other family


    StringLess@ badHandle = LessString;
    xs.Sort(badHandle);          // funcdef handle of the wrong shape

    print("" + xs.Length + items.Length + dicts.Length + vals.Length);
}
