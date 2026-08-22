// Negative guard for the GH #28 No-matching-symbol rule: user-defined
// helper namespaces that aren't in the typedb must stay silent — the
// checker cannot distinguish "user namespace" from "typedb gap" there.
// `DrawProbe` shows the contrast: Draw is in the removed-API table (known
// game namespace, absent from 1.29.5 typedb) so it DOES diagnose.

namespace MyHelpers {
    void UsefulThing() {}
}

void Main() {
    MyHelpers::UsefulThing();       // line 11: user namespace — must stay silent
    MyHelpers::NotInDbEither(42);   // line 12: unknown member of user ns — silent
    Display::GetWidth();            // line 13: legal relocated API — silent
    UI::Begin("x");                 // line 14: known member of known ns — silent
}
