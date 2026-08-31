// The SECOND spelling. The ticket's forms list names only the explicit
// `Ns::P` form; a bare name resolved through the namespace ladder from
// INSIDE the namespace reaches the same recorder by a different route, and a
// test pinning only the first passes while half the defect survives.

namespace Inner {
    int slot = 0;
    int get_Slot() { return slot; }        // credited: read as bare `Slot` below
    void set_Slot(int v) { slot = v; }     // credited: written as bare `Slot` below

    void UseFromInside() {
        int v = Slot;                      // bare read through the ladder
        Slot = v + 1;                      // bare write through the ladder
    }
}
