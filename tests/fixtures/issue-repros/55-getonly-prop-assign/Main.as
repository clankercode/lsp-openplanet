// Minimal repro (todo-cover-cases-next.txt, spike-live-add-kinematic-ao):
// `iso4` registers `tx` as a GET-ONLY virtual property (`float get_tx()
// const`; no `set_tx`). The game rejects writes and compound writes:
//   Tools.as (242, 16) : ERR : The property has no set accessor
//   Tools.as (242, 16) : ERR : Compound assignments with property accessors
//                        require both get and set accessors
// The LSP used to accept both silently.
// Legal: reading `loc.tx` and assigning the whole struct (`loc = iso4()`).

void F(uint64 ptr) {
    iso4 loc = Dev::ReadIso4(ptr);
    loc.tx += 8.0;    // line 10: game ERR ×2 (no set accessor)
    loc.tx = 3.0;     // line 11: game ERR (no set accessor)
    float r = loc.tx; // line 12: legal read
    loc = iso4();     // line 13: legal whole-struct assign
    r += loc.ty;      // line 14: legal read into compound on a FLOAT local
    print("" + r);
}

void Main() {}
